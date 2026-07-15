# RetroForge — Build-Readiness Report

Date: 2026-07-15 · Branch: `review/design-review-hardening` (7 review commits, both remotes) · Arc: DESIGN_REVIEW_KICKOFF.md v2, all passes complete.

## What changed (one screen)

- **Board**: 39→**64 tickets / 366 pts**. New owners for: CI nightly tier, 3-OS builds, releases, rf-cache, retroforge-tool CLI, settings screens, UI smoke test, RF-Scroller fixture (+SNES sibling via W6-00), Alter Ego smoke gate, trace/audio viewers, authoring loop. All 13-pt tickets split a/b. `stories[]` on every ticket; hold flags on W5-04/W5-05; claim protocol made legal via the always-writable set (G-1).
- **Decisions**: D-000..D-007 in the new docs/DECISIONS.md — fixture doctrine, Ralph+challenger process law, build policy, heuristic trust ladder, license-gated intake, path containment, facts-only policy. **D-001..D-006 are flagged vetoable** (adopted via direction/bundle); veto any individually and I re-thread.
- **Validators**: `validate-plan.mjs` (P1–P9) + `validate-traceability.mjs` (F1–F5/W1–W3) in CI and the per-ticket gate; validate-arch.sh extended (core-access rule + determinism grep); toolchain pinned 1.94.
- **New docs**: DECISIONS, ADRS (16, each with Rejected:), design/FAILURE_MODES (FM-01..16), design/CONTRACTS (6 surfaces), PREREQUISITES (HP-1..7), LESSONS (RF-L-01..08), fixtures/README.
- **Licensing**: Nova removed everywhere (assets were NC/all-rights-reserved); xBRZ→xBR (MIT); DataCrystal facts-only policy locked; no-vendor rule for unlicensed test sources; full ledger in DESIGN_REVIEW §4.

## Open items (all have owners)

| Item | Owner | When |
|---|---|---|
| HP-4 GitHub Actions macOS/Windows runner budget | Brad | before W3-06 |
| HP-5 three user-supplied LoROM commercial ROMs | Brad | before P7 exit |
| HP-6 upstream license requests (SingleStepTests/65816, PeterLemon, nes-test-roms) | Brad (goodwill) | anytime |
| HP-7 Gitea reachability from build machine | Brad | before overnight build |
| 10 late-phase stories planning-ticket-only | W7-01/W8-01/W9-01 expansion | phase entry |
| Vetoes on D-001..D-006 | Brad | anytime (rows flagged) |

## Exact launch sequence (when Brad says go)

1. Merge `review/design-review-hardening` → `main` (--no-ff), push both remotes. **[STOP: Brad approves merge]**
2. Preflight: `git status` clean · full gate green on pinned 1.94 · both board validators green (all verified this session, see STATUS gate entry).
3. Configure the executor per PLAYBOOK (D-003): **Sonnet floor**, no auto-frontier, ONE conductor account-wide, hold-list active (W5-04, W5-05 excluded), per-ticket close gate = cargo gate + validate-arch + both board validators.
4. Claimable set at boot: **W0-02** (rf-cart parsing/hashing) and **W0-04** (core traits) — then W0-03, W0-05 unlock.
5. Babysit: monitor wakes on blocked/fatal/idle; on any event, root-cause from evidence (candidate causes → verify → fix), relaunch, and write the LESSONS row before moving on.

## Definition-of-done checklist (kickoff §9)

- [x] Gap register: G-1..G-44 all fixed/ticketed/closed (incl. in-arc closure of G-2, G-12).
- [x] Adopted amendments threaded decision→SRS→stories→design→roadmap→tickets.
- [x] ADRs (16, with Rejected:), failure-modes table (16 rows), contracts doc, trust-ladder state diagram.
- [x] Board: stories linkage, chokepoints split, hold flags, validators green.
- [x] Traceability 100/100 both directions; challenger deliverable-hunt run (17 findings) and closed; residuals listed in STATUS.
- [x] LESSONS.md updated and routed (RF-L-01..08 + analysis queue).
- [x] This report.
- [ ] P7 build launch — awaiting Brad's explicit go.
