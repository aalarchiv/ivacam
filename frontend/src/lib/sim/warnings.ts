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
    // wpzm: cell_size coarsening is informational — sim still works,
    // just at coarser resolution. Render in the "info" lane.
    case 'cell_size_coarsened':
      return 'info';
  }
}

/// Segment index a warning attaches to. `cell_size_coarsened` is
/// setup-time and not attached to any segment — return -1 so the
/// caller can skip marker placement.
export function simWarningSegmentIdx(w: SimWarning): number {
  if (w.kind === 'cell_size_coarsened') return -1;
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
    case 'cell_size_coarsened':
      return `Sim cell size coarsened ${w.original_cell_size_mm.toFixed(3)} mm → ${w.coarsened_cell_size_mm.toFixed(3)} mm (${w.reason})`;
  }
}
