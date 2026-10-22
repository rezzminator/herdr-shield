use std::collections::BTreeSet;
use std::fs;

use crate::lifetime::identity::{Identity, Life};
use crate::{Result, fail};

// Popups have no public identity in v22. Observe PTY owners without reading their arguments or environment.
pub fn pty_children(server: &Identity) -> Result<Vec<Identity>> {
    if server.life() != Life::Live {
        return fail("server identity changed before PTY census");
    }
    let mut children = BTreeSet::new();
    for (count, entry) in fs::read_dir(format!("/proc/{}/task", server.pid))?.enumerate() {
        if count >= 1024 {
            return fail("server thread census exceeds bound");
        }
        let entry = entry?;
        let child_file = entry.path().join("children");
        let text = match fs::read_to_string(&child_file) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        for value in text.split_whitespace() {
            children.insert(value.parse::<u32>()?);
            if children.len() > 4096 {
                return fail("server process census exceeds bound");
            }
        }
    }
    let mut result = Vec::new();
    for pid in children {
        let stat = match fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => stat,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let (_, tail) = stat.rsplit_once(')').ok_or("malformed PTY process stat")?;
        let fields: Vec<_> = tail.split_whitespace().collect();
        if fields.len() < 20 {
            return fail("incomplete PTY process stat");
        }
        if fields[1].parse::<u32>()? == server.pid
            && fields[3].parse::<u32>()? == pid
            && fields[4] != "0"
            && !matches!(fields[0], "Z" | "X")
        {
            let identity = Identity::read(pid)?;
            if identity.start != fields[19].parse::<u64>()? || identity.life() != Life::Live {
                return fail("PTY process identity changed during census");
            }
            result.push(identity);
        }
    }
    if server.life() != Life::Live {
        return fail("server identity changed during PTY census");
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn census_rejects_reused_and_unknown_server_identity() {
        let server = Identity::read(std::process::id()).unwrap();
        let mut reused = server.clone();
        reused.start += 1;
        assert!(pty_children(&reused).is_err());
        reused.boot.clear();
        assert!(pty_children(&reused).is_err());
    }

    #[test]
    fn census_sees_an_independently_owned_real_pty_session() {
        let pair = portable_pty::native_pty_system()
            .openpty(portable_pty::PtySize::default())
            .unwrap();
        let mut command = portable_pty::CommandBuilder::new("/bin/sleep");
        command.arg("30");
        let mut child = pair.slave.spawn_command(command).unwrap();
        let pid = child.process_id().unwrap();
        let server = Identity::read(std::process::id()).unwrap();
        let result = pty_children(&server);
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(
            result.unwrap().iter().any(|identity| identity.pid == pid),
            "real PTY omitted: {stat}"
        );
    }
}
