# Full-System Design Review & Hardening Kickoff — v2 (the Shipwright Method)

**For: a fresh Claude Code session opened in the target project's root.**
v1 was written after the RepoPulse arc; **v2 folds in the Shipwright arc (2026-07-14/15)**:
its 30-gap register, the adopt-all + follow-up-domain cycle, the challenger's refutation of
green traceability, the overnight autonomous build relaunch, and 22 field lessons
(`~/Code/shipwright/docs/LESSONS.md` — read it; its L-ids are cited throughout).
Brad will say "read DESIGN_REVIEW_KICKOFF.md and go" — this file is your complete
instruction set. Copy it into the target repo root first so it travels with the project.

---

## FILL IN PER PROJECT (Brad edits this block before launch, or you ask ONE question batch)

- **PROJECT:** retroforge · root: ~/Code/retroforge
- **BOARD:** plan.json
- **BUILD STATE:** <never started | paused (why) | active (then §0 rules about not disturbing it)>
- **LOCKED DECISIONS FILE:** none — P1 creates it
- **EXEMPLAR ARTIFACTS (read-only, formats to crib):**
  - `~/Code/shipwright` — the richest exemplar: `docs/work/DESIGN_REVIEW.md` (gap register),
    `docs/work/IMPROVEMENT_RECOMMENDATIONS.md` (R-x index + adoption banner + addendum pattern),
    `scripts/validate-plan.mjs` (P1–P9 incl. territory-release lane law + scaffold flags),
    `scripts/validate-traceability.mjs` (F1–F6/W1–W3 + slash-compound expansion),
    `docs/design/{GATE_ECONOMICS,IMPROVEMENT_PLANS,CONTRACTS}.md`, `docs/LESSONS.md`,
    `docs/PREREQUISITES.md`, `PLAYBOOK.md` §Ralph+challenger, `scripts/conductor.mjs` + `supervise.sh`
  - `~/Code/repopulse` — `docs/work/DESIGN_REVIEW.md`, `UX_AGENTS_IMPROVEMENT_PLANS.md`,
    `docs/design/SECURITY_SUITE.md` §3.6 (VEX), `CODE_HEALTH_SUITE.md` §8 (rule lifecycle)
  - `~/Code/bpm-opencode-experts` — the protocol canon (MICRO_LOOP cap-2, RALPH_WIGGUM,
    GATE_SCORING asymmetry + "re-ran independently", FIX_VERIFY movement classes 6/12,
    CHALLENGER, CONTEXT_BUDGET, BOUNDED_TASK_CONTRACT, models.json tier gates). **Check its
    CURRENT version first** — it moves fast (Shipwright's pause condition was already
    satisfied when checked; the ALIGNMENT_MATRIX was 9 releases stale).

## 0. What you are doing, and what you are NOT doing

You are running a **multi-domain design review and hardening pass** over the project's
design package and board, ending with a board so complete, contradiction-free, and
machine-validated that inexpensive coding agents execute it wave-by-wave with quality
gates — then (if Brad says go) **launching the unattended build yourself** (§P7).
Reference outcomes: RepoPulse 44→89 tickets, 122/122 FR traced; Shipwright 65→84 tickets /
268→329 pts, 81/81 FR + 54/54 stories machine-traced, challenger-verified, build relaunched
same day with 5 tickets landed overnight.

You are NOT:
- Re-litigating locked decisions. Changes are **amendments Brad approves** — you propose,
  never silently adopt. His approvals arrive as plain sentences ("adopt all"); record them
  as decision-ledger rows the same hour, flagged vetoable when they rode a bundle (L-08 kin).
- Touching running automation (conductors, supervisors, cron) except at §P7 with his go.
- Running parallel heavy agents. One account: max 3 parallel read/research agents; ONE
  build conductor account-wide, ever (L-21 — parallel conductors halved throughput).
- Leaving uncommitted founder edits in the tree. **First action on arrival: `git status`.
  Anything dirty is either committed onto your review branch with attribution or surfaced
  as a question — never left floating (L-08: an uncommitted edit is an invisible decision).**

## 1. Load order (before anything else)

1. The repo's own laws: CLAUDE.md → executor prompts (MASTER_PROMPT/PLAYBOOK equivalents) → board.
2. Locked decisions + founding brief/blueprint.
3. **The project's own telemetry first** — build logs, field reports, status ledgers,
   incident notes. Real field lessons outrank generic advice, always.
4. The full docs tree + the board file.
5. Exemplar artifacts (block above) — formats to crib, not reinvent.
6. `~/Code/shipwright/docs/LESSONS.md` — the current cross-project lessons ledger.

## 2. The end state (keep in view the whole time)

1. **Decision-complete design packages cheap models can execute**: IDs everywhere, tables
   over prose, worked examples, canonical JSON shapes, known-traps list, vocabulary canon
   (one glossary block: status axes, severity scales, casing — cheap agents copy exactly).
2. **Traceability as a machine property**: decision → scope → FR/NFR → story → ticket →
   design-doc → commit, validated BOTH directions in CI; dangling refs hard-fail.
   **AND (L-05): ID-graph green is NOT deliverable ownership — the challenger pass hunts
   doc-mandated deliverables (endpoints, tables, screens, CI jobs, ops commands) that no
   ticket owns. Both checks, always.**
3. **Quality loops in process AND product**: wave gates run INVENTORY→VERIFY→GAP (cap 3,
   byte-identical-gap-set halt) then a fresh-agent CHALLENGER (maker≠verifier,
   "re-ran independently: command/counts/exit-code" on every accepted criterion,
   CONTRADICTED reopens tickets).
4. **Rules-first, LLM-second**: deterministic catalogs/validators are truth; LLMs order,
   narrate, draft, propose — never invent, activate, or dismiss. Zero-LLM functionality
   on every surface.
5. **Trust economics**: measured FP rates, shadow modes, red-fixture-gated promotion,
   justification-gated suppression that auto-reopens, raw-vs-effective counts never hidden.
   (Shipwright D-014 is the reference design: `docs/design/GATE_ECONOMICS.md`.)

## 3. The arc — run IN ORDER (STOP = present to Brad before continuing)

Feature branch `review/design-review-hardening`; commit per pass with detailed bodies;
push BOTH remotes every commit (note unsynced state if one is offline).

**P0 — Preflight (new in v2).** `git status` (see §0); verify the toolchain runs (the full
test gate) — if a native module is ABI-broken, suspect a runtime-version mismatch in
automation scripts before suspecting code (L-15: Shipwright's supervisor hardcoded Node 24
against a Node 22 pin); inventory existing validators/scripts; note runtime pins.

**P1 — Full design review → gap register + fix pack.** Read everything. Produce
`docs/work/DESIGN_REVIEW.md` (crib the format): modularity assessment (machine-enforced or
aspirational? Shipwright's "ESLint-enforced" claim was false with no owning ticket),
persona journeys walked as processes (gaps live BETWEEN steps: day-0, daily loop, weekly
ritual, resume journey, trust-graduation moments), customization surfaces, licensing
ledger, spec-vs-built drift (if any code exists), and the G-x gap register
(Sev B/H/M/L; Status fixed/ticketed/open). **v2 additions, both proven headline-finders:**
- **Dogfood-as-review (L-20): run the project's own validators against its own board and
  process.** Shipwright's board failed its own lane validator 66× — the single
  highest-leverage finding of the review. If the product validates X, feed it its own X.
- **License/ToS research pass NOW, not at launch (L-09)**: every dependency, every tool the
  validators shell out to, every provider/service ToS — per-claim source URLs, UNVERIFIED
  marked, red flags become founder slates. (Shipwright found a user-account-ban risk in a
  locked MVP decision this way.)
Then APPLY the fix pack: contradictions fixed, missing tickets added, validators green.
The missing-ticket classes to sweep (all found again at Shipwright): CI itself; ops
lifecycle (healthz, doctor, backup + EXECUTED restore drill, service install, retention);
human prerequisites (accounts/tokens/hardware — make PREREQUISITES.md with HP-ids);
settings/admin UI for every config the docs mention; API mechanics (OpenAPI, idempotency,
error wire format, route-walker); a11y CI gate; empty states; seam/public-surface backlog
(barrels, package.json deps — L-04/L-12); "runs"-style tables nobody creates; the
project's own automation scripts (L-15).

**P2 — Domain interrogations → R-x index → STOP.** Per-domain passes: process/UX
persona-by-persona; loop/agent architecture vs the CURRENT protocol canon (check version
first!); reports→action (the Improvement Plans pattern); **false-positive economics** (Brad
WILL push here — design the funnel: dedup → scope → applicability → effective-risk →
propose-never-auto-dismiss → justified suppression that reopens; rule lifecycle
shadow→advisory→gate with measured-FP promotion); content/extensibility (deny-by-default
license-gated intake); core-brain audit end-to-end against field reports (is every observed
failure mode in the docs' failure table? is the symlink/realpath containment class closed
everywhere paths are trusted?). Produce the R-x index (effort, wave, needs-amendment) +
proposed amendments with recommendations. **STOP: Brad adopts/rejects.**
**Expect his reply to open new domains (it did both times — the best decisions come from
his follow-ups). Treat each as a first-class interrogation: research it, design it fully,
thread it completely, and add an addendum section + updated adoption banner.**

**P3 — Adoption threading.** For each adopted item: DECISIONS row → SCOPE → SRS FR/NFR
(with acceptance sketches) → design doc → DB deltas → stories → ROADMAP wave/exit criteria
→ board tickets. Never skip a link. **Law (L-10): never amend a done ticket's acceptance —
new work gets a new ticket.** Record bundle-adopted decisions as vetoable.

**P4 — Architecture completion.** Sequence/state diagrams per adopted flow; ADRs with a
"Rejected:" line for every load-bearing choice; failure-modes table rows for every moving
part the field reports ever hit (truncated review, limit pause, oversized output/ENOBUFS,
bookkeeping-vs-verdict divergence, state-drift refusal); plug-in CONTRACTS.md; build/adapt
`validate-plan.mjs` + `validate-traceability.mjs` from the Shipwright versions (they
already encode territory-release + scaffold exemptions + slash-compound expansion + the
write-scope deliverable exemption). Run the Ralph loop over the package until it converges
(cap 3, byte-identical halt). Codify Ralph+challenger in the project PLAYBOOK if absent.

**P5 — Board completion.** `stories: []` linkage on every ticket (ONE bulk node script —
FR-intersection + manual overrides where intersection lies); split any dependency
chokepoint (≤3 pts each); hold-list/human-pair flags on trust-core lanes; scaffold flags
on genuinely broad bootstrap tickets. Then the **CHALLENGER completeness pass — a fresh
agent, never you, mandated to REFUTE** (v2 mandate, all five proven necessary):
1. Independently re-run every validator (record `re-ran independently: cmd — counts — exit`).
2. Hunt doc-mandated deliverables with no owning ticket (endpoints, tables, screens, CI
   jobs, ops commands) — the ID graph cannot see these (L-05: 16 orphans behind a green 81/81).
3. Spot-check acceptance vs cited docs for the **"ALL of the set" failure** (L-03: it
   recurred in the very ticket that formalized the rule).
4. Verify no split/merge lost acceptance; no done ticket carries unmet amendments (L-10).
5. Sample story-linkage honesty (a link that satisfies the counter but not the story).
Close every confirmed gap same-day.

**P6 — Gate entry + readiness report.** STATUS gate section with independent re-run
evidence for every validator; accepted residuals LISTED, never silently waived; readiness
report: what changed, open decisions (vetoables flagged), exact resume/launch sequence.

**P7 — Build launch (only on Brad's explicit go; he'll often say "leave it on overnight").**
- Fix automation runtime pins FIRST (resolve from .nvmrc-style files + boot assertions, L-15).
- Content/library resync if the project imports from a canonical repo — against CURRENT
  HEAD, then re-audit counts (all-of-the-set) (L-03 kin).
- Model policy: **Sonnet floor** (haiku/small only for ≤1-pt trivial, L-18); launch
  **without** auto-frontier — hard tickets park for the morning queue (escalation-token
  principle by configuration, D-018 pattern).
- Hold-list: trust-core / human-pair tickets excluded from unattended claims, logged at boot.
- Per-ticket gate includes the board validators (an agent's plan edits must keep the
  citation graph green — catches hallucinated doc paths automatically, L-13).
- Evidence preservation: blocked-ticket branches renamed OUT of any namespace the crash
  cleanup deletes (L-16); block notes written with schema-tolerant code (string vs array
  crashed Shipwright's harness 15×, destroying evidence each time — L-11).
- ONE conductor account-wide (L-21). `caffeinate` + supervisor + STOP-file semantics.
- Then babysit: a background monitor that wakes you on ticket.blocked / fatal /
  security.critical / idle / limit events (or a 2h heartbeat). When it fires: root-cause
  from evidence (list candidate causes, verify, THEN fix — Shipwright's morning crash-loop
  was two stacked causes), fix, relaunch, and **write the lesson down (§8) before moving on**.

## 4. Method laws (non-negotiable; v2 additions marked ●)

1. Contradiction hunt first: enum vocabularies, status words, severity scales, table
   names, landing-wave claims ("Lands: W0" vs a board that says W8 — L-06), runner names
   (npm vs pnpm). Pick ONE canonical value, fix every doc, add the vocabulary-canon block.
2. Every security control needs a ticket home; every "lands at wave X" claim is checked
   against the board (L-06).
3. The missing-ticket classes (P1 list) — check every one, every project.
4. Acceptance criteria that NAME examples get the examples, not the set — write "ALL of
   X", and have the challenger check it (L-03).
5. No dependency chokepoints; ceiling-while-progressing = split the ticket.
6. Licensing ledger with actions, not vibes; ToS research at review time (L-09). Standing
   verdicts: AGPL tools = subprocess-only/optional, never vendored; registry rule packs
   with no-compete clauses = format-compatible only; MPL = fine unmodified; source-available
   hosts = endpoint-only, never bundled; per-user BYOK, never pooled keys.
7. Signals, not grades: raw counts never hidden; honest states; basis labels on heuristics.
8. Machine-checkable verify criteria on anything called done; refuse to loop otherwise.
9. ● Never amend a done ticket's acceptance (L-10).
10. ● Territory releases at `done`; scaffold tickets declare exemption explicitly (L-07 —
    the default any-status overlap rule made a completed scaffold a 58-violation landmine).
11. ● Seams are mechanical: a ticket whose deliverable imports a sibling package includes
    that package's manifest in write_scope — check it at plan time (L-04/L-12: this class
    has now bitten FOUR times across two repos, including the reviewer who wrote the rule).
12. ● Validators run bare or under pipefail — a `| tail` swallowed a FAIL into a merge (L-14).
13. ● Scripts arg-guard: unknown flags refuse, never default to executing (L-19: `--help`
    ran a full content import).
14. ● Persist evidence BEFORE mutating status, in harnesses too (L-11).
15. ● Schema-tolerant readers for human-and-agent-edited files (notes: string|array — L-11).

## 5. Research protocol

Explore agents for repo mining (conclusions, not dumps); general-purpose + WebSearch for
external research — every claim carries a source URL, unconfirmed marked UNVERIFIED,
primary sources over blogs. Max 3 parallel agents, launched in one message. Never re-do an
agent's sweep yourself. Evidence lands in the design docs as "Why (evidence basis)" tables
with numbers + sources. Skip external research when a proven internal design already
answers the question — and say so explicitly in the doc (cited internal > UNVERIFIED web).

## 6. Validators & board standards

Crib `~/Code/shipwright/scripts/validate-{plan,traceability}.mjs` (supersede the repopulse
versions — they encode v2 laws 10/11 hooks). Adapt the closed vocabularies (modules, lanes,
statuses, points ∈ {1,2,3,5,8}, ticket key set incl. `stories` + optional `scaffold`).
Definition of review-done: **all validators green, N/N FRs and N/N stories covered both
directions, zero dangling refs, challenger pass recorded with independent re-runs, accepted
residuals listed.** Wire both validators into CI AND into the build harness's per-ticket
gate (L-13).

## 7. Operating rules

- Feature branch; commit per pass; push both remotes; keep chat concise, checkpoint via commits.
- STOP-and-ask: adoption (P2→P3), anything touching a locked decision or NON-GOAL,
  anything that would start/stop a build, the final merge, and §P7 launch.
- Mid-review founder questions = new first-class domains (both arcs proved it). Design
  fully, thread completely, extend the adoption banner.
- Save a project memory at the end: branch, adopted/rejected, open items, resume sequence.
- End-of-arc: update this file's lessons if you learned something the laws don't cover —
  this prompt is itself append-only versioned method.

## 8. The LESSONS ledger (required, from day one)

Create/append `docs/LESSONS.md` in the standard format (copy the header from
`~/Code/shipwright/docs/LESSONS.md`): stable L-ids, one-line lesson, **route-to**
(`product:<ticket/FR>` | `experts:<protocol/validator>` | `harness` | `process`), status
(shipped/ticketed/open), evidence pointers, and the closing **analysis queue** (check
candidates, upstream-PR candidates, features-carrying-lessons map). Every incident during
the arc AND the build gets a row before you move on. This file is the raw material for
product features and bpm-opencode-experts process improvements — treat it as a deliverable,
not a diary.

## 9. Definition of done (the full bar)

- [ ] Gap register: every gap fixed, ticketed, or open-with-owner.
- [ ] All adopted amendments threaded decision→scope→SRS→stories→design→DB→roadmap→tickets.
- [ ] Diagrams/ADRs(with Rejected:)/failure rows for every adopted flow and moving part.
- [ ] Board: stories linkage; chokepoints split; hold/scaffold flags; validators green.
- [ ] Traceability 100% both directions + challenger deliverable-hunt clean; residuals listed.
- [ ] LESSONS.md updated and routed.
- [ ] Readiness report: changes, vetoables, open slates, exact launch sequence.
- [ ] (If P7) build launched: sonnet floor, frontier-gated, hold-list active, monitor armed,
      first ticket claimed and verified producing.

## 10. Target projects (surveyed 2026-07-15 from ~/Code; Brad picks the order)

**Tier 1 — active, review-arc-ready:**
| Project | Signal | Notes for the arc |
|---|---|---|
| `bpm-agent-amplifier` | 460 commits, active 07-13 | The program repo. Its conductor is the most advanced harness (escalation tokens, session receipts, advisor ladder R0–R5) but its own board is markdown (write-race, T10.6) and RELEASE_TRACKER header is stale — a board+tracker hardening arc fits perfectly. |
| `bpm-memory-mcp` | 54 commits, active 07-14 | Real product (memory MCP). Likely no SDLC package yet — the arc's P1 output IS the founding package. |
| `ai-assistant-agent` | 995 commits, 1749 files, active 07-09 | The Jarvis/Foreman runtime — Shipwright's source system. Big; needs Onboard-mode landscape pass before the register. |
| `bpm-code-search-mcp` | active 07-14, small | Light arc (1–2 days): contracts, licensing, board, CI. |
| `playwright-search` | active 07-14, small | Same light-arc shape; it's load-bearing for all research agents — worth the hardening. |
| `ai-daytrader` | 58 commits, 07-05, origin-only | Active project, single remote (add GitHub per dual-remote law). Domain risk (trading) = the FP-economics + NEVER-AUTO interrogations matter double here. |
| `retroforge` | 8 commits, 07-06, early | Early-phase: run the arc as blueprint-first (Phase 2.5) rather than retrofit. |

**Tier 2 — real but paused (arc when resumed):** `kryptkeeper` (199c, 06-19),
`Flow-Threat-Model` + `ThreatForge-internal` (06-20; check which is canonical before
touching), `vulnforge` (06-11).

**Excluded, with reasons:** `claude-experts` (GENERATED from bpm-opencode-experts — never
edit directly), `repopulse` + `shipwright` (arcs done — they're the exemplars),
`bpm-opencode-experts` (the canon itself — it receives upstream PRs from LESSONS analysis
queues rather than a review arc, though a lessons-consolidation pass over all projects'
LESSONS.md files is a strong candidate once 2–3 more arcs complete), `test`/pre-2026
dormants (theron, ThreatAssist, Code-Assist-CLI, threat-model-platform-old, etc.).

Now go: §P0 preflight, then §1 load order, then P1. Brad is not watching in real time —
work autonomously between STOPs, put everything he needs in commits and the final report,
and leave the LESSONS ledger better than you found it.
