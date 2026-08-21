#!/usr/bin/env bash
# RetroForge -- Action 53 (mapper 28) fixture build (ticket W7-11).
#
# The single, exact invocation rom.sha256 was generated from, matching
# fixtures/nes/rf-scroller/build.sh's rule: same source -> same bytes, via
# one command rather than a convention. Requires cc65 on PATH.
#
# The ROM itself is NOT committed (law 5: no ROM bytes in git) -- only its
# hash is. Build it locally to run the ignored test that consumes it.
set -euo pipefail
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$script_dir"

if ! command -v ca65 >/dev/null 2>&1 || ! command -v ld65 >/dev/null 2>&1; then
  echo "build.sh: ca65/ld65 not found -- install cc65 (brew install cc65)" >&2
  exit 1
fi

mkdir -p build
ca65 -o build/main.o src/main.s
ld65 -C src/nes.cfg -o build/action53.nes build/main.o
shasum -a 256 build/action53.nes | awk '{print $1}' > build/action53.sha256
echo "built build/action53.nes ($(wc -c < build/action53.nes) bytes)"
cat build/action53.sha256
