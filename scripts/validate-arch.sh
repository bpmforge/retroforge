#!/usr/bin/env bash
# Layer-boundary law (docs/ARCHITECTURE.md §3): core-side crates must not
# depend on host/enhancement/frontend crates; the enhancement/host side must
# not reach into console cores directly; cores must be deterministic.
set -euo pipefail
cd "$(dirname "$0")/.."
fail=0

# 1. Core crates never import upper layers.
CORE_CRATES=(rf-core-api rf-nes rf-snes rf-cart)
FORBIDDEN='rf-renderer|rf-audio|rf-input|rf-enhance|rf-profiles|rf-plugin-sdk|rf-debugger|rf-ai|rf-cache|rf-state|rf-harness|retroforge'
for c in "${CORE_CRATES[@]}"; do
  if grep -E "^\s*($FORBIDDEN)\s*=" "crates/$c/Cargo.toml" >/dev/null 2>&1; then
    echo "ARCH VIOLATION: crates/$c depends on an upper-layer crate" >&2
    fail=1
  fi
done

# 2. rf-core-api depends on no rf-* crate at all.
if grep -E '^\s*rf-' crates/rf-core-api/Cargo.toml >/dev/null 2>&1; then
  echo "ARCH VIOLATION: rf-core-api must not depend on any rf-* crate" >&2
  fail=1
fi

# 3. Only the app shell and the test harness may depend on console cores
#    directly; everything else sees cores through rf-core-api (ARCH §3).
for c in crates/*/; do
  name=$(basename "$c")
  case "$name" in retroforge|rf-harness|rf-nes|rf-snes) continue ;; esac
  if grep -E '^\s*(rf-nes|rf-snes)\s*=' "$c/Cargo.toml" >/dev/null 2>&1; then
    echo "ARCH VIOLATION: crates/$name depends on a console core directly (use rf-core-api)" >&2
    fail=1
  fi
done

# 4. Determinism lint (FR-CORE-003): no wall clock, no host RNG in core crates.
#    Sources only; tests/benches under the crate are exempt via path filter.
DETERMINISM_BAN='std::time|SystemTime|Instant::now|rand::|fastrand|getrandom'
for c in rf-core-api rf-nes rf-snes rf-cart; do
  if grep -RE "$DETERMINISM_BAN" "crates/$c/src" --include='*.rs' >/dev/null 2>&1; then
    echo "ARCH VIOLATION: crates/$c/src uses wall-clock/RNG (determinism invariant, FR-CORE-003):" >&2
    grep -RnE "$DETERMINISM_BAN" "crates/$c/src" --include='*.rs' >&2
    fail=1
  fi
done

# 5. One-wgpu invariant (W3-01): rf-renderer now takes a direct wgpu dep,
#    pinned in lockstep with whatever version egui-wgpu resolves (see
#    docs/TECH_STACK.md's GPU row) -- exactly ONE `wgpu` entry may ever
#    appear in Cargo.lock, or two incompatible wgpu::Device/Queue types
#    would coexist and the shared-device design (RENDERER.md §1) breaks.
#    Previously an honour-system sentence in docs/work/HANDOFF.md; this
#    ticket is precisely when a second wgpu-pulling crate landed, so it
#    stops being reliable unless it's mechanical.
wgpu_count=$(grep -c '^name = "wgpu"$' Cargo.lock || true)
if [ "$wgpu_count" -ne 1 ]; then
  echo "ARCH VIOLATION: expected exactly 1 \"wgpu\" entry in Cargo.lock, found $wgpu_count" >&2
  fail=1
fi

[ $fail -eq 0 ] && echo "arch OK"
exit $fail
