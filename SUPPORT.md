# Getting help

1. **Read first.** The [README](./README.md) covers install, usage and what is not protected; [docs/spec.md](./docs/spec.md) is the numbered acceptance contract behind every behaviour.
2. **Ask herdr-shield.** `herdr-shield doctor` names the first prerequisite that fails; `herdr-shield list --json` shows every pane's PROTECTED or UNPROTECTED status with its reason, every holder, and each orphan's cleanup command.
3. **Open an issue.** Use the [issue templates](https://github.com/rezzminator/herdr-shield/issues/new/choose) for a bug or an idea. For a bug, include:
   - `herdr-shield --version` and `herdr --version`;
   - your Linux distribution and systemd version (`systemctl --version`);
   - the output of `herdr-shield doctor`;
   - the parts of `herdr-shield list --json` that show the problem.

Before you paste `list` output, read it: it holds socket paths, executable paths, process IDs and boot IDs. Remove anything you do not want public.

Security problems go through [SECURITY.md](./SECURITY.md), never a public issue.
