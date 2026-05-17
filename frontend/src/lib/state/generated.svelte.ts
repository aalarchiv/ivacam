/// Generate-pipeline slice of ProjectState (audit 6cpl step 2).
/// Owns every field that's downstream of the CAM pipeline run plus
/// the lifecycle methods that mutate them.
///
/// Why a slice? Everything in here is:
///   * untouched by the undo/redo command bus (no commands.ts ops
///     mutate generate state)
///   * driven by a single producer (the pipeline streaming events),
///   * read by lots of components but written from one place,
/// — which makes it the cleanest first cut out of ProjectState's
/// 1500-line god class.
///
/// `ProjectState` retains `gen = new GeneratedState()` and exposes
/// proxy getters/setters for backwards compatibility so every
/// existing `project.generated`, `project.pipelineState`, etc.
/// call site keeps working unchanged.

import type { GenerateResponse, SimDiagnostics } from '../api/types';

export type PipelinePhase = 'idle' | 'running' | 'cancelling' | 'completed';

export interface PipelineProgress {
  opIdx: number;
  opTotal: number;
  opFraction: number;
  opName: string;
}

/// Pipeline streaming-event union accepted by `notePipelineEvent`.
/// Loose record fallback supports the richer wire payload the API
/// client emits (`op_id`, `total_time_s`, etc.) without re-narrowing
/// it here.
export type PipelineNoteEvent =
  | { kind: 'op_started'; idx: number; total: number; name: string }
  | { kind: 'op_progress'; fraction: number; message: string }
  | { kind: 'op_completed'; cached: boolean }
  | { kind: 'cancelled' }
  | { kind: 'done' }
  | (Record<string, unknown> & { kind: string });

export class GeneratedState {
  /// Most recent CAM pipeline result (gcode + toolpath + warnings).
  /// `null` between Generate runs and after a project reload.
  generated = $state<GenerateResponse | null>(null);

  /// Monotonic counter bumped on every `setGenerated` write. Scene3D's
  /// sim-rebuild key uses this instead of `generated.gcode.length` so
  /// two runs with identical gcode length but different content can't
  /// silently dedupe.
  generatedVersion = $state(0);

  /// True while the awaited generate promise is in flight. Drives the
  /// "Generate" button's busy state.
  generating = $state(false);

  /// Streaming pipeline lifecycle. Transitions:
  ///   idle → running (beginGenerate)
  ///   running → cancelling (cancelGenerate)
  ///   running | cancelling → completed (finishGenerate)
  ///   completed → idle (1 s later, via setTimeout)
  ///   any → idle (failGenerate, on ProjectState)
  pipelineState = $state<PipelinePhase>('idle');

  /// Latest per-op progress for the GenerateProgress card. Reset to
  /// null when `pipelineState` returns to idle.
  pipelineProgress = $state<PipelineProgress | null>(null);

  /// Stats from the most recent generate run — surfaced as "N of M
  /// cached" in the GenerateBar. Reset on every beginGenerate.
  lastGenerateCachedCount = $state<number>(0);
  lastGenerateOpCount = $state<number>(0);

  /// Per-segment cumulative-length lookup for the playhead → segment
  /// mapping. Built lazily from `generated.toolpath` after each
  /// successful run; `null` when there's no toolpath.
  toolpathCumLen = $state<Float64Array | null>(null);
  toolpathTotalLen = $state(0.0);

  /// Most recent sim diagnostics. Written through by the sim driver
  /// after each forward `advance()`. `null` when no sim has run (or
  /// the preview is wireframe-only).
  simDiagnostics = $state<SimDiagnostics | null>(null);

  /// Start a new pipeline run. Resets all transient state so the UI
  /// can't leak progress / cache counters from the prior run.
  beginGenerate(): void {
    this.generating = true;
    this.pipelineState = 'running';
    this.pipelineProgress = null;
    this.lastGenerateCachedCount = 0;
    this.lastGenerateOpCount = 0;
  }

  /// Apply a streaming pipeline event from the backend. `cached` and
  /// the per-op-progress arithmetic live here so the UI component
  /// stays render-only. `cancelled` / `done` collapse to no-ops on
  /// this slice — finishGenerate / failGenerate handle the lifecycle
  /// explicitly so this method can stay purely additive.
  notePipelineEvent(ev: PipelineNoteEvent): void {
    if (ev.kind === 'op_started') {
      // Project explicitly out of the loose-record fallback: an
      // op_started event from the pipeline always carries idx/total/
      // name; the Record<string, unknown> alternative is for richer
      // wire payloads we don't dispatch on here.
      const e = ev as { idx: number; total: number; name: string };
      this.pipelineProgress = {
        opIdx: e.idx,
        opTotal: e.total,
        opFraction: 0,
        opName: e.name,
      };
    } else if (ev.kind === 'op_progress') {
      if (this.pipelineProgress) {
        const e = ev as { fraction: number };
        this.pipelineProgress = {
          ...this.pipelineProgress,
          opFraction: e.fraction,
        };
      }
    } else if (ev.kind === 'op_completed') {
      this.lastGenerateOpCount += 1;
      if ((ev as { cached?: boolean }).cached) this.lastGenerateCachedCount += 1;
      if (this.pipelineProgress) {
        this.pipelineProgress = {
          ...this.pipelineProgress,
          opFraction: 1,
          opIdx: this.pipelineProgress.opIdx + 1,
        };
      }
    }
  }

  /// Successful pipeline completion. Briefly shows the `completed`
  /// state so the UI can flash the success indicator before reverting
  /// to idle.
  finishGenerate(): void {
    this.pipelineState = 'completed';
    setTimeout(() => {
      if (this.pipelineState === 'completed') this.pipelineState = 'idle';
    }, 1000);
  }

  /// Pipeline aborted by the user (Cancel button or AbortSignal).
  /// Moves through `cancelling` so the UI can show a transient state
  /// while the worker bails; the awaited promise will resolve to
  /// `endGenerate`.
  cancelGenerate(): void {
    if (this.pipelineState === 'running') {
      this.pipelineState = 'cancelling';
    }
  }

  /// Reset transient generate state regardless of how the run ended.
  /// Always pairs with `beginGenerate` so the UI doesn't leak a stale
  /// progress card after the awaited generate promise settles.
  endGenerate(): void {
    this.generating = false;
    this.pipelineProgress = null;
  }
}
