#!/usr/bin/env bash
# Fetches a Real-ESRGAN-class ONNX upscaler model, SHA-256 pinned, into a
# gitignored cache OUTSIDE the git tree (ticket W16-01; NFR-011; see
# crates/rf-ai/ai-model-manifest.toml's own module doc for the vocabulary
# this script's license accounting follows).
#
# Model: an fp32 ONNX export of Real-ESRGAN's x4 general model
# (xinntao/Real-ESRGAN upstream weights, BSD-3-Clause -- verified against
# https://github.com/xinntao/Real-ESRGAN/blob/master/LICENSE, 2026-09-17),
# re-hosted as a mechanical ONNX conversion of those same weights at
# huggingface.co/imgdesignart/realesrgan-x4-onnx. fp32, NOT the sibling
# fp16 export in that same repo -- verified empirically (ticket W16-01):
# the fp16 file demands a tensor(float16) input, which does not match
# `OnnxUpscaler`'s NCHW-float32-RGB contract
# (`crates/rf-ai/src/onnx.rs`'s module doc) and errors at inference time
# with "Unexpected input data type. Actual: (tensor(float)), expected:
# (tensor(float16))". The re-export carries no LICENSE file of its own in
# that repo, so this artifact is recorded as
# `license_status = "no-license-grant-fetch-only"` in crates/rf-ai/ai-model-manifest.toml
# (NOT "permissive") pending an explicit re-statement from the mirror --
# same "the weights' upstream licence does not automatically cover a third
# party's re-export" caution this project already applies to ROMs. It is
# fetch-from-origin only: never vendored, never re-hosted by this repo.
#
# OpenRAIL-class weights are refused outright by this ticket's own rule
# (docs/design/ENHANCEMENT_WAVE_16.md §7) -- this model is not OpenRAIL.
#
# Usage: scripts/fetch-ai-upscale-model.sh
#   Optional: RF_AI_CACHE=/custom/cache/dir
set -euo pipefail

URL="https://huggingface.co/imgdesignart/realesrgan-x4-onnx/resolve/main/onnx/model.onnx"
# Verified by this ticket (2026-09-17): `shasum -a 256` of the downloaded
# file, fetched directly from the URL above.
SHA256="fa18ce70de3a55f3149d0cc898d335d2d69fca29edc0692cb362c856b2942c3f"

cache_dir="${RF_AI_CACHE:-$HOME/.cache/retroforge-ai}"
mkdir -p "$cache_dir"
dest="$cache_dir/realesrgan-x4-fp32.onnx"

if [ "${1:-}" = "--print-path" ]; then
  if [ ! -f "$dest" ]; then
    echo "fetch-ai-upscale-model: not fetched yet -- run scripts/fetch-ai-upscale-model.sh first" >&2
    exit 1
  fi
  echo "$dest"
  exit 0
fi

if [ -f "$dest" ]; then
  actual_sha="$(shasum -a 256 "$dest" | awk '{print $1}')"
  if [ "$actual_sha" = "$SHA256" ]; then
    echo "fetch-ai-upscale-model: already present and verified at $dest" >&2
    exit 0
  fi
  echo "fetch-ai-upscale-model: cached file hash mismatch, re-fetching" >&2
fi

echo "fetch-ai-upscale-model: downloading $URL..." >&2
tmp="$dest.tmp"
curl -fsSL -o "$tmp" "$URL"

actual_sha="$(shasum -a 256 "$tmp" | awk '{print $1}')"
if [ "$actual_sha" != "$SHA256" ]; then
  echo "fetch-ai-upscale-model: FATAL -- sha256 mismatch" >&2
  echo "  expected: $SHA256" >&2
  echo "  actual:   $actual_sha" >&2
  rm -f "$tmp"
  exit 1
fi

mv "$tmp" "$dest"
echo "fetch-ai-upscale-model: verified, saved to $dest" >&2
