# herdr-shield — standalone Rust process continuity for Herdr

A mistaken binding, close signal or recovery decision can terminate a living workload or start a duplicate.

# Vocabulary

- config: private roots, shared shell/process façade and unsupported-platform fallback · `src/config.rs`, `src/shell.rs`, `src/unsupported.rs`
- holder: systemd-owned inner PTY, child, attachment lease and private registry · `src/holder/`
- attach: outer-terminal client and guarded holder-loss recovery · `src/attach/`
- terminal: inner emulator, query replies and controlled redraw · `src/terminal/`
- Herdr API relay: typed integration proxy and closed method policy · `src/herdr/relay.rs`, `src/herdr/schema/`, `src/herdr/policy.rs`
- binding: holder generation and public identity aliases · `src/holder/registry.rs`, `src/herdr/mapping.rs`
- reconciliation: coverage, orphan reporting and authenticated close observation · `src/herdr/reconcile.rs`, `src/herdr/observer.rs`
- lifetime: user-service placement and PID/boot/start identity checks · `src/lifetime/`
- test jail: dummy workloads, isolated paths and owned-resource cleanup · `tests/jail.rs`, `tests/herdr/jail.py`, `tests/herdr/real.py`, `tests/fakes/`
- checks: structural, mutation and real-Herdr gates · `Makefile`, `scripts/gate.sh`, `scripts/checks.sh`, `scripts/gate_run.py`, `scripts/test-herdr.sh`, `scripts/release-check.sh`, `pins.env`
- CI: `ci.yml` runs `make check` (ubuntu-latest only) and `make release` on a pull request into `main`; its `ci` job is the required check; `publish.yml` tags and releases on that pull request's merge · `.github/workflows/`
- acceptance contract: numbered promises, launch-door census and verification gaps · `docs/spec.md`

# Runtime

## Supported boundary

- Target Linux with a verified systemd user manager and holder cgroup outside the login scope; Herdr v0.9.5, commit `2ecf19a92fc3a7fff75b6ce3655ef04f4625ec01`, integration protocol 22.
- V1 protects processes launched through `terminal.default_shell` after successful holder verification. Report every other census door and unknown provenance as `UNPROTECTED` with its reason in list/action and coverage output.
- Refused platform/backend/cgroup verification prints the reason and opens the configured plain shell as `UNPROTECTED`; preserve this boundary without a `setsid` downgrade or logout/reboot guarantee.
- Require an explicit inner shell, Herdr `shell_mode = "non_login"` and session-wide `resume_agents_on_restore = false`; plugin registration does not configure them.

## Build and verification

- Use the pinned `rust-toolchain.toml`, `Cargo.lock` and the Herdr release in `pins.env`.
- Verify with `make check`: one short throwaway root holds TMPDIR, HOME, every XDG root including XDG_RUNTIME_DIR, CARGO_HOME and CARGO_TARGET_DIR; it downloads and SHA-256-verifies the pinned Herdr and runs `cargo fmt`, `cargo clippy -D warnings`, a release build (the distinct old executable HERDR_SHIELD_OLD_BIN), `cargo test --all-targets --all-features`, `scripts/checks.sh` and `scripts/test-herdr.sh`; a failed run keeps its root and prints the path.
- `make release`: Cargo.toml's version is the one version; `herdr-plugin.toml`, its `CHANGELOG.md` section and `releases/v{version}.md` agree, and tag `v{version}` does not exist yet.
- Report acceptance rows against watched results. Missing tools, refused backends, timeouts, skipped tests and undiscovered suites are named gaps, never passes; a regression counts after its unfixed failure was observed.

# Rules

## Workload and privacy safety

- Never use live Herdr configuration, state, sockets, servers or real agents for tests; validate the exact owned socket inside the test jail before every Herdr command.
- Signal only recorded owned PID/boot/start identities; test services use unique `herdr-shield-test-` names. Stop/reset those exact services, clean owned processes and sockets even on failure, and finish with an independent survivor census and resource list.
- Never infer intentional close from EOF, SIGHUP, missing sockets, failed enumeration or successful empty enumeration. Retain ambiguous holders as orphans with the reason and exact cleanup command.
- Automatic termination requires positively authenticated, generation-matching intentional close; preserve holders when provenance is insufficient. Explicit cleanup requires the exact holder, capability, live identity and confirmation.
- Never execute recovery automatically. Show holder ID, known cause or unknown and captured argv verbatim as a lossless escaped array; offer Enter only after independently proving the original process gone, recheck before execution, and execute the stored array. Other keys choose the plain shell; live, reused, remote or unverifiable identity suppresses Enter.
- Keep credentials in their original home; copy neither credentials nor full environments into state or logs. Keep recovery argv private and terminal content in bounded memory by default.
- Publication, installation, plugin linking and host configuration changes require explicit authorization; ordinary startup performs none. Preserve living holders and compatible executable copies across upgrades and removal.

## Terminal and integration

- The holder's terminal is the sole inner query responder, including while detached. Render controlled screen/cursor/mode state; queries and historical prompt markers do not become live outer-terminal operations.
- Keep the Herdr API relay pinned to its closed protocol-22 schema and exhaustive method policy. Refuse unknown/untyped/binary/admin/incompatible traffic visibly, preserve native authority and errors, and map typed identities rather than opaque strings.
- Keep binding generations authoritative for leases and streams; interrupted generations produce explicit errors. Internal TerminalId, public IDs and endpoint reuse alone do not prove durable pane identity.
- Distinguish enumeration ERROR from successful empty results at list/action/doctor surfaces. Derive compatibility and COMPLETE claims from the acceptance contract rather than prototype survival or the holder's own verdict.

