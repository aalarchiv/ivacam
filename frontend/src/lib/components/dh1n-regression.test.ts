/// Regression guard for the dh1n self-scheduling-effect bug (p2c3).
///
/// The bug: an `$effect` body assigned to `$state` (e.g. `draft`) and
/// then read the same proxy back via a function call (e.g.
/// `JSON.stringify(draft)` or `snapshotKey()`) — Svelte 5 tracks reads
/// inside the effect body as dependencies of THAT effect, including
/// reads inside nested function calls, so the assignment rescheduled
/// the same effect. After ~1000 self-runs Svelte threw
/// `effect_update_depth_exceeded`, which kills the entire reactivity
/// scheduler — every `onclick` keeps firing but visible state never
/// updates. The Machine and Tool Library dialogs both manifested this
/// as "X / Cancel / OK don't close".
///
/// This test refuses to render Svelte (vitest skips the Svelte plugin
/// here), so we scan the source as text. The shape we ban: a
/// `$effect(...)` whose body assigns `draft = ...` AND later calls
/// `JSON.stringify(draft)` or `snapshotKey()` in the same block.
///
/// If a future refactor reintroduces the pattern, this test fails and
/// the offender shows up by file:line. Trade-off acknowledged: a
/// purely-textual scan is brittle vs. a real-Svelte runtime test, but
/// it costs no rendering setup and pins down the exact lexical shape
/// that broke us.

import { describe, expect, it } from 'vitest';
import { readFileSync, readdirSync, statSync } from 'node:fs';
import { join } from 'node:path';

function* walkSvelte(dir: string): Generator<string> {
  for (const entry of readdirSync(dir)) {
    const path = join(dir, entry);
    const s = statSync(path);
    if (s.isDirectory()) yield* walkSvelte(path);
    else if (entry.endsWith('.svelte')) yield path;
  }
}

/// Extract `$effect(...)` arrow-fn bodies as `{ file, line, body }`.
/// Naive brace-counter: starts at `$effect(`, increments on `{`,
/// decrements on `}`, returns the slice between the first matching
/// pair. Handles only the arrow-function form used in this codebase —
/// `$effect(() => { ... })`. Good enough for a lint.
function findEffectBodies(file: string): { line: number; body: string }[] {
  const src = readFileSync(file, 'utf8');
  const out: { line: number; body: string }[] = [];
  const marker = '$effect(';
  let idx = 0;
  while ((idx = src.indexOf(marker, idx)) !== -1) {
    const openBrace = src.indexOf('{', idx);
    if (openBrace === -1) break;
    let depth = 1;
    let cursor = openBrace + 1;
    while (cursor < src.length && depth > 0) {
      const ch = src[cursor];
      if (ch === '{') depth++;
      else if (ch === '}') depth--;
      cursor++;
    }
    const body = src.slice(openBrace + 1, cursor - 1);
    const line = src.slice(0, openBrace).split('\n').length;
    out.push({ line, body: stripComments(body) });
    idx = cursor;
  }
  return out;
}

/// Drop // and /* */ comments before regex-matching. Comments may
/// legitimately mention the banned patterns (e.g. "// don't call
/// snapshotKey() here") and should not trigger the lint.
function stripComments(s: string): string {
  return s
    .replace(/\/\*[\s\S]*?\*\//g, '')
    .replace(/\/\/[^\n]*/g, '');
}

describe('dh1n self-scheduling effect regression (p2c3)', () => {
  it('no $effect writes a draft and then reads it via JSON.stringify / snapshotKey in the same body', () => {
    const offenders: string[] = [];
    const root = join(__dirname, '../../../src');
    for (const file of walkSvelte(root)) {
      for (const { line, body } of findEffectBodies(file)) {
        // Skip bodies that don't write `draft`.
        const writesDraft = /^\s*draft\s*=/m.test(body);
        if (!writesDraft) continue;
        // Ban only the dangerous shapes where `draft` is read AS A
        // PROXY VALUE (not as an object-literal property key):
        //   JSON.stringify(draft)                     — bare argument
        //   JSON.stringify({ draft, ... })            — shorthand
        //   JSON.stringify({ draft })                 — shorthand
        //   snapshotKey()                             — original helper
        // Safe shapes (no match):
        //   JSON.stringify({ draft: newDraft, ... })  — explicit key
        //   const pristine = '...'.replace(draft, …)  — irrelevant
        const readsDraftBack =
          /JSON\.stringify\s*\(\s*draft\s*[,)]/.test(body) ||
          /JSON\.stringify\s*\(\s*\{\s*draft\s*[,}]/.test(body) ||
          /\bsnapshotKey\s*\(/.test(body);
        if (readsDraftBack) {
          offenders.push(`${file}:${line}`);
        }
      }
    }
    expect(offenders).toEqual([]);
  });
});
