# NEXT SESSION — resume point (rewritten 2026-08-21, second run)

Supersedes `docs/work/HANDOFF.md`'s "Current state"/"START HERE"; that
file's *process* sections still apply. Read `CLAUDE.md` first.

---

## State

Branch `main`, tree clean, both remotes in sync. **133 of 140 tickets
done.** `plan.json` reports `claimable now: (none)`.

### The gate is now NINE commands, not seven

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

**And that is still not enough.** Two things the nine miss:

1. **`cargo test --workspace` does not run the `#[ignore]`d goldens.**
   A hires change shipped in `4ca5f93` broke `peterlemon_golden.rs` and
   the full gate stayed green through it. Run
   `cargo test --release -p rf-snes --test peterlemon_golden -- --ignored`
   on anything touching the PPU.
2. **CI leaves `gamepad` off**, so `#[cfg(feature = "gamepad")]` code is
   never type-checked by the gate. A green default build hid a real
   compile error in W8-04. Use
   `cargo clippy -p retroforge --features gamepad --all-targets`.

**Run the suite with nothing else running** (RF-L-10, now fixed): a red
run with a second `cargo test` in flight is suspect before it is believed.

---

## Everything left needs a HUMAN or an ARTIFACT — not more code

This is the honest reason there is no "next ticket". Seven remain and not
one is blocked on programming.

| Ticket | What it needs |
|---|---|
| **W7-15** crit 1 | **Look at one frame.** Per-dot composition was built (segmentation at write boundaries), works, and *changes RotZoom's pinned golden* — because RotZoom writes registers mid-line, which is exactly what per-dot renders correctly. Isolated, not guessed: disabling segmentation alone restored `6ebfb8ce`. Re-pinning needs the visual check this suite requires. **Criterion 3 as written ("RotZoom unchanged") is unsatisfiable for any ROM that writes mid-line and must be amended.** |
| **W7-06** crit 3 | **Look at six frames.** Mode 5 now composes 512 dots; the Interlace ROMs render but nobody has verified them. `RF_GOLDEN_DUMP=1` emits a correctly-headed 512-wide PPM. |
| **W9-06** crit 3 | A **licence-clear** community HD pack. SCOPE puts third-party assets as gate fixtures in the OUT column; needs a fetch-only artifact designation or an amended criterion. Do **not** satisfy it with a pack we authored. |
| **W7-08** crit 2, 3 | BRR sample-exactness, and an audio RMS comparison against a **designated** SPC set that does not exist. Check it is obtainable and licence-clear **before** claiming. |
| W7-10 | Waits on W7-06. |
| W7-13 | Waits on W7-15. |
| W5-05 | **Held** by the board — excluded from unattended claims. |

**The recurring shape:** every one is "we cannot verify this here", never
"we cannot build this". That is a healthy place to stop, and the reason
the gate is worth trusting.

---

## Read RF-L-11 before writing any doc comment

Four stale second-sources-of-truth were found and fixed in one session,
and the fourth **propagated into the plan** — a stale `dsp.rs` scope
comment told W7-08's notes the DSP was absent, and that was restated into
`plan.json` and `STATUS.md` as fact. Three layers of restatement, no
compiler anywhere in the chain.

The standing rules, in full at `docs/LESSONS.md` RF-L-11:

1. Add a second place that must agree with a first → write the test in the
   **same commit**.
2. A scope claim in a doc comment needs an assertion or an expiry.
3. **Read the code before repeating a doc's claim about it.**

---

## Standing follow-ups

1. **`rf-intake` is not wired into CI** (`.github/**` was out of W9-05's
   scope), so FR-PROF-007's "nothing activates without passing" is a
   property of the code path, not of the repo's automation.
2. **`docs.yml` has never run.** GitHub Actions cannot execute here;
   `mdbook build` is unproven until the first push exercises it.
3. **GAME_PROFILES.md §2 does not document `[decode.room_grid]`** —
   `RoomGridSpec`'s doc comment is the specification for now.
4. **An RSS/wall-clock cap around `cargo test --workspace`** (RF-L-09's
   proposal), still unbuilt and still the higher-value of its two guards.
5. **Commercial-title profiles are permitted and unused** (W9-08 ruling) —
   every claim needs an FR-PROF-003 citation the loader enforces.
