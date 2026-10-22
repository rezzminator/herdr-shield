use std::fs;

use serde::{Deserialize, Serialize};

use crate::{Result, fail};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub pid: u32,
    pub boot: String,
    pub start: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Life {
    Live,
    Gone,
    Unknown,
}

impl Identity {
    pub fn read(pid: u32) -> Result<Self> {
        if pid == 0 {
            return fail("invalid zero process ID");
        }
        let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id")?
            .trim()
            .to_owned();
        let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
        Ok(Self {
            pid,
            boot,
            start: start_time(&stat)?,
        })
    }

    pub fn life(&self) -> Life {
        if self.boot.is_empty() || self.start == 0 || self.pid == 0 {
            return Life::Unknown;
        }
        let Ok(boot) = fs::read_to_string("/proc/sys/kernel/random/boot_id") else {
            return Life::Unknown;
        };
        if boot.trim() != self.boot {
            return Life::Unknown;
        }
        match Self::read(self.pid) {
            Ok(current) if current == *self => Life::Live,
            // Reused PID is not a trustworthy absence oracle for recovery.
            Ok(_) => Life::Unknown,
            Err(_)
                if self.pid <= i32::MAX as u32
                    && matches!(
                        nix::sys::signal::kill(nix::unistd::Pid::from_raw(self.pid as i32), None),
                        Err(nix::errno::Errno::ESRCH)
                    ) =>
            {
                Life::Gone
            }
            Err(_) => Life::Unknown,
        }
    }
}

fn start_time(stat: &str) -> Result<u64> {
    let tail = stat.rsplit_once(')').ok_or("invalid proc stat")?.1;
    Ok(tail
        .split_whitespace()
        .nth(19)
        .ok_or("missing start time")?
        .parse()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn independent_identity_rejects_reused_and_unverifiable_pid() {
        let mut id = Identity::read(std::process::id()).unwrap();
        assert_eq!(id.life(), Life::Live);
        id.start += 1;
        assert_eq!(id.life(), Life::Unknown);
        id.boot.clear();
        assert_eq!(id.life(), Life::Unknown);
    }

    #[test]
    fn stat_names_with_parentheses_do_not_shift_start_time() {
        let stat = format!("1 (a b ) x) S {} 1234 0", "0 ".repeat(18));
        assert_eq!(start_time(&stat).unwrap(), 1234);
    }
}
