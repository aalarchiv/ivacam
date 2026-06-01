// 5ue0: message protocol shared by the wasm Web Worker host
// (`wasm.worker.ts`) and the main-thread client (`wasm-worker-client.ts`).
// Kept in its own module so both sides — and the unit test's fake worker
// — agree on the wire shape without the client importing the worker or
// vice-versa.

import type { PipelineEvent } from './client';

/// The `WiacClient` pipeline methods the worker can run. The sim
/// (`Simulator`) deliberately stays on the main thread, so it is NOT
/// here — only request→response pipeline calls cross the boundary.
export type WorkerMethod =
  | 'health'
  | 'version'
  | 'importBytes'
  | 'generate'
  | 'generateStreaming'
  | 'renderText'
  | 'renderTextLayer'
  | 'computeHelixRadius';

/// Main thread → worker. `id` correlates the eventual response(s);
/// `args` is the positional argument list for `method`.
export interface WorkerRequest {
  id: number;
  method: WorkerMethod;
  args: unknown[];
}

/// Worker → main thread. A streaming call emits zero or more `event`
/// messages before exactly one terminal `result` / `error` / `cancelled`.
export type WorkerResponse =
  | { id: number; type: 'event'; event: PipelineEvent }
  | { id: number; type: 'result'; value: unknown }
  | { id: number; type: 'error'; error: string }
  | { id: number; type: 'cancelled' };
