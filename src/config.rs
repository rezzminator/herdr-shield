use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use nix::unistd::Uid;
use serde::{Deserialize, Serialize};

use crate::{Result, fail};

pub const MAX_JSON: usize = 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Paths {
    pub home: PathBuf,
    pub config: PathBuf,
    pub state: PathBuf,
    pub runtime: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub shell: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
}

impl Paths {
    pub fn load() -> Result<Self> {
        let home = env_path("HOME")?;
        let config = root("XDG_CONFIG_HOME", home.join(".config"))?;
        let state = root("XDG_STATE_HOME", home.join(".local/state"))?;
        let runtime = root("XDG_RUNTIME_DIR", state.join("run"))?;
        let paths = Self {
            home,
            config: config.join("herdr-shield"),
            state: state.join("herdr-shield"),
            runtime: runtime.join("herdr-shield"),
        };
        for path in [&paths.config, &paths.state, &paths.runtime] {
            secure_dir(path)?;
        }
        Ok(paths)
    }

    pub fn config(&self) -> Result<Config> {
        let config: Config = read_json(&self.config.join("config.json"))?;
        config.validate()?;
        Ok(config)
    }

    pub fn holder_dir(&self, id: &str) -> Result<PathBuf> {
        valid_id(id)?;
        let path = self.runtime.join(id);
        secure_dir(&path)?;
        if path.join("holder.sock").as_os_str().len() >= 104 {
            return fail("runtime socket path too long; choose a shorter XDG_RUNTIME_DIR");
        }
        Ok(path)
    }
}

impl Config {
    pub fn validate(&self) -> Result<()> {
        let shell = fs::canonicalize(&self.shell)?;
        if !self.shell.is_absolute()
            || !shell.is_file()
            || fs::metadata(&shell)?.permissions().mode() & 0o111 == 0
            || shell == fs::canonicalize(std::env::current_exe()?)?
            || shell.file_name().is_some_and(|n| n == "herdr-shield")
        {
            return fail("inner shell must be an explicit executable and cannot be herdr-shield");
        }
        if self.args.iter().any(|a| a.contains('\0')) {
            return fail("shell argv contains NUL");
        }
        Ok(())
    }
}

fn env_path(key: &str) -> Result<PathBuf> {
    let path = PathBuf::from(std::env::var_os(key).ok_or_else(|| format!("{key} is required"))?);
    if !path.is_absolute() {
        return fail(&format!("{key} must be absolute"));
    }
    Ok(path)
}

fn root(key: &str, fallback: PathBuf) -> Result<PathBuf> {
    match std::env::var_os(key) {
        Some(_) => env_path(key),
        None => Ok(fallback),
    }
}

pub fn secure_dir(path: &Path) -> Result<()> {
    if !path.is_absolute() {
        return fail("private directory must be absolute");
    }
    // Check every existing ancestor before creating anything through it.
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(meta) if meta.file_type().is_symlink() => return fail("symlinked private path"),
            Ok(_) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
    }
    if !path.exists() {
        fs::create_dir_all(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_dir() || meta.uid() != Uid::current().as_raw() || meta.mode() & 0o077 != 0 {
        return fail("private root must be owned by this user and mode 0700");
    }
    Ok(())
}

pub fn private_open(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC | nix::libc::O_NONBLOCK)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.uid() != Uid::current().as_raw() || meta.mode() & 0o077 != 0 {
        return fail("private file has unsafe ownership or permissions");
    }
    Ok(file)
}

pub fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let mut bytes = Vec::new();
    private_open(path)?
        .take((MAX_JSON + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_JSON {
        return fail("private JSON exceeds limit");
    }
    decode_json(&bytes)
}

pub fn decode_json<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    if bytes.len() > MAX_JSON {
        return fail("JSON exceeds resource limit");
    }
    let _: UniqueJson = serde_json::from_slice(bytes)?;
    Ok(serde_json::from_slice(bytes)?)
}

pub fn read_upstream(path: &Path) -> Result<Vec<u8>> {
    if !path.is_absolute() {
        return fail("upstream observation path must be absolute");
    }
    for ancestor in path.ancestors() {
        if fs::symlink_metadata(ancestor)?.file_type().is_symlink() {
            return fail("symlinked upstream observation path");
        }
    }
    let parent = fs::symlink_metadata(path.parent().ok_or("missing upstream observation parent")?)?;
    if !parent.is_dir() || parent.uid() != Uid::current().as_raw() || parent.mode() & 0o077 != 0 {
        return fail("upstream observation requires an owned private parent");
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK | nix::libc::O_CLOEXEC)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.uid() != Uid::current().as_raw() || meta.nlink() != 1 {
        return fail("unsafe upstream observation file");
    }
    let mut bytes = Vec::new();
    file.take((MAX_JSON + 1) as u64).read_to_end(&mut bytes)?;
    if bytes.len() > MAX_JSON {
        return fail("upstream observation exceeds resource limit");
    }
    Ok(bytes)
}

pub fn observe_json(path: &Path) -> Result<(serde_json::Value, String)> {
    let bytes = read_upstream(path)?;
    use sha2::{Digest, Sha256};
    Ok((
        decode_json(&bytes)?,
        format!("{:x}", Sha256::digest(&bytes)),
    ))
}

struct UniqueJson;

impl<'de> Deserialize<'de> for UniqueJson {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = UniqueJson;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("bounded JSON with unique object keys")
            }
            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                mut map: M,
            ) -> std::result::Result<Self::Value, M::Error> {
                let mut keys = std::collections::BTreeSet::new();
                while let Some(key) = map.next_key::<String>()? {
                    if !keys.insert(key) {
                        return Err(serde::de::Error::custom("duplicate JSON field"));
                    }
                    let _: UniqueJson = map.next_value()?;
                }
                Ok(UniqueJson)
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                while seq.next_element::<UniqueJson>()?.is_some() {}
                Ok(UniqueJson)
            }
            fn visit_str<E: serde::de::Error>(
                self,
                _: &str,
            ) -> std::result::Result<Self::Value, E> {
                Ok(UniqueJson)
            }
            fn visit_bool<E: serde::de::Error>(
                self,
                _: bool,
            ) -> std::result::Result<Self::Value, E> {
                Ok(UniqueJson)
            }
            fn visit_i64<E: serde::de::Error>(self, _: i64) -> std::result::Result<Self::Value, E> {
                Ok(UniqueJson)
            }
            fn visit_u64<E: serde::de::Error>(self, _: u64) -> std::result::Result<Self::Value, E> {
                Ok(UniqueJson)
            }
            fn visit_f64<E: serde::de::Error>(self, _: f64) -> std::result::Result<Self::Value, E> {
                Ok(UniqueJson)
            }
            fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Self::Value, E> {
                Ok(UniqueJson)
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

pub fn atomic_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let parent = path.parent().ok_or("missing parent")?;
    secure_dir(parent)?;
    let temp = parent.join(format!(".{}.tmp", random_id()?));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp)?;
    let result = (|| {
        serde_json::to_writer(&mut file, value)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

pub fn random_id() -> Result<String> {
    let mut bytes = [0u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

pub fn valid_id(id: &str) -> Result<()> {
    if id.len() == 32
        && id
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    {
        Ok(())
    } else {
        fail("invalid holder ID (expected 32 lowercase hex digits)")
    }
}

pub fn command(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    std::process::Command::new(program)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_open_rejects_fifo_without_waiting_for_a_writer() {
        let root = std::env::temp_dir().join(format!("herdr-shield-fifo-{}", random_id().unwrap()));
        secure_dir(&root).unwrap();
        let fifo = root.join("metadata.json");
        nix::unistd::mkfifo(
            &fifo,
            nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
        )
        .unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || sender.send(private_open(&fifo).is_err()).unwrap());
        assert_eq!(
            receiver
                .recv_timeout(std::time::Duration::from_millis(250))
                .ok(),
            Some(true)
        );
        let fifo = root.join("upstream.toml");
        nix::unistd::mkfifo(
            &fifo,
            nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
        )
        .unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || sender.send(read_upstream(&fifo).is_err()).unwrap());
        assert_eq!(
            receiver
                .recv_timeout(std::time::Duration::from_millis(250))
                .ok(),
            Some(true)
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn identifiers_cannot_be_paths() {
        assert!(valid_id("../holder").is_err());
        assert!(valid_id(&"a".repeat(32)).is_ok());
        assert!(valid_id(&"A".repeat(32)).is_err());
    }

    #[test]
    fn private_paths_reject_symlinks_public_modes_and_corrupt_json() {
        let root = std::env::temp_dir().join(format!("herdr-shield-config-{}", random_id().unwrap()));
        secure_dir(&root).unwrap();
        let linked = root.join("linked");
        std::os::unix::fs::symlink(&root, &linked).unwrap();
        assert!(secure_dir(&linked).is_err());
        assert!(secure_dir(&linked.join("child")).is_err());
        let public = root.join("public");
        std::fs::create_dir(&public).unwrap();
        std::fs::set_permissions(&public, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(secure_dir(&public).is_err());
        let path = root.join("record.json");
        atomic_json(&path, &serde_json::json!({"revision":1})).unwrap();
        atomic_json(&path, &serde_json::json!({"revision":2})).unwrap();
        assert_eq!(
            read_json::<serde_json::Value>(&path).unwrap()["revision"],
            2
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::write(&path, b"{\"truncated\"").unwrap();
        assert!(read_json::<serde_json::Value>(&path).is_err());
        assert!(decode_json::<serde_json::Value>(br#"{"nested":{"a":1,"a":2}}"#).is_err());
    }
}
