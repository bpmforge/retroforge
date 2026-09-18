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

**The second step of the triage is `crates/rf-harness/tests/title_probe.rs`**
(ticket W14-11): an `#[ignore]`d, env-driven probe that instruction-steps
one title, samples where both CPUs spend their time, and prints the loops
each is stuck in with the register state — plus disassembly, ARAM dumps,
ROM searches and rings of port changes and PCs on request. The census says
*which* titles; the probe says *where to look*. Its module doc has the
env-var contract. Like the census it takes ROM paths from the environment
and is in no gate.
