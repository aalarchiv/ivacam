import { describe, expect, it } from 'vitest';
import {
  effectiveDiameterHint,
  effectiveDiameterMm,
  isCalibrationStale,
  todayIso,
  wearOffsetFromSlotWidth,
} from './tool_wear';

describe('effectiveDiameterMm', () => {
  it('mirrors the Rust semantics: nominal − wear, floored at 0.01', () => {
    expect(effectiveDiameterMm({ diameter: 3, wearOffsetMm: undefined })).toBe(3);
    expect(effectiveDiameterMm({ diameter: 3, wearOffsetMm: 0.06 })).toBeCloseTo(2.94, 12);
    expect(effectiveDiameterMm({ diameter: 3, wearOffsetMm: -0.02 })).toBeCloseTo(3.02, 12);
    expect(effectiveDiameterMm({ diameter: 3, wearOffsetMm: 5 })).toBe(0.01);
  });
});

describe('wearOffsetFromSlotWidth', () => {
  it('computes nominal − measured (slot width IS the effective diameter)', () => {
    expect(wearOffsetFromSlotWidth(3, 2.94)).toBeCloseTo(0.06, 12);
    expect(wearOffsetFromSlotWidth(6, 6.02)).toBeCloseTo(-0.02, 12);
    expect(wearOffsetFromSlotWidth(3, 3)).toBe(0);
  });

  it('rejects garbage measurements', () => {
    expect(wearOffsetFromSlotWidth(3, 0)).toBeNull();
    expect(wearOffsetFromSlotWidth(3, -1)).toBeNull();
    expect(wearOffsetFromSlotWidth(3, NaN)).toBeNull();
    expect(wearOffsetFromSlotWidth(0, 3)).toBeNull();
  });
});

describe('isCalibrationStale', () => {
  const now = new Date('2026-06-10T12:00:00Z');
  it('flags measurements older than 90 days', () => {
    expect(isCalibrationStale('2026-01-01', now)).toBe(true);
    expect(isCalibrationStale('2026-06-01', now)).toBe(false);
    expect(isCalibrationStale('2026-03-13', now)).toBe(false); // ~89.5 days
    expect(isCalibrationStale('2026-03-11', now)).toBe(true); // ~91.5 days
  });
  it('never-calibrated is not stale (shown as its own state)', () => {
    expect(isCalibrationStale(undefined, now)).toBe(false);
  });
  it('unparseable dates count as stale', () => {
    expect(isCalibrationStale('not-a-date', now)).toBe(true);
  });
});

describe('effectiveDiameterHint', () => {
  it('formats worn / reground / pristine tools', () => {
    expect(effectiveDiameterHint({ diameter: 3, wearOffsetMm: 0.06 })).toBe(
      '2.94 mm (3 mm − 0.06 wear)',
    );
    expect(effectiveDiameterHint({ diameter: 3, wearOffsetMm: -0.02 })).toBe(
      '3.02 mm (3 mm + 0.02 wear)',
    );
    expect(effectiveDiameterHint({ diameter: 3, wearOffsetMm: 0 })).toBe('');
    expect(effectiveDiameterHint({ diameter: 3, wearOffsetMm: undefined })).toBe('');
  });
});

describe('todayIso', () => {
  it('formats a local YYYY-MM-DD', () => {
    expect(todayIso(new Date(2026, 5, 10))).toBe('2026-06-10');
    expect(todayIso(new Date(2026, 0, 3))).toBe('2026-01-03');
  });
});
