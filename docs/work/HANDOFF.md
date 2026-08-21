# HANDOFF — resume point for the next session (rewritten 2026-08-08)

> **Start at `docs/work/NEXT_SESSION.md` (2026-08-21).** It supersedes the
> "Current state" and "START HERE" sections below, which are stale. The
> process sections of this file — read-order, the seven-command gate,
> block-note discipline — are still current.

For a fresh coding session (any model). The design-review arc is DONE and
merged. **Your job is implementation: execute tickets from plan.json, one at
a time.** Do not redesign anything.

## Read in this order (skim, don't study)

1. `CLAUDE.md` — the laws (short, mandatory)
2. `MASTER_PROMPT.md` — session protocol incl. the **always-writable set**
3. `plan.json` — the board (67 tickets; schema note at top is authoritative).
   **Read the claimed ticket's `notes` array** — every trap the conductor
   already paid for is recorded there, per ticket.
4. `PLAYBOOK.md` — per-ticket loop, gate command, block-note discipline
5. The tail of `docs/STATUS.md` — per-ticket evidence for everything below

## Current state (verified 2026-08-08, commit `45f5188` on main, both remotes)

**45 tickets done, 45 todo, across 90 tickets / 437 pts. Workspace tests:
712 passing / 0 failed / 7 ignored.** Nothing `in_progress`, tree clean,
all seven validators green, `cargo deny check licenses` ok, exactly one
`wgpu` in `Cargo.lock`.

Per-ticket evidence for every close is in the tail of `docs/STATUS.md` —
that is the authoritative record, and it is long because each entry
carries the traps and corrections, not just the outcome.

**The four things the project owner asked for are all built end to end:**

| Goal | Where it landed |
|---|---|
| Whole-level / ultrawide view | W4-00 → W4-01 → W4-03a → W4-08 → W4-03b → W4-03d → W4-03c → W4-03e — ROM to screen, behind a toggle that defaults to Accuracy |
| De-flicker without erasing deliberate blinking | W3-05 (`SpriteHistorian`), red fixture from W2-10a |
| 4× integer scaling | W3-01b (integer + 8:7 PAR + 224-line overscan + tolerance harness) |
| Shaders | W3-02 (chain + nearest/sharp-bilinear/scanlines); W3-02a holds the rest |

Toolchain pinned **1.94** (`rust-toolchain.toml` — never change to
"stable"). **cc65 2.18 is required** for the RF-Scroller fixture
(`brew install cc65`); `fixtures/nes/rf-scroller/build.sh` is the single
build invocation and it verifies the checked-in hash rather than trusting
it.


## The gate is SEVEN commands (validate-evidence joined in W0-07)

```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
scripts/validate-arch.sh
node scripts/validate-plan.mjs
node scripts/validate-traceability.mjs
node scripts/validate-evidence.mjs
```

**`cargo deny check licenses` is in CI but NOT in that list** — a dependency
change can leave the local gate green and CI red. Run it explicitly whenever
you touch dependencies.

## START HERE — recommended order

Claimable now, highest leverage first:

1. **W4-02** (8 pts, profile loader + schema v0 + validator tool) — unblocks
   W4-05, W4-06b, W5-01 and W5-06. The single biggest unblocker left.
2. **W3-02a** (3 pts) — the remaining three shaders. **Read its notes before
   touching it:** RENDERER §4's licensing law is a *clean-room* requirement,
   not an attribution one, and `cargo deny` structurally cannot check a
   `.wgsl` file.
3. **W2-12** (5 pts) — five Tier-A blargg suites are fetched by the manifest
   and executed by nothing. A downloaded ROM is not a tested ROM.
4. **W3-01a** (3 pts) — renderer device-loss/shader-compile fallback, the
   hazard that stalled W3-01 twice.
5. **W2-11a** (1 pt) — needs a human decision first, see below.

`node scripts/validate-plan.mjs` prints the full claimable list every run.


## Needs a human — decisions and eyes, not code

**Two decisions that are the project owner's, not an agent's:**

1. **W2-11a — Alter Ego is not Public Domain.** `notes.txt` *inside*
   `alter_ego.zip` says "released as freeware, not Public Domain… rights to
   other components (game concept, characters, title, music) are reserved".
   The manifest says `license_status = "public-domain"`. That field is
   machine-readable and the no-vendor/no-rehost rule keys on it, so the
   label is not cosmetic. The conservative relabel to
   `no-license-grant-fetch-only` *tightens* handling and changes no
   behaviour — but D-001 is the owner's decision and calls the fixture PD.
2. **D-001 says "cc65/neslib"; W2-10 used cc65's own `-t nes` target.**
   They are different toolchains. Only `-t nes` was probe-verified, and it
   avoids vendoring third-party C into `fixtures/` entirely. If "neslib"
   was meant literally, that is a one-line veto and W2-10 reopens with a
   vendoring + licence step.

**Two things only a human running the app can confirm** (recorded as such
rather than claimed):

- **W4-03e** — the camera toggle, the fog, and the FM-13 "view too large,
  reduced" toast in a live window. Tests assert against real
  GPU-composited RGBA buffers, which is stronger than compile-checking,
  but nobody has looked at it.
- **W3-03** — the older "Layers (debug)" checkbox, still verified by
  compile only.


## Execution pattern that has been working (D-003 "one conductor")

Conductor in the main session; **Sonnet** subagents implement ONE ticket each,
**strictly serial** (WIP=1 is law; tickets share Cargo.lock and target/, so
parallel gates collide). The conductor owns all status mutation: claim before
spawning, set `done` only after an **independent** seven-command gate re-run.
Never accept a subagent's "gate is green" — RF-L-08 is this project's own
lesson that a green report hid 17 real gaps.

**Pre-flight every ticket before writing the brief.** This is where the
session's value came from — see "Pre-flight finds" below. Compile-probe
external APIs in a scratchpad `cargo new`; parse the actual artifacts; verify
the ticket's acceptance is *achievable* before briefing.

**Conductor may fix real defects, not just fmt/clippy nits** — precedent set
2026-08-03 with the `tempdir()` race. Criteria: in scope, small, verified,
and leaving it red would corrode the verification discipline. Record it in
the ticket notes and STATUS.

**One retry with evidence** for semantic gaps, then park blocked-with-evidence.

## Verification lessons — earned the hard way, do not relearn

- **MUTATION-TEST every ticket with no test-ROM oracle.** Green unit tests
  prove very little. Every real defect found this session was invisible to a
  fully green suite: W1-04a's `write_scroll` mask, W1-05a's sprite/background
  priority. Break a constant, confirm the *right* test fails, restore, confirm
  byte-identical. Mutate **code** — `ppu/scroll.rs` quotes nesdev pseudocode
  in doc comments that looks exactly like the logic beneath it (a first
  attempt mutated a comment and "proved" nothing).
- **A vacuous test looks exactly like a passing one.** Three occurrences: a
  sprite-priority test asserted on scanline 0, which renders no sprites
  because evaluation on line N feeds rendering on line N+1; an overflow
  assertion was tripped by phantom Y=0 sprites in zero-filled OAM; a
  termination test re-hit the same boot-artifact trap. **Where a pipeline has
  a delay, assert where the effect lands.** Pad unused fixture state with
  `$FF`, never leave it zeroed.
- **Tests that all start from a zero state hide whole bug classes.** The
  `write_scroll` bug survived because every test began at `t=0`, where OR-ing
  and replacing are indistinguishable. When a write should *replace* a field,
  test it from a dirty starting state.
- **Goldens must be ANALYTIC, never recorded.** W1-04b computes its expected
  frame from a closed-form formula with zero hash constants, and immediately
  caught a real bug W1-04a had shipped. A golden captured by running the
  emulator once and saving the output would have baked that bug in and looked
  just as green. Choose fixture parameters so an off-by-one *changes* the
  expected value (W1-04b used multipliers coprime with 256).
- **Anti-tamper your oracles.** Corrupting one line of nestest.log produced
  exactly one mismatch at the right line — that is what proves a diff is real
  rather than vacuous.

## Pre-flight finds — the pattern that paid off most

Each of these would have burned a session if discovered mid-ticket:

- **W1-05b: the ticket was unrunnable as written.** The manifest's
  `ppu-vbl-nmi` was blargg's *combined* build — 256 KB PRG, **mapper 1
  (MMC1)** — but only NROM exists and MMC1 is W2-02, two phases out. The ten
  `rom_singles/` are 32 KB **mapper 0** and run today. Swapping them also made
  the manifest agree with TESTING.md, which already said "10 sub-ROMs".
- **W1-06: TECH_STACK pinned an impossible pair.** wgpu 30.0 *and* egui/eframe
  0.35.0, while mandating lockstep bumps — but `egui-wgpu 0.35.0` needs **wgpu
  29.0.4** and 0.35.0 is the newest egui published. Corrected at the source.
- **W1-04a: the design doc's `PpuPixel` did not exist.** `EMULATION_CORES.md`
  documented `color_index`/`palette_group`; the shipped struct has
  `palette_index`/`sprite_id` and no `palette_group`. Coding from the doc
  would not compile. Corrected.
- **The board's most common defect: self-blocking `write_scope`** — a scope
  that makes the ticket's own acceptance impossible. Hit **five** times
  (W1-01a, W0-07, W2-02, W1-04a, W1-05b). **Always fix by adding exact paths,
  never by widening the glob** (Brad's W1-01a ruling). `W1-04b`, `W1-05a`,
  `W1-05b` were fixed as claimed; **`W2-03` still carries `mappers/**` alone**
  and is annotated. Check scope-vs-acceptance BEFORE claiming, every time.

## Rulings — do not re-litigate or "restore" the old wording

- **The sink is ACCURACY-EXACT** (Brad, 2026-08-03). `CoreSink` always carries
  the true hardware framebuffer; `dropped_by_limit` is always `false` on the
  NES path; W3-05's limit bypass reconstructs dropped sprites from OAM via
  `StateView`/SpriteHistorian. Full reasoning in
  `crates/rf-core-api/src/video.rs`; `EMULATION_CORES.md` §1 and §2.2 were
  corrected to match. *Why:* `video_scanline` carries one `PpuPixel` per x, so
  emitting a dropped sprite would displace what the CRT actually showed —
  violating law 6 and FR-MODE-002.
- **Licence exceptions are per-crate, never allowlist-widening** (NFR-011, and
  Brad 2026-08-03). deny.toml now carries 21 W1-06 exceptions: BSL-1.0 via
  arboard, Unicode-3.0 via webbrowser→url→idna, OFL-1.1/Ubuntu-font-1.0 for
  egui's bundled fonts. A feature-trim was tried first and is **impossible**:
  eframe 0.35 hard-enables egui-winit's `clipboard` and `links` regardless of
  its own `default-features`.
- **wgpu comes through `eframe::wgpu`, never a direct Cargo entry** — else you
  get two wgpu versions and `wgpu30::Device != wgpu29::Device`. Verify with
  `grep -c '^name = "wgpu"$' Cargo.lock` == 1 after any GUI dep change.

## Architecture invariants — inviolable

- **The master clock lives in the bus.** `NesBus::master_cycle` advances
  inside `CpuBus::read`/`write`, one tick per bus op — never by summing
  `Cpu::step`'s return. Single mutation site: `tick_master`, which also ticks
  the PPU 3 dots per CPU cycle. **`crates/rf-nes/src/cpu/` contains no PPU or
  DMA reference and must stay that way.** This is what makes OAM DMA's stolen
  cycles land for free, and DMC DMA plus the `$2007`/`$4016` RDY double-read
  are meant to arrive the same way. If a ticket needs `Cpu` to know about
  stalls, **stop and escalate**.
- **The seam's one known ceiling → W1-05c.** `cpu::exec::CountingBus` samples
  `nmi_line()` once per whole bus op, and the bus ticks 3 PPU dots strictly
  *after* a register read completes, so nesdev's sub-CPU-cycle "read `$2002`
  on the very clock the flag sets" race is unreachable. The one-clock-*early*
  case is reachable and implemented. Costs 6 of 10 `ppu_vbl_nmi` sub-ROMs.
  **Treat W1-05c as the highest-risk ticket in Phase 1**: its blast radius is
  the 2,560,000-case vector suite and the 8991-line byte-exact trace. Those
  two are the guardrails — a regression in either is a stop, not a
  re-baseline. Waivers expire **2026-11-01** and turn the gate red on their own.
- Determinism, layer boundaries, indexed-pixels-never-RGB: see CLAUDE.md and
  `scripts/validate-arch.sh`, which is the executable form.

## Test data & the evidence gate (W0-07)

Four **Tier-A-local** suites — their inputs are gitignored, so they cannot run
in CI: `nes6502` (2,560,000 vector cases), `nestest` (8991 lines),
`sprite_hit_tests` (11/11), `ppu_vbl_nmi` (4/10, 6 waived to W1-05c).

`cargo test --workspace` **MUST pass when `roms/` is absent** (skip cleanly) —
but that skip path is also the obvious way to fake success. So:
`scripts/local-gate.sh` runs the heavy suites and writes
`docs/evidence/local-gate.json`; `scripts/validate-evidence.mjs` fails if
evidence is missing, generated from a **dirty tree**, or **stale** (staleness
is `git merge-base --is-ancestor`, never timestamps). Touch
`crates/rf-nes/src/cpu` and the gate goes red until you re-run local-gate —
that is intended, and it has fired for real several times, including on the
conductor. CI checks out with `fetch-depth: 0` and the validator refuses to
run on a shallow clone; without both it would pass while enforcing nothing.

**Order matters: commit first, regenerate evidence on a clean tree, then
commit the evidence.** Never hand-edit that JSON.

Fetch ROMs with `scripts/fetch-test-roms.sh` (64 artifacts, 0 failures as of
2026-08-04). The 1.2 GB vector checkout is a `[[git_artifact]]`: a
`--filter=blob:none` sparse checkout at a pinned commit, integrity checked by
`git rev-parse HEAD` — a commit SHA is itself a content hash. It short-circuits
on a matching HEAD rather than re-cloning.

## Traps already paid for — do not rediscover

- **plan.json formatting**: 1-space indent AND `\uXXXX` escaping for non-ASCII.
  A `JSON.parse`→`JSON.stringify` round-trip reformats the whole file. **Always
  edit plan.json with a surgical text edit.**
- **RustCrypto 0.11** (sha2/sha1/md-5): `Digest::finalize()` returns
  `Array<u8,N>`, which does NOT impl `LowerHex`. `format!("{:x}", …)` is E0277.
- **`bincode 3.0.0` is NOT a release** — its lib.rs is `compile_error!`. Use
  bincode 2. Do not "upgrade".
- **cargo-deny 0.20.x** `[licenses]` is allow-list-only; `unlicensed`/
  `copyleft`/`deny` were REMOVED in 0.14 and now hard-error.
- **validate-traceability ordering**: F2 (ticket cites undefined FR/NFR) is a
  HARD failure; W1 (FR/NFR no ticket cites) is only a warning. So an SRS row
  must land in the same commit as — or before — the ticket citing it.
- **libtest runs a binary's tests in parallel threads sharing a pid** — a
  `tempdir()` keyed on pid+nanos races. Use the fixed `AtomicU64` pattern.

## Known debt (tracked, not drifting)

- **W1-05c** owns the sub-cycle VBL/NMI ceiling (above).
- **Duplicate vector runner**: rf-harness's `nes6502_evidence.rs` +
  `vector_json.rs` (~23 KB) is a peer implementation of rf-nes's `vectors.rs`
  + `json.rs`, forced because rf-nes's test code is `#[cfg(test)]`-gated. Both
  agree exactly (2,560,000 either way) but nothing mechanically prevents
  drift. A follow-up should promote one to a shared non-test module.
- **Catch-up scheduling path unbuilt**: `EMULATION_CORES.md` §1 requires both
  lock-step and catch-up to exist and for CI to diff them. Only lock-step is
  built (W1-04a). Owed by a later ticket.
- **`sprite_overflow_tests` protocol likely mistagged** in the manifest — it
  almost certainly shares `sprite_hit_tests`' pre-`$6000` runtime (a `$00F8`
  RAM result byte). Flagged in the manifest and TESTING.md, not fixed.
- **Holy Diver Batman contradiction**: DESIGN_REVIEW §4 records it as
  pinobatch/holy-mapperel; W0-03's research says that is a different project.
  Tier-B/nightly, not blocking.
- HP-4..7 in `docs/PREREQUISITES.md` still open. **HP-4 (GitHub Actions
  budget) is known-constrained** — which is why W0-07 moved heavy verification
  local. Vetoes on D-001..D-006 remain open. W5-04/W5-05 are `hold: true`.

## Rules a cheap model must not improvise around

1. ONE ticket per session. Stay inside `write_scope` + the always-writable set
   (own ticket status/notes · docs/STATUS.md append · Cargo.lock ·
   docs/TECH_STACK.md §2 row for any new dependency).
2. Verify EVERY external crate API against docs.rs or `~/.cargo/registry/src/`
   for the pinned version before use.
3. Full seven-command gate before closing. Then append one line to
   docs/STATUS.md and commit as `feat(W1-07): <summary>`.
4. Blocked? Write a `notes` entry: root cause · exact fix · why workarounds
   fail · "do not retry without X". Two blocked attempts = stop, leave for a
   human. **Blocked-with-evidence is success, not failure.**
5. No ROM bytes in git (NFR-006).
6. Push after merged work: `git push origin main && git push github main`
   (origin/Gitea may be unreachable off-LAN — GitHub always).
7. Commit trailer names the implementing model.

## The 2026-08-08 session's lessons — read these before writing a test

Twenty-nine tickets closed in one run. The failures were not in the code
so much as in what the tests *proved*, and the same shape recurred six
times. It is worth naming because it is invisible from inside:

**A test that EXERCISES a path is not a test that DISCRIMINATES it.** All
six of these passed their original gate:

1. `rf_cache::fsutil` — the leaf-symlink branch was documented as guarding
   dangling links, but its test pointed at an *existing* file, which the
   ancestor walk refuses on its own. Deleting the branch left all 20 tests
   green.
2. rf-enhance's FM-11 watermark test was **named** for coverage it did not
   have (unchanged background only, never scrolling).
3. `TripleBuffer`'s concurrency test detected its own race ~1 run in 60 —
   indistinguishable from no guard on a single CI run. **It let a real
   rollback bug ship** (W4-01a).
4. W2-10a's `$2002` sprite-overflow test measured a flag that *cannot*
   witness what it claimed, because rf-nes implements the authentic buggy
   hardware scan.
5. W3-01b's tolerance metric had two knobs and only one was doing any
   work.
6. W3-02's `nearest` stage was constructed, selectable and
   manifest-documented — and **never rendered a pixel** in any test.

The counter-practice that worked every time: **mutate the implementation
and require the specific test to fail.** Size a concurrency test until it
fails against the known defect *every* run, not sometimes. And check the
mutation itself is not vacuous — one of the conductor's own fog mutations
changed nothing, because both branches led to the same colour.

**Scope pre-flight paid for itself repeatedly.** Twenty tickets had a
`write_scope` that could not reach their own acceptance. The highest-cost
near-miss: W4-03d waited on a real scrolling fixture and then could not
have run it, because `validate-arch.sh` rule 3 forbids rf-enhance from
depending on a console core — **including under `[dev-dependencies]`**,
since the grep does not distinguish. That constraint binds rf-enhance,
rf-profiles, rf-debugger and rf-ai alike. Only `retroforge` and
`rf-harness` can drive a real core. Budget for it at pre-flight.

**Three findings that look like emulator bugs and are not:**

- `$2002` bit 6 (sprite-0 hit) reads set during vblank from the *previous*
  frame — the PPU clears it at dot 1 of pre-render, *after* vblank ends. A
  bare "wait until set" exits instantly on the stale flag; SMB1 uses a
  two-phase wait for exactly this reason.
- `$2002` bit 5 (overflow) fires on garbage, because the authentic buggy
  diagonal scan misreads tile/attribute/X bytes as Y.
- OAM DMA straddling dots 257–320 gets its OAMADDR reset mid-transfer,
  scrambling the copy. This is real hardware behaviour and cost two full
  passes to find. Note the conductor explicitly told an executor to *drop*
  this line of investigation; it was the actual mechanism.

**When a metric moves the wrong way after a change that should have
helped, that is information about the mechanism — not just a failed
attempt.** The gem count going 2→0 after a redesign was the clue that
identified the DMA straddle.

## Multi-session safety — a data-loss incident happened here

Two Claude sessions worked the same tree during W2-10a with no shared view
of each other's actions. The conductor ran `git checkout` over paths
holding **uncommitted** work and destroyed the keystone file of a change
set. A backup taken afterwards prevented a *second* loss, when the peer
was about to reconstruct from memory seven files that existed verbatim on
disk.

- **Commit code before verifying it.** That is already the board's
  discipline and it exists precisely to make a bad checkout recoverable.
- `git checkout <path>` discards uncommitted work with no confirmation.
- If a second session is active in the tree, **tell it when the tree
  changes under it** — and if you are that second session, stop and report
  a discrepancy rather than proceeding on your own model of the tree.
  That is what contained the damage both times.
