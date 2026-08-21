# NEXT SESSION — resume point (rewritten 2026-08-21, board drained)

Supersedes the "Current state" and "START HERE" sections of
`docs/work/HANDOFF.md`; that file's *process* sections — read-order, gate
command, block-note discipline — still apply.

Read `CLAUDE.md` first. **Law 8 exists because of a kernel panic** — see
`docs/LESSONS.md` RF-L-09. Nothing about it is outstanding.

---

## State

Branch `main`, tree clean, **both remotes in sync**. HEAD is the
`chore(W9-08): close` commit or later.

**`plan.json` reports `claimable now: (none)`.** 132 of 140 tickets are
done. Every ticket whose dependencies are satisfied has been closed, so
the next session's first job is **not** "claim the lowest-id claimable
ticket" — there isn't one. It is to unblock something below.

### Gate, all green at the close (add the last two; they are new)

```
cargo fmt --all --check                                  OK
cargo clippy --workspace --all-targets -- -D warnings    OK
cargo test --workspace                                   OK — 1599 passing
scripts/validate-arch.sh                                 OK
node scripts/validate-plan.mjs                           OK
node scripts/validate-traceability.mjs                   OK
node scripts/validate-evidence.mjs                       OK
cargo deny --all-features check licenses                 OK   (new: ort, wasmtime)
node .github/scripts/verify-doc-samples.mjs              OK   (new: W9-07)
```

**Run the suite with nothing else running.** RF-L-10: a fixed temp path
in `mode_invariant_corpus.rs` makes two concurrent `cargo test` runs
delete each other's evidence file. A red run with a second cargo in
flight is *suspect before it is believed*.

---

## What remains, and what each needs

| Ticket | State | What actually unblocks it |
|---|---|---|
| **W8-04** | blocked, no deps | The only one blocked on *work*, not on another ticket. Model + harness are complete; app-wide wiring remains (mechanical, touches every window) plus §6's contrast/UI-scale items. |
| **W9-06** crit. 3 | blocked | Needs a **licence-clear** community HD pack. SCOPE puts third-party assets as gate fixtures in the OUT column, so this needs either a designated fetch-only artifact (`NoLicenseGrantFetchOnly` in `tests/rom-manifest.toml`, which means widening scope to `rf-harness` + `tests/`) or an amendment to what criterion 3 accepts as proof. **Do not satisfy it with a pack we authored** — that proves the importer against its own author. |
| **W7-15** | blocked on W7-07 | Per-dot SNES PPU timing. The keystone: it also unblocks W7-06 → W7-10, and is one of W7-13's four deps. |
| W7-06, W7-10, W7-13 | blocked | Chain behind W7-15/W7-07/W7-16. |
| W7-08 | blocked on W6-04b | S-DSP echo/gaussian/ADSR. |
| W5-04, W5-05 | **held** | Excluded from unattended claims by the board itself; W5-05 also waits on W3-06. |

**W7-15 is the highest-leverage unblock** — it is upstream of three
tickets. W8-04 is the only one that needs no ruling and no other ticket.

---

## Standing follow-ups recorded during the last run

None of these block anything; each is written up on its ticket.

1. **RF-L-10's fix** — give `mode_invariant_corpus.rs`'s output directory
   a per-process suffix, as `rf-enhance/tests/intake_poisoned_bundle.rs`
   already does. It is outside W9-06's scope, which is why it is still
   here.
2. **`rf-intake` is not wired into CI** (`.github/**` was outside W9-05's
   scope), so FR-PROF-007's "nothing activates without passing" is a
   property of the code path, not of the repository's automation.
3. **`docs.yml` has never run.** GitHub Actions cannot execute in the dev
   environment; the verifier, example build and profile validation all
   pass locally, but `mdbook build` is unproven until the first push
   exercises it. Check it.
4. **GAME_PROFILES.md §2 does not document `[decode.room_grid]`** —
   `docs/design/**` was outside W9-08's scope, so `RoomGridSpec`'s doc
   comment is the specification.
5. **An RSS/wall-clock cap around `cargo test --workspace`** (RF-L-09's
   proposal). Still unbuilt, and still the higher-value of the two guards
   it suggested: it bounds the blast radius of *any* future hang.
6. **Commercial-title profiles are permitted and unused** — Brad's W9-08
   ruling allows them from published documentation, but every claim needs
   an FR-PROF-003 citation the loader enforces.

---

## The pattern worth carrying forward

Three times in one run, shipped code contradicted its own documentation,
and each was a **second source of truth no compiler checks**:

* `rf-profiles/src/shape.rs` beside the serde structs (found in W8-12) —
  a table missing there deserialises fine and only *warns*;
* `wit/plugin.wit` read by nothing while a doc claimed drift was
  compiler-checked (W9-03);
* `example-mode7` declaring a decoder family it never configured, loading
  cleanly for months (W9-08).

Each needed a **test**, because none produced a build failure. When you
add a second place that must agree with a first, write the test in the
same commit.
