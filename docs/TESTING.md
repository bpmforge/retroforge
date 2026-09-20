# RetroForge — Test Strategy

Status: Phase 2 baseline · 2026-07-06 · CI banner added 2026-08-30
Sources: `docs/research/accuracy-and-testing.md` (suite provenance + URLs),
SRS verification column, `docs/design/SAVE_STATES.md`.

## 0. Where these gates actually run (read before trusting a "CI" label)

**There is no CI.** Hosted GitHub Actions has rejected every run since
~2026-08-07 on the Actions budget, and Brad ruled 2026-08-22 that the
budget is not being increased — so this is permanent policy, not a dip
(`CLAUDE.md` → Build). **What runs instead is a self-hosted runner on
Brad's darwin workstation** (ticket W12-01, green 2026-09-15): runner
`rf-darwin`, a launchd LaunchAgent installed from `~/actions-runner`,
labels `self-hosted, macOS, ARM64`, driving
`.github/workflows/gate-darwin.yml`. GitHub does not bill self-hosted
minutes and the Actions budget does not gate scheduling onto it (probed
2026-08-31, docs/STATUS.md). First green run: run 35012699193 on
bb21d05, every step successful, 4m28s. Gitea Actions on `origin` remains
open and not set up.

The darwin gate is bounded against law 8 by design, not by default:
push-to-main only (never `pull_request`), `cancel-in-progress`
concurrency, a 60-minute job timeout, and the tools step **verifies and
never installs**. **The heavy suites do not run unattended.** The 10k
determinism double-run, the 5k mode-invariant boundary, debugger idle
cost, the un-profiled replay and the UI smoke flow live in a separate
`heavy` job that only `workflow_dispatch` starts, with its own 90-minute
cap: one click, attended, on the machine RF-L-09 happened on.

Read every "every PR", "nightly", "Tier A/B" and "in CI" phrase in this
document as **which gate a suite belongs to**, not as a claim that
something ran. What actually executes today is:

- law 3's four commands — `cargo fmt --check`, `cargo clippy --workspace
  -- -D warnings`, `cargo test --workspace`, `scripts/validate-arch.sh`;
- `scripts/local-gate.sh` (adds the fetched-corpus suites and the
  evidence rows of §4) and `scripts/docs-gate.sh` (doc samples,
  plugin-SDK examples, profile validation, `mdbook build`);
- `node scripts/validate-plan.mjs` and `node scripts/validate-traceability.mjs`.

All of it runs on **one darwin machine**, and the runner is that same
machine. Of the three things hosted CI used to cover, the darwin runner
restores exactly one: the **cc65 deterministic fixture rebuild** of
RF-Scroller / RF-Scroller-S / mirror-maps (brew cc65, every push). The
other two run nowhere and must be described as unverified wherever they
are claimed: **Linux**, and the **software-rasterizer GPU path**
(`LIBGL_ALWAYS_SOFTWARE=1` on llvmpipe, versus Metal on the runner).
`ci.yml` stays in the tree unedited as the Linux definition. A red
*hosted* run on GitHub is still not a signal; a red **gate-darwin** run
is, and it has already been right three times (§4's evidence file stale
after W13-02e, and an NCSA licence in the dev graph since W10-03, both
2026-09-15).

`docs/SRS.md`'s `Verification` column still uses `CI` as a *class* of
verification for 22 requirements. That naming was left alone deliberately
— it describes the kind of check, not where it ran — and this section is
what qualifies it.

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
- **Audio check**: capture the core's sample stream through the app's own
  output-stage filters and compare the RMS envelope (blargg `apu_mixer`);
  exact sample-hash for S-DSP BRR cases. **Corrected 2026-08-16 (W2-05):**
  the "known-good recording" this line originally named does not exist and
  is not needed — `apu_mixer`'s ROMs cancel their own tone against the DMC
  DAC, so near-silence between their beeps IS the reference, and it is a
  stronger one (a recording would also encode this engine's filtering,
  decimation and volume scaling).
- **Tier-B nightly** (ticket W2-09): `.github/workflows/nightly.yml` runs
  `cargo run -p rf-harness --bin tier_b_suites`, which selects Tier-B suites
  from `tests/rom-manifest.toml`'s own `tier` field (never a second list
  that could disagree with §4's table) and scores them through the `$6000`
  protocol. Known-fails go through the SAME `crates/rf-harness/waivers.toml`
  the Tier-A accuracy report uses — justification, expiry, open ticket — and
  the runner **fails on an expired waiver and on a waiver covering a ROM
  that has started passing** (a stale waiver hides the next regression).
  Unfetched ROMs are counted as skipped, never as passes, and a run where
  nothing executed is a failure rather than a green.
- **Accuracy-vs-Compatibility diff** (ticket W3-07; FR-MODE-004,
  `docs/design/EMULATION_CORES.md` §5's "CI diffs both"):
  `cargo run -p rf-harness --bin mode_diff` runs every executed ROM under
  both `CoreConfig` modes and compares verdict *and* message. It fails in
  **two** directions, and the second is the one that matters: an
  **undeclared** divergence fails (compatibility may only differ where
  someone wrote down that it may), and a **declared divergence that stops
  happening** also fails. §5 says "divergences must be test-suite-visible,
  or the switch doesn't exist" — so a run in which the two configs agree
  everywhere means the switch has become a no-op, and reporting that as
  green would be indistinguishable from reporting it as correct. Same
  shape as `tier_b_suites`' stale-waiver rule. One switch exists today
  (§5's "Open-bus modeling: full | simplified"), so one divergence is
  declared: `ppu_open_bus` passes under Accuracy and fails its decay
  sub-tests under Compatibility. Both failure directions are unit-tested
  without ROMs, and both were demonstrated against the real suite by
  mutation — breaking the compat path elsewhere and making the switch a
  no-op each exit 1, while the baseline exits 0. **Ticket W3-07b added a
  second switch, the PPU catch-up scheduler**, and it is held to a
  stricter contract: §5 rates its "risk of compat setting" as "none if
  catch-up correct", so it must produce NO divergence. It does — the
  diff still reports exactly the one open-bus divergence over 52 ROMs.
- **PPU catch-up scheduler** (ticket W3-07b; EMULATION_CORES §5 row 1).
  Compatibility advances provably-inert dot runs in one arithmetic step
  instead of processing them; Accuracy is untouched lock-step. Because
  CI has no ROMs (NFR-006), the equivalence property is *also* checked
  hermetically in `crate::system::tests::catch_up`, comparing every
  save-state region after 12 frames of a rendering-on and a
  rendering-off workload, and the predicate's structural rules are
  enumerated exhaustively over all 262x341 positions in
  `crate::ppu::tests::catch_up`.

  **What it buys, measured on `machine_frame` (Apple M-series, release):**

  | workload | Accuracy | Compatibility | |
  |---|---|---|---|
  | rendering **off** | 732 us/frame | 693 us/frame | **5.4% faster** |
  | rendering **on** | 1.124 ms/frame | 1.116 ms/frame | parity (inside noise) |

  The gap is structural, not incidental: with rendering off ~31% of a
  frame's dots are inert, with rendering on only ~8% are, and the inert
  ones are the cheapest dots in the frame. Measured over `mmc3_test_2`
  (26-31%), `cpu_timing_test6` (13%) and synthetic workloads (31% / 8%).
  **Most of the blargg suite runs with rendering off**, so quoting the
  suite's number alone would overstate what this buys a real game by
  about four times. The accuracy path was measured before and after and
  is unchanged within run-to-run noise (~3% on this machine); the mode
  check is hoisted out of `tick_master`'s per-cycle loop so the accuracy
  arm is byte-for-byte the loop that preceded the ticket.
- **Criterion regression gate** (W2-09): `benches/baseline.json` +
  `scripts/bench-compare.mjs`, >10% fails (§9), with NFR-002's absolute
  ms/frame budget checked separately from the relative threshold. R-C2 is
  enforced mechanically: a baseline whose numbers move without its `meta`
  moving is refused, checked against the committed file via `git show`. The
  gate has its own test suite (`scripts/bench-compare.test.mjs`) which the
  nightly runs FIRST — a threshold script that has silently stopped
  enforcing anything passes either way.
- **Audio underrun soak**: five minutes of device playback with zero
  silence-filled callbacks (`crates/retroforge/tests/audio_soak.rs`).
  Device-gated and `#[ignore]`d — CI has no sound card — so it is run
  deliberately, and the headless pieces it is built from (ring, rate loop,
  resampler, filters) run on every commit.
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
content. Accuracy oracles above stay external fetch-only. Alter Ego
(freeware — **D-008**, not PD as D-001 originally said) is the one
third-party smoke fixture (independent proof), fetched by manifest.
**No-vendor/no-rehost rule (design review G-43):** sources with no license
grant (SingleStepTests/65816, PeterLemon/SNES, christopherpow/nes-test-roms,
nestest/.log, **and Alter Ego since D-008**) are fetch-from-origin only —
never vendored, mirrored, or re-hosted; the CI cache is the only tolerated
copy. Manifest entries record each artifact's license status; upstream grant
requests tracked in docs/PREREQUISITES.md. Note this rule is **documented,
not mechanically enforced**: `license_status` is parsed and carried by
`rf_harness::manifest` but nothing gates on it, so the guard against an
accidental vendoring is `/roms/` being gitignored (NFR-006) plus review —
worth knowing before trusting the field to stop anything by itself.

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

**"NOT WIRED" in the Tier column means the ROMs are fetched by the manifest
but executed by NO code path** — the tier is the intended gate, not the
current one. Flagged 2026-08-05 by the Phase 1 exit gate's challenger, which
found `instr_test-v5` labelled Tier A ("every PR") while nothing ran it: a
downloaded ROM is not a tested ROM, and a tier label that overstates
coverage is worse than an honest gap.

**W2-12 closed that gap for the CPU suites (2026-08-16)**, and doing so
produced two findings worth reading before trusting any tier label:

1. **A wrongly-wired ROM is worse than an unwired one.** `branch_timing_tests`
   was tagged `six_thousand`; under that protocol all three ROMs report
   "signature never became valid", which is indistinguishable from a hang
   and would have been recorded as three failures. They actually **pass** —
   they are 2005-era RAM-result ROMs like `sprite_hit_tests`. `cpu_timing_test6`
   was mistagged the same way and is screen-only, so it needed a new
   `screen_text` protocol to be scored at all rather than looking hung.
2. **Passing 2.56M opcode vectors and a byte-exact nestest trace does not
   mean the CPU is right.** `cpu_interrupts_v2` failed on an NMI arriving
   during BRK and `cpu_timing_test6` on opcode `$00`. Per-opcode vectors
   never deliver an interrupt mid-instruction and nestest's trace never
   takes one, so the gap was structurally invisible to both.
   **W2-20 resolved it (2026-08-16)** and the shared-cause hypothesis held:
   `FAIL OP :$00` was never a BRK *timing* bug. Two real defects, both in
   interrupt handling — the `BRK`/`IRQ` hijack tested the NMI **level**
   (which the NES holds asserted for all of vblank, so a serviced edge kept
   hijacking), and interrupt-entry sequences **polled**, which nesdev says
   they do not. Both ROMs now pass.
3. **Fixing a masking failure exposes the next one, and that is the tier
   working.** With `2-nmi_and_brk` passing, `cpu_interrupts_v2` moved on to
   failing `3-nmi_and_irq` — an APU frame-counter IRQ timing gap that had
   been sitting behind it. W2-21 owns that; it is not a CPU defect.
4. **Two oracles can disagree, and the honest answer is to say which one
   this project follows.** `instr_test-v5`'s `AB ATX #n` failure was
   `LXA`'s magic constant, one of the genuinely *unstable* illegal opcodes
   (an analog artifact varying by chip and temperature). blargg checksums
   against `$FF`; SingleStepTests' vectors are generated against `$EE`; no
   value satisfies both. Measured both ways rather than argued — `$FF`
   gives `instr_test-v5` 16/16 with exactly one of 105 unofficial opcodes
   failing nes6502, `$EE` the reverse. `$FF` is implemented, because
   `docs/MVP.md` §3 makes `instr_test-v5` an acceptance item while scoping
   its vector requirement to "100% **official** opcodes". The cost is one
   named exception in the unofficial vector suite, carrying a guard that
   fails if `$AB` ever starts passing.

**W2-01d finished `dmc_dma_during_read4` 5/5 and closed wave 2
(2026-08-17)**, with the same shape as W2-01c one day earlier: **a second
access on a back-to-back cycle reports a held value, not a fresh one.**
The ROM provokes it with `lda $20F7,x`, `x = $10` — `$20F7 + $10 = $2107`
crosses a page, so the 6502 issues its dummy read at `$2007` and the real
read at `$2107` (also `$2007` after mirroring) on consecutive cycles. The
second reports what the first already presented while the buffer and `v`
advance twice.

Worth generalising, carefully: these two are the same *class* — a device
that cannot respond twice in consecutive cycles — but the controller
suppresses its shift while the PPU does not suppress its side effects,
only the reported value. **Don't assume a third instance holds the same
half.**

**W2-01c: the ROM was counting something other than what everyone assumed
(2026-08-17).** `dma_4016_read` had resisted a whole ticket's worth of
DMA-model work, and its own notes said the fix needed a model of *which
address* the halted 6502 holds on each no-operation cycle. It did not.
The ROM's `end:` routine counts how many reads it takes the pad to return
1 — so its expected `08 08 07 08 08` is measuring shifted **bits**, and
nesdev's "1 or 3 extra **reads**" is measuring bus cycles. Both are true
at once.

A standard controller's shift register is clocked by the **edge** of the
read strobe. The three back-to-back `$4016` reads a DMC halt puts on the
bus hold one strobe asserted — one rising edge, one extra bit — and the
DMA's own get (`$C000`) breaks the run so the CPU's resumed read supplies
the second. That gives exactly one extra bit for *either* alignment, which
is why the answer never depended on get/put parity, the one thing the
ticket explicitly forbade tuning.

Two models were measured against each other rather than argued: the
alternative (halt cycle is the CPU's own read, only later cycles repeat)
produces `08 08 06 08 08` **and** regresses `dma_2007_read` off its
documented CRC. Worth generalising: **when a ROM's expected output has
resisted several plausible models, read what its scoring routine actually
counts before building a sixth.**

**`crates/rf-harness/waivers.toml` is now EMPTY of waivers (2026-08-17).**
All ten ever raised — seven Tier-B by W2-09, three Tier-A CPU by W2-12 —
were deleted because the ROM passes, never re-dated. That is the state the
waiver mechanism exists to make reachable, and it is also the most fragile
state: the moment something legitimately needs waiving, it gets an entry
with a justification, an expiry and an open ticket. Emptiness is not a
target to protect by looking away from a red ROM.

**W2-21 finished `cpu_interrupts_v2` (2026-08-17)**, and what it took is
the point: after W2-20 fixed two interrupt bugs, three more *independent
one-cycle facts* stood between this engine and that one ROM.

1. **The CPU sees the APU's IRQ one cycle after the APU raises it.** The
   PPU's NMI path already modelled exactly this (`NesBus::nmi_level_latch`);
   the APU had no equivalent. Deliberately NOT fixed by moving the frame
   counter — setting its reset delay to 4/5 instead of 3/4 makes the same
   ROM pass and would break `apu_test/6-irq_flag_timing`, which pins the
   flag to "29831 clocks after writing $00 to $4017". The flag is set when
   the wiki says; the CPU's *view of the line* is what lags.
2. **The OAM-DMA get/put phase was backwards.** nesdev says the power-on
   phase is random, so this crate picked one and documented the choice —
   `4-irq_and_dma` pins it against hardware. Under the old phase the DMA
   ran one cycle short and the ROM's `8`/`9` boundary landed at `+526`
   where hardware puts it at `+527`, with all 528 other rows matching. An
   arbitrary choice resolved by measurement, not a rule changed.
3. **A taken, non-page-crossing branch polls interrupts a cycle earlier
   than everything else.** `5-branch_delays_irq` states the rule and marks
   the single row that proves it with `*** This is the special case`.

Lesson worth keeping: **each of these was invisible until the one before
it was fixed.** A multi-sub-test ROM reports only its first failure, so
"one ROM red" was never one bug — and the frozen replay goldens moved on
the DMA change, caught only because they were run explicitly (`cargo test
--workspace` reports them as ignored).

**W2-19 cleared the whole Tier-B waiver set (2026-08-16)** — all seven
entries deleted rather than re-dated, and the nightly runs green with none.
Four findings from it are worth carrying forward:

1. **The PPU has its own open bus, and it is not the CPU's.** blargg's
   `ppu_open_bus` readme says so in its first paragraph; this crate had
   only the CPU-side latch. Adding the PPU's decay register fixed FOUR
   ROMs at once (`ppu_open_bus`, both `cpu_exec_space` arms,
   `cpu_dummy_writes_ppumem`) — the ticket predicted three of them.
2. **A never-strobed controller is not an exhausted one.**
   `Controller::default` started with the shift register empty, so the
   first `$4016` read of a session returned 1. `cpu_exec_space`'s APU test
   catches this because it executes code from all 256 addresses in
   `$4000-$40FF`, and one wrong bit turns the `RTI` it relies on into a
   two-byte opcode.
3. **Two budgets were genuinely too small — proven, not assumed.** The
   ticket forbade raising a budget until a ROM went green, so completion
   frames were bisected first: `ppu_read_buffer` needs ~1300 against a
   budget of 600 (and says "This next test will take a while" on screen
   while it works), `oam_stress` ~3620 against 3600 — **short by twenty
   frames**, which is why it reported a partial result instead of an
   obvious timeout.
4. **A third protocol mistag.** `cpu_dummy_reads` never writes the `$6000`
   signature at all; it is screen-only, and under the `$6000` protocol it
   read as "never reached its test-status init" and was waived as a hang.
   It passes, in under 200 frames. That is the same class as
   `branch_timing_tests` and `cpu_timing_test6` (W2-12) — three now — so
   treat an unexplained "signature never valid" as a suspected mistag
   before treating it as a bug. The Tier-B runner gained a `screen_text`
   path so fixing the tag did not simply move the ROM to the skip list.

Executed today: `nes6502`, `nestest`, `sprite_hit_tests`, `ppu_vbl_nmi`,
`apu_test`, `instr_test-v5`, `instr_timing`, `branch_timing_tests`,
`cpu_timing_test6`, `cpu_interrupts_v2` (A-local, all in
`docs/evidence/local-gate.json`), plus the Tier-B set W2-09 wired into the
nightly. Still unwired: `sprite_overflow_tests` (same suspected protocol
mistag, flagged since W1-05b and deliberately NOT absorbed here) and
`apu_reset` (needs an `Apu::reset` path — W2-01a's handoff).

| Suite | Verifies | SRS | Tier | Pass criteria |
|---|---|---|---|---|
| SingleStepTests `nes6502` | per-opcode state + bus cycles | FR-CORE-020 | A-local | 100% of all 256 opcodes (151 official + 105 unofficial/illegal) — 2,560,000 cases |
| nestest + `nestest.log` | whole-CPU conformance (register/CYC + disassembly) | FR-CORE-021 | A-local | byte-exact trace diff empty over all 8991 lines |
| blargg `instr_test-v5` | official+unofficial instructions | FR-CORE-020 | A-local — **wired (W2-12), PASSES 16/16 as of W2-20** (its `AB ATX #n` failure was `LXA`'s magic constant; see below) | $6000 = 0 all ROMs |
| `instr_timing` | instruction cycle counts | FR-CORE-020 | A-local — **wired (W2-12), PASSES** | $6000 = 0 |
| `branch_timing_tests` (3 ROMs) | branch timing, page-cross | FR-CORE-020 | A-local — **wired (W2-12), 3/3 PASS** | **NOT `$6000`**: 2005-era ROMs like `sprite_hit_tests`, scored on the RAM-result byte at `$00F8`. Under the `six_thousand` tag the manifest used to carry, all three reported "signature never became valid" — a false hang, and three false failures. Corrected in `tests/rom-manifest.toml` |
| `cpu_timing_test6` | official-instruction cycle counts | FR-CORE-020 | A-local — **wired (W2-12), PASSES as of W2-20** — its `FAIL OP :$00` was never a BRK *timing* bug, it was the interrupt poll | **NOT `$6000`**: verdict is only on the nametable, scored by the new `screen_text` protocol using blargg's own rule ("if a test prints 'passed', it passed") |
| `cpu_interrupts_v2` | NMI/IRQ timing, hijacking | FR-CORE-022 | A-local — **5/5 as of W2-21**; took three tickets and five distinct one-cycle facts to get there (W2-12 wired it, W2-20 fixed the hijack and the interrupt-entry poll, W2-21 the APU IRQ lag, the OAM-DMA phase and the taken-branch poll) | $6000 = 0 |
| `cpu_dummy_reads/writes`, `cpu_exec_space` | dummy bus cycles, open bus | FR-CORE-020 | B — **5/5 as of W2-19** (was 1/5). `cpu_dummy_reads` is **`screen_text`**, not `$6000` — it was mistagged, which is why it looked like a hang | $6000 = 0, except `cpu_dummy_reads` (screen) |
| blargg `ppu_vbl_nmi` (10 sub-ROMs, `rom_singles/`) | VBL/NMI to the PPU cycle | FR-CORE-022 | A-local | $6000 = 0 — **10/10 clean, no waiver** (W1-05c took it 4→9, W1-05d closed `10-even_odd_timing`; see `crate::ppu::Ppu::render_enable_pipe`) |
| `sprite_hit_tests` | sprite-0 hit | FR-CORE-023 | A-local | RAM-result byte (`$00F8`) = 1 — NOT `$6000` (ticket W1-05b correction; this ROM generation predates blargg's `$6000` runtime, see `tests/rom-manifest.toml`'s comment on this suite) |
| `sprite_overflow_tests` | overflow bug | FR-CORE-023 | A | $6000 = 0 — **unverified as of W1-05b**: shares `sprite_hit_tests`' pre-`$6000` ROM family and almost certainly has the same protocol mistag; out of this ticket's scope, flagged in `tests/rom-manifest.toml` for whichever ticket implements this suite |
| `oam_read`, `oam_stress` | $2004 semantics | FR-CORE-023 | B — **2/2 as of W2-19**; `oam_stress` needed a frame budget of 5400, not 3600 (measured completion ~3620 — it was short by twenty frames) | $6000 = 0 |
| `full_palette`, `ppu_open_bus`, `ppu_read_buffer` | palette, open bus, $2007 buffer | FR-CORE-022 | B — **2/2 as of W2-19**: `ppu_open_bus` passes on the new PPU decay register, `ppu_read_buffer` on a budget of 2000 (measured completion ~1300; 600 was less than half what it needs). `full_palette` is `golden_frame` protocol and **still has no runner** | golden frame / $6000 |
| blargg `apu_test` (1 combined ROM, 8 sub-tests) | length counters, length table, frame IRQ + its timing, APU jitter, DMC basics + rates | FR-CORE-024 | A-local | $6000 = 0 — **8/8 as of W2-01a**; fifth Tier-A-local suite (gitignored ROM, so `scripts/local-gate.sh` + `docs/evidence/local-gate.json` carry the evidence, not CI) |
| blargg `apu_reset` (6 ROMs) | APU state across reset | FR-CORE-024 | A — **NOT WIRED**: fetched and in the manifest, but no ticket owns it and no `Apu::reset` path exists yet (W2-01a `HANDOFF:` note) | $6000 = 0 |
| blargg `dmc_dma_during_read4` (5 ROMs) | DMC DMA cycle stealing + the repeated-read glitch on `$2007`/`$4016` | FR-CORE-024 | A-local — **5/5 as of W2-01d** | **NOT `$6000`**: all five leave PRG-RAM zero (older screen-only shell), so the result is read out of the PPU nametable. `dma_2007_read` matches documented CRC `5E3DF9C4`, `double_2007_read` documented CRC `85CFD627`; the other three self-report `Passed` |
| blargg `apu_mixer` (4 ROMs) | non-linear mixer levels | FR-CORE-024 | B-local | RMS envelope of the cancellation section between the ROMs' two beeps — **the ROM's own design is the oracle** ("generate a tone, then generate the inverse waveform using the DMC DAC, canceling to (near) silence"), so no reference recording is needed. **`square` and `dmc` are gated** (residue < 0.10 of peak; a +20% `tnd`-LUT mutation measures 0.31/0.21, so the gate is not vacuous). `triangle` (0.092 residue vs a 0.057 noise floor) and `noise` (its section is *meant* to be noisy — "fade noise in, and out, without any tone") are structural-only, stated in `crates/retroforge/tests/apu_mixer.rs` rather than gated on a threshold picked to pass |
| `mmc3_test_2` + IRQ tests | MMC3 A12 IRQ counter | FR-CORE-025 | A | $6000 = 0 |
| Holy Diver Batman (28 ROMs) | mapper acid breadth | FR-CORE-025 | B | golden frame per ROM |
| RF-Scroller 5-min replay | real-game regression (in-repo fixture, D-001) | FR-CORE-026 | A | final-hash + 6 golden frames |
| Alter Ego 5-min replay | independent-proof regression (freeware fixture, D-008) | FR-CORE-026 | A — **RUNS LOCALLY ONLY (W2-11)** | final-hash + 4 golden frames, real `.rfreplay` round trip + independent-run determinism check |

**"RUNS LOCALLY ONLY" (Alter Ego row, ticket W2-11):** wired and
self-verifying (`crates/rf-harness/tests/alter_ego_replay.rs`), but this is
a narrower claim than plain Tier A ("every PR"), and a narrower one than
Tier A-local too — stated precisely rather than rounded up to either. By
this section's own Tier A-local definition above ("a suite whose … fixture
is a gitignored fetched artifact CI never has (NFR-006)") Alter Ego
qualifies for A-local treatment, but A-local also means "still a hard
release gate" via `docs/evidence/local-gate.json` +
`scripts/validate-evidence.mjs`'s staleness check, and wiring either that
or a `.github/**` CI job was explicitly out of ticket W2-11's scope — a
finding for whichever ticket does that wiring, not assumed here. Today: the
suite SKIPs loudly (never silently) if `roms/nes/alter-ego/alter_ego.zip`
is absent, matching every other fetched-ROM suite's convention; when
present, the full 5-minute double-run (record + independent replay,
~18s/pass release) is `#[ignore]`'d for cost — same discipline as
`retroforge`'s `determinism.rs` 10k-frame suite (ticket W1-08) — and must be
run explicitly (`cargo test --release -p rf-harness --test
alter_ego_replay -- --ignored`); no CI job and no evidence file confirms it
ran on any given commit. A second, cheap, NOT-`#[ignore]`'d test
(`alter_ego_scripted_input_actually_drives_the_game`, ~3s) does run as part
of the normal `cargo test --workspace` pass wherever the ROM is fetched,
and is the anti-vacuity check (asserts the scripted input actually moved
the on-screen player, not merely that the ROM booted).

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
  `08-nmi_off_timing`, `10-even_odd_timing`) failed against what W1-05b's
  own investigation believed was a sub-CPU-cycle timing ceiling this
  crate's architecture couldn't reach without CPU-crate changes.

**`ppu_vbl_nmi` W1-05c follow-up: the ceiling was real but narrower than
W1-05b thought.** Register access never needed sub-CPU-cycle placement —
with rendering disabled (every sub-ROM's own precondition), a frame is
89342 dots and 89342 mod 3 = 2, so the ROMs' own multi-frame
`sync_vbl`/`sync_vbl_delay` convergence loops visit every PPU-dot residue
against this crate's fixed CPU-cycle window boundaries for free (see
`crate::ppu`'s module doc "Sub-CPU-cycle VBlank/NMI race timing" section
for the full derivation). The fix was three narrow, still-whole-cycle
changes: `scroll.rs::read_status` handling two more `(scanline, dot)`
positions (the "same PPU clock as the set" and the pre-render "same clock
as the auto-clear" fenceposts) plus a one-PPU-*dot* NMI-visibility latch in
`crate::system::NesBus::tick_master` (a genuine dot-loop split, not a
`master_cycle` one — see that method's doc). Result: 9/10 —
`02`/`05`/`06`/`07`/`08` all pass now. `10-even_odd_timing` still failed, at
the same "Clock is skipped too late, relative to enabling BG" sub-test
W1-05b measured, byte-identical across every experiment W1-05c ran
(multiple `read_status` models, both `CpuBus::read` orderings) — confirming
W1-05b's own prediction that it's a separate, `$2001`-write-timing cause,
not this read-side/NMI-edge race.

**`ppu_vbl_nmi` W1-05d: 10/10, and the waiver is deleted rather than
re-dated.** `10-even_odd_timing` needed no re-sequencing of register access
at all — the defect was that the odd-frame skip decision read `PPUMASK`
with zero propagation delay, against nesdev.org/wiki/PPU_registers's
"toggling rendering takes effect approximately 3-4 dots after the write".
The ROM pins the depth exactly (its sub-tests 2 and 3 enable BG one PPU dot
apart and both expect X=8; instrumented, those land at pre-render dots 337
and 338 in this engine's write-application convention, and the decision
runs after dot 339), so a two-dot latch — `crate::ppu::Ppu::render_enable_pipe`,
scoped to that decision and not to rendering generally — is the only depth
that accepts one and rejects the other. Corroboration that it is a model
and not a tuned constant: sub-tests 4 and 5, the *disabling*-BG pair that
had never run before, pass untouched, and the ROM's printed output is
`08 08 09 07`, byte-identical to the expected output in its own source
header. `crates/rf-harness/waivers.toml` now holds no waivers at all.

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

**GPU-pass / local-AI benchmark evidence gate (ticket W16-01;
`docs/design/ENHANCEMENT_WAVE_16.md` §7-8):** a second, unrelated evidence
file, `docs/evidence/gpu-passes.json` — timing rows (`pass`, `size`,
`p50_ms`, `p95_ms`, `n`, `source`), never accuracy rows, and validated by a
sibling script rather than an extension of `validate-evidence.mjs` (that
script's whole schema is accuracy-gate-specific; see
`scripts/validate-gpu-evidence.mjs`'s own module doc for why bolting a
timing schema onto it would make every accuracy check also reason about
an unrelated concern). Also never run in CI, for a stronger reason than
the accuracy suites above: it needs a real (or software-fallback) GPU
adapter, and for the two local-AI rows, a multi-hundred-MB model plus an
`ORT_DYLIB_PATH` staged on disk.

1. `cargo run --release -p rf-renderer --bin bench-passes` times the
   existing shader-chain passes (`nearest`, `xbr`, `crt`) plus a stub
   "neural" compute pass (`crates/rf-renderer/src/bin/bench_passes/
   neural_stub.wgsl` — a fixed 4x conv-like workload, NOT a trained model)
   at 256x240 and 512x448, 300 frames each, and writes/merges rows tagged
   `source: "gpu-bench"`.
2. `scripts/fetch-ai-upscale-model.sh` / `scripts/fetch-onnx-runtime.sh`
   stage a Real-ESRGAN-class ONNX model and the ONNX Runtime dylib into a
   cache OUTSIDE the git tree (`$RF_AI_CACHE`, default
   `~/.cache/retroforge-ai` — never `roms/`, never committed; see
   `crates/rf-ai/ai-model-manifest.toml` for the licence ledger, same
   vocabulary as `tests/rom-manifest.toml` but a separate file since
   `tests/**` is outside ticket W16-01's write_scope). Then
   `cargo test --release -p rf-ai --features onnx-coreml --test
   onnx_bench -- --ignored --nocapture` runs the CPU-EP-control and
   CoreML-EP `#[ignore]`d specs, tagged `source: "onnx-ort"`.
3. `cargo test --release -p rf-renderer --features metalfx --test
   metalfx_bench -- --ignored --nocapture` times MetalFX spatial
   upscaling via `objc2-metal-fx` (macOS only), tagged `source: "metalfx"`.
4. `scripts/gpu-gate.sh` runs step 1 then `node
   scripts/validate-gpu-evidence.mjs` (step 2's spike is opt-in via
   `RF_ONNX_BENCH=1`, since it needs the multi-hundred-MB fetch above);
   the validator checks row shape, percentile ordering (`p95_ms >=
   p50_ms`), no placeholder zeros, and that both required sizes have a
   `neural-stub` row.

This file's numbers, not a guess, set `docs/design/ENHANCEMENT_WAVE_16.md`
§8's real-time frame-budget gate threshold — see that section for the
measured table and which passes clear it today.

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
| Double-run at scale | ROADMAP's Phase 1 exit criterion, "2x 10k-frame runs, identical per-frame hashes over reachable state" — two `#[ignore]`'d tests in the same file, `same_log_two_10k_frame_runs_produce_identical_hash_sequences` and `different_10k_frame_logs_produce_divergence_detected_at_first_occurrence` (ticket W1-08); see below for why they're `#[ignore]`'d and how they're invoked | FR-CORE-002, FR-CORE-003 |
| Replay determinism | every test-ROM run is recorded and replayed; final hash equal (suite: `crates/retroforge/tests/determinism.rs` + `rf-input`, not `rf-harness` — same reason, ticket W1-07) | FR-STATE-006 |
| State roundtrip | at frames {60, 600, 3600}: save → load → run 600 more ⇒ hash equals uninterrupted run | FR-STATE-002 |
| Cross-mode state | Enhanced-mode state loads in Accuracy config (enhancement chunks skipped) and continues hash-identical | FR-STATE-007 |
| Golden fixtures | every released `.rfstate`/`.rfreplay` fixture loads on current main | FR-STATE-005 |
| **Mode invariant** | same ROM + log run in Accuracy and Enhanced (all features on) ⇒ identical core hashes every frame | FR-MODE-002 |
| Wrong-ROM refusal | state with mismatched normalized hash refused with diagnostic | FR-STATE-003 |
| SA-1 determinism | two independent `SnesSystem::load` runs of a hand-assembled SA-1 cart, same instruction count ⇒ identical `StateRegion::ALL` snapshot; a save/load round trip taken mid-DMA-setup (armed, not yet triggered) resumes byte-identical to an uninterrupted run (suite: `crates/rf-snes/tests/sa1_determinism.rs`, ticket W17-04 — `rf-snes` has no crate-local suite named "determinism"/"mode-invariant" of its own; those names belong to `crates/retroforge/tests/mode_invariant_*.rs` above, out of W17-04's `write_scope`, so this is the SA-1-specific equivalent) | FR-CORE-039 |

**10k-frame double-run: why `#[ignore]` + release-mode, and why no
`docs/evidence/local-gate.json` row (ticket W1-08).** `crates/retroforge/tests/determinism.rs`'s
24/32-frame double-run tests (`same_log_two_independent_runs_produce_identical_hash_sequences`,
`different_logs_produce_different_and_sticky_hash_sequences`) prove the
*shape* of the determinism property, but ROADMAP's Phase 1 exit criterion
is explicitly 2x **10,000** frames — over 400x more. Measured (not
guessed): this fixture runs a real NES frame per script step, so
2x10,000 frames is ~600M master cycles. Debug: ~4.7 ms/frame, linear,
measured at 250 and 1000 frames ⇒ 2x10,000 ≈ 93 s — more than double the
whole workspace's ~45 s `cargo test --workspace`, which would nearly
triple every contributor's default test run if run inline. Release: ~10.5x
faster, measured directly on the real 10k tests at 9.42 s (cold release
build of the `retroforge` crate, first-time compile of its `eframe`/`wgpu`
chain, adds ~57 s on top the first time only — amortized away by CI's
existing `Swatinem/rust-cache`, same as the debug cache already is) —
closely matching the pre-flight extrapolation of ~9 s. So: both tests are
`#[ignore]`'d, and invoked explicitly in `--release` from both
`.github/workflows/ci.yml` (a dedicated CI step, every PR) and
`scripts/local-gate.sh` (for anyone running the local gate by hand) —
run directly with:

```
cargo test --release -p retroforge --test determinism -- --ignored
```

They do **not** get a row in `docs/evidence/local-gate.json`, and this is
a deliberate decision, not an oversight — record the reasoning here so it
isn't re-litigated. §4 above explains what the evidence-row mechanism is
actually *for*: suites CI **cannot run at all** — `nes6502`/`nestest`/
`ppu_vbl_nmi`/`sprite_hit_tests` all depend on real, fetched, gitignored
ROM/vector corpora (NFR-006) that never exist in a CI checkout, so
`cargo test` skips them cleanly and a skip is visually indistinguishable
from a pass; the evidence file plus its staleness check
(`git merge-base --is-ancestor` over each suite's `coveredPaths`) is what
turns "ran once, locally, trust me" into something CI can mechanically
verify without ever fetching the corpus itself. This suite has neither
problem: its fixture, `controller_reader_rom()`, is a synthetic in-code
NROM assembled by the test itself — nothing to fetch, nothing gitignored,
nothing to skip, nothing to fake. And at ~9 s release, it is cheap enough
to run **directly, in full, in CI, on every single PR** — which is a
*strictly stronger* guarantee than an evidence row would give: the row's
staleness check only forces a re-run when `coveredPaths` changes, whereas
direct CI execution runs it unconditionally every time. Adding a row here
would apply a workaround built for suites CI can't run to a suite CI can
and does run — solving a problem this test doesn't have — at a real cost:
`local_gate_evidence.rs` would have to drive `EmuStepper`, so `rf-harness`
(which today depends on `rf-nes` directly only via `scripts/validate-arch.sh`'s
"test harness" exemption, and nothing else console-shaped) would gain a
dependency on the `retroforge` lib crate, pulling `eframe`/`wgpu` into the
harness build for no coverage gain. (Ruled out for the same reason:
`scripts/local-gate.sh` passing a pass/fail *result* into the evidence
binary as a CLI flag — shell-supplied *inputs* have precedent, e.g. the
open-ticket list, but a shell-supplied *result* would make the evidence
forgeable by the very script meant to check it.)

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
- **Atmosphere-layer detector (ticket W16-03, shadow rung)**:
  `crates/rf-harness/tests/atmosphere_layer_red_fixture.rs`. No `cc65`
  toolchain is present on this machine to rebuild an RF-Scroller-S ROM
  variant with a fog plane, so the red fixture is a synthetic
  `Vec<PpuPixel>`/`Vec<SubPixel>` frame sequence (a plane with half/additive
  colour math, slow independent scroll, and low tile variety) driven
  directly through `rf_enhance::atmosphere::AtmosphereDetector`:
  `atmosphere_plane_triggers_and_stays_triggered` (must keep firing),
  `plain_scene_does_not_trigger` and `palette_cycling_scene_does_not_trigger`
  (paired negative controls — the latter is `ENHANCEMENT_WAVE_16.md` §4's
  binding Norfair-heat correction). `crates/rf-enhance/src/atmosphere.rs`'s
  own unit tests cover the individual threshold boundaries.
- **Path containment (NFR-010)**: symlink-escape attempts on plugin
  cache_dir, profiles.d references, and library scan roots are refused;
  scan survives a symlink loop (unit tests per surface).

## 8. Phase exit gates (roadmap enforcement)

**`docs/ROADMAP.md`'s per-phase "Exit criteria" list is AUTHORITATIVE. This
table is a summary of it and must never add or drop a criterion** (corrected
2026-08-05 by the Phase 1 exit gate). Until then this table restated the
criteria independently and had silently drifted: its Phase-1 row demanded
`instr_test-v5` (which ROADMAP places in **Phase 2**) and a save-state
`roundtrip` suite (owned by W2-04, Phase 2), so the two documents specified
different gates and a phase could "pass" against whichever was convenient.
`docs/work/DESIGN_REVIEW.md`'s G-34 recorded that drift as fixed in 2026-07,
but that was a wording alignment only — hence the rule above, which removes
the duplication rather than re-aligning it a second time.

| Phase | Exit = all of |
|---|---|
| 1 (NES MVP) | nes6502 vectors 100% official · nestest diff empty · frame stepping works · 2× 10k-frame double-run identical over **reachable** state |
| 2 (NES compat) | Tier-A NES table fully green (incl. `instr_test-v5`, wired by W2-12) · mapper set complete · battery saves · save-state roundtrip green · determinism extended to **full** machine state (PPU + APU, once W2-04 lands `save_state`) · 2 NROM homebrew titles boot · RF-Scroller + Alter Ego replays green |
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

## Hidden gates: `#[ignore]`d tests CI never runs (W7-14)

`cargo test --workspace` — which is what CI runs — **skips every
`#[ignore]`d test**. Several Tier-A gates are `#[ignore]`d because they
cost minutes of emulated time or need fetched, gitignored artifacts, so
they are invisible to CI by construction.

That is not hypothetical. RF-Scroller's five-minute NES replay was **red
for months and green in CI the whole time**: W5-02c changed the fixture
ROM (`rom.sha256` moved) without regenerating the golden hashes gating it,
and nothing ran the test that would have said so. It was found by accident
while proving an unrelated change's backward compatibility.

`scripts/local-gate.sh` is the answer, and every such suite must be wired
into it. As of W7-14 it runs: the nes6502 and 65816 and spc700 vector
suites, nestest, the ppu_vbl_nmi / sprite_hit_tests / apu_test ROM sets,
gilyon `cputest`, the PeterLemon PPU goldens, the 10k-frame determinism
double-run, and **both five-minute replays** (NES and SNES).

**If you add an `#[ignore]`d test that gates behaviour, add it to
`local-gate.sh` in the same commit.** A gate nothing runs is not a gate.

### The one deliberate exception: the boot census (ticket W14-03)

`crates/rf-harness/tests/boot_census.rs` is `#[ignore]`d and is **NOT** in
`local-gate.sh`, **NOT** in `gate-darwin.yml`, and never will be. That is a
written decision, recorded before the first full run rather than after it.

It runs a real, locally-held commercial ROM library — thousands of unknown
programs — on the developer's own workstation. That is the **RF-L-09 shape
at scale**, and *finding hangs is close to the point of the exercise*. So
it is bounded three ways: **one child process per ROM**, so a hang or an
abort costs one row and cannot take the harness with it; a **wall-clock cap
enforced by the parent**, outside any emulation loop, so it holds however
tightly a child is spinning; and **`RF_ROM_LIBRARY` must be set**, so no
gate and no plain `cargo test` can start it. It is run by hand, attended.

**Nothing it reports is a pass.** The buckets are triage — *rendered
something*, *rendered nothing*, *refused*, *crashed*, *timed out* — and
"it booted" is not "it is correct". Treating a count of boots as an
accuracy claim would repeat the mistake that reopened W7-08.

First run, 2026-09-15, release build:

| library | titles | rendered something | uniform screen | refused | crashed | timed out |
|---|---|---|---|---|---|---|
| NES | 1281 | 1167 | 7 | 107 | **0** | **0** |
| SNES, first run | 1265 | 303 | 806 | 150 | **0** | 6 |
| SNES, after W14-06 | 1265 | **860** | 244 | 150 | **0** | 11 |
| SNES, after W14-08 | 1265 | 870 | 245 | 150 | **0** | **0** |
| SNES, after W14-09 | 1265 | 934 | 181 | 150 | **0** | **0** |
| SNES, after W14-10 | 1265 | **1001** | 114 | 150 | **0** | **0** |
| NES, after W14-12 | 1281 | **1180** | 6 | 95 | **0** | **0** |
| NES, after W14-13 | 1281 | **1184** | 6 | 91 | **0** | **0** |
| NES, after W14-14 | 1281 | **1197** | 6 | 78 | **0** | **0** |
| NES, after W14-15 | 1281 | **1212** | 6 | 63 | **0** | **0** |
| NES, after W14-16 | 1281 | **1222** | 6 | 53 | **0** | **0** |
| NES, after W14-17 | 1281 | **1222** | 6 | 53 | **0** | **0** |
| SNES, after W14-19 slice 1 | 1265 | **1012** | 115 | 138 | **0** | **0** |
| NES, after W14-22 | 1281 | **1222** | 6 | 53 | **0** | **0** |
| SNES, after W14-21 | 1265 | **1012** | 115 | 138 | **0** | **0** |
| SNES, after W16-11 | 1265 | **1012** | 115 | 138 | **0** | **0** |
| SNES, after W17-01 | 1265 | **1015** | 120 | 130 | **0** | **0** |
| SNES, after W17-04 | 1265 | **1017** | 118 | 130 | **0** | **0** |

**The NES row's zeros are one finding.** 1281 real commercial programs,
none of which this emulator had ever seen, and not one crash or hang in
16 minutes.

**The SNES row is the other, and it is worse news honestly reported.**
806 titles emitted scanlines whose every pixel carried the same palette
index — the renderer runs and paints a flat colour. A 60-title re-run
with a finer bucket found **zero** cases of "no video at all", which is
what separates a dead renderer from one drawing nothing, and the same
harness against NES produced a uniform screen for 7 of 1174 (0.6%)
against roughly 76% here. That asymmetry is the evidence that it is
systemic and SNES-side. That was ticket **W14-06**, and it is **closed**: one
over-specific comparison in the APU boot handshake, which the third row
above measures. The hangs are **W14-07**, and they went from 6 to 11 as
games got further — a census bucket rising after a fix is the honest
shape of progress, not a regression.

Neither was fixed by the ticket that found them, on purpose: the census
is a measurement, and folding its findings in would have turned it into
an open-ended accuracy ticket.

**W14-19 slice 1 (DSP-1 HLE, D-010)**, 2026-09-17, release build: refused
fell from 150 to 138 as the DSP coprocessor nibble stopped being an
automatic refusal — of that movement, 11 titles landed in "rendered
something" and 1 in "uniform screen" (1001->1012 and 114->115). Both
named acceptance titles rendered a title screen individually within the
census's 600 frames: **Super Mario Kart** and **Pilotwings** each exit
`RENDERED`. The move is 12 titles against the 13 named in D-010's
"~13 titles" count; this run did not track down which title accounts
for the gap (a duplicate archive, a header that scores below
`MINIMUM_SCORE`, or a title this build's copier-header handling
mishandles are the plausible causes, in no particular order) — recorded
here rather than assumed away. Slice 1 implements SNES Development
Manual §5.1-5.3 (multiply, inverse, triangle, radius, range, distance,
rotate, polar) plus the three test commands; **titles that need §5.4+
(raster/projection/attitude, not implemented this slice) are expected to
land in "uniform screen" rather than "rendered something" here — the
census does not attribute a bucket to a specific missing command, so
this is stated as an expectation the numbers are consistent with (1 of
the moved titles landed in "uniform" rather than "rendered"), not a
per-title confirmation — recorded honestly rather than fudged into a
pass.** The three DSP-2/3/4 titles named in D-010 (Dungeon Master, SD Gundam GX
Rasetsu no Sho, Top Gear 3000) now load through the DSP-1 HLE as
known-wrong, per the identification rule rf-cart applies (coprocessor
nibble alone cannot tell the DSP families apart) — none of the three is
expected to render correctly, and none is claimed to.

**W14-21 (DSP-1 HLE slice 2)**, 2026-09-17/18, release build: implements
Manual §5.4-5.6 (projection parameter setting, raster, object
projection, screen point, set/convert attitude A/B/C, inner product,
3D angle rotation) plus the 38h double-precision range variant, per the
dsp1.rs module doc's Tier-1/Tier-2 split (equation-stated commands
transcribed; projection/raster reconstructed from parameter
descriptions, no stated formula exists in the source set). The unknown-
command counter (a new per-opcode histogram, `Dsp1::unknown_opcodes`)
reads **0** for both **Super Mario Kart** and **Pilotwings** across 600
frames, down from 264 and 128 respectively on this slice's first pass —
both nonzero counts were entirely one opcode, `$80`, written by both
titles repeatedly before their first real command with no parameters
and no output read; documented in dsp1.rs as an empirically-discovered
no-op (not in the Manual/snesdev/fullsnes command lists), most likely a
chip-presence check that expects to read back its own idle sentinel.
Both titles still exit `RENDERED` in the census child individually.

**The census bucket counts did not move**: 1012/115/138/0/0, identical
to slice 1's row. The raster/projection math is unit-tested directly
(`raster_scale_shrinks_toward_the_viewer`,
`projection_straight_down_centers_on_base_point`, and the attitude/
gyrate equation tests) and confirmed exercised by both named titles (the
`$0A` raster command appears in their DR traffic), but neither title's
own bucket needed raster to already read as `RENDERED` — both were
rendering their title screen (menus, HUD, sprites) before this slice,
and the census's 600-frame pixel-variance check does not attribute
variance to a specific command. A plausible reason the *track* itself
may not visibly use this slice's raster output: Manual §5.4.2 names `0Ah`
as "to output result of calculation via DMA", and both titles' DR
traffic (traced during this ticket) writes the raster terminator
immediately after starting the stream rather than reading scanlines
through the CPU — consistent with expecting the real hardware's DSP-1-
to-PPU DMA path to drain DR, which this build's DMA engine was not
confirmed (nor is it in this ticket's write scope to confirm) to source
from `Dsp1Dr`. **Recorded rather than assumed away, per the same rule
slice 1 used for its own gap.** The by-eye check of Super Mario Kart's
track against a reference emulator, and of Pilotwings' flight view, is
**pending** — the user's own verification, not run in this session.

**The "13th DSP title" gap slice 1 left open is resolved, not by a
missing DSP-1 title but by an incorrect premise**: this ticket's
`crates/rf-snes/tests/dsp1_probe.rs::scan_dsp_coprocessor_titles` found
exactly **12** archives in `~/Games/Roms/snes` whose header reports the
DSP coprocessor nibble (both `Ballz 3D` dumps, `Dungeon Master`,
`Lock On`, both `Michael Andretti's Indy Car Challenge` dumps,
`Pilotwings`, `Super Bases Loaded 2`, both `Super Mario Kart` dumps,
`Suzuka 8 Hours`, `Top Gear 3000`), all twelve of which load and render
(none refused). `Metal Combat - Falcon's Revenge` — a title sometimes
mentally grouped with the DSP-1 racing/flight titles for its similar
sprite-scaling pseudo-3D effect — is **not** DSP-family at all:
`rf_cart::Cartridge::load` on its raw ROM bytes returns
`UnsupportedChip { name: "OBC1 (SNES chipset $25)" }`, a distinct,
already-deferred SNES coprocessor (NON_GOALS #10 / D-010's deferral
list). It stays correctly refused for an unrelated, already-recorded
reason. Brad's "~13 titles" in D-010 most likely counted Metal Combat
alongside the real DSP-1 titles by genre/effect resemblance rather than
by chip; the actual DSP-coprocessor-nibble count in this library is 12,
and this run accounts for all of them.

**W16-11 (DSP-1 raster output via DMA: confirm and wire `Dsp1Dr` as a DMA
source)**, 2026-09-18: resolves the open follow-up W14-21 left ("this
build's DMA engine was not confirmed to source from `Dsp1Dr`"), and the
answer is not what that note guessed. W14-21's trace ran with an empty
`InputFrame` for all 600 frames — the SNES core's `run_frame` does not
wire the `InputFrame` argument to the joypads at all yet (`SnesCore::
run_frame`'s own comment: "Controller wiring is W11-12's"), so that trace
never left the title screen, and what it saw — the raster terminator
written immediately after starting a session — was a boot-time
chip-presence check, not real raster use. This ticket's extended probe
(`crates/rf-snes/tests/dsp1_probe.rs::dsp1_raster_drain_trace`) drives the
pad directly through `system_mut().bus.joypads.ports[0]` (the same route
`peterlemon_golden.rs`/`gilyon_cputest.rs` use, since `run_frame`'s input
argument still goes nowhere), mashing Start with A held, for 600 frames on
each title. With that, both titles reach real gameplay and the DR-drain
trace (`SnesBus::dsp1_trace`, ticket-only diagnostic counters, not part of
save state) is unambiguous:

| title | first raster session | CPU DR reads (raster) | general-DMA reads | HDMA reads |
|---|---|---|---|---|
| Super Mario Kart | frame 42 | 99,200 | 0 | 0 |
| Pilotwings | frame 537 | 87,868 | 0 | 0 |

**Neither title drains Raster (`0Ah`) output by DMA or HDMA at all — both
poll the DR directly from the CPU**, for the whole session, at real
gameplay volume. So the acceptance's conditional ("if by DMA: wire the
engine...") does not apply to either named title, and the census cannot
move: it did not move (1012/115/138/0/0, unchanged from W14-21).

The DMA/HDMA engine's ability to source from `Dsp1Dr` was checked anyway,
because the Manual and fullsnes still document raster as DMA-drained
hardware behaviour and a different DSP-1 title could rely on it. It did
not need fixing: `SnesBus::read`/`write`'s `Target::Dsp1Dr` arm is reached
through the identical `target()` resolution for a CPU access, a
general-DMA byte (`run_channel`), and an HDMA unit (`hdma_transfer_unit`)
— none of the three touches `rom`/`wram` directly — so a DMA/HDMA byte
already advances the DR exactly as a CPU read does, general DMA's
fixed-address mode (`$43x0` bit 3, `Channel::a_step`) already returns 0
and already works (the DSP-1 window matches on being inside a wide
`RangeInclusive<u16>`, not on the literal offset, so a fixed or advancing
address both resolve to the same chip), and `Dsp1Sr` resolves the same
way. Two new bus-level tests confirm this by construction rather than by
assertion — `crates/rf-snes/src/tests/dsp1_dma.rs`'s
`fixed_address_mdma_drains_a_raster_session_byte_for_byte` (general DMA,
fixed A-bus, per fullsnes' documented hardware technique) and
`indirect_hdma_drains_a_raster_session_into_mode7_registers` (HDMA
indirect mode, two channels writing the write-twice `$211B`-`$211E` Mode 7
matrix ports low-byte-then-high) — both compare the DMA/HDMA-drained
result against a CPU-driven reference `Dsp1` instance byte for byte, and
both pass unmodified against the existing `dma.rs`/`bus.rs` machinery; no
code in `dma.rs` changed. No new save-state field was added: the trace
counters are diagnostic-only and intentionally not serialized, the same
"diagnostic, unsaved" contract `Dsp1::unknown_opcode_hist` already
documents, so a save/load round trip is unaffected (existing
`Channel::save`/`Dsp1::save` cover everything that changes emulated
behaviour).

**By-eye check of Super Mario Kart's track and Pilotwings' flight view
against a reference emulator is still pending — Brad's own verification,
not run in this session** (unchanged from W14-21's note).

**W17-04 (SA-1 slice 4: timing accuracy, determinism, census and docs)**,
2026-09-18, release build. All eight real SA-1 archives in the library
(law 5: named here only because this is test evidence, never in engine
code) — **Kirby Super Star**, **Kirby's Dream Land 3**, **PGA European
Tour**, both **PGA Tour '96** dumps, **Power Rangers Zeo: Battle
Racers**, and both **Super Mario RPG** dumps — were probed individually
with `title_probe`'s new `PROBE_SA1REGS=1` (prints
`Sa1Regs::unknown_write_offsets`, the counter this ticket added for
writes into `$22xx` offsets fullsnes's own I/O map table leaves blank)
for 600 frames each: **all eight report `sa1 unknown register writes:
none`.** Kirby Super Star, Kirby's Dream Land 3 and both PGA titles
render (`varied_at` 97, 102, 32 and 32 frames respectively) — the four
by-eye titles from W17-03's close note.

**Power Rangers Zeo, traced further this ticket**: `PROBE_MODE=frames
PROBE_FRAMES=1800` still shows `forced_blank=true` at 1800 frames (30 s
game time), matching W17-03's note. Pushed further with an instruction-
count probe (`PROBE_INSTR=70000000`, ~1200 emulated frames past that),
forced blank **does lift** — the sampled snapshot at frame 4842 shows
`forced_blank=false bright=15 mode=7`, `cgram_nonzero=220`,
`vram_nonzero=46205`, `tm=[1100+obj]` (BG1/BG2/OBJ enabled), consistent
with a Mode 7 track view having started drawing. But a `PROBE_MODE=frames
PROBE_FRAMES=8000` run's `varied_at` is still `None`, and a fresh 8000-
frame snapshot shows forced blank **back on** (`mode=1`) — the title
toggles forced blank on and off across a long attract/track-select
sequence, and the census's per-scanline palette-uniformity check never
catches a varied frame inside an 8000-frame (133 s) window even though
real content is being drawn partway through it. This is not chased
further: it is a slow-boot/rendering-completeness question (is Mode 7 in
this state actually drawing distinct pixels, or is the 4842-frame
snapshot itself still uniform under the hood?), not a SA-1 register or
timing gap, and 8000 frames is more than 13x the census's 600-frame
budget for every other title in both libraries — scaling the census
child's per-title budget for SA-1 carts specifically was considered and
rejected as unprincipled: [`FRAMES`](../crates/rf-harness/tests/boot_census.rs)
is a fixed wall-clock/frame cap applied uniformly, not derived from
main-CPU instruction count, so there is no SA-1-specific quantity to
scale it by — an SA-1 cart's main CPU runs exactly as many frames per
census run as any other cart's. **Zeo stays in the "uniform screen"
bucket, named cause: forced-blank toggles on a long boot/attract
sequence well past any practical census budget; confirmed still
producing SA-1 traffic with zero unknown register writes.**

**Super Mario RPG, confirmed this ticket**: both dumps sit at the same
spin `title_probe` found in W17-03 — `C4:0541: CMP $002140` /
`C4:0545: BNE $0541`, a tight two-instruction loop, 10,000+ hits on each
PC in a 20,000-instruction sample. This ticket's own trace adds the
handshake's other half: **`apu.cpu.stopped=true`** — the SPC700 itself
is halted (a `STOP`/`SLEEP`-shaped instruction, not a crash) while
`apu.boot_running=true`, so nothing will ever write `$2140`/`$2141`
again and the main CPU's compare can never succeed. `$2140`'s current
value (`ports_in[0]`) is `0x5F`, `$2141` is `0x02`, and the main CPU's
16-bit accumulator holds a different combination of the same two bytes —
the two sides parted ways mid-handshake with the SPC700 driver going
idle before writing the exact word the main CPU is waiting for. This is
the standard SPC700 IPL/upload handshake (the same shape as every
"waiting on $2140" pattern this project has already named for non-SA-1
titles), entirely on the APU side of the machine — **named cause,
explicitly out of SA-1 scope**: the SA-1 register report for both dumps
reads `none`, so nothing about the coprocessor is implicated, and no
change was made here.

Census: run after this ticket's cost-model and register-counter changes
("SNES, after W17-04" row above) — **1017/118/130/0/0**, up from
1015/120/130/0/0 at W17-01 (before the second CPU existed). Every SA-1
archive's bucket, confirmed individually via the `title_probe` run above
(the census's own report only names Crashed/Timed-out titles, not every
bucket's members): **rendered** — Kirby Super Star, Kirby's Dream Land 3,
PGA European Tour, both PGA Tour '96 dumps (5 of the 8 SA-1 archives);
**uniform screen, named cause** — Power Rangers Zeo (forced-blank
toggles on a long attract sequence past any practical budget, above) and
both Super Mario RPG dumps (SPC700 handshake stall, above) — 3 of 8. No
SA-1 archive is refused, crashed, or timed out. Gate at close: see the
commit trailer.

**By-eye items for Brad (pending — not run in this session, same status
as the DSP-1 titles above):**
- **Kirby Super Star** — renders (`varied_at=97`); check the SA-1-
  accelerated character-conversion/scrolling against a reference
  emulator.
- **Kirby's Dream Land 3** — renders (`varied_at=102`); same check.
- **Super Mario RPG** — does **not** render in this build (see the SPC700
  handshake finding above, out of SA-1 scope); nothing to by-eye until
  the APU-side stall is separately investigated, so this item stays
  conditional on that.

**The second step of the triage is `crates/rf-harness/tests/title_probe.rs`**
(ticket W14-11): an `#[ignore]`d, env-driven probe that instruction-steps
one title, samples where both CPUs spend their time, and prints the loops
each is stuck in with the register state — plus disassembly, ARAM dumps,
ROM searches and rings of port changes and PCs on request. The census says
*which* titles; the probe says *where to look*. Its module doc has the
env-var contract. Like the census it takes ROM paths from the environment
and is in no gate.

**W14-23 (SPC700 halts during the boot upload: Super Mario RPG's `$2140`
poll never completes), 2026-09-19, release build — corrects W17-04's
named cause, no fix shipped, ticket left BLOCKED.**

Traced past where W17-04 stopped. `apu.cpu.stopped=true` /
`apu.boot_running=true` is real, but it is **not** the IPL handshake
stalling: the first-stage IPL transfer completes cleanly (37 bytes to
`$0200`, `boot=Running`, `ipl` still banked in) and the SPC700 runs real
uploaded driver code afterward. The halt, pinned three ways with
`title_probe`'s `PROBE_STOP_ON_SPC_STOP`/`PROBE_PORTS`/`PROBE_ARAM`: SPC
PC settles at `$09B6` with `stopped=true`; `spcring` shows the final
pass through the driver's own port-polling loop going `$09B4 → $09B5 →
$09B6` where every earlier pass went straight `$09B4 → $09B6`; ARAM
`$09B4` held `E4 16` (`MOV A,$16`, live code) at instruction 70,000 and
`FD EF` (`MOV Y,A` / `SLEEP`) at the halt. Byte-for-byte: the CPU's
packet at the halting write carried data `$40 $FD $EF`, and the driver's
own receive loop stores three bytes per call at `$0912+X`/`$0913+X`/
`$0914+X` with **no bound and no reset on `X`** — traced climbing `0x98
→ 0x9B → 0x9E → 0xA1 → 0xA4` by 3 per call across dozens of calls. At
`X=0xA1` the three stores land on `$09B3`/`$09B4`/`$09B5` — **the
driver's own command-receive loop overwrites its own code with ordinary
incoming data**, and the next fetch through `$09B4` executes the
just-written `SLEEP`.

Ruled out by trace, not by argument: an unimplemented opcode (`$EF`
SLEEP is implemented, `ops.rs:800`, and `step_counted` returns `Ok`, not
an error); the IPL boot machine exiting with the PC still in the
`SLEEP`-filled `$FFC0-$FFFF` window (`ipl=false` throughout the halt,
and the halt PC is in ARAM, not the IPL region); and the first-stage
upload itself being short or misaddressed (37 bytes landed at the
correct `$0200`, `entry=$0200`, cleanly).

**A fix was attempted in `IplBoot` and reverted — recorded so the next
executor does not repeat the three hours.** The working theory was that
`IplBoot`'s per-byte counter prediction (`expected.wrapping_add(1)`)
should skip `$00` on wraparound, per snes.nesdev.org/wiki/S-SMP: "if
your counter is 0 after incrementing ... increment it a third time to be
non-zero ... because a value of 0 in port 0 will also signal the first
byte of the transfer." **That page states the rule for the two-step
"next block" increment specifically; extrapolating it to every ordinary
`+1` continuation is wrong**, disproved by a same-tree A/B: Wild Guns'
own uploader wraps its counter `$FF → $00` in the plain, undocumented
way (traced with `PROBE_PORTS=1`), and the skip-zero prediction turned
that legitimate continuation into a false counter mismatch, misreading
`ports_in[2]`/`[3]` as a bogus destination address and jumping to
`$A400` — the same failure *shape* this ticket set out to fix, now
hitting a previously-working title. Super Mario World was not checked
before the Wild Guns regression surfaced and disproved the rule, so it
was not needed to kill the theory, but both are census-baseline titles
that must not move. The attempted change (`crates/rf-snes/src/apu/
boot.rs`, `next_counter`/`last_counter`, plus a new
`crates/rf-snes/src/tests/apu_ports.rs` case) was reverted in full; the
working tree for this ticket's close is identical to before it started.

**Named cause corrected**: W17-04's "SPC700 IPL/upload handshake"
framing was too narrow — the IPL upload itself is clean. The actual
cause is a buffer overflow in the **uploaded driver's own runtime
command-receive loop** (unbounded `X` walking into adjacent code), which
this ticket's write scope (`IplBoot`, the SPC700 core) cannot fix by
construction: the driver bytes are the game's own, real hardware would
run the identical code, and law 5 forbids reconstructing or patching
around them. Whether real hardware avoids this specific overflow (a
different, still-uncorrupted `X` growth rate; a bound this emulator
isn't honouring; a rate mismatch between the SPC700 and the main CPU)
is unresolved and is the next ticket's question — **not** an IPL
handshake question, so it should not be filed as one.

Both Super Mario RPG dumps stay in the **uniform screen** bucket. Named
cause updated to: *uploaded driver's command-receive loop overflows its
own destination buffer into adjacent code (SLEEP at ARAM `$09B5`), not
an IPL handshake stall; unresolved whether this is a timing mismatch
against real hardware or a buffer real hardware also relies on being
sized differently.* Census not re-run — no code changed, so no bucket
counts can have moved and doing so would only spend budget confirming a
tautology. `plan.json`'s W14-23 note carries the same finding as a
BLOCKED handoff; ticket status left `in_progress`, not `done`.

**One instrumented-but-unconfirmed lead for whoever picks this up**:
under the (reverted) `next_counter` change, SMRPG's execution got
further — past the SLEEP, into a second `IplBoot`-managed transfer
triggered by the driver jumping back to `$FFC0` — before hanging on a
*different* spin, `LDA $2142` / `BNE $086E`, waiting for `$2143==2`.
`ports_out[3]` is set only by an SPC700 write to `$F7`, and neither
`Publish` nor `reenter_ipl` touch it, so either the driver never reaches
that write or something clears it first. This was observed only under
code that has since been reverted and is not a finding about the current
tree — flagged as a place to instrument (`write_register` on `$F6`/
`$F7`, with the writing SPC PC) before assuming it is the same class of
bug as the SLEEP overflow above.

**W14-23 stage 2, 2026-09-19, release build — corrects stage 1's own
"unbounded X" framing; the three ranked mechanisms in the ticket are
each traced and rejected as stated; the real destination-corruption
event is pinned to one instruction pair; no fix shipped, ticket stays
BLOCKED.**

Stage 1 described the defect as "X has no bound and no reset." That is
wrong, and the trace that corrects it is worth recording so it is not
retried: `title_probe`'s new `PROBE_TIMERLOG`/`PROBE_PACKETLOG` env vars
(module doc has the contract) plus a manual disassembly of ARAM
`$0970-$09E2` (dumped clean at instruction 80,000, before the region is
corrupted) show the receive loop is **not** an unbounded index. It is:

```
09B4: E4 16        MOV A,$16
09B6: 64 F4        CMP A,$F4
09B8: F0 FC        BEQ $09B6        ; spin for the CPU's next $2140 write
09BA: E4 F5        MOV A,$F5
09BC: D5 12 4B     MOV !$4B12+X,A   ; self-patched operand at $09BD/$09BE
09BF: E4 F6        MOV A,$F6
09C1: D5 13 4B     MOV !$4B13+X,A   ; operand at $09C2/$09C3
09C4: E4 F7        MOV A,$F7
09C6: D5 14 4B     MOV !$4B14+X,A   ; operand at $09C7/$09C8
09C9: E4 F4        MOV A,$F4
09CB: 2E F4 FB     CBNE $F4,$09C9
09CE: C4 F4        MOV $F4,A        ; echo the command byte back
09D0: C4 16        MOV $16,A
09D2: 60           CLRC
09D3: 7D           MOV A,X
09D4: 88 03        ADC A,#$03       ; X += 3, with the carry OUT kept
09D6: 90 09        BCC $09E1        ; no page rollover this time -> done
09D8: AC BE 09     INC !$09BE       ; three carries into the self-patched
09DB: AC C3 09     INC !$09C3       ; page bytes -- ALL THREE in lockstep
09DE: AC C8 09     INC !$09C8
09E1: 5D           MOV X,A
09E2: FE D0        DBNZ Y,$09B4     ; the loop IS bounded, by Y
```

`X` is the low byte of a 16-bit destination pointer with an explicit
`ADC`/`BCC` carry chain into the three page bytes, and the outer loop
is bounded by `DBNZ Y`. **Ruled out by this disassembly, not by
argument: an unbounded raw index.** `spcring` across the actual run
confirms the carry path is taken (`... 09D6 09D8 09DB 09DE 09E1 ...`),
and it is legitimate, ordinary buffer-fill code — the same shape reused
for three parallel per-voice buffers, seeded once from a 16-bit pointer
in `$0000`/`$0001` via `MOVW YA,$00` / `MOV !$09BD,A` / `MOV !$09BE,Y`
(and the `+1`/`+1` pair for the second and third buffers) at ARAM
`$0986-$09A1`.

**The three ranked mechanisms, traced and rejected as stated:**

1. **Consumer never runs (timer0/1-paced).** `PROBE_TIMERLOG` on the
   full run to the halt shows timer0/timer1 (target `$00` = divide by
   256) **enabled from instruction 74,213** — essentially the whole
   run — and only disabled at **instruction 988,617**. The pointer
   corruption this ticket chases happens at **instruction 984,014**,
   *before* the disable, not after. The consumer ran for the entire
   window that matters; "never runs" is refuted by the timer log
   itself, and the later disable is downstream of the corruption, not
   its cause.
2. **Duplicate packets from non-atomic `$2140-$2143` writes.** Refuted
   two ways. By protocol: the receive loop only gates on `$F4`
   (`$2140`) changing (`CMP A,$F4` / `BEQ`), and the CPU's own port
   ring (`PROBE_PORTS`) shows it writes `$2141-$2143` first and `$2140`
   last every time — there is no intermediate state for the SPC to
   observe as a phantom packet, because the SPC isn't watching the
   other three ports for a trigger. By count: `PROBE_PACKETLOG`'s
   totals over the whole run are `x_register_changes=17036`,
   `ports_in0_changes=22218` — the CPU changes port 0 *more* often than
   the receive loop consumes a packet, the opposite of the
   over-consumption this theory needs.
3. **Clock ratio (SPC starved relative to the main CPU).** Not
   reproduced. `catch_up_apu` (ticket W14-09) already runs the APU to
   the exact master-cycle count before every `$2140-$2143` access, so
   the two sides are synchronous by construction on this path, not
   free-running; the steady ~68-instruction cadence between packets in
   `PROBE_PACKETLOG`'s per-call lines (main CPU write loop timing
   unchanged for the whole run) shows no starvation or catch-up
   backlog forming.

**The actual corruption, pinned to one instruction pair.** None of the
three mechanisms explains the failure, so the search moved to *why* the
self-patched page bytes (`$09BE`/`$09C3`/`$09C8`) end up at `$09`
instead of the safe `$4B-$4D` range they start at. `PROBE_PACKETLOG`'s
new dp`$01` watch shows the 16-bit reseed pointer at ARAM `$0000`/
`$0001` — normally written only at boot (`$0000/$0001` set to
`$0048`/`$0012` around instruction 79,656, matching the `$4812`-ish
base the disassembly above shows, folded through the `+1`/`+1` seeding
pattern) — changes **again**, unexpectedly, at **instruction 984,014**:
`dp$01: 48->07`. A one-shot local trace on `Apu::write` (added and
removed for this session; not shipped) caught the writer: **the write
comes from the receive loop's own store instructions**, `MOV
!$4B13+X,A` at `$09C1` and `MOV !$4B14+X,A` at `$09C6` — i.e. by
instruction 984,014 the self-patched operands at `$09C2/$09C3` and
`$09C7/$09C8` have *already* carried down through page `$01` and `$00`
via the plain `INC`-on-carry chain (steps 09D8/09DB/09DE above, which
has no upper or lower bound and no periodic reset anywhere in this
disassembly), so the receive loop's own writes land on `$0000`/`$0001`
— **the exact bytes that seed the next reseed** — and stamp them with
whatever incoming command byte happened to be in `A`/`X` at that
moment (`$FB`/`$07`). The `$07` that lands in `$0001` is not a table
value or a deliberate page selection at all; it is leftover port data,
and it becomes the new page. Two more `INC`-on-carry steps (`$07->$08
->$09`, consistent with the ~55,000 instructions and ~9-10 more
`DBNZ`-bounded passes between instruction 984,014 and the halt at
1,039,833) walk that page onto `$09` — the driver's own code — matching
stage 1's `$09B3-$09B5` overwrite exactly.

**Not confirmed, and not shipped as a fix.** The three carry chains
(`$09BE`/`$09C3`/`$09C8`) are the game's own ROM bytes with no visible
bound or periodic reset in this disassembly window — law 5 forbids
patching around them, and this ticket's write scope (`IplBoot`, the
SPC700 core) has no way to add one without doing exactly that. Two
explanations remain open and neither is verified: (a) real hardware
never drives this loop through ~200 page-carries in one boot sequence
in the first place, meaning some other rate or gating difference (not
the three mechanisms above — clock ratio was checked and synchronous)
feeds this driver faster than real hardware would; or (b) a reset does
exist somewhere else in the ROM (a jump target this session never
reached, gated on a condition this emulator computes differently) and
this emulator fails to reach it. Distinguishing those needs either a
real-hardware capture of this exact boot sequence's `$2140` traffic
rate, or disassembling the full command dispatcher this loop is called
from (out of this session's budget) — **left for the next ticket, with
this trace as its starting point** so it does not re-derive the
disassembly above.

Named cause updated again: *the uploaded driver's own self-modifying
buffer-pointer arithmetic (an `ADC`/`BCC` carry into three page bytes,
looped via `DBNZ Y`) has no bound and no periodic reset in the window
traced; once the carry chain passes through ARAM page `$00`, the same
loop's in-flight stores corrupt the 16-bit reseed pointer that feeds
it, and the corrupted value walks the pointer onto the driver's own
code page.* Both Super Mario RPG dumps stay in the **uniform screen**
bucket; census not re-run (no code changed — `crates/rf-harness/tests/
title_probe.rs` gained two diagnostic env vars, `PROBE_TIMERLOG` and
`PROBE_PACKETLOG`, and nothing in `crates/rf-snes` changed). Gate at
close: see the commit trailer.

**W14-24 (Super Mario RPG boot upload overflows ARAM: the CPU-side
producer), 2026-09-19, release build — root cause found and FIXED. Not
one of the ticket's three ranked hypotheses: a fourth mechanism, found by
tracing the producer per the ticket's own method.**

Started from W14-23 stage 2's disassembly of the SPC700 receive loop
(ARAM `$09B4-$09E2`) and the standing question: why does the 65816 send
~17,036 packets when the buffer (`$4B12-$FFFF`) holds room for only
~15,439? `title_probe`'s `PROBE_PORTS`/`PROBE_RINGP` traced the producer
to a two-instruction spin at `C4:0541`/`C4:0545` (`CMP $2140` / `BNE`),
and `PROBE_DIS=c4:0400:0900` disassembled the surrounding routine.

**Hypothesis 1 (SA-1 ROM mapping) is exonerated by the disassembly, not
just by the register report.** The block-header setup at `C4:04E0-
C4:0539` reads its per-block length through `[$3A],Y` — an ordinary
direct-page-indirect-long read, not a super-banked `$2220`-`$2223` CXB-
FXB access — and `PROBE_SA1REGS=1` reports `sa1 unknown register writes:
none` for the whole run, matching W17-04's and W17-03's prior findings.
The producer never reads sample data or its length through SA-1-mapped
ROM on this path, so there is no mapping to check bank/offset against
fullsnes for. Hypotheses 2 (duplicate/non-atomic port writes) and 3 (SPC
echo semantics) were also not needed: the actual defect is upstream of
any of the three, in a piece of hardware none of them named.

**The producer's packet count comes from the SNES's own $4204-$4217
hardware divider, and this emulator's divider was finishing too late.**
`C4:04E6-C4:0501`: `REP #$20; LDA [$3A],Y` reads a 16-bit length from the
sample-header blob, `+2`, stores it to `$4204/$4205` (`WRDIV`), then
`SEP #$20; LDA #$03; STA $4206` starts a divide-by-3 (`$4206` = 16-step
divide start, `rf-snes`'s `MathUnit::start_divide`, `regs.rs`). The game
then spaces the divide's known 16-cycle latency with `INY; INY; STY $3D;
NOP; NOP; LDX #$FFFF` — the ordinary "do filler work while the divider
runs" idiom fullsnes documents — before reading the quotient: `LDA
$4215` (`C4:04FD`, high byte) then `LDA $4214` (`C4:0505`, low byte,
after `XBA`/`TAX` combine them into `X`, the packet countdown for the
`$0541` send loop).

New diagnostic (kept, matching the `PROBE_TIMERLOG`/`PROBE_PACKETLOG`
precedent): `title_probe.rs`'s `PROBE_MATHPC=hex[,hex]` prints the
`$4204-$4217` unit's `busy()`/`wrdiv`/`rddiv`/`rdmpy` state whenever the
CPU is about to execute an instruction at one of the given 24-bit PCs.
`PROBE_MATHPC=c404fd,c40539` on the unpatched tree caught it directly —
two blocks logged before the run:

```
MATHLOG n=79919 pc=C404FD busy=true  wrdiv=0014 rddiv=0003 rdmpy=0002 a=0003 x=FFFF y=0002
MATHLOG n=80890 pc=C404FD busy=true  wrdiv=0065 rddiv=8010 rdmpy=0005 a=0003 x=FFFF y=0002
MATHLOG n=80961 pc=C40539            wrdiv=0065 rddiv=0021 rdmpy=0002 a=8021 x=0001 y=0002
```

Both `$4215` reads land while `busy()==true` — the divide has not
finished. The first happens to be harmless (partial high byte `00`
matches the eventual correct high byte `00`). The second is not: dividend
`0x65`=101 by 3 is 33 remainder 2 (`0x0021`, correct high byte `00`), but
the shift register mid-divide reads `0x8010`, handing the high byte
`0x80` to `A`. Combined with the (correctly-settled-by-then) low byte
`0x21`, `X` becomes `0x8021` = **32,801** instead of **33** — a single
corrupted block asking the `$0541` loop to send 32,801 packets. The SPC
halts at 17,036 partway through that one block, consistent with W14-23's
numbers to the byte: this block alone dwarfs the entire 15,439-packet
buffer capacity, so nothing about the outer dispatcher or a second block
is needed to explain the overshoot.

**Root cause: `SnesBus::tick_math` stepped the divide/multiply unit once
per CPU *bus access*, and every access counted as exactly one step
regardless of how many real CPU cycles it actually cost.** Per fullsnes
("SNES Maths Multiply/Divide"): "set WRDIVB, wait 16 clk cycles, then
read the ... result", and "the 42xxh Ports are clocked by the CPU Clock,
meaning that one needs the same amount of 'wait' opcodes no matter if the
CPU Clock is 3.5MHz or 2.6MHz" — the divider's latency is 16 **CPU**
cycles, one per CPU cycle regardless of that cycle's bus cost. `INY`,
`NOP` and `XBA`'s second cycle cost real CPU cycles but touch no address
in this core (`ops.rs`'s `0xEA => {}` for `NOP` is representative), so
`AccessCost` — correctly scoped to what it can actually charge, per
`speed.rs`'s own doc — counted zero accesses for them, and the divide
fell behind real CPU-cycle time whenever a game filled with that kind of
instruction between the `$4206` write and its result read.

**Traced with a new diagnostic, `PROBE_ACCESSWIN=start:end`** (sums
`accesses` and `spent` master cycles over every instruction from `start`
to `end`), over the exact window the game spaces with `INY; INY; STY
$3D; NOP; NOP; LDX #$FFFF` between the `$4206` write (`C4:04F1`) and the
`$4215` high-byte read (`C4:04FD`): **15 accesses, 118 master cycles**.
Under the old model (one step per access), 15 steps is one short of the
16 the divide needs — exactly matching the observed `rddiv=0x8010`
(step 15 of 16 in this crate's own shift-and-subtract algorithm,
confirmed independently by hand-simulating `MathUnit::step` in Python
against the same `wrdiv=0x65`/divisor-3 inputs). Over the full window to
the `$4214` low-byte read (`C4:0505`, two more instructions further):
**26 accesses, 202 master cycles** — the old model's 26 steps has long
since finished the divide by then, which is why the low byte read at
`C4:0505` was always correct and only the high-byte read at `C4:04FD`
was exposed.

**The fix is not "internal cycles are now counted" — no internal-cycle
time is added anywhere.** `master_cycles`/`spent` still comes entirely
from `AccessCost`, which still charges bus accesses only; an
internal-only instruction still contributes zero extra `master_cycles`
for real CPU cycles it spent touching no address. What actually changed
(`crates/rf-snes/src/regs.rs`, `bus.rs`, `system.rs` — narrowly scoped,
not the full cycle-accurate 65816 executor `W6-02a` still defers):
`MathUnit` gained `tick(&mut self, master_cycles: u32)`, which re-buckets
that same access-based `master_cycles` figure into
[`crate::cpu::speed::FAST`]-sized (6-master-cycle) steps instead of one
step per access. Since real accesses on this machine cost 6, 8 or 12
master cycles (`speed.rs`'s table), this over-credits every access
slower than `FAST` — a `SLOW` (8) access is worth 8/6 ≈ 1.33 re-bucketed
steps, an `XSLOW` (12) access worth 2 — which is exactly why the 118
master cycles above (mostly `SLOW`-region WRAM/register/ROM accesses)
convert to 118/6 = 19 re-bucketed steps, comfortably past the 16 needed,
where the old model's 15 raw accesses were not. This is a coarse,
deliberate compensation for the undercount above, not a real internal-
cycle model, and the direction is safe either way: over-crediting can
only make the divide finish *sooner* in emulated time than 16 real CPU
cycles would, never later, so software that waits out the documented
latency (the correct thing to do) is unaffected, while software that
was exposed to a partial result before (this bug) now more often is not.
`SnesBus::tick_math` now just forwards to `MathUnit::tick`, and
`system.rs` feeds it `spent` (already computed for the master clock and
the APU catch-up) instead of `counting.accesses`. A `carry: u32` field
(sub-6-master-cycle remainder) was added to `MathUnit` and to its
save/load state so a mid-divide save/load round-trip stays exact. This
does **not** add per-opcode internal-cycle accounting to the rest of the
timing model — `master_cycles`/scanline/frame timing are unchanged and
still have the same internal-cycle undercount W6-02a's deferred cycle-
accurate executor is for; only the math unit's own clocking moved from
"one step per access" to "one step per 6 master cycles of access cost",
which happens to compensate for the right thing in the direction that
only ever helps.

**Known residual, not fixed here** (documented on `MathUnit::tick`): the
`$4206` write happens mid-instruction, but `tick_math` is only called
with that whole instruction's cost after the instruction finishes, so
the handful of accesses the triggering instruction made *before*
reaching the write (its own opcode/operand fetches) are also credited
toward the divide's latency — over-crediting the first `tick` by a few
re-bucketed steps. Harmless for every case checked here (`gilyon_cputest`
and the SMRPG fix both have comfortable margin), but a title timed
exactly against the 16-step boundary could still see a read complete a
step or two early. Fixing that needs sub-instruction timing, which is
what W6-02a's deferred cycle-accurate executor is for, not a change to
this method.

Three new unit tests cover `tick` directly (`crates/rf-snes/src/tests/
regs.rs`): `tick_completes_a_divide_after_its_real_master_cycle_latency`
reproduces the exact `101 / 3` case above as a `MathUnit`-level assertion
(busy one cycle short of the 16-cycle latency, settled and correct at
exactly 16); `tick_accumulates_partial_master_cycles_across_calls` checks
the `carry` field actually does its job — feeding the same total master
cycles through many small, non-`speed::FAST`-aligned `tick` calls (the
way `step_one` really drives it, once per instruction) must match one
large call; and `bus_tick_math_gates_the_divide_quotient_by_real_master_
cycles` drives the same `101 / 3` case through `SnesBus`'s `$4204`-
`$4217` window the way the CPU actually would, asserting a `$4215` read
one cycle short of the latency is not yet the final high byte and a read
at the latency is.

**Verified fixed**, same `PROBE_MATHPC` trace on the patched tree: both
blocks now show `busy=false` at `C4:04FD` and the correct small counts at
`C4:0539` (`a=0006`, `a=0021`, `a=000F`, `a=001B`, `a=0042`, `a=006C`,
`a=00A8`, `a=0066`, `a=00A5`, `a=0096`, ...). `PROBE_STOP_ON_SPC_STOP=1`
with `PROBE_INSTR=1050000` no longer halts at all — the SPC reaches real
driver code (`spc distinct_pc=61`, addresses in the `$02xx-$03xx` driver
range, not stuck at `$09B6`) and the PPU shows real state
(`forced_blank=false cgram_nonzero=29 vram_nonzero=1561`).
`PROBE_MODE=frames PROBE_FRAMES=600` reports `varied_at=Some(27)` — the
title renders by frame 27.

**Gate**: `cargo fmt --check` clean; `cargo clippy --workspace -- -D
warnings` clean; `cargo test -p rf-snes` — 357 passed (three new, above),
0 failed (plus the
existing ignored/gated suites); `cargo test --workspace` — **2213
passed, 0 failed**. The four suites this ticket's brief named as the
oracles for exactly this class of change all pass on the patched tree:
`singlestep_spc700_vectors` (256,000/256,000), `singlestep_65816_vectors`
(5,080,000/5,080,000, same "254 of 256 opcodes, `$44`/`$54` excluded"
report as before this ticket — unrelated MVN/MVP cycle-truncation gap,
not touched here), `spc_timer_reports_pass` ("PASSED TESTS"), and —
the discriminating one for a math-unit timing change —
`gilyon_cputest`'s `cputest_full_reports_success_and_every_test_passes`:
`test_num=0x0649/0x0649, ROM says "Success"`.

**Census children** (`boot_census_child`, per-title, not the full
orchestrator run — law of this ticket's brief): **both Super Mario RPG
dumps now exit 0** (USA and USA/Europe Virtual Console), and the four
regression canaries all still exit 0 unmoved: **Kirby Super Star**,
**Kirby's Dream Land 3**, **Wild Guns**, **Super Mario World**. The full
SNES census re-run (to move the bucket counts and name every title this
touches) is the orchestrator's — not run here, per this session's
instructions.

**Determinism**: no core state field was made non-deterministic; `carry`
is included in `MathUnit`'s save/load so a save/load round trip mid-divide
reproduces the same completion timing. No RNG, wall-clock, or thread
dependency was introduced.
