#!/usr/bin/env bash
# Local evidence gate (ticket W0-07): runs the heavy, local-only nes6502
# SingleStepTests vector suite (2,560,000 cases; too expensive for every CI
# run — that's the entire reason this exists) and writes the result as a
# small, committed JSON evidence file at docs/evidence/local-gate.json.
# CI never runs this script; it only validates the evidence file
# (scripts/validate-evidence.mjs) — see docs/TESTING.md §4.
#
# Usage: scripts/local-gate.sh
#   Optional env override: RF_NES6502_VECTORS (same convention as
#   crates/rf-nes/src/cpu/tests/vectors.rs — see that file's doc comment
#   for how to fetch the vectors if they aren't present yet, or run
#   `scripts/fetch-test-roms.sh singlestep-nes6502-src`).
#
# POSIX-ish, macOS (BSD userland, no `timeout`) and Linux CI-image
# compatible — matches scripts/fetch-test-roms.sh's portability contract,
# though this script is never actually invoked in CI.
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"
cd "$repo_root"

vectors_dir="${RF_NES6502_VECTORS:-$repo_root/roms/nes/singlestep-nes6502-src/nes6502/v1}"

if [ ! -d "$vectors_dir" ]; then
  echo "local-gate: nes6502 vectors not found at $vectors_dir" >&2
  echo "Fetch them first: scripts/fetch-test-roms.sh singlestep-nes6502-src" >&2
  echo "(or set RF_NES6502_VECTORS to point at an existing checkout)" >&2
  exit 1
fi

# --today / open-ticket-id list: this is the one place in the whole
# pipeline allowed to read plan.json — rf-harness itself never does (see
# crates/rf-harness/src/accuracy.rs module doc and src/bin/local_gate_evidence.rs
# doc comment). "Open" = any ticket not yet marked done; a failing nes6502
# row would need an open, ticketed waiver to pass the accuracy report at
# all (TESTING.md §5) — nes6502 is expected 100% green, so this list is
# exercised but not load-bearing for a normal run.
today="$(date -u +%Y-%m-%d)"
generated_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

open_tickets=()
while IFS= read -r id; do
  open_tickets+=("--open-ticket" "$id")
done < <(node -e "
const plan = require('$repo_root/plan.json');
for (const t of plan.tickets) if (t.status !== 'done') console.log(t.id);
")

echo "local-gate: building local-gate-evidence..." >&2
cargo build --quiet --manifest-path "$repo_root/Cargo.toml" -p rf-harness --bin local-gate-evidence

tmp_out="$(mktemp "${TMPDIR:-/tmp}/local-gate-evidence.XXXXXX")"
trap 'rm -f "$tmp_out"' EXIT

if ! cargo run --quiet --manifest-path "$repo_root/Cargo.toml" \
  -p rf-harness --bin local-gate-evidence -- \
  --manifest "$repo_root/tests/rom-manifest.toml" \
  --waivers "$repo_root/crates/rf-harness/waivers.toml" \
  --repo-root "$repo_root" \
  --vectors-dir "$vectors_dir" \
  --today "$today" \
  --generated-at "$generated_at" \
  "${open_tickets[@]}" \
  > "$tmp_out"; then
  echo "local-gate: evidence generation FAILED — docs/evidence/local-gate.json left unchanged" >&2
  exit 1
fi

mkdir -p "$repo_root/docs/evidence"
mv "$tmp_out" "$repo_root/docs/evidence/local-gate.json"
trap - EXIT

echo "local-gate: wrote docs/evidence/local-gate.json" >&2
