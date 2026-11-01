# Changelog

Every release of herdr-shield. Versions follow [semantic versioning](https://semver.org); each release is the `main` commit tagged `v<version>`, with a GitHub release carrying its note from [`releases/`](./releases/). Releases are source only: build the binary as the [README](./README.md#install) shows.

## [Unreleased]

## [0.1.0] — 2026-11-01

The first public release. It was developed under an earlier working name, which the commit history before this release still uses.

### Added

- A Herdr plugin that keeps the shell of a pane alive when the Herdr server crashes, is killed or restarts. Set `herdr-shield` as Herdr's `terminal.default_shell`: each new pane's shell then runs inside a holder, a transient systemd user service (`herdr-shield-{holder-id}.service`) that owns the pane's PTY, so the shell and everything running in it keep the same process, memory and terminal screen while Herdr is gone.
- `herdr-shield attach <holder-id>` reconnects a living holder from any pane, redrawing its current screen. After a cold Herdr restart the holder is kept as an orphan and the restored pane opens a plain shell marked UNPROTECTED: herdr-shield never guesses which new pane should own an old process. A Herdr live handoff (`server.live_handoff`) rebinds a holder automatically, only while the original attachment and Herdr's reported shell process both still match.
- `herdr-shield list [--json]`, the plugin action `list` and the startup `reconcile`: every pane as PROTECTED or UNPROTECTED with its reason, the full table of pane launch routes and which of them are covered, every holder with its process identities, and each orphan with its exact cleanup command. A failed pane enumeration prints ERROR, never an empty list. A read-only census also reports terminals Herdr does not list as panes, such as popups and overlays.
- `herdr-shield doctor` checks the platform, the systemd user manager, the private configuration, that Herdr's native agent resume is off, Herdr's protocol 22 and the holder registry, without changing any configuration.
- `herdr-shield kill <holder-id>` ends one exact holder after you type its full ID, or with `--confirm <the-same-id>` for scripts; piped input is never taken as consent, and the holder's recorded process identity is checked before any signal.
- Closing a pane in Herdr ends its holder only after an authenticated close of that exact binding generation is observed. An EOF, a SIGHUP, a missing socket, a failed or empty pane list and plugin event hints all keep the holder.
- When a holder is lost, a recovery screen shows the holder ID, the known cause and the captured command as a JSON array. It never reruns anything by itself: Enter is offered only once the original process is independently proven gone, and any other key opens a plain shell.
- Each holder keeps its own terminal model (Alacritty's), which answers every terminal query, including while no client is attached, and redraws the screen on reattach. Clipboard writes (OSC 52) of valid base64 up to 64 KiB reach Herdr while a client is attached; clipboard reads are never forwarded, and nothing is replayed after a detach. When no program has set a cursor shape, your terminal's own default cursor is restored.
- Programs in a protected shell reach Herdr's API through a relay pinned to protocol 22, with one explicit policy for each of its 102 methods: server administration, plugin installation, binary transport and untyped `agent.explain` are refused visibly, native errors pass through unchanged, and pane, tab and workspace IDs are mapped to the current binding.
- On a platform or system where a holder cannot be verified (not Linux, no systemd user manager, a holder cgroup inside the login session), and on any bootstrap failure, herdr-shield prints UNPROTECTED with the reason and opens the plain shell you configured.
- A holder and its API relay keep serving when one save or one accepted connection fails: the failure is logged and the loop continues. An unreadable registry record is reported, and one that names the current pane blocks a duplicate holder for it. An over-long OSC string is dropped and shown in the holder's status.
- `make check` runs the whole gate in one throwaway root: `cargo fmt`, `cargo clippy -D warnings`, a release build, the test suite, structural checks and real-server tests against the pinned Herdr 0.9.5 release, downloaded and SHA-256-verified. `make release` checks that the version, this changelog and the release note agree.

### Known limits

- Linux with a running systemd user manager only. Logout, a stop of the user manager and reboot still end protected shells; herdr-shield does not enable linger.
- Only panes started through `terminal.default_shell` are protected. Panes launched with an explicit command (layouts with a command, plugin panes, custom commands, popups, replacements) and panes already running at installation are UNPROTECTED, and every report says so.
- Herdr's native agent resume must be off for the whole session (`resume_agents_on_restore = false`), and Herdr's `shell_mode` must be `non_login`.
