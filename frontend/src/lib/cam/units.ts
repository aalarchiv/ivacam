/// Display helpers for mm-stored values when the user's machine is
/// configured for inch output (G20). Internal storage is always mm —
/// the machine `unit` field only affects G-code emission and display.
///
/// Both helpers are pure and dependency-free so call sites can use them
/// without pulling state slices into a render path.

const MM_PER_INCH = 25.4;

/// Format a mm-stored value as either "5.00 mm" or `0.197"` depending
/// on the project unit. Inch precision defaults to 3 decimals (≈0.025
/// mm) — enough to spot a 1 mm tool from a ⅛" tool at a glance. Callers
/// that need finer (e.g. tool-tip diameters in tenths) pass `inchDp`.
export function formatLength(
  mm: number,
  unit: 'mm' | 'inch',
  mmDp: number = 2,
  inchDp: number = 3,
): string {
  if (!Number.isFinite(mm)) return unit === 'inch' ? `0"` : '0 mm';
  if (unit === 'inch') {
    return `${(mm / MM_PER_INCH).toFixed(inchDp)}"`;
  }
  return `${mm.toFixed(mmDp)} mm`;
}

/// Unit suffix only — useful when the value is rendered separately
/// (e.g. inside an `<input>` next to a static-label suffix).
export function unitSuffix(unit: 'mm' | 'inch'): string {
  return unit === 'inch' ? 'in' : 'mm';
}

/// Numeric-input parsing with explicit "invalid" feedback. Returns
/// `{ value, invalid }` so call sites can flag the input with a red
/// border (via `class:invalid`) when the user typed something we
/// couldn't use. Replaces `parseFloat(v) || 0` / `parseInt(v) || 1`
/// patterns that silently coerced garbage to 0 / 1 without any UI
/// signal — fatal when those zeros became cut depths or 1-flute
/// SFM defaults.
export interface ParsedNumber {
  value: number | null;
  invalid: boolean;
}

/// Parse a string-or-number input as a finite number. Pass `min` /
/// `max` to enforce a range; out-of-range parses return `invalid:
/// true` with `value: null` so the caller can keep the prior value
/// (don't write it through to state) AND render the red-border cue.
export function parseFiniteNumber(
  raw: string | number | null | undefined,
  opts: { min?: number; max?: number; integer?: boolean } = {},
): ParsedNumber {
  if (raw === '' || raw == null) return { value: null, invalid: false };
  const n = typeof raw === 'number' ? raw : opts.integer ? parseInt(raw, 10) : parseFloat(raw);
  if (!Number.isFinite(n)) return { value: null, invalid: true };
  if (opts.min != null && n < opts.min) return { value: null, invalid: true };
  if (opts.max != null && n > opts.max) return { value: null, invalid: true };
  return { value: n, invalid: false };
}
