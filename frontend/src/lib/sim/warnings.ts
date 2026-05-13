/// Sim-warning helpers — extracted from project.svelte.ts so vitest
/// (and any other consumer) can import them without bringing in the
/// Svelte 5 rune compiler. Pure functions over the SimWarning union.

import type { SimSeverity, SimWarning } from '../api/types';

/// Severity mapping for a sim warning. Mirrors
/// `wiac_core::sim::diagnostics::severity` so the UI can color-code
/// without a round-trip.
export function simWarningSeverity(w: SimWarning): SimSeverity {
  switch (w.kind) {
    case 'rapid_through_material':
    case 'fixture_collision':
    case 'holder_collision':
      return 'critical';
    case 'engagement_overload':
    case 'dragging_rapids':
      return 'warning';
  }
}

/// Segment index a warning attaches to. `dragging_rapids` reports a
/// run; we anchor it at the first segment in the run for marker
/// placement.
export function simWarningSegmentIdx(w: SimWarning): number {
  if (w.kind === 'dragging_rapids') return w.first_segment_idx;
  return w.segment_idx;
}

/// Short human-readable line for tooltips / list rows.
export function simWarningSummary(w: SimWarning): string {
  switch (w.kind) {
    case 'rapid_through_material':
      return `Rapid through material at segment ${w.segment_idx}, x=${w.worst_x.toFixed(1)} y=${w.worst_y.toFixed(1)}`;
    case 'fixture_collision':
      return `Fixture #${w.fixture_id} collision at segment ${w.segment_idx}`;
    case 'holder_collision':
      return `Tool holder hits wall at segment ${w.segment_idx} (clearance ${w.required_clearance_mm.toFixed(2)} mm)`;
    case 'engagement_overload':
      return `Engagement ${w.engagement_pct.toFixed(0)}% at segment ${w.segment_idx}`;
    case 'dragging_rapids':
      return `Dragging rapids: ${w.count} consecutive rapids from segment ${w.first_segment_idx}`;
  }
}
