pub mod recovery;
mod theme;

use std::io::{Read, Write};
use std::os::fd::{AsFd, AsRawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use nix::poll::{PollFd, PollFlags, poll};
use nix::sys::signal::{self, SaFlags, SigAction, SigHandler, SigSet, Signal};
use nix::sys::termios::{self, SetArg, Termios};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::config::Paths;
use crate::holder::{
    self,
    protocol::{self, Frame},
    registry::{self, Metadata},
    state::Spawn,
};
use crate::{Result, fail};

static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn stop(_: i32) {
    STOP.store(true, Ordering::Relaxed);
}

pub struct RawTerminal {
    saved: Option<Termios>,
}

impl RawTerminal {
    pub fn enter() -> Result<Self> {
        let saved = termios::tcgetattr(std::io::stdin())?;
        let mut raw = saved.clone();
        termios::cfmakeraw(&mut raw);
        termios::tcsetattr(std::io::stdin(), SetArg::TCSANOW, &raw)?;
        Ok(Self { saved: Some(saved) })
    }
}

impl Drop for RawTerminal {
    fn drop(&mut self) {
        if let Some(saved) = self.saved.take() {
            if let Err(error) = termios::tcsetattr(std::io::stdin(), SetArg::TCSANOW, &saved) {
                eprintln!("herdr-shield: restoring terminal failed: {error}");
            }
        }
        let _=std::io::stdout().write_all(b"\x1b]8;;\x1b\\\x1b[0m\x1b[?25h\x1b[?1l\x1b[?7h\x1b[20l\x1b[=0u\x1b[?2004l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1004l\x1b[?1005l\x1b[?1006l\x1b>\x1b[?1049l");
    }
}

fn size() -> (u16, u16) {
    let mut size = nix::libc::winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // ioctl writes only this winsize, using our owned stdin descriptor.
    unsafe {
        nix::libc::ioctl(
            std::io::stdin().as_raw_fd(),
            nix::libc::TIOCGWINSZ,
            &mut size,
        )
    };
    (size.ws_row.clamp(1, 160), size.ws_col.clamp(1, 320))
}

pub fn default_shell(paths: &Paths) -> Result<()> {
    let config = match paths.config() {
        Ok(config) => config,
        Err(error) => {
            return recovery::plain(
                &recovery::fallback(),
                &format!("configuration unusable: {error}"),
            );
        }
    };
    // A pane always gets a shell: every bootstrap failure opens the plain shell.
    let (mut meta, binding) = match bootstrap(paths) {
        Ok(found) => found,
        Err(error) => return recovery::plain(&config, &error.to_string()),
    };
    match run(paths, &mut meta, Some(binding), false) {
        Err(error) if error.to_string().contains("invalid_binding") => {
            recovery::plain(&config, &error.to_string())
        }
        result => result,
    }
}

/// This pane's verified holder, found or created; any failure leaves no new holder serving.
fn bootstrap(paths: &Paths) -> Result<(Metadata, registry::Binding)> {
    if std::env::var_os("HERDR_SHIELD_INNER").is_some() {
        return fail("recursive shield refused");
    }
    let binding = crate::herdr::binding()?;
    crate::herdr::prerequisites(paths)?;
    let lock = registry::lock(paths)?;
    let (holders, unreadable) = Metadata::all(paths)?;
    if let Some(record) = unreadable.iter().find(|record| record.may_bind(&binding)) {
        return fail(&format!(
            "invalid_binding: unreadable holder record {} may own this pane ({}); retained, no duplicate created",
            record.id, record.error
        ));
    }
    let candidates: Vec<_> = holders
        .into_iter()
        .filter(|meta| {
            meta.binding.socket == binding.socket
                && meta.binding.pane == binding.pane
                && meta.state != "terminated"
        })
        .collect();
    if candidates.len() > 1 {
        return fail("invalid_binding: ambiguous existing holders; retained, no duplicate created");
    }
    let meta = if let Some(meta) = candidates.into_iter().next() {
        meta
    } else {
        let mut meta = Metadata::new(binding.clone(), std::env::current_dir()?)?;
        let (rows, cols) = size();
        let mut env = Vec::new();
        for (key, value) in std::env::vars_os() {
            match (key.to_str(), value.to_str()) {
                (Some(key), Some(value)) => env.push((key.to_owned(), value.to_owned())),
                // The holder takes UTF-8 only; one such variable costs itself, never the shell.
                _ => eprintln!(
                    "herdr-shield: skipped non-UTF-8 environment variable {}",
                    key.to_string_lossy()
                ),
            }
        }
        let request = Frame::new(
            &meta.id,
            "spawn",
            serde_json::to_value(Spawn {
                cwd: std::env::current_dir()?,
                env,
                rows,
                cols,
            })?,
        );
        if let Err(error) = crate::lifetime::systemd::launch(paths, &mut meta) {
            meta.state = "unprotected".into();
            meta.reason = error.to_string();
            meta.save(paths)?;
            return Err(error);
        }
        if let Err(error) = holder::client(paths, &meta)
            .and_then(|mut client| protocol::exchange(&mut client, &request))
        {
            meta.state = "unprotected".into();
            meta.reason = format!("bootstrap refused: {error}");
            if let Err(stop) = crate::lifetime::systemd::stop(&meta) {
                // The unbootstrapped holder expires on its own and then records its own reason;
                // the stop failure reaches the caller in this error.
                meta.reason = format!("{}; stop failed: {stop}", meta.reason);
            }
            meta.save(paths)?;
            return fail(&meta.reason);
        }
        Metadata::load(paths, &meta.id)?
    };
    drop(lock);
    if meta.state == "unprotected" {
        return fail(&meta.reason);
    }
    Ok((meta, binding))
}

pub fn existing(paths: &Paths, id: &str) -> Result<()> {
    let mut meta = Metadata::load(paths, id)?;
    let binding = crate::herdr::binding().ok();
    run(paths, &mut meta, binding, true)
}

fn run(
    paths: &Paths,
    meta: &mut Metadata,
    binding: Option<registry::Binding>,
    confirmed: bool,
) -> Result<()> {
    let mut stream = match holder::client(paths, meta) {
        Ok(client) => client,
        Err(error) => return recovery::missing(meta, &paths.config()?, &error.to_string()),
    };
    let status = protocol::exchange(&mut stream, &Frame::new(&meta.id, "status", json!({})))?;
    let supports = |feature: &str| {
        status.payload["features"]
            .as_array()
            .is_some_and(|features| features.iter().any(|item| item == feature))
    };
    let supports_sideband = supports("terminal-sideband-v1");
    let supports_theme = supports("theme-v1");
    if !confirmed && !supports("binding-proof-v1") {
        return fail(
            "invalid_binding: ambiguous pane instance; older holder lacks binding proof; explicitly choose herdr-shield attach <holder-id>",
        );
    }
    if let Some(binding) = binding {
        protocol::exchange(
            &mut stream,
            &Frame::new(
                &meta.id,
                if confirmed && supports("binding-proof-v1") {
                    "bind-confirmed"
                } else {
                    "bind"
                },
                serde_json::to_value(binding)?,
            ),
        )?;
    }
    let (mut token, mut generation) = acquire(&mut stream, &meta.id)?;
    let action = SigAction::new(SigHandler::Handler(stop), SaFlags::empty(), SigSet::empty());
    for signal in [Signal::SIGHUP, Signal::SIGTERM, Signal::SIGINT] {
        // Handler only sets an atomic flag; terminal restoration happens in the normal path.
        unsafe { signal::sigaction(signal, &action) }?;
    }
    let raw = RawTerminal::enter()?;
    std::io::stdout().write_all(b"\x1b[?1049h")?;
    if supports_theme {
        std::io::stdout().write_all(theme::Probe::queries().as_bytes())?;
        std::io::stdout().flush()?;
    }
    let mut theme_probe = theme::Probe::default();
    let theme_deadline = Instant::now() + Duration::from_secs(2);
    let mut theme_finished = !supports_theme;
    let mut input_activity = Instant::now();
    let mut sequence = 0u64;
    let mut previous_size = (0, 0);
    let mut previous_revision = None;
    let mut discarding = false;
    let mut next_render = Instant::now();
    let mut sideband_after = None;
    let result = (|| -> Result<()> {
        while !STOP.load(Ordering::Relaxed) {
            if !theme_finished && Instant::now() > theme_deadline {
                theme_finished = true;
                sequence += 1;
                mutate(
                    &mut stream,
                    Frame::new(&meta.id, "theme", json!({"sequence":sequence,"colors":[]})),
                    &mut token,
                    &mut generation,
                )?;
            }
            if input_activity.elapsed() > Duration::from_millis(80) {
                let pending = theme_probe.flush();
                if !pending.is_empty() {
                    sequence += 1;
                    mutate(
                        &mut stream,
                        Frame::new(
                            &meta.id,
                            "input",
                            json!({"sequence":sequence,"data":B64.encode(pending)}),
                        ),
                        &mut token,
                        &mut generation,
                    )?;
                }
            }
            let current = size();
            if current != previous_size {
                sequence += 1;
                let frame = Frame::new(
                    &meta.id,
                    "resize",
                    json!({"lease":token,"sequence":sequence,"rows":current.0,"cols":current.1}),
                );
                mutate(&mut stream, frame, &mut token, &mut generation)?;
                previous_size = current;
            }
            if Instant::now() >= next_render {
                let request = Frame::new(
                    &meta.id,
                    "snapshot",
                    if supports_sideband {
                        json!({"sideband_after":sideband_after})
                    } else {
                        json!({})
                    },
                );
                protocol::write(&mut stream, &request)?;
                let Some((final_frame, bytes)) = read_snapshot(&mut stream)? else {
                    if !discarding {
                        eprintln!(
                            "herdr-shield: discarded a snapshot with a chunk gap or checksum mismatch; requesting a fresh one"
                        );
                    }
                    discarding = true;
                    previous_revision = None;
                    next_render = Instant::now() + Duration::from_millis(40);
                    continue;
                };
                discarding = false;
                if final_frame.generation != generation {
                    (token, generation) = acquire(&mut stream, &meta.id)?;
                    previous_revision = None;
                    if final_frame.generation != generation {
                        continue;
                    }
                }
                let revision = final_frame.payload["revision"].as_u64();
                if final_frame.payload["sideband"]["gap"] == true {
                    eprintln!(
                        "herdr-shield: sideband history gap; restored latest cwd/title and any undelivered clipboard write without replaying historical command markers"
                    );
                }
                if let Some(data) = final_frame.payload["sideband"]["data"].as_str() {
                    std::io::stdout().write_all(data.as_bytes())?;
                }
                sideband_after = final_frame.payload["sideband"]["sequence"].as_u64();
                if redraw(previous_revision, revision) {
                    std::io::stdout().write_all(&bytes)?;
                    std::io::stdout().flush()?;
                    previous_revision = revision;
                }
                if final_frame.payload["state"] == "exited" {
                    println!("\r\nherdr-shield: owned shell exited; use list for status\r");
                    break;
                }
                next_render = Instant::now() + Duration::from_millis(40);
            }
            let input = std::io::stdin();
            let mut fds = [PollFd::new(input.as_fd(), PollFlags::POLLIN)];
            match poll(&mut fds, 20u16) {
                Ok(0) => continue,
                Ok(_) => (),
                Err(nix::errno::Errno::EINTR) => continue,
                Err(error) => return Err(error.into()),
            }
            if fds[0]
                .revents()
                .is_some_and(|f| f.intersects(PollFlags::POLLHUP | PollFlags::POLLERR))
            {
                break;
            }
            let mut bytes = [0u8; 8192];
            let count = std::io::stdin().read(&mut bytes)?;
            if count == 0 {
                break;
            }
            input_activity = Instant::now();
            let (input, colors) = if !supports_theme {
                (bytes[..count].to_vec(), Vec::new())
            } else {
                theme_probe.feed(&bytes[..count])
            };
            if !colors.is_empty() {
                sequence += 1;
                mutate(
                    &mut stream,
                    Frame::new(
                        &meta.id,
                        "theme",
                        json!({"sequence":sequence,"colors":colors}),
                    ),
                    &mut token,
                    &mut generation,
                )?;
            }
            if input.is_empty() {
                continue;
            }
            sequence += 1;
            let request = Frame::new(
                &meta.id,
                "input",
                json!({"lease":token,"sequence":sequence,"data":B64.encode(input)}),
            );
            mutate(&mut stream, request, &mut token, &mut generation)?;
        }
        Ok(())
    })();
    drop(raw);
    let _ = protocol::exchange(&mut stream, &Frame::new(&meta.id, "detach", json!({})));
    if let Err(error) = result {
        if holder::client(paths, meta).is_err() {
            let latest = Metadata::load(paths, &meta.id)?;
            return recovery::missing(&latest, &paths.config()?, &error.to_string());
        }
        return Err(error);
    }
    Ok(())
}

/// One snapshot reply read whole. `None` discards a reply with a chunk gap or a checksum
/// mismatch; either way the stream is left at the next reply.
fn read_snapshot(stream: &mut impl Read) -> Result<Option<(Frame, Vec<u8>)>> {
    let mut bytes = Vec::new();
    let mut expected = 0u64;
    let mut intact = true;
    loop {
        let frame = protocol::read(stream)?;
        if frame.kind != "render" {
            return fail(&format!(
                "snapshot refused; reconnect required: {}",
                frame.payload
            ));
        }
        let sequence = frame.payload["sequence"].as_u64();
        intact &= sequence == Some(expected);
        if intact {
            bytes.extend_from_slice(
                &B64.decode(
                    frame.payload["data"]
                        .as_str()
                        .ok_or("snapshot data missing")?,
                )?,
            );
            if bytes.len() > 8 * 1024 * 1024 {
                return fail("snapshot resource limit");
            }
        }
        expected += 1;
        // The last chunk names itself, so a reply with a gap is still read to its end.
        let total = frame.payload["total"].as_u64();
        if total.is_some() && sequence.map(|sequence| sequence + 1) == total {
            let matches = frame.payload["checksum"].as_str()
                == Some(&format!("{:x}", Sha256::digest(&bytes)));
            return Ok((intact && matches).then_some((frame, bytes)));
        }
    }
}

/// Redraw on a new revision; a snapshot without one always redraws.
fn redraw(previous: Option<u64>, revision: Option<u64>) -> bool {
    revision.is_none() || revision != previous
}

fn acquire(stream: &mut std::os::unix::net::UnixStream, id: &str) -> Result<(String, u64)> {
    let lease = protocol::exchange(stream, &Frame::new(id, "acquire", json!({})))?;
    Ok((
        lease.payload["lease"]
            .as_str()
            .ok_or("invalid lease")?
            .to_owned(),
        lease.generation,
    ))
}

fn mutate(
    stream: &mut std::os::unix::net::UnixStream,
    mut frame: Frame,
    token: &mut String,
    generation: &mut u64,
) -> Result<()> {
    for attempt in 0..2 {
        frame.generation = *generation;
        frame.payload["lease"] = json!(token);
        match protocol::exchange(stream, &frame) {
            Ok(_) => return Ok(()),
            Err(error) if attempt == 0 && error.to_string().contains("stale_generation") => {
                (*token, *generation) = acquire(stream, &frame.holder_id)?;
            }
            Err(error) => return Err(error),
        }
    }
    fail("stale_generation: binding changed repeatedly; reattach")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(chunks: &[(u64, &str)], total: u64, whole: &str) -> Vec<u8> {
        let mut bytes = Vec::new();
        for (sequence, data) in chunks {
            let frame = Frame::new(
                &"a".repeat(32),
                "render",
                json!({"sequence":sequence,"total":total,"data":B64.encode(data),"checksum":format!("{:x}", Sha256::digest(whole))}),
            );
            protocol::write(&mut bytes, &frame).unwrap();
        }
        bytes
    }

    #[test]
    fn snapshot_gap_or_checksum_mismatch_discards_one_reply_and_keeps_the_stream() {
        for (name, first, expected) in [
            (
                "intact",
                reply(&[(0, "hel"), (1, "lo")], 2, "hello"),
                Some("hello"),
            ),
            (
                "chunk gap",
                reply(&[(0, "hel"), (2, "lo")], 3, "hello"),
                None,
            ),
            (
                "checksum mismatch",
                reply(&[(0, "hel"), (1, "lo")], 2, "other"),
                None,
            ),
        ] {
            let mut stream =
                std::io::Cursor::new([first, reply(&[(0, "next")], 1, "next")].concat());
            let text = |snapshot: Option<(Frame, Vec<u8>)>| {
                snapshot.map(|(_, bytes)| String::from_utf8(bytes).unwrap())
            };
            assert_eq!(
                text(read_snapshot(&mut stream).unwrap()).as_deref(),
                expected,
                "{name}"
            );
            assert_eq!(
                text(read_snapshot(&mut stream).unwrap()).as_deref(),
                Some("next"),
                "{name}: the following reply must be read intact"
            );
        }
        let mut refused = Vec::new();
        protocol::write(
            &mut refused,
            &Frame::new(&"a".repeat(32), "snapshot", json!({})).error("invalid_frame", "bad"),
        )
        .unwrap();
        assert_eq!(
            read_snapshot(&mut std::io::Cursor::new(refused))
                .unwrap_err()
                .to_string(),
            r#"snapshot refused; reconnect required: {"code":"invalid_frame","message":"bad"}"#
        );
    }

    #[test]
    fn snapshot_without_revision_redraws() {
        for (previous, revision, expected) in [
            (None, Some(1), true),
            (Some(1), Some(1), false),
            (Some(1), Some(2), true),
            (Some(1), None, true),
            (None, None, true),
        ] {
            assert_eq!(
                redraw(previous, revision),
                expected,
                "{previous:?} -> {revision:?}"
            );
        }
    }
}
