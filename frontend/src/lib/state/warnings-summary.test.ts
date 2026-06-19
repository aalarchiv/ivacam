import { describe, it, expect } from 'vitest';
import { summarizeWarnings, type WarningsSummaryInput } from './warnings-summary';
import type { SimWarning } from '../api/types';
import type { PipelineWarning } from '../api/pipeline-warnings';

// Minimal builders — only the fields the summary logic reads.
function simCritical(seg = 0): SimWarning {
  return { kind: 'rapid_through_material', segment_idx: seg, worst_x: 0, worst_y: 0 } as SimWarning;
}
function simInfo(): SimWarning {
  return { kind: 'cell_size_coarsened' } as SimWarning;
}
function pipeCritical(): PipelineWarning {
  return { kind: 'tool_too_large', message: 'too big' } as PipelineWarning;
}
function pipeInfo(): PipelineWarning {
  return { kind: 'plunge_overridden', message: 'fyi' } as PipelineWarning;
}

function base(over: Partial<WarningsSummaryInput> = {}): WarningsSummaryInput {
  return {
    simWarnings: [],
    pipelineWarnings: [],
    hasGenerated: false,
    hasSimDiagnostics: false,
    dirty: false,
    ...over,
  };
}

describe('summarizeWarnings', () => {
  it('reports idle when nothing has generated or simulated', () => {
    const s = summarizeWarnings(base());
    expect(s.severity).toBe('idle');
    expect(s.hasRun).toBe(false);
    expect(s.total).toBe(0);
    expect(s.critical).toBe(0);
  });

  it('treats a clean Generate (no warnings, no sim) as clean — not idle', () => {
    // This is the drift the helper fixes: the old desktop copy keyed
    // "not run yet" off simDiagnostics + pipelineWarnings only, so a
    // clean Generate fell through to the idle lane.
    const s = summarizeWarnings(base({ hasGenerated: true }));
    expect(s.hasRun).toBe(true);
    expect(s.severity).toBe('clean');
  });

  it('counts critical sim + pipeline warnings together', () => {
    const s = summarizeWarnings(
      base({
        hasGenerated: true,
        hasSimDiagnostics: true,
        simWarnings: [simCritical(), simInfo()],
        pipelineWarnings: [pipeCritical(), pipeInfo()],
      }),
    );
    expect(s.total).toBe(4);
    expect(s.critical).toBe(2);
    expect(s.severity).toBe('critical');
  });

  it('reports warning when only non-critical warnings are present', () => {
    const s = summarizeWarnings(
      base({ hasGenerated: true, hasSimDiagnostics: true, pipelineWarnings: [pipeInfo()] }),
    );
    expect(s.critical).toBe(0);
    expect(s.total).toBe(1);
    expect(s.severity).toBe('warning');
  });

  it('goes stale when a sim verdict exists and the project is dirty', () => {
    const s = summarizeWarnings(
      base({ hasGenerated: true, hasSimDiagnostics: true, dirty: true, simWarnings: [simInfo()] }),
    );
    expect(s.stale).toBe(true);
    expect(s.severity).toBe('stale');
  });

  it('does not go stale on dirty alone without a sim verdict', () => {
    const s = summarizeWarnings(base({ hasGenerated: true, dirty: true }));
    expect(s.stale).toBe(false);
    expect(s.severity).toBe('clean');
  });
});
