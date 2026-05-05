// i18n bootstrap. Registers en + de locales with svelte-i18n and exposes
// helpers for the language switcher. The active locale is persisted in
// localStorage; default falls back to the browser language, then to 'en'.

import { addMessages, getLocaleFromNavigator, init, locale, locales } from 'svelte-i18n';
import en from './locales/en.json';
import de from './locales/de.json';

const STORAGE_KEY = 'wiac.locale';

addMessages('en', en);
addMessages('de', de);

function pickInitial(): string {
  if (typeof window === 'undefined') return 'en';
  const stored = window.localStorage.getItem(STORAGE_KEY);
  if (stored === 'en' || stored === 'de') return stored;
  const browser = getLocaleFromNavigator();
  if (typeof browser === 'string' && browser.startsWith('de')) return 'de';
  return 'en';
}

init({
  fallbackLocale: 'en',
  initialLocale: pickInitial(),
});

export function setLocale(code: 'en' | 'de'): void {
  locale.set(code);
  try {
    window.localStorage.setItem(STORAGE_KEY, code);
  } catch {
    // ignore — localStorage may be disabled in some embedded contexts
  }
}

export { locale, locales };
