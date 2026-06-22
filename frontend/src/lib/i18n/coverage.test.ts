/// i18n coverage gates — makes translation coverage *enforceable* (ivac-os2k.5).
/// Runs under the standard `pnpm run test` (vitest) step, so it guards every CI
/// run and `scripts/pre-release.sh` invocation alongside the codegen drift guard.
///
/// What it enforces:
///   1. Locale parity      — no stray keys in any locale; full parity for
///                           shipped locales (COMPLETE_LOCALES).
///   2. No empty values    — a key is never the empty string.
///   3. Placeholder parity — `{token}` sets match the base for translated keys.
///   4. No dead keys       — every catalog key is referenced in src/.
///   5. Hardcoded guard    — no un-wrapped user-facing literal in .svelte
///                           markup beyond the documented allowlist.
import { describe, it, expect } from 'vitest';
import { readFileSync, writeFileSync, readdirSync, existsSync, statSync } from 'node:fs';
import { join, resolve, relative, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { SUPPORTED_LOCALES, type Locale } from './i18n-core';
import { keyDiff, emptyValues, placeholderMismatches, deadKeys } from './coverage';
import { scanSvelte } from './hardcoded-scan';

/// Locales held to the full bar (no missing keys, no untranslated values).
/// Add 'de' here once the German authoring pass (ivac-os2k.8) is complete; the
/// other gates already apply to every locale that exists.
const COMPLETE_LOCALES: readonly Locale[] = ['en'];

const i18nDir = dirname(fileURLToPath(import.meta.url));
const srcDir = resolve(i18nDir, '..', '..'); // …/frontend/src
const messagesDir = join(i18nDir, 'messages');
const allowlistPath = join(i18nDir, 'hardcoded-allowlist.json');
/// `UPDATE_I18N_ALLOWLIST=1 pnpm test` rewrites the allowlist from the current
/// scan instead of asserting (snapshot-style refresh after an extraction sweep).
const UPDATE = !!process.env.UPDATE_I18N_ALLOWLIST;

function readJson<T = Record<string, string>>(file: string): T {
  return JSON.parse(readFileSync(file, 'utf8'));
}

const catalogs = Object.fromEntries(
  SUPPORTED_LOCALES.map((l) => [l, readJson(join(messagesDir, `${l}.json`))]),
) as Record<Locale, Record<string, string>>;
const en = catalogs.en;
const allKeys = Object.keys(en);

/// Walk a dir for files matching `exts`, skipping generated/catalog noise.
function collectFiles(dir: string, exts: string[]): string[] {
  const out: string[] = [];
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) {
      out.push(...collectFiles(p, exts));
    } else if (exts.some((e) => name.endsWith(e)) && p !== join(i18nDir, 'keys.ts')) {
      out.push(p);
    }
  }
  return out;
}

describe('i18n locale parity', () => {
  for (const loc of SUPPORTED_LOCALES) {
    if (loc === 'en') continue;
    const { missing, extra } = keyDiff(en, catalogs[loc]);
    it(`${loc}: no keys outside the English base`, () => {
      expect(extra, `${loc}.json has keys absent from en.json`).toEqual([]);
    });
    const strict = COMPLETE_LOCALES.includes(loc);
    it.skipIf(!strict)(`${loc}: every base key is translated`, () => {
      expect(missing, `${loc}.json is missing keys`).toEqual([]);
    });
  }
});

describe('i18n no empty values', () => {
  for (const loc of SUPPORTED_LOCALES) {
    it(`${loc}: no empty/whitespace-only values`, () => {
      expect(emptyValues(catalogs[loc])).toEqual([]);
    });
  }
});

describe('i18n placeholder parity', () => {
  for (const loc of SUPPORTED_LOCALES) {
    if (loc === 'en') continue;
    it(`${loc}: {token} sets match the English base`, () => {
      expect(placeholderMismatches(en, catalogs[loc])).toEqual([]);
    });
  }
});

describe('i18n no dead keys', () => {
  it('every catalog key is referenced in src/', () => {
    const src = collectFiles(srcDir, ['.ts', '.svelte'])
      .filter((p) => !p.startsWith(messagesDir))
      .map((p) => readFileSync(p, 'utf8'))
      .join('\n');
    expect(deadKeys(allKeys, src)).toEqual([]);
  });
});

describe('i18n hardcoded-string guard', () => {
  it('no un-wrapped user-facing literals beyond the allowlist', () => {
    const found: Record<string, string[]> = {};
    for (const file of collectFiles(srcDir, ['.svelte'])) {
      const hits = scanSvelte(readFileSync(file, 'utf8'));
      if (hits.length) found[relative(srcDir, file).replaceAll('\\', '/')] = hits;
    }
    const sorted = Object.fromEntries(Object.entries(found).sort(([a], [b]) => a.localeCompare(b)));

    if (UPDATE) {
      writeFileSync(allowlistPath, JSON.stringify(sorted, null, 2) + '\n');
      return;
    }

    expect(
      existsSync(allowlistPath),
      'missing hardcoded-allowlist.json — run `UPDATE_I18N_ALLOWLIST=1 pnpm test` to seed it',
    ).toBe(true);
    const allowed = readJson<Record<string, string[]>>(allowlistPath);

    // Exact match in both directions: NEW literals fail (extract or allowlist
    // them); allowlist entries that no longer appear also fail (delete them as
    // sweeps land). Refresh with `UPDATE_I18N_ALLOWLIST=1 pnpm test`.
    expect(sorted, 'hardcoded strings drifted from the allowlist — see message above').toEqual(
      allowed,
    );
  });
});
