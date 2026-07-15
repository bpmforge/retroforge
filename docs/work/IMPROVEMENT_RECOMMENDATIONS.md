# RetroForge — P2 Domain Interrogations → Recommendation Index

Date: 2026-07-15 · Branch: `review/design-review-hardening` · Follows docs/work/DESIGN_REVIEW.md (P1).
Canon reference: bpm-opencode-experts at v2.15.0-in-flight (CHANGELOG trails HEAD by 2 releases; verified this session).
Effort: S (≤half day) / M (1–2 days) / L (ticket-sized+). Wave = where it lands.
**STOP: nothing below is adopted until Brad says so.** Founder slates FS-1..3 from P1 ride this STOP too.

## A. Process & UX (persona-by-persona, beyond the P1 gap fixes)

**R-A1 (M, W4) — Scripted UI smoke test in CI.** egui 0.35 ships an inspection protocol + `egui_mcp` for agent-driven UI testing (TECH_STACK notes it; nothing uses it). One headless CI flow: boot app → open fixture ROM → assert first frame + status-bar mode badge → open/close each dockable panel. Catches the "UI ticket passed its manual checklist once, regressed forever" class and gives NFR-004 (cold-start ≤2 s) a measurement home.
**R-A2 (S, W5) — Authoring walkthrough doc as a W5 deliverable.** Ada's promise ("hours, not days") gets a worked end-to-end tutorial (annotate → export skeleton → decode iterate → validate → golden test) written *while* building the Nova/fallback profile — the doc is the dogfood evidence. North-star metric named in the doc: time-to-first-decoded-level.
**R-A3 (S, P9 design-note) — Replay interop.** .rfreplay is BK2-shaped by design; record a one-paragraph commitment (import BK2 v1 headers, export best-effort) so the TAS community's trust transfers. No code before P9.
**R-A4 (S, W4-04 close) — PLUGIN_AUTHORING.md minimal page** at Lua-host landing (capability table, error containment behavior, one worked overlay) instead of waiting for P9 SDK docs.

## B. Build-loop / agent architecture (vs canon v2.15)

**R-B1 (M, P4 — kickoff-mandated) — Codify Ralph+challenger in PLAYBOOK.** Wave-gate coverage loop (INVENTORY→VERIFY→GAP, cap 3, byte-identical-gap-set halt) + fresh-agent challenger (maker≠verifier, `re-ran independently: cmd — counts — exit` on every accepted criterion, CONTRADICTED reopens). Text lands in P4.
**R-B2 (S, P4) — Board validators join the per-ticket gate.** `validate-plan.mjs` + `validate-traceability.mjs` (built in P4) run in the executor's close gate and in CI — an agent's plan edit that cites a hallucinated doc path fails mechanically (shipwright L-13).
**R-B3 (S, now) — Block-note template** in PLAYBOOK (shipwright L-24): a `BLOCKED:` note must carry root cause / exact fix / why workarounds fail / "do not retry without X". Blocked-with-evidence is a success state.
**R-B4 (S, now) — Attempt ledger:** executors append one `attempt:` line to `notes` per session on a ticket; two blocked attempts ⇒ park for human (shipwright L-22, survives restarts because it lives in the board).
**R-B5 (S, P7 config) — Model policy:** Sonnet floor; haiku/small only for ≤1-pt tickets (board has none — effectively banned); launch without auto-frontier — hard tickets park for the morning queue (L-18 / D-018 escalation-token-by-configuration).
**R-B6 (S, P7 config) — Run-ops:** ONE conductor account-wide (L-21); caffeinate + supervisor + STOP-file semantics; crash cleanup must exclude `blocked/*` evidence branches (L-16); evidence persisted before status mutation (L-11).

## C. Reports → action

**R-C1 (M, W5) — Accuracy table as a machine report.** rf-harness already doubles as the accuracy-table generator (TESTING §5 note). Make the table machine-readable (suite × ROM × pass/fail/frame JSON), and map failures deterministically: suite → FR (the TESTING tables already pair them) → owning ticket; a red row with no open ticket fails the report step. Rules-first: the mapping is a lookup, never an LLM judgment. This is the Improvement-Plans pattern scaled to an emulator.
**R-C2 (S, W2-09) — Bench re-baseline requires justification.** `benches/baseline.json` gets a meta block (`rebaselined_by`, `justification`, `pr`); the threshold script refuses a baseline change without one. TESTING §9 already says this in prose — make it mechanical.

## D. False-positive economics (heuristics are this product's findings)

The enhancement heuristics — temporal de-flicker, HUD-split detection, scene-cut identity, idle-loop detection, sprite-limit auto-re-enable — are exactly a findings pipeline: they fire, they're sometimes wrong (Mesen #60/#188, wideNES palette cuts), and user trust dies on silent wrongness. NFR-005 (no telemetry) means FP economics must be **local-first**:

**R-D1 (M, W3-05/W4 design + threading) — Heuristic trust ladder.** Every heuristic ships with three states: **shadow** (detect + log to a local per-game report card, act never), **advisory** (badge suggests "de-flicker would help here"), **active** (user/profile enabled). Fresh install: all shadow. Profiles may pin verdicts per game (that's what profiles are). The per-game report card counts contradiction events locally (auto-re-enable fires, scene-cut resets, blink-period violations) and shows them in the Enhance workspace — measured FP without telemetry. This is Shipwright D-014's rules-first shape mapped onto a local product.
**R-D2 (S, W3-05) — Justification-gated suppression, local.** When the sprite-limit-bypass safety heuristic re-enables the limit, the event is surfaced (badge pulse + report card row), never silent. Suppressing the safety per-game requires a stored reason; the suppression auto-reopens (re-prompt) if the trigger fires again in a *different* scene context.
**R-D3 (S, TESTING/W3-05) — Red fixtures for every heuristic.** Each heuristic ships a cc65-built fixture that MUST trigger it (intentional-blink ROM for de-flicker safety, scroll-split ROM for HUD bands, palette-flash ROM for scene cuts). A heuristic change that stops firing on its red fixture fails CI. (TESTING §7 has the de-flicker cases; generalize the rule.)
**R-D4 (S, W0-03/W2-09) — Raw-vs-effective suite counts.** The harness reports raw results and effective results separately; known-fails live in an explicit waiver file (justification + expiry date); an expired waiver reopens red. No silently-skipped suites, ever.

## E. Content & extensibility (deny-by-default intake)

**R-E1 (M, design now / enforce P9) — License-gated community intake.** Contributed profiles/packs/plugins require SPDX license metadata + provenance fields; CI rejects missing/unknown; a denylist encodes the review's lessons (GFDL text, NC-licensed assets, GPL shader ports). Deny-by-default: nothing activates without passing intake. Lands as a GAME_PROFILES/PLUGINS section now, CI job at P9.
**R-E2 (S, schema v0.2) — Decoder families are versioned.** Profiles pin `decode.family_version`; a family behavior change bumps it; loader refuses newer-major (mirrors profile_version semantics). Prevents silent re-decode drift under old profiles.
**R-E3 (S, recorded) — Pack import stays format-compatible-only** (Mesen hires.txt from docs, never from GPL parser source) — already law via G-42; restated here so the P9 intake design inherits it.

## F. Core-brain / trust surfaces

**R-F1 (M, P4 — kickoff-mandated) — FAILURE_MODES.md.** No failure-modes table exists anywhere in the doc set. P4 writes it: core panic, audio underrun cascade, GPU device loss, shader-compile failure, cache corruption (bad hash ⇒ rebuild, never crash), profile decode error mid-play, plugin fault storm, replay divergence, save-state migration failure, stitcher canvas explosion (R-17), OOM on ultrawide targets. Each row: detection · containment · user-visible state · recovery · test.
**R-F2 (M, W4 acceptance threading) — Path-containment class.** Every place a path is trusted gets a realpath-containment rule: library scan (symlink loops; results confined to configured roots), `profiles.d` (profile-referenced files resolve inside the profile dir), plugin `filesystem = "cache_dir"` cap (realpath-checked, symlink-escape refused), host `export` API (user-picked dir only). Threads into W4-04/W4-08/W2-07 acceptance + PLUGINS.md. (The kickoff's symlink/realpath class, closed before any of it is built.)
**R-F3 (S, W5-05) — Executed migration drill as a release-checklist row:** loading the previous release's golden `.rfstate`/`.rfreplay` fixtures is a release step with recorded evidence, not just a CI hope (FR-STATE-005 made ceremonial).
**R-F4 (S, W4-01) — Divergence artifacts:** when determinism/mode-invariant tests fail, the harness dumps first-divergent-frame number + both state hashes + the replay slice as CI artifacts (bisection is already O(log) by design; make the evidence automatic).

## Consolidated index

| ID | What | Effort | Wave | Needs amendment? |
|---|---|---|---|---|
| R-A1 | UI smoke test in CI (egui inspection protocol) | M | W4 | no (new ticket) |
| R-A2 | Authoring walkthrough doc, time-to-first-level metric | S | W5 | no (W5-06 acceptance) |
| R-A3 | BK2 interop design note | S | P9 | no (doc note) |
| R-A4 | PLUGIN_AUTHORING.md minimal | S | W4 | no (W4-04 acceptance) |
| R-B1 | Ralph+challenger in PLAYBOOK | M | P4 | **yes — process law** |
| R-B2 | Board validators in per-ticket gate + CI | S | P4 | **yes — process law** |
| R-B3 | Block-note template | S | now | no (PLAYBOOK text) |
| R-B4 | Attempt ledger in notes | S | now | no (PLAYBOOK text) |
| R-B5 | Model policy: sonnet floor, no auto-frontier | S | P7 | **yes — build policy** |
| R-B6 | Run-ops: one conductor, evidence-safe cleanup | S | P7 | **yes — build policy** |
| R-C1 | Machine accuracy table; red-row⇒ticket rule | M | W5 | no (W0-03/W5 acceptance) |
| R-C2 | Justification-gated bench re-baseline | S | W2-09 | no (acceptance) |
| R-D1 | Heuristic trust ladder (shadow/advisory/active + local report card) | M | W3-W4 | **yes — product principle (ARCH §2 addition)** |
| R-D2 | Surfaced, justification-gated safety suppression | S | W3-05 | rides R-D1 |
| R-D3 | Red fixture per heuristic, CI-gated | S | W3-05+ | no (TESTING rule) |
| R-D4 | Raw-vs-effective counts + expiring waivers | S | W0-03 | no (harness acceptance) |
| R-E1 | License-gated deny-by-default intake | M | design now | **yes — product policy** |
| R-E2 | Versioned decoder families | S | schema v0.2 | no (GAME_PROFILES) |
| R-E3 | Format-compatible-only pack import (restate) | S | recorded | no |
| R-F1 | FAILURE_MODES.md | M | P4 | no (kickoff-mandated) |
| R-F2 | Path-containment class everywhere paths are trusted | M | W4 | **yes — security law (CONSTRAINTS)** |
| R-F3 | Executed migration drill per release | S | W5-05 | no (acceptance) |
| R-F4 | Automatic divergence artifacts | S | W4-01 | no (acceptance) |

## Decisions Brad owns at this STOP

| # | Question | Recommendation |
|---|---|---|
| FS-1 | Nova the Squirrel 2 assets are all-rights-reserved (commercial game) — SNES fixture for FR-CORE-037/P7 exit | Email NovaSquirrel for written test-use permission (author is emulator-community friendly); in parallel W6-00 names a fallback (libSFX fixtures + a licensed LoROM homebrew). Don't build the SNES arc on an unlicensed fixture |
| FS-2 | Nova 1 assets are CC BY-NC-SA + character-use restriction — MVP demo fixture | Confirm RetroForge's non-commercial posture (MIT code is fine; *project* must stay non-commercial while Nova is the demo), or make Alter Ego (PD) the primary MVP demo and Nova secondary |
| FS-3 | DataCrystal facts-only transcription policy (GFDL 1.2) — drafted into CONSTRAINTS §2 | Sign off (it's already written, vetoable). Without it, profile authoring inherits copyleft risk |
| AM-1 | Adopt R-B1/R-B2 (Ralph+challenger + validators-in-gate) as process law | Adopt — it's the arc's own method, codified for the build |
| AM-2 | Adopt R-B5/R-B6 (model policy + run-ops) as the P7 launch configuration | Adopt as configuration, revisit after first overnight run |
| AM-3 | Adopt R-D1 heuristic trust ladder as a product principle (ARCHITECTURE §2 addendum: every heuristic is shadow-first with a local report card) | Adopt — it is the honesty contract made mechanical |
| AM-4 | Adopt R-E1 license-gated intake as product policy (design now, enforce P9) | Adopt |
| AM-5 | Adopt R-F2 path containment as a CONSTRAINTS security law | Adopt |

Everything else in the index is acceptance-level threading I can apply without an amendment (they'll be threaded in P3 regardless of bundle-adoption, each recorded vetoable).

**Rejected during interrogation (recorded so they aren't re-derived):** a full Improvement-Plans database (repopulse D-016 shape) — overkill for a desktop emulator with no warehouse; telemetry-based FP measurement — violates NFR-005, local report cards suffice; adopting a conductor harness *now* — premature until the board has >1 executor working concurrently, P7 revisits; WASM plugin tier pull-forward — no third-party demand exists, NON_GOALS #17 stands.
