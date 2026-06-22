/// Pure i18n logic — no Svelte runes, so it's unit-testable under the
/// logic-only vitest config. The reactive layer (`$state` locale + the
/// `t()` binding) lives in `i18n.svelte.ts`, which wraps these helpers.
import type { MsgKey } from './keys';

export type Locale = 'en' | 'de';
/// User preference: an explicit locale or `auto` (follow the environment).
export type LanguagePref = 'auto' | Locale;

export const SUPPORTED_LOCALES: readonly Locale[] = ['en', 'de'];

export function isLocale(s: string): s is Locale {
  return (SUPPORTED_LOCALES as readonly string[]).includes(s);
}

/// The subset of `navigator` we read, so tests can pass a fake.
export interface LocaleEnv {
  language?: string;
  languages?: readonly string[];
}

function defaultEnv(): LocaleEnv | undefined {
  return typeof navigator === 'undefined' ? undefined : navigator;
}

/// Best-effort match of the environment's preferred languages against the
/// locales we ship. Works in the Tauri webview and the browser/wasm build
/// (both expose `navigator.language(s)`), so no OS-locale plugin is needed.
export function detectLocale(env: LocaleEnv | undefined = defaultEnv()): Locale {
  if (!env) return 'en';
  const prefs =
    env.languages && env.languages.length ? env.languages : env.language ? [env.language] : [];
  for (const tag of prefs) {
    const base = tag?.toLowerCase().split('-')[0] ?? '';
    if (isLocale(base)) return base;
  }
  return 'en';
}

/// Resolve a stored preference to a concrete locale.
export function resolveLocale(
  pref: LanguagePref,
  env: LocaleEnv | undefined = defaultEnv(),
): Locale {
  return pref === 'auto' ? detectLocale(env) : pref;
}

/// Look up a key in `locale`, falling back to English and then to the key
/// itself (so a missing string is visible, never a crash). `{name}`
/// placeholders are substituted from `params`.
export function lookup(
  catalogs: Record<Locale, Record<string, string>>,
  locale: Locale,
  key: MsgKey,
  params?: Record<string, string | number>,
): string {
  let s = catalogs[locale]?.[key] ?? catalogs.en[key] ?? key;
  if (params) {
    for (const [k, v] of Object.entries(params)) {
      s = s.replaceAll(`{${k}}`, String(v));
    }
  }
  return s;
}
