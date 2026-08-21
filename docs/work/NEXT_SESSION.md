# NEXT SESSION — resume point (written 2026-08-21)

Supersedes the "Current state" and "START HERE" sections of
`docs/work/HANDOFF.md` (that file's *process* sections — read-order, gate
command, block-note discipline — are still correct and still apply).

Read `CLAUDE.md` first. **Law 8 is new**, and it exists because of what
happened in the session that wrote this file.

---

## What happened last session

`W8-09` (frame-interpolation, design-first) was claimed and its design
document written. The module that followed contained an unbounded `while`
walk: in `candidacy()`, a `y` sitting inside a HUD band advanced in neither
inner loop, so the outer loop pushed `(0, 0)` forever.

Two concurrent `cargo test` runs reached **245 GB and 111 GB RSS** on a
128 GB machine, drove free memory to 194 MB, stalled `tccd`, blocked
WindowServer's main thread inside a TCC preflight, and **kernel-panicked the
workstation twice** (07:24 and 07:54). Full forensics: `docs/LESSONS.md`
**RF-L-09**.

It is fixed, tested, and the lesson is recorded in three places (CLAUDE.md
Law 8, LESSONS.md RF-L-09, and a comment at the loop itself). Nothing is
outstanding on the bug. It is described here only so the next session
understands why Law 8 exists and does not treat it as boilerplate.

---

## State of the tree (verified 2026-08-21)

Branch `main`, HEAD `c19adce` *(chore(W8-09): claim)*. **`c19adce` is local
only — both remotes are still at `f997d12`.**

Working tree is dirty, all of it W8-09 work plus the incident write-up:

| File | State | Belongs to |
|---|---|---|
| `docs/design/FRAME_INTERPOLATION.md` | new, complete (150 lines, §1–§7) | W8-09 |
| `crates/rf-enhance/src/interpolation.rs` | new, fixed, 7 tests passing | W8-09 |
| `crates/rf-enhance/src/lib.rs` | modified — `pub mod interpolation;` | W8-09 |
| `CLAUDE.md` | modified — new Law 8, old 8 became 9 | RF-L-09 |
| `docs/LESSONS.md` | modified — RF-L-09 row, details, queue entry | RF-L-09 |

`plan.json` still has **W8-09 `in_progress`**.

### Gate status — 5 of 7 green, 1 warn, 1 pre-existing FAIL

```
cargo fmt --all --check                                  OK
cargo clippy --workspace --all-targets -- -D warnings    OK
cargo test --workspace                                   OK — 1474 passing
scripts/validate-arch.sh                                 OK
node scripts/validate-plan.mjs                           OK
node scripts/validate-traceability.mjs                   OK, 1 WARN  (see step 1)
node scripts/validate-evidence.mjs                       FAIL, 4 suites (see step 2)
```

**The evidence FAIL is not yours and is not new.** Four NES suites
(`nestest`, `ppu_vbl_nmi`, `sprite_hit_tests`, `apu_test`) went stale at
`27049cd` (W7-11, eight commits back) — that commit touched `rf-nes` cores
and is not an ancestor of the recorded `retroforge_commit`. It has been red
across every ticket closed since. Under Law 3 it blocks closing *any*
ticket, W8-09 included.

---

## Steps, in order

### 1. Cite the design doc from the ticket (2 minutes)

`validate-traceability` warns: `W2 docs/design/FRAME_INTERPOLATION.md:
referenced by no ticket`. The check is a literal substring match of the
filename against all ticket text (`validate-traceability.mjs:114-116`), so
W8-09's `notes` or `acceptance` must name the file. `plan.json` is in the
always-writable set, so no scope widening is needed.

### 2. Regenerate the stale NES evidence (`scripts/local-gate.sh`)

This is the real blocker, and it is a **decision, not just a command** —
resolve it before doing anything else:

- **Regenerate now** and close W8-09 behind a fully green gate. Needs the
  fetched test ROMs (`roms/`, gitignored, W0-03's fetcher) and real
  wall-clock. Correct if the ROMs are present.
- **Split it into its own ticket** if the ROMs are missing or the run is
  long, and close W8-09 with a block note recording the pre-existing
  failure. Precedent for block notes is in `PLAYBOOK.md`.

Do not close W8-09 by quietly ignoring a red validator — RF-L-06 is exactly
that failure mode.

### 3. Close W8-09

Design doc, implementation, and tests are all done; only the paperwork is
left. Acceptance criteria and how they are met:

- *"a design document precedes any implementation, per TRACEABILITY G5"* —
  met, and the git order shows it.
- *"states what interpolation can and cannot preserve, and how it interacts
  with the determinism invariant"* — §2, §3, §4.
- *"if implemented, it is opt-in, labelled, and absent from Accuracy Mode"* —
  vacuous by design: §7 **declines to implement blending**. What shipped is
  the candidacy predicate, which decides whether a pair *may* be
  interpolated and blends nothing.

Then: `plan.json` → `done`, a `docs/STATUS.md` entry in the established
per-ticket-evidence style, gate, commit, push to **both** remotes.

**Say in the STATUS.md entry that §7 declines to implement.** A future
reader who greps for `interpolation` and finds a shipped module will
otherwise assume the blender exists.

### 4. Commit the RF-L-09 files separately

`CLAUDE.md` and `docs/LESSONS.md` are outside W8-09's `write_scope`
(`docs/design/**`, `crates/rf-enhance/**`). They are process files, and this
is **RF-L-01 recurring** — process files writable by no ticket. Commit them
as their own `docs(RF-L-09):` commit rather than smuggling them into the
W8-09 close, and consider whether the always-writable set should name
`CLAUDE.md` and `docs/LESSONS.md` explicitly.

### 5. Then pick the next ticket

`validate-plan` reports claimable now: **W8-10, W8-12, W9-03, W9-08**.
Read the claimed ticket's `notes` array first — per-ticket traps the
conductor already paid for live there.

---

## Optional, and worth it

RF-L-09's analysis-queue entry proposes two mechanical guards. Neither
exists yet:

1. **A lint for the loop shape** — flag `while <ix> < <bound>` loops whose
   body can leave `<ix>` unchanged on some path. Law 8 is currently enforced
   by nothing but attention.
2. **An RSS / wall-clock cap around `cargo test --workspace`** — so the next
   runaway fails fast instead of taking the workstation down. This is the
   higher-value of the two: it bounds the blast radius of *any* future hang,
   not just this one shape. A hang in a test suite is a denial of service
   against the developer, and right now nothing stops it.
