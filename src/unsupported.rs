use std::fs;
use std::io::Read;
use std::path::Path;

use crate::{Result, fail, shell::Config};

pub fn run(path: &Path) -> Result<()> {
    eprintln!(
        "herdr-shield: UNPROTECTED: Linux with a verified systemd user manager is required; opening configured plain shell"
    );
    if !path.is_absolute() {
        return fail("plain-shell configuration path must be absolute");
    }
    for ancestor in path.ancestors() {
        if fs::symlink_metadata(ancestor)?.file_type().is_symlink() {
            return fail("symlinked plain-shell configuration refused");
        }
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK | nix::libc::O_CLOEXEC);
    }
    let file = options.open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return fail("plain-shell configuration must be a regular file");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.uid() != nix::unistd::Uid::current().as_raw() || meta.mode() & 0o077 != 0 {
            return fail("plain-shell configuration requires private ownership and permissions");
        }
    }
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        return fail("plain-shell configuration exceeds limit");
    }
    let config: Config = serde_json::from_slice(&bytes)?;
    config.validate()?;
    let mut command = crate::shell::command(&config.shell);
    command.args(&config.args).env_remove("HERDR_SHIELD_INNER");
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        Err(command.exec().into())
    }
    #[cfg(not(unix))]
    {
        let status = command.status()?;
        std::process::exit(status.code().unwrap_or(1));
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn unsupported_platform_entrypoint_runs_plain_shell_and_reports_unprotected() {
        if let Some(path) = std::env::var_os("HERDR_SHIELD_PLATFORM_PROBE") {
            run(Path::new(&path)).unwrap();
            panic!("unsupported platform fallback did not exec the configured shell");
        }
        let root =
            std::env::temp_dir().join(format!("platform-{}", crate::config::random_id().unwrap()));
        crate::config::secure_dir(&root).unwrap();
        let path = root.join("config.json");
        crate::config::atomic_json(
            &path,
            &Config {
                shell: "/bin/sh".into(),
                args: vec!["-c".into(), "printf PLATFORM_PLAIN_SHELL".into()],
            },
        )
        .unwrap();
        let output = crate::config::command(std::env::current_exe().unwrap())
            .args(["--exact", "unsupported::tests::unsupported_platform_entrypoint_runs_plain_shell_and_reports_unprotected", "--nocapture"])
            .env("HERDR_SHIELD_PLATFORM_PROBE", &path).output().unwrap();
        assert!(output.status.success(), "{:?}", output.stderr);
        assert!(String::from_utf8_lossy(&output.stdout).contains("PLATFORM_PLAIN_SHELL"));
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("UNPROTECTED: Linux with a verified systemd user manager is required")
        );
        fs::remove_dir_all(root).unwrap();
    }
}
