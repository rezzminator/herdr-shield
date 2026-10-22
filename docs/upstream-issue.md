# Upstream issue draft

**Title:** Add a durable pre-spawn wrapper hook for all pane launches and
restores

**Body:**

## Motivation

Plugins currently have startup and event hooks, but no common hook that can
wrap every pane process before it is spawned. A plugin that wraps only
`terminal.default_shell` misses explicit-command launches: `layout.apply`
command leaves, plugin panes, custom-command panes, popups, and replacement
commands can start their own processes. Snapshot restore also needs to preserve
launch identity, and a live handoff imports an already-running PTY rather than
making a fresh spawn.

This leaves process-preserving plugins with a partial coverage boundary. The
`herdr-shield` v1 integration protects only fresh pane processes launched through
`terminal.default_shell`; it reports every other launch route as `UNPROTECTED`.
Universal pane coverage is a v2 goal and depends on an upstream lifecycle
facility.

## Requested facility

Add one opt-in, pre-spawn wrapper facility at the shared fresh-PTY
construction seam. At the pinned source revision, the pane factories converge
on `spawn_command_builder` and its call to `spawn_with_portable_pty` in
`src/pane.rs` (commit `2ecf19a92fc3a7fff75b6ce3655ef04f4625ec01`,
`src/pane.rs:2687-2701,2726-2727`). The facility should cover every fresh
process launch, including normal shells, history/custom-shell/argv factories,
layout command leaves, plugin placements, popups, custom commands, replacements,
and cold-restored panes.

The hook should give the wrapper the original executable, lossless argv, cwd,
and environment without converting them to a shell command. It should also
provide:

- a durable random pane-instance token, assigned on pane creation and
  persisted with the snapshot;
- the session incarnation and public pane identity needed to bind the wrapper
  after restore;
- an explicit failure result when the token is absent or mismatched, so the
  wrapper cannot silently bind to a reused public ID.

The wrapper must be able to launch with the original inputs intact. The hook
must not require plugins to rewrite global shell configuration to intercept
explicit command launches.

## Restore and lifecycle behavior

Cold restore should pass the same pane-instance token and original launch
specification back through the wrapper. It must not create a duplicate
workload or inject a native resume command into a child that the wrapper
already preserved. Assign a new token only when a pane is actually created; a
missing or mismatched token must fail binding visibly and safely.

Provide a trusted intentional-close signal that includes pane-instance and
generation provenance, so a process holder can distinguish a confirmed close
from a server crash, socket loss, or stale event. Define how the facility
treats live handoff and already-existing PTYs: handoff imports file descriptors
for an existing runtime through `src/terminal/runtime.rs:63-72` and
`src/server/handoff.rs:394-425` at the pinned commit, so it must not wrap those
descriptors as a new process. Existing PTYs need an explicit unsupported or
migration boundary; a new hook cannot retroactively adopt them.

Also define a supported holder-aware native process/agent detection and resume
policy. A wrapper can make the process lifetime independent of the server, but
native detection must not mistake the outer attach client for the inner shell
or resume a second workload into a preserved process.

## Acceptance criteria

- A closed launch-door test matrix proves the hook runs for every fresh pane
  spawn path, including explicit commands and cold restore.
- Wrapper input preserves executable, argv boundaries, cwd, environment,
  pane-instance token, and session incarnation without shell re-parsing.
- Restoring a pane preserves the same wrapped workload and does not launch a
  duplicate or inject a competing native resume.
- Intentional close is distinguishable from ambiguous transport/server loss
  and identifies the exact pane instance and generation.
- Live handoff preserves the existing PTY without invoking the fresh-spawn
  wrapper again.
- Existing live panes are explicitly identified as unsupported or have a
  separately specified, safe migration mechanism.
- Native agent/process detection and resume behavior have tests showing that a
  preserved workload is not duplicated.

This request is for universal v2 coverage. The default-shell-only v1
integration remains partial and continues to report uncovered panes as
`UNPROTECTED` until this facility is available.
