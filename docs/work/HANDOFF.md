# HANDOFF — resume point for the next session (rewritten 2026-08-04)

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

## Current state (verified 2026-08-04, commit `ac5e165` on main, both remotes)

**16 tickets done. Workspace tests: 358 passing / 0 failed / 1 ignored.**
Nothing `in_progress`, nothing `blocked`, tree clean.

| Ticket | What landed |
|---|---|
| W0-01 | CI pipeline + arch validator |
| W0-02 | rf-cart: iNES/NES2.0/SNES parse, RA-convention normalized hashing |
| W0-03 | rf-harness: manifest fetcher, blargg $6000 protocol, accuracy table + waivers |
| W0-04 | rf-core-api: EmulatorCore/CoreSink/StateView/CoreEvent/PpuPixel, zero deps |
| W0-05 | rf-state: `.rfstate` TLV container + zstd + bincode 2 |
| W0-06 | cargo-deny licence gate (NFR-011) |
| W0-07 | manifest `[[git_artifact]]` kind + local evidence gate |
| W1-01a | 6502: 151 official opcodes, cycle-stepped |
| W1-01b | 6502: 105 unofficial opcodes + interrupt edges |
| W1-02 | NES bus + NROM + OAM DMA + controller strobe |
| W1-03 | nestest golden trace 8991/8991 byte-exact + reset/power-on sequence |
| W1-04a | PPU background: loopy regs + fetch pipeline + CoreSink emission |
| W1-04b | PPU frame-timing edges + analytic golden frame |
| W1-05a | PPU sprites: secondary OAM, 8-sprite limit, buggy overflow flag |
| W1-05b | sprite-0 hit (sprite_hit 11/11) + VBL/NMI wiring (ppu_vbl_nmi 4/10) |
| W1-06 | frontend shell: eframe window, CPU blit, frame stepping, FM-01 containment |

Board: 67 tickets / 379 pts · validators green · traceability 101/101.
Toolchain pinned **1.94** (rust-toolchain.toml — never change to "stable").

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

1. **W1-07** — keyboard input + InputFrame + replay log. Builds directly on
   W1-06's shell. **See "Needs a human" below first.**
2. **W1-05c** — the sub-cycle VBL/NMI ceiling. Higher risk than its 5 points
   suggest; read its notes before claiming.
3. **W2-02** — mappers; scope already fixed, and it owns extracting NROM into
   a real `Mapper` trait.

Claim = set `in_progress` in plan.json + commit that change first.

## Needs a human (blocks nothing mechanically, but W1-07 builds on it)

**W1-06's visual criteria are UNVERIFIED.** The window painting, the native
ROM open dialog, and transport-button response could not be checked in a
sandboxed macOS session (no screen-recording / assistive-access grant). The
binary runs as a real GUI process with no panic and no winit/wgpu init error
— which is not the same as "the window works". Run `./target/debug/retroforge`,
open a ROM, confirm frames appear and pause/step respond, before layering
input on top.

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
