.PHONY: check release

# The whole local gate in one throwaway root: fmt, clippy, tests, structural checks, real Herdr.
check:
	bash scripts/gate.sh

# Cargo.toml's version, its CHANGELOG.md section and releases/v{version}.md agree; the tag is new.
release:
	bash scripts/release-check.sh
