#!/usr/bin/env bash
# GPU-pass benchmark evidence gate (ticket W16-01; docs/design/
# ENHANCEMENT_WAVE_16.md §8; docs/TESTING.md §4). Sibling to
# scripts/local-gate.sh, wired the same way: runs the (real-hardware,
# release-mode) benchmark, then validates the resulting evidence file --
# never invoked in CI (no GPU there is guaranteed; local-gate.sh's own
# accuracy suites are the CI-never-runs-this precedent this follows), so
# this is the only place these numbers get regenerated.
#
# Usage: scripts/gpu-gate.sh
#   Optional: RF_ONNX_BENCH=1 also runs the rf-ai `onnx` feature's
#   #[ignore]d ESRGAN-class spike (crates/rf-ai/tests/onnx_bench.rs) if a
#   model + ORT_DYLIB_PATH are already staged -- off by default because it
#   needs a network fetch and a several-hundred-MB model the first time
#   (see scripts/fetch-ai-upscale-model.sh).
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"
cd "$repo_root"

echo "gpu-gate: building + running bench-passes (release)..." >&2
cargo run --quiet --release -p rf-renderer --bin bench-passes

if [ "${RF_ONNX_BENCH:-0}" = "1" ]; then
  echo "gpu-gate: running rf-ai onnx_bench spike (RF_ONNX_BENCH=1)..." >&2
  cargo test --quiet --release -p rf-ai --features onnx --test onnx_bench -- --ignored --nocapture
fi

echo "gpu-gate: validating docs/evidence/gpu-passes.json..." >&2
node "$repo_root/scripts/validate-gpu-evidence.mjs"
