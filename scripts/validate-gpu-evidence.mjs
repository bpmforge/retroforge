#!/usr/bin/env node
// GPU-pass / local-AI benchmark evidence validator (ticket W16-01;
// docs/design/ENHANCEMENT_WAVE_16.md §8; docs/TESTING.md §4). Sibling to
// scripts/validate-evidence.mjs rather than an extension of it: that
// script's schema (accuracy_table rows, staleness against Tier-A-local
// suite source paths, nes6502/nestest/ppu_vbl_nmi anti-placeholder checks)
// is specific to the accuracy-gate evidence file and has nothing to do
// with this one's shape (timing rows: pass/size/p50/p95) or its producers
// (`cargo run --release -p rf-renderer --bin bench-passes`, plus rf-ai's
// `#[ignore]`d onnx/metalfx spikes) — bolting this onto validate-evidence.mjs
// would mean every accuracy-gate check function also has to reason about
// an unrelated timing schema. Wired the same way: mechanical, exit 1 on
// any violation, no network/GPU access needed to validate (it only reads
// the committed JSON).
//
// This file is NOT part of the Tier-A-local accuracy gate
// (docs/evidence/local-gate.json); it exists so
// docs/design/ENHANCEMENT_WAVE_16.md §8's frame-budget table can be
// checked against a real, non-placeholder measurement rather than trusted
// on prose alone.
import { readFileSync, existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const EVIDENCE_PATH = join(root, 'docs/evidence/gpu-passes.json');

const errors = [];
const err = (m) => errors.push(m);

if (!existsSync(EVIDENCE_PATH)) {
  console.error(`validate-gpu-evidence: FATAL — ${EVIDENCE_PATH} does not exist`);
  process.exit(1);
}

let doc;
try {
  doc = JSON.parse(readFileSync(EVIDENCE_PATH, 'utf8'));
} catch (e) {
  console.error(`validate-gpu-evidence: FATAL — ${EVIDENCE_PATH} is not valid JSON: ${e.message}`);
  process.exit(1);
}

if (typeof doc.machine !== 'string' || doc.machine.trim() === '') {
  err('doc.machine is missing or empty');
}
if (typeof doc.generated_at !== 'string' || !/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$/.test(doc.generated_at)) {
  err(`doc.generated_at is not an ISO-8601 UTC timestamp: ${JSON.stringify(doc.generated_at)}`);
}
if (!Array.isArray(doc.rows) || doc.rows.length === 0) {
  err('doc.rows is missing, not an array, or empty');
}

const SIZE_RE = /^\d+x\d+$/;
const seen = new Set();
for (const [i, row] of (doc.rows ?? []).entries()) {
  const where = `rows[${i}]`;
  if (typeof row.pass !== 'string' || row.pass.trim() === '') {
    err(`${where}.pass is missing or empty`);
  }
  if (typeof row.size !== 'string' || !SIZE_RE.test(row.size)) {
    err(`${where}.size is not a "WxH" string: ${JSON.stringify(row.size)}`);
  }
  if (!(typeof row.p50_ms === 'number' && row.p50_ms > 0)) {
    err(`${where}.p50_ms is ${JSON.stringify(row.p50_ms)} — must be a positive number, not a placeholder`);
  }
  if (!(typeof row.p95_ms === 'number' && row.p95_ms > 0)) {
    err(`${where}.p95_ms is ${JSON.stringify(row.p95_ms)} — must be a positive number, not a placeholder`);
  }
  if (typeof row.p50_ms === 'number' && typeof row.p95_ms === 'number' && row.p95_ms < row.p50_ms) {
    err(`${where}: p95_ms (${row.p95_ms}) < p50_ms (${row.p50_ms}) — a percentile ordering violation`);
  }
  if (!(typeof row.n === 'number' && row.n > 0)) {
    err(`${where}.n (sample count) is ${JSON.stringify(row.n)} — must be a positive number`);
  }
  if (typeof row.source !== 'string' || row.source.trim() === '') {
    err(`${where}.source is missing — must name which producer wrote this row (e.g. "gpu-bench", "onnx-ort", "metalfx")`);
  }
  const key = `${row.pass}/${row.size}/${row.source}`;
  if (seen.has(key)) {
    err(`duplicate row for pass=${row.pass} size=${row.size} source=${row.source} — the merge step should have overwritten, not duplicated`);
  }
  seen.add(key);
}

// Sanity: the two required GPU-bench sizes from W16-01's acceptance
// criteria must each have at least one shader-chain row and the neural
// stub row.
const REQUIRED_SIZES = ['256x240', '512x448'];
for (const size of REQUIRED_SIZES) {
  const rowsForSize = (doc.rows ?? []).filter((r) => r.size === size && r.source === 'gpu-bench');
  if (rowsForSize.length === 0) {
    err(`no gpu-bench rows for required size ${size} — run: cargo run --release -p rf-renderer --bin bench-passes`);
    continue;
  }
  if (!rowsForSize.some((r) => r.pass === 'neural-stub')) {
    err(`no neural-stub row for size ${size} — the stub compute pass must be timed alongside the shader chain`);
  }
}

if (errors.length) {
  console.error(`validate-gpu-evidence: ${errors.length} violation(s)`);
  for (const e of errors) console.error('  FAIL ' + e);
  process.exit(1);
}

console.log(
  `validate-gpu-evidence OK — ${doc.rows.length} row(s), machine=${doc.machine}, generated_at=${doc.generated_at}`
);
