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
    let config = paths.config()?;
    if std::env::var_os("HERDR_SHIELD_INNER").is_some() {
        return fail("recursive keeper refused; run the configured inner shell directly");
    }
    let binding = match crate::herdr::binding() {
        Ok(value) => value,
        Err(error) => return recovery::plain(&config, &error.to_string()),
    };
    if let Err(error) = crate::herdr::prerequisites(paths) {
        return recovery::plain(&config, &error.to_string());
    }
    let lock = registry::lock(paths)?;
    let candidates: Vec<_> = Metadata::all(paths)?
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
    let mut meta = if let Some(meta) = candidates.into_iter().next() {
        meta
    } else {
        let mut meta = Metadata::new(binding.clone(), std::env::current_dir()?)?;
        let (rows, cols) = size();
        let env = std::env::vars_os()
            .map(|(key, value)| {
                Ok((
                    key.into_string()
                        .map_err(|_| "non-UTF8 environment key cannot bootstrap safely")?,
                    value
                        .into_string()
                        .map_err(|_| "non-UTF8 environment value cannot bootstrap safely")?,
                ))
            })
            .collect::<Result<Vec<_>>>()?;
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
            let reason = error.to_string();
            meta.state = "unprotected".into();
            meta.reason = reason.clone();
            meta.save(paths)?;
            drop(lock);
            return recovery::plain(&config, &reason);
        }
        if let Err(error) = holder::client(paths, &meta)
            .and_then(|mut client| protocol::exchange(&mut client, &request))
        {
            crate::lifetime::systemd::stop(&meta)?;
            meta.state = "unprotected".into();
            meta.reason = format!("bootstrap refused: {error}");
            meta.save(paths)?;
            drop(lock);
            return recovery::plain(&config, &meta.reason);
        }
        Metadata::load(paths, &meta.id)?
    };
    drop(lock);
    if meta.state == "unprotected" {
        return recovery::plain(&config, &meta.reason);
    }
    match run(paths, &mut meta, Some(binding), false) {
        Err(error) if error.to_string().contains("invalid_binding") => {
            recovery::plain(&config, &error.to_string())
        }
        result => result,
    }
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
    let mut previous_revision = 0;
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
                let mut bytes = Vec::new();
                let mut expected = 0u64;
                let final_frame = loop {
                    let frame = protocol::read(&mut stream)?;
                    if frame.kind != "render"
                        || frame.payload["sequence"].as_u64() != Some(expected)
                    {
                        return fail("snapshot gap or stale_generation; reconnect required");
                    }
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
                    expected += 1;
                    if frame.payload["total"].as_u64() == Some(expected) {
                        break frame;
                    }
                };
                if final_frame.generation != generation {
                    (token, generation) = acquire(&mut stream, &meta.id)?;
                    previous_revision = 0;
                    if final_frame.generation != generation {
                        continue;
                    }
                }
                if final_frame.payload["checksum"].as_str()
                    != Some(&format!("{:x}", Sha256::digest(&bytes)))
                {
                    return fail("snapshot checksum mismatch");
                }
                let revision = final_frame.payload["revision"]
                    .as_u64()
                    .ok_or("snapshot revision missing")?;
                if final_frame.payload["sideband"]["gap"] == true {
                    eprintln!(
                        "herdr-shield: sideband history gap; restored latest cwd/title without replaying historical command markers"
                    );
                }
                if let Some(data) = final_frame.payload["sideband"]["data"].as_str() {
                    std::io::stdout().write_all(data.as_bytes())?;
                }
                sideband_after = final_frame.payload["sideband"]["sequence"].as_u64();
                if revision != previous_revision {
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
