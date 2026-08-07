#!/usr/bin/env bash
# RetroForge -- RF-Scroller (ticket W2-10) build script.
#
# The single, exact build invocation this fixture's checked-in ROM hash
# (rom.sha256) was generated from -- CI (.github/workflows/ci.yml) and
# any local rebuild both go through this script, never a hand-typed
# variant, so "same source -> same bytes" is actually one command, not a
# convention two places have to independently remember.
#
# Requires cc65 (cl65/ca65/ld65) on PATH -- `brew install cc65` on
# macOS, matching docs/research/accuracy-and-testing.md's documented
# install method and this ticket's conductor pre-flight probe. Toolchain
# provenance/version pinning risk is recorded in FORMAT.md and the
# ticket's own report -- read those before treating a hash mismatch here
# as a bug in the source.
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$script_dir"

if ! command -v cl65 >/dev/null 2>&1; then
  echo "build.sh: cl65 not found on PATH -- install cc65 (brew install cc65 on macOS)" >&2
  exit 1
fi

mkdir -p build
rm -f build/rf-scroller.nes build/*.o src/*.o

# -Cl (list files last) not needed; explicit source order controls link
# order, which controls BSS/data symbol addresses (FORMAT.md's
# "Documented RAM addresses" section depends on this order never
# changing without also regenerating that section).
cl65 -t nes -o build/rf-scroller.nes \
  --mapfile build/rf-scroller.map \
  src/chr.s \
  src/level_data.c \
  src/main.c

# cl65 compiles each source file to a same-directory .o before linking,
# regardless of -o's target (verified: they land in src/, not build/,
# even though build/rf-scroller.nes and build/rf-scroller.map both land
# where -o/--mapfile said) -- clean those up so a build never leaves
# uncommitted-by-convention-but-not-actually-gitignored cruft next to
# source (src/*.o is also in .gitignore as defense in depth, but this
# is the fix, not just a backstop).
rm -f src/*.o

actual_sha256="$(shasum -a 256 build/rf-scroller.nes | awk '{print $1}')"
echo "build.sh: built build/rf-scroller.nes, sha256=$actual_sha256"

if [ -f rom.sha256 ]; then
  expected_sha256="$(head -n1 rom.sha256 | awk '{print $1}')"
  if [ "$actual_sha256" != "$expected_sha256" ]; then
    echo "build.sh: ROM HASH MISMATCH" >&2
    echo "  expected (rom.sha256): $expected_sha256" >&2
    echo "  actual   (this build): $actual_sha256" >&2
    echo "  cc65 toolchain: $(cl65 --version 2>&1 || true)" >&2
    echo "  This means either the source changed without regenerating" >&2
    echo "  rom.sha256, or this build's cc65 produced different bytes" >&2
    echo "  than the toolchain rom.sha256 was generated with (see" >&2
    echo "  FORMAT.md's toolchain-provenance note) -- never loosen this" >&2
    echo "  check to work around a mismatch; find and pin the cause." >&2
    exit 1
  fi
  echo "build.sh: sha256 matches rom.sha256 (deterministic build confirmed)"
else
  echo "build.sh: no rom.sha256 present yet -- not verifying (first build)"
fi
