#!/usr/bin/env bash
# Fetch SingleStepTests 65816 vectors for the rf-snes CPU suite (W6-01a).
#
# WHY THIS IS NOT `rf-harness fetch`
#
# tests/rom-manifest.toml deferred a choice to "the ticket that actually
# consumes these vectors": fetch a 490 MB archive per run, or curate a
# leaner subset. The premise was wrong in a helpful direction — upstream
# is ALREADY per-opcode. `v1/` holds 512 files, `<op>.e.json` and
# `<op>.n.json`, so there is no archive to unpack and no subset to
# hand-curate. This script pulls exactly the opcodes rf-snes implements.
#
# Local only. CI never runs this: NFR-006 keeps ROM and vector data off CI
# entirely, the same posture the nes6502 suite has.
set -euo pipefail

COMMIT=db6b10401729d5f20f2181dde5d3d7b037093a4a
DEST="${RF_65816_VECTORS:-$(git rev-parse --show-toplevel)/roms/snes/singlestep-65816/v1}"
BASE="https://raw.githubusercontent.com/SingleStepTests/65816/${COMMIT}/v1"

# ALL 256 opcodes, both modes — deliberately NOT "the ones ops.rs
# implements".
#
# The first version of this script derived the list by grepping ops.rs.
# That made the gate self-referential — an opcode the core does not
# implement is invisible to the suite that is supposed to police it — and
# it failed in an even dumber way than that in practice: the grep only
# matched the FIRST opcode on each match-arm line, so `0x09 | 0x05 | 0x15
# | ...` contributed `0x09` alone. The ALU addressing modes were never
# fetched, never run, and hid a completely broken `alu_mode` offset table
# underneath a 139-of-139-green report.
#
# So the list is not derived from the code at all. The runner reports
# which opcodes the core does not implement; coverage is a fact it
# discovers, not an assumption baked into the fetch.
mkdir -p "$DEST"
count=0; skipped=0
for op in $(seq 0 255 | xargs -n1 printf "%02x\n"); do
  for mode in e n; do
    f="$DEST/${op}.${mode}.json"
    if [ -s "$f" ]; then skipped=$((skipped+1)); continue; fi
    if curl -fsSL "${BASE}/${op}.${mode}.json" -o "$f.part"; then
      mv "$f.part" "$f"; count=$((count+1))
    else
      rm -f "$f.part"
      echo "WARN: no upstream file for ${op}.${mode}" >&2
    fi
  done
done

echo "65816 vectors: fetched $count, already present $skipped, in $DEST"
echo "Run them with: cargo test --release -p rf-snes -- --ignored --nocapture"
