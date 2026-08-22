# NEXT SESSION — resume point (rewritten 2026-08-21, end of second run)

Supersedes `docs/work/HANDOFF.md`'s "Current state"/"START HERE" sections;
that file's *process* sections (read-order, block-note discipline) still
apply. Read `CLAUDE.md` first.

**Branch `main`, tree clean, both remotes at `0c87f44`. 133 of 140
tickets done. `plan.json` reports `claimable now: (none)`.**

---

## 1. WHAT BRAD NEEDS TO DO — this is the whole blocker list

Seven tickets remain and **not one is blocked on programming.** Four need
a human decision or an external artifact; two wait on those; one is held
by board policy. Nothing here needs more code written first.

### A. ~~Look at one frame~~ → RESOLVED 2026-08-21, no human action needed

**Investigated and answered: per-dot has a WRITE-ATTRIBUTION defect. Do
not re-pin RotZoom; the pinned `6ebfb8ce` is right.**

Re-applying segmentation and diffing every golden: 31 of 32 ROMs
byte-identical, RotZoom differs by 105 of 57,344 pixels — **all on row 0**,
one span from x=26 rightward. A change confined to the first visible line
is frame-setup writes landing on line 0, not a mid-line effect.

Tracing every write treated as mid-line found `$2104` OAMDATA (16,307),
`$2122` CGDATA (19,455 at one dot), `$210D` BG1HOFS (9,839), and
`$211C`/`$211D` — RotZoom's own Mode 7 matrix — at dots 1-3. Bulk
OAM/CGRAM transfers happen in **vblank**. `timing.dot()` cannot tell
"dot N of the visible line" from "somewhere in vblank".

**Remaining work on W7-15 crit 1** is therefore attribution, not
composition: tag each write with **(line, dot)** and exclude
vblank/forced-blank, then re-measure. The segmentation design itself is
sound — 31 unchanged ROMs prove it is inert when it should be.

**Also: criterion 3 is probably fine as written.** An earlier note called
it unsatisfiable; if RotZoom's only change came from the attribution bug,
correct attribution should leave it unchanged. Do not amend it — fix
attribution and re-measure.

### B. RESOLVED 2026-08-21 — two defects found and FIXED. W7-06 is closed; W7-10 is now claimable.

Brad looked at the six mode-5 Interlace frames and called them jagged. He
was right, and the cause was not the half-dot ORDER that two earlier
passes of this ticket kept reasoning about.

**Defect 1 — no double-rate fetch.** Modes 5/6 are ONE 512-wide picture
whose even columns feed the sub screen and odd columns the main screen.
The code instead interleaved two independent 256-wide renders — which is
*pseudo-hires*, a different mechanism that happens to share the same 512
output. Both screens therefore fetched the same background at the same
scroll, so the 512 picture was the 256 picture with every column
duplicated: **55,842 of 57,344 half-dot pairs identical on
InterlaceFont**, each plane individually sharp, glyph edges serrated
wherever the remaining 2.6% disagreed. `bg::HiresPhase` now separates the
two routes; `hires_requested()` still answers yes to both, because the
question it asks ("does this line emit 512 dots?") is genuinely different.

**Defect 2 — hires tile width.** In modes 5/6 the `$2105` size bit selects
**16×8 or 16×16**: a tile is sixteen half-dots wide, so 32 tiles span the
line exactly as they do at 256. Treating it as eight read 64 tiles where
32 exist, wrapped a 32-wide tilemap, and **rendered the whole line twice
side by side** — which is how it was caught, from the PeterLemon font
chart appearing twice across the frame.

**Also corrected:** mosaic must snap in the same space the fetch walks
(512 on a hires line). Snapping the dot and scaling afterwards makes both
half-dots of a block resolve to the same *pair* rather than the same
pixel, filling every block with a two-colour vertical stripe instead of a
flat colour. Both versions were rendered and looked at; the reasoning for
the wrong one was perfectly plausible.

**Evidence.** InterlaceFont renders the full printable-ASCII chart, sharp.
Moogle, RPG, Scroll, MystHDMA and SimpsonsHDMA all render their intended
pictures. MosaicMode5 shows flat mosaic blocks — which also discharges the
criterion folded in from the withdrawn W7-17. All seven are now pinned
goldens. Three new unit tests, each mutation-checked to fail on its own
defect and only its own. Workspace 1,617 passing; nine-command gate green.

**One clause still needs you:** criterion 1's *"and the renderer
letterboxing"*. It is `rf-renderer`, and W7-06's write_scope is
`crates/rf-snes/** + scripts/**` — unchanged through all three passes. The
core half (SETINI decode, 224-vs-239 visible lines, vblank shortening)
shipped in the first pass. This needs **either an rf-renderer ticket or a
scope amendment**, and it was not filed unilaterally because splitting a
ticket is your call.

### C. Rule on an artifact → unblocks W9-06 crit 3

Needs a **licence-clear community HD pack**. `docs/SCOPE.md` puts
third-party assets as gate fixtures in the OUT column, so this needs
either a fetch-only artifact designation (`tests/rom-manifest.toml` has
`LicenseStatus::NoLicenseGrantFetchOnly` for exactly this posture, which
means widening scope to `rf-harness` + `tests/`) or an amended criterion.
**Do not satisfy it with a pack we authored** — that would prove the
importer against its own author, which is the whole point of the
criterion.

### D. Designate a reference set → unblocks W7-08 crit 3

An audio RMS comparison needs a **designated** SPC reference set. None is
designated or fetched. **Check it is obtainable and licence-clear before
claiming**, or this stalls the way W7-05/W7-08/W7-11 already did on
artifacts that did not exist.

### E. Release the hold → W5-05

`W5-04`/`W5-05` are **held** — the board excludes them from unattended
claims. A human can take W5-05 (release engineering v0, 3 pts) whenever
you want it.

### F. RESOLVED 2026-08-21 — CI was never going to run. GitHub is storage now.

`docs.yml` had never executed, and neither had anything else. Every
workflow run since roughly **2026-08-07** was rejected before starting:

> The job was not started because an Actions budget is preventing further use.

166 failures to 34 successes, both workflows, every commit — and **not one
of them a code failure**. The last genuinely green run was 2026-08-07.

**Ruling (Brad, 2026-08-21): treat GitHub as storage and stop worrying
about CI. Made PERMANENT 2026-08-22: the Actions budget is not being
increased**, so hosted CI will not run again and no claim anywhere in this
repo may cite "green in CI" as evidence. One such claim was found and
corrected on W5-05, which asserted the 3-OS build matrix was green.** A red badge that means "no minutes left" is worse than no
badge, because it trains everyone to ignore the one that would have meant
something. Recorded in `CLAUDE.md` under Build so the next session does not
rediscover red CI and panic.

**What was recovered.** `scripts/docs-gate.sh` now mirrors `docs.yml`:
doc samples, plugin-SDK examples, profile validation, and `mdbook build`.
The last of those **had never run anywhere** — the workflow was added and
its very first run was already budget-blocked. It passes, so there was no
latent breakage, and `docs/site/book/` is now gitignored (it was not, and
a `git add -A` would have committed the whole built site).

**What now runs NOWHERE — the honest cost of this ruling.** Treat changes
in these three areas as unverified:

1. **Linux.** Everything here is built and tested on darwin.
2. **The software-rasterizer GPU path.** CI ran the golden-frame GPU suite
   under `LIBGL_ALWAYS_SOFTWARE=1` on llvmpipe; locally it runs on Metal.
   A backend divergence is exactly what that job existed to catch.
3. **The cc65 deterministic fixture rebuild** of RF-Scroller,
   RF-Scroller-S and the SNES mirror-map fixtures from source.

`ci.yml` also carried debugger pay-for-use, the 5k-frame Accuracy-vs-
Enhanced MVP boundary, the un-profiled scroller replay, the UI smoke flow
with NFR-004 timing, and mode-invariant failure evidence. Several of those
have local counterparts in `scripts/local-gate.sh`; **which ones is not
verified**, and that audit is worth doing before relying on it.

The workflow files are left in place. They are correct and would run on a
**self-hosted runner**, which GitHub does not bill for on private repos
(the per-minute platform fee announced for March 2026 was postponed
indefinitely). Gitea Actions on the `origin` remote is the other free
path, and it is self-hosted by definition. Neither is set up, and neither
should be until distribution is a real requirement — see W5-05.

---

## 2. The gate is NINE commands, and still not enough

```
cargo fmt --all --check                                  OK
cargo clippy --workspace --all-targets -- -D warnings    OK
cargo test --workspace                                   OK — 1614 passing
scripts/validate-arch.sh                                 OK
node scripts/validate-plan.mjs                           OK
node scripts/validate-traceability.mjs                   OK
node scripts/validate-evidence.mjs                       OK
cargo deny --all-features check licenses                 OK   (ort, wasmtime)
node .github/scripts/verify-doc-samples.mjs              OK   (doc site)
```

**Three things all nine miss.** All bit this session:

1. **The `#[ignore]`d goldens.** A hires change shipped in `4ca5f93` broke
   `peterlemon_golden.rs` and the full gate stayed green through it. On
   anything touching the PPU, run
   `cargo test --release -p rf-snes --test peterlemon_golden -- --ignored`.
2. **Feature-gated code.** CI leaves `gamepad` off, so
   `#[cfg(feature = "gamepad")]` is never type-checked by the gate. A
   green default build hid a real compile error in W8-04. Use
   `cargo clippy -p retroforge --features gamepad --all-targets`.
3. **The hires path had no golden at all — CLOSED 2026-08-21, and worth
   keeping as the pattern.** Every mode 5/6 ROM sat on the excluded list,
   so `render_scanline_hires` was uncovered even by the `#[ignore]`d
   suite: deliberately swapping its half-dot order left all nine gate
   commands *and* that suite green, while two real defects sat in the
   code (§1B). Seven hires goldens are now pinned and re-running that
   swap FAILS. **The general form: an entry on an exclusion list is a
   hole in the gate, not a note about a ROM.** Before trusting a green
   run on any subsystem, check whether its ROMs are actually in
   `GOLDENS` or merely in `EXCLUDED`.

**Run the suite with nothing else running.** RF-L-10 (now fixed) made two
concurrent `cargo test` runs delete each other's evidence file; a red run
with a second cargo in flight is *suspect before it is believed*.

---

## 3. Read RF-L-11 before writing a doc comment

Four stale second-sources-of-truth were found and fixed in one session,
and the fourth **propagated into the plan**: a stale `dsp.rs` scope
comment told W7-08's notes the DSP was absent, and that was restated into
`plan.json` and `STATUS.md` as fact. Three layers of restatement, no
compiler anywhere in the chain.

1. Add a second place that must agree with a first → write the test in the
   **same commit**.
2. A scope claim in a doc comment needs an assertion or an expiry.
3. **Read the code before repeating a doc's claim about it.**

Full write-up: `docs/LESSONS.md` RF-L-11. RF-L-09 (unbounded walks, now
CLAUDE.md Law 8) and RF-L-10 (fixed temp paths) are the other two from
this run.

---

## 4. Standing follow-ups (none blocking)

1. **`rf-intake` is not wired into CI** (`.github/**` was outside W9-05's
   scope), so FR-PROF-007's "nothing activates without passing" is a
   property of the code path, not of the repository's automation.
2. **`GAME_PROFILES.md` §2 does not document `[decode.room_grid]`** —
   `RoomGridSpec`'s doc comment is the specification for now.
3. **An RSS/wall-clock cap around `cargo test --workspace`** (RF-L-09's
   proposal), still unbuilt and still the higher-value of its two guards.
4. **Commercial-title profiles are permitted and unused** (W9-08 ruling) —
   every claim needs an FR-PROF-003 citation the loader enforces.
5. **No publishing step for the doc site** — `docs.yml` uploads the book
   as an artifact; Pages is a repository-settings decision.
