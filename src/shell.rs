use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{Result, fail};

pub fn command(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    std::process::Command::new(program)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub shell: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
}

impl Config {
    pub fn validate(&self) -> Result<()> {
        let shell = fs::canonicalize(&self.shell)?;
        if !self.shell.is_absolute()
            || !shell.is_file()
            || shell == fs::canonicalize(std::env::current_exe()?)?
            || shell.file_name().is_some_and(|name| name == "herdr-shield")
        {
            return fail("inner shell must be an explicit executable and cannot be herdr-shield");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if fs::metadata(&shell)?.permissions().mode() & 0o111 == 0 {
                return fail("configured inner shell is not executable");
            }
        }
        if self.args.iter().any(|arg| arg.contains('\0')) {
            return fail("shell argv contains NUL");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_shell_rejects_self_alias_and_nul() {
        let mut config = Config {
            shell: std::env::current_exe().unwrap(),
            args: vec![],
        };
        assert!(config.validate().is_err());
        config.shell = "/bin/sh".into();
        config.args.push("bad\0argument".into());
        assert!(config.validate().is_err());
        config.args.clear();
        assert!(config.validate().is_ok());
    }
}
