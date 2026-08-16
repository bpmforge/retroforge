#!/usr/bin/env node
// Criterion regression gate (ticket W2-09; docs/TESTING.md §7 and §9,
// NFR-002/NFR-003, R-C2).
//
// Reads criterion's own `estimates.json` output, compares each benchmark
// against `benches/baseline.json`, and fails the run on a regression worse
// than the threshold TESTING.md §9 sets (">10% regression fails the run and
// blocks merge until re-baselined with justification in the PR").
//
// ## The half that matters: a baseline change must be justified (R-C2)
//
// A regression gate whose baseline anyone can quietly raise is not a gate.
// So this script also refuses a baseline whose NUMBERS moved without its
// `meta` moving with them — checked against the committed version via
// `git show HEAD:benches/baseline.json`, not against a copy this script
// keeps. Re-baselining is legitimate and expected (a faster machine, a real
// optimisation, a deliberate accuracy-for-speed trade); doing it silently
// is what this refuses.
//
// ## Why point estimates, not criterion's own change detection
//
// Criterion compares against whatever it last saw in `target/criterion`,
// which on a fresh CI runner is nothing at all — its "Performance has
// regressed" line is meaningless there. The committed baseline is the only
// stable reference across machines, so this reads the point estimate and
// does its own arithmetic.
import { readFileSync, existsSync, readdirSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');

/** TESTING.md §9's threshold. */
export const REGRESSION_THRESHOLD = 0.10;

/** Fields R-C2 requires on any baseline. */
export const REQUIRED_META = ['rebaselined_by', 'justification', 'pr'];

/**
 * Read every criterion benchmark's point estimate, in nanoseconds.
 * Returns `{ name: ns }`.
 */
export function readCriterion(criterionDir) {
  const results = {};
  if (!existsSync(criterionDir)) return results;
  for (const entry of readdirSync(criterionDir, { withFileTypes: true })) {
    if (!entry.isDirectory() || entry.name === 'report') continue;
    const estimates = join(criterionDir, entry.name, 'new', 'estimates.json');
    if (!existsSync(estimates)) continue;
    const parsed = JSON.parse(readFileSync(estimates, 'utf8'));
    const ns = parsed?.mean?.point_estimate;
    if (typeof ns === 'number') results[entry.name] = ns;
  }
  return results;
}

/**
 * Compare measurements against a baseline.
 *
 * Returns `{ regressions, improvements, missing, budgetFailures, compared }`.
 * A benchmark present in the run but absent from the baseline is NOT an
 * error — a new benchmark has nothing to regress against yet, and failing
 * on it would make adding one require a baseline change first. A benchmark
 * in the baseline but absent from the run IS reported: a suite that
 * silently stopped running looks exactly like one that passed.
 */
export function compare(measured, baseline, threshold = REGRESSION_THRESHOLD) {
  const regressions = [];
  const improvements = [];
  const missing = [];
  const budgetFailures = [];
  let compared = 0;

  for (const [name, entry] of Object.entries(baseline.benchmarks ?? {})) {
    const now = measured[name];
    if (typeof now !== 'number') {
      missing.push(name);
      continue;
    }
    compared += 1;
    const base = entry.ns;
    const ratio = (now - base) / base;
    if (ratio > threshold) {
      regressions.push({ name, base, now, ratio });
    } else if (ratio < -threshold) {
      improvements.push({ name, base, now, ratio });
    }
    // NFR-002's budgets are absolute, not relative: a benchmark can be
    // within 10% of a baseline that is itself over budget.
    if (typeof entry.budget_ns === 'number' && now > entry.budget_ns) {
      budgetFailures.push({ name, now, budget: entry.budget_ns });
    }
  }

  return { regressions, improvements, missing, budgetFailures, compared };
}

/** R-C2: every baseline must carry its provenance. */
export function checkMeta(baseline) {
  const problems = [];
  const meta = baseline.meta ?? {};
  for (const field of REQUIRED_META) {
    const value = meta[field];
    if (typeof value !== 'string' || value.trim() === '') {
      problems.push(`meta.${field} is missing or empty`);
      continue;
    }
    if (/^(tbd|todo|n\/a|xxx|\?+)$/i.test(value.trim())) {
      problems.push(`meta.${field} is a placeholder (${JSON.stringify(value)})`);
    }
  }
  return problems;
}

/**
 * R-C2's teeth: numbers may not move unless `meta` moved too.
 *
 * `previous` is the committed baseline (or `null` when there isn't one —
 * the first commit of a baseline is not a "change").
 */
export function checkRebaselineJustified(previous, current) {
  if (!previous) return [];
  const numbersChanged =
    JSON.stringify(previous.benchmarks ?? {}) !== JSON.stringify(current.benchmarks ?? {});
  if (!numbersChanged) return [];
  const metaChanged = JSON.stringify(previous.meta ?? {}) !== JSON.stringify(current.meta ?? {});
  if (metaChanged) return [];
  return [
    'baseline numbers changed but meta did not: a re-baseline must record who did it, ' +
      'why, and in which PR (R-C2, docs/TESTING.md §9)',
  ];
}

/** The committed baseline, or `null` if this is its first commit. */
function committedBaseline(path) {
  try {
    const text = execFileSync('git', ['show', `HEAD:${path}`], {
      cwd: root,
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
    });
    return JSON.parse(text);
  } catch {
    return null;
  }
}

function main(argv) {
  const args = new Map();
  for (let i = 0; i < argv.length; i += 2) args.set(argv[i], argv[i + 1]);
  const criterionDir = join(root, args.get('--criterion-dir') ?? 'target/criterion');
  const baselineRel = args.get('--baseline') ?? 'benches/baseline.json';
  const baselinePath = join(root, baselineRel);

  if (!existsSync(baselinePath)) {
    console.error(`bench-compare: FATAL — no baseline at ${baselineRel}`);
    process.exit(1);
  }
  const baseline = JSON.parse(readFileSync(baselinePath, 'utf8'));

  const problems = [
    ...checkMeta(baseline),
    ...checkRebaselineJustified(committedBaseline(baselineRel), baseline),
  ];
  if (problems.length > 0) {
    for (const problem of problems) console.error(`bench-compare: ${problem}`);
    process.exit(1);
  }

  const measured = readCriterion(criterionDir);
  if (Object.keys(measured).length === 0) {
    console.error(
      `bench-compare: FATAL — no criterion results under ${criterionDir}. ` +
        'Run `cargo bench --workspace` first; an empty result set must never read as a pass.',
    );
    process.exit(1);
  }

  const result = compare(measured, baseline);

  for (const item of result.improvements) {
    console.log(
      `bench-compare: ${item.name} improved ${(item.ratio * -100).toFixed(1)}% ` +
        `(${(item.base / 1000).toFixed(2)}µs → ${(item.now / 1000).toFixed(2)}µs) — ` +
        're-baseline deliberately if this is real',
    );
  }
  for (const name of result.missing) {
    console.error(`bench-compare: baseline benchmark ${name} did not run`);
  }
  for (const item of result.regressions) {
    console.error(
      `bench-compare: REGRESSION ${item.name} +${(item.ratio * 100).toFixed(1)}% ` +
        `(${(item.base / 1000).toFixed(2)}µs → ${(item.now / 1000).toFixed(2)}µs, ` +
        `threshold ${(REGRESSION_THRESHOLD * 100).toFixed(0)}%)`,
    );
  }
  for (const item of result.budgetFailures) {
    console.error(
      `bench-compare: OVER BUDGET ${item.name} ${(item.now / 1e6).toFixed(3)}ms > ` +
        `${(item.budget / 1e6).toFixed(3)}ms (NFR-002)`,
    );
  }

  const failed =
    result.regressions.length > 0 ||
    result.missing.length > 0 ||
    result.budgetFailures.length > 0;
  if (failed) {
    process.exit(1);
  }
  console.log(
    `bench-compare OK — ${result.compared} benchmark(s) within ` +
      `${(REGRESSION_THRESHOLD * 100).toFixed(0)}% of baseline` +
      (result.improvements.length > 0 ? `, ${result.improvements.length} improved` : ''),
  );
}

// Only run when invoked directly, so the exported helpers can be unit-tested.
if (process.argv[1] && process.argv[1].endsWith('bench-compare.mjs')) {
  main(process.argv.slice(2));
}
