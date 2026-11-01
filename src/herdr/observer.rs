use std::io::{BufRead, BufReader, Write};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use crate::holder::{Shared, protocol, registry::Metadata};
use crate::lifetime::identity::Identity;
use crate::lifetime::identity::Life;
use crate::{Result, fail};

pub fn start(shared: Shared) {
    std::thread::spawn(move || {
        loop {
            let stopping = shared.lock().map_or(true, |state| state.stopping);
            if stopping {
                break;
            }
            if observe(Arc::clone(&shared)).is_err() {
                // Loss of the observer proves nothing about close intent; retry without signalling.
                std::thread::sleep(Duration::from_millis(250));
            }
        }
    });
}

fn observe(shared: Shared) -> Result<()> {
    let meta = shared
        .lock()
        .map_err(|_| "holder mutex poisoned")?
        .meta
        .clone();
    let mut socket = super::connect(&meta.binding.socket)?;
    let server = Identity::read(protocol::peer(&socket)?)?;
    if server != meta.binding.server {
        if meta.binding.server.life() != Life::Gone {
            return fail("observer old server still live or unverifiable; rebind refused");
        }
        let attachment = meta
            .attachment
            .as_ref()
            .ok_or("invalid_binding: ambiguous pane instance; original attachment unknown")?;
        if attachment.life() != Life::Live {
            return fail(
                "invalid_binding: ambiguous pane instance; original attachment gone; explicitly attach exact holder",
            );
        }
        let process = super::rpc(
            &meta.binding.socket,
            "pane.process_info",
            json!({"pane_id":meta.binding.pane}),
        )?;
        if process["process_info"]["shell_pid"].as_u64() != Some(attachment.pid.into()) {
            return fail(
                "invalid_binding: ambiguous pane instance; original attachment does not own this pane",
            );
        }
        let pong = super::rpc(&meta.binding.socket, "ping", json!({}))?;
        if pong["protocol"] != 22 {
            return fail("observer incompatible upstream protocol");
        }
        let result = super::rpc(
            &meta.binding.socket,
            "pane.get",
            json!({"pane_id":meta.binding.pane}),
        )?;
        let (pane, tab, workspace) = restored(&result)?;
        if pane != meta.binding.pane
            || workspace != meta.binding.workspace
            || tab != meta.binding.tab
        {
            return fail("observer restored public identity mismatch; holder retained");
        }
        let binding = crate::holder::registry::Binding {
            server,
            pane,
            tab,
            workspace,
            socket: meta.binding.socket.clone(),
        };
        let mut state = shared.lock().map_err(|_| "holder mutex poisoned")?;
        if state.meta.generation != meta.generation {
            return fail("observer binding already changed");
        }
        state.bind(binding)?;
        return Ok(());
    }
    let request = json!({"id":"keeper-observer","method":"events.subscribe","params":{"subscriptions":[{"type":"pane.closed"},{"type":"pane.moved"}]}});
    serde_json::to_writer(&mut socket, &request)?;
    socket.write_all(b"\n")?;
    socket.set_read_timeout(None)?;
    follow(&mut BufReader::new(socket), &shared, &meta)
}

/// The restored pane's public IDs from pane.get; only these are read, so a field the pinned
/// schema lacks never refuses the rebind.
fn restored(result: &Value) -> Result<(String, String, String)> {
    let id = |key: &str| {
        result["pane"][key]
            .as_str()
            .map(str::to_owned)
            .ok_or(format!("observer restored pane lacks {key}"))
    };
    Ok((id("pane_id")?, id("tab_id")?, id("workspace_id")?))
}

/// The line naming an event the pinned schema lacks by its type, printed as escaped JSON.
fn skipped(value: &Value) -> String {
    format!(
        "herdr-shield: observer skipped event {}, which the pinned v22 schema lacks",
        value["event"]
    )
}

/// Follows the subscription until a typed event for this generation is acted on.
fn follow(reader: &mut impl BufRead, shared: &Shared, meta: &Metadata) -> Result<()> {
    loop {
        let value: Value = crate::config::decode_json(&super::line(reader)?)?;
        if !value["error"].is_null() {
            return fail("observer subscription refused");
        }
        if !value["event"].is_null() {
            // Only a typed event can rebind or close; one the pinned schema lacks is skipped
            // and the subscription stays open for the next.
            let Ok(event) = serde_json::from_value::<super::schema::EventEnvelope>(value.clone())
            else {
                eprintln!("{}", skipped(&value));
                continue;
            };
            let mut state = shared.lock().map_err(|_| "holder mutex poisoned")?;
            if state.meta.generation != meta.generation
                || state.meta.binding != meta.binding
                || state.stopping
            {
                return fail("observer generation ended");
            }
            if let super::schema::EventData::PaneMoved {
                previous_pane_id,
                previous_tab_id,
                previous_workspace_id,
                pane,
                ..
            } = &event.data
            {
                if previous_pane_id == &state.meta.binding.pane
                    && previous_tab_id == &state.meta.binding.tab
                    && previous_workspace_id == &state.meta.binding.workspace
                {
                    let binding = crate::holder::registry::Binding {
                        server: state.meta.binding.server.clone(),
                        socket: state.meta.binding.socket.clone(),
                        pane: pane.pane_id.clone(),
                        tab: pane.tab_id.clone(),
                        workspace: pane.workspace_id.clone(),
                    };
                    state.bind(binding)?;
                    return Ok(());
                }
            }
            if let super::schema::EventData::PaneClosed { pane_id, .. } = event.data {
                if pane_id == state.meta.binding.pane && state.meta.state == "running" {
                    if let Some(child) = state.child.as_mut() {
                        if child.try_wait()?.is_none() {
                            child.kill()?;
                            child.wait()?;
                        }
                    }
                    state.meta.state = "terminated".into();
                    state.meta.reason =
                        "intentional-pane-close (authenticated generation-bound live observer)"
                            .into();
                    state.persist();
                    state.stopping = true;
                    return Ok(());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpinned_event_is_skipped_and_the_subscription_reaches_the_next() {
        use crate::config::{Paths, random_id, secure_dir};
        use crate::holder::{registry::Binding, state::State};
        use crate::terminal::{Size, Terminal};
        use std::sync::{Arc, Mutex};
        let root = std::env::temp_dir().join(format!("observer-{}", random_id().unwrap()));
        secure_dir(&root).unwrap();
        let binding = Binding {
            socket: "/absent.sock".into(),
            pane: "w1:p1".into(),
            tab: "w1:t1".into(),
            workspace: "w1".into(),
            server: Identity::read(std::process::id()).unwrap(),
        };
        let mut meta = Metadata::new(binding, "/".into()).unwrap();
        meta.state = "running".into();
        let shared = Arc::new(Mutex::new(State {
            paths: Paths {
                home: root.clone(),
                config: root.clone(),
                state: root.clone(),
                runtime: root.clone(),
            },
            meta: meta.clone(),
            terminal: Arc::new(Mutex::new(Terminal::new(Size::new(24, 80).unwrap()))),
            writer: None,
            master: None,
            child: None,
            lease: None,
            stopping: false,
        }));
        let events = concat!(
            r#"{"event":"pane_future","data":{"type":"pane_future","pane_id":"w1:p1"}}"#,
            "\n",
            r#"{"event":"pane_closed","data":{"type":"pane_closed","pane_id":"w1:p1","workspace_id":"w1"}}"#,
            "\n"
        );
        assert_eq!(
            skipped(
                &json!({"event":"pane_future","data":{"type":"pane_future","pane_id":"w1:p1"}})
            ),
            "herdr-shield: observer skipped event \"pane_future\", which the pinned v22 schema lacks"
        );
        follow(&mut std::io::Cursor::new(events), &shared, &meta).unwrap();
        let state = shared.lock().unwrap();
        assert_eq!(
            (state.meta.state.as_str(), state.stopping),
            ("terminated", true)
        );
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn restored_pane_ids_are_read_past_unpinned_fields() {
        assert_eq!(
            restored(&json!({"pane":{"pane_id":"w1:p1","tab_id":"w1:t1","workspace_id":"w1","future":true}})).unwrap(),
            ("w1:p1".into(), "w1:t1".into(), "w1".into())
        );
        assert_eq!(
            restored(&json!({"pane":{"pane_id":"w1:p1","workspace_id":"w1"}}))
                .unwrap_err()
                .to_string(),
            "observer restored pane lacks tab_id"
        );
    }

    #[test]
    fn intentional_close_and_natural_exit_have_distinct_typed_events() {
        use super::super::schema::{EventData, EventEnvelope};
        let close:EventEnvelope=serde_json::from_str(r#"{"event":"pane_closed","data":{"type":"pane_closed","pane_id":"w1:p1","workspace_id":"w1"}}"#).unwrap();
        assert!(matches!(close.data, EventData::PaneClosed { .. }));
        assert!(
            serde_json::from_str::<EventEnvelope>(r#"{"event":"pane.closed","data":{}}"#).is_err()
        );
    }
}
