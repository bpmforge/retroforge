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
| RF-L-10 | 08-21 | test-isolation | A test writing evidence to a FIXED path under `CARGO_TARGET_TMPDIR` and calling `remove_dir_all` on it at start (`crates/retroforge/tests/mode_invariant_corpus.rs:196`) is not isolated between concurrent `cargo test` runs: one run's cleanup deletes the other's just-written file and the reader asserts on an empty string. Cost three red runs in one session \u2014 a gate at TEST=101/225, and BOTH stop-hook failures that could not be reproduced afterwards, because by the time anyone re-ran it the second process was gone. A flaky test is worse than a failing one: it teaches people to re-run instead of read | process: a test that writes to disk gets a PER-PROCESS path (`std::process::id()`), never a fixed one; a red run with a second `cargo test` in flight is suspect before it is believed; experts: check idea \u2014 flag fixed paths under CARGO_TARGET_TMPDIR combined with remove_dir_all | FIXED 2026-08-21 in W8-04 (crates/retroforge/tests/** is inside its write_scope); per-process suffix, proven by two concurrent runs both passing |
| RF-L-11 | 08-21 | stale-docs | A comment that DESCRIBES SCOPE is a claim about code: it duplicates the truth, rots like any other duplicate, and nothing compiles it. Four in one session \u2014 `rf-profiles/shape.rs` beside the serde structs (a table missing there only WARNS), `wit/plugin.wit` read by nothing while a doc claimed drift was compiler-checked, `example-mode7` declaring a decoder family it never configured and loading cleanly for months, and `apu/dsp.rs` still describing a skeleton whose features had all landed. The last was worst because it PROPAGATED INTO THE PLAN: the stale doc told W7-08\u2019s notes the DSP was absent, those notes told the next executor, and it was restated into plan.json and STATUS.md \u2014 three layers of restatement, no compiler anywhere in the chain | process: when you add a second place that must agree with a first, write the test in the SAME commit; a scope claim in a doc comment needs an assertion or an expiry, not trust; READ THE CODE before repeating a doc\u2019s claim about it, especially one you are about to write into the board | fixed (all four corrected; drift tests added for the WIT world and the doc-site samples) |

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

## RF-L-10 — a fixed temp path is not test isolation (2026-08-21)

**What happened.** `crates/retroforge/tests/mode_invariant_corpus.rs:196`
builds its output directory as

```rust
Path::new(env!("CARGO_TARGET_TMPDIR"))
    .join("mode-invariant-evidence-test")
    .join(poking_entry.name.replace(' ', "_"));
let _ = std::fs::remove_dir_all(&out_dir); // start clean
```

That path is **fixed** — no PID, no randomness — and the test wipes it
before writing. Two overlapping `cargo test` runs share
`CARGO_TARGET_TMPDIR`, so run B's `remove_dir_all` deletes the
`replay-slice.rfreplay` run A just wrote, and A fails asserting on an
empty string.

**Why it took a session to see.** Overlap happened whenever the stop hook
fired its own `cargo test` while a gate run was in flight. By the time
anyone re-ran the suite the second process was gone, so it always passed
on inspection — twice reported as "could not reproduce". The third
occurrence landed inside a gate whose log survived, which is what made it
diagnosable: `TEST=101` after only 225 tests, the assertion message
showing an empty replay, the same test green 6/6 in isolation, and the
touched code (`rf-enhance`) nowhere near the failing test (`retroforge`,
replay dumps).

**Why this is worse than a plain failure.** A test that fails tells you
something. A test that fails only under concurrency teaches the opposite
lesson — that red runs are noise and the fix is to run it again. That
habit is how a real regression gets waved through, and it is the same
family as RF-L-06's "a validator whose failures evaporate is a rumor".

**FIXED 2026-08-21, in ticket W8-04** — `crates/retroforge/tests/**` turned
out to be inside that ticket's `crates/retroforge/**` scope. The directory
now carries a `std::process::id()` suffix, and the fix was **proven
against the real failure mode** rather than reasoned about: two
deliberately concurrent runs of the suite both pass 6/6, where that same
pairing previously produced the empty file. It surfaced a fourth time
while W8-04 was in flight — this time with both processes still visible
in `pgrep`, which is what made it demonstrable.

**The fix was**: give the path a per-process suffix.
`crates/rf-enhance/tests/intake_poisoned_bundle.rs` already does exactly
this (`std::process::id()` in the directory name) and is unaffected.
Not applied here because `crates/retroforge/tests/**` is outside W9-06's
write_scope.

**Standing rule.** A test that writes to disk takes a per-process path,
never a fixed one. And a red run with a second `cargo test` in flight is
**suspect before it is believed** — check for concurrency before
diagnosing the code.

## RF-L-11 — a scope comment is a claim about code (2026-08-21)

**Four instances in one session**, all the same shape: a second place
that must agree with a first, and nothing that checks it.

| Where | The claim | Reality |
|---|---|---|
| `rf-profiles/src/shape.rs` | key tree beside the serde structs | a table missing there deserialises fine and only *warns* |
| `rf-plugin-sdk/wit/plugin.wit` | doc said drift was caught by an exhaustive match | nothing read the file at all |
| `profiles/snes/example-mode7` | `kind = "room_grid"` | configured nothing, decoded nothing, loaded cleanly for months |
| `rf-snes/src/apu/dsp.rs` | "does NOT implement ... Gaussian, ADSR/GAIN, echo, pitch modulation or noise" | all five implemented, wired into `mix()`, 14 tests |

**The fourth is the instructive one, because it escaped the file.** The
stale doc told W7-08's notes the DSP was absent. Those notes told the next
executor. The next executor wrote it into `plan.json` and `STATUS.md` as
fact. Three layers of restatement and no compiler anywhere in the chain —
and the person repeating it had spent the same session writing drift tests
against exactly this pattern in other people's code.

**Why "just keep docs updated" is not the lesson.** Every one of these was
true when written. The defect is structural: a claim that lives only in
prose has no failure mode. `WIT_WORLD` is now `include_str!`'d and tested
(including an interface *count*, so adding one to the WIT and not the enum
fails); the doc site `{{#include}}`s files CI compiles rather than copying
them; `example-mode7`'s absent `[decode]` is asserted.

**Standing rules.**

1. Add a second place that must agree with a first → write the test in the
   **same commit**.
2. A scope claim in a doc comment needs an assertion or an expiry, not
   trust.
3. **Read the code before repeating a doc's claim about it** — especially
   one you are about to write into the board, where it becomes someone
   else's premise.

## RF-L-12 — the harness that reads the verdict is part of the evidence (2026-08-27)

**Two defects in two days, both in the code that decided whether
blargg's SPC test ROMs passed.** Neither was in the emulator. Both made
the results look better, or emptier, than they were.

| Defect | What it did | How it presented |
|---|---|---|
| `contains("passed")` | matched `Passed 01`, a **per-subtest** line with a running count | a nine-subtest ROM reported green after one subtest ran |
| screen window `$0B60`, 32x20 | straddled the end of the text map, cutting off everything above | `Failed 02` and its checksum were **never once logged**, on two ROMs |

**The first one reached the board.** I reported "spc_dsp6 passes", Brad
closed W7-08 on that report, and the closure had to be reversed. The
ticket's acceptance says *SPC timing suites pass*; what was actually
demonstrated was that one subtest of nine passed.

**The second hid real evidence.** For two ROMs the harness had been
printing a partial trace with no verdict, which read as "it stalls". They
were not stalling: both finish, park the 65816 in `BRA -2`, and print
`Failed 02` with a checksum — above the line the window happened to
catch. The whole "these ROMs hang" diagnosis, twice written into the
board, was an artefact of a mis-sized read.

**Why this is not RF-L-11.** That lesson is about *prose* claims with no
failure mode. This is worse in one specific way: these were **assertions
in test code**, which is exactly what you would point at to prove a claim
is checked. `cargo test` was green throughout. The harness was load-
bearing evidence and was itself never tested against a known input.

**Why it survived so long.** Both defects are invisible while everything
fails and while everything passes. They only mislead in the middle — the
regime this project spent a week in. A verdict reader gets written once,
early, against a ROM that is failing anyway, and is never revisited when
it starts mattering.

**Standing rules.**

1. **A verdict reader needs its own test.** Feed it a captured
   known-pass output and a known-fail output and assert it says so. The
   ROM is the oracle; the reader is code like any other.
2. **Match the terminal token, not a substring of it.** `PASSED TESTS` is
   the banner that means what the test claims; `Passed` is a progress
   line. If the format publishes both a per-item and a final verdict,
   only the final one may satisfy an assertion.
3. **Print what you read, in full, before trusting a window into it.**
   Both defects would have died in minutes against one raw dump of the
   whole text map.
4. **"It stalls" is a conclusion, not an observation.** Check where the
   CPUs actually are first — `BRA -2` at the end of a run is a completed
   program, not a hang.
