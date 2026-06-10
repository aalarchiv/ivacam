// `WiacClient` that runs the wasm CAM pipeline in a Web Worker, so
// a heavy generate no longer blocks the UI thread, and a long run can be
// cancelled for real. It speaks the same JSON contract as the HTTP /
// Tauri / direct-wasm transports; `http.ts` prefers it for `?api=wasm`
// and falls back to the main-thread `WasmWiacClient` where module
// workers aren't available.
//
// Cancellation: the wasm call is synchronous inside the worker, so a
// "cancel" message couldn't be processed mid-run. Aborting the supplied
// signal therefore TERMINATES the worker (killing the blocked run) and
// rejects with `CancelledError`; the next call transparently respawns a
// fresh worker. This is the design's "hard cancel".

import { CancelledError, type PipelineEvent, type ProgressEvent, type WiacClient } from './client';
import type {
  GenerateRequest,
  GenerateResponse,
  HelixRadiusRequest,
  HelixRadiusResponse,
  ImportResponse,
  RenderTextRequest,
  RenderTextResponse,
  RenderTextLayerResponse,
  WireTextLayer,
  VersionResponse,
} from './types';
import type { WorkerMethod, WorkerRequest, WorkerResponse } from './wasm-worker-protocol';

/// The slice of the DOM `Worker` API this client uses. Narrowed to an
/// interface so unit tests can inject a fake without a real worker / wasm.
export interface WorkerLike {
  postMessage(message: unknown, transfer?: Transferable[]): void;
  terminate(): void;
  onmessage: ((ev: MessageEvent) => void) | null;
  onerror: ((ev: unknown) => void) | null;
}

export type WorkerFactory = () => WorkerLike;

/// Real factory: a Vite-bundled module worker. The `new URL(...,
/// import.meta.url)` form keeps the worker (and the wasm it lazily
/// imports) out of the main bundle until `?api=wasm` instantiates this.
function defaultFactory(): WorkerLike {
  return new Worker(new URL('./wasm.worker.ts', import.meta.url), {
    type: 'module',
  }) as unknown as WorkerLike;
}

interface Pending {
  resolve: (value: unknown) => void;
  reject: (err: unknown) => void;
  onEvent?: (ev: PipelineEvent) => void;
  cleanup?: () => void;
}

/// Make a worker argument structured-clone-safe. Request payloads are
/// built from Svelte 5 `$state`, i.e. Proxy objects — `postMessage`'s
/// structured clone throws "Proxy object could not be cloned" on those.
/// JSON round-trip de-proxies plain data (the requests are already pure
/// JSON — it's exactly what the HTTP transport serializes), while binary
/// (the import `Uint8Array`) passes through untouched so it can transfer.
/// Primitives are returned as-is.
export function toCloneable(arg: unknown): unknown {
  if (arg == null || typeof arg !== 'object') return arg;
  if (arg instanceof ArrayBuffer || ArrayBuffer.isView(arg)) return arg;
  return JSON.parse(JSON.stringify(arg)) as unknown;
}

export class WasmWorkerClient implements WiacClient {
  private readonly factory: WorkerFactory;
  private worker: WorkerLike | null = null;
  private readonly pending = new Map<number, Pending>();
  private nextId = 1;

  constructor(factory: WorkerFactory = defaultFactory) {
    this.factory = factory;
    // Spawn eagerly so a construction failure (no module-worker support)
    // surfaces synchronously and `http.ts` can fall back.
    this.ensureWorker();
  }

  private ensureWorker(): WorkerLike {
    if (!this.worker) {
      const w = this.factory();
      w.onmessage = (ev: MessageEvent) => this.onMessage((ev as MessageEvent<WorkerResponse>).data);
      w.onerror = () => this.failAll(new Error('wasm worker crashed'));
      this.worker = w;
    }
    return this.worker;
  }

  private onMessage(msg: WorkerResponse) {
    const entry = this.pending.get(msg.id);
    if (!entry) return;
    switch (msg.type) {
      case 'event':
        entry.onEvent?.(msg.event);
        break;
      case 'result':
        this.settle(msg.id, () => entry.resolve(msg.value));
        break;
      case 'error':
        this.settle(msg.id, () => entry.reject(new Error(msg.error)));
        break;
      case 'cancelled':
        this.settle(msg.id, () => entry.reject(new CancelledError()));
        break;
    }
  }

  private settle(id: number, run: () => void) {
    const entry = this.pending.get(id);
    if (!entry) return;
    entry.cleanup?.();
    this.pending.delete(id);
    run();
  }

  /// Hard cancel: kill the (blocked) worker and reject every in-flight
  /// call. The next call respawns a fresh worker via `ensureWorker`.
  private cancelHard() {
    const dead = this.worker;
    this.worker = null;
    if (dead) {
      try {
        dead.terminate();
      } catch {
        /* already gone */
      }
    }
    this.rejectAll(() => new CancelledError());
  }

  /// Worker error: reject every in-flight call and drop the worker so
  /// the next call respawns.
  private failAll(err: Error) {
    this.worker = null;
    this.rejectAll(() => err);
  }

  private rejectAll(makeErr: () => unknown) {
    const entries = [...this.pending.values()];
    this.pending.clear();
    for (const e of entries) {
      e.cleanup?.();
      e.reject(makeErr());
    }
  }

  private call<T>(
    method: WorkerMethod,
    args: unknown[],
    opts: {
      onEvent?: (ev: PipelineEvent) => void;
      signal?: AbortSignal;
      transfer?: Transferable[];
    } = {},
  ): Promise<T> {
    const worker = this.ensureWorker();
    const id = this.nextId++;
    return new Promise<T>((resolve, reject) => {
      if (opts.signal?.aborted) {
        reject(new CancelledError());
        return;
      }
      const entry: Pending = {
        resolve: resolve as (value: unknown) => void,
        reject,
        onEvent: opts.onEvent,
      };
      if (opts.signal) {
        const signal = opts.signal;
        const onAbort = () => this.cancelHard();
        signal.addEventListener('abort', onAbort, { once: true });
        entry.cleanup = () => signal.removeEventListener('abort', onAbort);
      }
      this.pending.set(id, entry);
      const req: WorkerRequest = { id, method, args: args.map(toCloneable) };
      worker.postMessage(req, opts.transfer ?? []);
    });
  }

  health(): Promise<boolean> {
    return this.call('health', []);
  }

  version(): Promise<VersionResponse> {
    return this.call('version', []);
  }

  async importFile(file: File): Promise<ImportResponse> {
    const buf = await file.arrayBuffer();
    // Transfer (not copy) the file bytes into the worker.
    return this.call('importBytes', [file.name, new Uint8Array(buf)], { transfer: [buf] });
  }

  generate(request: GenerateRequest): Promise<GenerateResponse> {
    return this.call('generate', [request]);
  }

  async generateStream(
    request: GenerateRequest,
    onProgress: (e: ProgressEvent) => void,
  ): Promise<GenerateResponse> {
    onProgress({ phase: 'import', fraction: 0.05, message: 'in-browser core (worker)' });
    const r = await this.generate(request);
    onProgress({ phase: 'done', fraction: 1.0, message: 'complete' });
    return r;
  }

  generateStreaming(
    request: GenerateRequest,
    onEvent: (event: PipelineEvent) => void,
    cancelToken?: AbortSignal,
  ): Promise<GenerateResponse> {
    return this.call('generateStreaming', [request], { onEvent, signal: cancelToken });
  }

  renderText(request: RenderTextRequest): Promise<RenderTextResponse> {
    return this.call('renderText', [request]);
  }

  renderTextLayer(layer: WireTextLayer): Promise<RenderTextLayerResponse> {
    return this.call('renderTextLayer', [layer]);
  }

  computeHelixRadius(request: HelixRadiusRequest): Promise<HelixRadiusResponse> {
    return this.call('computeHelixRadius', [request]);
  }

  /// Explicit teardown (not part of `WiacClient`) — terminate the worker
  /// and reject anything still pending. Safe to call more than once.
  dispose() {
    this.cancelHard();
  }
}
