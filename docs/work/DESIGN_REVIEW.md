# RetroForge — Full-System Design Review (pre-build hardening)

Date: 2026-07-15 · Branch: `review/design-review-hardening` · Reviewer: design-review arc v2 (Shipwright method)
Inputs: full docs tree (28 files, ~3.5k lines) · plan.json (41 tickets / 316 pts) · workspace (16 skeleton crates, gate green) · exemplars (shipwright, repopulse) · protocol canon (bpm-opencode-experts v2.15.0-in-flight)
**Verdict: strong Phase-0 package — the honesty contract, research provenance, and test-gate discipline are genuinely above exemplar baseline — but the board is NOT executable as written (G-1), traceability is prose-only (G-2), and ~14 doc-mandated deliverables have no owning ticket. Build-ready after the fix pack + P4/P5 validator work.**

Severity: **B** blocker (agents/build break) · **H** high · **M** medium · **L** low.
Status: `fixed` = corrected on this branch · `ticketed` = added to plan.json · `open` = needs Brad / lands in a later arc pass (P4/P5 noted).

## 1. Modularity assessment — PARTIAL PASS (real but under-enforced)

| Property | Evidence | Verdict |
|---|---|---|
| One-way core layering (cores never import upward) | `scripts/validate-arch.sh` greps 4 core crates' manifests; in CI + full gate; ran clean this session (`arch OK`, exit 0) | **machine-enforced** |
| rf-core-api depends on no rf-* crate | same script, dedicated check | **machine-enforced** |
| Enhancement side reads cores only via rf-core-api (ARCH §3: enhance/renderer/debugger never import rf-nes/rf-snes directly) | **no check exists** — the diagram's most load-bearing edge after the core boundary is aspirational | **gap → fixed** (validator extended, see G-16) |
| Determinism: no wall-clock/RNG in cores (FR-CORE-003 "CI (lint + review)") | **no lint exists**; "review" is the only mechanism | **gap → fixed** (grep check added, G-17) |
| `unsafe` requires `// SAFETY:` comment (NFR-007) | workspace `unsafe_code = "warn"` exists; comment requirement had no mechanism | **gap → fixed** (clippy `undocumented_unsafe_blocks`, G-18) |
| Profiles are data, not code (FR-PROF-006) | designed (CI profile-validate) but tool lands W4-02 | deferred, owned |
| Crate responsibility matrix (ARCH §7) vs workspace | 16/16 crates exist and match the table exactly | **pass** |

Weak point: enforcement was concentrated on the boundary everyone already respects (cores) and absent on the boundaries agents are most likely to violate under deadline pressure (enhance→core shortcuts, renderer→rf-nes peeking).

## 2. Persona journeys walked as processes

Gaps live between steps; `[G-x]` marks where a journey broke before this review.

### 2.1 Priya — Player, day-0
Install (GitHub release **[G-7: no release ticket existed]**) → first launch → **empty library [G-21: no first-run/empty-state design existed]** → add ROM folder (Paths settings **[G-19: no ticket owned app-wide settings screens]**) → play NROM game (W1/W2 ✓) → save state (W2-04 ✓, but the slots/screenshots modal UI was unowned **[G-20]**) → quits, resumes tomorrow: per-game settings persist (W2-07 ✓).

### 2.2 Tomás — Tinkerer, daily loop
Opens game → Enhance workspace (W4-05 ✓) → toggles features (FR-ENH-010 ✓) → trusts the **ENHANCED honesty badge [G-22: designed in FRONTEND_UI §1 but in no ticket's acceptance]** → compares side-by-side (W3-04 ✓) → ultrawide stitched view (W4-03 ✓) → wants the canvas to survive restart: rf-cache persistence **[G-10: no ticket owned rf-cache itself — LRU, size cap, eviction]**.

### 2.3 Ada — Profile author, weekly ritual
Research mode → viewers (W4-06 ✓) → labels addresses → **pastes a DataCrystal table [import dialog in DEBUGGER §4, folded into W4-06 acceptance — G-23]** → exports skeleton (W4-06 ✓) → iterates decode rules with **live re-decode on save (E6-S2) [G-24: no owning ticket]** → validates with retroforge-tool (W4-02 ✓) → commits profile. Cliff at step 4 and 6 without fixes. Annotation-store backing (SQLite vs RON) is an explicitly deferred at-ticket-time decision — acceptable, noted.

### 2.4 Kenji — Plugin developer
Reads PLUGINS.md → writes Lua overlay (W4-04 ✓) → debugs in the **Lua console REPL panel (DEBUGGER §5) [G-25: not in any acceptance]** → capability manifest UX (W4-05/plugin list ✓ minimal) → SDK docs land P9 (honest, owned by W9-01).

### 2.5 Sam — Speedrunner/TASer
Frame stepping (W1-06 ✓) → replay record/playback (W1-07 ✓) → determinism CI protects the workflow (✓, genuinely first-class) → rewind honestly deferred to P8 (✓) → verifies runs via .rfreplay final-hash (✓). **Cleanest journey of the five.**

### 2.6 Resume journey (coding agent, cold start)
CLAUDE.md → MASTER_PROMPT → plan.json → claim ticket… **[G-1: the claim protocol itself violates write_scope — see register. Every single session would have started with an illegal edit.]** Docs otherwise excellent for this consumer: precise, ID-rich, trap-listed.

## 3. Customization surfaces

| Knob | Where | Status |
|---|---|---|
| Per-game settings (mode, shaders, toggles, input) | FR-FE-002, W2-07/W4-05 | ✅ designed + owned |
| Profile user overrides (`~/.retroforge/profiles.d/`) | FR-PROF-005, GAME_PROFILES §4 | ⚠️ designed, P2, inspector UI unowned until W9 (accepted residual) |
| Shader chain + params | RENDERER §4, W3-02 | ✅ |
| Anti-flicker per-game exclusions/blink periods | GAME_PROFILES `[antiflicker]` | ✅ |
| Input remap + per-game overrides | W2-06 | ✅ |
| App-wide settings screens (Video/Audio/Paths incl. cache cap) | FRONTEND_UI §2 | ❌ was unowned → **ticketed W2-08** |
| Plugin capability grants + write-ledger | PLUGINS §2 | ✅ designed; UI polish P9 |
| Core accuracy/compat switches | EMULATION_CORES §5 (`CoreConfig`) | ✅ |

## 4. Licensing ledger

Researched 2026-07-15 (every claim carries a source URL in the research transcript; UNVERIFIED marked). Full detail: the per-component table below is the action summary. Note: one research fetch to tcrf.net returned an anti-bot page containing prompt-injection text (instructions to run destructive commands); the agent ignored it and verified on datacrystal.tcrf.net content pages directly.

| Component | License | Constraint | Action |
|---|---|---|---|
| Entire planned Rust crate tree (wgpu, winit, egui/eframe, egui_dock, cpal, rtrb, rubato, gilrs, mlua+vendored Lua/LuaJIT/Luau, hashes, bincode 3, zstd, toml, criterion, ort/ONNX Runtime, wasmtime, rfd) | MIT / Apache-2.0 / Zlib family throughout | winit+cpal are Apache-only (carry NOTICE); libzstd dual BSD-3/GPL-2 — elect BSD | **GREEN** — nothing incompatible |
| **Nova the Squirrel 2 assets** | **All rights reserved** ("Assets… are not licensed to be used outside of this game"); commercially sold | The designated SNES fixture (FR-CORE-037, P7 exit, W5-era flagship-profile strategy) uses unlicensed assets even fetch-only | **RED — founder slate FS-1**: ask NovaSquirrel for written test-use permission, or swap the SNES fixture (libSFX-built + another homebrew) |
| Nova the Squirrel 1 assets | Code GPLv3; **assets CC BY-NC-SA 4.0** + character-use restriction | NC clause: fine for a non-commercial OSS project fetch-only; gray if RetroForge ever has a commercial angle | **RED-if-commercial — founder slate FS-2**: confirm non-commercial posture or swap MVP demo emphasis to Alter Ego (PD) |
| DataCrystal (datacrystal.tcrf.net) | **GFDL 1.2** (verified in page footers) — not CC-BY-SA as commonly assumed | GFDL is copyleft; raw facts aren't copyrightable (Feist), but wholesale table transcription risks taking protected selection/arrangement + prose | **RED→GREEN with policy — founder slate FS-3**: adopt the facts-only transcription policy (drafted into CONSTRAINTS §2, vetoable) |
| xBRZ (Zenju) / CRT-Easymode / zfast_lcd | GPL-3.0 / GPL / GPL-2.0+ | porting the shader source = GPL taint of an MIT/Apache repo | **fixed**: RENDERER §4 now ships **xbr-class from Hyllian's MIT xBR** instead of xBRZ; crt-easymode-class + lcd-grid are behavior-spec clean-room with source-closed rule |
| sharp-bilinear, lcd3x | Public domain (verified headers) | may port directly | GREEN |
| SingleStepTests 65x02 + spc700 · gilyon/snes-tests · undisbeliever · holy-mapperel · libSFX · cc65 | MIT / MIT / Zlib / Zlib / MIT / Zlib (all verified) | keep notices in manifest | GREEN |
| SingleStepTests/**65816** · PeterLemon/SNES · christopherpow/nes-test-roms · nestest(.log) | **No license file/grant** | fetch-from-origin only; never vendor/re-host; CI cache tolerated-but-unlicensed | **fixed**: no-vendor rule in TESTING §3; HP row asks upstream for grants |
| Mesen HD-pack format (hires.txt) | Format docs carry no restriction; Mesen code is GPL-3 | implement from docs only, never read Mesen's parser (Google v. Oracle) | GREEN (rule recorded in RENDERER/ROADMAP context) |
| Alter Ego (Shiru) | informal PD ("consider it Public Domain"); bundled music inherits claim (UNVERIFIED separately) | keep source-zip notice in manifest | GREEN (low risk) |
| GitHub CI fetching | ToS §D.8 permits; §H rate limits | authenticated fetches + Actions cache + pinned commits | GREEN |

Correction recorded: Holy Diver Batman is by Damian Yerrick (pinobatch/holy-mapperel, Zlib), not rainwarrior as docs/research/accuracy-and-testing.md §3 states.

## 5. Gap register

| ID | Sev | Gap | Resolution | Status |
|---|---|---|---|---|
| G-1 | B | **The board's own claim protocol is unexecutable under its own write_scope law**: claiming = editing plan.json; closing = appending docs/STATUS.md; any new dependency = Cargo.lock (+ TECH_STACK.md row per its own rule 3) — none of these files is in ANY ticket's write_scope. The L-04/L-12 seam class, now bitten in its 5th project, this time on file one. A strict executor refuses or violates on every ticket | plan.json schema note now declares a global **always-writable set** (own-ticket `status`/`notes` in plan.json; `docs/STATUS.md` appends; `Cargo.lock`; `docs/TECH_STACK.md` §2 dependency rows); MASTER_PROMPT documents it | fixed |
| — | — | *P4/P5 closure note (2026-07-15): G-2 closed — both validators built, wired into CI + the per-ticket gate, Ralph-converged, challenger-verified. G-12 closed — all 11 13-pt tickets split a/b with dependents rewired; points vocabulary now {1,2,3,5,8}.* | — | — |
| G-2 | B | Traceability is prose-only: tickets carry no `stories[]`, acceptance criteria don't cite FR ids, no validate-plan/validate-traceability exist — decision→FR→story→ticket is unverifiable both directions | P4 builds both validators (shipwright versions adapted); P5 adds `stories[]` linkage via bulk script | open → P4/P5 (in-arc) |
| G-3 | H | ROADMAP "Key tickets" lists cite **17 phantom ticket ids** (`W1-04-bg`, `W3-01-core`, `W6-02-snes`, `W7-01-modes`…) plus duplicates (`W2-02, W2-02, W2-02`; `W1-02` twice) — sed artifact from commit fc3eac7 survived its own fix | ROADMAP key-ticket lists rewritten against the real board | fixed |
| G-4 | H | Test-ROM manifest path has three spellings: `tests/rom-manifest.toml` (board, script, repo) vs `tests/manifest.toml` (TESTING §3) vs `tests/roms/manifest.toml` (CONSTRAINTS §2); download dir `roms/` (gitignore, script) vs `tests/roms/` (TESTING §3) | Canonical: **`tests/rom-manifest.toml`** fetching into **`roms/`**; both docs fixed | fixed |
| G-5 | H | Nightly CI tier does not exist and nothing owns it: Tier-B suites, criterion benches, `benches/baseline.json`, the >10%-regression threshold script (TESTING §1/§7/§9, PLAYBOOK bench gate) are all mandated, all orphan | New ticket **W2-09** (nightly workflow + bench baseline + threshold script) | ticketed |
| G-6 | H | 3-OS CI builds "from Phase 3 onward" (NFR-009) and MVP product-floor "macOS + Linux + Windows builds from CI" have no owning ticket; CI is ubuntu-only | New ticket **W3-06** (mac/win runners, release-shaped build job, software-rasterizer golden frames documented) | ticketed |
| G-7 | H | Release engineering unowned: SCOPE Distribution row (GitHub releases, 3 OS), FR-STATE-005 golden fixtures "from each release" — no ticket produces a release, ever | New ticket **W5-05** (tagged release v0: artifacts ×3 OS, golden fixture archive step, release notes from accuracy table) | ticketed |
| G-8 | H | `retroforge-tool` CLI is FR-DBG-007 **P1** (rom hash/inspect, header parse, trace capture, VRAM/OAM/palette dump, tilemap export, level/map export) but W4-02 owns only `profile validate` + `hash` | New ticket **W4-07** (tool CLI core: inspect/header/dumps/trace/tilemap; level export rides W5-02) | ticketed |
| G-9 | H | Renderer device-loss fallback (FR-REND-007, E4-S3: recover ≤1 s, emulation never dies) in no acceptance | W3-01 acceptance extended (fallback + simulated-device-loss test) — W3-01 is `todo`, edit legal under L-10 | fixed |
| G-10 | H | **rf-cache has no owning ticket** (content-addressed store, LRU residency, size cap — ARCH §7, RENDERER §3, FRONTEND_UI Paths). W4-03 merely consumes it | New ticket **W4-08** (rf-cache v0 + size-cap setting + eviction test) | ticketed |
| G-11 | H | Toolchain floats: `rust-toolchain.toml` says `channel = "stable"`, CI uses `dtolnay/rust-toolchain@stable`, yet TECH_STACK claims "pinned (currently 1.94 line)" — with `clippy -D warnings`, every new stable release can break every open ticket overnight (L-15 class) | Pinned `channel = "1.94"` (matches gate-verified local 1.94.0); CI unchanged (rustup honors the file); TECH_STACK note aligned | fixed |
| G-12 | H | 11 tickets at 13 points (W1-01/04/05, W2-01, W4-03/06, W5-02, W6-01..04) — USER_STORIES' own convention says 13 = "split before scheduling"; W6-01→02→03/04 is a 52-pt serial chokepoint | Split at P5 board-completion (needs acceptance re-partition, done with stories linkage) | open → P5 |
| G-13 | M | FR-FE-004 marks the plugin-manager panel "MVP(core set)" while MVP.md explicitly excludes plugin manager UI | SRS row reworded (plugin manager → P1, not MVP core set) | fixed |
| G-14 | M | Cross-mode state-load test (FR-STATE-007, TESTING §6 row) in no acceptance | W2-04 acceptance extended (Enhanced-state→Accuracy-load continues hash-identical) | fixed |
| G-15 | M | AxROM + Action 53 (FR-CORE-027 P2; SCOPE keeps "~96%" in v1 scope) unowned | Folded into **W7-01** planning-ticket expansion list (NES mapper wave rides SNES-era planning) | fixed |
| G-16 | M | validate-arch.sh checked only core-crate manifests; enhance/renderer/debugger/plugin-sdk/ai importing rf-nes/rf-snes directly (forbidden by ARCH §3) was uncheckable | Script extended: only `retroforge` (bin) + `rf-harness` may depend on core crates | fixed |
| G-17 | M | FR-CORE-003 (no wall-clock/RNG in cores) claimed "CI (lint)" — no lint existed | validate-arch.sh now greps core-crate sources for `std::time`, `SystemTime`, `Instant::now`, `rand::`/`fastrand`/`getrandom` | fixed |
| G-18 | M | NFR-007 `// SAFETY:` comment requirement had no mechanism | `clippy::undocumented_unsafe_blocks = "warn"` added to workspace lints | fixed |
| G-19 | M | App-wide settings screens (Video/Audio/Paths + cache cap — FRONTEND_UI §2) unowned | New ticket **W2-08** (settings screens v0) | ticketed |
| G-20 | M | Save-state manager modal (10 slots, screenshots, mods-warning flag — FRONTEND_UI §3.2) unowned | W2-04 acceptance extended (slots UI + screenshot thumbnails) | fixed |
| G-21 | M | First-run/empty-library state undesigned + unowned (Priya day-0 dead-ends) | FRONTEND_UI §3.1 empty-state paragraph added; W2-07 acceptance extended | fixed |
| G-22 | M | ENHANCED honesty badge + hold-to-peek (FRONTEND_UI §1.2 — the product's trust centerpiece) in no acceptance | W4-05 acceptance extended | fixed |
| G-23 | M | DataCrystal TSV import dialog (DEBUGGER §4) unowned | W4-06 acceptance extended | fixed |
| G-24 | M | Live re-decode authoring loop (E6-S2, FRONTEND_UI §3.5) — the "profile takes hours not days" promise — unowned | New ticket **W5-06** (profile hot-reload + inline decode errors + preview panel) | ticketed |
| G-25 | M | Lua console REPL panel (DEBUGGER §5, FRONTEND_UI §3.4) unowned | W4-04 acceptance extended | fixed |
| G-26 | M | PAL support named "a Phase 7 config" (EMULATION_CORES §3) but absent from ROADMAP P7 and W7-01's expansion list | Added to W7-01 expansion list | fixed |
| G-27 | M | HD-pack loader + Mesen import + pack builder are ROADMAP-P8/SCOPE-in-scope but missing from W8-01's expansion list | Added to W8-01 list | fixed |
| G-28 | M | Palette LUT `.pal` assets + generator tool (RENDERER §2 "checked-in assets") unowned | W3-01 acceptance extended | fixed |
| G-29 | M | PREREQUISITES.md absent: P7 exit needs **user-supplied commercial ROMs**; stretch needs Micro Mages; releases need mac/win CI runners; Gitea remote is LAN-bound | docs/PREREQUISITES.md created (HP-1..HP-6) | fixed |
| G-30 | M | plan.json schema omits `notes` while MASTER_PROMPT §3 mandates HANDOFF notes *in the notes field*; unversed executors will improvise shapes (L-11 string-vs-array crashed a harness 15×) | Schema note now declares `notes?: string[]` (array of strings, append-only) | fixed |
| G-31 | L | ARCHITECTURE §5 cites "ticket C-01" — an id scheme that never existed (board says W0-04) | Fixed to W0-04 | fixed |
| G-32 | L | SRS FR-CORE-026 gates on Nova **and Alter Ego** replays; TESTING §4 has only the Nova row | Alter Ego replay row added to TESTING §4 | fixed |
| G-33 | L | Event viewer + audio viewer designed (DEBUGGER §3) but absent from FR-DBG-001 and every acceptance | FR-DBG-001 extended; W4-06 acceptance extended (event viewer; audio scopes ride W2-05) | fixed |
| G-34 | L | Phase-1 exit drift: ROADMAP says "Alter Ego title screen renders"; TESTING §8 says "NROM boots 2 homebrew titles" | Canonical: 2 homebrew titles (Alter Ego + 1 neslib fixture); both docs aligned | fixed |
| G-35 | L | W6-01 buries a planning duty ("refine remaining W6+ tickets at phase entry") inside a 13-pt CPU ticket — L-03 "all of the set" kin: it will be skipped | Moved to new **W6-00** planning ticket (2 pts, depends W5-04) | fixed |
| G-36 | L | docs/work/telemetry.jsonl records a **failing** `validate-ux-spec` run (gaps:1, exit 1, 2026-07-07) from validators that exist nowhere in-repo; the failure was never triaged (L-14/L-20 kin) | Documented here; superseded by the in-repo validator suite (G-2). Original gap unknowable — treated as expired | fixed (recorded) |
| G-37 | L | Historical counts read stale ("plan.json (39 tickets)" in STATUS/TRACEABILITY; board now 41→49 after this fix pack) | Left as-is — they were true when written; ledgers aren't rewritten. Validator P9 report is the live count | fixed (no-op, recorded) |
| G-38 | L | CI golden-frame env naming drift: workflow uses `WGPU_BACKEND: gl` + `LIBGL_ALWAYS_SOFTWARE` while docs say "llvmpipe/lavapipe (Vulkan)" | Note added to W3-01 acceptance (decide + align at implementation) | fixed |
| G-39 | H | **Nova the Squirrel 2 assets are all-rights-reserved** (and the game is sold commercially) — the SNES fixture for FR-CORE-037, the P7 exit gate, and the flagship SNES profile is unlicensed for our use (R-16's trigger has fired) | ~~FS-1~~ **Resolved same day by D-001** (self-contained fixture doctrine — Brad's direction): RF-Scroller-S via W6-00 replaces Nova 2 everywhere | fixed (D-001) |
| G-40 | M | Nova 1 assets are CC BY-NC-SA 4.0 + character-use restriction — MVP demo depends on a fixture that constrains any commercial future | ~~FS-2~~ **Resolved same day by D-001**: RF-Scroller (W2-10, in-repo) is the MVP demo; Alter Ego (PD) stays as independent proof; Nova removed everywhere | fixed (D-001) |
| G-41 | M | DataCrystal is **GFDL 1.2** — wholesale RAM-map table transcription into MIT/Apache profiles risks copyleft taint (facts are safe; curated selection/arrangement + prose are not) | Facts-only transcription policy drafted into CONSTRAINTS §2 (**vetoable**, FS-3); GAME_PROFILES authoring pipeline note | fixed (policy) + open (sign-off) |
| G-42 | M | RENDERER §4 named GPL-derived shaders (xbrz-class, crt-easymode-class) and an unlicensed one (lcd-grid) as first-party ships in an MIT/Apache repo | RENDERER §4 now ships **xbr-class (Hyllian, MIT)**; clean-room source-closed rule recorded for CRT/LCD looks; W3-02 acceptance updated | fixed |
| G-43 | L | Four test-content sources have no license grant (SingleStepTests/65816, PeterLemon/SNES, christopherpow/nes-test-roms, nestest) — vendoring or re-hosting would be unlicensed copying | TESTING §3 no-vendor/no-rehost rule; PREREQUISITES HP row: request upstream grants | fixed |
| G-44 | L | Research doc misattributes Holy Diver Batman to rainwarrior (actual: Damian Yerrick, pinobatch/holy-mapperel, Zlib) | Correction noted in DESIGN_REVIEW §4 (research briefs are dated snapshots; not rewritten) | fixed (recorded) |

**Also verified — no action needed:** dependency graph acyclic, no dangling `depends_on`, all write_scope crates exist; ticket statuses consistent (1 done / 40 todo matched STATUS ledger); Accuracy-mode defaults (FR-MODE-003) consistently threaded across 6 docs; honesty-contract language consistent everywhere it appears; determinism invariant identically stated in CLAUDE.md/CONSTRAINTS/ARCHITECTURE/MASTER_PROMPT; no ROM-in-repo violations (manifest-fetch design consistent post-G-4); NON_GOALS internally consistent with SCOPE's Won't column; research docs carry per-claim URLs and honest [thin] flags (exemplar-grade); bincode-3 trap documented in four places; wgpu/egui lockstep rule in three.

## 6. Spec-vs-built drift appendix

Code is 16 empty skeletons + CI + arch validator (W0-01 only) — too early for real drift. The only code-level findings: the toolchain float (G-11), the validator coverage holes (G-16/17/18), and `scripts/fetch-test-roms.sh` correctly refusing until W0-03 (good pattern: it names its ticket). Workspace matches ARCH §7 crate-for-crate.

## 7. What happens next (the arc)

P2 domain interrogations → R-x index → **STOP for Brad** → P3 threading → P4 validators + ADRs + failure modes + Ralph loop → P5 stories linkage + 13-pt splits + fresh-agent challenger → P6 gate + readiness report. Build launch (P7) only on explicit go.
