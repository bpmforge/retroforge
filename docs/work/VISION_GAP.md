# RetroForge — VISION gap register

Date: 2026-09-03 · No code changed · Nothing filed in `plan.json`
Question answered: *what is left to reach the end goal and the vision?*

**Why this file exists.** The board reads **157 done / 4 blocked / 1
in_progress of 162**, which invites the answer "five tickets left". That
answer is materially wrong. Several `docs/VISION.md` §5 criteria have **no
owning ticket at all**, and phases 2-9 have every ticket closed with **no
recorded exit gate**. This register grades the project against VISION §5
and the ROADMAP exit criteria rather than against ticket status, so the
difference is visible in one place.

Nothing here is a decision. Items marked **RULING** need Brad; the rest
name work that exists but is unowned.

---

## 1. Board remainder — 5 tickets, none blocked on effort

### W12-01 (`in_progress`) — the gate workflow has never run

**New finding, 2026-09-03.** `gate-darwin.yml` landed 2026-08-31
(commit `fd05d3c`) and its first and only run **never executed**:

```
$ gh api repos/:owner/:repo/actions/runners
{"total_count":0,"runners":[]}

$ gh run view 33412355165
X main Gate (darwin, self-hosted) · 33412355165 · push, 2026-08-31
X gate in 24h0m0s
  The job has exceeded the maximum execution time while awaiting a
  runner for 24h0m0s
```

**The cause is the thing the 2026-08-31 ledger entry praised.** The probe
proved scheduling works with an *ephemeral* runner — "registered, ran,
deregistered; **no launchd service, nothing persistent installed**". That
is exactly why the real gate sat queued for a day and was cancelled: at
push time there was no runner listening, and there is none now. The
property that made the probe clean makes the gate inert.

**What is left is not YAML.** The workflow's law-8 bounding (push-to-main
only, `timeout-minutes`, heavy suites split to `workflow_dispatch`) is
done and is not the gap. The gap is a **runner-lifecycle decision**:

| Option | Gets | Costs |
|---|---|---|
| Persistent LaunchAgent | a real unattended gate; acceptance criterion 1 satisfiable | `cargo test --workspace` runs unattended on the workstation an `interpolation.rs` loop panicked twice (law 8, RF-L-09) |
| On-demand `run.sh` before pushing | no unattended exposure | it is a ritual, not a gate — an unstarted runner is a silently skipped gate, which is the failure mode this whole arc exists to end |

Until one is chosen and a **completed green run has been read**, W12-01's
first acceptance criterion is unmet and `TESTING.md` §0 must keep saying
the cc65 fixture rebuild is verified by nothing. **RULING.**

### W7-08 / W7-17 / W7-18 — SPC700 and S-DSP accuracy

`blocked` on W7-08 is **not stale**; I checked, because ruling #3
(2026-08-26) unblocked it. The 2026-08-27 correction in `STATUS.md`
walked the close back: the pass criterion had been too loose, the
criterion is now `contains("PASSED TESTS")`, and all four SPC ROMs are
correctly red. The S-DSP deliveries themselves are real and are not
walked back.

The live question is on **W7-18**, and it is not a coding question. The
nesdev thread that names Overload's logic-analyzer document also records
**higan and Overload disagreeing** on `(dp),Y` and the CALL/RET/RTI stack
orders, with no arbiter as of 2017. `spc_mem_access_times.sfc` compares
an internal checksum it never prints an expectation for, so each further
edit changes the number without saying whether it moved closer — the
search-oracle mode that has already produced three plausible-but-wrong
models here.

**RULING:** is that ROM an oracle, or is it encoding higan's order? If the
latter, passing it means matching a bug, and something other than "the ROM
goes green" has to close the ticket.

### W11-06 — ten curated profiles

Six profiles ship (`profiles/nes/rf-scroller`, `rf-rooms`,
`rf-scroller-demo`; `profiles/snes/rf-scroller-s`, `example-mode7`,
`rf-rooms-flat`), every one against our own fixtures. VISION §5 asks for
**≥10** and the point of the number is breadth, not count.

**RULING** (unchanged, restated): which of the three routes — a
locally-held ROM verified against and gitignored (ruling #2 created
`roms/` for exactly this), offsets shipped marked UNVERIFIED with the UI
saying so, or more in-repo fixtures (breadth without commercial coverage).

---

## 2. VISION §5 criteria with **no owning ticket**

This section is the substance. Each row is a promise the vision makes that
the board does not track.

| VISION §5 (18-month) | State | Owner |
|---|---|---|
| "SNES core boots the plain-LoROM/HiROM commercial mainstream" | Phase 7's exit criterion — *3 designated plain-LoROM commercial titles (user-supplied) playable start-to-credits sampled* — **has never been run**. Same ROM-supply blocker as W11-06, but no ticket carries it. | **none** |
| "Profile + Lua + pack formats stable at **v1**" | `profile_version` is `"0.1"` in `docs/design/GAME_PROFILES.md`, `crates/retroforge/src/profile_editor.rs`, `crates/rf-debugger/src/profile_export.rs` and `crates/rf-enhance/src/overlay.rs`. W4-02 shipped "TOML schema v0" as filed. Nothing owns stabilization, a compat policy, or a migration path. | **none** |
| "≥10 curated game profiles" | 6, all in-repo fixtures | W11-06 (blocked) |
| "Mesen HD-pack import path working" | W9-06 closed; W11-05 + W11-14 put packs (background **and** sprite art) in front of a user | done |
| "Debugger suite at 'daily-drivable for ROM hackers' quality" | **No ticket, no criterion, never measured.** The only 18-month promise with no defined test. | **none** |

| VISION §5 (long-term) | State |
|---|---|
| "Community authors profiles/packs/scripts without touching Rust" | Phase 9's exit criterion is *a third party ships a profile + pack + script without touching Rust or asking us questions*. W9-01..08 closed is not that criterion; it has **never been attempted with an actual third party**. |
| "Project survives its founder" | Asserted by the doc set (permissive licence, docs, handoff files); verified by nobody else having built from them. |

**Reachability of the VISION §2 promises is the good news and should not
be lost in the above.** All five compose-what-the-PPU-meant promises are
reachable by a user today — de-flicker (bypass **and** temporal), ultrawide
terrain, whole levels on one screen, live Lua overlays, replaced art —
after W11-01..05 and W11-14. The 2026-08-26 table in `STATUS.md` showed
two of seven; it is now seven of seven.

---

## 3. Verification debt

**Phases 2-9 have every ticket closed and no recorded exit gate. Only
Phase 1 has one (2026-08-06).** This is the 2026-08-05 finding — "R-05's
phase gate is not enforced anywhere in code" — showing up as seven phases
of accumulated consequence. `ROADMAP.md` deliberately marks *tickets
closed* and *exit gate recorded* as two separate facts per phase rather
than collapsing them into a ✅ that would manufacture verification nobody
performed.

**RULING:** retroactive exit gates, or an R-05 amendment saying ticket
completion *is* the exit. A docs edit cannot make this choice.

**Verified by nothing at all** (`TESTING.md` §0, unchanged by this file):
**Linux**, the **software-rasterizer GPU path**
(`LIBGL_ALWAYS_SOFTWARE=1` on llvmpipe vs Metal locally), and the **cc65
deterministic fixture rebuild** of RF-Scroller / RF-Scroller-S /
mirror-maps. W12-01 recovers **one** of the three, and only once a runner
exists. Hosted Actions remains permanently off (ruling 2026-08-22); the
three `Nightly` failures on 2026-09-01/02/03 are budget rejections, not
signals, and must not be chased.

**Last measured workspace state, quoted rather than re-run** (2026-08-30
ledger entry; law 8 makes an unprompted full-suite run a bad trade):
**1723 passing, 0 failed, 33 ignored**; law-3 gate green, exit 0;
`validate-plan` 161 tickets / 865 pts; `validate-traceability` 101/101
FR/NFR reachable, 33/33 stories covered, zero dangling refs.

---

## 4. The decisions, as questions

1. **W12-01 runner lifecycle** — persistent LaunchAgent (real gate, law-8
   exposure) or on-demand start (no exposure, not a gate)?
2. **W7-18** — is `spc_mem_access_times.sfc` an oracle, or higan's order?
   If the latter, what closes the ticket instead of "it goes green"?
3. **W11-06** — which profile-verification route?
4. **Phases 2-9** — retroactive exit gates, or amend R-05?
5. **The three unticketed vision criteria** — schema v1, the commercial-title
   pass, the debugger quality bar — do they become a new phase, or does
   VISION §5 get amended to say what the project now intends?

Question 5 is the one that decides whether "the end goal" is close or
open-ended. Items 1-4 are finite and known. Item 5 contains the only work
in this register whose *size* has never been estimated.
