use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU32, Ordering},
};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

pub struct Jail {
    pub root: PathBuf,
    pub socket: PathBuf,
    pub protocol: Arc<AtomicU32>,
    pub extension: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    records: std::sync::Mutex<Vec<Value>>,
}

impl Jail {
    pub fn new(name: &str) -> Self {
        let parent = PathBuf::from(
            std::env::var_os("HERDR_SHIELD_TEST_ROOT")
                .expect("ERROR: HERDR_SHIELD_TEST_ROOT must name a private throwaway directory"),
        );
        assert!(
            parent.is_absolute() && parent.starts_with(std::env::temp_dir()),
            "test root outside isolated TMPDIR"
        );
        let root = parent.join(format!("{name}-{}", std::process::id()));
        assert!(!root.exists(), "test jail must be fresh");
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        for child in [
            "config/herdr",
            "config/herdr-shield",
            "state",
            "data",
            "cache",
            "run",
        ] {
            fs::create_dir_all(root.join(child)).unwrap();
            fs::set_permissions(root.join(child), fs::Permissions::from_mode(0o700)).unwrap();
        }
        fs::write(
            root.join("config/herdr/config.toml"),
            "[session]\nresume_agents_on_restore = false\n",
        )
        .unwrap();
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fakes/dummy.sh");
        fs::copy(fixture, root.join("dummy-shell")).unwrap();
        fs::set_permissions(root.join("dummy-shell"), fs::Permissions::from_mode(0o700)).unwrap();
        let config = root.join("config/herdr-shield/config.json");
        fs::write(
            &config,
            serde_json::to_vec(&json!({"shell":root.join("dummy-shell"),"args":[]})).unwrap(),
        )
        .unwrap();
        fs::set_permissions(config, fs::Permissions::from_mode(0o600)).unwrap();
        let socket = root.join("mock.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let protocol = Arc::new(AtomicU32::new(22));
        let mock_protocol = Arc::clone(&protocol);
        let extension = Arc::new(AtomicBool::new(false));
        let mock_extension = Arc::clone(&extension);
        let stopping = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            while !stopping.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_millis(100)))
                            .unwrap();
                        let mut line = String::new();
                        if BufReader::new(stream.try_clone().unwrap())
                            .read_line(&mut line)
                            .is_ok()
                        {
                            if let Ok(request) = serde_json::from_str::<Value>(&line) {
                                let result = match request["method"].as_str() {
                                    Some("ping") => {
                                        json!({"type":"pong","protocol":mock_protocol.load(Ordering::Relaxed),"version":"0.9.5"})
                                    }
                                    Some("pane.list") => json!({"type":"pane_list","panes":[]}),
                                    _ if mock_extension.load(Ordering::Relaxed) => {
                                        json!({"type":"ok","unknown_extension":true})
                                    }
                                    _ => json!({"type":"ok"}),
                                };
                                let _ = writeln!(
                                    stream,
                                    "{}",
                                    json!({"id":request["id"],"result":result})
                                );
                            }
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5))
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            root,
            socket,
            protocol,
            extension,
            stop,
            thread: Some(thread),
            records: std::sync::Mutex::new(Vec::new()),
        }
    }

    pub fn command(&self) -> Command {
        self.guard(&self.socket);
        let mut command = Command::new(env!("CARGO_BIN_EXE_herdr-shield"));
        command
            .env_clear()
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env("XDG_RUNTIME_DIR", self.root.join("run"))
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "C.UTF-8")
            .env("TERM", "xterm-256color")
            .env("HERDR_SOCKET_PATH", &self.socket)
            .env("HERDR_PANE_ID", "w1:p1")
            .env("HERDR_TAB_ID", "w1:t1")
            .env("HERDR_WORKSPACE_ID", "w1")
            .env("HERDR_SHIELD_TEST_PREFIX", "herdr-shield-test-");
        command
    }

    pub fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }

    pub fn guard(&self, socket: &Path) {
        assert!(
            socket.starts_with(&self.root) && socket == self.socket,
            "ERROR: socket outside owned jail"
        );
    }

    pub fn metadata(&self) -> Value {
        let state = self.root.join("state/herdr-shield");
        let file = fs::read_dir(state)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.file_stem().is_some_and(|stem| stem.len() == 32))
            .unwrap();
        let meta: Value = serde_json::from_slice(&fs::read(file).unwrap()).unwrap();
        self.records.lock().unwrap().push(meta.clone());
        let mut ledger = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root.join("owned-resources.jsonl"))
            .unwrap();
        writeln!(ledger, "{meta}").unwrap();
        meta
    }
}

pub fn wait(mut predicate: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(15);
    while Instant::now() < until {
        if predicate() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("ERROR: predicate timeout");
}

/// The one door for a test unit's `systemctl` action: the user manager's own bus, never the
/// isolated XDG_RUNTIME_DIR, which holds no bus; returns the action, unit, exit status and stderr.
pub fn manage_unit(action: &str, unit: &str) -> String {
    assert!(
        unit.starts_with("herdr-shield-test-") && unit.ends_with(".service"),
        "ERROR: not a test unit: {unit}"
    );
    let uid = nix::unistd::Uid::current();
    let output = Command::new("systemctl")
        .args(["--user", &format!("--machine={uid}@.host"), action, unit])
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path=/run/user/{uid}/bus"),
        )
        .output()
        .unwrap();
    format!(
        "{action} {unit}: {} {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

impl Drop for Jail {
    fn drop(&mut self) {
        let mut identities = Vec::new();
        let mut sockets = Vec::new();
        let mut records = self.records.lock().unwrap().clone();
        if self.root.join("state/herdr-shield").exists() {
            for entry in fs::read_dir(self.root.join("state/herdr-shield")).unwrap() {
                let path = entry.unwrap().path();
                if path.extension().is_some_and(|s| s == "json") {
                    if let Ok(meta) = serde_json::from_slice::<Value>(&fs::read(&path).unwrap()) {
                        records.push(meta);
                    }
                }
            }
        }
        records.sort_by_key(|meta| meta["id"].as_str().unwrap_or("").to_owned());
        records.dedup_by(|left, right| left["id"] == right["id"]);
        for meta in records {
            for key in ["holder", "child"] {
                if let (Some(pid), Some(start)) =
                    (meta[key]["pid"].as_u64(), meta[key]["start"].as_u64())
                {
                    identities.push((pid, start));
                }
            }
            if let Some(id) = meta["id"]
                .as_str()
                .filter(|id| id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()))
            {
                for name in ["holder.sock", "herdr-api.sock"] {
                    sockets.push(self.root.join("run/herdr-shield").join(id).join(name));
                }
            }
            if let Some(unit) = meta["unit"]
                .as_str()
                .filter(|unit| unit.starts_with("herdr-shield-test-") && unit.ends_with(".service"))
            {
                for action in ["stop", "reset-failed"] {
                    println!("CLEANUP unit {}", manage_unit(action, unit));
                }
            }
        }
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
        let _ = fs::remove_file(&self.socket);
        let survivors: Vec<_> = identities
            .into_iter()
            .filter(|(pid, start)| {
                let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
                    return false;
                };
                let fields: Vec<_> = stat
                    .rsplit_once(')')
                    .unwrap()
                    .1
                    .split_whitespace()
                    .collect();
                fields.get(19).and_then(|value| value.parse::<u64>().ok()) == Some(*start)
                    && !matches!(fields[0], "Z" | "X")
            })
            .collect();
        println!("CLEANUP process survivors: {survivors:?}");
        assert!(survivors.is_empty(), "owned process survived cleanup");
        for socket in sockets {
            assert!(socket.starts_with(&self.root));
            if socket.exists() {
                fs::remove_file(socket).unwrap();
            }
        }
    }
}
