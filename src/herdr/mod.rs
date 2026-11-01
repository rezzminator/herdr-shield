pub mod census;
pub mod mapping;
pub mod observer;
pub mod policy;
pub mod reconcile;
pub mod relay;
pub mod resume;
pub mod schema;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use crate::config::{MAX_JSON, Paths};
use crate::holder::protocol;
use crate::holder::registry::Binding;
use crate::lifetime::identity::Identity;
use crate::{Result, fail};

pub fn endpoint() -> Result<PathBuf> {
    let path =
        PathBuf::from(std::env::var_os("HERDR_SOCKET_PATH").ok_or("HERDR_SOCKET_PATH required")?);
    if !path.is_absolute() {
        return fail("Herdr endpoint must be absolute");
    }
    if std::env::var_os("HERDR_SHIELD_INNER").is_some() {
        return fail("recursive keeper invocation refused");
    }
    Ok(path)
}

pub fn connect(path: &Path) -> Result<UnixStream> {
    if !path.is_absolute() {
        return fail("Herdr endpoint must be absolute");
    }
    let unreachable = |error: std::io::Error| {
        format!("Herdr server not reachable at {}: {error}", path.display())
    };
    let meta = std::fs::symlink_metadata(path).map_err(unreachable)?;
    if meta.file_type().is_symlink() {
        return fail("symlinked Herdr endpoint refused");
    }
    let stream = UnixStream::connect(path).map_err(unreachable)?;
    protocol::peer(&stream)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    Ok(stream)
}

pub fn line(reader: &mut impl BufRead) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::io::Read::take(&mut *reader, (MAX_JSON + 1) as u64).read_until(b'\n', &mut bytes)?;
    if bytes.is_empty() || bytes.len() > MAX_JSON || bytes.last() != Some(&b'\n') {
        return fail("empty, incomplete or oversized upstream frame");
    }
    Ok(bytes)
}

pub fn rpc(socket: &Path, method: &str, params: Value) -> Result<Value> {
    let mut stream = connect(socket)?;
    serde_json::to_writer(
        &mut stream,
        &json!({"id":"keeper-local","method":method,"params":params}),
    )?;
    stream.write_all(b"\n")?;
    let value: Value = crate::config::decode_json(&line(&mut BufReader::new(stream))?)?;
    if value["id"] != "keeper-local" {
        return fail("upstream correlation mismatch");
    }
    if !value["error"].is_null() {
        return fail(&format!("upstream ERROR: {}", value["error"]));
    }
    if value["result"].is_null() {
        return fail("upstream missing result");
    }
    Ok(value["result"].clone())
}

pub fn binding() -> Result<Binding> {
    let socket = endpoint()?;
    let stream = connect(&socket)?;
    let server = Identity::read(protocol::peer(&stream)?)?;
    let pong = rpc(&socket, "ping", json!({}))?;
    if pong["protocol"] != 22 {
        return fail("incompatible upstream: Herdr protocol 22 required");
    }
    let pane = std::env::var("HERDR_PANE_ID")?;
    let tab = std::env::var("HERDR_TAB_ID")?;
    let workspace = std::env::var("HERDR_WORKSPACE_ID")?;
    if !pane.starts_with(&format!("{workspace}:p")) || !tab.starts_with(&format!("{workspace}:t")) {
        return fail("invalid Herdr public identities");
    }
    Ok(Binding {
        socket,
        pane,
        tab,
        workspace,
        server,
    })
}

pub fn prerequisites(paths: &Paths) -> Result<()> {
    let root = paths.config.parent().ok_or("missing config parent")?;
    let content = String::from_utf8(crate::config::read_upstream(
        &root.join("herdr/config.toml"),
    )?)?;
    let config: toml::Value = toml::from_str(&content)?;
    if config
        .get("session")
        .and_then(|s| s.get("resume_agents_on_restore"))
        .and_then(|s| s.as_bool())
        != Some(false)
    {
        return fail(
            "session.resume_agents_on_restore must explicitly be false to avoid duplicate execution",
        );
    }
    Ok(())
}
