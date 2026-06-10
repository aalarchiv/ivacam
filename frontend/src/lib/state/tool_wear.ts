/// Tool wear-compensation helpers — pure logic shared by the tool
/// library (wear input + stale chip), the calibration dialog, and the
/// op-properties effective-diameter hint. Mirrors the Rust
/// `ToolEntry::effective_diameter()` semantics: path math cuts at
/// nominal − wear; the UI keeps displaying the nominal diameter.

import type { ToolEntry } from './project-types';

/// Days after which a calibration is considered stale (bits keep
/// wearing; a 90-day-old measurement is a guess, not a measurement).
export const CALIBRATION_STALE_DAYS = 90;

/// Mirror of Rust `ToolEntry::effective_diameter()`: nominal minus the
/// measured wear offset, floored at 0.01 mm.
export function effectiveDiameterMm(tool: Pick<ToolEntry, 'diameter' | 'wearOffsetMm'>): number {
  return Math.max(tool.diameter - (tool.wearOffsetMm ?? 0), 0.01);
}

/// Wear offset from a slot-test calibration: cut a shallow single-pass
/// slot (one straight line) with the tool, measure the slot WIDTH with
/// a caliper — the width IS the effective diameter — and the offset is
/// what's missing from the nominal. Returns null for non-finite /
/// non-positive measurements (caller shows a validation hint).
export function wearOffsetFromSlotWidth(
  nominalDiameterMm: number,
  measuredSlotWidthMm: number,
): number | null {
  if (!Number.isFinite(measuredSlotWidthMm) || measuredSlotWidthMm <= 0) return null;
  if (!Number.isFinite(nominalDiameterMm) || nominalDiameterMm <= 0) return null;
  return roundMm(nominalDiameterMm - measuredSlotWidthMm);
}

/// Whether a calibration date (ISO `YYYY-MM-DD`) is older than
/// [`CALIBRATION_STALE_DAYS`]. Unparseable dates count as stale —
/// better a spurious "re-measure" hint than trusting garbage.
export function isCalibrationStale(lastCalibrated: string | undefined, now: Date): boolean {
  if (!lastCalibrated) return false; // never calibrated ≠ stale — shown separately
  const then = Date.parse(lastCalibrated);
  if (Number.isNaN(then)) return true;
  const ageDays = (now.getTime() - then) / 86_400_000;
  return ageDays > CALIBRATION_STALE_DAYS;
}

/// Today as ISO `YYYY-MM-DD` (local date) — what `lastCalibrated`
/// stores when a calibration is applied.
export function todayIso(now: Date = new Date()): string {
  const y = now.getFullYear();
  const m = String(now.getMonth() + 1).padStart(2, '0');
  const d = String(now.getDate()).padStart(2, '0');
  return `${y}-${m}-${d}`;
}

/// One-line effective-diameter hint ("2.94 mm (3 mm − 0.06 wear)") for
/// the op panel / library row. Empty string when there's no wear.
export function effectiveDiameterHint(tool: Pick<ToolEntry, 'diameter' | 'wearOffsetMm'>): string {
  const wear = tool.wearOffsetMm ?? 0;
  if (wear === 0) return '';
  const sign = wear > 0 ? '−' : '+';
  return `${roundMm(effectiveDiameterMm(tool))} mm (${roundMm(tool.diameter)} mm ${sign} ${roundMm(
    Math.abs(wear),
  )} wear)`;
}

function roundMm(v: number): number {
  return Math.round(v * 1000) / 1000;
}
