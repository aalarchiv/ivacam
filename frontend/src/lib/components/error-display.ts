/// Pure rendering helpers for `ErrorToast.svelte` — kept rune-free so they
/// unit-test under the logic-only vitest config. They take the `translate`
/// function as a parameter (the component passes `t`), which both decouples
/// them from the reactive i18n layer and lets tests inject a plain lookup.
///
/// The localization seam: a backend `WiacError` carries a stable `code` +
/// structured `params` (see crates/ivac-core/src/errors.rs). When `code` is
/// present we render `error.code.<code>` / `error.hint.<code>` against
/// `params`; otherwise we fall back to the English `message`/`recovery_hint`
/// the backend always supplies (e.g. import-parser failures with no code).
import type { WiacError } from '../api/types';
import type { MsgKey } from '../i18n/keys';

export type Translate = (key: MsgKey, params?: Record<string, string | number>) => string;

export function errorMessage(e: WiacError, t: Translate): string {
  return e.code ? t(`error.code.${e.code}`, e.params ?? {}) : e.message;
}

export function errorHint(e: WiacError, t: Translate): string | null {
  if (!e.recovery_hint) return null;
  return e.code ? t(`error.hint.${e.code}`, e.params ?? {}) : e.recovery_hint;
}

export function fixLabel(fix: WiacError['auto_fix'], t: Translate): string {
  if (!fix) return t('error.fix.apply');
  switch (fix.kind) {
    case 'assign_tool':
      return t('error.fix.assign_tool', { tool_id: fix.suggested_tool_id, op_id: fix.op_id });
    case 'disable_op':
      return t('error.fix.disable_op', { op_id: fix.op_id });
    case 'change_profile_offset':
      return t('error.fix.change_profile_offset', { op_id: fix.op_id, offset: fix.suggested });
    case 'lower_sim_resolution':
      return t('error.fix.lower_sim_resolution', { cell_mm: fix.suggested_cell_mm });
  }
}
