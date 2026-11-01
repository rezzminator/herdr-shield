#!/usr/bin/env bash
# make release: Cargo.toml's version is the one version; herdr-plugin.toml, the CHANGELOG.md
# section and releases/v{version}.md agree with it, and tag v{version} does not exist yet.
# Prints the version alone on stdout, so publish.yml reads it through this same door.
set -euo pipefail
cd -- "$(dirname -- "$0")/.."
fail() {
    echo "ERROR: release: $*" >&2
    exit 1
}
version=$(sed -n 's/^version = "\([^"]*\)"$/\1/p' Cargo.toml | head -n 1)
[[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "Cargo.toml has no X.Y.Z package version (read: '$version')"
plugin=$(sed -n 's/^version = "\([^"]*\)"$/\1/p' herdr-plugin.toml)
[[ $plugin == "$version" ]] || fail "herdr-plugin.toml version '$plugin' differs from Cargo.toml version $version"
[[ -f CHANGELOG.md ]] || fail "CHANGELOG.md is missing; it needs a '## [$version]' section"
awk -v heading="## [$version]" 'index($0, heading) == 1 { found = 1; next } found && /^## / { exit } found && NF { body = 1 } END { exit !body }' CHANGELOG.md ||
    fail "CHANGELOG.md has no non-empty '## [$version]' section"
note="releases/v$version.md"
[[ -f $note ]] || fail "release note $note is missing"
head -n 1 "$note" | grep -q "^# v$version\b" || fail "$note does not start with '# v$version'"
[[ -n $(sed 1d "$note" | tr -d '[:space:]') ]] || fail "$note has no body below its heading"
tags=$(git tag --list "v$version") || fail "could not list local tags"
[[ -z $tags ]] || fail "tag v$version already exists locally"
if git remote get-url origin >/dev/null 2>&1; then
    rc=0
    git ls-remote --exit-code --tags origin "refs/tags/v$version" >/dev/null || rc=$?
    [[ $rc != 0 ]] || fail "tag v$version already exists on origin"
    [[ $rc == 2 ]] || fail "could not look up tag v$version on origin (git ls-remote exit $rc)"
fi
echo "release: v$version ready: Cargo.toml, herdr-plugin.toml, CHANGELOG.md and $note agree; the tag is new" >&2
echo "$version"
