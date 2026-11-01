pub mod protocol;
pub mod registry;
pub mod state;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::config::{Paths, random_id};
use crate::lifetime::identity::{Identity, Life};
use crate::terminal::{Size, Terminal};
use crate::{Result, fail};
use protocol::Frame;
use registry::Metadata;
use state::State;

pub type Shared = Arc<Mutex<State>>;

pub fn serve(paths: Paths, id: &str) -> Result<()> {
    let mut meta = Metadata::load(&paths, id)?;
    if meta
        .holder
        .as_ref()
        .is_some_and(|identity| identity.life() == Life::Live && identity.pid != std::process::id())
    {
        return fail("holder already live; refusing replacement");
    }
    meta.holder = Some(Identity::read(std::process::id())?);
    let dir = paths.holder_dir(id)?;
    // Existing socket is evidence of an ambiguous prior holder, never unlinked automatically.
    let listener = UnixListener::bind(dir.join("holder.sock"))?;
    fs::set_permissions(dir.join("holder.sock"), fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    let shared = Arc::new(Mutex::new(State {
        paths: paths.clone(),
        meta,
        terminal: Arc::new(Mutex::new(Terminal::new(Size::new(24, 80)?))),
        writer: None,
        master: None,
        child: None,
        lease: None,
        stopping: false,
    }));
    crate::herdr::relay::start(Arc::clone(&shared), dir.join("herdr-api.sock"))?;
    crate::herdr::observer::start(Arc::clone(&shared));
    let active = Arc::new(AtomicUsize::new(0));
    let bootstrap_deadline = Instant::now() + Duration::from_secs(30);
    loop {
        {
            let mut state = shared.lock().map_err(|_| "holder mutex poisoned")?;
            if state.stopping {
                break;
            }
            if state.child.is_none() && Instant::now() > bootstrap_deadline {
                state.meta.state = "unprotected".into();
                state.meta.reason = "unbootstrapped holder expired; no workload started".into();
                state.meta.save(&state.paths)?;
                state.stopping = true;
                break;
            }
            if let Some(child) = state.child.as_mut() {
                if let Some(status) = child.try_wait()? {
                    if state.meta.state != "exited" {
                        state.meta.state = "exited".into();
                        state.meta.reason = format!(
                            "owned child exited: code={} signal={:?}",
                            status.exit_code(),
                            status.signal()
                        );
                        state.meta.save(&state.paths)?;
                    }
                }
            }
        }
        match listener.accept() {
            Ok((stream, _)) => {
                if active.load(Ordering::Relaxed) >= 32 {
                    continue;
                }
                active.fetch_add(1, Ordering::Relaxed);
                let count = Arc::clone(&active);
                let holder = Arc::clone(&shared);
                std::thread::spawn(move || {
                    if let Err(error) = connection(stream, holder) {
                        eprintln!("herdr-shield: holder connection ended: {error}");
                    }
                    count.fetch_sub(1, Ordering::Relaxed);
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(15))
            }
            Err(e) => return Err(e.into()),
        }
    }
    fs::remove_file(dir.join("holder.sock"))?;
    fs::remove_file(dir.join("herdr-api.sock"))?;
    Ok(())
}

fn connection(mut stream: UnixStream, shared: Shared) -> Result<()> {
    let caller = Identity::read(protocol::peer(&stream)?)?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    let hello = protocol::read(&mut stream)?;
    if let Err(error) = hello.validate_payload() {
        protocol::write(
            &mut stream,
            &hello.error("invalid_frame", &error.to_string()),
        )?;
        return Ok(());
    }
    {
        let state = shared.lock().map_err(|_| "holder mutex poisoned")?;
        let error = if hello.major != 1 {
            Some((
                "incompatible_protocol",
                format!(
                    "wire major 1 required; compatible client: {}",
                    state.meta.executable.display()
                ),
            ))
        } else if hello.minor != 0 {
            Some(("incompatible_protocol", "unsupported minor/features".into()))
        } else if hello.kind != "hello"
            || hello.holder_id != state.meta.id
            || hello.payload["capability"].as_str() != Some(&state.meta.capability)
        {
            Some(("unauthorized", "holder capability required".into()))
        } else {
            None
        };
        if let Some((code, message)) = error {
            protocol::write(&mut stream, &hello.error(code, &message))?;
            return Ok(());
        }
        protocol::write(&mut stream,&hello.reply("welcome",json!({"major":1,"minor":0,"features":["snapshot-sha256","lease-v1","binding-proof-v1","terminal-sideband-v1","theme-v1"],"generation":state.meta.generation,"version":state.meta.version,"state":state.meta.state,"child":state.meta.child})))?;
    }
    let owner = random_id()?;
    let result = (|| -> Result<()> {
        loop {
            let frame = protocol::read(&mut stream)?;
            if let Err(error) = frame.validate_payload() {
                protocol::write(
                    &mut stream,
                    &frame.error(error_code(&error.to_string()), &error.to_string()),
                )?;
                continue;
            }
            let mut state = shared.lock().map_err(|_| "holder mutex poisoned")?;
            if frame.major != 1 || frame.minor != 0 || frame.holder_id != state.meta.id {
                protocol::write(
                    &mut stream,
                    &frame.error("invalid_frame", "connection envelope changed"),
                )?;
                break;
            }
            if frame.kind == "snapshot" || frame.kind == "resync" {
                let terminal = state
                    .terminal
                    .lock()
                    .map_err(|_| "terminal mutex poisoned")?;
                let snapshot = terminal.snapshot();
                let revision = terminal.revision;
                let generation = state.meta.generation;
                let lifecycle = state.meta.state.clone();
                let sideband = terminal
                    .sideband
                    .since(frame.payload["sideband_after"].as_u64());
                drop(terminal);
                drop(state);
                let checksum = format!("{:x}", Sha256::digest(snapshot.as_bytes()));
                let chunks = snapshot.as_bytes().chunks(49152);
                let total = chunks.len();
                for (sequence, chunk) in chunks.enumerate() {
                    let mut response=frame.reply("render",json!({"sequence":sequence,"total":total,"revision":revision,"data":B64.encode(chunk),"checksum":checksum,"state":lifecycle,"sideband":sideband}));
                    response.generation = generation;
                    protocol::write(&mut stream, &response)?;
                }
                continue;
            }
            if frame.kind == "bind" {
                let result =
                    state.bind_automatic(serde_json::from_value(frame.payload.clone())?, &caller);
                match result {
                    Ok(payload) => {
                        let mut reply = frame.reply("bound", payload);
                        reply.generation = state.meta.generation;
                        protocol::write(&mut stream, &reply)?;
                    }
                    Err(error) => protocol::write(
                        &mut stream,
                        &frame.error("invalid_binding", &error.to_string()),
                    )?,
                }
                continue;
            }
            let response = dispatch(&mut state, &owner, &frame);
            match response {
                Ok((kind, payload)) => {
                    if kind == "lease" {
                        state.meta.attachment = Some(caller.clone());
                        state.meta.save(&state.paths)?;
                    }
                    let mut response = frame.reply(kind, payload);
                    response.generation = state.meta.generation;
                    protocol::write(&mut stream, &response)?;
                }
                Err(error) => protocol::write(
                    &mut stream,
                    &frame.error(error_code(&error.to_string()), &error.to_string()),
                )?,
            }
        }
        Ok(())
    })();
    shared
        .lock()
        .map_err(|_| "holder mutex poisoned")?
        .detached(&owner)?;
    result
}

fn error_code(error: &str) -> &str {
    for code in [
        "unauthorized",
        "stale_generation",
        "stale_input",
        "lease_held",
        "invalid_binding",
        "incompatible_protocol",
        "unknown_message",
    ] {
        if error.starts_with(code) {
            return code;
        }
    }
    "invalid_frame"
}

fn dispatch(state: &mut State, owner: &str, frame: &Frame) -> Result<(&'static str, Value)> {
    match frame.kind.as_str() {
        "spawn" => Ok(("spawned", state.spawn(frame.payload.clone())?)),
        "bind-confirmed" => Ok((
            "bound",
            state.bind(serde_json::from_value(frame.payload.clone())?)?,
        )),
        "acquire" => Ok(("lease", state.acquire(owner)?)),
        "input" => {
            state.check_lease(owner, frame.generation, &frame.payload)?;
            state.input(&frame.payload)?;
            Ok(("ack", json!({})))
        }
        "resize" => {
            state.check_lease(owner, frame.generation, &frame.payload)?;
            state.resize(&frame.payload)?;
            Ok(("ack", json!({})))
        }
        "theme" => {
            state.check_lease(owner, frame.generation, &frame.payload)?;
            state
                .terminal
                .lock()
                .map_err(|_| "terminal mutex poisoned")?
                .set_theme(serde_json::from_value(frame.payload["colors"].clone())?)?;
            Ok(("ack", json!({})))
        }
        "status" => Ok(("status_result", {
            let terminal = state
                .terminal
                .lock()
                .map_err(|_| "terminal mutex poisoned")?;
            json!({"id":state.meta.id,"version":state.meta.version,"child":state.meta.child,"state":state.meta.state,"attached":state.lease.is_some(),"reason":state.meta.reason,"binding":state.meta.binding,"generation":state.meta.generation,"resume_status":state.meta.resume_status,"features":["binding-proof-v1","terminal-sideband-v1","theme-v1"],"terminal":{"theme_policy":terminal.theme_policy,"query_error":terminal.query_error,"sideband_policy":"bounded audited OSC7 cwd, OSC133 boundaries, OSC0/2 title, live OSC52 clipboard writes; reconnect restores latest cwd/title, not historical commands or clipboard writes","sideband_sequence":terminal.sideband.sequence},"cleanup":format!("herdr-shield kill {}",state.meta.id)})
        })),
        "detach" => {
            state.detached(owner)?;
            Ok(("ack", json!({})))
        }
        "ack" => Ok(("ack", json!({}))),
        "terminate" => {
            if frame.payload["confirm"].as_str() != Some(&state.meta.id)
                || frame.payload["capability"].as_str() != Some(&state.meta.capability)
            {
                return fail("unauthorized: exact holder confirmation and capability required");
            }
            let identity: Identity = serde_json::from_value(frame.payload["identity"].clone())?;
            if state.meta.child.as_ref() != Some(&identity) || identity.life() == Life::Unknown {
                return fail("unauthorized: child identity mismatch or unverifiable");
            }
            if let Some(child) = state.child.as_mut() {
                if child.try_wait()?.is_none() {
                    child.kill()?;
                    child.wait()?;
                }
            }
            state.meta.state = "terminated".into();
            state.meta.reason = "explicit confirmed kill".into();
            state.meta.save(&state.paths)?;
            state.stopping = true;
            Ok(("terminated", json!({})))
        }
        _ => fail("unknown_message: unsupported holder operation"),
    }
}

pub fn client(paths: &Paths, meta: &Metadata) -> Result<UnixStream> {
    let mut stream = UnixStream::connect(paths.holder_dir(&meta.id)?.join("holder.sock"))?;
    let pid = protocol::peer(&stream)?;
    if meta
        .holder
        .as_ref()
        .is_none_or(|identity| identity.pid != pid || identity.life() != Life::Live)
    {
        return fail("holder identity mismatch; refused connection");
    }
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    protocol::exchange(
        &mut stream,
        &Frame::new(
            &meta.id,
            "hello",
            json!({"capability":meta.capability,"features":["snapshot-sha256","lease-v1"]}),
        ),
    )?;
    Ok(stream)
}
