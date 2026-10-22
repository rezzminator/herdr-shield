use std::io::{BufReader, Write};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use crate::holder::{Shared, protocol};
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
        let pane: super::schema::PaneInfo = serde_json::from_value(result["pane"].clone())?;
        if pane.pane_id != meta.binding.pane
            || pane.workspace_id != meta.binding.workspace
            || pane.tab_id != meta.binding.tab
        {
            return fail("observer restored public identity mismatch; holder retained");
        }
        let binding = crate::holder::registry::Binding {
            server,
            pane: pane.pane_id,
            tab: pane.tab_id,
            workspace: pane.workspace_id,
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
    let mut reader = BufReader::new(socket);
    loop {
        let value: Value = crate::config::decode_json(&super::line(&mut reader)?)?;
        if !value["error"].is_null() {
            return fail("observer subscription refused");
        }
        if !value["event"].is_null() {
            let event: super::schema::EventEnvelope = serde_json::from_value(value)?;
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
                    state.meta.save(&state.paths)?;
                    state.stopping = true;
                    return Ok(());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
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
