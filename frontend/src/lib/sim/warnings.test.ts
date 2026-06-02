import { describe, expect, it } from 'vitest';
import { simWarningSeverity, simWarningKey } from './warnings';
import type { SimWarning } from '../api/types';

const coarsened = (reason = 'max_simulation_cells'): SimWarning =>
  ({
    kind: 'cell_size_coarsened',
    original_cell_size_mm: 0.2,
    coarsened_cell_size_mm: 0.41,
    reason,
  }) as unknown as SimWarning;

const rapid = (segment_idx: number): SimWarning =>
  ({
    kind: 'rapid_through_material',
    segment_idx,
    worst_x: 1,
    worst_y: 2,
  }) as unknown as SimWarning;

const fixture = (fixture_id: number, segment_idx: number): SimWarning =>
  ({ kind: 'fixture_collision', fixture_id, segment_idx }) as unknown as SimWarning;

const holder = (segment_idx: number): SimWarning =>
  ({
    kind: 'holder_collision',
    segment_idx,
    required_clearance_mm: 3,
  }) as unknown as SimWarning;

describe('simWarningSeverity', () => {
  it('cell_size_coarsened is informational (never blocks)', () => {
    expect(simWarningSeverity(coarsened())).toBe('info');
  });
  it('collision/rapid warnings are critical', () => {
    expect(simWarningSeverity(rapid(0))).toBe('critical');
    expect(simWarningSeverity(fixture(1, 0))).toBe('critical');
    expect(simWarningSeverity(holder(0))).toBe('critical');
  });
});

describe('simWarningKey (dedup identity)', () => {
  it('collapses every cell_size_coarsened to one key (it is re-emitted each advance)', () => {
    // Different reasons / sizes still dedupe — it is the same setup-time
    // notice, and we only want one row no matter how many frames re-emit.
    expect(simWarningKey(coarsened('max_simulation_cells'))).toBe(
      simWarningKey(coarsened('other')),
    );
  });

  it('keys segment warnings by kind + segment so distinct hits stay distinct', () => {
    expect(simWarningKey(rapid(3))).not.toBe(simWarningKey(rapid(4)));
    expect(simWarningKey(rapid(3))).toBe(simWarningKey(rapid(3))); // scrub-back re-fire → same key
    expect(simWarningKey(holder(3))).not.toBe(simWarningKey(rapid(3))); // kind disambiguates
  });

  it('keys fixture collisions by fixture id + segment', () => {
    expect(simWarningKey(fixture(1, 5))).toBe(simWarningKey(fixture(1, 5)));
    expect(simWarningKey(fixture(1, 5))).not.toBe(simWarningKey(fixture(2, 5)));
    expect(simWarningKey(fixture(1, 5))).not.toBe(simWarningKey(fixture(1, 6)));
  });
});
