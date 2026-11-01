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
    /// Fields a newer herdr-shield wrote: kept through a load and save, never read.
    #[serde(flatten)]
    pub newer: serde_json::Map<String, serde_json::Value>,
}

/// A registry record that could not be loaded: retained on disk, reported, never matched.
#[derive(Debug, Serialize)]
pub struct Unreadable {
    pub id: String,
    pub error: String,
    /// The record's own endpoint and pane, when its JSON still names them.
    pub socket: Option<PathBuf>,
    pub pane: Option<String>,
}

impl Unreadable {
    /// An unreadable record may own `binding`'s pane unless its JSON names another one.
    pub fn may_bind(&self, binding: &Binding) -> bool {
        self.socket
            .as_ref()
            .is_none_or(|socket| *socket == binding.socket)
            && self.pane.as_ref().is_none_or(|pane| *pane == binding.pane)
    }
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
            newer: serde_json::Map::new(),
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

    /// The loadable records, and apart from them each one that could not be loaded:
    /// one bad record costs only itself.
    pub fn all(paths: &Paths) -> Result<(Vec<Self>, Vec<Unreadable>)> {
        let mut result = Vec::new();
        let mut unreadable = Vec::new();
        for entry in fs::read_dir(&paths.state)? {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "json") {
                // Holder IDs are 32 hex characters; any other name is not a holder record.
                let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                if id.len() == 32 {
                    match Self::load(paths, id) {
                        Ok(meta) => result.push(meta),
                        Err(error) => {
                            let named = read_json::<serde_json::Value>(&path).ok();
                            let binding = named.as_ref().map(|value| &value["binding"]);
                            unreadable.push(Unreadable {
                                id: id.into(),
                                error: error.to_string(),
                                socket: binding
                                    .and_then(|binding| binding["socket"].as_str())
                                    .map(PathBuf::from),
                                pane: binding
                                    .and_then(|binding| binding["pane"].as_str())
                                    .map(Into::into),
                            });
                        }
                    }
                }
            }
        }
        result.sort_by(|a, b| a.id.cmp(&b.id));
        unreadable.sort_by(|a, b| a.id.cmp(&b.id));
        Ok((result, unreadable))
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
        let mut meta = Metadata::new(binding.clone(), "/".into()).unwrap();
        assert!(meta.validate().is_ok());
        let root = std::env::temp_dir().join(format!("registry-{}", random_id().unwrap()));
        crate::config::secure_dir(&root).unwrap();
        let paths = Paths {
            home: root.clone(),
            config: root.clone(),
            state: root.clone(),
            runtime: root.clone(),
        };
        let record = root.join(format!("{}.json", meta.id));
        let mut newer = serde_json::to_value(&meta).unwrap();
        newer["future"] = serde_json::json!("kept");
        atomic_json(&record, &newer).unwrap();
        Metadata::load(&paths, &meta.id)
            .unwrap()
            .save(&paths)
            .unwrap();
        assert_eq!(
            read_json::<serde_json::Value>(&record).unwrap()["future"],
            "kept",
            "a newer field must load and survive the next save: {}",
            record.display()
        );
        let corrupt = "c".repeat(32);
        atomic_json(
            &root.join(format!("{corrupt}.json")),
            &serde_json::json!({"schema":999,"binding":{"socket":"/socket","pane":"w1:p2"}}),
        )
        .unwrap();
        let garbage = "d".repeat(32);
        atomic_json(&root.join(format!("{garbage}.json")), &"not a record").unwrap();
        let (holders, unreadable) = Metadata::all(&paths).unwrap();
        assert_eq!(
            holders
                .iter()
                .map(|meta| meta.id.as_str())
                .collect::<Vec<_>>(),
            [meta.id.as_str()]
        );
        assert_eq!(
            serde_json::to_value(&unreadable).unwrap(),
            serde_json::json!([
                {"id":corrupt,"error":"missing field `tab` at line 1 column 46","socket":"/socket","pane":"w1:p2"},
                {"id":garbage,"error":"invalid type: string \"not a record\", expected struct Metadata at line 1 column 14","socket":null,"pane":null}
            ])
        );
        assert!(!unreadable[0].may_bind(&binding), "names another pane");
        assert!(unreadable[1].may_bind(&binding), "names no pane");
        fs::remove_dir_all(root).unwrap();
        meta.schema = 99;
        assert!(meta.validate().is_err());
    }
}
