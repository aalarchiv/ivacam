import { describe, expect, it } from 'vitest';
import { detectLocale, resolveLocale, isLocale, lookup, SUPPORTED_LOCALES } from './i18n-core';

const CATALOGS = {
  en: { 'a.b': 'Hello {name}', 'only.en': 'English only' },
  de: { 'a.b': 'Hallo {name}' },
} as const;

describe('isLocale', () => {
  it('accepts shipped locales, rejects others', () => {
    for (const l of SUPPORTED_LOCALES) expect(isLocale(l)).toBe(true);
    expect(isLocale('fr')).toBe(false);
    expect(isLocale('')).toBe(false);
  });
});

describe('detectLocale', () => {
  it('matches the first supported language by base tag', () => {
    expect(detectLocale({ languages: ['de-DE', 'en-US'] })).toBe('de');
    expect(detectLocale({ languages: ['en-GB'] })).toBe('en');
    expect(detectLocale({ language: 'de' })).toBe('de');
  });

  it('skips unsupported languages and falls back to en', () => {
    expect(detectLocale({ languages: ['fr-FR', 'de-AT'] })).toBe('de');
    expect(detectLocale({ languages: ['fr', 'es'] })).toBe('en');
    expect(detectLocale({})).toBe('en');
    expect(detectLocale(undefined)).toBe('en');
  });
});

describe('resolveLocale', () => {
  it('passes explicit locales through and resolves auto from the env', () => {
    expect(resolveLocale('en', { languages: ['de'] })).toBe('en');
    expect(resolveLocale('de', { languages: ['en'] })).toBe('de');
    expect(resolveLocale('auto', { languages: ['de-DE'] })).toBe('de');
    expect(resolveLocale('auto', { languages: ['fr'] })).toBe('en');
  });
});

describe('lookup', () => {
  it('returns the active-locale string and interpolates params', () => {
    // Synthetic keys (cast) — the production MsgKey union is derived from
    // the real en.json, so test fixtures live outside it.
    expect(lookup(CATALOGS, 'de', 'a.b' as never, { name: 'Welt' })).toBe('Hallo Welt');
    expect(lookup(CATALOGS, 'en', 'a.b' as never, { name: 'World' })).toBe('Hello World');
  });

  it('falls back to en, then to the key itself', () => {
    // Missing in de → English fallback.
    expect(lookup(CATALOGS, 'de', 'only.en' as never)).toBe('English only');
    // Missing everywhere → the key is shown (never a crash).
    expect(lookup(CATALOGS, 'de', 'no.such.key' as never)).toBe('no.such.key');
  });

  it('replaces every occurrence of a placeholder', () => {
    const cats = { en: { rep: '{x}-{x}' }, de: {} } as Record<'en' | 'de', Record<string, string>>;
    expect(lookup(cats, 'en', 'rep' as never, { x: 7 })).toBe('7-7');
  });
});
