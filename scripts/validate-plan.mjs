#!/usr/bin/env node
// plan.json schema/graph validator (design-review arc P4, 2026-07-15).
// Adapted from ~/Code/shipwright/scripts/validate-plan.mjs (which encodes
// the D-015 territory-release + scaffold laws). Exit 1 on any violation.
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const plan = JSON.parse(readFileSync(join(root, 'plan.json'), 'utf8'));

const CRATES = [
  'rf-core-api', 'rf-nes', 'rf-snes', 'rf-cart', 'rf-renderer', 'rf-audio',
  'rf-input', 'rf-state', 'rf-cache', 'rf-enhance', 'rf-profiles',
  'rf-plugin-sdk', 'rf-ai', 'rf-debugger', 'rf-harness', 'retroforge',
  'infra', 'tools', 'profiles', 'fixtures',
];
const STATUSES = ['todo', 'in_progress', 'done', 'blocked'];
const POINTS = [1, 2, 3, 5, 8];
const TICKET_KEYS = ['id', 'title', 'phase', 'crate', 'write_scope', 'depends_on', 'acceptance', 'points', 'status', 'stories'];
const OPTIONAL_KEYS = ['notes', 'scaffold', 'hold'];
const ID_RE = /^W\d+-\d{2}[a-z]?$/;
const STORY_RE = /^E\d+-S\d+$/;

const errors = [];
const err = (m) => errors.push(m);

// P1 — board shape
if (plan.version !== 1) err('P1: plan.version must be 1');
if (!Array.isArray(plan.tickets) || plan.tickets.length === 0) {
  console.error('P1 FATAL: plan.tickets missing/empty');
  process.exit(1);
}
const T = plan.tickets;
const ids = new Set();

for (const t of T) {
  const tag = t.id ?? '<no id>';
  // P2 — key set
  for (const k of TICKET_KEYS) if (!(k in t)) err(`P2 ${tag}: missing key '${k}'`);
  for (const k of Object.keys(t)) if (!TICKET_KEYS.includes(k) && !OPTIONAL_KEYS.includes(k)) err(`P2 ${tag}: unknown key '${k}'`);
  // P3 — id/phase
  if (!ID_RE.test(t.id ?? '')) err(`P3 ${tag}: bad id`);
  if (ids.has(t.id)) err(`P3 ${tag}: duplicate id`);
  ids.add(t.id);
  const wave = Number((t.id ?? '').match(/^W(\d+)-/)?.[1]);
  if (wave !== t.phase) err(`P3 ${tag}: phase ${t.phase} != wave prefix ${wave}`);
  // P4 — vocabularies
  if (!t.title) err(`P4 ${tag}: empty title`);
  if (!CRATES.includes(t.crate)) err(`P4 ${tag}: crate '${t.crate}' not in vocabulary`);
  if (!STATUSES.includes(t.status)) err(`P4 ${tag}: status '${t.status}'`);
  if (!POINTS.includes(t.points)) err(`P4 ${tag}: points ${t.points} not in {1,2,3,5,8}`);
  // P5 — scopes + acceptance
  if (!Array.isArray(t.write_scope) || t.write_scope.length === 0 || t.write_scope.some((g) => typeof g !== 'string' || !g)) err(`P5 ${tag}: write_scope must be non-empty strings`);
  if (!Array.isArray(t.acceptance) || t.acceptance.length < 1 || t.acceptance.length > 8 || t.acceptance.some((a) => typeof a !== 'string' || !a)) err(`P5 ${tag}: acceptance must be 1-8 non-empty strings`);
  // P6 — stories
  if (!Array.isArray(t.stories) || t.stories.some((s) => !STORY_RE.test(s))) err(`P6 ${tag}: stories must be E<n>-S<n> ids (empty array OK for infra)`);
  if ('notes' in t && !Array.isArray(t.notes)) err(`P2 ${tag}: notes must be an array of strings`);
}

// P7 — dependency graph
const byId = new Map(T.map((t) => [t.id, t]));
for (const t of T) {
  for (const d of t.depends_on) {
    if (!byId.has(d)) err(`P7 ${t.id}: dangling dep ${d}`);
    if (d === t.id) err(`P7 ${t.id}: self-dependency`);
    const dw = Number(d.match(/^W(\d+)-/)?.[1]);
    if (dw > t.phase) err(`P7 ${t.id}: forward-wave dep ${d}`);
  }
}
const color = new Map();
const cyc = (id) => {
  if (color.get(id) === 2) return false;
  if (color.get(id) === 1) return true;
  color.set(id, 1);
  for (const d of byId.get(id)?.depends_on ?? []) if (cyc(d)) return true;
  color.set(id, 2);
  return false;
};
for (const t of T) if (cyc(t.id)) err(`P7 ${t.id}: dependency cycle`);

// glob-overlap engine (same dialect as shipwright's validator)
const segTextOverlaps = (a, b) => {
  const dp = Array.from({ length: a.length + 1 }, () => new Array(b.length + 1).fill(false));
  dp[0][0] = true;
  for (let i = 1; i <= a.length; i++) dp[i][0] = dp[i - 1][0] && a[i - 1] === '*';
  for (let j = 1; j <= b.length; j++) dp[0][j] = dp[0][j - 1] && b[j - 1] === '*';
  for (let i = 1; i <= a.length; i++)
    for (let j = 1; j <= b.length; j++) {
      const ca = a[i - 1], cb = b[j - 1];
      if (ca === '*') dp[i][j] = dp[i - 1][j] || dp[i][j - 1] || dp[i - 1][j - 1];
      else if (cb === '*') dp[i][j] = dp[i][j - 1] || dp[i - 1][j] || dp[i - 1][j - 1];
      else dp[i][j] = dp[i - 1][j - 1] && (ca === cb || ca === '?' || cb === '?');
    }
  return dp[a.length][b.length];
};
const segListOverlaps = (as, bs) => {
  const dp = Array.from({ length: as.length + 1 }, () => new Array(bs.length + 1).fill(false));
  dp[0][0] = true;
  for (let i = 1; i <= as.length; i++) dp[i][0] = dp[i - 1][0] && as[i - 1] === '**';
  for (let j = 1; j <= bs.length; j++) dp[0][j] = dp[0][j - 1] && bs[j - 1] === '**';
  for (let i = 1; i <= as.length; i++)
    for (let j = 1; j <= bs.length; j++) {
      const sa = as[i - 1], sb = bs[j - 1];
      if (sa === '**') dp[i][j] = dp[i - 1][j] || dp[i][j - 1] || dp[i - 1][j - 1];
      else if (sb === '**') dp[i][j] = dp[i][j - 1] || dp[i - 1][j] || dp[i - 1][j - 1];
      else dp[i][j] = dp[i - 1][j - 1] && segTextOverlaps(sa, sb);
    }
  return dp[as.length][bs.length];
};
const globOverlaps = (a, b) => segListOverlaps(a.split('/'), b.split('/'));
const scopesOverlap = (A, B) => A.some((a) => B.some((b) => globOverlaps(a, b)));

// Files every ticket may touch (plan.json schema note: always-writable set).
const ALWAYS_WRITABLE = ['plan.json', 'docs/STATUS.md', 'Cargo.lock', 'docs/TECH_STACK.md'];
const scopeMinusAW = (t) => t.write_scope.filter((g) => !ALWAYS_WRITABLE.includes(g));

// P8 — territory law, adapted to this board's WIP=1 execution model
// (PLAYBOOK: one ticket per session; D-003: one conductor). Territory is
// claimed at in_progress and released at done (D-015 spirit): two
// concurrently ACTIVE tickets must never overlap scopes; scaffold declares
// an explicit exemption. Overlap between todo tickets is legal (they
// serialize) and reported informationally for parallelism planning.
let todoOverlapPairs = 0;
for (let i = 0; i < T.length; i++)
  for (let j = i + 1; j < T.length; j++) {
    const a = T[i], b = T[j];
    if (!scopesOverlap(scopeMinusAW(a), scopeMinusAW(b))) continue;
    if (a.scaffold === true || b.scaffold === true) continue;
    if (a.status === 'in_progress' && b.status === 'in_progress') {
      err(`P8 concurrent-active overlap: ${a.id}(${a.crate}) x ${b.id}(${b.crate})`);
    } else if (a.status === 'todo' && b.status === 'todo') {
      todoOverlapPairs++;
    }
  }

// P9 — informational report
const done = new Set(T.filter((t) => t.status === 'done').map((t) => t.id));
const claimable = T.filter((t) => t.status === 'todo' && t.depends_on.every((d) => done.has(d)) && t.hold !== true);
const byWave = {};
for (const t of T) {
  byWave[t.phase] ??= { n: 0, pts: 0 };
  byWave[t.phase].n++;
  byWave[t.phase].pts += t.points;
}

if (errors.length) {
  console.error(`validate-plan: ${errors.length} violation(s)`);
  for (const e of errors) console.error('  ' + e);
  process.exit(1);
}
console.log(`validate-plan OK — ${T.length} tickets · ${T.reduce((a, t) => a + t.points, 0)} pts`);
console.log('  per wave: ' + Object.entries(byWave).map(([w, v]) => `W${w}:${v.n}/${v.pts}pts`).join(' '));
console.log('  claimable now: ' + (claimable.map((t) => t.id).join(' ') || '(none)'));
console.log('  held (excluded from unattended claims): ' + (T.filter((t) => t.hold === true).map((t) => t.id).join(' ') || '(none)'));
console.log(`  todo-overlap pairs (must serialize; relevant only if parallel executors ever run): ${todoOverlapPairs}`);
