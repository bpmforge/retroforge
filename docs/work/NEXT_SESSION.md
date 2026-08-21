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

### B. Look at six frames → unblocks W7-06 crit 3, then W7-10

Mode 5 now composes the full 512 dots. The six PeterLemon Interlace ROMs
render, but **nobody has verified them**, and this suite's standard is
that a golden is pinned only after a frame was looked at — a standard that
changed three of four outcomes when W7-05 applied it. Same dump command;
`RF_GOLDEN_DUMP` now emits a correctly-headed 512-wide PPM.

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

### F. Watch the first CI run of `docs.yml`

GitHub Actions cannot run in the dev environment, so `mdbook build` is
**unproven**. The verifier, the example build and the profile validation
all pass locally, and the workflow uses only actions `ci.yml` already
uses — but the first push to `main` is its real test.

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

**Two things all nine miss.** Both bit this session:

1. **The `#[ignore]`d goldens.** A hires change shipped in `4ca5f93` broke
   `peterlemon_golden.rs` and the full gate stayed green through it. On
   anything touching the PPU, run
   `cargo test --release -p rf-snes --test peterlemon_golden -- --ignored`.
2. **Feature-gated code.** CI leaves `gamepad` off, so
   `#[cfg(feature = "gamepad")]` is never type-checked by the gate. A
   green default build hid a real compile error in W8-04. Use
   `cargo clippy -p retroforge --features gamepad --all-targets`.

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
