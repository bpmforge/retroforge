#!/usr/bin/env bash
# Fetch + verify test ROMs per tests/rom-manifest.toml into gitignored roms/.
# Ticket W0-03: rf-harness owns manifest parsing + mirror/hash verification
# (crates/rf-harness/src/manifest.rs, fetch.rs); this script is a thin
# wrapper that builds/runs the `fetch-test-roms` binary against the repo's
# real manifest. No unzip/archive-extraction step for [[artifact]] entries
# here or in the binary — see crates/rf-harness/src/fetch.rs module doc for
# why. [[git_artifact]] entries (ticket W0-07, e.g. the nes6502 SingleStepTests
# vectors) are fetched too: a pinned-commit sparse `git` checkout instead of
# a single-file download, verified by commit SHA rather than a hash.
#
# Usage:
#   scripts/fetch-test-roms.sh                 # fetch every artifact (both kinds)
#   scripts/fetch-test-roms.sh nestest-rom ...  # fetch only the named artifact id(s)
#
# POSIX-ish, macOS (BSD userland, no `timeout`) and Linux CI compatible —
# no BSD/GNU-specific flags used here; the actual fetch/hash logic lives in
# Rust (crates/rf-harness), not in this script.
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"

exec cargo run --quiet --manifest-path "$repo_root/Cargo.toml" \
  -p rf-harness --bin fetch-test-roms -- \
  --manifest "$repo_root/tests/rom-manifest.toml" \
  --repo-root "$repo_root" \
  "$@"
