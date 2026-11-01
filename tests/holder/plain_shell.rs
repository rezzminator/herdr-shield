use super::*;
use std::fs;

#[test]
fn failed_manager_cgroup_native_resume_and_recursive_launch_open_plain_shell() {
    use std::os::unix::fs::PermissionsExt;
    for scenario in ["manager", "cgroup", "native", "recursive", "config", "stop"] {
        let jail = Jail::new(match scenario {
            "manager" => "m",
            "cgroup" => "c",
            "native" => "k",
            "config" => "w",
            "stop" => "o",
            _ => "z",
        });
        // A key this version does not know is warned about, never a refusal of the shell.
        fs::write(
            jail.root.join("config/herdr-shield/config.json"),
            serde_json::to_vec(&json!({"shell":"/bin/sh","args":["-c","printf PLAIN_SAFE"],"comment":"kept by the person"}))
                .unwrap(),
        )
        .unwrap();
        let bin = jail.root.join("bin");
        fs::create_dir(&bin).unwrap();
        let mut command = jail.command();
        match scenario {
            "config" => {
                fs::write(jail.root.join("config/herdr-shield/config.json"), b"{").unwrap();
                fs::write(bin.join("login-shell"), b"#!/bin/sh\nprintf PLAIN_SAFE\n").unwrap();
                command.env("SHELL", bin.join("login-shell"));
            }
            "stop" => {
                // A refused spawn whose service stop also fails still opens the plain shell.
                fs::write(bin.join("systemctl"), b"#!/bin/sh\ncase \"$*\" in *\" stop \"*) exit 1;; *) exec /usr/bin/systemctl \"$@\";; esac\n").unwrap();
                for index in 0..8193 {
                    command.env(format!("HERDR_SHIELD_FLOOD_{index}"), "x");
                }
            }
            "manager" => {
                fs::write(bin.join("systemctl"), b"#!/bin/sh\nprintf verified\n").unwrap();
                fs::write(bin.join("systemd-run"), b"#!/bin/sh\nexit 1\n").unwrap();
            }
            "cgroup" => {
                fs::write(bin.join("systemctl"), b"#!/bin/sh\ncase \"$*\" in *--property=MainPID*) /usr/bin/systemctl \"$@\" > \"$HOME/actual-holder-pid\"; printf '%s\\n' \"$HERDR_SHIELD_JAIL_PID\";; *) exec /usr/bin/systemctl \"$@\";; esac\n").unwrap();
                command.env("HERDR_SHIELD_JAIL_PID", std::process::id().to_string());
            }
            "native" => fs::write(
                jail.root.join("config/herdr/config.toml"),
                b"[session]\nresume_agents_on_restore = true\n",
            )
            .unwrap(),
            "recursive" => {
                command.env("HERDR_SHIELD_INNER", "existing-holder");
            }
            _ => unreachable!(),
        }
        for entry in fs::read_dir(&bin).unwrap() {
            fs::set_permissions(entry.unwrap().path(), fs::Permissions::from_mode(0o700)).unwrap();
        }
        command.env("PATH", format!("{}:/usr/bin:/bin", bin.display()));
        let output = command.output().unwrap();
        assert!(output.status.success(), "{scenario}: {:?}", output.stderr);
        assert!(String::from_utf8_lossy(&output.stdout).contains("PLAIN_SAFE"));
        let reason = match scenario {
            "manager" => "service creation failed",
            "cgroup" => "cgroup verification failed",
            "native" => "resume_agents_on_restore",
            "recursive" => "recursive shield refused",
            "config" => {
                "UNPROTECTED: configuration unusable: EOF while parsing an object at line 1 column 1; opening configured plain shell"
            }
            "stop" => {
                "UNPROTECTED: bootstrap refused: \"invalid_frame\": \"too many environment entries\"; stop failed: owned service stop failed; opening configured plain shell"
            }
            _ => unreachable!(),
        };
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("UNPROTECTED") && stderr.contains(reason),
            "{scenario}: {stderr}"
        );
        assert_eq!(
            stderr
                .lines()
                .any(|line| line == "herdr-shield: ignoring unknown configuration keys: comment"),
            scenario != "config",
            "{scenario}: {stderr}"
        );
        if scenario == "stop" {
            let meta = jail.metadata();
            assert_eq!(
                (meta["state"].as_str(), meta["reason"].as_str()),
                (
                    Some("unprotected"),
                    Some(
                        "bootstrap refused: \"invalid_frame\": \"too many environment entries\"; stop failed: owned service stop failed"
                    )
                ),
                "{}",
                jail.root.display()
            );
        }
        if scenario == "cgroup" {
            let pid = fs::read_to_string(jail.root.join("actual-holder-pid")).unwrap();
            wait(|| !std::path::Path::new(&format!("/proc/{}", pid.trim())).exists());
            println!("CLEANUP unbootstrapped failed-cgroup holder {}", pid.trim());
        }
    }
}
