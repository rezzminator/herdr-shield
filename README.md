# herdr shield

herdr shield v1 keeps the same shell process and its private PTY alive when the
outer Herdr server crashes. It protects new panes started through
`terminal.default_shell`, using a verified Linux systemd user service.

Explicit layout commands, plugin panes, custom commands, popups, replacements
outside the default-shell route, and panes already running at installation are
**UNPROTECTED**. `herdr-shield list --json` and the plugin list action include every
enumerated pane, its reason, the complete launch-door coverage table, holders,
and orphan cleanup commands. Unknown provenance is UNPROTECTED. Failed pane
enumeration is ERROR, not an empty successful list.

This tree is an implementation under verification against [the spec](docs/spec.md).
Do not treat unrun acceptance rows as a compatibility guarantee. The pinned
integration target is Herdr 0.9.5, protocol 22, commit
`2ecf19a92fc3a7fff75b6ce3655ef04f4625ec01`.

## Requirements and limits

Linux, an available systemd user manager, and positive verification that each
holder's cgroup belongs to its transient service outside any login session
scope are required. Failed launch or verification prints UNPROTECTED and runs
the explicitly configured plain shell. There is no detachment-backend fallback.
Logout, user-manager shutdown and reboot can still end workloads. herdr shield
does not enable linger or change system services.

Native agent resume must be disabled **for the entire Herdr session**, including
unprotected panes. Otherwise Herdr can inject a duplicate command into an
already reattached living shell. Native agent discovery sees the outer attach
process, not the inner process; native `agent.start` busy/errors remain visible.

No automatic recovery command executes after holder loss. The private recovery
screen shows the holder ID, known reason and exact captured argv as a JSON
array. Enter is offered only with independent process-death proof; any other
key selects a plain shell. Reporting helper PIDs do not establish the agent's
identity, so their reports alone never enable resume. Capture also requires a
changed persisted upstream resume slot and a fresh report epoch. The foreground
process root must be validated through ancestry, cgroup, boot/start identity and
executable provenance. Missing or untrusted snapshot or provenance evidence
suppresses fallback.

EOF, SIGHUP, hook hints and failed/empty enumeration retain holders. Only a
completed authenticated, generation-matching explicit close observed through
the holder relay or its authenticated live observer can authorize automatic
termination. A close without sufficient provenance leaves an orphan for explicit
cleanup. Private credentials prevent accidental cross-binding, but do not
sandbox against another malicious process running as the same user.

## Build and manual installation

From a release tarball or source tree with Rust 1.99.0:

```sh
cargo build --release --locked
mkdir -p bin
cp target/release/herdr-shield bin/herdr-shield
```

Choose a permanent, versioned executable destination under your own install
root. Keep old compatible executable copies while holders reference them.
Point a stable executable path at your selected version only after checking its
wire compatibility. Never kill/recreate living holders merely for an upgrade.
An incompatible major reports the exact compatible-client path.

Create a private configuration directory `herdr-shield` below `XDG_CONFIG_HOME`
(default: your home configuration directory), mode 0700. Create `config.json`,
mode 0600, with your explicitly chosen real shell and arguments:

```json
{"shell":"<absolute executable path of your real shell>","args":[]}
```

The shell must be executable and cannot be this binary, a symlink to it or a
recursive attach. Inner login/startup behavior is entirely your chosen argv;
Herdr's outer shell mode does not decide it. Ordinary user environment is
transferred privately in memory; the holder replaces Herdr binding variables
with its private API relay and birth identities. No environment snapshot is
written to metadata or logs.

Register the plugin explicitly:

```sh
herdr plugin link <absolute-plugin-root>
herdr plugin list
```

Make these explicit manual edits to your Herdr configuration:

```toml
[terminal]
default_shell = "<absolute installed herdr-shield executable>"
shell_mode = "non_login"

[session]
resume_agents_on_restore = false
```

Plugin registration cannot set these values. Restart/reload Herdr using its
documented procedure, deliberately end existing workloads before replacing
their panes, then inspect `herdr-shield doctor` and `herdr-shield list --json`.
Normal startup never installs itself or modifies host configuration.

## Commands and rollback

`herdr-shield` attaches or creates the default-shell pane's holder.
`attach <holder-id>` only attaches an existing holder. One attachment owns input;
a competing attachment is refused without taking over. `reconcile` is
idempotent and never treats absence as intent. `list [--json]` prints full
coverage; the manifest action records that same output in the plugin log and
posts a short notification. Inspect it with
`herdr plugin log list --plugin herdr-shield`.

`kill <holder-id>` requires an interactive full-ID confirmation. Automated
cleanup must explicitly pass `--confirm <the-same-exact-id>`; piped input is not
consent. The private holder capability and exact child identity are checked
before signalling. There is no destructive manifest action.

To roll back, restore your original `terminal.default_shell`, remove the plugin
link through Herdr, and leave running holders and their compatible executable
copies in place. Attach them directly or explicitly drain/kill each exact ID.
Removing a plugin link does not adopt, resume or terminate their workloads.

## Terminal and API support

Alacritty owns the inner terminal model and all DA/DSR/color/size replies,
including while detached. The renderer emits bounded full-screen snapshots
with sequence/checksum verification; scrollback remains bounded and in memory.
PTY input/resize require the current lease, generation and increasing sequence.

| Feature | Implementation / verification boundary |
| --- | --- |
| CR/LF, erase/insert, Unicode/wide/combining, scrolling, alternate screen | Alacritty model; fixed cell/redraw corpus |
| SGR default/indexed/truecolor, cursor, app cursor/keypad, paste/mouse/focus modes | Controlled renderer and mode restore; real mouse hardware is unverified |
| DA/DSR, color, text size queries split over bytes | Exactly one inner responder; queries never replayed to Herdr |
| Clipboard OSC 52 | Disabled for reads and writes |
| Unknown/private OSC/DCS | Model ignores unsupported sequences; no raw forwarding |
| OSC 7 / OSC 133 / title sideband | Bounded, audited cwd, A/B/C/D command markers and title metadata; reconnect restores only the latest cwd/title, never historic commands |
| OSC 8 hyperlinks and advanced underline styles/colors | Rendered by the terminal model and covered by fixed protocol tests |
| Attachment color probes | Validated current-attachment replies merge with retained/default palette; policy is visible in status and list |

The API relay uses vendored typed protocol-22 schemas and one exhaustive policy
for all 102 methods. Server administration, plugin/integration installation,
binary transport and untyped `agent.explain` are refused explicitly. Native
errors pass through unchanged. Typed old identity aliases map on input; opaque
text, argv, paths, tokens and agent names are never globally rewritten.
Results remain canonical current identities. Binding changes interrupt streams
with a stale-generation error so callers can resubscribe.

Observed pane moves update the current binding. Binding history is capped at
256; the plugin explicitly refuses new history at the cap. Historical aliases
are refused when live/reused or when the census is unknown. Stock Herdr supplies
no durable pane-instance token across deleted and recreated sessions, so cold
automatic restore will not rebind: it retains the orphan, opens a plain shell,
and marks the replacement UNPROTECTED. `herdr-shield attach <id>` deliberately
selects and reconnects a living holder. A live handoff can rebind automatically
only while the original attachment PID/boot/start identity is live and Herdr's
shell_pid independently matches.

Streams, exhaustive input/security matrices, detached RSS measurement,
cross-version executable upgrades, and non-Linux/platform behavior remain
UNVERIFIED or UNRUN. This development tree is not release-certified.

## Verification

Run builds and tests with HOME, all XDG roots, CARGO_HOME, CARGO_TARGET_DIR and
TMPDIR inside an explicit throwaway test root. Set `HERDR_SHIELD_TEST_ROOT` to a
fresh subdirectory of TMPDIR for the black-box Rust tests. They require a real
systemd user manager and use only `herdr-shield-test-` services. Missing tools,
refused backends and skipped tests are errors, never passing suites.

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
bash scripts/checks.sh
HERDR_SHIELD_BUILD_ROOT=<absolute-private-root> \
HERDR_SHIELD_BIN=<absolute-built-herdr-shield> \
bash scripts/test-herdr.sh --herdr <pinned-v0.9.5-binary>
```

The real-Herdr jail validates its socket before every command, records exact
owned process identities, uses dummy agents, and stops/resets only its uniquely
named test services. Fault tests clean their resources even on failure. There
is no live-install test and no claim of logout/reboot or cross-version handoff
survival. Upstream schema attribution is in `LICENSE-HERDR`.
