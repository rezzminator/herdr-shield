## What and why

<!-- What this changes, and the issue it closes, if any. -->

## Checklist

- [ ] Targets `develop`
- [ ] `make check` passes
- [ ] A test for each behaviour change; a bug fix's test failed on the unfixed code first
- [ ] `docs/spec.md` updated when a promise changes
- [ ] A line under `## [Unreleased]` in `CHANGELOG.md`, when a user would notice
- [ ] Tests touch no live Herdr, and signal only processes and `herdr-shield-test-` services they started
- [ ] No credentials, environment dumps, machine-absolute paths, usernames or emails in any file
