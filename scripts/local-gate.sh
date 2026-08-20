#!/usr/bin/env bash
# Local evidence gate (tickets W0-07, W1-03, W1-05b, W6-01a): runs the heavy,
# local-only nes6502 SingleStepTests vector suite (2,560,000 cases; too
# expensive for every CI run — that's the entire reason this exists), the
# nestest golden-trace diff (8991 lines against a fetched golden log), and
# the real, fetched ppu_vbl_nmi (10 ROMs)/sprite_hit_tests (11 ROMs) suites
# — all gitignored, NFR-006 — and writes the combined result as a small,
# committed JSON evidence file at docs/evidence/local-gate.json. CI never
# runs this script; it only validates the evidence file
# (scripts/validate-evidence.mjs) — see docs/TESTING.md §4.
#
# Usage: scripts/local-gate.sh
#   Optional env overrides:
#     RF_NES6502_VECTORS (same convention as
#       crates/rf-nes/src/cpu/tests/vectors.rs — see that file's doc
#       comment for how to fetch the vectors if they aren't present yet,
#       or run `scripts/fetch-test-roms.sh singlestep-nes6502-src`).
#     RF_65816_VECTORS (same convention as
#       crates/rf-snes/src/cpu/tests/vectors.rs; fetch with
#       `scripts/fetch-65816-vectors.sh`). Unlike the nes6502 vectors this
#       one SKIPS rather than fails when absent — see the block below.
#     RF_NESTEST_ROM / RF_NESTEST_LOG (same convention as
#       crates/rf-nes/src/system/tests/nestest.rs; fetch with
#       `scripts/fetch-test-roms.sh nestest-rom nestest-log`).
#   No overrides for the ppu_vbl_nmi/sprite_hit_tests ROM directories below
#   — `local_gate_evidence.rs` resolves each ROM's path itself from
#   `tests/rom-manifest.toml` plus `--repo-root`, the same way it already
#   does for every other manifest-declared artifact; this script only
#   pre-checks that the expected directories exist, for the same fast/
#   friendly-error reason the vectors/nestest checks below do.
#
# POSIX-ish, macOS (BSD userland, no `timeout`) and Linux CI-image
# compatible — matches scripts/fetch-test-roms.sh's portability contract,
# though this script is never actually invoked in CI.
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"
cd "$repo_root"

vectors_dir="${RF_NES6502_VECTORS:-$repo_root/roms/nes/singlestep-nes6502-src/nes6502/v1}"
nestest_rom="${RF_NESTEST_ROM:-$repo_root/roms/nes/other/nestest.nes}"
nestest_log="${RF_NESTEST_LOG:-$repo_root/roms/nes/other/nestest.log}"
ppu_vbl_nmi_dir="$repo_root/roms/nes/ppu_vbl_nmi/rom_singles"
sprite_hit_dir="$repo_root/roms/nes/sprite_hit_tests_2005.10.05"

# 10k-frame determinism double-run (ticket W1-08): #[ignore]'d in
# crates/retroforge/tests/determinism.rs (debug cost ~93s/pair, measured;
# release cost ~9s, measured) — release-only, same as CI's dedicated step
# (.github/workflows/ci.yml). No docs/evidence/local-gate.json row — see
# docs/TESTING.md §6 for why — but still a hard local-gate failure.
#
# Deliberately runs BEFORE the four ROM-directory pre-checks below
# (conductor ruling, ticket W1-08): this suite's fixture is a synthetic
# in-code NROM and needs no fetched ROMs at all, and because both tests
# are `#[ignore]`'d this script is the ONLY local path that runs them.
# Ordering it after the ROM checks would mean someone without a fetched
# `roms/` — the exact person who most needs a ROM-free determinism check —
# could never run it. The cost is that a missing-ROM run now pays ~9s
# before reporting the missing directory, which is the cheaper mistake.
echo "local-gate: running 10k-frame determinism double-run (release)..." >&2
if ! cargo test --release --manifest-path "$repo_root/Cargo.toml" \
  -p retroforge --test determinism -- --ignored; then
  echo "local-gate: 10k-frame determinism double-run FAILED" >&2
  exit 1
fi

# 65816 SingleStepTests vectors (tickets W6-01a, W6-01b): 512 per-opcode files —
# ALL 256 opcodes, in BOTH emulation
# (`.e`) and native (`.n`) mode, at ~10,000 cases each. The list is
# deliberately NOT derived from the implementation — coverage is a number
# the suite discovers and prints (254 of 256 as of W6-01b), which is how
# it caught a wholly broken addressing-mode table that an ops.rs-derived
# list had hidden behind an all-green run. Local-only and
# release-only for the same reason nes6502 is: too expensive per CI run,
# and NFR-006 keeps vector data off CI entirely.
#
# Follows the determinism run directly above in shape: a hard local-gate
# failure with NO docs/evidence/local-gate.json row. The row is omitted
# because the evidence generator lives in rf-harness, which does not (and
# under the layering rules should not) depend on rf-snes to run a core's
# own unit tests — the nes6502 row is generated there only because
# rf-harness already owns that path. Adding a 65816 row is a rf-harness
# change and belongs to a rf-harness-scoped ticket, not this one.
#
# Skips cleanly rather than failing when the vectors are absent: the test
# itself prints a SKIP and returns, so someone without
# scripts/fetch-65816-vectors.sh run yet still gets a usable gate. What it
# will NOT do is silently pass on an empty directory — see the
# "ran zero cases is not a pass" assertion in the runner.
echo "local-gate: running 65816 SingleStepTests vectors (release)..." >&2
if ! cargo test --release --manifest-path "$repo_root/Cargo.toml" \
  -p rf-snes -- --ignored --nocapture; then
  echo "local-gate: 65816 vector suite FAILED" >&2
  exit 1
fi

# gilyon snes-tests cputest (ticket W6-02b): 1610 tests covering every
# 65C816 opcode except STP/WAI, in every addressing mode, in both
# emulation and native mode. Local-only (NFR-006) and #[ignore]'d, so this
# script is the only path that runs it.
#
# A DIFFERENT ORACLE from the 65816 vectors above, not a weaker copy of
# them: this one is a PROGRAM. It boots on the real bus, sets up the PPU,
# waits on $4210, reads controllers through auto-joypad, and runs across
# four ROM banks with JSL/RTL. A vector suite cannot fail the way a wrong
# memory map, a stuck vblank flag or a mis-latched joypad port fails.
#
# Needs the archive EXTRACTED, which rf-harness does not do (see its
# fetch.rs module doc — manifest artifacts land as the downloaded file
# itself). Unzipped here rather than by the test, so the test stays a
# test; it skips cleanly when the directory is absent.
gilyon_zip="$repo_root/roms/snes/gilyon-snes-tests-v1.4.zip"
gilyon_dir="$repo_root/roms/snes/gilyon-snes-tests"
if [ -f "$gilyon_zip" ] && [ ! -f "$gilyon_dir/cputest/cputest-full.sfc" ]; then
  echo "local-gate: extracting gilyon snes-tests..." >&2
  mkdir -p "$gilyon_dir"
  unzip -o -q "$gilyon_zip" -d "$gilyon_dir"
fi

echo "local-gate: running gilyon cputest (release)..." >&2
if ! cargo test --release --manifest-path "$repo_root/Cargo.toml" \
  -p rf-snes --test gilyon_cputest -- --ignored --nocapture; then
  echo "local-gate: gilyon cputest FAILED" >&2
  exit 1
fi

# PeterLemon PPU golden frames (ticket W6-03b): the four 2BPP BGMAP tests,
# one per background layer, in BG mode 0. Local-only (NFR-006).
#
# Fetched per-file rather than as the 240 MB repo snapshot the manifest
# once pointed at — see scripts/fetch-peterlemon-ppu.sh.
if [ ! -d "$repo_root/roms/snes/peterlemon-ppu" ]; then
  echo "local-gate: PeterLemon PPU ROMs absent; fetching..." >&2
  "$script_dir/fetch-peterlemon-ppu.sh" || true
fi

echo "local-gate: running PeterLemon PPU golden frames (release)..." >&2
if ! cargo test --release --manifest-path "$repo_root/Cargo.toml" \
  -p rf-snes --test peterlemon_golden -- --ignored --nocapture; then
  echo "local-gate: PeterLemon golden frames FAILED" >&2
  exit 1
fi

# SPC700 SingleStepTests vectors (ticket W6-04a): 256 opcode files, 1000
# cases each. Local-only (NFR-006); needs the archive extracted, which
# rf-harness does not do.
spc_zip="$repo_root/roms/snes/singlestep-spc700-67d15f4.zip"
spc_dir="$repo_root/roms/snes/singlestep-spc700"
if [ -f "$spc_zip" ] && [ ! -d "$spc_dir" ]; then
  echo "local-gate: extracting spc700 vectors..." >&2
  mkdir -p "$spc_dir"
  unzip -o -q "$spc_zip" -d "$spc_dir"
fi

echo "local-gate: running spc700 vectors (release)..." >&2
if ! cargo test --release --manifest-path "$repo_root/Cargo.toml" \
  -p rf-snes --test spc700_vectors -- --ignored --nocapture; then
  echo "local-gate: spc700 vector suite FAILED" >&2
  exit 1
fi

if [ ! -d "$vectors_dir" ]; then
  echo "local-gate: nes6502 vectors not found at $vectors_dir" >&2
  echo "Fetch them first: scripts/fetch-test-roms.sh singlestep-nes6502-src" >&2
  echo "(or set RF_NES6502_VECTORS to point at an existing checkout)" >&2
  exit 1
fi

if [ ! -f "$nestest_rom" ] || [ ! -f "$nestest_log" ]; then
  echo "local-gate: nestest ROM/log not found ($nestest_rom / $nestest_log)" >&2
  echo "Fetch them first: scripts/fetch-test-roms.sh nestest-rom nestest-log" >&2
  echo "(or set RF_NESTEST_ROM/RF_NESTEST_LOG to point at existing files)" >&2
  exit 1
fi

if [ ! -d "$ppu_vbl_nmi_dir" ]; then
  echo "local-gate: ppu_vbl_nmi ROMs not found at $ppu_vbl_nmi_dir" >&2
  echo "Fetch them first: scripts/fetch-test-roms.sh" >&2
  exit 1
fi

if [ ! -d "$sprite_hit_dir" ]; then
  echo "local-gate: sprite_hit_tests ROMs not found at $sprite_hit_dir" >&2
  echo "Fetch them first: scripts/fetch-test-roms.sh" >&2
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
  --nestest-rom "$nestest_rom" \
  --nestest-log "$nestest_log" \
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
