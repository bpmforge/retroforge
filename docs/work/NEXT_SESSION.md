# NEXT SESSION — resume point (rewritten 2026-08-23)

**Board: 137 of 140 done.** Everything that can be finished without a
ruling has been. The three open tickets each need a decision from Brad,
and each is a one-line answer rather than an investigation — the
investigation is already recorded on the ticket.

Read `plan.json`'s notes for the ticket you pick up. They are long on
purpose: several of them record a claim that turned out to be FALSE, and
the correction is usually the useful part.

---

## 1. THE THREE DECISIONS — this is the whole blocker list

### A. W7-13 criterion 2 — which oracle? (5 pts, blocked)

*"The undisbeliever snes-test-roms set is green."* It cannot mean 29
pinned hashes, and the measurement says why: **two distinct pixel hashes
across 29 ROMs.** 13 render one identical 7-index frame, 16 render an
entirely blank 1-index frame. What these ROMs test — INIDISP brightness,
forced-blank timing, DMA bugs — is exactly what law 4 keeps out of the
indexed pixel stream.

W7-15 created a signal that did not exist before: per-line **mid-line
write records**, which give 7 distinct hashes and separate the
`inidisp_hammer` family cleanly (1,988 writes over 70 lines, 5,460 over
224, 1,724 over 101, 1,844 over 108, one ROM with exactly one write).

* **(a)** Pin those ~8 on `(pixel hash + write hash)`. Discriminating —
  but it pins OUR INSTRUMENTATION rather than rendered output, and
  couples those goldens to `Ppu::is_segmentable`.
* **(b)** Amend criterion 2 to the subset the index domain can cover.
* **(c)** Leave it at 2 pinned and record the limit.

All 29 ROMs are already accounted for (2 pinned, 27 excluded) and every
excluded one is RUN and reported each pass, with its measurement written
beside it. The 8 in family (c) say *"do not pin until the ruling lands"*.
Criteria 1 and 3 are green.

### B. W7-08 criterion 3 — the IPL (8 pts, blocked)

blargg's SPC test ROMs read `$FFC0` as DATA, compare it against `$CD`,
spin forever otherwise, and then `JMP !$FFC0` to execute it. They need a
**real SPC700 boot ROM**, which this project will never contain (law 5;
Brad's 2026-08-20 ruling extended it to "not in a fetch list either").

`Apu::set_ipl_rom` is built and is the door that ruling left open: point
`RF_SPC_IPL_ROM` at a 64-byte dump of your own console's IPL and the suite
runs; without one it SKIPS. **No bytes were added to this repo.**

* **(a)** Accept user-supplied-only. Criterion 3 is then verifiable by
  anyone with a dump, and unverifiable here.
* **(b)** A **clean-room IPL** — write the documented boot ROM ourselves.
  It would be our code, not Nintendo's bytes, and blargg only compares the
  FIRST byte before jumping, so functional equivalence is what it needs.
  The risk is real and is why this is not a coding decision: a faithful
  reimplementation of a 64-byte ROM plausibly converges on identical
  bytes, which is exactly the line law 5 draws.
* **(c)** Amend criterion 3 again.

Criterion 1 is met, and criterion 2 is half met (SPC timing suites pass;
BRR sample-exactness is unverified and blargg is what would verify it).

### C. W5-05 — the hold, and what survives the CI ruling (3 pts, todo/held)

Criterion 1 (tagged 3-OS artifacts) is unreachable for **two independent**
reasons: Brad's 2026-08-19 ruling that GitHub is code storage for a
private repo, and the 2026-08-22 ruling that the Actions budget is not
being increased. Cross-building Windows and Linux from the darwin dev
machine is not a workaround worth pretending to — wgpu makes it awkward,
and an artifact nobody has RUN on the target OS is not evidence of a
release.

Criteria **2, 3 and 4 need no CI at all** and are real work whenever you
want them: the golden `.rfstate`/`.rfreplay` fixture archive
(FR-STATE-005), the accuracy table in release notes (TESTING.md §5), and
the migration drill (R-F3 — which needs its own small ruling, because v0
has no previous release to drill against).

---

## 2. The gate is TEN commands now, and the blind spot is narrower

```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace                                   1643 passing
scripts/validate-arch.sh
node scripts/validate-plan.mjs
node scripts/validate-traceability.mjs
node scripts/validate-evidence.mjs
cargo deny --all-features check licenses
node .github/scripts/verify-doc-samples.mjs
scripts/docs-gate.sh                                     needs mdbook
```

**GitHub CI is not one of them and never will be again** (ruling
2026-08-22, recorded in `CLAUDE.md`). Nothing in this repo may cite "green
in CI" as evidence; one such claim was found on W5-05 and corrected.

**What all ten still miss:**

1. **`#[ignore]`d suites.** `cargo test --workspace` skips them. This is
   not theoretical: a hires change shipped broken in `4ca5f93` behind a
   green gate, and a deliberate half-dot swap left every command green
   through three passes of W7-15. **`scripts/local-gate.sh` now runs ten
   of them**, including the four that ran nowhere until 2026-08-23
   (`undisbeliever_golden`, `region_golden`, `blargg_spc`,
   `mesen_hdpack_real`). Run it on anything touching the PPU, APU or DMA.
2. **Feature-gated code.** CI left `gamepad` off, so
   `#[cfg(feature = "gamepad")]` is never type-checked. Use
   `cargo clippy -p retroforge --features gamepad --all-targets`.
3. **Linux, the software-rasterizer GPU path, and the cc65 fixture
   rebuild.** These only ever ran on CI. Treat changes in those areas as
   unverified. Self-hosted runners are free on private repos and Gitea
   Actions is available on `origin`; neither is set up, and neither should
   be until distribution is a real requirement.

**Run the suite with nothing else running.** RF-L-10 (fixed) had two
concurrent `cargo test` runs deleting each other's evidence file; a red
run with a second cargo in flight is *suspect before it is believed*.

---

## 3. An exclusion is a hole in the gate, not a note about a ROM

This bit the project twice in two days and is worth internalising.

* Every mode 5/6 ROM sat in `EXCLUDED`, so the hires path had **zero**
  coverage while two real defects lived in it.
* The three HiColor ROMs sat in `EXCLUDED` on the reason *"needs the
  sub-screen this core does not have"* — which W7-16 made false. Nobody
  re-read it. They turned out to be perfectly pinnable (155/240/155
  distinct colours) and are now goldens.
* 26 of 29 undisbeliever ROMs were in **neither** list: fetched, never
  run, never reported, invisible.

So: before trusting a green suite, check whether its ROMs are in
`GOLDENS` or merely in `EXCLUDED` — and re-read the reason.

---

## 4. Read RF-L-11 before writing a doc comment

**A doc comment is a claim about code, and it decays.** Corrected this
session: `render_scanline_hires` claimed a swapped half-dot order would
look "subtly soft" (it changes 9,012 bytes and is obvious); `apu/mod.rs`
claimed no program reads the IPL as data (test ROMs do, and they are what
verify this emulator); W5-05 claimed the 3-OS matrix was "green in CI".

When a note records a measurement, give the number. "97.4% of half-dot
pairs identical" survived three passes of a ticket; "looks wrong" would
not have.

---

## 5. Standing follow-ups (none blocking)

* **HDMA at its true H-position.** `hdma_run_line` fires from the frame
  loop at whatever dot the CPU reached, so per-dot treats HDMA as line
  setup. That is why `hdma-2100-glitch` and `hdma-21ff-glitch` still hash
  identically to each other. Fixing it is beyond segmentation.
* **Mode 6 is untested.** It is the only hires + offset-per-tile mode; no
  ROM in the corpus uses it, so the OPT-column and scroll-doubling choices
  there are made without an oracle.
* **The SNES core is not wired into the app.** `crates/retroforge` never
  constructs a `SnesSystem`, which is why W7-10 left a region toggle
  unbuilt — it would have been wired to nothing. `FramePacer` also
  hardcodes the NTSC period and takes no parameter.
* **`FrameBundleBuilder` truncates to one width per frame**, so a frame
  mixing 256- and 512-wide lines cannot be represented there.
