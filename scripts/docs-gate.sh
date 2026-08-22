#!/usr/bin/env bash
# Local mirror of .github/workflows/docs.yml.
#
# WHY THIS EXISTS: GitHub Actions is out of budget, and as of 2026-08-21
# the project treats GitHub as STORAGE rather than as a gate (Brad's
# ruling). Every workflow run since roughly 2026-08-07 was rejected with
# "The job was not started because an Actions budget is preventing further
# use" — 166 failures to 34 successes, and NOT ONE of them a code failure.
# A red badge that means "no minutes left" is worse than no badge, because
# it trains everyone to ignore the one that would have meant something.
#
# So the four steps docs.yml runs are reproduced here, and they run on the
# developer's machine or they run nowhere. `mdbook build` in particular had
# NEVER EXECUTED ANYWHERE before this script was written — the workflow was
# added and its first run was already budget-blocked.
#
# Requires mdbook: cargo install --locked mdbook --version '^0.4'
#
# Usage: scripts/docs-gate.sh
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"
cd "$repo_root"

fail=0
step() {
  local name="$1"
  shift
  echo "docs-gate: $name..." >&2
  if ! "$@"; then
    echo "  docs-gate: $name FAILED" >&2
    fail=1
  fi
}

step "doc samples compile" node .github/scripts/verify-doc-samples.mjs
step "plugin-sdk examples build" cargo build -p rf-plugin-sdk --examples
step "example profile validates" \
  cargo run -q -p retroforge-tool -- profile validate \
  profiles/nes/rf-scroller-demo/profile.toml

if command -v mdbook >/dev/null 2>&1; then
  # Output lands in docs/site/book/, which .gitignore excludes — it is
  # generated and must never be committed.
  step "mdbook build" mdbook build docs/site
else
  echo "docs-gate: SKIP mdbook build — not installed." >&2
  echo "  cargo install --locked mdbook --version '^0.4'" >&2
  fail=1
fi

if [ "$fail" -ne 0 ]; then
  echo "docs-gate: FAILED" >&2
  exit 1
fi
echo "docs-gate: OK" >&2
