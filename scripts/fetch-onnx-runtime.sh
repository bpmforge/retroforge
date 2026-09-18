#!/usr/bin/env bash
# Fetches the official ONNX Runtime shared library for macOS arm64,
# hash-pinned, into a gitignored cache OUTSIDE the git tree (ticket W16-01;
# docs/design/AI_UPSCALING.md §5's `load-dynamic` decision:
# `ort::init_from` dlopen's this dylib at a caller-supplied path -- it is
# NEVER linked at build time and NEVER vendored into git, same posture as
# scripts/fetch-test-roms.sh for ROM bytes, NFR-006).
#
# License: ONNX Runtime is MIT (Microsoft) -- see the extracted
# LICENSE file; recorded in crates/rf-ai/ai-model-manifest.toml (this ticket's own licence ledger, tests/rom-manifest.toml-style, but outside this ticket's write_scope so a separate file) as a
# license_status = "permissive" [[artifact]] row (ticket W16-01).
#
# Destination defaults to a directory OUTSIDE the repo entirely
# ($RF_AI_CACHE, default: a persistent user cache dir) rather than
# anywhere under the repo tree -- this ticket's write_scope does not cover
# .gitignore, so "never in the tree" is enforced by never writing into the
# tree in the first place, not by an ignore rule.
#
# Usage: scripts/fetch-onnx-runtime.sh
#   Optional: RF_AI_CACHE=/custom/cache/dir
#
# After running, set:
#   export ORT_DYLIB_PATH="$(scripts/fetch-onnx-runtime.sh --print-path)"
set -euo pipefail

VERSION="1.20.1"
ASSET="onnxruntime-osx-arm64-${VERSION}.tgz"
URL="https://github.com/microsoft/onnxruntime/releases/download/v${VERSION}/${ASSET}"
# Verified by this ticket (2026-09-17): `shasum -a 256` of the downloaded
# release asset, fetched directly from the GitHub releases page above.
SHA256="b678fc3c2354c771fea4fba420edeccfba205140088334df801e7fc40e83a57a"

cache_dir="${RF_AI_CACHE:-$HOME/.cache/retroforge-ai}"
mkdir -p "$cache_dir"
archive="$cache_dir/$ASSET"
extract_dir="$cache_dir/onnxruntime-osx-arm64-${VERSION}"
dylib_path="$extract_dir/lib/libonnxruntime.dylib"

if [ "${1:-}" = "--print-path" ]; then
  if [ ! -f "$dylib_path" ]; then
    echo "fetch-onnx-runtime: not fetched yet -- run scripts/fetch-onnx-runtime.sh first" >&2
    exit 1
  fi
  echo "$dylib_path"
  exit 0
fi

if [ -f "$dylib_path" ]; then
  echo "fetch-onnx-runtime: already present at $dylib_path" >&2
  exit 0
fi

echo "fetch-onnx-runtime: downloading $URL..." >&2
curl -fsSL -o "$archive" "$URL"

actual_sha="$(shasum -a 256 "$archive" | awk '{print $1}')"
if [ "$actual_sha" != "$SHA256" ]; then
  echo "fetch-onnx-runtime: FATAL -- sha256 mismatch for $ASSET" >&2
  echo "  expected: $SHA256" >&2
  echo "  actual:   $actual_sha" >&2
  rm -f "$archive"
  exit 1
fi

tar -xzf "$archive" -C "$cache_dir"
rm -f "$archive"
echo "fetch-onnx-runtime: extracted to $extract_dir" >&2
echo "fetch-onnx-runtime: dylib at $dylib_path" >&2
echo "fetch-onnx-runtime: set ORT_DYLIB_PATH=$dylib_path" >&2
