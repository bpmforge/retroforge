#!/usr/bin/env node
// Tests for the criterion regression gate (ticket W2-09).
//
// A threshold script is exactly the kind of thing that silently stops
// enforcing anything — it passes either way, and nobody notices until a
// regression ships. So every rule it claims to enforce is tested here, on
// both sides: the case it must reject AND the case it must accept.
//
// Run: `node scripts/bench-compare.test.mjs` (the nightly workflow runs it
// before it runs the gate itself, so a broken gate fails loudly rather than
// passing vacuously).
import assert from 'node:assert/strict';
import {
  compare,
  checkMeta,
  checkRebaselineJustified,
  REGRESSION_THRESHOLD,
} from './bench-compare.mjs';

let failures = 0;
function test(name, fn) {
  try {
    fn();
    console.log(`ok   ${name}`);
  } catch (e) {
    failures += 1;
    console.error(`FAIL ${name}\n     ${e.message}`);
  }
}

const baseline = (benchmarks, meta = {}) => ({
  meta: {
    rebaselined_by: 'someone',
    justification: 'because',
    pr: '#1',
    ...meta,
  },
  benchmarks,
});

test('a benchmark within the threshold passes', () => {
  const result = compare({ a: 109 }, baseline({ a: { ns: 100 } }));
  assert.equal(result.regressions.length, 0);
  assert.equal(result.compared, 1);
});

test('a benchmark just past the threshold is a regression', () => {
  const result = compare({ a: 111 }, baseline({ a: { ns: 100 } }));
  assert.equal(result.regressions.length, 1);
  assert.equal(result.regressions[0].name, 'a');
  assert.ok(result.regressions[0].ratio > REGRESSION_THRESHOLD);
});

test('the threshold boundary itself is not a regression', () => {
  // Exactly +10% must pass: TESTING.md §9 says ">10% regression fails".
  const result = compare({ a: 110 }, baseline({ a: { ns: 100 } }));
  assert.equal(result.regressions.length, 0);
});

test('a large improvement is reported but does not fail', () => {
  const result = compare({ a: 50 }, baseline({ a: { ns: 100 } }));
  assert.equal(result.regressions.length, 0);
  assert.equal(result.improvements.length, 1);
});

test('a baseline benchmark that did not run is reported, never ignored', () => {
  // The failure this catches: a suite that silently stopped running looks
  // exactly like one that passed.
  const result = compare({}, baseline({ a: { ns: 100 } }));
  assert.deepEqual(result.missing, ['a']);
  assert.equal(result.compared, 0);
});

test('a new benchmark absent from the baseline is not an error', () => {
  const result = compare({ a: 100, brand_new: 999 }, baseline({ a: { ns: 100 } }));
  assert.equal(result.regressions.length, 0);
  assert.equal(result.missing.length, 0);
});

test('an absolute budget fails even when the relative change is fine', () => {
  // NFR-002's budgets are absolute: a benchmark can sit within 10% of a
  // baseline that is itself over budget.
  const result = compare(
    { frame: 2_100_000 },
    baseline({ frame: { ns: 2_000_000, budget_ns: 2_000_000 } }),
  );
  assert.equal(result.regressions.length, 0, 'only +5%, not a regression');
  assert.equal(result.budgetFailures.length, 1);
});

test('meta must be present and complete (R-C2)', () => {
  assert.deepEqual(checkMeta({ meta: { rebaselined_by: 'a', justification: 'b', pr: 'c' } }), []);
  assert.equal(checkMeta({}).length, 3, 'all three fields missing');
  assert.equal(
    checkMeta({ meta: { rebaselined_by: 'a', justification: '', pr: 'c' } }).length,
    1,
  );
});

test('placeholder meta is refused (R-C2)', () => {
  const problems = checkMeta({
    meta: { rebaselined_by: 'a', justification: 'TBD', pr: '???' },
  });
  assert.equal(problems.length, 2, `expected two placeholder problems, got ${problems}`);
});

test('changing the numbers without changing meta is refused (R-C2)', () => {
  const before = baseline({ a: { ns: 100 } });
  const after = baseline({ a: { ns: 500 } });
  const problems = checkRebaselineJustified(before, after);
  assert.equal(problems.length, 1, 'a silent re-baseline must be refused');
  assert.match(problems[0], /meta did not/);
});

test('changing the numbers WITH new meta is accepted (R-C2)', () => {
  const before = baseline({ a: { ns: 100 } });
  const after = baseline({ a: { ns: 500 } }, { justification: 'moved to a slower runner' });
  assert.deepEqual(checkRebaselineJustified(before, after), []);
});

test('a baseline whose numbers did not move needs no new meta', () => {
  const before = baseline({ a: { ns: 100 } });
  const after = baseline({ a: { ns: 100 } });
  assert.deepEqual(checkRebaselineJustified(before, after), []);
});

test('the first commit of a baseline is not a re-baseline', () => {
  assert.deepEqual(checkRebaselineJustified(null, baseline({ a: { ns: 100 } })), []);
});

if (failures > 0) {
  console.error(`\nbench-compare.test: ${failures} failure(s)`);
  process.exit(1);
}
console.log('\nbench-compare.test OK');
