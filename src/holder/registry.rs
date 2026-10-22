use std::fs::{self, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

use nix::fcntl::{Flock, FlockArg};
use serde::{Deserialize, Serialize};

use crate::config::{Paths, atomic_json, random_id, read_json, valid_id};
use crate::lifetime::identity::Identity;
use crate::{Result, fail};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub socket: PathBuf,
    pub pane: String,
    pub tab: String,
    pub workspace: String,
    pub server: Identity,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resume {
    pub argv: Vec<String>,
    pub process: Option<Identity>,
    pub source: String,
    #[serde(default)]
    pub agent: String,
    pub seq: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Metadata {
    pub schema: u16,
    pub id: String,
    pub capability: String,
    pub version: String,
    pub executable: PathBuf,
    pub wire: u16,
    pub created: u64,
    pub holder: Option<Identity>,
    pub child: Option<Identity>,
    pub birth: Binding,
    pub binding: Binding,
    #[serde(default)]
    pub history: Vec<Binding>,
    #[serde(default)]
    pub attachment: Option<Identity>,
    #[serde(default)]
    pub report_sequences: std::collections::BTreeMap<String, Option<u64>>,
    #[serde(default)]
    pub report_epoch: u64,
    #[serde(default)]
    pub resume_status: String,
    pub generation: u64,
    pub cwd: PathBuf,
    pub state: String,
    pub reason: String,
    pub resume: Option<Resume>,
    #[serde(default)]
    pub pending_resume: Option<Resume>,
    pub unit: String,
}

impl Metadata {
    pub fn new(binding: Binding, cwd: PathBuf) -> Result<Self> {
        Ok(Self {
            schema: 1,
            id: random_id()?,
            capability: random_id()?,
            version: env!("CARGO_PKG_VERSION").into(),
            executable: std::env::current_exe()?,
            wire: 1,
            created: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs(),
            holder: None,
            child: None,
            birth: binding.clone(),
            binding,
            history: Vec::new(),
            attachment: Some(Identity::read(std::process::id())?),
            report_sequences: std::collections::BTreeMap::new(),
            report_epoch: 0,
            resume_status: "no accepted resume captured".into(),
            generation: 1,
            cwd,
            state: "starting".into(),
            reason: "awaiting verified bootstrap".into(),
            resume: None,
            pending_resume: None,
            unit: String::new(),
        })
    }

    pub fn validate(&self) -> Result<()> {
        valid_id(&self.id)?;
        valid_id(&self.capability)?;
        if self.schema != 1 || self.wire != 1 || self.generation == 0 {
            return fail("unsupported or corrupt registry schema/wire/generation; retained");
        }
        if !self.cwd.is_absolute() || !self.binding.socket.is_absolute() {
            return fail("registry contains relative paths; retained");
        }
        Ok(())
    }

    pub fn save(&self, paths: &Paths) -> Result<()> {
        self.validate()?;
        atomic_json(&paths.state.join(format!("{}.json", self.id)), self)
    }

    pub fn load(paths: &Paths, id: &str) -> Result<Self> {
        valid_id(id)?;
        let value: Self = read_json(&paths.state.join(format!("{id}.json")))?;
        value.validate()?;
        if value.id != id {
            return fail("registry holder ID mismatch; retained");
        }
        Ok(value)
    }

    pub fn all(paths: &Paths) -> Result<Vec<Self>> {
        let mut result = Vec::new();
        for entry in fs::read_dir(&paths.state)? {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "json") {
                let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
                    return fail("invalid registry filename");
                };
                if id.len() == 32 {
                    result.push(Self::load(paths, id)?);
                }
            }
        }
        result.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(result)
    }
}

pub fn lock(paths: &Paths) -> Result<Flock<std::fs::File>> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
        .open(paths.state.join("registry.lock"))?;
    Flock::lock(file, FlockArg::LockExclusive).map_err(|(_, e)| e.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrupt_schema_is_never_an_empty_registry() {
        let binding = Binding {
            socket: "/socket".into(),
            pane: "w1:p1".into(),
            tab: "w1:t1".into(),
            workspace: "w1".into(),
            server: Identity::read(std::process::id()).unwrap(),
        };
        let mut meta = Metadata::new(binding, "/".into()).unwrap();
        assert!(meta.validate().is_ok());
        meta.schema = 99;
        assert!(meta.validate().is_err());
    }
}
