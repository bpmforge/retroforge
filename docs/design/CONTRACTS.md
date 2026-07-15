# Design: Public-Surface Contracts (design-review arc P4)

Each section is the ENTIRE coupling for that surface — anything not listed
is private and may change without notice. Every surface ends with a
**Proof:** clause naming the conformance test that guards it. Versioning
law: NFR-008 — all public formats versioned from first release.

## 1. Core contract (`rf-core-api`) — internal-stable

`EmulatorCore` + `CoreSink` + `StateView` + `CoreEvent` + `InputFrame` +
`Step` exactly as ARCHITECTURE §5. Consumers: frontend, harness (drive);
enhancement/debugger (observe). Cores: rf-nes, rf-snes, and nothing else
constructs machine state. Pixels are indexed + metadata, never RGB (ADR-4).
`state_view()` valid only between frames.
**Proof:** mock-core contract tests (W0-04); validate-arch.sh forbids any
other crate importing cores directly.

## 2. Profile format (TOML, schema v0.x) — PUBLIC

GAME_PROFILES §2 is the schema; `profile_version` semver gates loads
(reject newer major, warn unknown keys — FR-PROF-004). `sources` required
per map entry (FR-PROF-003); `license` (SPDX) required at intake (D-005).
Decoder families versioned via `decode.family_version` (v0.2+).
**Proof:** `retroforge-tool profile validate` in CI over /profiles (W4-02);
golden decode screenshots per shipped profile.

## 3. Save states (`.rfstate`) + replays (`.rfreplay`) — PUBLIC

SAVE_STATES §2/§3: TLV container, per-crate chunk ownership, frame-boundary
only, unknown-optional-skip / missing-required-fail, chunk version bump ⇒
migration fn or explicit error. Replays are BK2-shaped (header + per-frame
inputs + periodic hashes).
**Proof:** roundtrip + cross-mode + golden-fixture CI suites (TESTING §6);
release migration drill (W5-05, R-F3).

## 4. Plugin manifest + host API — PUBLIC (Lua) / in-tree (native until P9)

PLUGINS §2 capability manifest (declared caps enforced at the boundary,
FR-PLUG-001; `write_memory` = mod tier, ledgered; `filesystem` realpath-
contained per D-006). Lua surface: `rf.mem`, `rf.rom`, `rf.on_frame`,
`rf.gui.*`, `rf.cache` with BizHawk-shaped naming (FR-PLUG-002). `api`
version field gates loads. No stable native ABI before Phase 9 (NON_GOALS
#17).
**Proof:** capability-violation UTs, containment tests (TESTING §7), the
shipped example scripts run in CI.

## 5. Fixture format contract (`fixtures/`) — internal-stable

Each fixture ships source + FORMAT.md (level format spec) + checked-in
built-ROM hash + input logs. The fixture's format doc is the ground truth
its profile decodes against — a format change bumps the fixture version and
regenerates goldens together, one commit.
**Proof:** deterministic CI build hash match (W2-10); decode goldens
(W5-02b).

## 6. Board + validator contract (`plan.json`) — process surface

Schema note in plan.json is authoritative (keys, always-writable set,
notes as string-array). `scripts/validate-plan.mjs` (P1–P9) and
`scripts/validate-traceability.mjs` (F1–F5, W1–W3) are the law; both run
in CI and in the per-ticket close gate (D-002).
**Proof:** CI board-validators step; gate evidence rows in docs/STATUS.md.
