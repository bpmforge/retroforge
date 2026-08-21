#!/usr/bin/env node
// Verify the documentation site's code samples (ticket W9-07, criterion
// 3: "every code sample in the site is compiled or executed by CI").
//
// ## Why this exists rather than a doctest runner
//
// The site's samples are not written in the site. Every one is an
// `{{#include}}` of a file the build already compiles, validates or runs
// — a plugin example built by `cargo test --workspace`, a profile checked
// by `retroforge-tool profile validate`, a spec section from the design
// docs. There is ONE copy of each sample and the site quotes it.
//
// That makes drift structurally impossible instead of merely detectable,
// which is stronger than compiling a second copy and finding out later
// that the two disagree. But it only holds while the rule is actually
// followed, so this script enforces it:
//
//   1. every `{{#include}}` target resolves to a real file (and a line
//      range does not run off its end);
//   2. no chapter carries an INLINE fenced block in a compiled or
//      validated language — those must be includes;
//   3. every SUMMARY.md entry points at a chapter that exists.
//
// Rule 2 is the one with teeth. Without it someone pastes a snippet in,
// it is never compiled, and the site quietly becomes the thing this
// ticket was written to prevent.
//
// ## Where this lives
//
// `scripts/` is where this project's other validators live
// (`validate-plan.mjs` and friends) and is the conventional home. It is
// outside W9-07's write_scope (`docs/**`, `.github/**`), so this sits
// under `.github/scripts/` instead. Worth moving when a ticket owns both.

import { readFileSync, existsSync, readdirSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, '..', '..');
const srcDir = join(repoRoot, 'docs', 'site', 'src');

/** Languages whose samples MUST come from a file the build checks. */
const MUST_BE_INCLUDED = new Set(['rust', 'rs', 'toml', 'lua', 'wat']);

const errors = [];
const stats = { chapters: 0, includes: 0 };

function chapterFiles() {
  return readdirSync(srcDir).filter((f) => f.endsWith('.md'));
}

/**
 * `{{#include path}}`, `{{#include path:START:END}}` or
 * `{{#include path:anchor}}`.
 *
 * Anchors are strongly preferred and line ranges are discouraged: a range
 * shifts silently when the source document is edited, which is the same
 * class of drift this whole script exists to stop. Writing this check
 * caught exactly that — an early draft of `plugins.md` cited lines
 * 243:255 of a 204-line file.
 */
const INCLUDE_RE = /\{\{#include\s+([^}\s:]+)(?::([^}\s:]+)(?::([^}\s]+))?)?\s*\}\}/g;

function checkIncludes(file, text) {
  for (const m of text.matchAll(INCLUDE_RE)) {
    stats.includes += 1;
    const [, rel, arg1, arg2] = m;
    const isRange = arg1 !== undefined && /^\d+$/.test(arg1);
    const startRaw = isRange ? arg1 : undefined;
    const endRaw = isRange ? arg2 : undefined;
    const anchor = arg1 !== undefined && !isRange ? arg1 : undefined;
    const target = resolve(srcDir, rel);
    if (!existsSync(target)) {
      errors.push(`${file}: {{#include ${rel}}} — no such file (${target})`);
      continue;
    }
    if (anchor !== undefined) {
      // An anchor that does not exist makes mdBook emit the WHOLE file,
      // which looks like a formatting mistake rather than a broken
      // reference — so it has to be an error here.
      const body = readFileSync(target, 'utf8');
      if (!body.includes(`ANCHOR: ${anchor}`)) {
        errors.push(`${file}: {{#include ${rel}:${anchor}}} — ${rel} has no \`ANCHOR: ${anchor}\``);
      } else if (!body.includes(`ANCHOR_END: ${anchor}`)) {
        errors.push(
          `${file}: {{#include ${rel}:${anchor}}} — ${rel} opens the anchor but never closes it`,
        );
      }
      continue;
    }
    if (startRaw !== undefined) {
      const start = Number(startRaw);
      const end = Number(endRaw);
      const lines = readFileSync(target, 'utf8').split('\n').length;
      if (start < 1 || end < start) {
        errors.push(`${file}: {{#include ${rel}:${start}:${end}}} — inverted or zero range`);
      } else if (end > lines) {
        errors.push(
          `${file}: {{#include ${rel}:${start}:${end}}} — ${rel} has only ${lines} lines. ` +
            `A range that runs off the end silently truncates the sample.`,
        );
      }
    }
  }
}

/**
 * Refuse an inline fenced block in a checked language.
 *
 * A fence counts as an include when the ONLY non-blank content between
 * its markers is an `{{#include}}` directive.
 */
function checkNoInlineSamples(file, text) {
  const lines = text.split('\n');
  let inFence = false;
  let lang = '';
  let body = [];
  let openedAt = 0;
  lines.forEach((line, i) => {
    const fence = line.match(/^```(\w*)/);
    if (fence && !inFence) {
      inFence = true;
      lang = fence[1].toLowerCase();
      body = [];
      openedAt = i + 1;
      return;
    }
    if (inFence && /^```\s*$/.test(line)) {
      inFence = false;
      if (MUST_BE_INCLUDED.has(lang)) {
        const meaningful = body.filter((l) => l.trim() !== '');
        const isInclude =
          meaningful.length > 0 && meaningful.every((l) => /\{\{#include\s/.test(l));
        if (!isInclude) {
          errors.push(
            `${file}:${openedAt}: inline \`${lang}\` block. Every ${lang} sample on the site must ` +
              `be {{#include}}d from a file CI compiles or validates — an inline copy is never ` +
              `checked and is exactly the drift this rule prevents.`,
          );
        }
      }
      return;
    }
    if (inFence) body.push(line);
  });
  if (inFence) errors.push(`${file}: unclosed code fence opened at line ${openedAt}`);
}

function checkSummary() {
  const summaryPath = join(srcDir, 'SUMMARY.md');
  if (!existsSync(summaryPath)) {
    errors.push('docs/site/src/SUMMARY.md is missing — mdBook needs it');
    return;
  }
  const text = readFileSync(summaryPath, 'utf8');
  const linked = new Set();
  for (const m of text.matchAll(/\]\(([^)]+\.md)\)/g)) {
    const rel = m[1];
    linked.add(rel);
    if (!existsSync(join(srcDir, rel))) {
      errors.push(`SUMMARY.md links ${rel}, which does not exist`);
    }
  }
  // The converse: a chapter nobody links is a chapter nobody reads, and
  // it will rot unnoticed.
  for (const f of chapterFiles()) {
    if (f === 'SUMMARY.md') continue;
    if (!linked.has(f)) {
      errors.push(`${f} exists but SUMMARY.md never links it — it would not appear on the site`);
    }
  }
}

for (const f of chapterFiles()) {
  if (f === 'SUMMARY.md') continue;
  stats.chapters += 1;
  const text = readFileSync(join(srcDir, f), 'utf8');
  checkIncludes(f, text);
  checkNoInlineSamples(f, text);
}
checkSummary();

if (errors.length > 0) {
  console.error(`verify-doc-samples: ${errors.length} problem(s)`);
  for (const e of errors) console.error(`  ${e}`);
  process.exit(1);
}
console.log(
  `verify-doc-samples OK — ${stats.chapters} chapter(s), ${stats.includes} include(s) resolved, ` +
    `no inline samples in checked languages`,
);
