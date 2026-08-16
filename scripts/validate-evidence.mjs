#!/usr/bin/env node
// Local-evidence-gate validator (tickets W0-07, W1-03). Reads the
// committed docs/evidence/local-gate.json (a few KB — this is the "cheap"
// half of the gate; the expensive half, scripts/local-gate.sh, runs the
// heavy nes6502 vector suite plus the nestest golden-trace diff locally
// and is never invoked in CI, see docs/TESTING.md §4) and mechanically
// checks that it is (a) present for every Tier-A-local suite, (b) not
// stale relative to the code it covers, and (c) was generated against a
// clean working tree. Exit 1 on any violation, matching
// validate-plan.mjs/validate-traceability.mjs style.
//
// CRITICAL TRAP this script exists to close (plan.json W0-07 pre-flight
// notes): `actions/checkout@v4` defaults to `fetch-depth: 1`. On a
// shallow clone, `git rev-list`/`git merge-base --is-ancestor` silently
// degrade rather than erroring, so a staleness check built on them would
// PASS in CI while enforcing nothing — this project's own "the skip path
// is also the way to fake success" failure mode (RF-L-08). This script
// therefore checks shallowness itself and fails loudly BEFORE running any
// staleness command, and .github/workflows/ci.yml sets `fetch-depth: 0`.
import { readFileSync, existsSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const EVIDENCE_PATH = join(root, 'docs/evidence/local-gate.json');
const MANIFEST_PATH = join(root, 'tests/rom-manifest.toml');

// Suite -> covered-code-paths DATA table (mirrors accuracy.rs's own rule
// for suite->FR->tier: "a lookup from the tables above, never a judgment
// call"). Deliberately does NOT include crates/rf-harness/** — that would
// make the instrument part of what it measures, so every touch to the
// harness itself (including this file) would re-trigger staleness against
// evidence whose *parent* commit predates that touch, forcing a
// regeneration loop that has nothing to do with CPU correctness.
const TIER_A_LOCAL_SUITES = {
  nes6502: {
    coveredPaths: ['crates/rf-nes/src/cpu'],
  },
  // nestest (ticket W1-03): the golden-trace diff needs the CPU (opcode
  // execution + the reset/power-on sequence lives in cpu/mod.rs), the
  // system bus (peek + memory map), and the trace-logger module itself —
  // deliberately NOT crates/rf-harness/** for the same reason nes6502
  // excludes it (see the comment above the constant).
  nestest: {
    coveredPaths: [
      'crates/rf-nes/src/cpu',
      'crates/rf-nes/src/system',
      'crates/rf-nes/src/trace.rs',
    ],
  },
  // ppu_vbl_nmi (ticket W1-05b, third Tier-A-local suite): VBlank/NMI
  // wiring lives in ppu/** and the CpuBus::nmi_line passthrough in
  // system/mod.rs; crates/rf-nes/src/cpu is ALSO covered here (not for
  // sprite_hit_tests below) because the NMI edge-detector this suite
  // exercises heavily (cpu::exec::CountingBus) lives there — a change to
  // it is directly load-bearing for this suite's real pass/fail outcome,
  // unlike sprite_hit_tests, which never triggers an NMI. Deliberately NOT
  // crates/rf-harness/** (same reason as nes6502/nestest above).
  ppu_vbl_nmi: {
    coveredPaths: ['crates/rf-nes/src/ppu', 'crates/rf-nes/src/system', 'crates/rf-nes/src/cpu'],
  },
  // sprite_hit_tests (ticket W1-05b, fourth Tier-A-local suite):
  // sprite-0-hit lives in ppu/** (sprites.rs's output_pixel); system/mod.rs
  // for the bus plumbing (prg_ram/frame_count accessors) rf-harness's
  // RAM-result reader depends on.
  sprite_hit_tests: {
    coveredPaths: ['crates/rf-nes/src/ppu', 'crates/rf-nes/src/system'],
  },
  // apu_test (ticket W2-01a, fifth Tier-A-local suite): the whole APU lives
  // in apu/**, and system/mod.rs carries the register decode ($4000-$4017),
  // the per-CPU-cycle Apu::tick call and the IRQ wired-OR the suite's
  // frame-counter and DMC interrupt sub-tests exercise. Deliberately NOT
  // crates/rf-nes/src/cpu: the CPU's IRQ handling is exercised, but this
  // suite's pass/fail turns on the APU's flags, and cpu/** already carries
  // its own two Tier-A-local suites.
  apu_test: {
    coveredPaths: ['crates/rf-nes/src/apu', 'crates/rf-nes/src/system'],
  },
};

// Suite -> expected [[suite.roms]] row count DATA table (ticket W1-05b) —
// the multi-row analog of nes6502's `opcodes_tested !== 256` / nestest's
// `lines_compared !== 8991` anti-placeholder guards below: a manifest edit
// that silently shrinks one of these suites must fail loudly here, not
// quietly under-report coverage.
const EXPECTED_ROM_COUNTS = {
  ppu_vbl_nmi: 10,
  sprite_hit_tests: 11,
  // One combined ROM that runs all eight sub-tests and reports the first
  // failure's number (blargg's apu_test/readme.txt); the per-sub-test
  // `rom_singles` are not in the manifest.
  apu_test: 1,
};

const errors = [];
const err = (m) => errors.push(m);

const git = (args) => execFileSync('git', args, { cwd: root, encoding: 'utf8' }).trim();

// ---- 0. shallow-clone guard: must run before ANY staleness command ----
let isShallow;
try {
  isShallow = git(['rev-parse', '--is-shallow-repository']) === 'true';
} catch (e) {
  console.error(`validate-evidence: FATAL — could not determine clone depth: ${e.message}`);
  process.exit(1);
}
if (isShallow) {
  console.error(
    'validate-evidence: FATAL — this is a shallow git clone (git rev-parse --is-shallow-repository = true). ' +
      'Staleness checks (git rev-list / git merge-base --is-ancestor) silently degrade on shallow history ' +
      'instead of erroring, which would make this validator pass while enforcing nothing. ' +
      'Fix: checkout with fetch-depth: 0 (see .github/workflows/ci.yml).'
  );
  process.exit(1);
}

// ---- 1. evidence file must exist ----
if (!existsSync(EVIDENCE_PATH)) {
  console.error(
    `validate-evidence: FATAL — ${EVIDENCE_PATH} does not exist; no evidence for Tier-A-local suite(s): ${Object.keys(TIER_A_LOCAL_SUITES).join(', ')}`
  );
  process.exit(1);
}

let evidence;
try {
  evidence = JSON.parse(readFileSync(EVIDENCE_PATH, 'utf8'));
} catch (e) {
  console.error(`validate-evidence: FATAL — ${EVIDENCE_PATH} is not valid JSON: ${e.message}`);
  process.exit(1);
}

const HEX40 = /^[0-9a-f]{40}$/;

// ---- 2. dirty-tree-at-generation-time is a hard fail ----
if (evidence.tree_clean !== true) {
  err(
    `evidence.tree_clean is ${JSON.stringify(evidence.tree_clean)}, not true — the recorded ` +
      'retroforge_commit does not truthfully describe what was tested (working tree was dirty ' +
      'at generation time)'
  );
}

// ---- 3. retroforge_commit must be a real, well-formed commit ----
if (typeof evidence.retroforge_commit !== 'string' || !HEX40.test(evidence.retroforge_commit)) {
  err(`evidence.retroforge_commit is not 40 lowercase hex chars: ${JSON.stringify(evidence.retroforge_commit)}`);
}

// ---- 4. toolchain sanity ----
if (typeof evidence.toolchain !== 'string' || evidence.toolchain.trim() === '') {
  err('evidence.toolchain is missing or empty');
}

// ---- 5. per-Tier-A-local-suite: evidence present, staleness ----
// `.filter()`, not `.find()`: nes6502/nestest each have exactly one
// aggregate row, but ppu_vbl_nmi/sprite_hit_tests (ticket W1-05b) have one
// row PER named sub-ROM (10 and 11 respectively) — `.find()` would only
// ever check the first of those, silently ignoring the other 9/10.
const rows = Array.isArray(evidence.accuracy_table?.rows) ? evidence.accuracy_table.rows : [];
for (const [suiteId, { coveredPaths }] of Object.entries(TIER_A_LOCAL_SUITES)) {
  const suiteRows = rows.filter((r) => r.suite === suiteId);
  if (suiteRows.length === 0) {
    err(`Tier-A-local suite '${suiteId}' has no evidence row(s) in ${EVIDENCE_PATH}`);
    continue;
  }
  const expectedCount = EXPECTED_ROM_COUNTS[suiteId];
  if (expectedCount !== undefined && suiteRows.length !== expectedCount) {
    err(
      `Tier-A-local suite '${suiteId}' has ${suiteRows.length} evidence row(s), expected ${expectedCount} — ` +
        'a partial/edited manifest or evidence run would silently under-report coverage'
    );
  }
  for (const row of suiteRows) {
    if (row.status !== 'pass' && row.status !== 'waived') {
      err(
        `Tier-A-local suite '${suiteId}' rom ${JSON.stringify(row.rom)} evidence row has status ` +
          `${JSON.stringify(row.status)}, expected pass or waived`
      );
    }
  }

  if (!Array.isArray(coveredPaths) || coveredPaths.length === 0) {
    err(`TIER_A_LOCAL_SUITES['${suiteId}'].coveredPaths is empty — cannot check staleness`);
    continue;
  }

  let lastTouch;
  try {
    lastTouch = git(['rev-list', '-1', 'HEAD', '--', ...coveredPaths]);
  } catch (e) {
    err(`suite '${suiteId}': git rev-list failed for ${coveredPaths.join(', ')}: ${e.message}`);
    continue;
  }
  if (lastTouch === '') {
    // No commit in history touches these paths at all — a renamed/typo'd
    // path would silently disable the entire staleness check if this were
    // treated as "no constraint" instead of a failure.
    err(
      `suite '${suiteId}': git rev-list -1 HEAD -- ${coveredPaths.join(', ')} found no commit at all — ` +
        'coveredPaths is almost certainly wrong (renamed/typo\'d), not "never touched"'
    );
    continue;
  }

  let isAncestor = true;
  try {
    execFileSync('git', ['merge-base', '--is-ancestor', lastTouch, evidence.retroforge_commit], {
      cwd: root,
    });
  } catch {
    isAncestor = false;
  }
  if (!isAncestor) {
    err(
      `suite '${suiteId}' evidence is STALE: last commit touching ${coveredPaths.join(', ')} ` +
        `(${lastTouch}) is not an ancestor of the evidence's retroforge_commit ` +
        `(${evidence.retroforge_commit}) — regenerate via scripts/local-gate.sh`
    );
  }
}

// ---- 6. nes6502-specific consistency (anti-placeholder) ----
const v = evidence.vectors;
if (!v || typeof v !== 'object') {
  err('evidence.vectors is missing');
} else {
  if (v.suite !== 'nes6502') err(`evidence.vectors.suite is ${JSON.stringify(v.suite)}, expected "nes6502"`);
  if (typeof v.source_commit !== 'string' || !HEX40.test(v.source_commit)) {
    err(`evidence.vectors.source_commit is not 40 lowercase hex chars: ${JSON.stringify(v.source_commit)}`);
  }
  if (v.opcodes_tested !== 256) {
    err(`evidence.vectors.opcodes_tested is ${JSON.stringify(v.opcodes_tested)}, expected 256`);
  }
  if (v.total_fail !== 0) {
    err(`evidence.vectors.total_fail is ${JSON.stringify(v.total_fail)}, expected 0`);
  }
  if (!(typeof v.total_pass === 'number' && v.total_pass > 0)) {
    err(`evidence.vectors.total_pass is ${JSON.stringify(v.total_pass)} — looks like a placeholder, not a real run`);
  }

  // Cross-check against the manifest's own pinned commit for the
  // singlestep-nes6502-src git_artifact, so a manifest bump that forgot to
  // regenerate evidence is caught mechanically rather than trusted.
  if (existsSync(MANIFEST_PATH)) {
    const manifestText = readFileSync(MANIFEST_PATH, 'utf8');
    const m = manifestText.match(
      /\[\[git_artifact\]\][^[]*?id\s*=\s*"singlestep-nes6502-src"[^[]*?commit\s*=\s*"([0-9a-f]{40})"/
    );
    if (!m) {
      err('could not find singlestep-nes6502-src git_artifact commit in tests/rom-manifest.toml to cross-check');
    } else if (v.source_commit !== m[1]) {
      err(
        `evidence.vectors.source_commit (${v.source_commit}) does not match tests/rom-manifest.toml's ` +
          `pinned commit for singlestep-nes6502-src (${m[1]}) — regenerate via scripts/local-gate.sh`
      );
    }
  }
}

// ---- 6b. nestest-specific consistency (anti-placeholder, ticket W1-03) ----
const nt = evidence.nestest;
if (!nt || typeof nt !== 'object') {
  err('evidence.nestest is missing');
} else {
  if (nt.lines_compared !== 8991) {
    err(`evidence.nestest.lines_compared is ${JSON.stringify(nt.lines_compared)}, expected 8991`);
  }
  if (!(typeof nt.lines_matched === 'number' && nt.lines_matched > 0)) {
    err(`evidence.nestest.lines_matched is ${JSON.stringify(nt.lines_matched)} — looks like a placeholder, not a real run`);
  }
  if (nt.lines_matched !== nt.lines_compared) {
    err(
      `evidence.nestest.lines_matched (${nt.lines_matched}) !== lines_compared (${nt.lines_compared}) — ` +
        `nestest is not byte-exact yet`
    );
  }
  if (nt.first_divergence !== null) {
    err(`evidence.nestest.first_divergence is ${JSON.stringify(nt.first_divergence)}, expected null (no divergence)`);
  }
}

// ---- 6c/6d. ppu_vbl_nmi / sprite_hit_tests summary consistency
// (anti-placeholder, ticket W1-05b) — same shape check as vectors/nestest
// above, applied to both new Tier-A-local suites' top-level summary
// objects (`{suite, roms_tested, roms_passed, roms_failed, failing[]}`,
// see `crates/rf-harness/src/bin/local_gate_evidence.rs`'s
// `suite_summary_json`).
for (const suiteId of ['ppu_vbl_nmi', 'sprite_hit_tests']) {
  const s = evidence[suiteId];
  const expectedCount = EXPECTED_ROM_COUNTS[suiteId];
  if (!s || typeof s !== 'object') {
    err(`evidence.${suiteId} is missing`);
    continue;
  }
  if (s.roms_tested !== expectedCount) {
    err(`evidence.${suiteId}.roms_tested is ${JSON.stringify(s.roms_tested)}, expected ${expectedCount}`);
  }
  if (!(typeof s.roms_passed === 'number' && s.roms_passed >= 0)) {
    err(`evidence.${suiteId}.roms_passed is ${JSON.stringify(s.roms_passed)} — looks like a placeholder, not a real run`);
  }
  if (!(typeof s.roms_failed === 'number' && s.roms_failed >= 0)) {
    err(`evidence.${suiteId}.roms_failed is ${JSON.stringify(s.roms_failed)} — looks like a placeholder, not a real run`);
  }
  if (
    typeof s.roms_tested === 'number' &&
    typeof s.roms_passed === 'number' &&
    typeof s.roms_failed === 'number' &&
    s.roms_passed + s.roms_failed !== s.roms_tested
  ) {
    err(
      `evidence.${suiteId}: roms_passed (${s.roms_passed}) + roms_failed (${s.roms_failed}) != ` +
        `roms_tested (${s.roms_tested})`
    );
  }
  if (!Array.isArray(s.failing) || s.failing.length !== s.roms_failed) {
    err(
      `evidence.${suiteId}.failing has ${Array.isArray(s.failing) ? s.failing.length : 'non-array'} ` +
        `entries, expected ${s.roms_failed} (roms_failed) — a failure must be named, not only counted`
    );
  }
}

// ---- 7. accuracy_table shape sanity (reuses Report::to_json's own keys) ----
if (!evidence.accuracy_table || !Array.isArray(evidence.accuracy_table.rows)) {
  err('evidence.accuracy_table.rows is missing or not an array');
}
if (!evidence.accuracy_table?.raw || !evidence.accuracy_table?.effective) {
  err('evidence.accuracy_table.raw/effective counts are missing');
}
for (const r of rows) {
  if (r.status === 'fail') {
    err(`evidence row ${r.suite}/${r.rom} has status "fail" — this should be structurally impossible ` +
      '(build_report refuses to emit an un-waived red row) unless the file was hand-edited');
  }
}

if (errors.length) {
  console.error(`validate-evidence: ${errors.length} violation(s)`);
  for (const e of errors) console.error('  FAIL ' + e);
  process.exit(1);
}

console.log(
  `validate-evidence OK — ${Object.keys(TIER_A_LOCAL_SUITES).length} Tier-A-local suite(s) covered, ` +
    `retroforge_commit=${evidence.retroforge_commit}, tree_clean=${evidence.tree_clean}, ` +
    `nes6502: ${evidence.vectors.total_pass}/${evidence.vectors.total_pass + evidence.vectors.total_fail} cases, ` +
    `nestest: ${evidence.nestest.lines_matched}/${evidence.nestest.lines_compared} lines, ` +
    `ppu_vbl_nmi: ${evidence.ppu_vbl_nmi.roms_passed}/${evidence.ppu_vbl_nmi.roms_tested} ROMs, ` +
    `sprite_hit_tests: ${evidence.sprite_hit_tests.roms_passed}/${evidence.sprite_hit_tests.roms_tested} ROMs`
);
