# herdr-shield build specification

Herdr pane → `herdr-shield` attach client → private holder socket → systemd-user-owned holder → inner PTY + terminal model → user shell → agent

## Contents

- [Build boundary](#build-boundary)
- [Evidence and eleven resolved unknowns](#evidence-and-eleven-resolved-unknowns)
- [Closed launch-door census](#closed-launch-door-census)
- [Numbered acceptance contract](#numbered-acceptance-contract)
- [v2 — needs an upstream Herdr facility](#v2--needs-an-upstream-herdr-facility)
- [Components and interfaces](#components-and-interfaces)
- [Holder protocol](#holder-protocol)
- [Herdr API relay](#herdr-api-relay)
- [Terminal contract](#terminal-contract)
- [Failures and recovery](#failures-and-recovery)
- [Build layout and verification](#build-layout-and-verification)
- [Install and orientation deliverables](#install-and-orientation-deliverables)
- [Upstream change draft](#upstream-change-draft)
- [Research limits and cleanup](#research-limits-and-cleanup)

## Build boundary

Build a standalone Rust Herdr plugin named `herdr-shield`. This document is the complete builder brief; do not infer implementation from the research prototype. The plugin preserves the **same running child process and PTY**, not merely an agent conversation or terminal image, when the outer Herdr server disappears. No plugin implementation exists yet. Every proposed implementation behavior below is **UNVERIFIED** until its named acceptance runs; verified research observations are explicitly distinguished.

**V1 protection boundary:** protect each pane whose process starts through `terminal.default_shell`. All other launch doors are explicitly `UNPROTECTED` and remain visible with their reason in the `list` action and coverage report. This default-shell-only scope is authorized for v1. Explicit-command panes bypass `terminal.default_shell`; the probe confirmed the bypass by killing its direct-command dummy. Universal pre-spawn coverage and restore cooperation are v2 requirements, not v1 build gates. Panes already running when herdr-shield is configured are `UNPROTECTED`; they require a deliberate workload-ending replacement and cannot be migrated by a startup hook.

V1 supports Linux only when a systemd user manager is available and verified, including positive verification that the holder escaped the login cgroup. On other platforms, when the user manager is unavailable or fails, or cgroup verification fails, print a clear refusal, run the configured plain inner shell so the pane still opens, and report that pane as `UNPROTECTED` with the reason. Never silently fall back to `setsid`. Do not promise logout/reboot survival, enable linger, change host services, or claim macOS support. One ordinary binary and Herdr plugin manifest; no framework runtime, framework agents, policies, wording, or hardcoded development-machine paths in code, help, services, configuration, or generated state. Evidence paths in this specification are research citations only. Acknowledgment in the eventual README/manifest is the sole allowed credit exception.

Out of scope: implementing Herdr changes, importing existing PTYs, checkpointing process memory across reboot, real agent/credential probes, live installation, and changing unrelated projects. The builder writes implementation, tests, README, manifest, and its own `CLAUDE.md`; this specification writes none of those files.

## Evidence and eleven resolved unknowns

All `S/` references mean the upstream `herdrdev/herdr` source tree at commit `2ecf19a92fc3a7fff75b6ce3655ef04f4625ec01`, tag v0.9.5. `L/` (logs), `R/` (spec review) and `D/` (dependencies) name the research run's private evidence directories, which this repository does not ship; its research report is not shipped either. These are evidence locations, never runtime defaults. Source facts essential to the build are inlined here; no prior chat or report is required to understand the design.

### 1. Agent authority and resume arbitration

There is one reported-resume slot, not a per-source stack. Matching live authority, or a matching detected known agent without authority/recent exit, can publish it (`S/src/terminal/state.rs:1931–1941`). Source sequences reject equal/older reports and later unsequenced reports (`1944–1951`); `ok` alone need not mean mutation (`S/src/app/api/panes.rs:1570–1596,1632–1641`). A non-owner reaching the final handler gets `resume_not_accepted`: `resume_argv requires the reporter to hold the pane; report its state with pane.report_agent first` (`1668–1698`). Explicit resume outranks native session resume; both obey the global restore switch (`S/src/persist/restore.rs:813–830`).

Observed custom `shield` ownership/resume accepted, competing native `herdr:claude` and `shield2` resume rejected; the failed native request nevertheless installed native session data. Thus errors are not atomic no-ops. Shield must **not compete for native authority or publish attach as native resume**. Relay reports unchanged except typed identity mapping; store fallback separately, and never infer acceptance merely from `ok`. `herdr:claude` specifically reports `pane.report_agent_session` at SessionStart and is not full-lifecycle authority (`S/src/integration/assets/claude/herdr-agent-state.sh:32–83`, `S/src/detect/mod.rs:330–349`). Evidence: `L/021-baseline-restored.log`; idle clearing below.

### 2. Restore identities, shell, environment, and timing

Format-3 snapshots save public mappings, cwd, labels, session/resume refs and launch argv, not an arbitrary original environment (`S/src/persist/snapshot.rs:11–28,51–120`; ID remap `S/src/persist/restore.rs:354–403`). Public `w1:p1` survived cold restore; internal `TerminalId` changed. Startup can enumerate before client attachment. Native resume defers runtime creation (`restore.rs:568–603`), waits for client/theme readiness (`S/src/app/agent_resume.rs:48–94,236–239`), starts the configured shell, then injects escaped argv plus CR (`266,307–308`), not exec. Missing cwd reports `restore_error` (`257–262`). `launch_argv` only returns as imported-handoff metadata, not cold replay (`restore.rs:540,632–650,692–695`).

Restore reconstructs HERDR identities with empty extra env (`restore.rs:572–580`). Herdr sets `TERM=xterm-256color`, `COLORTERM=truecolor`, `TERM_PROGRAM=herdr`, `TERM_PROGRAM_VERSION=0.9.5` and strips nesting variables (`S/src/pane.rs:100–135,184–204`); observed `HERDR_ENV`, `HERDR_BIN_PATH`, `HERDR_SOCKET_PATH`, `HERDR_WORKSPACE_ID`, `HERDR_TAB_ID`, `HERDR_PANE_ID` were reconstructed. `default_shell` is one executable: Linux auto is non-login; login mode uses SHELL/default-program and login argv0, not a guaranteed `-l` (`pane.rs:2023–2031,2123–2155`). Preserve the inner child's holder socket/birth identity; attach updates only upstream binding. Evidence: `L/021-baseline-restored.log` (fresh bash PID 2948321, dummy 2948323), `L/023-holder-feasibility.log`. No fixed sleep is a restore-readiness contract.

### 3. Process detection and shell integration

Herdr inspects outer PTY foreground processes. Idle means foreground process group equals the pane child, contains only that PID, and is a recognized shell (`S/src/platform/mod.rs:483–495`, `S/src/platform/linux.rs:345–365`). Unknown self-reported agents are checked about once a second (`S/src/pane.rs:383–410`); clearing also removes reported resume (`S/src/terminal/state.rs:1885–1906`). A detached inner agent is invisible to that topology. `agent.start` checks shell availability and can return `agent_pane_busy` (`S/src/app/agents.rs:187–201,263–265`). Forwarding OSC 7/133, titles and progress cannot repair kernel process discovery. Preserve supported metadata ordering; do not spoof shell/process detection or turn busy errors into success. Native holder-aware detection requires upstream cooperation. Evidence: `L/027-idle-cause.log`, `L/029-events-restored.log`: idle clearing occurred without establishing OSC133 as its cause.

### 4. Terminal ownership and redraw

Herdr generates terminal replies (`S/src/pane/terminal.rs:1491–1563`), resizes emulator and PTY (`1728–1738`, `S/src/pty/actor/unix.rs:45–64`), and restores interaction modes during handoff (`terminal.rs:1594–1655`). Probe `L/044-terminal-direct-probe.log` observed DA1 `ESC[?62;22c`, DA2 `ESC[>1;0;0c`, CPR `ESC[3;1R`, XTGETTCAP TN=`xterm-256color`, SIGWINCH with pane size 83×31→105×39, and bracketed paste. OSC10/11 returned no bytes within 400 ms, not proof of unsupported queries; real mouse input was UNRUN. Choose one query owner: holder emulator answers inner queries even detached; renderer emits controlled screen/cursor/mode state, not raw queries answered again by Herdr. A byte-tail prototype proved PID survival, **not** VT redraw. Emulator API evidence: `D/alacritty_terminal/source/alacritty_terminal-0.26.0/src/event.rs:14–58`; full renderer/query arbitration remains UNVERIFIED.

### 5. Intentional pane close versus server loss

Exact manifest hook names (`S/src/api/schema/events.rs:224–251,286–309`): `workspace.created/updated/closed/renamed/moved/reordered/focused`; `worktree.created/opened/removed`; `tab.created/closed/renamed/moved/focused`; `pane.created/closed/focused/moved/exited/agent_detected/agent_status_changed`. Prefix shorthand expands each suffix; `pane.updated`, `pane.output_changed`, `layout.updated`, and `workspace.metadata_updated` exist as JSON events but are **not plugin hooks**. Hooks receive dotted `HERDR_PLUGIN_EVENT`; their JSON envelope uses snake_case tags. Explicit API close removes the pane/runtime and emits `pane.closed` (`S/src/app/api/panes.rs:1967–2028`); natural exit emits `pane.exited` (`S/src/app/api.rs:309–310`). Runtime drop shuts down its session with HUP→TERM→KILL unless handing off (`S/src/pane.rs:1702–1717,1760–1807`). Evidence `L/050-event-outcomes.log` distinguishes live close from exit. EOF/SIGHUP cannot distinguish close from crash; retain ambiguous holders. Prototype `L/023-holder-feasibility.log` retained its holder even on explicit close: automatic teardown remains UNVERIFIED.

### 6. Socket protocol and identity surface

The integration socket is UTF-8 newline-delimited JSON, initial request limit 1 MiB: `{"id":"x","method":"pane.get","params":{"pane_id":"w1:p1"}}\n`. Response is the same `id` plus `result:{type,...}` or `error:{code,message}`. `ping` advertises protocol 22; this is not a JSON per-request version handshake. The separate terminal-client transport is u32 little-endian length plus bincode, protocol 22; refuse it on shield sockets. All **102** `Method` variants were positively enumerated, not inferred from grep absence. Typed request/response/event identities require structured mapping; untyped `agent.explain` is refused. Evidence: `S/src/api/schema.rs:35–252`, `S/src/api/schema/response.rs:24–280`, `S/src/api/client.rs:184–188,222–228`, `S/src/api/server.rs:34,738–766`, `S/src/protocol/wire.rs:20–35,464–617,1646–1707,1756–1766`, `L/042-exhaustive-protocol.log`, `L/043-closed-universe-summary.log`.

### 7. Handoff and startup hooks

Same-binary `server.live_handoff` preserved a dummy child and ran startup hooks against a view with a changed internal terminal ID. The handoff passes PTY FDs using SCM_RIGHTS; it is not a fresh inner process launch. Rebind by public pane identity plus server/session generation, never `TerminalId`. Combined shield-holder + handoff, cross-version handoff, and actual `update --handoff` installation are **UNRUN**. Evidence: `S/src/server/handoff.rs:394–425`, `S/src/terminal/runtime.rs:63`, `L/042-exhaustive-protocol.log`, `R/verified-observations.log`.

### 8. Lifetime and platform boundary

A `setsid` holder remained inside the login `session-521.scope`; detaching a POSIX session does not escape the systemd/logind cgroup. A throwaway systemd user service moved a dummy to `user@1000.service/app.slice`, positively outside that login scope. Require that backend and verify the actual cgroup after launch. No silent `setsid` downgrade. Logind `KillUserProcesses`, user-manager shutdown, explicit service kill, logout, and reboot remain distinct risks; no logout/reboot survival promise and no automatic linger changes. Linux without a usable user manager and macOS are unsupported in the proposed first release. Evidence: `L/041-scope-service-probe.log`; `D/portable-pty/source/portable-pty-0.9.0/src/unix.rs:257–271`. Systemd documents session-scope killing separately from user-manager lifetime/linger: `https://www.freedesktop.org/software/systemd/man/latest/logind.conf.html` and `https://www.freedesktop.org/software/systemd/man/latest/systemd-run.html`, downloaded at `L/040-systemd-download.log`. A commented `KillUserProcesses=no` is not an effective-policy measurement. Logout/macOS behavior **UNRUN**.

### 9. Rust dependencies

Select exact initial versions: `portable-pty = 0.9.0` (MIT; WezTerm project; published 2025-02-11) for owned master/child read-write-resize-wait APIs; `alacritty_terminal = 0.26.0` (Apache-2.0; published 2026-04-06; Rust ≥1.85) for a full terminal model with explicit query events and keyboard modes; `tokio = 1.54.1` (MIT; published 2026-10-21; Rust ≥1.85) for Unix sockets/tasks/backpressure, with blocking PTY work isolated from async workers; `nix = 0.31.3` (MIT; published 2026-05-11) for Unix credential/signal operations absent from PTY APIs. Versions are non-yanked in saved registry metadata; publication recency is maintenance evidence, not a future-support guarantee. All selected crate metadata/checksums were saved; PTY/emulator source archives were inspected, but Rust builds/compatibility are UNRUN. Evidence: `D/{portable-pty,alacritty_terminal,tokio,nix}/metadata.json`, `D/fetch.log`, `D/archive.log`, `R/dependency-interface.log`, `R/native-resume-and-version-contracts.log`. Canonical version citations are `https://crates.io/crates/portable-pty/0.9.0`, `https://crates.io/crates/alacritty_terminal/0.26.0`, `https://crates.io/crates/tokio/1.54.1`, `https://crates.io/crates/nix/0.31.3`.

`vt100 0.18.1` is also maintained by the registry-recency measure (MIT; published 2026-10-19; `D/vt100/metadata.json`), so it is not rejected as abandoned. Prefer Alacritty's inspected query-event and mode interfaces for the single-emulator-authority design (`src/event.rs:14–58`, `src/term/mod.rs:55–85` under its downloaded crate); the cost is implementing an explicit VT renderer. No compatibility result is claimed for wezterm-term or other unevaluated alternatives. Keep Cargo.toml/Cargo.lock as the dependency version source. Add serde/serde_json/base64 only with validated registry metadata/checksums and compatible licenses; exact resolution is a builder gate, not an invented research result.

### 10. Holder loss, recovery, and upgrades

A new client must negotiate with an old running holder; never kill/recreate it merely to upgrade. Preserve versioned executable copies under the user-selected install root and retain a compatible old attach client while any holder requires it; the stable launcher can move to a new copy. Report the exact compatible-client path on major mismatch. Wire major 1 remains mandatory across compatible binary upgrades; an incompatible future major must coexist rather than replace living major-1 holders. Missing holder is not proof the captured agent died. Show holder ID, known loss reason or `unknown`, and exact captured resume argv. Offer Enter-to-resume only after independently proving the captured session process gone; any other key chooses a plain shell. If proof is unavailable, say so and do not offer Enter. This guard, UI, versioned protocol, and fallback implementation are **UNVERIFIED**; research established only the Herdr resume input constraints and process-survival feasibility. Evidence: `L/021-baseline-restored.log`, `L/023-holder-feasibility.log`, protocol/version source above.

### 11. Manifest, startup, actions, and installation

Use a normal `herdr-plugin.toml` with startup/event reconciliation, a list action and explicitly confirmed CLI kill. Startup runs after restore and live handoff; it does not supply a pre-spawn wrapper. No manifest field sets `terminal.default_shell`; installation must tell the user to configure the executable explicitly. The default-shell research path requires `session.resume_agents_on_restore = false`, because automatic native resume injects commands into an already reattached living shell. Native resume is thereby disabled for the entire Herdr session, including unprotected panes: disclose this cost. Exact manifest fields and argv are inlined below, verified against `S/src/app/api/plugins/manifest.rs:12–96` and command execution `S/src/app/api/plugins/runtime.rs:124–129,161–249`. Evidence: `L/029-events-restored.log`, `L/032-handoff-observed.log`, `S/src/app/agent_resume.rs:266,307–308`. Stock registration cannot fulfill v2 universal pre-spawn coverage; v1 reports uncovered panes as `UNPROTECTED`.

## Closed launch-door census

`PROTECTED` means a v1 pane process starts through `terminal.default_shell` and is successfully placed under a verified systemd user manager. It does not cover any other launch door. `UNPROTECTED` panes must appear in both `list` action output and the coverage report with the stated reason. All implementation results remain UNVERIFIED. Four fresh-PTY factories plus imported-FD handoff were positively traced through the common backend; test-only callers were excluded by `cfg(test)`.

| Door | Classification | Source at pinned commit | Observation / required coverage |
| --- | --- | --- | --- |
| Interactive initial workspace, new tab, ordinary split | PROTECTED when process starts through `terminal.default_shell` | `S/src/workspace.rs:369,384,462,540,555`; `S/src/workspace/tab.rs:137–155,349–379` | Default-shell prototype `L/023-holder-feasibility.log`; exercise each UI route |
| JSON/CLI `workspace.create`, `tab.create`, `pane.split` | PROTECTED when process starts through `terminal.default_shell` | `S/src/api/schema/workspaces.rs:8–21`; `tabs.rs:8–19`; `panes.rs:19–34`; `S/src/app/api/tabs.rs:83` | These schemas have no command field; CLI tab uses the same params (`S/src/cli/tab.rs:117`) |
| CLI `pane run` / API send-input | UNPROTECTED unless the existing pane was launched through `terminal.default_shell` | `S/src/cli/pane.rs:1038–1053` | Sends input to an existing shell; it does not create or protect that process |
| `layout.apply`, leaf without command | PROTECTED | `S/src/app/api/layouts.rs:116–128,431–439` | Exercise new-tab and split paths |
| `layout.apply`, explicit leaf `command` argv | UNPROTECTED | Same dispatch; `S/src/api/schema/panes.rs:195–205` | Explicit command bypasses `terminal.default_shell`; direct-command probe died on server kill (`L/044-terminal-direct-probe.log`) |
| Plugin pane, split/tab/popup/overlay command | UNPROTECTED | `S/src/app/api/plugins/panes.rs:27,58,133,212` | Explicit plugin command bypasses `terminal.default_shell` |
| Custom command pane / overlay / split | UNPROTECTED | `S/src/app/custom_commands.rs:363,440,480` | Explicit shell-command or argv bypasses `terminal.default_shell` |
| Popup shell-command / argv | UNPROTECTED | `S/src/app/popup.rs:47,79` | Explicit popup command bypasses `terminal.default_shell`; include it in coverage reporting |
| Production shell replacement/respawn | PROTECTED only when process starts through configured default shell | `S/src/app/api.rs:607` | Replacement using another command is UNPROTECTED because it bypasses `terminal.default_shell`; never infer child death from replacement alone |
| Cold snapshot normal/history pane | PROTECTED only when restored process starts through configured default shell | `S/src/persist/restore.rs:632–650`; `S/src/pane.rs:2395` | A restored explicit command is UNPROTECTED; public ID stable, TerminalId not stable; attach by private registry |
| Saved `launch_argv` on cold restore | UNPROTECTED when it launches a process outside `terminal.default_shell` | `S/src/persist/restore.rs:540,692–695` | Not reexecuted by native cold restore; do not invent automatic argv replay |
| Native resume command | DISABLED prerequisite; resumed pane is protected only if its process starts through `terminal.default_shell` | `S/src/app/agent_resume.rs:266,307–308` | Shell + injected argv; nested same-holder attach must not create a second holder |
| Live-handoff imported PTY | UNPROTECTED unless its already-running process was launched through `terminal.default_shell` | `S/src/terminal/runtime.rs:63`; `S/src/server/handoff.rs:394–425` | Imported FD is not a fresh process start through the wrapper; same-binary dummy survived, shield combination UNRUN |
| Preexisting pane/agent at installation | UNPROTECTED | No PTY adoption hook in factories | Existing process did not start through `terminal.default_shell`; list/report this reason and require deliberate replacement after ending its workload |

Factory closure: `S/src/terminal/runtime.rs:86,119,154,189` → `S/src/pane.rs:2395,2418–2452,2454–2498,2687` → `S/src/pty/backend/unix.rs:34`. Imported FD is the fifth, non-spawn route. API test callers at `api.rs:1872,1965`, notification/snapshot test helpers, and test fixtures are not product doors; verification census: `R/closed-door-census.log`, `R/exact-launch-door-details.log`.

## Numbered acceptance contract

Each row is one promise with its own observable acceptance. Status for all builder rows is **UNVERIFIED**; the evidence column gives the source/probe motivating it, not a claim it passed. Research rows 1–3 are already observed.

### 2. Off-server PTY ownership survives server kill

Acceptance: holder and child PID remain live after crash and restored attachment. Evidence: `L/023-holder-feasibility.log`; VERIFIED prototype PID survival only, not terminal/counter validation.

### 3. Lifetime backend escapes login scope

Acceptance: actual cgroup is a systemd user service outside login scope. Evidence: `L/041-scope-service-probe.log`; VERIFIED dummy only.

### 5. Coverage failures are visible

Acceptance: list action and coverage report identify every pane not positively proven protected through `terminal.default_shell` as `UNPROTECTED` with a reason, including each census door outside that route and panes already present at install. If launch provenance is unavailable, say so and classify the pane `UNPROTECTED`; do not omit it. Enumeration failure reports ERROR. A successful empty enumeration is distinct from failure. Evidence: census; UNVERIFIED.

### 6. Reattach preserves process identity

Acceptance: server SIGKILL then restored attachment retains child PID/boot/start identity and monotonic in-memory counter; no duplicate. Evidence: prototype; UNVERIFIED production/counter behavior.

### 7. Input and resize have one owner

Acceptance: competing clients cannot write; stale generation input and resize fail. Evidence: protocol design; UNVERIFIED.

### 8. Detached output is bounded and redrawable

Acceptance: detached output flood leaves bounded memory; reconnect has independently asserted cells/cursor/modes. Evidence: terminal design; UNVERIFIED.

### 9. Close teardown requires positive intent

Acceptance: trusted completed pane-close kills only its bound holder; moved/reused identity does not. Evidence: `S/src/app/api/panes.rs:1967–2028`; UNVERIFIED implementation.

### 10. Ambiguous loss retains holders

Acceptance: EOF, SIGHUP, enumeration failure and successful empty result each preserve children. Evidence: fail-safe design; UNVERIFIED.

### 11. Orphans are actionable

Acceptance: list shows holder ID, orphan reason and exact cleanup command. Evidence: lifecycle design; UNVERIFIED.

### 12. Explicit kill targets the correct holder

Acceptance: confirmation is required; mismatched capability/PID/start identity signals nobody. Evidence: protocol design; UNVERIFIED.

### 13. Missing-holder UI is truthful

Acceptance: show ID, known reason or unknown, and safely encoded exact resume argv. Evidence: recovery design; UNVERIFIED.

### 14. Resume requires proof of process death

Acceptance: live/reused/remote/unverifiable identity suppresses resume; independently proved-gone identity permits it. Evidence: recovery design; UNVERIFIED.

### 15. Resume requires confirmation

Acceptance: Enter executes stored argv once; other keys select a plain shell; displayed text is never parsed. Evidence: recovery design; UNVERIFIED.

### 16. Agent authority stays native

Acceptance: native/custom/stale-sequence/rejected-resume reports retain real upstream outcomes; fallback capture never replaces shield binding. Evidence: `L/021-baseline-restored.log`; UNVERIFIED relay.

### 17. API identity mapping is exhaustive

Acceptance: all 102 methods have one policy; request/result/event cases cover nested, optional, moved and stale aliases. Evidence: `S/src/api/schema.rs:47–252`; UNVERIFIED implementation.

### 18. Refusal is explicit

Acceptance: unknown, binary, admin, untyped, malformed and incompatible upstream requests fail visibly, never success-shaped. Evidence: relay policy; UNVERIFIED.

### 19. Streams preserve lifetime

Acceptance: subscribe/wait/cancel/reconnect preserve independent upstream connection state. Evidence: `S/src/api/server.rs`; UNVERIFIED.

### 20. Terminal queries have one responder

Acceptance: arbitrary-byte-split DA/DSR/color/size queries produce exactly one correct reply. Evidence: terminal source and query probe; UNVERIFIED holder.

### 21. Terminal interactions round-trip

Acceptance: independently expected grids/input cover resize, Unicode, alternate screen, paste, mouse, reset and theme. Evidence: terminal design; UNVERIFIED.

### 22. Live handoff preserves holder and binding

Acceptance: real holder plus same-binary Herdr handoff retains inner identity and rebinds once. Evidence: `L/032-handoff-observed.log`; UNVERIFIED combined case.

### 23. Startup reconciliation is non-destructive

Acceptance: repeated startup/attach/handoff is idempotent; failed or empty enumeration retains holders. Evidence: startup probe; UNVERIFIED implementation.

### 24. Upgrades preserve old holders

Acceptance: new client attaches supported old wire; incompatible major errors without killing or replacing it. Evidence: versioned protocol design; UNVERIFIED.

### 25. Runtime state is private and independent

Acceptance: permission, symlink/peer, log-redaction and forbidden-origin-string checks pass. Evidence: security design; UNVERIFIED.

### 26. Unsupported environments fail safely

Acceptance: unsupported platform, unavailable/failed systemd user manager, or failed cgroup verification prints a clear refusal, runs the configured plain inner shell, and marks the pane `UNPROTECTED`; no `setsid` fallback. Enabled native auto-resume or recursive shell is also refused safely. Evidence: lifetime probe/config source; UNVERIFIED implementation.

### 27. Test jail rejects real sessions

Acceptance: broken-jail negative control refuses outside-root sockets/processes; fault tests end with zero owned survivors. Evidence: test design; UNVERIFIED.

### 28. Installation matches pinned schema

Acceptance: real isolated Herdr parses manifest and follows README without implicit configuration mutation. Evidence: `S/src/app/api/plugins/manifest.rs:12–96`; UNVERIFIED implementation.

### 29. Gates detect broken artifacts

Acceptance: broken controls fail formatting/lint/test/structure checks; production artifact passes every named command. Evidence: verification design; UNVERIFIED.

## v2 — needs an upstream Herdr facility

The requirements below keep their original acceptance-row IDs for traceability and are not v1 build gates. V2 requires universal pre-spawn and restore cooperation in Herdr. V1 reports every uncovered door in `list` and the coverage report.

### 1. Explicit-command bypass is covered

Acceptance: after the upstream facility exists, reproduce the `layout.apply` explicit-command path and verify its dummy is holder-owned and survives server SIGKILL. Evidence: `L/044-terminal-direct-probe.log`; the probe verified the current bypass, not this future requirement.

### 4. Every fresh pane is protected automatically

Acceptance: the universal pre-spawn facility covers every fresh-spawn census door; independently inspect each PTY owner. Evidence: source census; UNVERIFIED, pending the upstream facility.

## Components and interfaces

### `config`

One path/config façade resolves the user's HOME/XDG roots, explicit shield configuration, executable locations, server identity and test jail. Use `$XDG_RUNTIME_DIR/herdr-shield/` for private sockets when available, otherwise a securely created private runtime directory below the user's state root; use `$XDG_STATE_HOME/herdr-shield/` for metadata, defaulting to `$HOME/.local/state/herdr-shield/`. Configuration lives below `$XDG_CONFIG_HOME/herdr-shield/`, default `$HOME/.config/herdr-shield/`. Never derive an unlink or kill target from an unchecked peer path or PID. Keep socket paths below Unix path limits. Roots 0700; files/sockets 0600; reject symlinked or wrong-owner roots. A missing required HOME/runtime identity is an error, not a reason to use another user's paths.

Choose the real inner shell explicitly at installation from a user-confirmed executable path; reject `herdr-shield`, symlink aliases to itself, and recursive attach. Preserve a configured original shell and argv; do not use `$SHELL` blindly after changing Herdr's default shell. Login/startup mode is explicit shield configuration, not a guessed invocation of Herdr's `shell_mode` on the inner shell. Capture cwd as an OS path and environment via an allow/override policy: retain normal user variables, replace `HERDR_SOCKET_PATH` with the private relay, preserve the birth `HERDR_PANE_ID`, and keep current upstream identities only in the holder binding. Never copy credentials between homes. Record no full environment in logs.

### `holder`

One holder owns one inner PTY, shell child, bounded terminal/scrollback model, relay endpoint, attachment lease and private recovery metadata. Launch as a transient systemd user service with fixed validated executable/argv; avoid shell interpolation. Confirm child start and cgroup placement before declaring protection. If service creation/verification fails, stop only the just-created owned process and present an explicit unprotected/error outcome; do not pretend detached survival. The attach launcher then prints a clear refusal and runs the configured plain inner shell in the pane, which remains `UNPROTECTED`; never silently fall back to `setsid`. Use transient service properties `Restart=no`, `KillMode=control-group`; no permanent unit files. Start the holder without its workload, then transmit validated cwd/argv/environment over an authenticated private bootstrap connection; user-manager environment is not the pane environment, and full environments never go in durable files. Spawn the workload only after bootstrap and cgroup verification. The holder must not inherit server/client control FDs. Keep precise process identity (PID + boot identity + start time and retained OS handle where supported), not a stale PID alone. Inner shell exit is observed through the owned child handle; its exit status is shown on next attach.

Metadata is atomically replaced and includes schema version, random holder ID, creation/boot identity, executable/wire versions, birth server/session/pane/tab/workspace identity, current binding generation, owned child identity, cwd, lifecycle state, loss reason, and separately captured integration resume argv plus its process provenance. Screen/scrollback stays in memory by default. Metadata cannot itself prove a child dead. Reject unsupported/corrupt schema visibly; do not delete it or create a duplicate holder automatically.

### `attach`

No-argument default-shell invocation requires a validated Herdr environment. It looks up the private binding by server/session identity and public pane ID, then attaches or creates one holder. `herdr-shield attach <holder-id>` attaches an existing holder only; it never creates a replacement implicitly. Recursive same-holder attachment is a no-op/error with a clear message, never another holder around itself. A successful attach submits current upstream socket/pane identity and receives a generation-bound lease. On disconnect it relinquishes input, not ownership of the child. Attach uses raw outer-terminal I/O, forwards input and resize, renders snapshots/diffs, and restores terminal settings on every exit path. It must remain in a recovery UI rather than silently turning missing-holder attachment into a fresh agent.

### `herdr`

One typed protocol module owns the pinned Herdr v22 schema, upstream connections, request/result/event mapping, native integration capture and reconciliation. A generation-bound map relates birth aliases to current public pane/tab/workspace identities. New result identities remain canonical current identities so child callers can learn created/moved targets; map known old aliases on input, never globally rewrite all strings. If an alias becomes ambiguous due to ID reuse, fail explicitly. Track pane moves atomically using old/new IDs and owned attachment identity. Internal `TerminalId` is observational only.

Reconcile after startup, restore, handoff and binding changes. Prove enumeration ran successfully and was parsed; distinguish ERROR, empty result and positive observations. Enumerate live holder ownership and Herdr panes; classify protected/unprotected/orphan. Absence or empty results are not an automatic destruction signal. Only an authenticated, generation-matching explicit-close operation/event with sufficient provenance can authorize automatic teardown. If Herdr's event cannot distinguish an ambiguous disappearance, retain and explain it; do not invent certainty.

### `lifetime`

Encapsulate user-service launch, cgroup verification and owned-process signalling. No shell-constructed commands, automatic linger, broad process-name kill, blanket `pkill`, or signals to parent/live server. A holder can outlive an outer Herdr process while still being subject to user-manager or machine shutdown. The service name contains only the plugin name and random holder ID. A normal session startup never rewrites system configuration. Dependency/process-exec/time/randomness/filesystem paths each have one seam with unit fakes, plus real-process tests.

## Holder protocol

Separate `holder.sock` (private framed protocol) from `herdr-api.sock` (NDJSON integration relay). Never multiplex by guessing arbitrary payloads. Verify same-UID Unix peer credentials before parsing, then authenticate a random per-holder capability delivered only through private metadata/owned channels. Same-user checks are not a sandbox against a malicious process with that user's credentials.

Wire v1: eight-byte ASCII magic `HDRSHLD1`, u32 big-endian frame length, UTF-8 JSON envelope; maximum frame 1 MiB, bounded nesting and per-connection queues. Envelope fields are `major`, `minor`, `request_id`, `type`, `holder_id`, `generation`, `payload`. Reject duplicate fields, invalid IDs, invalid lengths and unsupported major before mutation. Negotiate supported major/minor/features through `hello`/`welcome`; major mismatch returns `incompatible_protocol` and closes only that connection. Minor extensions require feature negotiation; unknown required fields/types fail. Input/output bytes use base64 only in bounded data frames, not JSON strings with lossy decoding. Use chunked snapshots with sequence and final checksum; a reconnect cannot mix generations.

| Message | Direction | Contract |
| --- | --- | --- |
| `hello` / `welcome` | Client ↔ holder | Versions/features, identity/capability; welcome reports holder/child state, never secrets |
| `spawn` / `spawned` | Bootstrap client ↔ holder | Authenticated once-only cwd/argv/env transfer; only empty verified holder may create workload |
| `bind` / `bound` | Attach ↔ holder | Validated upstream endpoint and current public IDs; generation changes only after acknowledged rebind |
| `acquire` / `lease` | Attach ↔ holder | One writable attachment, opaque lease; explicit takeover confirmation if occupied |
| `input` | Attach → holder | Lease + generation + monotonically increasing sequence; reject stale/replayed input |
| `resize` | Attach → holder | Lease + generation + rows/cols/pixel bounds; resize PTY and model together |
| `snapshot` / `render` | Holder → attach | Chunked model snapshot then ordered state diffs; gap requests full redraw |
| `ack` / `resync` | Attach → holder | Bounded backpressure; slow/disconnected attachment cannot block PTY draining |
| `status` / `status_result` | Local client ↔ holder | Owned identities, versions, binding/orphan/exit reason; no terminal content by default |
| `detach` | Attach → holder | Releases lease only; never kills child |
| `terminate` / `terminated` | Confirmed local client ↔ holder | Capability + exact live identity, explicit confirmation; graceful owned shutdown then bounded escalation |
| `error` | Either direction | Stable code, request ID, safe contextual message; never success-shaped failure |

Error codes include `unauthorized`, `invalid_frame`, `frame_too_large`, `incompatible_protocol`, `unknown_message`, `invalid_binding`, `stale_generation`, `lease_held`, `stale_input`, `holder_missing`, `child_identity_unverifiable`, `upstream_unavailable`, `unsupported_api`, `orphan_retained`. Invalid frames never become commands; peer-supplied paths never become filesystem operations. On holder restart after actual death, do not reuse a live holder's ID/capability or assume a child can be adopted.

## Herdr API relay

Relay source of truth is the 102-variant `Method` enum, schemas and response/event enums at the pinned commit. Prefer vendored typed schema with upstream license/commit attribution over untyped recursive rewriting. A generated census test proves every variant has exactly one policy and every nested identity field has a mapping case. Unknown variants/extensions fail closed with a visible typed error. This is a compatibility proxy, not an expansion of native authority.

### Request rewrite table

`P`, `T`, `W` mean map known birth/previous pane, tab and workspace aliases to current public IDs. Omitted optionals preserve upstream focus/default semantics, not an invented “this holder” target. Preserve request IDs, `source`, `seq`, agent/session IDs, argv, text, paths, tokens and correlation values. Fields not listed are opaque. `panes.rs` etc below mean `S/src/api/schema/<file>`.

| Method(s) | Structured identities | Source |
| --- | --- | --- |
| `workspace.create` | optional `source_workspace_id` W; env opaque | `workspaces.rs:8–20` |
| `workspace.get/focus/rename/move/report_metadata/close` | `workspace_id` W | `common.rs:29–31`; `workspaces.rs:23–59` |
| `workspace.move_block` | each `workspace_ids[]` W, optional `before_workspace_id` W | `workspaces.rs:42–46` |
| `tab.create/list` | optional `workspace_id` W; env opaque | `tabs.rs:8–25` |
| `tab.get/focus/rename/move/close` | `tab_id` T | `common.rs:50–52`; `tabs.rs:28–37` |
| `worktree.list/create/open` | optional `workspace_id` W; cwd/path/branch opaque | `worktrees.rs:4–49` |
| `worktree.remove` | `workspace_id` W | `worktrees.rs:52–58` |
| `pane.get/focus/clear/edit_scrollback/close` | `pane_id` P | `common.rs:34–36` |
| `pane.input.set`, `pane.link.activate/resolve` | `pane_id` P | `panes.rs:38–51` |
| `pane.split` | optional `target_pane_id` P, `workspace_id` W; env opaque | `panes.rs:19–34` |
| `pane.swap` | optional `pane_id`, `source_pane_id`, `target_pane_id` P | `panes.rs:64–73` |
| `pane.move` | `pane_id` P; Tab destination `tab_id` T and optional `target_pane_id` P; NewTab optional W | `panes.rs:76–105,542–561` |
| `pane.zoom/layout/process_info` | optional `pane_id` P | `panes.rs:109–136` |
| `layout.export/set_split_ratio` | optional P/T; split-tree `path` opaque | `panes.rs:140–167` |
| `layout.apply` | W/T; recursive root `first`/`second` leaf `pane_id` P; argv opaque | `panes.rs:148–157,180–205` |
| `pane.neighbor/edges/focus_direction/resize` | optional `pane_id` P | `panes.rs:209–234` |
| `pane.scroll/selection.read/copy_motion/copy_search` | `pane_id` P | `panes.rs:238–303` |
| `pane.list`, `pane.current` | optional W; optional `caller_pane_id` P respectively | `panes.rs:307–316` |
| `pane.rename/send_text/send_keys/send_input/read` | `pane_id` P | `panes.rs:319–355` |
| `pane.report_agent/report_agent_session/report_metadata/clear_agent_authority/release_agent` | `pane_id` P; first two capture optional `resume_argv` separately | `panes.rs:362–445` |
| `pane.wait_for_output` | `pane_id` P | `events.rs:95–104` |
| `command.invoke` | optional P/W/T and `selection.pane_id` P | `commands.rs:4–15` |
| `agent.start` | `pane_id` P; preserve real native busy/error outcome | `agents.rs:167–175` |
| `agent.get/explain/focus/read/send_keys/rename/prompt/wait` | typed `target` pane selector vs name; never blindly replace name strings | `common.rs:55–57`; `agents.rs:8–49,179–183`; `S/src/app/terminal_targets.rs:33–103` |
| `agent.view.set` | recurse All/Any/Not/Eq/In/Exists; builtin pane/tab/workspace fields map string `value`/`values[]`; user tokens opaque | `agents.rs:53–126` |
| `events.subscribe` | only pane-output/status/scroll subscription selectors contain P | `events.rs:18–84` |
| `events.wait` | match_event pane variants P, PaneCreated optional P/W, workspace/tab variants W/T | `events.rs:116–190` |
| `plugin.action.invoke` | context `focused_pane_id` P, W/T | `plugins.rs:306–312,363–395` |
| `plugin.pane.open` | optional `target_pane_id` P, W; env opaque | `plugins.rs:418–438` |
| `plugin.pane.focus/close` | `pane_id` P | `plugins.rs:455–462` |

### Closed method-policy inventory

The groups below are disjoint and together must equal all 102 methods; aliases separated by `/` share a prefix, not a new invented method. The builder test expands them and compares against the actual enum. `ALLOW` still applies rewrite/error/version rules. `STREAM` requires a dedicated upstream connection and cancellation/lifetime handling, not one-response forwarding. `REFUSE` is explicit plugin policy, not an assertion that Herdr lacks the method.

| Policy | Exact methods |
| --- | --- |
| ALLOW server reads | `ping`, `server.agent_manifests`, `session.snapshot` |
| REFUSE server/admin | `server.stop/live_handoff/reload_config/ssh_agent.register/reload_agent_manifests`, `client_shell.surface.set` |
| ALLOW notifications/window | `notification.show`, `product_announcement.dismiss`, `release_notes.dismiss`, `client.window_title.set/clear`, `popup.close` |
| ALLOW workspace | `workspace.create/list/get/focus/rename/move/move_block/report_metadata/close` |
| ALLOW tab | `tab.create/list/get/focus/rename/move/close` |
| ALLOW pane | `pane.get/focus/input.set/link.activate/link.resolve/clear/edit_scrollback/close/split/swap/move/zoom/layout/neighbor/edges/focus_direction/resize/process_info/scroll/selection.read/copy_motion/copy_search/list/current/rename/send_text/send_keys/send_input/read/report_agent/report_agent_session/report_metadata/clear_agent_authority/release_agent` |
| ALLOW layout | `layout.export/apply/set_split_ratio` |
| STREAM | `pane.wait_for_output`, `events.subscribe`, `events.wait`, `agent.prompt`, `agent.wait` |
| ALLOW commands | `command.invoke` |
| ALLOW agent | `agent.list/get/focus/read/send_keys/start/rename`, `agent.view.set/clear` |
| REFUSE untyped | `agent.explain` |
| ALLOW worktree | `worktree.list/create/open/remove` |
| ALLOW integration/plugin reads | `integration.list`, `plugin.list`, `plugin.log.list` |
| REFUSE integration/plugin writes | `integration.install/uninstall`, `plugin.link/unlink/enable/disable` |
| ALLOW plugin interaction | `plugin.action.list/invoke`, `plugin.pane.open/focus/close` |

Admin refusals prevent a child relay from restarting/configuring the outer server or installing plugins; ordinary allowed mutations still carry native user-level API power, including explicit pane close. `server.ssh_agent.register` is refused rather than emulating connection-scoped state; closing its upstream connection drops registration (`S/src/api/server.rs:17–20`). `client_shell.surface.set` natively errors `connection_local_only` on JSON (`server.rs:574–579`). `agent.explain` returns untyped `serde_json::Value` (`response.rs:184–186`) and is unsupported, not an unchecked transparent forward. Native `agent.start` may be busy because Herdr sees attach, not the inner shell; expose that limitation. `layout.apply`, plugin panes and invoked commands may create unprotected panes; successful mutation responses must report/reconcile that fact, never imply protected creation.

### Response and event mapping

Read and type-check `S/src/api/schema/response.rs:44–280`. Map explicit typed identities, not arbitrary keys: `SessionSnapshot` focused P and nested panes/layouts/agents/W/T (`session.rs:9–22`); `PaneInfo` P/TerminalId/W/T (`panes.rs:449–483`); `AgentInfo` P/TerminalId/W/T (`agents.rs:187–228`); `PaneProcessInfo.pane_id` (`panes.rs:494–503`); swaps (`521–529`); moves with previous/new/created/closed/focused IDs and nested layouts (`542–561`); zoom/layout/neighbors/edges/focus/resize/read (`572–688`); recursive `LayoutDescription`/`LayoutPane` (`170–205`); selection/copy responses (`response.rs:165–182`); matched-output nested read (`190–195`); plugin `PluginPaneInfo.pane`, action/link context (`plugins.rs:363–395,465–469`). Also map `WorkspaceInfo.workspace_id/active_tab_id`, `TabInfo.tab_id/workspace_id`, `WorktreeSourceInfo.source_workspace_id`, `WorktreeInfo.open_workspace_id`, and workspace/tab creation, close and move results (`workspaces.rs:62–85`, `tabs.rs:40–48`, `worktrees.rs:61–84`, `response.rs:44–280`). Process info remains an honest **outer** process view unless an explicitly separate shield view is requested.

For `EventData` (`events.rs:422–557`) handle nested PaneInfo in create/update, P in close/focus/exit/output/agent events, old/new/related P in move, nested layout updates, and all workspace/tab old/new IDs. Subscription output/status/scroll payloads contain P and nested reads (`events.rs:385–418`). Bind stream responses to the generation used at request start; a rebind either completes under that mapping or returns an explicit interrupted-generation error and asks the caller to resubscribe. Never silently deliver another pane's events after identity reuse. Keep result IDs canonical current; do not invent a half-virtual inverse namespace.

The complete binary client/server message enums were also enumerated in `L/042-exhaustive-protocol.log`; **all** binary transport is refused. Identity-bearing binary doors include clipboard-image Pane target, attach/observe/control terminal, client-shell pane/popup input, nested endpoint requests/data; outputs include session/layout/delta/observe, active/created terminal, observers, clipboard images, popup sessions, client-shell scopes/updates, owned terminals, terminal dumps, debug/endpoint payloads. No selected binary variant is accidentally admitted by the JSON proxy.

## Terminal contract

The holder is the inner terminal authority. Use `portable-pty` master read/write/resize and owned child wait/kill (`D/portable-pty/source/portable-pty-0.9.0/src/lib.rs:88–123,130–157`); `alacritty_terminal` grid/mode/cursor model, `PtyWrite`, `ColorRequest`, `TextAreaSizeRequest` and explicit clipboard events. Translate logical model changes into bounded attach render frames. The attach renderer targets Herdr's outer VT; it is a renderer, not an assertion that the library already serializes a full screen.

The test corpus must cover CR/LF/backspace, scroll regions, erase/insert/delete, SGR palette/truecolor/default colors, indexed color changes, cursor shape/visibility/save/restore, origin/wrap/application-cursor/keypad modes, alternate screen, bracketed paste, mouse protocols/focus reporting, Unicode combining/wide cells, tabs, resize/reflow, scrollback boundaries, hyperlinks, OSC 7 cwd and OSC 133 prompt/command boundaries. Inner input must obey active modes after reconnect. Theme comes from current outer attachment through validated explicit color messages; while detached answer from the last/default palette and report the policy. Replies must be counted, including queries split across arbitrary byte boundaries. Do not pass raw DA/DSR/OSC queries through and answer them again.

Forward only an audited metadata sideband for Herdr-relevant cwd/prompt/title state, with bounded parsed values and correct ordering. Do not replay historic prompt markers as live commands during redraw. Clipboard reads are never forwarded. A clipboard write (valid base64, at most 64 KiB) made while a client is attached reaches Herdr, whose own policy applies, exactly as in an unwrapped pane; a write made while detached is never replayed on reattach, so detached output cannot exfiltrate to the outer clipboard. Unknown/private OSC/DCS sequences are safely ignored with a visible capability limitation, not blindly executed. Define the supported-sequence matrix in tests/README; fail the release claim if a required mode cannot be rendered. A terminal-tail demo is not evidence for these requirements.

## Failures and recovery

| Event | Required action | Visible effect / limit |
| --- | --- | --- |
| Outer server SIGKILL/crash | Retain holder/PTY/child; reconnect on restored default-shell pane | Same PID/counter; detached state shown until reattach |
| Graceful server stop | Treat transport loss as detach, not explicit pane destruction | Holder retained; no claim every server-stop path preserves outer PTY |
| Live handoff | Keep holder, rebind upstream generation once, reconcile hooks idempotently | No duplicate child; combination test required |
| Trusted intentional pane close | Terminate only positively matched holder using owned handle | Exit reason `intentional-pane-close`; ambiguous event retains orphan |
| Socket EOF/SIGHUP, failed or empty enumeration | Retain holder; never infer intent/death | `ORPHAN`, reason, exact cleanup command |
| Inner shell/agent exit | Observe owned status; keep final screen/status until explicit cleanup policy | Truthful exit code, no automatic agent restart |
| Holder crash | Cannot promise inner survival; independently inspect captured process identity | Recovery UI; no speculative auto-resume |
| Holder missing, process live or unverifiable | Do not offer Enter-to-resume | Explain risk; any-key plain-shell path does not start another agent |
| Holder missing, process independently proved gone | Show exact captured argv and explicit Enter choice | Enter executes stored argv; any other key plain shell |
| Protocol/binary upgrade mismatch | Retain old holder, reject unsupported client, explain compatible-client path | No forced restart; explicit drain/kill only |
| User-manager unavailable/stops; logout | On launch, print refusal and run plain inner shell marked `UNPROTECTED`; never use `setsid` | Existing holders may be lost if user manager stops; no automatic linger |
| Reboot | Process/PTY continuity ends | Private metadata may explain loss; resume remains guarded and user-confirmed |
| Invalid/corrupt registry, ID reuse | Refuse ambiguous binding; retain live state | Actionable error, never kill by stale PID or replace silently |

For recovery, store argv as an exact array of OS-compatible arguments. Reject NUL/unrepresentable native resume inputs at capture; do not silently normalize whitespace, quoting, empty args or controls. A safe escaped display must round-trip to the exact array, and execution must use the stored array directly. Herdr resume constraints are separate: ≤64 args, ≤8 KiB, first word a plain command on PATH, no apostrophes. If a captured integration request violates them, preserve the real upstream rejection and do not call it an accepted fallback. Prove process death from owned identity/handle or trustworthy boot/start identity, not a missing socket, absent metadata, or reused PID. Unknown remote/session provenance cannot satisfy the guard. Showing argv occurs only in the private recovery UI, never general logs.

## Build layout and verification

```text
Cargo.toml                 Cargo.lock                rust-toolchain.toml
herdr-plugin.toml          README.md                 CLAUDE.md
src/main.rs                src/config.rs
src/holder/{mod.rs,protocol.rs,state.rs,registry.rs}
src/attach/{mod.rs,recovery.rs}
src/terminal/{mod.rs,render.rs,queries.rs}
src/herdr/{mod.rs,schema.rs,relay.rs,reconcile.rs}
src/lifetime/{mod.rs,systemd.rs,identity.rs}
tests/{holder,attach,terminal,herdr,lifetime}/
tests/fakes/               tests/jail.rs
scripts/checks.sh          scripts/test-herdr.sh
docs/spec.md
```

Prefer in-file unit tests or the mirrored module directory, not a catch-all suite. Keep a concept in its owning directory; no `utils`, duplicate schema registries, or parallel help/command tables. A single command registry drives help/action wiring where possible. The future `CLAUDE.md` vocabulary maps canonical names to these homes. No source file >800 lines, no doc topic >500 lines/80 KiB. `scripts/checks.sh` verifies size, test mirrors, forbidden-origin strings/absolute machine paths, command-registry parity, method-policy completeness, and path/process-exec façade bypasses; exact enumeration failures report ERROR, not PASS. Establish known-broken controls for each check before trusting it. Metrics without a baseline are UNMEASURED, not invented results.

Required unit tests: holder frame codec/version/limits/auth/lease generations; exact argv display roundtrip; process identity guard including PID reuse; registry atomicity/corruption; all 102 API policies and nested rewrites/refusals; sequence/native resume capture; successful-empty vs failed enumeration; terminal query/mode/render corpus; config recursion and ownership checks. Property/fuzz tests target framed input and terminal byte chunking within bounded resources. Fakes implement the same typed seam contract as real path/process/time/socket backends.

Required hermetic process tests boot the built binary, not imported internals: owned PTY shell with monotonic in-memory counter, detach/output flood/reconnect/resize, same child after attach-client kill, holder kill recovery UI, competing/stale lease, explicit close, old/new wire, unsupported backend, path/symlink/peer attacks, and private log/content checks. Each assertion has an independent oracle (OS identity, separately read cells, scripted protocol transcript), never the holder's own success label. Test terminal expected values come from fixed protocol cases/reference terminal behavior, not the production formatter. Mutation controls must demonstrate that removing the relevant guard makes its test fail.

Required pinned real-Herdr v1 tests use dummy agents only: default-shell kill9 outer server with same child identity/counter after restore; attach before/after restore timing; authority/resume rejection and partial mutation; intentional pane-close vs crash; startup twice with enumeration failure/empty result; same-binary handoff combined with holder; native `agent.start` busy limitation; changed internal TerminalId; changed pane ID via move; enumerate every census door and assert non-default-shell doors are visibly `UNPROTECTED` with reasons in list and coverage report; unsupported platform/user-manager/cgroup paths visibly refuse and open a plain shell. The explicit-command survival test and universal protection for all census doors are v2 (acceptance rows 1 and 4). Download/source identity must be fixed to the commit above. Cross-version update and actual installer handoff remain separate release tests, not a substituted same-binary claim.

All test resources live in an explicit throwaway root with isolated HOME and XDG_CONFIG_HOME/XDG_STATE_HOME/XDG_DATA_HOME/XDG_CACHE_HOME/XDG_RUNTIME_DIR. The jail validates the expected socket **before every Herdr invocation**, refuses any outside root, allocates its own PTY client, and records owned PID/start identities. Never use real agents, credentials, config, installed servers or broad process kills. Killing server PID is allowed only when that exact owned probe instance was recorded. An independent cleanup census asserts zero remaining test processes/services/sockets; every process fault test cleans in `finally`/drop even on assertion failure. Unit/hermetic gates require no network; real-Herdr tests use no external service.

Builder gates, run inside that project's isolated container/test jail with no host config: `cargo fmt --all -- --check`; `cargo clippy --all-targets --all-features -- -D warnings`; `cargo test --all-targets --all-features`; `bash scripts/checks.sh`; `bash scripts/test-herdr.sh --herdr <pinned-binary>`. The real-Herdr v1 suite covers only the authorized default-shell protection boundary and verifies visible `UNPROTECTED` status for other census doors; v2 rows are not v1 gates. Pin toolchain and dependency lockfile; check formatting idempotence. Missing tool, no tests discovered, refused backend, timeout or skipped suite is ERROR/named gap, never green. Record commands, exit status, discovered/executed test counts, assertion failures, cleanup census and artifact hashes. Do not run the unrelated parent project's full test gate for this standalone project.

## Install and orientation deliverables

README must explain the survival boundary and UNPROTECTED doors before installation. Document Linux plus verified systemd-user requirements, the refusal/plain-shell fallback for unsupported platform or backend, and that such panes remain UNPROTECTED. Document a build from a release tarball/source tree, user-selected executable destination, normal plugin link/install, explicit user edits to Herdr configuration, and rollback without killing holders. No self-installation or host config mutation by ordinary plugin startup.

Required manual configuration: `[terminal] default_shell = "<absolute installed herdr-shield executable>"`, plus `shell_mode = "non_login"` (`S/src/config/model.rs:243–249`) for this Linux wrapper; `[session] resume_agents_on_restore = false`. Shield config records the actual inner shell path/argv. Disclose session-wide native-resume disablement and require fresh/default-shell panes; existing panes stay unprotected. Verify backend cgroup escape and status before claiming protection. Do not ship machine-absolute paths in example values: use explanatory placeholders. Plugin registration cannot itself set default_shell. Preserve running holders on uninstall/upgrade; list and explicitly drain/kill them separately.

The CLI contract is `herdr-shield` (default-shell attach/create), `attach <holder-id>`, `reconcile`, `list [--json]`, `kill <holder-id>` (interactive explicit confirmation), and `doctor`. `list` includes protected panes, unprotected panes, holders, versions, child identity/state, orphan reason and cleanup command; a failed pane enumeration has an explicit ERROR section/JSON error, not an empty list. `doctor` validates dependencies, user manager, config/native-resume prerequisite, path security and protocol compatibility without changing config. A noninteractive kill path, if implemented, must require an explicit confirmation flag and exact holder ID/capability checks, never infer consent from piped input.

Exact manifest schema: top-level `id`, `name`, `version`, `min_herdr_version`, optional `description`, `platforms`; arrays `build`, `startup`, `actions`, `events`, `panes`, `link_handlers`. Startup/build need `command` and optional `platforms`; action needs `id`, `title`, `command`, optional `description/contexts/platforms`; event needs `on`, `command`, optional `platforms`. Pane and link-handler fields are not used. No configuration-mutation field exists (`S/src/app/api/plugins/manifest.rs:12–96`). Use this concrete development manifest; it supports the v1 default-shell route and does not provide v2 universal pre-spawn coverage:

```toml
id = "herdr-shield"
name = "Herdr Shield"
version = "0.1.0"
min_herdr_version = "0.9.5"
platforms = ["linux"]

[[startup]]
command = ["bin/herdr-shield", "reconcile"]

[[events]]
on = "pane.closed"
command = ["bin/herdr-shield", "event"]

[[events]]
on = "pane.moved"
command = ["bin/herdr-shield", "event"]

[[events]]
on = "tab.closed"
command = ["bin/herdr-shield", "event"]

[[events]]
on = "workspace.closed"
command = ["bin/herdr-shield", "event"]

[[actions]]
id = "list"
title = "List protected panes and holders"
contexts = ["global", "pane"]
command = ["bin/herdr-shield", "action-list"]
```

`event` parses bounded `HERDR_PLUGIN_EVENT_JSON`, checks actual binding/observer provenance, and reconciles; an unauthenticated hint never authorizes destruction. `action-list` logs the same structured status as CLI list and issues a short `notification.show` with counts/errors; full details remain in `herdr plugin log list --plugin herdr-shield` (`S/src/cli/plugin.rs:1821`). Actions have piped output and no owned interactive PTY (`S/src/app/api/plugins/runtime.rs:124–129`), so **no destructive manifest kill action**: use `herdr-shield kill <exact-id>` from a real terminal with confirmation. Startup is one-shot, not a supervised daemon (`runtime.rs:161–201`). Persist reconciliation errors privately and expose them in list/doctor, not only hook stderr. Relative command paths resolve in plugin root (`S/src/plugin_command.rs:4–26`); builder copies the release binary to `bin/herdr-shield` before linking. Source install sequence: `cargo build --release --locked`; copy binary; `herdr plugin link <plugin-root>`; inspect `herdr plugin list`; make explicit configuration edits; deliberately replace old workloads. None of these live-install steps ran in research. Pin published install instructions to the eventual supported upstream version, not a fabricated universal v0.9.5 guarantee.

Builder-written `CLAUDE.md` states: standalone scope and supported platform/version; true protection boundary; canonical vocabulary/module map; owned-process and path-jail safety; no kill on ambiguous loss; no speculative auto-resume; single terminal query owner; relay v22 closed schema/refusals; privacy/no credential copying; exact build/test commands and no skipped-as-pass; all processes cleaned; no publication/install/config changes without explicit authorization. It contains no inherited framework policies, role names, or machine paths. This specification does not create that orientation file.

## Upstream change draft

**Draft only; not filed. V2 request.** Ask upstream for one pre-spawn command-wrapper facility covering every fresh PTY builder, plus explicit restore/native-resume interaction. The smallest central seam found is `S/src/pane.rs:2687` before backend `spawn_command`, reached from normal/history/custom-shell/argv factories; `S/src/terminal/runtime.rs:63` imported-FD handoff is excluded from fresh wrapping. The hook must receive original executable/argv/cwd/env, a durable random pane-instance token persisted with the snapshot, and profile/session incarnation plus public pane identity; public IDs or socket paths alone are insufficient after session deletion/recreation. It must allow wrapper executable plus lossless argv handoff without shell re-parsing. Cold restore must deliver that same instance token and original launch specification to the wrapper, rather than creating a new workload or injecting a native resume into the preserved one. Assign a new token on actual pane creation; mismatched or missing tokens fail binding safely. It must apply to layout commands, plugin placements, popups, custom commands, replacement and cold restore as well as normal shells. A hook only on default-shell creation still leaves the proven bypass. V1 does not depend on this facility and exposes uncovered doors as `UNPROTECTED`.

Request a trusted intentional-close lifecycle signal with instance/generation provenance, a supported holder-aware native agent/process/resume policy, and clear behavior for imported PTYs and existing processes. Existing live PTYs need an explicit unsupported/migration boundary even after a new hook. Closed-census tests above become upstream-hook acceptance: every fresh spawn runs the holder, intentional close terminates the correct holder, native resume never duplicates the preserved child, handoff never wraps imported FDs again. No universal guarantee until those changes ship and tests pass.

The upstream facility is required only for v2 universal coverage. V1 is scoped to Linux with a verified systemd user manager. No publication, upstream filing or patch distribution is authorized by this spec. Unknown future Herdr APIs remain a v2 integration dependency, not a v1 build blocker.

## Research limits and cleanup

Observed feasibility is not an implemented plugin. Unrun: production holder/relay/VT renderer, all production unit/integration gates, combined holder + handoff, cross-version handoff, actual update/install, logout/reboot behavior, macOS/non-systemd backend, complete terminal compatibility. Source/API metadata supports the design but does not prove Rust dependency compilation.

Recorded probe errors, with corrected results kept separate:

- `L/018-baseline-arbitration.log`: `ConnectionRefusedError: [Errno 111] Connection refused`; stale socket file was not readiness. Corrected guarded-connect run: log 021.
- `L/026-idle-events.log`: `TimeoutError: condition not met in 20s`; idle clearing prevented resume persistence, diagnosed in log 027 and observed again in log 029.
- `L/028-events-verified.log`: ``invalid request: unknown variant `horizontal`, expected `right` or `down` at line 1 column 105``, then `IndexError: list index out of range`; corrected split uses right. Separate live-close proof is log 032.
- `L/023-holder-feasibility.log`: ``invalid request: unknown variant `screen`, expected one of `visible`, `recent`, `recent_unwrapped`, `detection` at line 1 column 108``; screen-read/redraw in that holder run remains UNVERIFIED.
- `L/029-events-restored.log`: `pane_not_found`, `pane w1:p2 not found` after natural exit; not counted as explicit-close success.

Historical transient-wrapper PID ledger is incomplete. All recorded primary identities were gone; the scratch-HOME process census found no residual probe processes. No source miss or failed request was treated as evidence of feature absence.

All 39 recorded primary server/client/holder/dummy/service PIDs were gone at `L/049-final-cleanup.log`; its final `CENSUS_MATCH_COUNT 0` is the cleanup oracle. No live server, real agent, credentials, installed configuration, or host installation was used. The final return carries the exact recorded PID list and document-check results. Research commands, outputs, downloaded archives and prototypes remain only in the research run's private evidence directories; plugin permanent output is specification documentation only.
