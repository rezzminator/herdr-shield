use serde_json::{Value, json};

use crate::config::{Paths, atomic_json};
use crate::holder::{
    self,
    protocol::{self, Frame},
    registry::Metadata,
};
use crate::lifetime::identity::Life;
use crate::{Result, fail};

pub const DOORS: &[(&str, &str)] = &[
    (
        "initial workspace / new tab / ordinary split",
        "PROTECTED only when terminal.default_shell creates a verified holder",
    ),
    (
        "workspace.create / tab.create / pane.split",
        "PROTECTED only when terminal.default_shell creates a verified holder",
    ),
    (
        "pane run / send-input",
        "UNPROTECTED unless the existing pane already has a verified holder; input does not adopt processes",
    ),
    (
        "layout.apply without command",
        "PROTECTED only when terminal.default_shell creates a verified holder",
    ),
    (
        "layout.apply explicit command",
        "UNPROTECTED: explicit argv bypasses terminal.default_shell",
    ),
    (
        "plugin pane split / tab / popup / overlay",
        "UNPROTECTED: explicit plugin command bypasses terminal.default_shell",
    ),
    (
        "custom command pane / overlay / split",
        "UNPROTECTED: explicit command bypasses terminal.default_shell",
    ),
    (
        "popup shell-command / argv",
        "UNPROTECTED: explicit popup command bypasses terminal.default_shell",
    ),
    (
        "shell replacement / respawn",
        "UNPROTECTED unless replacement starts through terminal.default_shell and receives a verified holder",
    ),
    (
        "cold snapshot normal / history pane",
        "PROTECTED only when restored default-shell pane reattaches its verified holder",
    ),
    (
        "saved launch_argv",
        "UNPROTECTED when launched outside terminal.default_shell; never replayed by this plugin",
    ),
    (
        "native resume",
        "DISABLED session-wide prerequisite; no automatic agent replay",
    ),
    (
        "live handoff imported PTY",
        "UNPROTECTED unless its original default-shell process already has a verified holder",
    ),
    (
        "preexisting pane / agent at installation",
        "UNPROTECTED: existing PTYs cannot be adopted; deliberately end and replace the workload",
    ),
];

pub fn enumerate(result: Value) -> Result<Vec<super::schema::PaneInfo>> {
    let typed: super::schema::ResponseResult = serde_json::from_value(result)?;
    match typed {
        super::schema::ResponseResult::PaneList { panes } => Ok(panes),
        _ => fail("ERROR: enumeration response is not pane_list"),
    }
}

pub fn report(paths: &Paths) -> Result<Value> {
    let metas = Metadata::all(paths)?;
    let endpoint = super::endpoint();
    let enumeration = match &endpoint {
        Ok(socket) => super::rpc(socket, "pane.list", json!({})).and_then(enumerate),
        Err(error) => fail(&error.to_string()),
    };
    let mut panes = Vec::new();
    let mut holders = Vec::new();
    let mut known_processes = Vec::new();
    for meta in &metas {
        let status = holder::client(paths, meta).and_then(|mut stream| {
            protocol::exchange(&mut stream, &Frame::new(&meta.id, "status", json!({})))
        });
        let reason = match &status {
            Ok(frame) => frame.payload["reason"]
                .as_str()
                .unwrap_or("unknown")
                .to_owned(),
            Err(_) => format!("holder unavailable; {}", meta.reason),
        };
        let live = meta
            .holder
            .as_ref()
            .is_some_and(|id| id.life() == Life::Live);
        let matched = enumeration
            .as_ref()
            .ok()
            .is_some_and(|panes| panes.iter().any(|pane| pane.pane_id == meta.binding.pane));
        holders.push(json!({"id":meta.id,"version":meta.version,"wire":meta.wire,"compatible_client":meta.executable,"holder":meta.holder,"child":meta.child,"state":meta.state,"binding":meta.binding,"binding_history":meta.history,"resume_status":meta.resume_status,"terminal":status.as_ref().ok().map(|frame|frame.payload["terminal"].clone()),"orphan":!matched||!status.as_ref().is_ok_and(|frame|frame.payload["attached"]==true),"reason":reason,"holder_live":live,"cleanup":format!("herdr-shield kill {}",meta.id)}));
    }
    if let Ok(enumerated) = &enumeration {
        for pane in enumerated {
            if let Ok(process) = super::rpc(
                endpoint.as_ref().unwrap(),
                "pane.process_info",
                json!({"pane_id":pane.pane_id}),
            ) {
                if let Some(pid) = process["process_info"]["shell_pid"].as_u64() {
                    if let Ok(pid) = u32::try_from(pid) {
                        if let Ok(identity) = crate::lifetime::identity::Identity::read(pid) {
                            known_processes.push(identity);
                        }
                    }
                }
            }
            let candidates: Vec<_> = metas
                .iter()
                .filter(|meta| {
                    meta.binding.pane == pane.pane_id
                        && endpoint
                            .as_ref()
                            .is_ok_and(|socket| *socket == meta.binding.socket)
                })
                .collect();
            let protected = candidates.len() == 1
                && candidates[0].state == "running"
                && candidates[0]
                    .child
                    .as_ref()
                    .is_some_and(|id| id.life() == Life::Live)
                && candidates[0]
                    .holder
                    .as_ref()
                    .is_some_and(|id| id.life() == Life::Live)
                && holder::client(paths, candidates[0])
                    .and_then(|mut stream| {
                        protocol::exchange(
                            &mut stream,
                            &Frame::new(&candidates[0].id, "status", json!({})),
                        )
                    })
                    .is_ok_and(|status| status.payload["attached"] == true);
            let protected = protected
                && super::connect(&candidates[0].binding.socket)
                    .and_then(|stream| {
                        crate::lifetime::identity::Identity::read(protocol::peer(&stream)?)
                    })
                    .is_ok_and(|identity| identity == candidates[0].binding.server)
                && super::rpc(
                    &candidates[0].binding.socket,
                    "pane.process_info",
                    json!({"pane_id":pane.pane_id}),
                )
                .is_ok_and(|process| {
                    candidates[0].attachment.as_ref().is_some_and(|attachment| {
                        attachment.life() == Life::Live
                            && process["process_info"]["shell_pid"].as_u64()
                                == Some(attachment.pid.into())
                    })
                });
            let reason = if protected {
                "verified default-shell holder".into()
            } else if candidates.len() == 1 {
                format!(
                    "holder not positively live/verified: {}",
                    candidates[0].reason
                )
            } else {
                "launch provenance unavailable: process did not positively enter a verified default-shell holder; explicit layout/plugin/custom/popup commands and preexisting panes bypass it".into()
            };
            panes.push(json!({"pane_id":pane.pane_id,"terminal_id":pane.terminal_id,"status":if protected {"PROTECTED"}else{"UNPROTECTED"},"reason":reason}));
        }
    }
    let process_census = endpoint
        .as_ref()
        .map_err(ToString::to_string)
        .and_then(|socket| {
            super::connect(socket)
                .and_then(|stream| {
                    let server =
                        crate::lifetime::identity::Identity::read(protocol::peer(&stream)?)?;
                    super::census::pty_children(&server)
                })
                .map_err(|error| error.to_string())
        });
    if let Ok(processes) = &process_census {
        for process in processes {
            if !known_processes.contains(process) {
                panes.push(json!({"pane_id":null,"process":process,"status":"UNPROTECTED","reason":"PTY absent from public pane enumeration; popup/overlay or unavailable launch provenance; process did not positively enter a verified default-shell holder"}));
            }
        }
    }
    let error = enumeration
        .as_ref()
        .err()
        .map(ToString::to_string)
        .or_else(|| {
            process_census
                .as_ref()
                .err()
                .map(|error| format!("PTY census ERROR: {error}"))
        });
    let persisted = paths.state.join("last-error.json");
    let previous = if persisted.exists() {
        Some(crate::config::read_json::<Value>(&persisted)?)
    } else {
        None
    };
    Ok(
        json!({"enumeration":if error.is_some(){"ERROR"}else if panes.is_empty(){"EMPTY"}else{"OK"},"error":error,"pty_enumeration":{"status":if process_census.is_ok(){"OK"}else{"ERROR"},"observed":process_census.as_ref().ok().map(Vec::len)},"previous_reconciliation_error":previous,"panes":panes,"holders":holders,"coverage":DOORS.iter().map(|(door,status)|json!({"door":door,"status":status})).collect::<Vec<_>>()}),
    )
}

pub fn run(paths: &Paths) -> Result<()> {
    let report = report(paths)?;
    if report["enumeration"] == "ERROR" {
        atomic_json(
            &paths.state.join("last-error.json"),
            &json!({"error":report["error"],"action":"all holders retained; retry reconcile with the correct endpoint"}),
        )?;
        println!("{}", serde_json::to_string_pretty(&report)?);
        return fail("ERROR: pane enumeration failed; all holders retained");
    }
    if paths.state.join("last-error.json").exists() {
        atomic_json(
            &paths.state.join("last-error.json"),
            &json!({"error":null,"action":"last reconciliation succeeded"}),
        )?;
    }
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

pub fn event(paths: &Paths) -> Result<()> {
    let bytes = std::env::var("HERDR_PLUGIN_EVENT_JSON")?;
    if bytes.len() > crate::config::MAX_JSON {
        return fail("event exceeds limit");
    }
    let _: super::schema::EventEnvelope = serde_json::from_str(&bytes)?;
    // Hook environment is an unauthenticated hint, never a termination capability.
    run(paths)
}

pub fn list(paths: &Paths, action: bool) -> Result<()> {
    let report = report(paths)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    if action {
        let count = report["panes"].as_array().map_or(0, Vec::len);
        let message = format!(
            "{count} panes; enumeration {}; inspect plugin log for UNPROTECTED reasons and cleanup commands",
            report["enumeration"]
        );
        let socket = super::endpoint()?;
        super::rpc(
            &socket,
            "notification.show",
            json!({"title":"herdr shield coverage","body":message}),
        )?;
    }
    if report["enumeration"] == "ERROR" {
        return fail("ERROR: pane enumeration failed; holder list retained");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_enumeration_and_failed_enumeration_are_distinct() {
        assert!(
            enumerate(json!({"type":"pane_list","panes":[]}))
                .unwrap()
                .is_empty()
        );
        assert!(enumerate(json!({"type":"ok"})).is_err());
        assert!(enumerate(json!({"type":"pane_list","panes":null})).is_err());
    }
    #[test]
    fn closed_launch_census_visibly_names_every_unprotected_door() {
        assert_eq!(DOORS.len(), 14);
        for word in [
            "explicit command",
            "plugin pane",
            "custom command",
            "popup shell",
            "preexisting",
        ] {
            assert!(
                DOORS
                    .iter()
                    .any(|(door, status)| door.contains(word) && status.starts_with("UNPROTECTED")),
                "{word}"
            );
        }
    }
}
