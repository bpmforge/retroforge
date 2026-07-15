# RetroForge — Constraints

Status: Phase 0 · 2026-07-06

Hard lines. Violating one is an incident, not a trade-off.

## 1. Technical constraints

- **Determinism invariant.** Cores contain no wall-clock reads, no RNG, no
  thread-timing dependence. Same ROM + initial state + input log ⇒
  bit-identical machine state, every frame, every platform. CI enforces via
  replay + per-frame state hashes (`docs/TESTING.md`).
- **Mode invariant.** Accuracy and Enhanced modes produce identical core
  state hashes for identical inputs — enhancement observes, never perturbs
  (`docs/ARCHITECTURE.md` §4).
- **One-way layering.** `rf-nes`/`rf-snes` depend only on `rf-core-api` +
  `rf-cart`. No core crate imports renderer, enhancement, profile, plugin,
  or UI crates. Enforced by `scripts/validate-arch.sh` in CI.
- **Indexed-pixel contract.** Pixels cross the core boundary as indexed
  color + source metadata (layer, sprite id, priority), never premixed RGB
  (ARCHITECTURE §5). Every enhancement feature depends on this; it cannot
  be traded away for render speed.
- **Frame-boundary states.** Save states are taken at frame boundaries only
  (`docs/design/SAVE_STATES.md`) — no mid-frame serialization complexity.
- **No AI, no network, no blocking I/O on the frame path.** AI and decode
  jobs are async, cache-backed, droppable.

## 2. Legal constraints

- **No ROM data in the repository or releases.** Not commercial ROMs, not
  ROM-derived assets (tiles, maps, audio) for commercial games. Profiles for
  commercial games contain only facts: addresses, formats, rules.
- **Test/demo ROMs arrive by fetch manifest** (`tests/rom-manifest.toml`:
  URL + SHA-256 + license), downloaded by `scripts/fetch-test-roms.sh` into
  a gitignored directory. This applies even to GPLv3 homebrew (Nova the
  Squirrel 1/2): fetch, don't vendor — keeps the repo license clean
  (GPLv3 fixtures must not link into our MIT/Apache tree) and every
  fixture's provenance auditable. Public-domain fixtures we build from
  source in CI (cc65/libSFX) are the exception and may live in-tree as
  source. Before any demo *bundles* homebrew content (screenshots in docs
  are fine), re-verify that title's asset licensing — code and assets are
  sometimes licensed differently; Nova's asset terms must be confirmed
  before redistribution beyond fetch-by-manifest.
- **Clean-room documentation.** Per-game knowledge in profiles cites its
  source (DataCrystal URL, own debugger session, disassembly) —
  `docs/design/GAME_PROFILES.md` makes `sources` required. No decompiled
  copyrighted code is committed.
- **Facts-only transcription policy** (design review G-41, 2026-07-15;
  vetoable — FS-3). DataCrystal content is **GFDL 1.2** (copyleft).
  Profiles may take individual facts (addresses, sizes, enumerated
  values) — facts are not copyrightable — but: all prose descriptions
  are written fresh; never transcribe a full curated table verbatim
  (selection/arrangement can be protected expression); restructure into
  our own schema and grouping; the `source` URL is provenance, not
  license inheritance; verify facts against the running game where
  practical. Same policy applies to any copyleft wiki source.
- **User-provided ROMs only.** The app never links to ROM sources. No
  circumvention features beyond standard emulation of unprotected dumps.
- **Repo license: MIT OR Apache-2.0** (workspace manifest already declares
  it). Contributions under the same. GPL-licensed reference emulators
  (bsnes, ares) are *behavioral* references — no code copying; MesenCE is
  GPLv3 likewise.

## 3. Resource constraints

- **Team = one maintainer + AI coding agents.** Every design must be
  executable by a cheaper coding model working from `plan.json` tickets with
  acceptance criteria; docs are written for that consumer (precision over
  prose). Mitigations for agent-quality risk in `docs/RISKS.md` R-13.
- **Primary dev machine: Apple Silicon macOS** — Metal-via-wgpu is the
  daily-driven backend; Vulkan/DX12 are CI/user targets. Perf targets in
  `docs/SRS.md` NFRs are set against M-class hardware.
- **CI: GitHub Actions** (macOS + Linux runners minimum; Windows for
  release gates). Test-ROM fetch must stay cache-friendly (~tens of MB).
- **Budget: zero recurring services.** No cloud inference, no hosted
  backend; CI minutes and a GitHub org are the infrastructure.

## 4. Dependency constraints

- Pinned majors for wgpu/egui; upgraded in lockstep, one PR, quarterly
  cadence expected (`docs/research/rust-stack.md` §1).
- bincode ≥3 idioms only (`Encode`/`Decode` derives); bincode 1.x patterns
  are a build error by policy (deny in code review; see RISKS R-11).
- `unsafe_code = "warn"` workspace-wide; each `unsafe` block needs a
  justifying comment and a test.
