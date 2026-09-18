#!/usr/bin/env bash
# Fetches a Real-ESRGAN-class ONNX upscaler model, SHA-256 pinned, into a
# gitignored cache OUTSIDE the git tree (ticket W16-01/W16-02; NFR-011;
# see crates/rf-ai/ai-model-manifest.toml's own module doc for the
# vocabulary this script's license accounting follows).
#
# ---------------------------------------------------------------------
# MODEL selector (ticket W16-02 criterion 1)
# ---------------------------------------------------------------------
#   MODEL=x4-fp32   (default) -- the original W16-01 fetch: a full-size
#                      Real-ESRGAN x4 fp32 export, STATIC 64x64 input.
#                      Third-party re-export, no licence file of its own
#                      -- ledgered `no-license-grant-fetch-only`.
#   MODEL=compact    -- Real-ESRGAN-General-x4v3 (the SRVGGNetCompact
#                      architecture -- ~5 MB vs. the x4-fp32 export's
#                      full ESRGAN body), Qualcomm AI Hub's own ONNX
#                      conversion, STATIC 128x128 input, x4 scale. Also
#                      third-party (Qualcomm's LICENSE file for this repo
#                      only points at xinntao's upstream, granting nothing
#                      of its own for the conversion) -- ledgered the same
#                      way, `no-license-grant-fetch-only`.
#
# Neither export has a DYNAMIC input shape (verified empirically for
# x4-fp32 in ticket W16-01; verified against `compactmodel/.../
# metadata.json`'s declared `"shape": [1, 3, 128, 128]` for `compact` in
# W16-02) -- both need tiling. `OnnxUpscaler` (ticket W16-12,
# `crates/rf-ai/src/tiling.rs`) reads each model's own declared input
# size from the session and tiles automatically; there is no longer a
# hardcoded tile-size constant anywhere in this crate. A W16-12 search
# for a permissively licensed DYNAMIC-shape alternative found none that
# clears this ledger's own licence bar -- see this file's own header
# comment above.
#
# Both models trace to xinntao/Real-ESRGAN's upstream weights, which ARE
# BSD-3-Clause (verified https://github.com/xinntao/Real-ESRGAN/blob/master/LICENSE).
# Neither re-export carries that licence file itself, which is why both
# are ledgered `no-license-grant-fetch-only` rather than `permissive` --
# same "a third party's re-export needs its own explicit statement"
# caution this project already applies to ROMs.
#
# OpenRAIL-class weights are REFUSED OUTRIGHT by this ticket's own rule
# (docs/design/ENHANCEMENT_WAVE_16.md §7) -- neither catalog entry below
# is OpenRAIL, and `refuse_if_openrail` below is a standing guard against
# a future entry being added without checking.
#
# ---------------------------------------------------------------------
# Runtime
# ---------------------------------------------------------------------
# `OnnxUpscaler` (`crates/rf-ai/src/onnx.rs`) `dlopen`'s the ONNX Runtime
# from a path YOU supply -- it is never linked at build time and never
# fetched by this script. Fetch it separately with
# `scripts/fetch-onnx-runtime.sh`, then point at it:
#
#   export ORT_DYLIB_PATH="$(scripts/fetch-onnx-runtime.sh --print-path)"
#
# ---------------------------------------------------------------------
# Usage
# ---------------------------------------------------------------------
#   scripts/fetch-ai-upscale-model.sh [--print-path]
#     Optional: MODEL=x4-fp32|compact   (default: x4-fp32)
#     Optional: RF_AI_CACHE=/custom/cache/dir
set -euo pipefail

MODEL="${MODEL:-x4-fp32}"

refuse_if_openrail() {
  case "$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]')" in
    *openrail*)
      echo "fetch-ai-upscale-model: FATAL -- OpenRAIL-class weights are refused (docs/design/ENHANCEMENT_WAVE_16.md §7)" >&2
      exit 1
      ;;
  esac
}

case "$MODEL" in
  x4-fp32)
    # Verified by ticket W16-01 (2026-09-17): `shasum -a 256` of the file
    # fetched directly from the URL below.
    URL="https://huggingface.co/imgdesignart/realesrgan-x4-onnx/resolve/main/onnx/model.onnx"
    SHA256="fa18ce70de3a55f3149d0cc898d335d2d69fca29edc0692cb362c856b2942c3f"
    LICENSE_STATUS="no-license-grant-fetch-only"
    DEST_NAME="realesrgan-x4-fp32.onnx"
    IS_ZIP=0
    INNER_PATH=""
    ;;
  compact)
    # Verified by ticket W16-02 (2026-09-18): `shasum -a 256` of the zip
    # fetched directly from the URL below (Qualcomm AI Hub's public S3
    # release bucket, named by `qualcomm/Real-ESRGAN-General-x4v3`'s own
    # `release_assets.json` on Hugging Face). The zip bundles the .onnx
    # graph plus its external-data (.data) weights file, so both must be
    # kept together -- `--print-path` returns the .onnx file's path, and
    # `ort` finds the sibling .data file next to it by ONNX convention.
    URL="https://qaihub-public-assets.s3.us-west-2.amazonaws.com/qai-hub-models/models/real_esrgan_general_x4v3/releases/v0.62.2/real_esrgan_general_x4v3-onnx-float.zip"
    SHA256="19e208e88e906097a9df3fc5ca2318cb745c26a556348dc48fce562f36c49d38"
    LICENSE_STATUS="no-license-grant-fetch-only"
    DEST_NAME="realesrgan-general-x4v3-compact.zip"
    IS_ZIP=1
    INNER_PATH="real_esrgan_general_x4v3-onnx-float/real_esrgan_general_x4v3.onnx"
    ;;
  *)
    echo "fetch-ai-upscale-model: unknown MODEL=$MODEL (expected x4-fp32 or compact)" >&2
    exit 1
    ;;
esac

refuse_if_openrail "$LICENSE_STATUS"

cache_dir="${RF_AI_CACHE:-$HOME/.cache/retroforge-ai}"
mkdir -p "$cache_dir"
dest="$cache_dir/$DEST_NAME"

resolved_path() {
  if [ "$IS_ZIP" = "1" ]; then
    echo "$cache_dir/${DEST_NAME%.zip}/$INNER_PATH"
  else
    echo "$dest"
  fi
}

if [ "${1:-}" = "--print-path" ]; then
  path="$(resolved_path)"
  if [ ! -f "$path" ]; then
    echo "fetch-ai-upscale-model: not fetched yet -- run MODEL=$MODEL scripts/fetch-ai-upscale-model.sh first" >&2
    exit 1
  fi
  echo "$path"
  exit 0
fi

if [ -f "$dest" ]; then
  actual_sha="$(shasum -a 256 "$dest" | awk '{print $1}')"
  if [ "$actual_sha" = "$SHA256" ]; then
    echo "fetch-ai-upscale-model: $MODEL already present and verified at $dest" >&2
  else
    echo "fetch-ai-upscale-model: cached file hash mismatch, re-fetching" >&2
    rm -f "$dest"
  fi
fi

if [ ! -f "$dest" ]; then
  echo "fetch-ai-upscale-model: downloading $MODEL from $URL..." >&2
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
fi

if [ "$IS_ZIP" = "1" ]; then
  extract_dir="$cache_dir/${DEST_NAME%.zip}"
  if [ ! -f "$extract_dir/$INNER_PATH" ]; then
    echo "fetch-ai-upscale-model: extracting $dest..." >&2
    mkdir -p "$extract_dir"
    unzip -o -q "$dest" -d "$extract_dir"
  fi
fi

echo "fetch-ai-upscale-model: $MODEL ready at $(resolved_path)" >&2
