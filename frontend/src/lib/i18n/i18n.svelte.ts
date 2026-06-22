/// Reactive i18n layer. Zero external dependencies: plain JSON catalogs +
/// a Svelte 5 `$state` locale. Reading `i18n.locale` inside `translate`
/// makes every `t()` call site reactive, so switching language re-renders
/// the UI live (no reload). The pure logic lives in `i18n-core.ts`.
///
/// Adding a language: drop `messages/<xx>.json` in, add `'xx'` to
/// `SUPPORTED_LOCALES` (i18n-core.ts), and add an option to the Settings
/// picker. The coverage tests (ivac-os2k.5) then enforce full key parity.
import en from './messages/en.json';
import de from './messages/de.json';
import type { MsgKey } from './keys';
import { type Locale, type LanguagePref, lookup, resolveLocale } from './i18n-core';

/// `en` is the source of truth and the ultimate fallback, so it must be a
/// total map; other locales may lag and fall back per-key.
const CATALOGS: Record<Locale, Record<string, string>> = {
  en: en as Record<string, string>,
  de: de as Record<string, string>,
};

class I18n {
  /// The active, resolved locale. `$state` so `translate` reads register a
  /// reactive dependency at every call site.
  locale = $state<Locale>('en');

  /// Apply a user preference (from AppSettings.language). Idempotent.
  setPreference(pref: LanguagePref): void {
    const next = resolveLocale(pref);
    if (next !== this.locale) this.locale = next;
  }

  translate(key: MsgKey, params?: Record<string, string | number>): string {
    return lookup(CATALOGS, this.locale, key, params);
  }
}

export const i18n = new I18n();

/// The translation function used throughout the UI: `t('settings.appearance.language')`.
/// Reactive when called in Svelte markup or a `$derived`/`$effect`.
export function t(key: MsgKey, params?: Record<string, string | number>): string {
  return i18n.translate(key, params);
}
