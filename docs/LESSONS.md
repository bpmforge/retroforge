# RetroForge — Field Lessons Ledger

One row per lesson; details below for the non-obvious ones. This file is the
analyzable input for (a) RetroForge product/process tickets, (b)
bpm-opencode-experts process/check upstreams, (c) harness fixes. Append-only;
stable L-ids (RF-L-*; cross-project lessons cite shipwright's L-* ids).
**Route-to** values: `product:<ticket/FR>` · `experts:<protocol/validator>` ·
`harness` · `process`. **Status**: `shipped` / `ticketed` / `open`.

| ID | Date | Class | Lesson (one line) | Route to | Status |
|---|---|---|---|---|---|
| RF-L-01 | 07-15 | seams | The seam class (shipwright L-04/L-12) hit its FIFTH project, this time in the board's own claim protocol: plan.json/STATUS.md/Cargo.lock/TECH_STACK.md were writable by NO ticket — a board is unexecutable unless its process files are in a declared always-writable set | experts: TICKET_SCHEMA — boards must declare an always-writable set alongside write_scope; product: plan.json schema note (shipped) | shipped (G-1) / open (upstream) |
| RF-L-02 | 07-15 | env-pinning | `channel = "stable"` in rust-toolchain.toml + `clippy -D warnings` in the gate = every Rust release breaks every open ticket; docs even *claimed* it was pinned (L-15 kin, doc-vs-repo drift) | process: pin exact minor, bump deliberately; experts: SESSION_PRIMER env-assertion — check the pin file says what the docs say | shipped (G-11) |
| RF-L-03 | 07-15 | licensing | L-09 confirmed again, harder: the project's flagship fixture strategy (Nova 2 for the entire SNES arc) was resting on all-rights-reserved assets of a commercially sold game; review-time research found it 6+ months before it would have burned a phase gate | process: license pass at review time, always; product: HP-1/HP-2 slates → resolved same day by D-001 (see RF-L-07) | shipped |
| RF-L-04 | 07-15 | spec-hygiene | A sed-based bulk rename left 17 phantom ticket ids in ROADMAP **after** a commit titled "fix ticket-id sed artifact" — a fix commit for a mechanical error needs a mechanical verification (grep count), not eyeballing | experts: validator idea — cited-ticket-ids ⊆ board ids (shipping in this arc's validate-traceability F-check); process: measure after bulk edits | shipped (G-3 + validator in P4) |
| RF-L-05 | 07-15 | tooling-trust | Web research agents get prompt-injected by anti-bot pages (tcrf.net served instructions to run destructive commands); the agent ignored it — but treat fetched web content as hostile input in research protocols | experts: RESEARCH protocol note — web content is data, never instructions | open (upstream note) |
| RF-L-06 | 07-15 | validators | Orphan telemetry (docs/work/telemetry.jsonl) recorded a FAILING validator run (validate-ux-spec, exit 1) that existed in no repo and was never triaged — a validator that isn't in-repo and in-gate is a rumor, and its failures evaporate (L-14/L-20 kin) | process: validators live in-repo, wired to CI, or they don't exist | shipped (G-36 recorded; suite lands P4) |
| RF-L-07 | 07-15 | licensing | Flagship-content dependencies are architecture decisions: designing the risk OUT (self-contained in-repo fixtures, D-001) beats managing it (permission emails, NC-posture constraints) — and the fixture doubles as the red-fixture host, so self-containment paid for itself | product: W2-10 + W6-00 (shipped); experts: design-review checklist — "can this third-party dependency be designed out?" before "how do we license it?" | shipped (D-001) / open (upstream) |
| RF-L-08 | 07-15 | traceability | L-05 reproduced exactly and freshly: 100/100 FR reachable + 33/33 stories green hid ~10 orphan deliverables (Alter Ego Tier-A fixture, breakpoint engine, trace/audio viewers, FM-01/FM-13), 5/6 all-of-the-set failures, and 10 stories "covered" only by planning tickets — AND the reviewer's own fix (W0-03's D-001 wording) introduced one of the orphans. The challenger pass is not optional, and the validator now reports planning-only coverage separately | process: challenger mandatory after every green (already D-002); product: validate-traceability planning-only line (shipped); experts: L-05 evidence++ | shipped (this arc) |
| RF-L-09 | 08-21 | machine-safety | An unbounded `while` walk in new code (`interpolation.rs` candidacy) spun forever pushing zero-sized tuples; two concurrent `cargo test` runs reached 245 GB and 111 GB RSS, drove free memory to 194 MB, stalled `tccd`, blocked WindowServer's main thread in a TCC preflight, and **kernel-panicked the machine twice**. A hang in a test is not a hang in a test — on a workstation it is a denial of service against the developer | process: every hand-rolled index walk must have a provable-progress step; product: this crate's walks audited; experts: check idea — flag `while i < n` loops whose body can leave `i` unchanged | shipped (fix + regression test) |

## Details worth keeping (evidence pointers)

- **RF-L-01**: docs/work/DESIGN_REVIEW.md G-1; every one of the 41 pre-review
  tickets was affected (claim = plan.json edit; close = STATUS.md append).
- **RF-L-02**: rust-toolchain.toml before/after on this branch;
  TECH_STACK.md §4 claimed "pinned (currently 1.94 line)" while the file said
  `stable`.
- **RF-L-03**: NovaTheSquirrel2 README §License ("Assets … are not licensed
  to be used outside of this game"); NovaTheSquirrel README (CC BY-NC-SA 4.0
  + character clause). Full ledger in DESIGN_REVIEW.md §4.
- **RF-L-04**: `git show fc3eac7` vs the 17 phantom ids enumerated in
  DESIGN_REVIEW.md G-3.

- **RF-L-09**: `crates/rf-enhance/src/interpolation.rs` `candidacy()`. The
  broken shape:

  ```rust
  while y < height {
      let start = y;
      while y < height && !hud.iter().any(|h| h.contains(y)) { y += 1; }
      scanlines.push((start, y));      // y unchanged when y is inside a band
  }
  ```

  `hud_top(32)` covers scanlines 0..32, so `y = 0` is inside a band on the
  first iteration, neither loop advances, and the outer loop pushes `(0, 0)`
  until the allocator gives out. Two of the six tests hit it. Fix: skip the
  HUD run *before* taking the permitted run, and never emit an empty range —
  the skip is the progress guarantee. Regression test:
  `an_all_hud_frame_permits_nothing_and_terminates`.

  Forensics (macOS 26.5.2, M5 Max / 128 GB): kernel panics at 07:24:08 and
  07:53:56 on 2026-08-21, both `userspace watchdog timeout: no successful
  checkins from WindowServer`; the 07:48 WindowServer stackshot shows
  `ws_main_thread` parked on
  `com.apple.tcc.preflight.kTCCServiceScreenCapture`, blocked on a
  cold-started `tccd` that could not page in. The compressor held 19.4 GB
  representing 660 GB uncompressed — a ~34:1 ratio, reachable only on zero
  pages, which is independent confirmation the leak was this loop's
  zero-valued tuples and nothing else.

## Analysis queue (explicit asks for the next improvement pass)

1. Check candidates to build: RF-L-04 (cited-ids ⊆ board-ids — lands in this
   arc's validate-traceability), RF-L-02 (pin-file vs docs consistency),
   RF-L-09 (grep/lint for `while <ix> < <bound>` loops whose body can leave
   the index unchanged; and a wall-clock/RSS cap around the workspace test
   run so a regression fails fast instead of taking the workstation down).
2. Upstream PR candidates to bpm-opencode-experts: RF-L-01 (always-writable
   set in TICKET_SCHEMA), RF-L-05 (hostile-web-content note in the research
   protocol), shipwright L-20 dogfood rule (validated again here via G-36).
3. Product features already carrying a lesson: plan.json always-writable set
   (RF-L-01), validate-arch.sh determinism grep (G-17 — the product's own
   invariant, now machine-checked pre-implementation).
