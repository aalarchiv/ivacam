/// 94sf: tests for the pipeline-warning severity classifier. The
/// classifier is what wires the GenerateBar safety gate into the
/// pipeline's planning-time warnings, so the rules need to stay
/// strict and stable.

import { describe, expect, it } from 'vitest';
import {
  countCriticalPipelineWarnings,
  pipelineWarningSeverity,
  type PipelineWarning,
} from './pipeline-warnings';

const w = (kind: string, op_id: number | null = null): PipelineWarning => ({
  kind,
  message: `${kind} on op ${op_id ?? 'project'}`,
  op_id: op_id ?? undefined,
});

describe('pipelineWarningSeverity', () => {
  it('marks tool_too_large as critical', () => {
    expect(pipelineWarningSeverity(w('tool_too_large', 1))).toBe('critical');
  });

  it('marks op_order_suspect as critical', () => {
    expect(pipelineWarningSeverity(w('op_order_suspect', 2))).toBe('critical');
  });

  it('marks spindle_speed_clamped_above_max as critical', () => {
    expect(pipelineWarningSeverity(w('spindle_speed_clamped_above_max', 3))).toBe('critical');
  });

  it('marks stock_origin_outside_geometry_bbox as critical', () => {
    expect(pipelineWarningSeverity(w('stock_origin_outside_geometry_bbox'))).toBe('critical');
  });

  it('keeps informational kinds non-critical', () => {
    // These are the "the program will run; here's a heads-up" kinds —
    // they MUST NOT be promoted to critical or the safety gate
    // becomes unusable in practice.
    expect(pipelineWarningSeverity(w('plunge_overridden', 4))).toBe('warning');
    expect(pipelineWarningSeverity(w('ramp_arcs_at_boundary', 5))).toBe('warning');
    expect(pipelineWarningSeverity(w('chamfer_non_vbit', 6))).toBe('warning');
  });
});

describe('countCriticalPipelineWarnings', () => {
  it('returns 0 for empty / nullish input', () => {
    expect(countCriticalPipelineWarnings(null)).toBe(0);
    expect(countCriticalPipelineWarnings(undefined)).toBe(0);
    expect(countCriticalPipelineWarnings([])).toBe(0);
  });

  it('counts only critical kinds', () => {
    const ws: PipelineWarning[] = [
      w('tool_too_large', 1),
      w('plunge_overridden', 2),
      w('op_order_suspect', 3),
      w('chamfer_non_vbit', 4),
    ];
    expect(countCriticalPipelineWarnings(ws)).toBe(2);
  });

  /// 94sf acceptance: a Pocket with tool_too_large + blockOnCriticalSimWarnings
  /// MUST register at least one critical warning, so the GenerateBar
  /// safety gate that aggregates this count refuses to ship gcode.
  it('critical_pipeline_warning_blocks_generate', () => {
    const ws: PipelineWarning[] = [w('tool_too_large', 42)];
    expect(countCriticalPipelineWarnings(ws)).toBeGreaterThan(0);
  });
});
