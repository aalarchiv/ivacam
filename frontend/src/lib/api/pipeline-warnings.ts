/// 94sf: pipeline-warning severity classifier. The Rust-side
/// PipelineWarning struct carries `kind`/`message`/`op_id` but no
/// severity field — the frontend maps `kind` to a severity here so
/// the GenerateBar safety gate can refuse to ship gcode that the
/// pipeline already flagged as broken (`tool_too_large`,
/// `frame_padding_below_tool_radius`, `op_order_suspect`, …).
///
/// Keep this list strict — anything Critical disables the Generate
/// button when `blockOnCriticalSimWarnings` is on. Informational
/// warnings (chamfer non-vbit, plunge_overridden, ramp_arcs_at_boundary)
/// stay non-critical so users can keep working with them visible.

import type { components } from './generated';

export type PipelineWarning = components['schemas']['PipelineWarning'];
export type PipelineSeverity = 'critical' | 'warning' | 'info';

/// Kinds the pipeline raises that signal "the emitted gcode is
/// substantively wrong and you should fix BEFORE running it on a
/// machine". These all reflect real silent-corruption modes the
/// audit caught:
///
/// * `tool_too_large` — cascade emitted ZERO toolpath; gcode is empty
///   or missing the pocket fill the user expects.
/// * `frame_padding_below_tool_radius` — the frame outline got clamped
///   to a degenerate width; cuts probably overlap the part wall.
/// * `op_order_suspect` (tnxu) — a profile cuts the part free
///   BEFORE a downstream op; the loose part doesn't get cut right.
/// * `spindle_speed_clamped_above_max` (3nnj) — the requested RPM
///   exceeded machine spindle_rpm_max; the controller may refuse, and
///   the new chipload doesn't match the user's intent.
/// * `chamfer_width_clamped_to_reach` — width clamped by V-bit reach;
///   final chamfer is narrower than ordered.
/// * `pocket_fill_incomplete` — wall is cut but interior is not;
///   the result is a hollow ring, not a pocket.
/// * `helix_radius_unfittable` — auto-helix bailed; cutter falls
///   through to a different (possibly unsafe) entry strategy.
const CRITICAL_KINDS: ReadonlySet<string> = new Set([
  'tool_too_large',
  'frame_padding_below_tool_radius',
  'op_order_suspect',
  'spindle_speed_clamped_above_max',
  'chamfer_width_clamped_to_reach',
  'pocket_fill_incomplete',
  'helix_radius_unfittable',
  'stock_origin_outside_geometry_bbox',
]);

export function pipelineWarningSeverity(w: PipelineWarning): PipelineSeverity {
  if (CRITICAL_KINDS.has(w.kind)) return 'critical';
  return 'warning';
}

/// Count Critical-severity pipeline warnings on a generate response.
/// `null` / undefined input returns 0 (no response = no warnings yet).
export function countCriticalPipelineWarnings(
  warnings: PipelineWarning[] | undefined | null,
): number {
  if (!warnings || warnings.length === 0) return 0;
  let n = 0;
  for (const w of warnings) {
    if (pipelineWarningSeverity(w) === 'critical') n++;
  }
  return n;
}
