use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::{
    mapping::Aliases,
    policy::{self, Policy},
    schema,
};
use crate::holder::{Shared, protocol};
use crate::{Result, fail};

pub fn start(shared: Shared, path: PathBuf) -> Result<()> {
    let listener = std::os::unix::net::UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    std::thread::spawn(move || {
        let result = (|| -> Result<()> {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()?;
            runtime.block_on(async move {
                let listener = tokio::net::UnixListener::from_std(listener)?;
                let permits = Arc::new(tokio::sync::Semaphore::new(32));
                loop {
                    // A failed accept (EMFILE, ECONNABORTED) costs that connection, never the relay.
                    let stream = match listener
                        .accept()
                        .await
                        .and_then(|(stream, _)| stream.into_std())
                    {
                        Ok(stream) => stream,
                        Err(error) => {
                            eprintln!(
                                "herdr-shield: API relay accept failed: {error}; still serving"
                            );
                            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                            continue;
                        }
                    };
                    let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
                        continue;
                    };
                    let holder = Arc::clone(&shared);
                    tokio::task::spawn_blocking(move || {
                        let _permit = permit;
                        if connection(stream, holder).is_err() {
                            eprintln!(
                                "herdr-shield: API relay connection failed (payload redacted)"
                            );
                        }
                    });
                }
                #[allow(unreachable_code)]
                Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
            })
        })();
        if result.is_err() {
            eprintln!("herdr-shield: API relay listener failed");
        }
    });
    Ok(())
}

fn error(writer: &mut impl Write, id: Value, code: &str, message: &str) -> Result<()> {
    serde_json::to_writer(
        &mut *writer,
        &json!({"id":id,"error":{"code":code,"message":message}}),
    )?;
    writer.write_all(b"\n")?;
    Ok(())
}

fn client_line(reader: &mut BufReader<UnixStream>) -> Result<Vec<u8>> {
    match reader.fill_buf()?.first() {
        Some(b'{' | b' ' | b'\t' | b'\r') => (),
        _ => return fail("unsupported_api: expected a JSON object; binary transport refused"),
    }
    reader
        .get_mut()
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
    let line = super::line(reader);
    reader.get_mut().set_read_timeout(None)?;
    line
}

fn connection(mut client: UnixStream, shared: Shared) -> Result<()> {
    let peer = protocol::peer(&client)?;
    client.set_nonblocking(false)?;
    client.set_write_timeout(Some(std::time::Duration::from_secs(5)))?;
    let meta = shared
        .lock()
        .map_err(|_| "holder mutex poisoned")?
        .meta
        .clone();
    let generation = meta.generation;
    let mut aliases = Aliases::for_holder(&meta);
    let census = census(super::rpc(&meta.binding.socket, "session.snapshot", json!({})).ok());
    aliases.verify_live(census.as_ref());
    let pong = super::rpc(&meta.binding.socket, "ping", json!({}))?;
    if pong["protocol"] != 22 {
        return error(
            &mut client,
            Value::Null,
            "incompatible_protocol",
            "Herdr protocol 22 required",
        );
    }
    let upstream = super::connect(&meta.binding.socket)?;
    if crate::lifetime::identity::Identity::read(protocol::peer(&upstream)?)? != meta.binding.server
    {
        return error(
            &mut client,
            Value::Null,
            "invalid_binding",
            "ambiguous pane instance; explicitly select retained holder before API rebinding",
        );
    }
    upstream.set_read_timeout(None)?;
    let client_read = client.try_clone()?;
    let upstream_write = upstream.try_clone()?;
    let pending = Arc::new(Mutex::new(BTreeMap::<String, Value>::new()));
    let requests = Arc::clone(&pending);
    let request_holder = Arc::clone(&shared);
    let output = Arc::new(Mutex::new(client.try_clone()?));
    let error_writer = Arc::clone(&output);
    let writer = std::thread::spawn(move || {
        let mut reader = BufReader::new(client_read);
        let mut upstream = upstream_write;
        let result = (|| -> Result<()> {
            loop {
                let bytes = client_line(&mut reader)?;
                let mut value: Value = match crate::config::decode_json(&bytes) {
                    Ok(value) => value,
                    Err(_) => {
                        error(
                            &mut *error_writer
                                .lock()
                                .map_err(|_| "relay output mutex poisoned")?,
                            Value::Null,
                            "invalid_frame",
                            "malformed JSON",
                        )?;
                        continue;
                    }
                };
                let id = value["id"].clone();
                if value.as_object().is_none_or(|o| {
                    o.keys()
                        .any(|k| !matches!(k.as_str(), "id" | "method" | "params"))
                }) {
                    error(
                        &mut *error_writer
                            .lock()
                            .map_err(|_| "relay output mutex poisoned")?,
                        id,
                        "invalid_frame",
                        "unknown request fields",
                    )?;
                    continue;
                }
                let typed: schema::Request = match serde_json::from_value(value.clone()) {
                    Ok(typed) => typed,
                    Err(_) => {
                        error(
                            &mut *error_writer
                                .lock()
                                .map_err(|_| "relay output mutex poisoned")?,
                            id,
                            "unsupported_api",
                            "unknown or malformed pinned request",
                        )?;
                        continue;
                    }
                };
                if policy::policy(&typed.method) == Policy::Refuse {
                    error(
                        &mut *error_writer
                            .lock()
                            .map_err(|_| "relay output mutex poisoned")?,
                        id,
                        "unsupported_api",
                        "method refused by pinned relay policy",
                    )?;
                    continue;
                }
                let method = value["method"]
                    .as_str()
                    .ok_or("method required")?
                    .to_owned();
                {
                    let state = request_holder.lock().map_err(|_| "holder mutex poisoned")?;
                    if state.meta.generation != generation {
                        error(
                            &mut *error_writer
                                .lock()
                                .map_err(|_| "relay output mutex poisoned")?,
                            id,
                            "stale_generation",
                            "binding changed; reconnect and resubscribe",
                        )?;
                        continue;
                    }
                }
                if aliases.request(&method, &mut value["params"]).is_err() {
                    error(
                        &mut *error_writer
                            .lock()
                            .map_err(|_| "relay output mutex poisoned")?,
                        id,
                        "invalid_binding",
                        "historical identity is reused or cannot be independently resolved; request not forwarded",
                    )?;
                    continue;
                }
                let key = id.as_str().ok_or("request id must be string")?.to_owned();
                {
                    let mut pending = requests.lock().map_err(|_| "pending mutex poisoned")?;
                    if pending.len() >= 128 || pending.contains_key(&key) {
                        return fail("relay request bound or duplicate id");
                    }
                    pending.insert(key, super::resume::track(&request_holder, &value, peer));
                }
                serde_json::to_writer(&mut upstream, &value)?;
                upstream.write_all(b"\n")?;
            }
        })();
        if result.is_err() {
            if let Ok(mut output) = error_writer.lock() {
                let _ = error(
                    &mut *output,
                    Value::Null,
                    "invalid_frame",
                    "invalid, incomplete or unsupported relay request; connection closed",
                );
            }
        }
        let _ = upstream.shutdown(Shutdown::Both);
        result
    });
    let result = (|| -> Result<()> {
        let mut reader = BufReader::new(upstream.try_clone()?);
        loop {
            let bytes = super::line(&mut reader)?;
            let value: Value = crate::config::decode_json(&bytes)?;
            if shared
                .lock()
                .map_err(|_| "holder mutex poisoned")?
                .meta
                .generation
                != generation
            {
                error(
                    &mut client,
                    value["id"].clone(),
                    "stale_generation",
                    "binding changed; resubscribe",
                )?;
                break;
            }
            if validate_response(&value).is_err() {
                error(
                    &mut client,
                    value["id"].clone(),
                    "unsupported_api",
                    "unknown or malformed upstream response; pinned schema required",
                )?;
                break;
            }
            let request = if let Some(id) = value["id"].as_str() {
                pending
                    .lock()
                    .map_err(|_| "pending mutex poisoned")?
                    .remove(id)
            } else {
                None
            };
            deliver(&output, &bytes, &shared, request, &value, generation)?;
        }
        Ok(())
    })();
    let _ = client.shutdown(Shutdown::Both);
    let _ = upstream.shutdown(Shutdown::Both);
    let _ = writer.join();
    result
}

/// Writes one upstream reply to its client, then records what the request completed. A
/// client gone before its reply (EPIPE, write timeout) still has the completion recorded; the
/// write error follows the bookkeeping.
fn deliver(
    output: &Mutex<UnixStream>,
    bytes: &[u8],
    shared: &Shared,
    request: Option<Value>,
    value: &Value,
    generation: u64,
) -> Result<()> {
    // Results/events already use canonical current identities, never inverse string rewriting.
    let written: Result<()> = match output.lock() {
        Ok(mut output) => output.write_all(bytes).map_err(Into::into),
        Err(_) => fail("relay output mutex poisoned"),
    };
    // Bookkeeping follows the reply: its failure never costs the reply or the connection.
    if let Some(request) = request {
        if let Err(error) = observe(shared, &request, value, generation) {
            eprintln!("herdr-shield: API relay bookkeeping failed: {error}");
        }
    }
    written
}

/// The live session snapshot alias verification reads. Only its ID strings are read, so a
/// field the pinned schema lacks never discards it.
fn census(result: Option<Value>) -> Option<Value> {
    let result = result?;
    if serde_json::from_value::<schema::ResponseResult>(result.clone()).is_err() {
        eprintln!(
            "herdr-shield: session.snapshot differs from the pinned v22 schema; reading its IDs"
        );
    }
    result.get("snapshot").cloned()
}

fn validate_response(value: &Value) -> Result<()> {
    if !value["error"].is_null() {
        let _: schema::ErrorResponse = serde_json::from_value(value.clone())?;
    } else if !value["result"].is_null() {
        let _: schema::SuccessResponse = serde_json::from_value(value.clone())?;
    } else if !value["event"].is_null() {
        if serde_json::from_value::<schema::EventEnvelope>(value.clone()).is_err() {
            let _: schema::SubscriptionEventEnvelope = serde_json::from_value(value.clone())?;
        }
    } else {
        return fail("unsupported_api: untyped upstream frame");
    }
    Ok(())
}

fn observe(shared: &Shared, request: &Value, response: &Value, generation: u64) -> Result<()> {
    super::resume::observe(shared, request, response, generation)?;
    let mut state = shared.lock().map_err(|_| "holder mutex poisoned")?;
    if state.meta.generation != generation {
        return fail("stale_generation");
    }
    if !response["error"].is_null() {
        return Ok(());
    }
    let method = request["method"].as_str().ok_or("method missing")?;
    if method == "pane.close"
        && request["params"]["pane_id"] == state.meta.binding.pane
        && response["result"]["type"] == "ok"
    {
        // The authenticated relay observed this exact generation's request and completed response.
        if let Some(child) = state.child.as_mut() {
            if child.try_wait()?.is_none() {
                child.kill()?;
                child.wait()?;
            }
        }
        state.meta.state = "terminated".into();
        state.meta.reason = "intentional-pane-close (completed authenticated relay request)".into();
        state.persist();
        state.stopping = true;
    }
    Ok(())
}

pub(super) fn valid_resume(argv: &[String]) -> bool {
    !argv.is_empty()
        && argv.len() <= 64
        && argv.iter().map(String::len).sum::<usize>() <= 8192
        && !argv[0].contains('/')
        && !argv[0].is_empty()
        && argv.iter().all(|arg| !arg.contains(['\0', '\'']))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn binary_transport_is_refused_without_waiting_for_newline() {
        let (mut sender, receiver) = UnixStream::pair().unwrap();
        sender.write_all(&[5, 0, 0, 0, 22]).unwrap();
        assert!(client_line(&mut BufReader::new(receiver)).is_err());
    }
    /// A holder bound to pane w1:p1 with its private root under the temp dir.
    fn holder(name: &str) -> Shared {
        use crate::{
            config::{Paths, random_id, secure_dir},
            holder::{
                registry::{Binding, Metadata},
                state::State,
            },
            lifetime::identity::Identity,
            terminal::{Size, Terminal},
        };
        let root = std::env::temp_dir().join(format!("{name}-{}", random_id().unwrap()));
        secure_dir(&root).unwrap();
        let paths = Paths {
            home: root.clone(),
            config: root.clone(),
            state: root.clone(),
            runtime: root,
        };
        let binding = Binding {
            socket: "/absent.sock".into(),
            pane: "w1:p1".into(),
            tab: "w1:t1".into(),
            workspace: "w1".into(),
            server: Identity::read(std::process::id()).unwrap(),
        };
        Arc::new(Mutex::new(State {
            paths,
            meta: Metadata::new(binding, "/".into()).unwrap(),
            terminal: Arc::new(Mutex::new(Terminal::new(Size::new(24, 80).unwrap()))),
            writer: None,
            master: None,
            child: None,
            lease: None,
            stopping: false,
        }))
    }
    #[test]
    fn newer_no_argv_report_prevents_stale_fallback_capture() {
        let shared = holder("report");
        observe(&shared,&json!({"method":"pane.report_agent","params":{"pane_id":"w1:p1","source":"custom:dummy","agent":"dummy","seq":2}}),&json!({"result":{"type":"ok"}}),1).unwrap();
        observe(&shared,&json!({"method":"pane.report_agent","params":{"pane_id":"w1:p1","source":"custom:dummy","agent":"dummy","seq":1,"resume_argv":["dummy","stale"]}}),&json!({"result":{"type":"ok"}}),1).unwrap();
        assert!(
            shared.lock().unwrap().meta.resume.is_none(),
            "upstream ok is not proof of accepted argv mutation"
        );
    }
    #[test]
    fn a_completed_close_whose_client_is_gone_still_stops_the_holder() {
        let shared = holder("close");
        let root = shared.lock().unwrap().paths.state.clone();
        // An unwritable registry costs the record, never the stop.
        std::fs::write(root.join("not-a-directory"), b"").unwrap();
        shared.lock().unwrap().paths.state = root.join("not-a-directory");
        let (gone, client) = UnixStream::pair().unwrap();
        drop(client);
        let error = deliver(
            &Mutex::new(gone),
            b"{\"id\":\"close\",\"result\":{\"type\":\"ok\"}}\n",
            &shared,
            Some(json!({"method":"pane.close","params":{"pane_id":"w1:p1"}})),
            &json!({"id":"close","result":{"type":"ok"}}),
            1,
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "Broken pipe (os error 32)");
        let state = shared.lock().unwrap();
        assert_eq!(
            (
                state.meta.state.as_str(),
                state.meta.reason.as_str(),
                state.stopping
            ),
            (
                "terminated",
                "intentional-pane-close (completed authenticated relay request)",
                true
            )
        );
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn native_resume_constraints_are_not_silently_normalized() {
        assert!(valid_resume(&["dummy".into(), "a b".into(), "".into()]));
        assert!(!valid_resume(&["/dummy".into()]));
        assert!(!valid_resume(&["dummy".into(), "a'b".into()]));
        assert!(!valid_resume(&[]));
    }
    #[test]
    fn census_snapshot_with_an_unpinned_field_still_verifies_aliases() {
        let snapshot = json!({"panes":[{"pane_id":"w1:p1","future":true}],"tabs":[{"tab_id":"w1:t1"}],"workspaces":[{"workspace_id":"w1"}],"future":1});
        assert_eq!(
            census(Some(json!({"type":"session_snapshot","snapshot":snapshot}))),
            Some(snapshot)
        );
        assert_eq!(census(None), None);
    }

    #[test]
    fn malformed_unknown_and_untyped_results_fail_visibly() {
        assert!(validate_response(&json!({"id":"1","result":{"type":"unknown"}})).is_err());
        assert!(validate_response(&json!({"id":"1","result":{"type":"ok"}})).is_ok());
        assert!(
            validate_response(&json!({"id":"1","error":{"code":"busy","message":"actual error"}}))
                .is_ok()
        );
    }

    #[test]
    fn unknown_required_request_fields_are_refused_by_typed_schema() {
        let request = json!({"id":"x","method":"pane.get","params":{"pane_id":"w1:p1","future_identity":"w9:p9"}});
        assert!(serde_json::from_value::<schema::Request>(request).is_err());
    }

    #[test]
    fn nested_schema_extensions_are_refused_before_forwarding() {
        for request in [
            json!({"id":"x","method":"agent.view.set","params":{"source":"custom:test","filter":{"op":"exists","field":{"token":"name","future_identity":"w9:p9"}}}}),
            json!({"id":"x","method":"agent.view.set","params":{"source":"custom:test","filter":{"op":"eq","field":"workspace_id","value":{"context":"current_workspace_id","future_identity":"w9:p9"}}}}),
            json!({"id":"x","method":"agent.view.set","params":{"source":"custom:test","sort":[{"field":{"token":"name","future_identity":"w9:p9"}}]}}),
            json!({"id":"x","method":"events.subscribe","params":{"subscriptions":[{"type":"pane.closed","future_identity":"w1:p1"}]}}),
            json!({"id":"x","method":"events.wait","params":{"match_event":{"event":"pane_closed","pane_id":"w1:p1","future_identity":"w9:p9"}}}),
            json!({"id":"x","method":"pane.move","params":{"pane_id":"w1:p1","destination":{"type":"new_workspace","future_identity":"w9:p9"}}}),
        ] {
            assert!(
                serde_json::from_value::<schema::Request>(request.clone()).is_err(),
                "{request}"
            );
        }
    }
}
