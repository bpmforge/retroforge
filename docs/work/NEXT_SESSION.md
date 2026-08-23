# NEXT SESSION — resume point (rewritten 2026-08-23)

**Board: 139 of 140 done.** One ticket is open — W7-08 — and it is
blocked on a spec rather than on effort. Everything else is closed.

Note the shape of that block before assuming it is a to-do: the oracle
that would judge W7-08 now works and says what is missing. What it names
is architectural.

Read `plan.json`'s notes for the ticket you pick up. They are long on
purpose: several of them record a claim that turned out to be FALSE, and
the correction is usually the useful part.

---

## 1. ONE TICKET OPEN — W7-08, blocked on a spec we do not have

W7-08's remaining criteria need a **cycle-accurate S-DSP**, and that is
blocked on a documented per-cycle schedule rather than on effort.

### What is already true

* **Criterion 1 is met.** Gaussian interpolation, ADSR/GAIN, echo with its
  ARAM ring buffer and FIR, pitch modulation and noise — all implemented,
  wired into `mix()`, unit-tested.
* The S-DSP is **reachable** (`$F2`/`$F3` register file) and **clocked**
  (32 SPC cycles per sample). Before this it was wired to nothing.
* blargg's four SPC ROMs **run and report real verdicts**, with no boot
  ROM required. That is the first external oracle this project's S-DSP
  and SPC timing have ever had.
* Three real accuracy bugs are fixed, each verified against a source: the
  EDL=0 echo buffer size, the FIR's per-tap shift and wrap/clamp/mask
  sequence, and ENVX/OUTX being writable storage rather than read-only.

### What remains, precisely

`spc_dsp6` reports `Failed 03`. Tracing every `$F2`/`$F3` access shows the
failing check writes `$88` to ENVX and then **counts how many reads
survive before the DSP overwrites it** — 4, then 1, then 1, then 0. It is
measuring **where inside the 32-cycle sample** voice 0's ENVX is written.
`Dsp::mix` updates all eight voices at once.

So criteria 2 and 3 need a 32-step state machine placing every register
and memory access at its exact SPC cycle. That is what this suite exists
to test — blargg's README calls his "the first DSP emulator with cycle
accuracy… whereas previous DSP emulators emulated these only to the
nearest sample".

### Why it is BLOCKED and not merely hard

**The complete 32-cycle schedule was not located.** Fragments are
documented — sneslab has echo left inserted at cycle 22 and written at 29,
right at 23 and 30, FIR coefficients read across 22–25; anomie has EFB at
26 and PMON at 27 — but no full table for all eight voices.

Without a spec, building this means guessing placements and using a
pass/fail ROM as a search oracle. That is slow, and it can converge on
something that passes without being right — this project has already
caught three plausible-but-wrong models in one week.

There is also a **licensing question that is Brad's, not mine**: the most
likely complete source is emulator source code (snes_spc, ares), which is
GPL. Reading it to reimplement is the same tainting question as the IPL,
in a different family. Not taken unilaterally.

### What would unblock it — any one of

| | Option |
|---|---|
| **(a)** | A doc carrying the complete per-cycle table. anomie's `apudsp.txt` is the canonical one; its host's certificate has expired, so a live mirror is needed |
| **(b)** | A ruling that reading GPL emulator source to extract **timing facts** (not code) is acceptable |
| **(c)** | Hardware measurement — needs a console and a capture rig |
| **(d)** | Amend criteria 2 and 3 to what a sample-granular DSP can honestly claim, and file the cycle-accurate S-DSP as its own ticket |

**(d) is probably the right shape.** Cycle accuracy is a phase of work,
not a criterion buried inside an 8-point DSP ticket.

## 2. The gate is TEN commands now, and the blind spot is narrower

```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace                                   1645 passing
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
