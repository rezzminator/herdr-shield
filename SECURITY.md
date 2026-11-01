# Security policy

## Supported versions

Only the latest release receives fixes.

## Reporting a vulnerability

Please do not open a public issue. Report it privately through GitHub: the repository's **Security** tab → **Report a vulnerability** ([open a report](https://github.com/rezzminator/herdr-shield/security/advisories/new)). The report and its fix are discussed in that private advisory, and a fix ships as a new release.

## What counts

- **Ending or duplicating a workload.** A way to make herdr-shield terminate a holder, or signal any process, without an authenticated close or a confirmed `kill` of that exact holder; or to make it run a recovery command without the user's Enter.
- **Crossing users or bindings.** A way for another user to reach a holder, its socket or its records, or for a pane to attach to, or drive input into, a holder it was not bound to.
- **The API relay.** A request that gets past the relay's method policy: a refused method that reaches Herdr, or an identity mapped onto the wrong pane, tab or workspace.
- **Private data.** Credentials, environment variables, recovery commands or terminal content written to disk, logs or another user's view, beyond what the [README](./README.md#how-it-works) states.

Out of scope: a hostile process already running as the same user. Private sockets and capabilities prevent accidental cross-binding; they are not a sandbox against your own user.
