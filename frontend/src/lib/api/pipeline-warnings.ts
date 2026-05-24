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
///
/// Round-2 audit additions (fj88) — these kinds emit silently when
/// the pipeline can't produce a valid toolpath but still returns a
/// "successful" generate response:
///
/// * `zero_rate_emitted` — feed / plunge / spindle rate resolved to 0;
///   downstream gcode lacks an F/S word and the machine may refuse.
/// * `op_source_empty` — op references zero geometry after filtering;
///   no toolpath emitted for this op.
/// * `op_source_missing_object` — op references an object that no
///   longer exists; that part of the user's selection is silently
///   dropped.
/// * `vcarve_no_medial_axis` / `vcarve_no_closed_region` /
///   `vcarve_below_tip_radius` — V-Carve op produced no usable cut
///   (no medial axis / no closed boundary / region too narrow for
///   the tip); the V-groove the user expects is missing.
/// * `tool_geometry_impossible` — declared tool dims are mathematically
///   impossible (e.g. tip ⌀ ≥ cutting ⌀); the depth-from-angle math
///   silently produces garbage.
/// * `thread_zero_bore` / `thread_tool_too_large` / `thread_no_depth` /
///   `thread_no_circles` — Thread op can't compute helical pass: no
///   thread is cut, but the gcode file looks "done".
/// * `halfpipe_tool_reach_exceeded` — half-pipe radius exceeds tool
///   stickout; the cut goes wherever the holder collides.
/// * `halfpipe_radius_mismatch` — selected tool radius doesn't match
///   the requested half-pipe radius; profile is the wrong shape.
/// * `parallel_offset_panicked` — the polygon offset library bailed;
///   the toolpath for that pocket is missing or partial.
/// * `dual_tool_no_toolchange` — Dual-tool op needs an M6, but the
///   post profile suppresses tool changes; the second tool's path
///   runs with the first tool still loaded.
const CRITICAL_KINDS: ReadonlySet<string> = new Set([
  'tool_too_large',
  'frame_padding_below_tool_radius',
  'op_order_suspect',
  'spindle_speed_clamped_above_max',
  'chamfer_width_clamped_to_reach',
  'pocket_fill_incomplete',
  'helix_radius_unfittable',
  'stock_origin_outside_geometry_bbox',
  // Front-end-synthesized kinds from GenerateBar's post-Generate
  // bounds scan — surfaced through the same warnings panel as the
  // pipeline's own warnings so the user has one place to look.
  'out_of_stock',
  'out_of_work_area',
  // fj88 round-2 additions
  'zero_rate_emitted',
  'op_source_empty',
  'op_source_missing_object',
  'vcarve_no_medial_axis',
  'vcarve_no_closed_region',
  'vcarve_below_tip_radius',
  'tool_geometry_impossible',
  'thread_zero_bore',
  'thread_tool_too_large',
  'thread_no_depth',
  'thread_no_circles',
  'halfpipe_tool_reach_exceeded',
  'halfpipe_radius_mismatch',
  'parallel_offset_panicked',
  'dual_tool_no_toolchange',
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
