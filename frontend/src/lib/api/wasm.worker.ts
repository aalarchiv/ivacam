// Web Worker host for the wasm CAM client. Runs the blocking
// pipeline OFF the main thread so the UI stays responsive, and streams
// per-op events back via postMessage as they are produced — postMessage
// enqueues fine even while this worker is synchronously busy inside the
// Rust call, so the main thread receives progress live.
//
// There is no in-worker cancel: the wasm call holds this worker's event
// loop, so a "cancel" message couldn't be read mid-run anyway. Real
// cancellation is the main thread calling `worker.terminate()` (see
// wasm-worker-client.ts), which kills the blocked run outright.
//
// The sim `Simulator` is NOT hosted here — it stays on the main thread
// (its per-frame zero-copy heightfield would otherwise need transferring
// every frame). Only the request→response pipeline calls cross over.

import type { WasmModule } from './wasm';
import type { WorkerRequest, WorkerResponse } from './wasm-worker-protocol';
import type {
  GenerateRequest,
  HelixRadiusRequest,
  RenderTextRequest,
  WireTextLayer,
} from './types';

// Minimal view of the dedicated-worker global. Declared locally rather
// than via `/// <reference lib="webworker" />` so this file doesn't pull
// the WebWorker lib into the DOM-typed program (which double-declares
// `self` / `postMessage` and fails the build).
interface WorkerScope {
  onmessage: ((ev: MessageEvent<WorkerRequest>) => void) | null;
  postMessage(message: unknown): void;
}
const ctx = self as unknown as WorkerScope;

let modPromise: Promise<WasmModule> | null = null;
function loadModule(): Promise<WasmModule> {
  if (!modPromise) {
    modPromise = (async () => {
      const wasm = (await import(/* @vite-ignore */ 'ivac-wasm')) as unknown as WasmModule;
      if (typeof wasm.default === 'function') {
        await wasm.default();
      }
      return wasm;
    })();
  }
  return modPromise;
}

function post(msg: WorkerResponse) {
  ctx.postMessage(msg);
}

function errText(err: unknown): string {
  if (err instanceof Error) return err.message;
  try {
    return String(err);
  } catch {
    return 'wasm worker error';
  }
}

ctx.onmessage = async (e: MessageEvent<WorkerRequest>) => {
  const { id, method, args } = e.data;
  try {
    const m = await loadModule();
    switch (method) {
      case 'health':
        post({ id, type: 'result', value: m.healthz().ok === true });
        break;
      case 'version':
        post({ id, type: 'result', value: m.version() });
        break;
      case 'importBytes':
        post({
          id,
          type: 'result',
          value: m.importBytes(args[0] as string, args[1] as Uint8Array),
        });
        break;
      case 'generate':
        post({ id, type: 'result', value: m.generate(args[0] as GenerateRequest) });
        break;
      case 'generateStreaming': {
        const req = args[0] as GenerateRequest;
        if (m.generateStreaming) {
          // The Rust call fires `on_event` synchronously per op; each
          // hop posts an event that reaches the (unblocked) main thread
          // live. A null return is the wasm's internal-cancel sentinel.
          const r = m.generateStreaming(req, (ev) => post({ id, type: 'event', event: ev }));
          if (r === null) post({ id, type: 'cancelled' });
          else post({ id, type: 'result', value: r });
        } else {
          const r = m.generate(req);
          post({
            id,
            type: 'event',
            event: { kind: 'done', op_count: r.stats?.offset_count ?? 0, total_time_s: 0 },
          });
          post({ id, type: 'result', value: r });
        }
        break;
      }
      case 'renderText':
        post({ id, type: 'result', value: m.renderText(args[0] as RenderTextRequest) });
        break;
      case 'renderTextLayer':
        post({ id, type: 'result', value: m.renderTextLayer(args[0] as WireTextLayer) });
        break;
      case 'computeHelixRadius':
        post({ id, type: 'result', value: m.computeHelixRadius(args[0] as HelixRadiusRequest) });
        break;
      default:
        post({ id, type: 'error', error: `unknown method: ${String(method)}` });
    }
  } catch (err) {
    post({ id, type: 'error', error: errText(err) });
  }
};
