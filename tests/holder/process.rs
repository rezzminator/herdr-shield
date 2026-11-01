#[path = "coverage.rs"]
mod coverage;
#[path = "../jail.rs"]
mod jail;
#[path = "plain_shell.rs"]
mod plain_shell;

use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::time::Duration;

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use jail::{Jail, wait};
use serde_json::{Value, json};

fn frame(meta: &Value, kind: &str, generation: u64, payload: Value) -> Value {
    json!({"major":1,"minor":0,"request_id":0,"type":kind,"holder_id":meta["id"],"generation":generation,"payload":payload})
}
fn send(stream: &mut UnixStream, value: Value) {
    let bytes = serde_json::to_vec(&value).unwrap();
    stream.write_all(b"HDRKEEP1").unwrap();
    stream
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .unwrap();
    stream.write_all(&bytes).unwrap();
}
fn read(stream: &mut UnixStream) -> Value {
    let mut header = [0u8; 12];
    stream.read_exact(&mut header).unwrap();
    assert_eq!(&header[..8], b"HDRKEEP1");
    let length = u32::from_be_bytes(header[8..].try_into().unwrap()) as usize;
    assert!(length <= 1024 * 1024);
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes).unwrap();
    serde_json::from_slice(&bytes).unwrap()
}
fn connect(jail: &Jail, meta: &Value) -> UnixStream {
    let socket = jail
        .root
        .join("run/herdr-shield")
        .join(meta["id"].as_str().unwrap())
        .join("holder.sock");
    assert!(socket.starts_with(&jail.root));
    let mut stream = UnixStream::connect(socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    send(
        &mut stream,
        frame(meta, "hello", 0, json!({"capability":meta["capability"]})),
    );
    assert_eq!(read(&mut stream)["type"], "welcome");
    stream
}
fn rpc(
    stream: &mut UnixStream,
    meta: &Value,
    kind: &str,
    generation: u64,
    payload: Value,
) -> Value {
    send(stream, frame(meta, kind, generation, payload));
    read(stream)
}
fn snapshot(stream: &mut UnixStream, meta: &Value) -> String {
    send(stream, frame(meta, "snapshot", 0, json!({})));
    let mut bytes = Vec::new();
    loop {
        let frame = read(stream);
        assert_eq!(frame["type"], "render");
        bytes.extend(
            B64.decode(frame["payload"]["data"].as_str().unwrap())
                .unwrap(),
        );
        if frame["payload"]["sequence"].as_u64().unwrap() + 1
            == frame["payload"]["total"].as_u64().unwrap()
        {
            break;
        }
    }
    String::from_utf8(bytes).unwrap()
}

#[test]
fn black_box_holder_survives_attach_loss_leases_redraw_and_explicit_kill() {
    let jail = Jail::new("h");
    let output = jail.run(&[]);
    let meta = jail.metadata();
    assert_eq!(
        meta["state"],
        "running",
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let holder = meta["holder"]["pid"].as_u64().unwrap();
    let child = meta["child"]["pid"].as_u64().unwrap();
    assert!(std::path::Path::new(&format!("/proc/{holder}")).exists());
    assert!(std::path::Path::new(&format!("/proc/{child}")).exists());
    let cgroup = std::fs::read_to_string(format!("/proc/{holder}/cgroup")).unwrap();
    assert!(
        cgroup.contains("user@")
            && cgroup.contains(meta["unit"].as_str().unwrap())
            && !cgroup.contains("session-")
    );
    let socket = jail
        .root
        .join("run/herdr-shield")
        .join(meta["id"].as_str().unwrap())
        .join("holder.sock");
    let mut mismatch = UnixStream::connect(&socket).unwrap();
    let mut hello = frame(&meta, "hello", 0, json!({"capability":meta["capability"]}));
    hello["major"] = json!(2);
    send(&mut mismatch, hello);
    assert_eq!(
        read(&mut mismatch)["payload"]["code"],
        "incompatible_protocol"
    );
    drop(mismatch);
    let mut bad_capability = UnixStream::connect(&socket).unwrap();
    send(
        &mut bad_capability,
        frame(&meta, "hello", 0, json!({"capability":"wrong"})),
    );
    assert_eq!(read(&mut bad_capability)["payload"]["code"], "unauthorized");
    drop(bad_capability);
    let mut first = connect(&jail, &meta);
    let mut wrong_identity = meta["child"].clone();
    wrong_identity["start"] = json!(wrong_identity["start"].as_u64().unwrap() + 1);
    assert_eq!(
        rpc(
            &mut first,
            &meta,
            "terminate",
            0,
            json!({"confirm":meta["id"],"capability":meta["capability"],"identity":wrong_identity})
        )["payload"]["code"],
        "unauthorized"
    );
    let empty = jail.run(&["reconcile"]);
    assert!(empty.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&empty.stdout).unwrap()["enumeration"],
        "EMPTY"
    );
    // An event hint the pinned schema lacks still runs the reconcile it asks for.
    let hinted = jail
        .command()
        .arg("event")
        .env(
            "HERDR_PLUGIN_EVENT_JSON",
            r#"{"event":"pane_closed","data":{"type":"pane_closed"}}"#,
        )
        .output()
        .unwrap();
    assert_eq!(
        (
            serde_json::from_slice::<Value>(&hinted.stdout).unwrap()["enumeration"].clone(),
            String::from_utf8_lossy(&hinted.stderr).into_owned()
        ),
        (
            json!("EMPTY"),
            "herdr-shield: event hint outside the pinned v22 schema (missing field `pane_id` at line 1 column 53); reconciling anyway\n".into()
        )
    );
    assert!(hinted.status.success());
    let failed = jail
        .command()
        .args(["reconcile"])
        .env("HERDR_SOCKET_PATH", jail.root.join("missing.sock"))
        .output()
        .unwrap();
    assert!(!failed.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&failed.stdout).unwrap()["enumeration"],
        "ERROR"
    );
    assert!(std::path::Path::new(&format!("/proc/{child}")).exists());
    // An unreadable last-error record is shown as such; the listing goes on.
    let last_error = jail.root.join("state/herdr-shield/last-error.json");
    std::fs::set_permissions(&last_error, std::fs::Permissions::from_mode(0o644)).unwrap();
    let listed = jail.run(&["list", "--json"]);
    std::fs::set_permissions(&last_error, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(
        listed.status.success(),
        "{}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let listing: Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(
        listing["previous_reconciliation_error"],
        json!({"unreadable":"private file has unsafe ownership or permissions"})
    );
    assert_eq!(listing["holders"][0]["id"], meta["id"]);
    assert_eq!(listing["holders"][0]["orphan"], true);
    assert_eq!(
        listing["holders"][0]["cleanup"],
        format!("herdr-shield kill {}", meta["id"].as_str().unwrap())
    );
    // Doctor names every unreadable record, and a reconcile that ends in their ERROR leaves
    // the last failure record as it was rather than recording a success.
    let unreadable = ["b", "c"].map(|digit| digit.repeat(32));
    for id in &unreadable {
        let record = jail.root.join(format!("state/herdr-shield/{id}.json"));
        std::fs::write(&record, br#"{"schema":999}"#).unwrap();
        std::fs::set_permissions(&record, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let failure = std::fs::read(&last_error).unwrap();
    let earlier = r#"{"error":"earlier","action":"retry reconcile"}"#;
    std::fs::write(&last_error, earlier).unwrap();
    let doctor = jail.run(&["doctor"]);
    let absent = jail.root.join("absent/herdr.sock");
    let unreachable = jail
        .command()
        .env("HERDR_SOCKET_PATH", &absent)
        .arg("doctor")
        .output()
        .unwrap();
    let reconciled = jail.run(&["reconcile"]);
    let [b, c] = &unreadable;
    assert_eq!(
        (
            String::from_utf8_lossy(&unreachable.stderr).into_owned(),
            unreachable.status.success(),
            String::from_utf8_lossy(&doctor.stderr).into_owned(),
            doctor.status.success(),
            String::from_utf8_lossy(&reconciled.stderr).into_owned(),
            reconciled.status.success(),
            std::fs::read_to_string(&last_error).unwrap()
        ),
        (
            format!(
                "herdr-shield: Herdr server not reachable at {}: No such file or directory (os error 2)\n",
                absent.display()
            ),
            false,
            format!(
                "herdr-shield: ERROR: unreadable holder records retained: {b} (missing field `id` at line 1 column 14), {c} (missing field `id` at line 1 column 14)\n"
            ),
            false,
            format!("herdr-shield: ERROR: unreadable holder records retained: {b}, {c}\n"),
            false,
            earlier.to_owned()
        ),
        "last failure record: {}",
        last_error.display()
    );
    std::fs::write(&last_error, failure).unwrap();
    for id in &unreadable {
        std::fs::remove_file(jail.root.join(format!("state/herdr-shield/{id}.json"))).unwrap();
    }
    // Descriptor exhaustion fails accept(); the holder and its API relay keep serving.
    let soft = std::fs::read_to_string(format!("/proc/{holder}/limits"))
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("Max open files"))
        .and_then(|limits| limits.split_whitespace().next().map(str::to_owned))
        .unwrap();
    let limit = |soft: &str| {
        let output = std::process::Command::new("prlimit")
            .args(["--pid", &holder.to_string(), &format!("--nofile={soft}:")])
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output.stderr);
    };
    let switches = || {
        std::fs::read_to_string(format!("/proc/{holder}/task/{holder}/status"))
            .ok()?
            .lines()
            .find_map(|line| line.strip_prefix("voluntary_ctxt_switches:"))?
            .trim()
            .parse::<u64>()
            .ok()
    };
    limit("3");
    let mut queued = UnixStream::connect(&socket).unwrap();
    let mut relay = UnixStream::connect(socket.with_file_name("herdr-api.sock")).unwrap();
    let before = switches().unwrap();
    wait(|| switches().is_none_or(|now| now >= before + 5));
    assert!(
        std::path::Path::new(&format!("/proc/{holder}")).exists(),
        "holder exited on a failed accept"
    );
    limit(&soft);
    queued
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    send(
        &mut queued,
        frame(&meta, "hello", 0, json!({"capability":meta["capability"]})),
    );
    assert_eq!(read(&mut queued)["type"], "welcome");
    drop(queued);
    relay
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    writeln!(
        relay,
        "{}",
        json!({"id":"after-limit","method":"pane.get","params":{"pane_id":"w1:p1"}})
    )
    .unwrap();
    let mut reply = String::new();
    std::io::BufRead::read_line(&mut std::io::BufReader::new(relay), &mut reply).unwrap();
    assert_eq!(
        reply,
        "{\"id\":\"after-limit\",\"result\":{\"type\":\"ok\"}}\n"
    );
    let lease = rpc(&mut first, &meta, "acquire", 0, json!({}));
    let generation = lease["generation"].as_u64().unwrap();
    let token = lease["payload"]["lease"].clone();
    assert_eq!(
        rpc(
            &mut first,
            &meta,
            "resize",
            generation - 1,
            json!({"lease":token,"sequence":1,"rows":24,"cols":80})
        )["payload"]["code"],
        "stale_generation"
    );
    let mut second = connect(&jail, &meta);
    assert_eq!(
        rpc(&mut second, &meta, "acquire", 0, json!({}))["payload"]["code"],
        "lease_held"
    );
    assert_eq!(
        rpc(
            &mut first,
            &meta,
            "input",
            generation - 1,
            json!({"lease":token,"sequence":1,"data":B64.encode(b"tick\n")})
        )["payload"]["code"],
        "stale_generation"
    );
    assert_eq!(
        rpc(
            &mut first,
            &meta,
            "input",
            generation,
            json!({"lease":token,"sequence":1,"data":B64.encode(b"tick\n")})
        )["type"],
        "ack"
    );
    wait(|| snapshot(&mut first, &meta).contains("COUNTER=1"));
    assert_eq!(
        rpc(
            &mut first,
            &meta,
            "input",
            generation,
            json!({"lease":token,"sequence":1,"data":B64.encode(b"tick\n")})
        )["payload"]["code"],
        "stale_input"
    );
    drop(first);
    drop(second);
    std::thread::sleep(Duration::from_millis(100));
    let mut reattached = connect(&jail, &meta);
    let lease = rpc(&mut reattached, &meta, "acquire", 0, json!({}));
    let token = lease["payload"]["lease"].clone();
    assert_eq!(
        rpc(
            &mut reattached,
            &meta,
            "resize",
            generation,
            json!({"lease":token,"sequence":1,"rows":31,"cols":99})
        )["type"],
        "ack"
    );
    assert!(snapshot(&mut reattached, &meta).contains("COUNTER=1"));
    assert_eq!(
        rpc(
            &mut reattached,
            &meta,
            "input",
            generation,
            json!({"lease":token,"sequence":2,"data":B64.encode(b"tick\n")})
        )["type"],
        "ack"
    );
    wait(|| snapshot(&mut reattached, &meta).contains("COUNTER=2"));
    drop(reattached);
    let bad = jail.run(&["kill", meta["id"].as_str().unwrap()]);
    assert!(!bad.status.success());
    // A registry it cannot write never keeps the holder serving a killed child.
    let state = jail.root.join("state/herdr-shield");
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o500)).unwrap();
    let kill = jail.run(&[
        "kill",
        meta["id"].as_str().unwrap(),
        "--confirm",
        meta["id"].as_str().unwrap(),
    ]);
    assert!(
        kill.status.success(),
        "{}",
        String::from_utf8_lossy(&kill.stderr)
    );
    wait(|| !std::path::Path::new(&format!("/proc/{child}")).exists());
    wait(|| !std::path::Path::new(&format!("/proc/{holder}")).exists());
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
    println!(
        "CLEANUP owned child {child}; holder {holder}; unit {}",
        meta["unit"]
    );
}

#[test]
fn unavailable_backend_opens_plain_shell_and_records_unprotected() {
    let jail = Jail::new("u");
    let config = jail.root.join("config/herdr-shield/config.json");
    std::fs::write(
        &config,
        serde_json::to_vec(&json!({"shell":"/bin/sh","args":["-c","printf PLAIN_SHELL"]})).unwrap(),
    )
    .unwrap();
    let output = jail
        .command()
        .env("PATH", "/not-a-program-directory")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("PLAIN_SHELL"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("UNPROTECTED"));
    assert_eq!(jail.metadata()["state"], "unprotected");
}

#[test]
fn nonreading_child_cannot_block_status_or_explicit_kill() {
    let jail = Jail::new("b");
    std::fs::write(jail.root.join("config/herdr-shield/config.json"),
        serde_json::to_vec(&json!({"shell":"/bin/sh","args":["-c","stty raw -echo; printf BLOCKED; exec sleep 30"]})).unwrap()).unwrap();
    let _ = jail.run(&[]);
    let meta = jail.metadata();
    let mut client = connect(&jail, &meta);
    wait(|| snapshot(&mut client, &meta).contains("BLOCKED"));
    let lease = rpc(&mut client, &meta, "acquire", 0, json!({}));
    let generation = lease["generation"].as_u64().unwrap();
    for sequence in 1..=64 {
        let result = rpc(
            &mut client,
            &meta,
            "input",
            generation,
            json!({"lease":lease["payload"]["lease"],"sequence":sequence,"data":B64.encode(vec![b'x';65536])}),
        );
        if result["type"] == "error" {
            break;
        }
    }
    let mut status = connect(&jail, &meta);
    assert_eq!(
        rpc(&mut status, &meta, "status", 0, json!({}))["type"],
        "status_result"
    );
    let killed = jail.run(&[
        "kill",
        meta["id"].as_str().unwrap(),
        "--confirm",
        meta["id"].as_str().unwrap(),
    ]);
    assert!(
        killed.status.success(),
        "{}",
        String::from_utf8_lossy(&killed.stderr)
    );
}

#[test]
fn broken_jail_rejects_outside_socket() {
    let jail = Jail::new("n");
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || jail.guard(std::path::Path::new("/outside.sock"))
        ))
        .is_err()
    );
}

#[test]
fn automatic_default_shell_refuses_detached_public_id_reuse() {
    let jail = Jail::new("r");
    jail.run(&[]);
    let before = jail.metadata();
    let retry = jail.run(&[]);
    assert!(
        String::from_utf8_lossy(&retry.stderr).contains("ambiguous pane instance"),
        "{}",
        String::from_utf8_lossy(&retry.stderr)
    );
    assert_eq!(jail.metadata()["child"], before["child"]);
}
