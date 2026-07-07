#!/usr/bin/env bash
# Layer-boundary law (docs/ARCHITECTURE.md §3): core-side crates must not
# depend on host/enhancement/frontend crates.
set -euo pipefail
cd "$(dirname "$0")/.."
CORE_CRATES=(rf-core-api rf-nes rf-snes rf-cart)
FORBIDDEN='rf-renderer|rf-audio|rf-input|rf-enhance|rf-profiles|rf-plugin-sdk|rf-debugger|rf-ai|rf-cache|retroforge'
fail=0
for c in "${CORE_CRATES[@]}"; do
  if grep -E "^\s*($FORBIDDEN)\s*=" "crates/$c/Cargo.toml" >/dev/null 2>&1; then
    echo "ARCH VIOLATION: crates/$c depends on an upper-layer crate" >&2
    fail=1
  fi
done
# rf-core-api additionally depends on no rf-* crate at all
if grep -E '^\s*rf-' crates/rf-core-api/Cargo.toml >/dev/null 2>&1; then
  echo "ARCH VIOLATION: rf-core-api must not depend on any rf-* crate" >&2
  fail=1
fi
[ $fail -eq 0 ] && echo "arch OK"
exit $fail
