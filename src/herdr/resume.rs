use std::path::Path;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::holder::{
    Shared,
    registry::{Metadata, Resume},
};
use crate::lifetime::identity::{Identity, Life};
use crate::{Result, fail};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Accepted {
    source: String,
    #[serde(default)]
    agent: String,
    argv: Vec<String>,
}

fn saved(meta: &Metadata) -> Result<(Option<Accepted>, String)> {
    if meta
        .binding
        .socket
        .file_name()
        .is_none_or(|name| name != "herdr.sock")
    {
        return fail("upstream snapshot location is not proven for this endpoint");
    }
    let path = meta
        .binding
        .socket
        .parent()
        .ok_or("missing endpoint parent")?
        .join("session.json");
    let (snapshot, revision) = crate::config::observe_json(&path)?;
    if snapshot["version"] != 3 {
        return fail("unsupported upstream snapshot version");
    }
    let suffix = meta
        .binding
        .pane
        .split_once(":p")
        .ok_or("invalid pane identity")?
        .1;
    let number = suffix.bytes().try_fold(0usize, |n, ch| {
        let digit = b"123456789ABCDEFGHJKMNPQRSTVWXYZ0"
            .iter()
            .position(|v| *v == ch)
            .ok_or("invalid pane number")?;
        n.checked_mul(32)
            .and_then(|n| n.checked_add(digit + 1))
            .ok_or("pane number overflow")
    })?;
    for workspace in snapshot["workspaces"]
        .as_array()
        .ok_or("missing snapshot workspaces")?
    {
        if workspace["id"] != meta.binding.workspace {
            continue;
        }
        let raw = workspace["public_pane_numbers"]
            .as_object()
            .ok_or("missing pane number map")?
            .iter()
            .find_map(|(raw, value)| (value.as_u64() == Some(number as u64)).then_some(raw));
        if let Some(raw) = raw {
            for tab in workspace["tabs"]
                .as_array()
                .ok_or("missing snapshot tabs")?
            {
                if let Some(pane) = tab["panes"].get(raw) {
                    return Ok((
                        if pane["agent_resume"].is_null() {
                            None
                        } else {
                            Some(serde_json::from_value(pane["agent_resume"].clone())?)
                        },
                        revision,
                    ));
                }
            }
        }
    }
    fail("upstream snapshot does not establish this pane's accepted resume")
}

fn ancestor(mut pid: u32, root: u32) -> bool {
    for _ in 0..64 {
        if pid == root {
            return true;
        }
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            return false;
        };
        let Some((_, tail)) = stat.rsplit_once(')') else {
            return false;
        };
        let Some(parent) = tail
            .split_whitespace()
            .nth(1)
            .and_then(|v| v.parse::<u32>().ok())
        else {
            return false;
        };
        if parent == pid || parent == 0 {
            return false;
        }
        pid = parent;
    }
    false
}

fn original_program(pid: u32, command: &str) -> Option<()> {
    if command.is_empty() || command.contains('/') {
        return None;
    }
    let env = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
    if env.len() > 1024 * 1024 {
        return None;
    }
    let path = env
        .split(|byte| *byte == 0)
        .find_map(|entry| entry.strip_prefix(b"PATH="))
        .and_then(|path| std::str::from_utf8(path).ok())?;
    let cwd = std::fs::read_link(format!("/proc/{pid}/cwd")).ok()?;
    let candidate = path
        .split(':')
        .map(|directory| cwd.join(directory).join(command))
        .find(|file| {
            std::fs::metadata(file).is_ok_and(|meta| {
                use std::os::unix::fs::PermissionsExt;
                meta.is_file() && meta.permissions().mode() & 0o111 != 0
            })
        })?
        .canonicalize()
        .ok()?;
    let executable = std::fs::read_link(format!("/proc/{pid}/exe"))
        .ok()?
        .canonicalize()
        .ok()?;
    if candidate == executable {
        return Some(());
    }
    // A kernel shebang launch has the script in argv[1]. Never mistake an
    // arbitrary helper or the caller's supplied resume argument for that script.
    let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let script = std::str::from_utf8(cmdline.split(|byte| *byte == 0).nth(1)?).ok()?;
    if cwd.join(script).canonicalize().ok()? != candidate {
        return None;
    }
    use std::io::Read;
    let mut header = Vec::new();
    std::fs::File::open(candidate)
        .ok()?
        .take(256)
        .read_to_end(&mut header)
        .ok()?;
    let interpreter = std::str::from_utf8(&header)
        .ok()?
        .strip_prefix("#!")?
        .lines()
        .next()?
        .split_whitespace()
        .next()?;
    (Path::new(interpreter).is_absolute()
        && Path::new(interpreter).canonicalize().ok()? == executable)
        .then_some(())
}

pub fn provenance(
    state: &crate::holder::state::State,
    peer: u32,
    argv: &[String],
) -> Option<Identity> {
    let child = state.meta.child.as_ref()?;
    let foreground = u32::try_from(state.master.as_ref()?.process_group_leader()?).ok()?;
    if !ancestor(foreground, child.pid) || !ancestor(peer, foreground) {
        return None;
    }
    let identity = Identity::read(foreground).ok()?;
    if identity.life() != Life::Live {
        return None;
    }
    let cgroup = std::fs::read_to_string(format!("/proc/{foreground}/cgroup")).ok()?;
    if !crate::lifetime::systemd::escaped(&cgroup, &state.meta.unit) {
        return None;
    }
    original_program(foreground, argv.first()?)?;
    (Identity::read(foreground).ok()? == identity).then_some(identity)
}

/// The request to forward, tracked for resume capture. A tracking failure is recorded in
/// resume_status and the request is still forwarded, uncaptured.
pub fn track(shared: &Shared, request: &Value, peer: u32) -> Value {
    track_capture(shared, request, peer).unwrap_or_else(|error| {
        let status = format!(
            "resume capture tracking failed: {error}; request forwarded, no fallback captured"
        );
        match shared.lock() {
            Ok(mut state) => state.meta.resume_status = status,
            Err(_) => eprintln!("herdr-shield: {status}"),
        }
        let mut untracked = request.clone();
        untracked["_capture"] = json!({"before":null});
        untracked
    })
}

fn track_capture(shared: &Shared, request: &Value, peer: u32) -> Result<Value> {
    let mut tracked = request.clone();
    if !matches!(
        request["method"].as_str(),
        Some("pane.report_agent" | "pane.report_agent_session")
    ) {
        return Ok(tracked);
    }
    let mut state = shared.lock().map_err(|_| "holder mutex poisoned")?;
    if request["params"]["pane_id"] != state.meta.binding.pane {
        return Ok(tracked);
    }
    let source = request["params"]["source"]
        .as_str()
        .ok_or("missing report source")?;
    let seq = request["params"]["seq"].as_u64();
    let fresh = (state.meta.report_sequences.contains_key(source)
        || state.meta.report_sequences.len() < 128)
        && state
            .meta
            .report_sequences
            .get(source)
            .is_none_or(|old| match (*old, seq) {
                (Some(a), Some(b)) => b > a,
                (Some(_), None) => false,
                _ => true,
            });
    if fresh {
        state.meta.report_sequences.insert(source.into(), seq);
        state.persist();
    }
    let argv = serde_json::from_value::<Vec<String>>(request["params"]["resume_argv"].clone()).ok();
    let process = argv
        .as_ref()
        .and_then(|argv| provenance(&state, peer, argv));
    let before = match saved(&state.meta) {
        Ok(saved) => Some(saved),
        Err(error) => {
            state.meta.resume_status = format!("accepted resume cannot be verified: {error}");
            state.persist();
            None
        }
    };
    let previous = state
        .meta
        .resume
        .take()
        .or_else(|| state.meta.pending_resume.clone());
    state.meta.pending_resume = previous.clone();
    state.meta.report_epoch = state
        .meta
        .report_epoch
        .checked_add(1)
        .ok_or("report epoch exhausted")?;
    if before.is_some() {
        state.meta.resume_status =
            "report arbitration pending; no confirmed fallback offered".into();
    }
    state.persist();
    tracked["_capture"] = json!({"fresh":fresh,"process":process,"before":before,"previous":previous,"epoch":state.meta.report_epoch});
    Ok(tracked)
}

pub fn observe(shared: &Shared, request: &Value, response: &Value, generation: u64) -> Result<()> {
    if !matches!(
        request["method"].as_str(),
        Some("pane.report_agent" | "pane.report_agent_session")
    ) {
        return Ok(());
    }
    // Unit callers use this same seam; live requests are tracked before forwarding.
    let tracked = if request["_capture"].is_null() {
        track(shared, request, 0)
    } else {
        request.clone()
    };
    if tracked["_capture"]["before"].is_null() {
        return Ok(());
    }
    // Snapshot persistence is debounced upstream. Never hold up its response,
    // and never accumulate an unbounded worker per API report.
    let job = (
        std::sync::Arc::clone(shared),
        tracked,
        response.clone(),
        generation,
    );
    {
        let mut workers = WORKERS
            .lock()
            .map_err(|_| "capture worker mutex poisoned")?;
        let active = workers.0;
        if active >= 4 {
            // The newest report waits for the first free worker; the busy ones serve
            // superseded epochs and end at their next check.
            workers.1 = Some(job);
            return Ok(());
        }
        workers.0 += 1;
    }
    let worker = std::thread::Builder::new()
        .name("herdr-capture".into())
        .spawn(move || {
            let mut job = Some(job);
            while let Some((shared, tracked, response, generation)) = job.take() {
                let _ = observe_saved(&shared, &tracked, &response, generation);
                let Ok(mut workers) = WORKERS.lock() else {
                    return;
                };
                job = workers.1.take();
                if job.is_none() {
                    workers.0 -= 1;
                }
            }
        });
    if let Err(error) = worker {
        if let Ok(mut workers) = WORKERS.lock() {
            workers.0 -= 1;
        }
        return Err(format!("accepted-state verification worker could not start: {error}").into());
    }
    Ok(())
}

/// One capture job: the holder, its tracked request, the upstream response and generation.
type Capture = (Shared, Value, Value, u64);

/// Running capture workers, and the newest report waiting for one.
static WORKERS: std::sync::Mutex<(usize, Option<Capture>)> = std::sync::Mutex::new((0, None));

fn observe_saved(
    shared: &Shared,
    request: &Value,
    response: &Value,
    generation: u64,
) -> Result<()> {
    let tracked = request;
    let meta = shared
        .lock()
        .map_err(|_| "holder mutex poisoned")?
        .meta
        .clone();
    if meta.generation != generation {
        return fail("stale_generation");
    }
    let candidate = serde_json::from_value::<Vec<String>>(request["params"]["resume_argv"].clone())
        .ok()
        .filter(|argv| super::relay::valid_resume(argv))
        .map(|argv| Accepted {
            argv,
            source: request["params"]["source"].as_str().unwrap_or("").into(),
            agent: request["params"]["agent"].as_str().unwrap_or("").into(),
        });
    let before: Option<(Option<Accepted>, String)> =
        serde_json::from_value(tracked["_capture"]["before"].clone()).unwrap_or(None);
    let deadline = Instant::now() + Duration::from_secs(7);
    loop {
        {
            let state = shared.lock().map_err(|_| "holder mutex poisoned")?;
            if Some(state.meta.report_epoch) != tracked["_capture"]["epoch"].as_u64() {
                return Ok(());
            }
        }
        if let Ok((actual, revision)) = saved(&meta) {
            if before.as_ref().is_some_and(|(_, old)| *old != revision) {
                let upstream = super::connect(&meta.binding.socket)?;
                if Identity::read(crate::holder::protocol::peer(&upstream)?)? != meta.binding.server
                {
                    return fail("upstream instance changed during accepted-state verification");
                }
                let mut state = shared.lock().map_err(|_| "holder mutex poisoned")?;
                if state.meta.generation != generation {
                    return fail("stale_generation");
                }
                if Some(state.meta.report_epoch) != tracked["_capture"]["epoch"].as_u64() {
                    return Ok(());
                }
                let previous: Option<Resume> =
                    serde_json::from_value(tracked["_capture"]["previous"].clone()).unwrap_or(None);
                let matches_old = previous.as_ref().is_some_and(|old| {
                    actual.as_ref().is_some_and(|slot| {
                        slot.source == old.source
                            && slot.agent == old.agent
                            && slot.argv == old.argv
                    })
                });
                state.meta.resume = if matches_old { previous } else { None };
                if tracked["_capture"]["fresh"] == true
                    && response["error"].is_null()
                    && candidate.as_ref() == actual.as_ref()
                    && before.as_ref().is_some_and(|(old, _)| old != &actual)
                {
                    if let Some(slot) = actual {
                        state.meta.resume = Some(Resume {
                            argv: slot.argv,
                            source: slot.source,
                            agent: slot.agent,
                            seq: request["params"]["seq"].as_u64(),
                            process: serde_json::from_value(tracked["_capture"]["process"].clone())
                                .unwrap_or(None),
                        });
                    }
                }
                state.meta.resume_status=if state.meta.resume.is_some(){"accepted persisted upstream resume observed; original-process proof recorded only when independently established"}else{"upstream accepted state differs; no fallback captured"}.into();
                state.meta.pending_resume = None;
                state.meta.save(&state.paths)?;
                return Ok(());
            }
        }
        if before.is_none() || Instant::now() >= deadline {
            let mut state = shared.lock().map_err(|_| "holder mutex poisoned")?;
            if state.meta.generation == generation
                && Some(state.meta.report_epoch) == tracked["_capture"]["epoch"].as_u64()
            {
                state.meta.resume_status="no independently observed accepted-slot mutation before persistence deadline; no fallback captured".into();
                state.meta.save(&state.paths)?;
            }
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_slot_and_epoch_not_ok_control_capture_and_partial_mutation() {
        use crate::{
            config::{Paths, atomic_json, random_id, secure_dir},
            holder::{registry::Binding, state::State},
            terminal::{Size, Terminal},
        };
        use std::sync::{Arc, Mutex};
        let root = std::env::temp_dir().join(format!("resume-{}", random_id().unwrap()));
        secure_dir(&root).unwrap();
        let _upstream = std::os::unix::net::UnixListener::bind(root.join("herdr.sock")).unwrap();
        let paths = Paths {
            home: root.clone(),
            config: root.clone(),
            state: root.clone(),
            runtime: root.clone(),
        };
        let binding = Binding {
            socket: root.join("herdr.sock"),
            pane: "w1:p1".into(),
            tab: "w1:t1".into(),
            workspace: "w1".into(),
            server: Identity::read(std::process::id()).unwrap(),
        };
        let shared = Arc::new(Mutex::new(State {
            paths,
            meta: Metadata::new(binding, "/".into()).unwrap(),
            terminal: Arc::new(Mutex::new(Terminal::new(Size::new(24, 80).unwrap()))),
            writer: None,
            master: None,
            child: None,
            lease: None,
            stopping: false,
        }));
        let save = |slot: Value, extra: &str| {
            atomic_json(&root.join("session.json"),&json!({"version":3,"workspaces":[{"id":"w1","public_pane_numbers":{"9":1},"tabs":[{"panes":{"9":{"agent_resume":slot,"agent_session":extra}}}]}]})).unwrap()
        };
        let request = |seq, argv| json!({"method":"pane.report_agent","params":{"pane_id":"w1:p1","source":"custom:dummy","agent":"dummy","seq":seq,"resume_argv":["dummy",argv]}});
        assert_eq!(
            serde_json::from_value::<Accepted>(
                json!({"source":"custom:dummy","argv":["dummy","no-agent"]})
            )
            .unwrap(),
            Accepted {
                source: "custom:dummy".into(),
                agent: String::new(),
                argv: vec!["dummy".into(), "no-agent".into()],
            },
            "a slot without agent is still an accepted slot"
        );
        let untracked = track(
            &shared,
            &json!({"method":"pane.report_agent","params":{"pane_id":"w1:p1","seq":1}}),
            0,
        );
        assert_eq!(untracked["_capture"], json!({"before":null}));
        assert_eq!(
            shared.lock().unwrap().meta.resume_status,
            "resume capture tracking failed: missing report source; request forwarded, no fallback captured"
        );
        std::fs::write(root.join("not-a-directory"), b"").unwrap();
        shared.lock().unwrap().paths.state = root.join("not-a-directory");
        // A registry save that fails costs the record, never the capture: the confirmed
        // fallback is demoted to pending before the report reaches Herdr.
        save(Value::Null, "initial");
        shared.lock().unwrap().meta.resume = Some(Resume {
            argv: vec!["dummy".into(), "confirmed".into()],
            process: None,
            source: "custom:unsaved".into(),
            agent: "dummy".into(),
            seq: Some(0),
        });
        let unsaved = track(
            &shared,
            &json!({"method":"pane.report_agent","params":{"pane_id":"w1:p1","source":"custom:unsaved","seq":1}}),
            0,
        );
        {
            let mut state = shared.lock().unwrap();
            assert_eq!(
                (
                    unsaved["_capture"]["previous"]["argv"].clone(),
                    state.meta.resume.is_none(),
                    state
                        .meta
                        .pending_resume
                        .as_ref()
                        .map(|resume| resume.argv.clone()),
                    state.meta.resume_status.as_str()
                ),
                (
                    json!(["dummy", "confirmed"]),
                    true,
                    Some(vec!["dummy".into(), "confirmed".into()]),
                    "report arbitration pending; no confirmed fallback offered"
                )
            );
            state.meta.pending_resume = None;
            state.paths.state = root.clone();
        }
        let first = track(&shared, &request(1, "first"), 0);
        let newer = track(&shared, &request(2, "second"), 0);
        save(
            json!({"source":"custom:dummy","agent":"dummy","argv":["dummy","first"]}),
            "old-epoch",
        );
        observe_saved(&shared, &first, &json!({"result":{"type":"ok"}}), 1).unwrap();
        assert!(
            shared.lock().unwrap().meta.resume.is_none(),
            "late old epoch must never reauthorize fallback"
        );
        save(
            json!({"source":"custom:dummy","agent":"dummy","argv":["dummy","second"]}),
            "current-epoch",
        );
        observe_saved(&shared, &newer, &json!({"result":{"type":"ok"}}), 1).unwrap();
        assert_eq!(
            shared.lock().unwrap().meta.resume.as_ref().unwrap().argv,
            ["dummy", "second"]
        );
        let overlapping = track(&shared, &request(3, "refused"), 0);
        let last = track(&shared, &request(4, "also-refused"), 0);
        save(
            json!({"source":"custom:dummy","agent":"dummy","argv":["dummy","second"]}),
            "unchanged slot after overlapping rejected reports",
        );
        observe_saved(
            &shared,
            &overlapping,
            &json!({"error":{"code":"resume_not_accepted"}}),
            1,
        )
        .unwrap();
        observe_saved(
            &shared,
            &last,
            &json!({"error":{"code":"resume_not_accepted"}}),
            1,
        )
        .unwrap();
        assert_eq!(
            shared
                .lock()
                .unwrap()
                .meta
                .resume
                .as_ref()
                .map(|resume| resume.argv.clone()),
            Some(vec!["dummy".into(), "second".into()]),
            "overlapping reports must retain the independently confirmed unchanged fallback"
        );
        let partial = track(&shared, &request(5, "refused"), 0);
        assert!(
            shared.lock().unwrap().meta.resume.is_none(),
            "pending arbitration must suppress stale fallback immediately"
        );
        save(Value::Null, "partial mutation despite native error");
        observe_saved(
            &shared,
            &partial,
            &json!({"error":{"code":"resume_not_accepted"}}),
            1,
        )
        .unwrap();
        assert!(shared.lock().unwrap().meta.resume.is_none());
        let stale = track(&shared, &request(2, "stale"), 0);
        save(
            json!({"source":"custom:dummy","agent":"dummy","argv":["dummy","stale"]}),
            "unrelated mutation",
        );
        observe_saved(&shared, &stale, &json!({"result":{"type":"ok"}}), 1).unwrap();
        assert!(shared.lock().unwrap().meta.resume.is_none());
        let pending = track(&shared, &request(6, "pending"), 0);
        for _ in 0..128 {
            observe(&shared, &pending, &json!({"result":{"type":"ok"}}), 1).unwrap();
        }
        let active = || {
            std::fs::read_dir("/proc/self/task")
                .unwrap()
                .filter_map(|entry| std::fs::read_to_string(entry.ok()?.path().join("comm")).ok())
                .filter(|name| name.trim() == "herdr-capture")
                .count()
        };
        let started = Instant::now() + Duration::from_secs(2);
        // Settled once every started worker has named its thread.
        while active() < 4 || active() != WORKERS.lock().unwrap().0 {
            assert!(
                Instant::now() < started,
                "capture workers were not independently enumerated"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            active(),
            4,
            "capture report flood exceeded four independently observed worker threads"
        );
        // The newest report, arriving while four workers serve a superseded epoch, is still captured.
        let newest = track(&shared, &request(7, "newest"), 0);
        observe(&shared, &newest, &json!({"result":{"type":"ok"}}), 1).unwrap();
        save(
            json!({"source":"custom:dummy","agent":"dummy","argv":["dummy","newest"]}),
            "newest report",
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while shared
            .lock()
            .unwrap()
            .meta
            .resume
            .as_ref()
            .map(|resume| resume.argv.clone())
            != Some(vec!["dummy".into(), "newest".into()])
        {
            assert!(
                Instant::now() < deadline,
                "newest report never captured: {}",
                shared.lock().unwrap().meta.resume_status
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while active() != 0 {
            assert!(
                Instant::now() < deadline,
                "stale capture workers did not end"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn helper_or_remote_process_is_not_ancestry_proof() {
        assert!(!super::ancestor(0, std::process::id()));
        assert!(super::ancestor(std::process::id(), std::process::id()));
    }
}
