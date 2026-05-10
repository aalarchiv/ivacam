// Transport-agnostic client interface. Implementations: HTTP (`http.ts`,
// talks to wiac-server), Tauri (`tauri.ts`, native invoke), and WASM
// (`wasm.ts`, runs the CAM pipeline in-browser).

import type {
  GenerateRequest,
  GenerateResponse,
  ImportResponse,
  RenderTextRequest,
  RenderTextResponse,
  VersionResponse,
  WiacError,
} from './types';

export interface WiacClient {
  health(): Promise<boolean>;
  version(): Promise<VersionResponse>;
  importFile(file: File, format?: string): Promise<ImportResponse>;
  generate(request: GenerateRequest): Promise<GenerateResponse>;
  /**
   * Streaming variant: emits {phase, fraction, message} via the supplied
   * onProgress callback as the pipeline advances, returning the same
   * GenerateResponse the non-streaming `generate()` would. Falls back
   * to the non-streaming endpoint on transports that don't support
   * streaming (Tauri / WASM emit a synthetic start+done pair).
   */
  generateStream?(
    request: GenerateRequest,
    onProgress: (e: ProgressEvent) => void,
  ): Promise<GenerateResponse>;
  /**
   * Per-op streaming variant with cooperative cancellation. Emits a
   * PipelineEvent for every op boundary (started / completed) plus a
   * final Done. Pass an `AbortSignal` and abort it to flip the shared
   * cancel flag — the pipeline bails within ≤200 ms p95 and rejects
   * with a CancelledError. Implementations that don't support
   * cancellation (WASM v1) ignore the signal but still emit the
   * per-op event stream.
   */
  generateStreaming?(
    request: GenerateRequest,
    onEvent: (event: PipelineEvent) => void,
    cancelToken?: AbortSignal,
  ): Promise<GenerateResponse>;
  /**
   * Render TTF font + string → segments. Used by the AddTextDialog to
   * stage geometry before adding it to the project.
   */
  renderText(request: RenderTextRequest): Promise<RenderTextResponse>;
}

export interface ProgressEvent {
  phase: string;
  fraction: number;
  message: string;
}

export type PipelineEvent =
  | { kind: 'op_started'; op_id: number; idx: number; total: number; name: string }
  | { kind: 'op_progress'; op_id: number; fraction: number; message: string }
  | { kind: 'op_completed'; op_id: number }
  | { kind: 'cancelled' }
  | { kind: 'done'; op_count: number; total_time_s: number };

export class CancelledError extends Error {
  constructor() {
    super('pipeline cancelled');
    this.name = 'CancelledError';
  }
}

/// Best-effort parser for a structured `WiacError` JSON payload that the
/// Tauri/WASM transports stuff into Error.message. Returns the parsed
/// object when the input looks like a `WiacError` (has `kind` + `message`
/// and a known `kind` value), or null when it doesn't — callers should
/// then fall back to the plain string message.
export function tryParseStructuredError(input: unknown): WiacError | null {
  let candidate: unknown = input;
  if (typeof candidate === 'string') {
    const trimmed = candidate.trim();
    if (trimmed.length === 0 || trimmed[0] !== '{') return null;
    try {
      candidate = JSON.parse(trimmed);
    } catch {
      return null;
    }
  }
  if (candidate == null || typeof candidate !== 'object') return null;
  const obj = candidate as Record<string, unknown>;
  if (typeof obj.kind !== 'string' || typeof obj.message !== 'string') return null;
  const knownKinds = new Set([
    'bad_input',
    'misconfigured',
    'limit',
    'unsupported',
    'io',
    'internal',
  ]);
  if (!knownKinds.has(obj.kind)) return null;
  return obj as unknown as WiacError;
}
