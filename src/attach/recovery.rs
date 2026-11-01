use std::io::{Read, Write};
use std::os::unix::process::CommandExt;

use crate::config::{Config, command};
use crate::holder::registry::{Metadata, Resume};
use crate::lifetime::identity::{Identity, Life};
use crate::{Result, fail};

pub fn can_resume(identity: Option<&Identity>) -> bool {
    identity.is_some_and(|id| id.life() == Life::Gone)
}

pub fn display(argv: &[String]) -> Result<String> {
    if argv.is_empty() || argv.iter().any(|arg| arg.contains('\0')) {
        return fail("invalid captured argv");
    }
    Ok(serde_json::to_string(argv)?)
}

/// The plain shell when config.json is unusable: `$SHELL` when it validates, else /bin/sh.
pub fn fallback() -> Config {
    std::env::var_os("SHELL")
        .map(|shell| Config {
            shell: shell.into(),
            ..Config::default()
        })
        .filter(|config| config.validate().is_ok())
        .unwrap_or_else(|| Config {
            shell: "/bin/sh".into(),
            ..Config::default()
        })
}

/// The argv line shown after holder loss and whether Enter may run it; an argv that
/// cannot be shown is never offered.
fn offer(meta: &Metadata, resume: &Resume) -> (String, bool) {
    match display(&resume.argv) {
        Ok(text) => (
            text,
            meta.resume.is_some() && can_resume(resume.process.as_ref()),
        ),
        Err(error) => (format!("unavailable ({error})"), false),
    }
}

pub fn plain(config: &Config, reason: &str) -> Result<()> {
    eprintln!("herdr-shield: UNPROTECTED: {reason}; opening configured plain shell");
    config.validate()?;
    let error = command(&config.shell)
        .args(&config.args)
        .env_remove("HERDR_SHIELD_INNER")
        .exec();
    Err(error.into())
}

pub fn missing(meta: &Metadata, config: &Config, cause: &str) -> Result<()> {
    println!("\r\nherdr-shield holder {} unavailable\r", meta.id);
    println!("cause: unknown; connection failure: {cause}\r");
    if matches!(meta.state.as_str(), "exited" | "terminated") {
        println!("recorded lifecycle reason: {}\r", meta.reason);
    }
    let Some(resume) = meta.resume.as_ref().or(meta.pending_resume.as_ref()) else {
        println!(
            "captured resume argv: unavailable\r\nProcess death is unproven. Resume is not offered.\r"
        );
        println!("Any key opens a plain shell.\r");
        std::io::stdout().flush()?;
        let raw = super::RawTerminal::enter()?;
        std::io::stdin().read_exact(&mut [0u8; 1])?;
        drop(raw);
        return plain(config, "holder missing; no captured resume argv");
    };
    let (shown, allowed) = offer(meta, resume);
    println!("captured resume argv: {shown}\r");
    if allowed {
        print!(
            "Original process independently proven gone. Enter runs the exact argv once; any other key opens a plain shell.\r\n"
        );
    } else {
        print!(
            "Original process is live or unverifiable, or resume arbitration is unconfirmed. Resume is not offered. Any key opens a plain shell.\r\n"
        );
    }
    std::io::stdout().flush()?;
    let raw = super::RawTerminal::enter()?;
    let mut key = [0u8; 1];
    std::io::stdin().read_exact(&mut key)?;
    drop(raw);
    if allowed && (key[0] == b'\r' || key[0] == b'\n') {
        // Recheck at the decision boundary; execute the stored array, never its display text.
        if !can_resume(resume.process.as_ref()) {
            return plain(config, "process death proof changed");
        }
        let error = command(&resume.argv[0])
            .args(&resume.argv[1..])
            .current_dir(&meta.cwd)
            .env_remove("HERDR_SHIELD_INNER")
            .exec();
        return Err(error.into());
    }
    plain(config, "user selected plain shell after holder loss")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_argv_display_roundtrips_empty_quotes_controls_and_unicode() {
        let argv = vec![
            "dummy".into(),
            "".into(),
            "a b".into(),
            "a'b\"c\\d\n\x1b".into(),
            "界".into(),
        ];
        let text = display(&argv).unwrap();
        assert_eq!(serde_json::from_str::<Vec<String>>(&text).unwrap(), argv);
        assert!(!text.contains('\x1b'));
        assert!(display(&["a\0b".into()]).is_err());
    }

    #[test]
    fn undisplayable_captured_argv_is_shown_unavailable_and_never_offered() {
        let binding = crate::holder::registry::Binding {
            socket: "/socket".into(),
            pane: "w1:p1".into(),
            tab: "w1:t1".into(),
            workspace: "w1".into(),
            server: Identity::read(std::process::id()).unwrap(),
        };
        let mut meta = Metadata::new(binding, "/".into()).unwrap();
        let mut child = crate::config::command("/bin/sh")
            .args(["-c", "exit 0"])
            .spawn()
            .unwrap();
        let exited = Identity::read(child.id()).unwrap();
        child.wait().unwrap();
        for (argv, expected) in [
            (vec!["dummy".to_owned()], (r#"["dummy"]"#.to_owned(), true)),
            (
                vec![],
                ("unavailable (invalid captured argv)".to_owned(), false),
            ),
            (
                vec!["a\0b".to_owned()],
                ("unavailable (invalid captured argv)".to_owned(), false),
            ),
        ] {
            let resume = Resume {
                argv,
                process: Some(exited.clone()),
                source: "custom:dummy".into(),
                agent: "dummy".into(),
                seq: None,
            };
            meta.resume = Some(resume.clone());
            assert_eq!(offer(&meta, &resume), expected);
        }
    }

    #[test]
    fn live_unknown_and_reused_identities_never_offer_resume() {
        assert!(!can_resume(None));
        let mut identity = Identity::read(std::process::id()).unwrap();
        assert!(!can_resume(Some(&identity)));
        identity.start += 1;
        assert!(!can_resume(Some(&identity)));
        let mut child = crate::config::command("/bin/sh")
            .args(["-c", "exit 0"])
            .spawn()
            .unwrap();
        let exited = Identity::read(child.id()).unwrap();
        child.wait().unwrap();
        assert!(can_resume(Some(&exited)));
    }
}
