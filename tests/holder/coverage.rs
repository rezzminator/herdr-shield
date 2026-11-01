use super::*;
use std::fs;

fn terminal(jail: &Jail, mode: &str) -> Value {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fakes/terminal.py");
    fs::write(
        jail.root.join("config/herdr-shield/config.json"),
        serde_json::to_vec(&json!({"shell":"/usr/bin/python3","args":["-I","-B",fixture,mode]}))
            .unwrap(),
    )
    .unwrap();
    let _ = jail.run(&[]);
    let meta = jail.metadata();
    assert_eq!(meta["state"], "running");
    meta
}

fn rss(pid: u64) -> u64 {
    fs::read_to_string(format!("/proc/{pid}/status"))
        .unwrap()
        .lines()
        .find_map(|line| {
            line.strip_prefix("VmRSS:")
                .map(|tail| tail.split_whitespace().next().unwrap().parse().unwrap())
        })
        .unwrap()
}

#[test]
fn saturated_detached_query_replies_are_delivered_exactly_once() {
    let jail = Jail::new("q");
    let meta = terminal(&jail, "queries");
    let mut client = connect(&jail, &meta);
    wait(|| snapshot(&mut client, &meta).contains("QUERY_READY"));
    let lease = rpc(&mut client, &meta, "acquire", 0, json!({}));
    let mut accepted = 0;
    for sequence in 1..=64 {
        let response = rpc(
            &mut client,
            &meta,
            "input",
            lease["generation"].as_u64().unwrap(),
            json!({"lease":lease["payload"]["lease"],"sequence":sequence,"data":B64.encode(vec![b'x';65536])}),
        );
        if response["type"] == "error" {
            break;
        }
        accepted += 65536;
    }
    assert!(
        accepted >= 65536,
        "ordinary input did not saturate the PTY queue"
    );
    drop(client);
    fs::write(jail.root.join("query-go"), b"go").unwrap();
    wait(|| jail.root.join("query-replies").exists());
    let bytes = fs::read(jail.root.join("query-replies")).unwrap();
    let replies: Vec<_> = bytes.iter().copied().filter(|byte| *byte != b'x').collect();
    assert_eq!(bytes.iter().filter(|byte| **byte == b'x').count(), accepted);
    assert_eq!(
        replies,
        b"\x1b[?6c\x1b[0n\x1b[1;12R\x1b]10;rgb:e5e5/e5e5/e5e5\x07\x1b[8;24;80t"
    );
    println!(
        "PASS detached saturated PTY: {accepted} accepted input bytes; five exact query replies once"
    );
}

#[test]
fn detached_real_output_flood_has_bounded_rss_and_reconnect_redraw() {
    let jail = Jail::new("f");
    let meta = terminal(&jail, "flood");
    let pid = meta["holder"]["pid"].as_u64().unwrap();
    let mut client = connect(&jail, &meta);
    wait(|| snapshot(&mut client, &meta).contains("FLOOD_READY"));
    drop(client);
    let baseline = rss(pid);
    fs::write(jail.root.join("flood-go"), b"go").unwrap();
    let mut peak = baseline;
    let deadline = std::time::Instant::now() + Duration::from_secs(90);
    while !jail.root.join("flood-done").exists() {
        peak = peak.max(rss(pid));
        assert!(
            std::time::Instant::now() < deadline,
            "32 MiB flood did not drain"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut client = connect(&jail, &meta);
    let mut redraw = String::new();
    wait(|| {
        redraw = snapshot(&mut client, &meta);
        redraw.contains("FINAL")
    });
    assert!(
        peak < baseline + 16 * 1024,
        "unbounded resident memory: {baseline} -> {peak} KiB"
    );
    assert!(redraw.len() < 200_000);
    assert!(redraw.contains("\x1b[4;1H      FINAL"));
    assert!(redraw.contains("\x1b[4;12H"));
    for mode in ["?2004h", "?1006h", "?1002h", "?1004h", "?25l"] {
        assert!(redraw.contains(mode), "missing reconnect mode {mode}");
    }
    assert_eq!(jail.metadata()["child"], meta["child"]);
    println!(
        "PASS 32 MiB detached output: RSS baseline={baseline} peak={peak} KiB; bounded redraw with fixed cursor and modes"
    );
}

struct Attachment {
    child: std::process::Child,
    master: std::fs::File,
    slave: std::os::fd::OwnedFd,
    saved: nix::sys::termios::Termios,
}

impl Attachment {
    fn new(jail: &Jail, id: &str) -> Self {
        use std::os::fd::{AsRawFd, FromRawFd};
        let pty = nix::pty::openpty(
            Some(&nix::pty::Winsize {
                ws_row: 32,
                ws_col: 99,
                ws_xpixel: 0,
                ws_ypixel: 0,
            }),
            None,
        )
        .unwrap();
        let saved = nix::sys::termios::tcgetattr(&pty.slave).unwrap();
        // Each duplicated descriptor belongs only to this exact attach child.
        let slave = || unsafe { std::fs::File::from_raw_fd(nix::libc::dup(pty.slave.as_raw_fd())) };
        let child = jail
            .command()
            .args(["attach", id])
            .stdin(slave())
            .stdout(slave())
            .stderr(slave())
            .spawn()
            .unwrap();
        let master = std::fs::File::from(pty.master);
        Self {
            child,
            master,
            slave: pty.slave,
            saved,
        }
    }
    fn output_until(&mut self, predicate: impl Fn(&[u8]) -> bool) -> Vec<u8> {
        use std::os::fd::AsFd;
        let mut output = Vec::new();
        wait(|| {
            let mut descriptors = [nix::poll::PollFd::new(
                self.master.as_fd(),
                nix::poll::PollFlags::POLLIN,
            )];
            nix::poll::poll(&mut descriptors, 100u16).unwrap();
            if descriptors[0]
                .revents()
                .unwrap()
                .contains(nix::poll::PollFlags::POLLIN)
            {
                let mut bytes = [0; 65536];
                let count = self.master.read(&mut bytes).unwrap();
                output.extend_from_slice(&bytes[..count]);
            }
            predicate(&output)
        });
        output
    }
}

impl Drop for Attachment {
    fn drop(&mut self) {
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(self.child.id() as i32),
            nix::sys::signal::Signal::SIGTERM,
        );
        self.child.wait().unwrap();
        assert_eq!(
            nix::sys::termios::tcgetattr(&self.slave).unwrap(),
            self.saved,
            "attach failed to restore outer terminal"
        );
        println!("CLEANUP attach {}", self.child.id());
    }
}

#[test]
fn actual_reconnect_roundtrips_mouse_focus_paste_unicode_modes_theme_and_reset() {
    let jail = Jail::new("i");
    let meta = terminal(&jail, "input");
    let id = meta["id"].as_str().unwrap();
    let mut first = Attachment::new(&jail, id);
    // Up to the end of the frame that set the modes, so its cursor style is in.
    let rendered = first.output_until(|bytes| {
        let text = String::from_utf8_lossy(bytes);
        text.rfind("?1006h")
            .is_some_and(|at| text[at..].contains("?2026l"))
    });
    assert!(String::from_utf8_lossy(&rendered).contains("界e\u{301}"));
    for mode in ["?1049h", "?2004h", "?1002h", "?1004h", "?1h", "\x1b="] {
        assert!(
            String::from_utf8_lossy(&rendered).contains(mode),
            "mode {mode}"
        );
    }
    assert!(String::from_utf8_lossy(&rendered).contains("https://example.invalid/"));
    // The app never styled its cursor: the outer terminal keeps its own default.
    assert!(String::from_utf8_lossy(&rendered).contains("\x1b[0 q"));
    assert!(!String::from_utf8_lossy(&rendered).contains("\x1b[2 q"));
    assert!(!String::from_utf8_lossy(&rendered).contains("REVUQUNIRUQ="));
    first.master.write_all(b"before|").unwrap();
    wait(|| fs::read(jail.root.join("input-transcript")).is_ok_and(|value| value == b"before|"));
    drop(first);
    let mut control = connect(&jail, &meta);
    let lease = rpc(&mut control, &meta, "acquire", 0, json!({}));
    assert_eq!(
        rpc(
            &mut control,
            &meta,
            "theme",
            lease["generation"].as_u64().unwrap(),
            json!({"lease":lease["payload"]["lease"],"sequence":1,"colors":[[256,[18,52,86]]]})
        )["type"],
        "ack"
    );
    drop(control);
    std::thread::sleep(Duration::from_millis(100));
    let mut reattached = Attachment::new(&jail, id);
    let rendered = reattached.output_until(|bytes| {
        let text = String::from_utf8_lossy(bytes);
        text.contains("38;2;18;52;86") && text.contains("?2004h")
    });
    assert!(String::from_utf8_lossy(&rendered).contains("?2004h"));
    let corpus = b"\x1b[200~paste \xce\xb1\n\x1b[201~\x1b[<0;7;4M\x1b[<0;7;4m\x1b[M !!\x1b[I\x1b[O\x1bOA\x1bOpDONE";
    reattached.master.write_all(corpus).unwrap();
    let expected = [b"before|".as_slice(), corpus].concat();
    wait(|| fs::read(jail.root.join("input-transcript")).is_ok_and(|value| value == expected));
    let reset = reattached.output_until(|bytes| {
        let text = String::from_utf8_lossy(bytes);
        text.contains("RESET_OK") && text.contains("\x1b]52;c;TElWRQ==\x1b\\")
    });
    let reset = String::from_utf8_lossy(&reset);
    assert!(!reset.contains("REVUQUNIRUQ="));
    for mode in ["?2004l", "?1006l", "?1002l", "?1004l", "?1l"] {
        assert!(reset.contains(mode), "reset mode {mode}");
    }
    assert_eq!(jail.metadata()["child"], meta["child"]);
    println!(
        "PASS actual reconnect: exact input bytes for paste/Unicode/SGR+legacy mouse/focus/application keys; theme, links, modes, default cursor, live-only clipboard write, reset; same child"
    );
}

#[test]
fn real_old_and_new_executables_coexist_without_replacing_old_holder() {
    use sha2::{Digest, Sha256};
    let jail = Jail::new("v");
    let old = std::env::var_os("HERDR_SHIELD_OLD_BIN")
        .expect("ERROR: set preserved compatible old executable HERDR_SHIELD_OLD_BIN");
    let old = std::path::PathBuf::from(old);
    assert!(old.is_absolute() && old.is_file());
    let new = std::path::Path::new(env!("CARGO_BIN_EXE_herdr-shield"));
    assert_ne!(
        Sha256::digest(fs::read(&old).unwrap()),
        Sha256::digest(fs::read(new).unwrap()),
        "upgrade needs two distinct built artifacts"
    );
    let command = jail.command();
    let output = std::process::Command::new(&old)
        .env_clear()
        .envs(
            command
                .get_envs()
                .filter_map(|(key, value)| value.map(|value| (key, value))),
        )
        .current_dir(&jail.root)
        .output()
        .unwrap();
    let meta = jail.metadata();
    assert_eq!(meta["state"], "running", "{:?}", output.stderr);
    let mut client = Attachment::new(&jail, meta["id"].as_str().unwrap());
    client.output_until(|bytes| String::from_utf8_lossy(bytes).contains("READY"));
    client.master.write_all(b"tick\n").unwrap();
    client.output_until(|bytes| String::from_utf8_lossy(bytes).contains("COUNTER=1"));
    drop(client);
    assert_eq!(jail.metadata()["child"], meta["child"]);
    assert_eq!(jail.metadata()["holder"], meta["holder"]);
    assert_eq!(jail.metadata()["executable"], old.to_str().unwrap());
    let mut connection = UnixStream::connect(
        jail.root
            .join("run/herdr-shield")
            .join(meta["id"].as_str().unwrap())
            .join("holder.sock"),
    )
    .unwrap();
    let mut unsupported = frame(&meta, "hello", 0, json!({"capability":meta["capability"]}));
    unsupported["major"] = json!(2);
    send(&mut connection, unsupported);
    let refusal = read(&mut connection);
    assert_eq!(refusal["payload"]["code"], "incompatible_protocol");
    assert!(refusal.to_string().contains(old.to_str().unwrap()));
    assert_eq!(jail.metadata()["holder"], meta["holder"]);
    let mut compatible = connect(&jail, &meta);
    assert!(snapshot(&mut compatible, &meta).contains("COUNTER=1"));
    println!(
        "PASS distinct compatible old/new executables: new attach drives old holder; incompatible major gives retained compatible path; exact holder/child unchanged"
    );
}

#[test]
fn private_state_attacks_and_content_redaction_preserve_the_live_holder() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let jail = Jail::new("p");
    let secret = "CANARY_SECRET_NEVER_LOG_7b9c";
    // One non-UTF-8 variable costs itself, never the protected shell.
    let first = jail
        .command()
        .env("PRIVATE_TEST_CREDENTIAL", secret)
        .env(
            "NON_UTF8_VALUE",
            <std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(b"\xff"),
        )
        .output()
        .unwrap();
    let meta = jail.metadata();
    let stderr = String::from_utf8_lossy(&first.stderr);
    assert_eq!(meta["state"], "running", "{stderr}");
    assert_eq!(
        stderr.lines().next(),
        Some("herdr-shield: skipped non-UTF-8 environment variable NON_UTF8_VALUE"),
        "{stderr}"
    );
    let id = meta["id"].as_str().unwrap();
    for directory in [
        "config/herdr-shield",
        "state/herdr-shield",
        "run/herdr-shield",
    ] {
        let permissions = fs::metadata(jail.root.join(directory)).unwrap();
        assert_eq!(permissions.mode() & 0o777, 0o700);
        assert_eq!(permissions.uid(), nix::unistd::Uid::current().as_raw());
    }
    let directory = jail.root.join("run/herdr-shield").join(id);
    for socket in ["holder.sock", "herdr-api.sock"] {
        assert_eq!(
            fs::metadata(directory.join(socket)).unwrap().mode() & 0o777,
            0o600
        );
    }
    let mut client = connect(&jail, &meta);
    let lease = rpc(&mut client, &meta, "acquire", 0, json!({}));
    rpc(
        &mut client,
        &meta,
        "input",
        lease["generation"].as_u64().unwrap(),
        json!({"lease":lease["payload"]["lease"],"sequence":1,"data":B64.encode(format!("{secret}\n"))}),
    );
    wait(|| snapshot(&mut client, &meta).contains(secret));
    drop(client);
    for args in [vec!["list", "--json"], vec!["reconcile"], vec!["doctor"]] {
        let result = jail.run(&args);
        assert!(!String::from_utf8_lossy(&result.stdout).contains(secret));
        assert!(!String::from_utf8_lossy(&result.stderr).contains(secret));
        assert!(
            !String::from_utf8_lossy(&result.stdout).contains(meta["capability"].as_str().unwrap())
        );
    }
    for entry in fs::read_dir(jail.root.join("state/herdr-shield")).unwrap() {
        let bytes = fs::read(entry.unwrap().path()).unwrap();
        assert!(
            !String::from_utf8_lossy(&bytes).contains(secret),
            "environment or terminal content persisted"
        );
    }
    let record = jail
        .root
        .join("state/herdr-shield")
        .join(format!("{id}.json"));
    let saved = fs::read(&record).unwrap();
    fs::set_permissions(&record, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(!jail.run(&["list"]).status.success());
    fs::set_permissions(&record, fs::Permissions::from_mode(0o600)).unwrap();
    let moved = record.with_extension("held");
    fs::rename(&record, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, &record).unwrap();
    assert!(!jail.run(&["list"]).status.success());
    fs::remove_file(&record).unwrap();
    fs::rename(&moved, &record).unwrap();
    fs::write(&record, b"{\"schema\":999}").unwrap();
    assert!(!jail.run(&["list"]).status.success());
    assert_eq!(fs::read(&record).unwrap(), b"{\"schema\":999}");
    let mut copy: Value = serde_json::from_slice(&saved).unwrap();
    fs::write(&record, saved).unwrap();
    // A second live record for this pane: no duplicate holder, and the pane still gets its shell.
    let sibling_id = "b".repeat(32);
    let sibling = jail
        .root
        .join("state/herdr-shield")
        .join(format!("{sibling_id}.json"));
    copy["id"] = json!(sibling_id);
    fs::write(&sibling, serde_json::to_vec(&copy).unwrap()).unwrap();
    fs::set_permissions(&sibling, fs::Permissions::from_mode(0o600)).unwrap();
    let ambiguous = jail.run(&[]);
    assert_eq!(
        (
            String::from_utf8_lossy(&ambiguous.stdout).as_ref(),
            String::from_utf8_lossy(&ambiguous.stderr).as_ref()
        ),
        (
            "\r\nPLAIN_READY\n",
            "herdr-shield: UNPROTECTED: invalid_binding: ambiguous existing holders; retained, no duplicate created; opening configured plain shell\n"
        )
    );
    // One unreadable record costs only itself: the rest is still listed.
    fs::write(&sibling, b"{\"schema\":999}").unwrap();
    let listed = jail.run(&["list", "--json"]);
    let listing: Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(
        (
            listing["holders"]
                .as_array()
                .unwrap()
                .iter()
                .map(|holder| holder["id"].clone())
                .collect::<Vec<_>>(),
            listing["unreadable_holders"].clone()
        ),
        (
            vec![meta["id"].clone()],
            json!([{"id":sibling_id,"error":"missing field `id` at line 1 column 14","socket":null,"pane":null}])
        )
    );
    assert_eq!(
        String::from_utf8_lossy(&listed.stderr),
        format!("herdr-shield: ERROR: unreadable holder records retained: {sibling_id}\n")
    );
    assert!(!listed.status.success());
    // It may own this pane, so bootstrap creates no holder and opens the plain shell.
    let blocked = jail.run(&[]);
    assert_eq!(
        (
            String::from_utf8_lossy(&blocked.stdout).as_ref(),
            String::from_utf8_lossy(&blocked.stderr).into_owned()
        ),
        (
            "\r\nPLAIN_READY\n",
            format!(
                "herdr-shield: UNPROTECTED: invalid_binding: unreadable holder record {sibling_id} may own this pane (missing field `id` at line 1 column 14); retained, no duplicate created; opening configured plain shell\n"
            )
        )
    );
    fs::remove_file(&sibling).unwrap();
    for invalid in [
        "../outside",
        "/outside",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/child",
    ] {
        assert!(
            !jail
                .run(&["kill", invalid, "--confirm", invalid])
                .status
                .success()
        );
    }
    let duplicate = jail.run(&["holder", id]);
    assert!(!duplicate.status.success());
    assert!(String::from_utf8_lossy(&duplicate.stderr).contains("already live"));
    assert_eq!(jail.metadata()["holder"], meta["holder"]);
    assert_eq!(jail.metadata()["child"], meta["child"]);
    // With the registry read-only, a relay-completed close still answers its caller and still
    // stops the holder and its child; the record keeps the last state it could write.
    use std::io::{BufRead, BufReader};
    let state = jail.root.join("state/herdr-shield");
    fs::set_permissions(&state, fs::Permissions::from_mode(0o500)).unwrap();
    let mut relay = BufReader::new(UnixStream::connect(directory.join("herdr-api.sock")).unwrap());
    relay
        .get_ref()
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    writeln!(
        relay.get_mut(),
        "{}",
        json!({"id":"close","method":"pane.close","params":{"pane_id":"w1:p1"}})
    )
    .unwrap();
    let mut line = String::new();
    relay.read_line(&mut line).unwrap();
    assert_eq!(line, "{\"id\":\"close\",\"result\":{\"type\":\"ok\"}}\n");
    let live =
        |identity: &str| std::path::Path::new(&format!("/proc/{}", meta[identity]["pid"])).exists();
    wait(|| !live("holder"));
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        (
            live("child"),
            directory.join("holder.sock").exists(),
            jail.metadata()["state"].clone()
        ),
        (false, false, json!("running"))
    );
    println!(
        "PASS private state corpus: owned 0700/0600 roots/files/sockets; public-mode/symlink/corrupt/path/duplicate attacks refused; credentials, terminal and capability absent from public logs/status"
    );
}

#[test]
fn corrupt_registry_assertion_failure_still_cleans_owned_resources() {
    if let Some(receipt) = std::env::var_os("HERDR_SHIELD_CLEANUP_PROBE") {
        let jail = Jail::new("j");
        let _ = jail.run(&[]);
        let meta = jail.metadata();
        assert_eq!(meta["state"], "running");
        fs::write(
            receipt,
            serde_json::to_vec(&json!({"root":jail.root,"meta":meta})).unwrap(),
        )
        .unwrap();
        fs::write(
            jail.root
                .join("state/herdr-shield")
                .join(format!("{}.json", meta["id"].as_str().unwrap())),
            b"{\"schema\":999}",
        )
        .unwrap();
        panic!("INJECTED_CORRUPT_REGISTRY_ASSERTION");
    }
    let receipt = std::env::temp_dir().join(format!("corrupt-fault-{}.json", std::process::id()));
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "coverage::corrupt_registry_assertion_failure_still_cleans_owned_resources",
            "--nocapture",
        ])
        .env("HERDR_SHIELD_CLEANUP_PROBE", &receipt)
        .output()
        .unwrap();
    let record: Value = serde_json::from_slice(&fs::read(&receipt).unwrap()).unwrap();
    let meta = &record["meta"];
    let mut survivors = Vec::new();
    for key in ["holder", "child"] {
        let pid = meta[key]["pid"].as_u64().unwrap();
        if let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) {
            let fields: Vec<_> = stat
                .rsplit_once(')')
                .unwrap()
                .1
                .split_whitespace()
                .collect();
            if fields[19].parse::<u64>().unwrap() == meta[key]["start"].as_u64().unwrap()
                && !matches!(fields[0], "Z" | "X")
            {
                survivors.push(pid);
            }
        }
    }
    // A broken cleanup control must itself clean its independently recorded exact unit before failing.
    let unit = meta["unit"].as_str().unwrap();
    let cleanup: Vec<_> = ["stop", "reset-failed"]
        .map(|action| jail::manage_unit(action, unit))
        .to_vec();
    let root = std::path::Path::new(record["root"].as_str().unwrap());
    assert!(root.starts_with(std::env::temp_dir()));
    for name in ["holder.sock", "herdr-api.sock"] {
        let socket = root
            .join("run/herdr-shield")
            .join(meta["id"].as_str().unwrap())
            .join(name);
        if socket.exists() {
            assert!(
                UnixStream::connect(&socket).is_err(),
                "live socket {} after cleanup {cleanup:?}",
                socket.display()
            );
            fs::remove_file(socket).unwrap();
        }
    }
    fs::remove_file(receipt).unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("INJECTED_CORRUPT_REGISTRY_ASSERTION")
    );
    assert!(
        survivors.is_empty(),
        "corrupt registry assertion leaked owned processes: {survivors:?}"
    );
    println!(
        "PASS injected corrupt-registry assertion: independent identity census finds zero survivors"
    );
}

#[test]
fn gate_timeout_cleans_owned_resources() {
    if std::env::var_os("HERDR_SHIELD_TIMEOUT_PROBE").is_some() {
        let jail = Jail::new("e");
        let _ = jail.run(&[]);
        assert_eq!(jail.metadata()["state"], "running");
        loop {
            std::thread::park();
        }
    }
    let root = std::path::PathBuf::from(std::env::var_os("HERDR_SHIELD_TEST_ROOT").unwrap())
        .parent()
        .unwrap()
        .join("g");
    fs::create_dir_all(&root).unwrap();
    let before: std::collections::BTreeSet<_> = fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/gate_run.py");
    let output = std::process::Command::new("/usr/bin/python3")
        .args(["-I", "-B"])
        .arg(script)
        .arg(std::env::current_exe().unwrap())
        .env("HERDR_SHIELD_TEST_ROOT", &root)
        .output()
        .unwrap();
    let mut survivors = Vec::new();
    let mut socket_survivors = Vec::new();
    for path in fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| !before.contains(path))
    {
        let ledger = path.join("owned-resources.jsonl");
        for line in fs::read_to_string(ledger).unwrap().lines() {
            let meta: Value = serde_json::from_str(line).unwrap();
            for key in ["holder", "child"] {
                let pid = meta[key]["pid"].as_u64().unwrap();
                if let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) {
                    let fields: Vec<_> = stat
                        .rsplit_once(')')
                        .unwrap()
                        .1
                        .split_whitespace()
                        .collect();
                    if fields[19].parse::<u64>().unwrap() == meta[key]["start"].as_u64().unwrap()
                        && !matches!(fields[0], "Z" | "X")
                    {
                        survivors.push(pid);
                    }
                }
            }
            let unit = meta["unit"].as_str().unwrap();
            let cleanup: Vec<_> = ["stop", "reset-failed"]
                .map(|action| jail::manage_unit(action, unit))
                .to_vec();
            for name in ["holder.sock", "herdr-api.sock"] {
                let socket = path
                    .join("run/herdr-shield")
                    .join(meta["id"].as_str().unwrap())
                    .join(name);
                if socket.exists() {
                    socket_survivors.push(socket.clone());
                    assert!(
                        UnixStream::connect(&socket).is_err(),
                        "live socket {} after cleanup {cleanup:?}; gate stdout:\n{}\ngate stderr:\n{}",
                        socket.display(),
                        String::from_utf8_lossy(&output.stdout),
                        String::from_utf8_lossy(&output.stderr)
                    );
                    fs::remove_file(socket).unwrap();
                }
            }
        }
        let socket = path.join("mock.sock");
        if socket.exists() {
            socket_survivors.push(socket.clone());
            assert!(
                UnixStream::connect(&socket).is_err(),
                "live socket {}; gate stdout:\n{}\ngate stderr:\n{}",
                socket.display(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            fs::remove_file(socket).unwrap();
        }
    }
    assert!(
        output.status.success(),
        "gate stdout:\n{}\ngate stderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        survivors.is_empty() && socket_survivors.is_empty(),
        "timed-out gate leaked resources: {survivors:?}, {socket_survivors:?}"
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("PASS timed-out control"));
    println!("{}", String::from_utf8_lossy(&output.stdout));
}

#[test]
fn live_incompatible_and_unknown_upstream_frames_are_visible_refusals() {
    use std::io::{BufRead, BufReader};
    use std::sync::atomic::Ordering;
    let jail = Jail::new("e");
    let _ = jail.run(&[]);
    let meta = jail.metadata();
    let socket = jail
        .root
        .join("run/herdr-shield")
        .join(meta["id"].as_str().unwrap())
        .join("herdr-api.sock");
    jail.protocol.store(23, Ordering::Relaxed);
    let mut stream = BufReader::new(UnixStream::connect(&socket).unwrap());
    stream
        .get_ref()
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut line = String::new();
    stream.read_line(&mut line).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&line).unwrap()["error"]["code"],
        "incompatible_protocol"
    );
    drop(stream);
    jail.protocol.store(22, Ordering::Relaxed);
    jail.extension.store(true, Ordering::Relaxed);
    let mut stream = BufReader::new(UnixStream::connect(&socket).unwrap());
    stream
        .get_ref()
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    writeln!(
        stream.get_mut(),
        "{}",
        json!({"id":"extension","method":"pane.get","params":{"pane_id":"w1:p1"}})
    )
    .unwrap();
    line.clear();
    stream.read_line(&mut line).unwrap();
    let response: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(response["id"], "extension");
    assert_eq!(response["error"]["code"], "unsupported_api");
    assert!(response.get("result").is_none());
    assert_eq!(jail.metadata()["child"], meta["child"]);
    println!(
        "PASS live upstream v23 and unknown result extension: typed visible errors; no success result; child retained"
    );
}

#[test]
fn graceful_shutdown_and_stubborn_child_escalation_are_bounded() {
    for (name, ignore) in [("g", false), ("s", true)] {
        let jail = Jail::new(name);
        let program = if ignore {
            "import signal,time; signal.signal(signal.SIGHUP, signal.SIG_IGN); signal.signal(signal.SIGTERM, signal.SIG_IGN); print('SHUTDOWN_READY',flush=True); time.sleep(60)"
        } else {
            "import signal,time,pathlib,os; signal.signal(signal.SIGHUP, lambda *_: (pathlib.Path(os.environ['HOME'],'graceful-exit').write_text('HUP'),exit(0))); print('SHUTDOWN_READY',flush=True); time.sleep(60)"
        };
        fs::write(
            jail.root.join("config/herdr-shield/config.json"),
            serde_json::to_vec(
                &json!({"shell":"/usr/bin/python3","args":["-I","-B","-c",program]}),
            )
            .unwrap(),
        )
        .unwrap();
        let _ = jail.run(&[]);
        let meta = jail.metadata();
        let mut client = connect(&jail, &meta);
        wait(|| snapshot(&mut client, &meta).contains("SHUTDOWN_READY"));
        drop(client);
        let began = std::time::Instant::now();
        let result = jail.run(&[
            "kill",
            meta["id"].as_str().unwrap(),
            "--confirm",
            meta["id"].as_str().unwrap(),
        ]);
        assert!(result.status.success(), "{:?}", result.stderr);
        assert!(
            began.elapsed() < Duration::from_secs(3),
            "shutdown was not bounded"
        );
        assert!(!std::path::Path::new(&format!("/proc/{}", meta["child"]["pid"])).exists());
        assert_eq!(jail.root.join("graceful-exit").exists(), !ignore);
    }
    println!(
        "PASS owned shutdown: HUP handler completed gracefully; HUP/TERM-ignoring child escalated and reaped within three seconds"
    );
}
