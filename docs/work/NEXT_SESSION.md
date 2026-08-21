# NEXT SESSION — resume point (rewritten 2026-08-21, after W8-09 closed)

Supersedes the "Current state" and "START HERE" sections of
`docs/work/HANDOFF.md` (that file's *process* sections — read-order, gate
command, block-note discipline — are still correct and still apply).

Read `CLAUDE.md` first. **Law 8 is new**, and it exists because of what
happened on 2026-08-21.

---

## Why Law 8 exists

`W8-09`'s first implementation contained an unbounded `while` walk: in
`candidacy()`, a `y` sitting inside a HUD band advanced in neither inner
loop, so the outer loop pushed `(0, 0)` forever.

Two concurrent `cargo test` runs reached **245 GB and 111 GB RSS** on a
128 GB machine, drove free memory to 194 MB, stalled `tccd`, blocked
WindowServer's main thread inside a TCC preflight, and **kernel-panicked
the workstation twice** (07:24 and 07:54). Full forensics:
`docs/LESSONS.md` **RF-L-09**.

**Nothing is outstanding on the bug.** It is fixed, tested, shipped, and
recorded in three places (CLAUDE.md Law 8, LESSONS.md RF-L-09, and a
comment at the loop in `crates/rf-enhance/src/interpolation.rs`). It is
described here only so the next session understands why Law 8 is there and
does not read it as boilerplate.

---

## State of the tree (verified 2026-08-21)

Branch `main`, HEAD `7b62cb3` *(chore(W8-09): close)*. **Working tree
clean.** Four commits landed this session:

| Commit | What |
|---|---|
| `1d5a777` | `docs(RF-L-09)` — Law 8, LESSONS.md RF-L-09, this file |
| `33c8745` | `feat(W8-09)` — design doc + candidacy predicate + 7 tests |
| `76f662b` | `chore(evidence)` — regenerated `local-gate.json` |
| `7b62cb3` | `chore(W8-09)` — close: plan.json `done` + STATUS.md |

`plan.json`: **W8-09 is `done`.**

### Gate status — all seven green at HEAD

```
cargo fmt --all --check                                  OK
cargo clippy --workspace --all-targets -- -D warnings    OK
cargo test --workspace                                   OK — 1474 passing
scripts/validate-arch.sh                                 OK
node scripts/validate-plan.mjs                           OK
node scripts/validate-traceability.mjs                   OK
node scripts/validate-evidence.mjs                       OK
```

**The four-suite evidence FAIL is resolved.** `nestest`, `ppu_vbl_nmi`,
`sprite_hit_tests` and `apu_test` had been stale since W7-11 (`27049cd`)
and were blocking closes under Law 3 for *every* ticket, not just W8-09.
`scripts/local-gate.sh` was re-run in full from a clean tree; the
regenerated file differs from the old one in exactly one line, the
`retroforge_commit`, so it was a bookkeeping lag rather than a regression.
All ROM/vector prerequisites are present under `roms/` if it needs running
again.

### Two things to know about W8-09's outcome

1. **§7 of `docs/design/FRAME_INTERPOLATION.md` declines to implement
   blending.** There is no blender. `rf-enhance/src/interpolation.rs` is
   the candidacy predicate only — it decides whether a frame pair *may* be
   interpolated and blends nothing.
2. The decline is on §3.1: interpolating toward frame N+1 requires having
   N+1, so it costs a full frame of input latency, inherently. §7 lists
   three preconditions for revisiting it.

---

## Next

`validate-plan` reports claimable now: **W8-10, W8-12, W9-03, W9-08**.
Read the claimed ticket's `notes` array first — per-ticket traps the
conductor already paid for live there.

---

## Standing optional work (proposed by RF-L-09, neither built)

Deliberately not done as part of the W8-09 close; each is its own ticket's
worth of work.

1. **An RSS / wall-clock cap around `cargo test --workspace`** — the
   higher-value of the two, because it bounds the blast radius of *any*
   future hang rather than one loop shape. A hang in a test suite is a
   denial of service against the developer, and right now nothing stops
   it.
2. **A lint for the loop shape** — flag `while <ix> < <bound>` loops whose
   body can leave `<ix>` unchanged on some path. Law 8 is currently
   enforced by nothing but attention.
