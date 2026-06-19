/// Shared warning-summary logic for the two warning bars: the desktop
/// `GenerateBar` chip + panel and the phone `PhoneWarnings` chip + panel.
/// Both reimplemented the same aggregation (sim + pipeline warning counts,
/// critical count, stale detection) and the same severity → presentation
/// state machine, and the copies had drifted: GenerateBar keyed "not run
/// yet" off `simDiagnostics == null && pipelineWarnings.length === 0`
/// (so a clean Generate that emitted no warnings AND ran no sim still read
/// as "not run yet"), while PhoneWarnings keyed it off
/// `generated != null || simDiagnostics != null` — the more correct test.
///
/// This module is the single source of truth, extracted as pure functions
/// over plain arrays/flags so vitest can cover it without the rune runtime
/// (same pattern as `sim/warnings.ts` and `op_creation.ts`). Components
/// pass in the raw `project.gen` fields and render the result; the
/// canonical (phone) `hasRun` semantics win.
import { simWarningSeverity } from '../sim/warnings';
import { countCriticalPipelineWarnings, type PipelineWarning } from '../api/pipeline-warnings';
import type { SimWarning } from '../api/types';

/// Single severity lane shared by both chips. Presentation (glyph, label,
/// color class) stays per-component — desktop and phone word and glyph
/// them differently — but the lane that drives it is computed once here.
///
/// * `idle`     — nothing generated and no sim run yet (Generate first).
/// * `stale`    — a sim verdict exists but the project changed since
///                (re-Generate to refresh).
/// * `critical` — at least one critical sim or pipeline warning.
/// * `warning`  — non-critical warnings present.
/// * `clean`    — ran, no warnings.
export type WarningSeverity = 'idle' | 'stale' | 'critical' | 'warning' | 'clean';

export interface WarningsSummaryInput {
  /// Accumulated sim diagnostics warnings (`project.gen.simDiagnostics?.warnings ?? []`).
  simWarnings: SimWarning[];
  /// Pipeline-level warnings from the last Generate
  /// (`project.gen.generated?.warnings ?? []`).
  pipelineWarnings: PipelineWarning[];
  /// Did the last Generate produce a result? (`project.gen.generated != null`)
  hasGenerated: boolean;
  /// Has the sim produced diagnostics? (`project.gen.simDiagnostics != null`)
  hasSimDiagnostics: boolean;
  /// Has the project changed since the last successful Generate?
  /// (`project.data.dirty`)
  dirty: boolean;
}

export interface WarningsSummary {
  sim: SimWarning[];
  pipeline: PipelineWarning[];
  /// Critical count across BOTH sim warnings and pipeline warnings — the
  /// number the download safety gate keys off.
  critical: number;
  /// Total warning rows (sim + pipeline).
  total: number;
  /// Anything generated or simulated yet?
  hasRun: boolean;
  /// A sim verdict exists but the project has been edited since.
  stale: boolean;
  severity: WarningSeverity;
}

export function summarizeWarnings(input: WarningsSummaryInput): WarningsSummary {
  const sim = input.simWarnings;
  const pipeline = input.pipelineWarnings;
  const critical =
    sim.filter((w) => simWarningSeverity(w) === 'critical').length +
    countCriticalPipelineWarnings(pipeline);
  const total = sim.length + pipeline.length;
  // Canonical "has anything run" test (the phone copy's, which also
  // covers a clean Generate that emitted no warnings and ran no sim).
  const hasRun = input.hasGenerated || input.hasSimDiagnostics;
  const stale = input.hasSimDiagnostics && input.dirty;

  let severity: WarningSeverity;
  if (!hasRun) severity = 'idle';
  else if (stale) severity = 'stale';
  else if (critical > 0) severity = 'critical';
  else if (total > 0) severity = 'warning';
  else severity = 'clean';

  return { sim, pipeline, critical, total, hasRun, stale, severity };
}
