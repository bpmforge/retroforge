#!/usr/bin/env node
// Two-way reference-chain validator (design-review arc P4, 2026-07-15).
// Adapted from ~/Code/shipwright/scripts/validate-traceability.mjs.
// Chain: D (DECISIONS) → FR/NFR (SRS) → story (USER_STORIES) → ticket
// (plan.json stories[] + text citations) → doc path.
// Hard failures F1-F5 exit 1; coverage warnings W1-W3 exit 0 (GAP input).
import { readFileSync, readdirSync, existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const read = (p) => readFileSync(join(root, p), 'utf8');

const srs = read('docs/SRS.md');
const stories = read('docs/USER_STORIES.md');
const decisions = read('docs/DECISIONS.md');
const plan = JSON.parse(read('plan.json'));

const failures = [];
const warnings = [];

// ---- definitions ----
const frDefs = new Set();
for (const m of srs.matchAll(/^\| ((?:FR-[A-Z]+|NFR)-\d{3}) /gm)) frDefs.add(m[1]);
const dDefs = new Set();
for (const m of decisions.matchAll(/^\| (D-\d{3}) /gm)) dDefs.add(m[1]);
const storyDefs = new Map(); // id -> body text (bullet + AC lines)
{
  const re = /^- \*\*(E\d+-S\d+)\*\*[\s\S]*?(?=^- \*\*E|^## |^\| Epic)/gm;
  for (const m of stories.matchAll(re)) storyDefs.set(m[1], m[0]);
}

// ---- citation expansion: FR-CORE-020..026, FR-PROF-005/006/007, FR-PROF-* ----
const expandFr = (text) => {
  const out = new Set();
  // ranges: FR-XXX-020..026
  for (const m of text.matchAll(/\b(FR-[A-Z]+|NFR)-(\d{3})\.\.(\d{3})\b/g)) {
    const [, pre, a, b] = m;
    for (let i = Number(a); i <= Number(b); i++) out.add(`${pre}-${String(i).padStart(3, '0')}`);
  }
  // slash compounds: FR-MODE-002/003, FR-PROF-005/006/007
  for (const m of text.matchAll(/\b(FR-[A-Z]+|NFR)-(\d{3}(?:\/\d{3})+)\b/g)) {
    for (const n of m[2].split('/')) out.add(`${m[1]}-${n}`);
  }
  // globs: FR-PROF-*
  for (const m of text.matchAll(/\b(FR-[A-Z]+)-\*/g)) {
    for (const d of frDefs) if (d.startsWith(m[1] + '-')) out.add(d);
  }
  // singles (skip range/compound parts already consumed — safe to re-add)
  for (const m of text.matchAll(/\b((?:FR-[A-Z]+|NFR)-\d{3})\b/g)) out.add(m[1]);
  return out;
};

// ---- ticket text + scope helpers ----
const ticketText = (t) => [t.title, ...(t.acceptance ?? []), ...(Array.isArray(t.notes) ? t.notes : [])].join('\n');
const allScopes = plan.tickets.flatMap((t) => t.write_scope);
const coveredByScope = (p) => allScopes.some((w) => w === p || p.startsWith(w.replace(/\*\*?$/, '')));

// ---- F1: doc paths cited by tickets must exist or be creatable in-scope ----
for (const t of plan.tickets) {
  for (const m of ticketText(t).matchAll(/\bdocs\/[A-Za-z0-9_./-]+\.md\b/g)) {
    const p = m[0];
    if (!existsSync(join(root, p)) && !coveredByScope(p)) failures.push(`F1 ${t.id}: cites missing doc ${p}`);
  }
  for (const m of ticketText(t).matchAll(/\bfixtures\/[A-Za-z0-9_./-]+\.md\b/g)) {
    const p = m[0];
    if (!existsSync(join(root, p)) && !coveredByScope(p)) failures.push(`F1 ${t.id}: cites missing doc ${p}`);
  }
}

// ---- F2: FR/NFR cited by tickets must be defined ----
for (const t of plan.tickets) {
  for (const fr of expandFr(ticketText(t))) {
    if (!frDefs.has(fr)) failures.push(`F2 ${t.id}: cites undefined ${fr}`);
  }
}

// ---- F3: D-xxx cited in normative docs/tickets must be defined ----
const dScanFiles = ['plan.json', 'docs/SRS.md', 'docs/SCOPE.md', 'docs/CONSTRAINTS.md', 'docs/ARCHITECTURE.md', 'docs/TESTING.md', 'docs/RISKS.md', 'docs/PREREQUISITES.md', 'PLAYBOOK.md', 'MASTER_PROMPT.md', ...readdirSync(join(root, 'docs/design')).map((f) => 'docs/design/' + f)];
for (const f of dScanFiles) {
  const txt = read(f);
  for (const m of txt.matchAll(/\b(D-\d{3})\b/g)) {
    if (!dDefs.has(m[1])) failures.push(`F3 ${f}: cites undefined decision ${m[1]}`);
  }
}

// ---- F4: ticket stories[] must be defined ----
for (const t of plan.tickets) {
  for (const s of t.stories ?? []) {
    if (!storyDefs.has(s)) failures.push(`F4 ${t.id}: cites undefined story ${s}`);
  }
}

// ---- F5: FR cited in USER_STORIES (headers + bodies) must be defined ----
for (const fr of expandFr(stories)) {
  if (!frDefs.has(fr)) failures.push(`F5 USER_STORIES: cites undefined ${fr}`);
}

// ---- coverage: FR -> ticket (direct text citation or via covered story epic/body) ----
const coveredStories = new Set(plan.tickets.flatMap((t) => t.stories ?? []));
const frCited = new Set();
for (const t of plan.tickets) for (const fr of expandFr(ticketText(t))) frCited.add(fr);
// story-transitive: an FR cited in a covered story's epic header or body counts
const epicBlocks = stories.split(/^## /m).slice(1);
for (const block of epicBlocks) {
  const header = block.split('\n')[0];
  const blockFrs = expandFr(block);
  const blockStories = [...block.matchAll(/\*\*(E\d+-S\d+)\*\*/g)].map((m) => m[1]);
  if (blockStories.some((s) => coveredStories.has(s))) for (const fr of blockFrs) frCited.add(fr);
}
for (const fr of frDefs) if (!frCited.has(fr)) warnings.push(`W1 ${fr}: defined but reachable by no ticket (text or covered story)`);

// ---- W2: design docs never referenced by any ticket ----
const allTicketText = plan.tickets.map(ticketText).join('\n');
for (const f of readdirSync(join(root, 'docs/design'))) {
  if (!allTicketText.includes(f)) warnings.push(`W2 docs/design/${f}: referenced by no ticket`);
}

// ---- W3: story covered by zero tickets ----
for (const s of storyDefs.keys()) if (!coveredStories.has(s)) warnings.push(`W3 ${s}: covered by no ticket's stories[]`);

// ---- report ----
console.log(`traceability inventory: ${frDefs.size} FR/NFR defined · ${frCited.size} reachable · ${storyDefs.size} stories defined · ${[...storyDefs.keys()].filter((s) => coveredStories.has(s)).length} ticket-covered · ${dDefs.size} decisions`);
for (const w of [...new Set(warnings)]) console.log('  WARN ' + w);
if (failures.length) {
  console.error(`validate-traceability: ${failures.length} hard failure(s)`);
  for (const f of [...new Set(failures)]) console.error('  FAIL ' + f);
  process.exit(1);
}
console.log('validate-traceability OK — zero dangling refs');
