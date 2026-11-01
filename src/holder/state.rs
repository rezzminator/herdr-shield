use std::io::Write;
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Mutex};

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::registry::{Binding, Metadata};
use crate::config::{Config, Paths, random_id};
use crate::lifetime::identity::Identity;
use crate::terminal::{Size, Terminal};
use crate::{Result, fail};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spawn {
    pub cwd: std::path::PathBuf,
    pub env: Vec<(String, String)>,
    pub rows: u16,
    pub cols: u16,
}

pub struct Lease {
    pub owner: String,
    pub token: String,
    pub generation: u64,
    pub sequence: u64,
}

pub struct State {
    pub paths: Paths,
    pub meta: Metadata,
    pub terminal: Arc<Mutex<Terminal>>,
    pub writer: Option<SyncSender<Vec<u8>>>,
    pub master: Option<Box<dyn MasterPty + Send>>,
    pub child: Option<Box<dyn Child + Send + Sync>>,
    pub lease: Option<Lease>,
    pub stopping: bool,
}

impl State {
    pub fn spawn(&mut self, payload: Value) -> Result<Value> {
        if self.child.is_some() || self.meta.state != "starting" {
            return fail("spawn is once-only");
        }
        let bootstrap: Spawn = serde_json::from_value(payload)?;
        if !bootstrap.cwd.is_absolute() || !bootstrap.cwd.is_dir() {
            return fail("invalid bootstrap cwd");
        }
        let cgroup = std::fs::read_to_string("/proc/self/cgroup")?;
        if !crate::lifetime::systemd::escaped(&cgroup, &self.meta.unit) {
            return fail("cgroup verification failed: workload not spawned");
        }
        let size = Size::new(bootstrap.rows.into(), bootstrap.cols.into())?;
        let config: Config = self.paths.config()?;
        let pair = portable_pty::native_pty_system().openpty(PtySize {
            rows: size.rows,
            cols: size.cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        let mut command = CommandBuilder::new(&config.shell);
        command.args(&config.args);
        command.cwd(&bootstrap.cwd);
        command.env_clear();
        if bootstrap.env.len() > 8192 {
            return fail("too many environment entries");
        }
        for (key, value) in bootstrap.env {
            if key.is_empty() || key.contains(['\0', '=']) || value.contains('\0') {
                return fail("invalid environment entry");
            }
            command.env(key, value);
        }
        command.env(
            "HERDR_SOCKET_PATH",
            self.paths.holder_dir(&self.meta.id)?.join("herdr-api.sock"),
        );
        command.env("HERDR_PANE_ID", &self.meta.birth.pane);
        command.env("HERDR_TAB_ID", &self.meta.birth.tab);
        command.env("HERDR_WORKSPACE_ID", &self.meta.birth.workspace);
        command.env("HERDR_SHIELD_INNER", &self.meta.id);
        command.env("SHELL", &config.shell);
        let mut child = pair.slave.spawn_command(command)?;
        let identity = match Identity::read(child.process_id().ok_or("child PID unavailable")?) {
            Ok(identity) => identity,
            Err(error) => {
                child.kill()?;
                child.wait()?;
                return Err(error);
            }
        };
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader()?;
        let mut pty_writer = pair.master.take_writer()?;
        let (writer, input) = sync_channel::<Vec<u8>>(32);
        let (query_sender, queries) = sync_channel::<Vec<u8>>(32);
        std::thread::spawn(move || {
            loop {
                let bytes = match queries.try_recv() {
                    Ok(bytes) => bytes,
                    Err(_) => match input.recv_timeout(std::time::Duration::from_millis(5)) {
                        Ok(bytes) => bytes,
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    },
                };
                if pty_writer.write_all(&bytes).is_err() {
                    eprintln!("herdr-shield: PTY writer failed (input redacted)");
                    break;
                }
            }
        });
        self.terminal
            .lock()
            .map_err(|_| "terminal mutex poisoned")?
            .resize(size);
        let terminal = Arc::clone(&self.terminal);
        let query_writer = query_sender;
        std::thread::spawn(move || {
            let result = (|| -> Result<()> {
                let mut bytes = [0u8; 8192];
                loop {
                    let count = match std::io::Read::read(&mut reader, &mut bytes) {
                        Ok(0) => break,
                        Ok(count) => count,
                        Err(e) if e.raw_os_error() == Some(nix::libc::EIO) => break,
                        Err(e) => return Err(e.into()),
                    };
                    let mut model = terminal.lock().map_err(|_| "terminal mutex poisoned")?;
                    let mut replies = Vec::new();
                    for event in model.feed(&bytes[..count]) {
                        if let Some(reply) =
                            crate::terminal::queries::reply(&event, model.size, &model.palette())
                        {
                            replies.extend_from_slice(reply.as_bytes());
                        }
                    }
                    drop(model);
                    // Ordinary-input backpressure cannot block output draining or consume reply capacity.
                    if !replies.is_empty() && query_writer.try_send(replies).is_err() {
                        terminal
                            .lock()
                            .map_err(|_| "terminal mutex poisoned")?
                            .query_error = Some(
                            "terminal query delivery failed: reply queue full or writer unavailable".into(),
                        );
                        eprintln!(
                            "herdr-shield: terminal query reply queue full or writer unavailable"
                        );
                    }
                }
                Ok(())
            })();
            if result.is_err() {
                eprintln!("herdr-shield: PTY reader failed (terminal content redacted)");
            }
        });
        self.meta.holder = Some(Identity::read(std::process::id())?);
        self.meta.child = Some(identity);
        self.meta.cwd = bootstrap.cwd;
        self.meta.state = "running".into();
        self.meta.reason = "verified systemd user service; default-shell route".into();
        self.meta.save(&self.paths)?;
        self.writer = Some(writer);
        self.master = Some(pair.master);
        self.child = Some(child);
        Ok(json!({"state":"running","child":self.meta.child}))
    }

    /// Writes the record; a failed write is logged and the holder keeps its in-memory state.
    pub fn persist(&self) {
        if let Err(error) = self.meta.save(&self.paths) {
            eprintln!("herdr-shield: holder record save failed: {error}; still serving");
        }
    }

    pub fn bind(&mut self, binding: Binding) -> Result<Value> {
        if binding.socket != self.meta.birth.socket {
            return fail("invalid_binding: different session endpoint");
        }
        let stream = crate::herdr::connect(&binding.socket)?;
        let actual = Identity::read(super::protocol::peer(&stream)?)?;
        if actual != binding.server {
            return fail("invalid_binding: server identity mismatch");
        }
        if self.meta.binding != binding {
            let previous = &self.meta.binding;
            // History feeds alias mapping by public IDs alone: a rebind that keeps them
            // (a server restart) adds nothing to it.
            if !self.meta.history.iter().any(|entry| {
                entry.pane == previous.pane
                    && entry.tab == previous.tab
                    && entry.workspace == previous.workspace
            }) {
                if self.meta.history.len() >= 256 {
                    return fail(
                        "invalid_binding: alias history capacity reached; holder retained",
                    );
                }
                self.meta.history.push(previous.clone());
            }
            self.meta.binding = binding;
            self.meta.generation = self
                .meta
                .generation
                .checked_add(1)
                .ok_or("binding generation exhausted")?;
            self.lease = None;
            self.meta.save(&self.paths)?;
        }
        Ok(json!({"generation":self.meta.generation}))
    }

    pub fn bind_automatic(&mut self, binding: Binding, caller: &Identity) -> Result<Value> {
        if self.meta.attachment.as_ref() != Some(caller)
            || self.meta.binding.server != binding.server
        {
            self.meta.reason="ORPHAN: ambiguous pane instance; automatic rebind refused; explicitly select the retained holder".into();
            self.meta.save(&self.paths)?;
            return fail(&format!(
                "invalid_binding: ambiguous pane instance; holder retained. Explicitly choose: herdr-shield attach {}",
                self.meta.id
            ));
        }
        self.bind(binding)
    }

    pub fn acquire(&mut self, owner: &str) -> Result<Value> {
        if self.lease.is_some() {
            return fail("lease_held: another attachment owns input; detach it explicitly");
        }
        let token = random_id()?;
        self.lease = Some(Lease {
            owner: owner.into(),
            token: token.clone(),
            generation: self.meta.generation,
            sequence: 0,
        });
        if self.meta.state == "running" && self.meta.reason.starts_with("ORPHAN:") {
            self.meta.reason =
                "verified systemd user service; default-shell holder explicitly/currently attached"
                    .into();
        }
        Ok(json!({"lease":token,"generation":self.meta.generation}))
    }

    pub fn check_lease(&mut self, owner: &str, generation: u64, payload: &Value) -> Result<()> {
        if generation != self.meta.generation {
            return fail("stale_generation");
        }
        let lease = self
            .lease
            .as_mut()
            .ok_or("lease_held: acquire input first")?;
        if lease.owner != owner || payload["lease"].as_str() != Some(&lease.token) {
            return fail("unauthorized: input lease mismatch");
        }
        if generation != self.meta.generation || generation != lease.generation {
            return fail("stale_generation");
        }
        let sequence = payload["sequence"]
            .as_u64()
            .ok_or("stale_input: sequence required")?;
        if sequence <= lease.sequence {
            return fail("stale_input: replayed sequence");
        }
        lease.sequence = sequence;
        Ok(())
    }

    pub fn input(&mut self, payload: &Value) -> Result<()> {
        let bytes = B64.decode(payload["data"].as_str().ok_or("invalid input data")?)?;
        if bytes.len() > 65536 {
            return fail("input exceeds limit");
        }
        self.writer
            .as_ref()
            .ok_or("child not started")?
            .try_send(bytes)
            .map_err(|_| "input_backpressure: PTY input queue full or writer unavailable")?;
        Ok(())
    }

    pub fn resize(&mut self, payload: &Value) -> Result<()> {
        let size = Size::new(
            payload["rows"].as_u64().ok_or("rows required")?,
            payload["cols"].as_u64().ok_or("cols required")?,
        )?;
        self.master
            .as_ref()
            .ok_or("child not started")?
            .resize(PtySize {
                rows: size.rows,
                cols: size.cols,
                pixel_width: 0,
                pixel_height: 0,
            })?;
        self.terminal
            .lock()
            .map_err(|_| "terminal mutex poisoned")?
            .resize(size);
        Ok(())
    }

    pub fn detached(&mut self, owner: &str) -> Result<()> {
        if self
            .lease
            .as_ref()
            .is_some_and(|lease| lease.owner == owner)
        {
            self.lease = None;
            if self.meta.state == "running" {
                self.meta.reason =
                    "ORPHAN: attachment transport lost; intentional close not proven".into();
            }
            self.meta.save(&self.paths)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lease_owner_generation_and_sequence_are_independent_guards() {
        let lease = Lease {
            owner: "owner".into(),
            token: "cap".into(),
            generation: 2,
            sequence: 8,
        };
        let identity = Identity::read(std::process::id()).unwrap();
        let binding = Binding {
            socket: "/test.sock".into(),
            pane: "w1:p1".into(),
            tab: "w1:t1".into(),
            workspace: "w1".into(),
            server: identity,
        };
        let mut meta = Metadata::new(binding, "/".into()).unwrap();
        meta.generation = 2;
        let mut state = State {
            paths: Paths {
                home: "/".into(),
                config: "/".into(),
                state: "/".into(),
                runtime: "/".into(),
            },
            meta,
            terminal: Arc::new(Mutex::new(Terminal::new(Size::new(24, 80).unwrap()))),
            writer: None,
            master: None,
            child: None,
            lease: Some(lease),
            stopping: false,
        };
        let payload = json!({"lease":"cap","sequence":9});
        assert!(state.check_lease("competitor", 2, &payload).is_err());
        assert!(state.check_lease("owner", 1, &payload).is_err());
        assert!(
            state
                .check_lease("owner", 2, &json!({"lease":"cap","sequence":8}))
                .is_err()
        );
        assert!(state.check_lease("owner", 2, &payload).is_ok());
        assert!(state.check_lease("owner", 2, &payload).is_err());
    }

    #[test]
    fn rebinds_between_known_public_ids_never_exhaust_alias_history() {
        let root =
            std::env::temp_dir().join(format!("bind-{}", crate::config::random_id().unwrap()));
        crate::config::secure_dir(&root).unwrap();
        let socket = root.join("herdr.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let identity = Identity::read(std::process::id()).unwrap();
        let binding = |pane: &str| Binding {
            socket: socket.clone(),
            pane: pane.into(),
            tab: "w1:t1".into(),
            workspace: "w1".into(),
            server: identity.clone(),
        };
        let mut state = State {
            paths: Paths {
                home: root.clone(),
                config: root.clone(),
                state: root.clone(),
                runtime: root.clone(),
            },
            meta: Metadata::new(binding("w1:p1"), "/".into()).unwrap(),
            terminal: Arc::new(Mutex::new(Terminal::new(Size::new(24, 80).unwrap()))),
            writer: None,
            master: None,
            child: None,
            lease: None,
            stopping: false,
        };
        for round in 0..300 {
            let pane = if round % 2 == 0 { "w1:p2" } else { "w1:p1" };
            if let Err(error) = state.bind(binding(pane)) {
                panic!("rebind {round} to {pane} refused: {error}");
            }
            drop(listener.accept().unwrap());
        }
        assert_eq!(
            state
                .meta
                .history
                .iter()
                .map(|entry| entry.pane.as_str())
                .collect::<Vec<_>>(),
            ["w1:p1", "w1:p2"]
        );
        assert_eq!(state.meta.generation, 301);
        std::fs::remove_dir_all(root).unwrap();
    }
}
