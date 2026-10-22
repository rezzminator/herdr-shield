use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

use nix::sys::socket::{getsockopt, sockopt::PeerCredentials};
use nix::unistd::Uid;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{Result, config::MAX_JSON, fail};

pub const MAGIC: &[u8; 8] = b"HDRKEEP1";
pub const MAJOR: u16 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Frame {
    pub major: u16,
    pub minor: u16,
    pub request_id: u64,
    #[serde(rename = "type")]
    pub kind: String,
    pub holder_id: String,
    pub generation: u64,
    pub payload: Value,
}

impl Frame {
    pub fn new(id: &str, kind: &str, payload: Value) -> Self {
        Self {
            major: MAJOR,
            minor: 0,
            request_id: 0,
            kind: kind.into(),
            holder_id: id.into(),
            generation: 0,
            payload,
        }
    }

    pub fn reply(&self, kind: &str, payload: Value) -> Self {
        Self {
            kind: kind.into(),
            payload,
            ..self.clone()
        }
    }

    pub fn error(&self, code: &str, message: &str) -> Self {
        self.reply("error", json!({"code":code,"message":message}))
    }

    pub fn validate_payload(&self) -> Result<()> {
        let allowed: &[&str] = match self.kind.as_str() {
            "hello" => &["capability", "features"],
            "spawn" => &["cwd", "env", "rows", "cols"],
            "bind" | "bind-confirmed" => &["socket", "pane", "tab", "workspace", "server"],
            "input" => &["lease", "sequence", "data"],
            "resize" => &["lease", "sequence", "rows", "cols"],
            "terminate" => &["confirm", "capability", "identity"],
            "snapshot" | "resync" => &["sideband_after"],
            "theme" => &["lease", "sequence", "colors"],
            "acquire" | "status" | "detach" | "ack" => &[],
            _ => return fail("unknown_message"),
        };
        let object = self
            .payload
            .as_object()
            .ok_or("invalid_frame: payload must be object")?;
        if object.keys().any(|key| !allowed.contains(&key.as_str())) {
            return fail("invalid_frame: unknown payload field");
        }
        Ok(())
    }
}

pub fn peer(stream: &UnixStream) -> Result<u32> {
    let cred = getsockopt(stream, PeerCredentials)?;
    if cred.uid() != Uid::current().as_raw() {
        return fail("unauthorized: peer is not this user");
    }
    Ok(cred.pid().try_into()?)
}

pub fn read(reader: &mut impl Read) -> Result<Frame> {
    let mut header = [0u8; 12];
    reader.read_exact(&mut header)?;
    if &header[..8] != MAGIC {
        return fail("invalid_frame: wrong magic (binary Herdr transport is unsupported)");
    }
    let length = u32::from_be_bytes(header[8..].try_into()?) as usize;
    if length == 0 || length > MAX_JSON {
        return fail("frame_too_large: bounded nonempty frame required");
    }
    let mut data = vec![0; length];
    reader.read_exact(&mut data)?;
    // The derived struct rejects duplicate envelope fields. serde_json bounds recursion.
    let frame: Frame = crate::config::decode_json(&data)?;
    crate::config::valid_id(&frame.holder_id)?;
    Ok(frame)
}

pub fn write(writer: &mut impl Write, frame: &Frame) -> Result<()> {
    let data = serde_json::to_vec(frame)?;
    if data.len() > MAX_JSON {
        return fail("frame_too_large");
    }
    writer.write_all(MAGIC)?;
    writer.write_all(&(data.len() as u32).to_be_bytes())?;
    writer.write_all(&data)?;
    writer.flush()?;
    Ok(())
}

pub fn exchange(stream: &mut UnixStream, request: &Frame) -> Result<Frame> {
    write(stream, request)?;
    let reply = read(stream)?;
    if reply.request_id != request.request_id || reply.holder_id != request.holder_id {
        return fail("invalid_frame: reply correlation mismatch");
    }
    if reply.kind == "error" {
        return fail(&format!(
            "{}: {}",
            reply.payload["code"], reply.payload["message"]
        ));
    }
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_codec_limits_and_chunked_input() {
        let frame = Frame::new(&"a".repeat(32), "hello", json!({"capability":"secret"}));
        let mut bytes = Vec::new();
        write(&mut bytes, &frame).unwrap();
        assert_eq!(read(&mut bytes.as_slice()).unwrap().kind, "hello");
        for cut in 0..bytes.len() {
            assert!(read(&mut &bytes[..cut]).is_err());
        }
        let mut bad = bytes.clone();
        bad[8..12].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(read(&mut bad.as_slice()).is_err());
        bad[0] = 0;
        assert!(read(&mut bad.as_slice()).is_err());
    }

    #[test]
    fn duplicate_and_unknown_fields_are_rejected() {
        let data = br#"{"major":1,"major":2,"minor":0,"request_id":0,"type":"hello","holder_id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","generation":0,"payload":{}}"#;
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&(data.len() as u32).to_be_bytes());
        bytes.extend_from_slice(data);
        assert!(read(&mut bytes.as_slice()).is_err());
        let data=br#"{"major":1,"minor":0,"request_id":0,"type":"hello","holder_id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","generation":0,"payload":{"capability":"a","capability":"b"}}"#;
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&(data.len() as u32).to_be_bytes());
        bytes.extend_from_slice(data);
        assert!(
            read(&mut bytes.as_slice()).is_err(),
            "duplicate authentication fields must fail before mutation"
        );
    }
}
