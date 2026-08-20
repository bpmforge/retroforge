#!/usr/bin/env bash
# RetroForge -- SNES LoROM/HiROM rf-scroller-s fixture (ticket W6-05) build
# script.
#
# The single, exact build invocation this fixture's checked-in ROM hash
# (rom.sha256) was generated from -- CI and any local rebuild both go
# through this script, never a hand-typed variant, so "same source ->
# same bytes" is one command rather than a convention two places have to
# independently remember. Same contract as
# fixtures/nes/rf-scroller/build.sh, deliberately.
#
# ## Why cc65 and not asar or libSFX
#
# docs/DECISIONS.md D-001 and this ticket both say "libSFX", and the
# phase-entry assumption was that a new toolchain had to be installed and
# licence-checked first. Checked rather than assumed: **cc65's ca65
# already assembles 65816** (`ca65 --cpu 65816`, with `.p816` and
# explicit `.a8`/`.a16` width directives), and ld65 links a flat LoROM
# image from a linker config. cc65 is ALREADY installed in CI for the NES
# fixture.
#
# So this fixture needs no new dependency, no TECH_STACK row and no
# licence review, and the SNES phase is not gated on a toolchain
# decision. libSFX remains the right answer for RF-Scroller-S (W6-05),
# which wants a framework's init code, macros and asset pipeline -- but a
# CARTRIDGE MAPPING test needs one bank, a header and vectors, and
# pulling in a framework for that would be a large dependency
# contributing nothing the test uses.
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$script_dir"

if ! command -v ca65 >/dev/null 2>&1 || ! command -v ld65 >/dev/null 2>&1; then
  echo "build.sh: ca65/ld65 not found on PATH -- install cc65 (brew install cc65 on macOS)" >&2
  exit 1
fi

mkdir -p build
rm -f build/*.o build/*.sfc

# ONE source, two mappings. The -D switch is what makes the pair a
# comparison rather than two unrelated fixtures: everything except the
# mapping under test is identical by construction.
build_one() {
  name="$1"; cfg="$2"; shift 2
  ca65 --cpu 65816 "$@" -o "build/$name.o" src/main.s
  ld65 -C "src/$cfg" -o "build/$name.sfc" "build/$name.o"
  rm -f "build/$name.o"
  actual="$(shasum -a 256 "build/$name.sfc" | cut -d' ' -f1)"
  echo "build.sh: built build/$name.sfc, sha256=$actual"

  pin="rom.sha256"
  if [ -f "$pin" ]; then
    expected="$(cut -d' ' -f1 "$pin")"
    if [ "$actual" != "$expected" ]; then
      echo "build.sh: sha256 MISMATCH" >&2
      echo "  expected ($pin): $expected" >&2
      echo "  actual:          $actual" >&2
      echo "  Either the source changed and the pin needs re-pinning, or this" >&2
      echo "  build's cc65 produced different bytes than the toolchain the pin" >&2
      echo "  was generated with -- never loosen this check to work around a" >&2
      echo "  mismatch; find and pin the cause." >&2
      exit 1
    fi
    echo "build.sh: matches $pin (deterministic build confirmed)"
  else
    echo "$actual  $name.sfc" > "$pin"
    echo "build.sh: pinned $pin (first build)"
  fi
}

build_one rf-scroller-s lorom.cfg
