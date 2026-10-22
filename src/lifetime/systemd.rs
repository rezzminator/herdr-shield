use std::fs;
use std::process::Stdio;
use std::time::{Duration, Instant};

use crate::config::{Paths, command};
use crate::holder::registry::Metadata;
use crate::lifetime::identity::{Identity, Life};
use crate::{Result, fail};

fn manager(program: &str) -> std::process::Command {
    let mut cmd = command(program);
    if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
        cmd.env(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path=/run/user/{}/bus", nix::unistd::Uid::current()),
        );
    }
    cmd.arg("--user");
    // Address this local UID's manager explicitly; the pane's isolated runtime is not its bus root.
    cmd.arg(format!("--machine={}@.host", nix::unistd::Uid::current()));
    cmd
}

pub fn available() -> Result<()> {
    if !cfg!(target_os = "linux") {
        return fail("Linux with a verified systemd user manager is required");
    }
    let output = manager("systemctl")
        .args(["show", "--property=Version", "--value"])
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() || output.stdout.is_empty() {
        return fail("systemd user manager unavailable or unverified");
    }
    Ok(())
}

pub fn escaped(cgroup: &str, unit: &str) -> bool {
    cgroup.lines().any(|line| {
        let Some(path) = line.strip_prefix("0::") else {
            return false;
        };
        path.split('/')
            .any(|part| part.starts_with("user@") && part.ends_with(".service"))
            && path.split('/').any(|part| part == unit)
            && !path
                .split('/')
                .any(|part| part.starts_with("session-") && part.ends_with(".scope"))
    })
}

pub fn launch(paths: &Paths, meta: &mut Metadata) -> Result<()> {
    available()?;
    let prefix = match std::env::var("HERDR_SHIELD_TEST_PREFIX") {
        Ok(value) if value == "herdr-shield-test-" => value,
        Ok(_) => return fail("invalid test unit prefix"),
        Err(_) => "herdr-shield-".into(),
    };
    meta.unit = format!("{prefix}{}.service", meta.id);
    let dir = paths.holder_dir(&meta.id)?;
    meta.save(paths)?;
    let mut cmd = manager("systemd-run");
    cmd.args([
        "--quiet",
        "--collect",
        "--service-type=exec",
        "--property=Restart=no",
        "--property=KillMode=control-group",
        "--property=UMask=0077",
        "--property=StandardOutput=null",
        "--property=StandardError=null",
    ])
    .arg(format!("--unit={}", meta.unit));
    for (key, value) in [
        ("HOME", paths.home.clone()),
        (
            "XDG_DATA_HOME",
            std::env::var_os("XDG_DATA_HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| paths.home.join(".local/share")),
        ),
        (
            "XDG_CACHE_HOME",
            std::env::var_os("XDG_CACHE_HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| paths.home.join(".cache")),
        ),
        (
            "XDG_CONFIG_HOME",
            paths.config.parent().ok_or("missing config root")?.into(),
        ),
        (
            "XDG_STATE_HOME",
            paths.state.parent().ok_or("missing state root")?.into(),
        ),
        (
            "XDG_RUNTIME_DIR",
            paths.runtime.parent().ok_or("missing runtime root")?.into(),
        ),
    ] {
        let mut setting = std::ffi::OsString::from("--setenv=");
        setting.push(key);
        setting.push("=");
        setting.push(value);
        cmd.arg(setting);
    }
    cmd.arg(&meta.executable)
        .args(["holder", &meta.id])
        .stdin(Stdio::null());
    let result = cmd.output()?;
    if !result.status.success() {
        return fail("systemd user service creation failed; workload was not started");
    }
    let readiness = (|| -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            let output = manager("systemctl")
                .args(["show", &meta.unit, "--property=MainPID", "--value"])
                .output()?;
            if output.status.success() {
                if let Ok(pid) = String::from_utf8_lossy(&output.stdout)
                    .trim()
                    .parse::<u32>()
                {
                    if pid > 0 {
                        let identity = Identity::read(pid)?;
                        let cgroup = fs::read_to_string(format!("/proc/{pid}/cgroup"))?;
                        if !escaped(&cgroup, &meta.unit) {
                            return fail(
                                "cgroup verification failed; unbootstrapped service stopped",
                            );
                        }
                        if dir.join("holder.sock").exists() {
                            meta.holder = Some(identity);
                            meta.save(paths)?;
                            return Ok(());
                        }
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        fail("holder service readiness timeout; unbootstrapped service stopped")
    })();
    if readiness.is_err() {
        stop_unit(&meta.unit)?;
    }
    readiness
}

pub fn stop(meta: &Metadata) -> Result<()> {
    let holder = meta
        .holder
        .as_ref()
        .ok_or("holder identity unavailable; no signal sent")?;
    if holder.life() != Life::Live {
        return fail("holder identity not live and exact; no signal sent");
    }
    let cgroup = fs::read_to_string(format!("/proc/{}/cgroup", holder.pid))?;
    if !escaped(&cgroup, &meta.unit) {
        return fail("holder service identity mismatch; no signal sent");
    }
    stop_unit(&meta.unit)
}

fn stop_unit(unit: &str) -> Result<()> {
    if !(unit.starts_with("herdr-shield-") && unit.ends_with(".service"))
        || unit
            .bytes()
            .any(|b| !b.is_ascii_alphanumeric() && !b"-.".contains(&b))
    {
        return fail("invalid owned service name");
    }
    let status = manager("systemctl").args(["stop", unit]).status()?;
    if !status.success() {
        return fail("owned service stop failed");
    }
    // --collect removes healthy units; reset-failed is restricted to this exact owned name.
    let _ = manager("systemctl")
        .args(["reset-failed", unit])
        .stderr(Stdio::null())
        .status();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cgroup_requires_user_manager_and_exact_unit_outside_login_scope() {
        let unit = "herdr-shield-test-a.service";
        assert!(escaped(
            &format!("0::/user.slice/user-1.slice/user@1.service/app.slice/{unit}\n"),
            unit
        ));
        assert!(!escaped("0::/user.slice/session-1.scope\n", unit));
        assert!(!escaped(
            "0::/user.slice/user@1.service/app.slice/wrong.service\n",
            unit
        ));
        assert!(!escaped("", unit));
    }
}
