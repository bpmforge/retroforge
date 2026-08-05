# RetroForge — Test Strategy

Status: Phase 2 baseline · 2026-07-06
Sources: `docs/research/accuracy-and-testing.md` (suite provenance + URLs),
SRS verification column, `docs/design/SAVE_STATES.md`.

## 1. Test pyramid

| Level | What | Runner | Gate |
|---|---|---|---|
| 1. CPU vector tests | SingleStepTests JSON vectors (`nes6502`, `65816`, `spc700`): per-opcode state + cycle-by-cycle bus activity, no ROM loader needed | `cargo test` (native unit tests in rf-nes / rf-snes) | every PR |
| 2. Trace conformance | nestest vs golden `nestest.log` (byte-exact PC/A/X/Y/P/SP/CYC + disassembly, `rf_nes::trace::format_trace_line`) | `cargo test` (rf-nes) + rf-harness local-gate evidence | Tier A-local (see §4) |
| 3. Test-ROM integration | blargg / mmc3 / gilyon / PeterLemon / undisbeliever suites | rf-harness headless ($6000 protocol or golden frame) | every PR (tier A), nightly (tier B) |
| 4. Golden-frame | Framebuffer SHA-256 vs checked-in golden PNGs at declared frame N | rf-harness | every PR |
| 5. Determinism & replay | double-run state-hash equality; `.rfreplay` replay to final hash; save-state roundtrip; mode invariant | rf-harness | every PR — **release gate (NFR-001)** |
| 6. Enhancement invariants | Accuracy-vs-Enhanced core-hash equality, anti-flicker goldens, stitcher determinism, profile validation | rf-harness + cargo test | every PR once rf-enhance exists |
| 7. Perf benchmarks | criterion: ms/frame per core, enhancement frame budget | `cargo bench` + threshold script | nightly; regression >10% blocks merge |

## 2. Harness protocols (rf-harness)

- **$6000 protocol (NES/blargg)**: run headless N frames, poll $6000
  (0x80=running, 0x00=pass, else fail code), read message text at $6004+.
  Timeout = declared frame budget per ROM ⇒ fail.
- **Golden-frame**: run to frame N (manifest-declared), hash framebuffer
  (indexed buffer, pre-shader — host GPU never affects goldens), compare
  SHA-256; on mismatch, dump PNG pair + diff heatmap as CI artifacts.
- **Audio check**: capture ring-buffer output, compare per-channel RMS
  envelope against known-good recording (blargg `apu_mixer`); exact
  sample-hash for S-DSP BRR cases.
- **Replay check**: play `.rfreplay`, assert periodic + final state hashes;
  on divergence report first divergent frame (hashes every N frames make
  bisection O(log) by re-run).

## 3. Test-ROM acquisition (never committed — NFR-006)

`tests/rom-manifest.toml` lists every external artifact: URL, SHA-256,
license note, unpack path. `scripts/fetch-test-roms.sh` downloads into
gitignored `roms/`, verifies hashes, and is idempotent; CI caches by
manifest hash. Sources: christopherpow/nes-test-roms, SingleStepTests repos,
gilyon/snes-tests releases, PeterLemon/SNES, undisbeliever/snes-test-roms.
**`[[git_artifact]]` (ticket W0-07):** a second manifest table for sources
that are naturally a pinned commit + sparse subpath of a git repo rather
than one fetchable file (e.g. SingleStepTests' per-opcode vector JSON
directories) — integrity comes from the commit SHA itself
(`git rev-parse HEAD` after a `--filter=blob:none` sparse checkout), not a
sha256, since a commit SHA already is a content hash and archive zips of
these repos are not byte-stable across requests (see
`crates/rf-harness/src/manifest.rs` module doc). §4 below records the
protocol this replaced for `nes6502`.
**Fixture doctrine (D-001, 2026-07-15):** game-shaped fixtures the project
demos or gates on are self-contained — in-repo source (RF-Scroller under
`fixtures/`, cc65/libSFX-built in CI, CC0/MIT assets), never third-party
content. Accuracy oracles above stay external fetch-only. Alter Ego (PD) is
the one third-party smoke fixture (independent proof), fetched by manifest.
**No-vendor/no-rehost rule (design review G-43):** sources with no license
grant (SingleStepTests/65816, PeterLemon/SNES, christopherpow/nes-test-roms,
nestest/.log) are fetch-from-origin only — never vendored, mirrored, or
re-hosted; the CI cache is the only tolerated copy. Manifest entries record
each artifact's license status; upstream grant requests tracked in
docs/PREREQUISITES.md.

## 4. NES CI gates

Tier A = every PR; Tier B = nightly (slow or visual-manual-once suites).
**Tier A-local (tickets W0-07, W1-03, W1-05b):** a suite whose full
100%-conformance run is too heavy — or whose fixture is a gitignored
fetched artifact CI never has (NFR-006) — to repeat on every PR (the
nes6502 vector suite is 2,560,000 cases across all 256 opcodes; nestest is
8991 golden-log lines against a fetched ROM+log pair) is verified locally
instead — see "Local evidence gate" below — and CI checks the resulting
evidence file rather than re-running the suite. It is still a hard release
gate, not an optional one: it is just verified on a different
cadence/machine than Tier A/B.

| Suite | Verifies | SRS | Tier | Pass criteria |
|---|---|---|---|---|
| SingleStepTests `nes6502` | per-opcode state + bus cycles | FR-CORE-020 | A-local | 100% of all 256 opcodes (151 official + 105 unofficial/illegal) — 2,560,000 cases |
| nestest + `nestest.log` | whole-CPU conformance (register/CYC + disassembly) | FR-CORE-021 | A-local | byte-exact trace diff empty over all 8991 lines |
| blargg `instr_test-v5` | official+unofficial instructions | FR-CORE-020 | A | $6000 = 0 all ROMs |
| `cpu_timing_test6`, `instr_timing`, `branch_timing_tests` | cycle counts, page-cross, branches | FR-CORE-020 | A | $6000 = 0 |
| `cpu_interrupts_v2` | NMI/IRQ timing, hijacking | FR-CORE-022 | A | $6000 = 0 |
| `cpu_dummy_reads/writes`, `cpu_exec_space` | dummy bus cycles, open bus | FR-CORE-020 | B | $6000 = 0 |
| blargg `ppu_vbl_nmi` (10 sub-ROMs, `rom_singles/`) | VBL/NMI to the PPU cycle | FR-CORE-022 | A-local | $6000 = 0 — 4/10 clean (`01`,`03`,`04`,`09`); 6/10 (`02`,`05`,`06`,`07`,`08`,`10`) waived, sub-CPU-cycle timing ceiling, see `crate::ppu`'s module doc + `crates/rf-harness/waivers.toml` |
| `sprite_hit_tests` | sprite-0 hit | FR-CORE-023 | A-local | RAM-result byte (`$00F8`) = 1 — NOT `$6000` (ticket W1-05b correction; this ROM generation predates blargg's `$6000` runtime, see `tests/rom-manifest.toml`'s comment on this suite) |
| `sprite_overflow_tests` | overflow bug | FR-CORE-023 | A | $6000 = 0 — **unverified as of W1-05b**: shares `sprite_hit_tests`' pre-`$6000` ROM family and almost certainly has the same protocol mistag; out of this ticket's scope, flagged in `tests/rom-manifest.toml` for whichever ticket implements this suite |
| `oam_read`, `oam_stress` | $2004 semantics | FR-CORE-023 | B | $6000 = 0 |
| `full_palette`, `ppu_open_bus`, `ppu_read_buffer` | palette, open bus, $2007 buffer | FR-CORE-022 | B | golden frame / $6000 |
| blargg `apu_test`, `apu_reset`, `dmc_dma_during_read4` | frame counter, IRQ, DMC DMA | FR-CORE-024 | A | $6000 = 0 |
| blargg `apu_mixer` | non-linear mixer levels | FR-CORE-024 | B | RMS envelope match |
| `mmc3_test_2` + IRQ tests | MMC3 A12 IRQ counter | FR-CORE-025 | A | $6000 = 0 |
| Holy Diver Batman (28 ROMs) | mapper acid breadth | FR-CORE-025 | B | golden frame per ROM |
| RF-Scroller 5-min replay | real-game regression (in-repo fixture, D-001) | FR-CORE-026 | A | final-hash + 6 golden frames |
| Alter Ego 5-min replay | independent-proof regression (PD fixture) | FR-CORE-026 | A | final-hash + golden frames |

**Local evidence gate (tickets W0-07, W1-03, W1-05b):** `nes6502` is the
first Tier A-local suite; `nestest` (ticket W1-03) is the second;
`ppu_vbl_nmi` and `sprite_hit_tests` (ticket W1-05b) are the third and
fourth — both real, fetched, gitignored ROM sets, unrunnable in CI the same
way. Protocol:

1. Vectors are fetched once via `scripts/fetch-test-roms.sh
   singlestep-nes6502-src` (a `[[git_artifact]]` manifest entry, §3) into
   gitignored `roms/nes/singlestep-nes6502-src/nes6502/v1` — never
   committed (NFR-006). The nestest ROM/log are fetched via
   `scripts/fetch-test-roms.sh nestest-rom nestest-log` into gitignored
   `roms/nes/other/{nestest.nes,nestest.log}` — also never committed. The
   10 `ppu_vbl_nmi` singles and 11 `sprite_hit_tests` ROMs fetch the same
   way (`scripts/fetch-test-roms.sh` with no arguments fetches everything,
   including these) into `roms/nes/ppu_vbl_nmi/rom_singles/` and
   `roms/nes/sprite_hit_tests_2005.10.05/`.
2. `scripts/local-gate.sh` runs all four suites locally (via
   `crates/rf-harness/src/bin/local_gate_evidence.rs`, which depends on
   `rf-nes` directly — `scripts/validate-arch.sh` exempts "the test
   harness" from the cores-via-`rf-core-api`-only rule) and writes the
   combined result to the single rolling file `docs/evidence/local-gate.json`
   (committed; git history is the audit trail) — the current retroforge
   commit, toolchain version, the vector source's own commit SHA, the
   nestest lines-compared/lines-matched/first-divergence, per-ROM
   pass/fail for `ppu_vbl_nmi`/`sprite_hit_tests` (via
   `rf_harness::blargg_evidence::run`/`run_ram_result`, which drive a real
   `NesBus`+`Cpu` rather than the `run_blargg_protocol`/`EmulatorCore`-mock
   path `blargg.rs` was built against — no `EmulatorCore` impl exists yet),
   and the W0-03 accuracy-table rows (raw/effective pass-fail, one row per
   ROM for the two new suites — see `docs/TESTING.md` §3's note on splitting
   bundled blargg programs into one `[[suite]]` row each) for all four
   suites.
3. `node scripts/validate-evidence.mjs` — cheap (reads a few KB of JSON,
   no ROM/vector/log bytes) — runs in CI on every PR and fails the build
   if: evidence is missing for a Tier A-local suite; the working tree was
   dirty when the evidence was generated; the per-ROM counts don't match
   the expected 10/11; or the evidence is **stale** (a later commit touches
   that suite's covered paths — `crates/rf-nes/src/cpu` for `nes6502`;
   `crates/rf-nes/src/cpu`, `crates/rf-nes/src/system`, and
   `crates/rf-nes/src/trace.rs` for `nestest`; `crates/rf-nes/src/ppu`,
   `crates/rf-nes/src/system`, and `crates/rf-nes/src/cpu` for
   `ppu_vbl_nmi`; `crates/rf-nes/src/ppu` and `crates/rf-nes/src/system`
   for `sprite_hit_tests` — than the commit the evidence was generated
   against — checked via `git rev-list`/`git merge-base --is-ancestor` on
   FULL history, never timestamps: mtimes aren't in git and committer dates
   are rewritable/non-monotonic across merges). CI never fetches any of
   these ROMs/vectors/logs itself — that cost stays local-only, which is
   the entire point of this tier.

**`ppu_vbl_nmi`/`sprite_hit_tests` findings (ticket W1-05b):**
- `sprite_hit_tests_2005.10.05` predates blargg's shared `$6000`/`$6004`
  runtime; it reports results via an on-screen text console + beep count
  (`readme.txt`, verbatim: "reports the result on screen and by beeping a
  number of times") and a RAM-resident result byte at `$00F8`
  (`source/runtime/validation.a`: `result = $f8`) instead. All 11 ROMs pass
  against this crate's real sprite-0-hit implementation once read via the
  correct protocol.
- Six of the ten real `ppu_vbl_nmi` sub-ROMs (`02-vbl_set_time`,
  `05-nmi_timing`, `06-suppression`, `07-nmi_on_timing`,
  `08-nmi_off_timing`, `10-even_odd_timing`) fail against a sub-CPU-cycle
  timing ceiling this crate's lock-step tick-after-register-access
  architecture cannot reach without CPU-crate changes — see `crate::ppu`'s
  module doc "Scope fence" section for the full derivation (cross-checked
  against the real fetched ROMs' own expected tables and an independently
  reported matching symptom on forums.nesdev.org) and
  `crates/rf-harness/waivers.toml` for the tracked, ticketed waivers.

**nestest disassembly-column finding (ticket W1-03):** both halves of the
trace are byte-exact over all 8991 lines, but the disassembly-annotation
half (`FR-DBG-003`) needed one narrow fix beyond straightforward peeking:
nestest.log's handful of `STA` lines targeting write-only/internal APU
registers (`$4004`-`$4007`, `$4015`) disassemble as `= FF` regardless of
actual prior bus traffic — `NesBus::peek` (the disassembly-only,
side-effect-free read) returns a fixed `$FF` for the whole
`$4000-$4015`/`$4018-$401F` stub range rather than the tracked `open_bus`
latch `read_untimed` (the real emulation path) still uses; see that
method's doc comment for the full reasoning (nesdev.org/wiki/APU: `$4015`
reads disconnect the external bus entirely, so `open_bus` passthrough was
never correct there regardless). This is a disassembly-display convention,
not a change to real emulated open-bus behavior.

## 5. SNES CI gates

| Suite | Verifies | SRS | Tier | Pass criteria |
|---|---|---|---|---|
| SingleStepTests `65816` | CPU vectors | FR-CORE-030 | A | 100% ops |
| SingleStepTests `spc700` | SPC vectors | FR-CORE-031 | A | 100% ops |
| gilyon/snes-tests `cputest` | on-console CPU behavior | FR-CORE-030 | A | RAM result block matches shipped `tests.txt` |
| gilyon `spctest` | SPC on-console | FR-CORE-031 | A | same |
| undisbeliever DMA/HDMA ROMs | DMA/HDMA edges | FR-CORE-032 | A | golden frame |
| PeterLemon PPU per-feature (modes 0-6, windows, mosaic, color math) | PPU features | FR-CORE-033 | A | golden frame (screen shows computed-vs-expected) |
| PeterLemon Mode 7 set | Mode 7 + HDMA perspective | FR-CORE-034 | A | golden frame |
| libSFX-built LoROM/HiROM mirror-map fixtures (ours) | cartridge address mapping incl. mirrors/banks | FR-CORE-035 | A | golden RAM result block |
| blargg SPC timing (higan mirror) | S-SMP/S-DSP timing | FR-CORE-036 | B | $-protocol / audio hash |
| RF-Scroller-S 5-min replay | real-game regression (in-repo fixture, D-001) | FR-CORE-037 | A | final-hash + goldens |

Note (research-verified): neither bsnes nor Mesen2 publishes a golden-frame
CI — this harness is our own build, and it doubles as the accuracy-table
generator (TASVideos-style) for release notes.

**Accuracy table + waivers (R-C1/R-D4):** the harness emits a
machine-readable table (suite × ROM × pass/fail/frame JSON) per run. Raw
and effective counts are reported separately: known-fails live in an
explicit waiver file carrying justification + expiry date; an expired
waiver reopens red; a red row with no open ticket fails the report step
(suite→FR→ticket mapping is a lookup from the tables above, never a
judgment call).

## 6. Determinism, state, and mode-invariant suites

| Test | Assertion | SRS |
|---|---|---|
| Double-run | run N frames twice from same state + input log ⇒ per-frame state hashes identical (suite: `crates/retroforge/tests/determinism.rs`, not `rf-harness` — `rf-nes` has no `EmulatorCore` impl yet for `rf-harness` to drive, ticket W1-07) | FR-CORE-002 |
| Replay determinism | every test-ROM run is recorded and replayed; final hash equal (suite: `crates/retroforge/tests/determinism.rs` + `rf-input`, not `rf-harness` — same reason, ticket W1-07) | FR-STATE-006 |
| State roundtrip | at frames {60, 600, 3600}: save → load → run 600 more ⇒ hash equals uninterrupted run | FR-STATE-002 |
| Cross-mode state | Enhanced-mode state loads in Accuracy config (enhancement chunks skipped) and continues hash-identical | FR-STATE-007 |
| Golden fixtures | every released `.rfstate`/`.rfreplay` fixture loads on current main | FR-STATE-005 |
| **Mode invariant** | same ROM + log run in Accuracy and Enhanced (all features on) ⇒ identical core hashes every frame | FR-MODE-002 |
| Wrong-ROM refusal | state with mismatched normalized hash refused with diagnostic | FR-STATE-003 |

## 7. Enhancement feature tests

- **Anti-flicker goldens**: RF-Scroller scenes engineered to overflow sprite
  limits (fixture doctrine D-001 — the fixture is the red-fixture host): (a)
  limit-bypass shows all sprites, (b) temporal mode reconstructs
  software-culled rotation, (c) intentional-blink case is respected, (d)
  sprite-0-hit ROM still passes with bypass on.
- **Stitcher**: deterministic canvas — same replay ⇒ byte-identical canvas;
  HUD band exclusion on a scroll-split fixture; scene-change spawns new
  canvas.
- **Profile validation**: `retroforge-tool profile validate` over `/profiles`
  in CI — schema, provenance (`source` required), no binary assets outside
  licensed homebrew dirs (FR-PROF-003/006).
- **Decoder goldens**: RF-Scroller level decode output (chunk grid PNG +
  collision map) hashed against goldens; re-decode determinism.
- **Plugin containment**: Lua script that errors every frame ⇒ script paused,
  emulation unaffected; over-budget plugin throttled (FR-PLUG-004/005).
- **Red-fixture rule (FR-ENH-013, D-004)**: every shipped heuristic has a
  fixture scene/ROM that MUST trigger it; CI fails when it stops firing.
  The anti-flicker cases above are instances; the rule is general.
- **Path containment (NFR-010)**: symlink-escape attempts on plugin
  cache_dir, profiles.d references, and library scan roots are refused;
  scan survives a symlink loop (unit tests per surface).

## 8. Phase exit gates (roadmap enforcement)

| Phase | Exit = all of |
|---|---|
| 1 (NES MVP) | nes6502 vectors 100% official · nestest diff empty · instr_test-v5 pass · NROM boots 2 homebrew titles · double-run + roundtrip + replay suites green |
| 2 (NES compat) | Tier-A NES table fully green · mapper set complete · battery saves · RF-Scroller + Alter Ego replays green |
| 3 (renderer) | golden frames render identically through wgpu original pipeline (pre-shader hash unchanged) · fallback test green |
| 4 (enhancement fw) | mode invariant green with runtime subscribed · overlay + profile-load demos · plugin containment tests |
| 5 (game-aware) | RF-Scroller full-level decode goldens · E6-S1 acceptance demo recorded |
| 6 (SNES MVP) | 65816 + spc700 vectors 100% · gilyon cputest/spctest green · LoROM homebrew boots |
| 7 (SNES compat) | Tier-A SNES table fully green · RF-Scroller-S replay green |
| 8 (adv. enhance) | widescreen/fast-load cases green · rewind memory budget documented |
| 9 (ecosystem) | profile/plugin CI checks green on example third-party submissions |

## 9. Performance benchmarks

criterion benches per core (`ms/frame`, Accuracy config, fixed replay
workload) and per enhancement stage (de-flicker, stitcher, decoder, compose).
Nightly CI compares against `benches/baseline.json`; >10% regression fails
the run and blocks merge until re-baselined with justification in the PR.
NFR-002 targets (NES ≤2 ms, SNES ≤8 ms per frame) are re-baselined after
Phase 1/6 measurements — treat as budgets, not guesses, thereafter.
