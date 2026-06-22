/// Public i18n entry point. Import `t` for translations and `i18n` /
/// `resolveLocale` when wiring the locale to settings (see App.svelte).
export { t, i18n } from './i18n.svelte';
export {
  detectLocale,
  resolveLocale,
  isLocale,
  SUPPORTED_LOCALES,
  type Locale,
  type LanguagePref,
} from './i18n-core';
export type { MsgKey } from './keys';
