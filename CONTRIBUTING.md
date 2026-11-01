# Contributing to herdr-shield

Thank you for helping. herdr-shield sits between Herdr and the processes people care about most: a mistaken close, binding or recovery decision can end a living workload or start a duplicate. A change is judged first by what it could kill, duplicate or expose.

## Before you start

- For anything larger than a small fix, open an issue first so the design can be agreed. Every promise lives in the numbered acceptance contract, [docs/spec.md](./docs/spec.md): a behaviour change lands there first, then in the code.
- Looking for a place to start? Try a [good first issue](https://github.com/rezzminator/herdr-shield/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22).
- Everyone taking part follows the [code of conduct](./CODE_OF_CONDUCT.md).

## Set up

- Linux on x86_64 with a running systemd user manager.
- [rustup](https://rustup.rs): `rust-toolchain.toml` pins the toolchain, and `Cargo.lock` pins every dependency.
- `curl` and `/usr/bin/python3`, for the gate and the real-Herdr tests.

```sh
git clone https://github.com/rezzminator/herdr-shield.git
cd herdr-shield
make check
```

`make check` downloads the Herdr release pinned in `pins.env`, verifies its SHA-256, and runs every check in one throwaway root under `/tmp`; on failure it keeps that root and prints its path.

## Branches

Work lands on `develop`, the default branch; open pull requests against it. `main` moves only by release: the release pull request from `develop` into `main` runs CI, and its merge tags and publishes the release.

## What a change owes

- `make check` passing: `cargo fmt`, `cargo clippy -D warnings`, the release build, `cargo test --all-targets --all-features`, the structural checks and the real-Herdr tests.
- A test for every behaviour change, at the lowest layer that sees it. A bug fix's test fails on the unfixed code first.
- [docs/spec.md](./docs/spec.md) updated in the same pull request when a promise changes.
- A line under `## [Unreleased]` in [CHANGELOG.md](./CHANGELOG.md) for anything a user would notice.

## Rules that never bend

- **Tests never touch a live Herdr.** Every test runs under its own `HOME` and XDG directories with an owned Herdr socket, validated before each command, and dummy workloads only.
- **Tests own what they start.** Test services are named `herdr-shield-test-…`; a test signals only processes whose PID, boot and start time it recorded, and cleans its services, processes and sockets even when it fails.
- **No kill on a guess.** EOF, SIGHUP, a missing socket and a failed or empty pane list never end a holder; only an authenticated, generation-matching close does.
- **No automatic recovery.** A command is rerun only on the user's Enter, after the original process is independently proven gone.
- **Nothing private leaves the machine or the user.** No credentials or full environments in state or logs, and no machine-absolute path, username or email address in any file.

## Commits

Commit subjects follow `type(scope): summary`, for example `fix(attach): refuse a second attachment without taking over`.

## License

By contributing you agree that your contribution is licensed under the project's [MIT License](./LICENSE).
