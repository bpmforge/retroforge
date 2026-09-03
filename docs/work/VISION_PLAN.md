# RetroForge — plan to the vision

Date: 2026-09-03 · Companion to `docs/work/VISION_GAP.md` (the register this
plans against) · **Nothing filed in `plan.json`**

**Read the gap register first.** This file does not restate the evidence; it
sequences the work that closes it.

---

## 0. Why this is a decision sheet before it is a work breakdown

Of the five tickets on the board, **four are ruling-gated and one is
in_progress and stalled on a ruling**. Of the three VISION §5 criteria with
no owning ticket, **two cannot even be sized** until a decision is made.
A plan presented as a work breakdown would be unexecutable on day one.

So §1 is the decision sheet, and every ticket in §3 names **which ruling
releases it**. Where a ruling has branches, both branches are priced, so the
choice is made with its cost visible.

**This is a claim queue, not a set of parallel waves.** Law 1 is one ticket
at a time and the board carries a WIP=1 territory law; §3 is therefore an
*ordered list of what gets claimed next*, and the ordering is load-bearing.

**Point estimates are fibonacci, matching the board.** Items marked
`points: ?` are honestly unsizable until their predecessor runs — saying so
is the point, not a placeholder to fill in later.

---

## 1. The decision sheet

| # | Decision | Branches, priced | Releases |
|---|---|---|---|
| **D-1** | **W12-01 runner lifecycle** | (a) persistent LaunchAgent — a real unattended gate, law-8 exposure on the workstation. (b) on-demand `run.sh` before pushing — no exposure, but an unstarted runner is a silently skipped gate. Either is ~1-2 pts of remaining work on an already-written workflow. | W12-01; the cc65 fixture rebuild stops being verified by nothing |
| **D-2** | **ROM supply** — Brad puts commercial ROMs in `roms/` (ruling #2 created it for exactly this) | (a) supply them — releases *two* vision criteria at once. (b) don't — W11-06 falls back to more in-repo fixtures (breadth without commercial coverage) and Phase 7's exit criterion becomes unreachable as written and must be amended. | W11-06 **and** W13-03 |
| **D-3** | **W7-18** — is `spc_mem_access_times.sfc` an oracle, or is it encoding higan's stack order? | (a) treat it as an oracle and keep searching — open-ended, no arbiter exists, three plausible-but-wrong models already produced. (b) close W7-18 on *documented conformance to Overload's traces* (which is met) and re-file the ROM as a known-red with its reason. ~2 pts of docs. | W7-18, then W7-17, then W7-08 |
| **D-4** | **R-05 / phases 2-9 exit gates** | (a) retroactive criteria→evidence passes — **~5 pts per phase × 7 = ~35 pts**, and Phase 1's took two attempts and produced a criteria amendment, so expect the same shape. (b) amend R-05 so ticket completion *is* the exit — ~2 pts, and the ledger keeps saying only Phase 1 was gated. | W12-02 |
| **D-5** | **The three unticketed VISION §5 criteria** — do they become Phase 13, or does VISION §5 get amended to say what the project now intends? | This is the only decision that changes whether "the end goal" is finite. §3's W13-\* assume Phase 13; if VISION is amended instead, most of §3 disappears rather than shrinking. | all of W13 |

**D-2 and D-5 are the two that matter.** D-1, D-3 and D-4 are finite either
way; those two decide the project's remaining size.

---

## 2. The one sequencing constraint that must not be optimized away

> **The ten profiles come BEFORE the v1 format freeze. Not after, and not
> alongside.**

`profile_version` is `"0.1"` and has been exercised by **six in-repo
fixtures we also wrote**. Authoring ten profiles against real, documented
games is the only thing that surfaces the keys v0.1 is missing — a schema
frozen first would be frozen around our own fixtures' shape. The same
argument applies to the pack format and the Lua surface: freeze *after* an
outside-shaped consumer has pushed on them, never before.

Consequence for the queue: **W11-06 → W13-04/05/06**, and W13-03
(commercial titles) belongs on the same side of that line, because playing
real games is what finds the identity and decode gaps a profile schema has
to express.

Second-order: this makes **D-2 the critical path**. If ROMs are not
supplied, the v1 freeze either happens against fixtures anyway (freezing the
wrong thing) or the criterion is amended. There is no third option, and no
amount of engineering removes the dependency.

---

## 3. The claim queue

Ordered. Each row carries the four fields `validate-plan.mjs` checks, so
filing is mechanical if Brad says to file.

### Claimable today — needs no ruling

**W13-01 — the debugger's "daily-drivable" bar, written down**
- `phase: 13` · `crate: infra` · `points: 2`
- `write_scope: ["plan.json", "docs/ROADMAP.md", "docs/design/DEBUGGER.md"]`
- `depends_on: []`
- **Releasing ruling: none. This is the only item on the whole plan that
  can be claimed right now.**

Shape it as a **phase-entry planning ticket**, exactly like W6-00 and W7-01
(both `crate: infra`, both `points: 2`, both scoped to `plan.json` +
`ROADMAP.md`) — convention, not invention.

The work: grade the fourteen shipped `rf-debugger` modules (`annotation`,
`audio_scope`, `breakpoint`, `datacrystal`, `event_timeline`, `layout`,
`memory_view`, `nametable`, `oam`, `palette`, `pattern`, `profile_export`,
`trace`) against `DEBUGGER.md` §1-6 (execution control, tracing, viewers,
annotation→profile export, Lua console tie-in, performance discipline), and
emit a **testable bar** — because "daily-drivable for ROM hackers" is
currently a phrase, not a criterion, and no work can be planned toward it.
The ticket's output is that bar plus whatever gap tickets it expands into.

Expected expansion: **W13-02**, `points: ?` — genuinely unsizable until
W13-01 runs. That is the correct estimate, not a missing one.

### Released by D-1

**W12-01 — the local gate runs unattended** *(exists, `in_progress`)*
- Remaining work is the runner-lifecycle choice and then **reading a
  completed green run**. The workflow's law-8 bounding is already done and
  is not the gap.

### Released by D-4

**W12-02 — R-05, settled either way**
- `phase: 12` · `crate: infra` · `points: 2` (branch b) **or ~35** (branch a)
- `write_scope: ["docs/ROADMAP.md", "docs/STATUS.md", "docs/RISKS.md", "plan.json"]`
- `depends_on: []`

Under branch (a) this is not one ticket but seven, one per phase, and should
be filed that way rather than as a 35-point monolith — that is the shape
Brad splits.

### Released by D-2 (the critical path)

**W11-06 — ten curated profiles** *(exists, `blocked`)*
- Unchanged as filed. Six ship today, all against our own fixtures.

**W13-03 — the plain-LoROM/HiROM commercial-mainstream pass**
- `phase: 13` · `crate: rf-snes` · `points: 8`
- `write_scope: ["tests/**", "docs/STATUS.md", "docs/evidence/**", "plan.json"]`
- `depends_on: []` (gated by D-2, not by a ticket)

This is **Phase 7's own exit criterion**, which has never been run: *3
designated plain-LoROM commercial titles playable start-to-credits,
sampled*. Filed as a **pass that produces a defect register**, not as a
fix ticket — the fixes get filed from its findings, which is how W7-01
worked. Its `write_scope` deliberately excludes the cores: a compat pass
that quietly starts patching `rf-snes` is two subsystems in one ticket.

Expected expansion: **W13-03a..n**, `points: ?` each, sized by what the
register actually finds.

### Released by D-2 **and** gated behind the profiles (see §2)

**W13-04 — profile schema v1**
- `phase: 13` · `crate: rf-profiles` · `points: 5`
- `write_scope: ["crates/rf-profiles/**", "profiles/**", "docs/design/GAME_PROFILES.md", "crates/retroforge/src/profile_editor.rs", "crates/rf-debugger/src/profile_export.rs", "crates/rf-enhance/src/overlay.rs"]`
- `depends_on: ["W11-06", "W13-03"]`

The version literal lives in four places and they must move together, which
is why the scope is wider than one crate. `GAME_PROFILES.md` §1 already
specifies the semantics to honour — *loader rejects newer majors, warns on
unknown keys* — so v1 is a **freeze plus a migration path plus a compat
test**, not a redesign. Note `family_version` is already at "schema v0.2"
in §151 while `profile_version` says 0.1; reconciling those two is part of
this ticket, not a separate finding.

**W13-05 — Lua/plugin API v1**
- `phase: 13` · `crate: rf-plugin-sdk` · `points: 5`
- `write_scope: ["crates/rf-plugin-sdk/**", "docs/PLUGIN_SDK.md", "docs/PLUGIN_AUTHORING.md", "docs/design/PLUGINS.md"]`
- `depends_on: ["W13-04"]`

Behind the profile freeze on purpose: a script's stability promise is only
as good as the profile surface it reads through.

**W13-06 — pack + bundle format v1**
- `phase: 13` · `crate: rf-enhance` · `points: 3`
- `write_scope: ["crates/rf-enhance/**", "docs/design/DISTRIBUTION.md", "docs/design/AI_UPSCALING.md"]`
- `depends_on: ["W13-04"]`

`bundle.toml` already carries `version`; this is the smallest of the three
freezes because W9-05/W9-06 and W11-05/W11-14 have already pushed on the
format from both the import and the render side.

### Last, and it cannot be closed by internal work

**W13-07 — the third-party criterion, or a proxy for it**
- `phase: 13` · `crate: infra` · `points: 3`
- `write_scope: ["docs/**", "plugins/**", "profiles/**"]`
- `depends_on: ["W13-04", "W13-05", "W13-06"]`

Phase 9's exit criterion is *a third party ships a profile + pack + script
without touching Rust or asking us questions.* **No amount of internal work
closes that** — it needs an external person, and their schedule is not ours
to set.

Proposed proxy, in the repo's own idiom: a **fresh-context dry run** that
may read only the published docs site and must ship all three artifacts,
scored on what it had to ask — the same instrument the design-review arc
used when it ran a fresh-agent challenger against its own ownership claims.

**Adopting a proxy is itself a VISION amendment**, not a testing detail:
it changes the criterion from "a third party did" to "a cold reader could".
That belongs in D-5, and it should be recorded as an amendment rather than
absorbed silently.

---

## 4. What the queue looks like as one line

```
NOW      W13-01 (debugger bar)          — no ruling needed
D-1      W12-01 (runner)                — finish the stalled ticket
D-3      W7-18 → W7-17 → W7-08          — the SPC/DSP chain, in that order
D-4      W12-02 (R-05)                  — 2 pts or ~35, Brad's branch
D-2  ┌─  W11-06 (ten profiles)  ┐
     └─  W13-03 (commercial pass)┘      — the critical path; both need ROMs
         └→ W13-04 (profile v1)
              ├→ W13-05 (Lua/plugin v1)
              └→ W13-06 (pack/bundle v1)
                   └→ W13-07 (third-party / proxy)
D-5      decides whether W13-* exists at all
```

Sized work: **~28 points** of tickets that can be written down today
(W13-01 2, W12-01 ~2 remaining, W7-18/17/08 chain, W12-02 2, W11-06 8,
W13-03 8, W13-04 5, W13-05 5, W13-06 3, W13-07 3 — minus the branches D-4
and D-5 may delete). Unsized: **W13-02 and the W13-03 expansion**, both
`?` on purpose.

---

## 5. Closure map — what each item actually buys

| VISION §5 criterion | Closed by |
|---|---|
| SNES boots the plain-LoROM/HiROM commercial mainstream | W13-03 (D-2) |
| Profile + Lua + pack formats stable at v1 | W13-04, W13-05, W13-06 |
| ≥10 curated game profiles | W11-06 (D-2) |
| Mesen HD-pack import path working | **already met** — W9-06, W11-05, W11-14 |
| Debugger daily-drivable for ROM hackers | W13-01 defines it, W13-02 closes it |
| Community authors without touching Rust | W13-07, or an amended criterion |
| Project survives its founder | not plannable as a ticket; the doc set is the mechanism and W13-07's cold-reader proxy is the closest available test |

Not a VISION criterion but a standing debt, recorded so it is not mistaken
for one: **Linux and the software-rasterizer GPU path remain verified by
nothing**, and W12-01 recovers only the cc65 fixture rebuild.
`MVP.md`'s product floor is already marked `[~]` for this. Restoring it
needs either Gitea Actions on `origin` or a Linux runner — neither is
filed, and neither is required by VISION §5.
