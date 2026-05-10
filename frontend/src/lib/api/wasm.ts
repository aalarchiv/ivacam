// WASM implementation of WiacClient. Loads the wiac-wasm pkg lazily so it
// only ships when the user opts in via `?api=wasm`. Useful for offline
// demos and CI smoke tests; the same JSON contract the HTTP / Tauri
// transports speak.

import { CancelledError, type PipelineEvent, type ProgressEvent, type WiacClient } from './client';
import type {
  GenerateRequest,
  GenerateResponse,
  HelixRadiusRequest,
  HelixRadiusResponse,
  ImportResponse,
  RenderTextRequest,
  RenderTextResponse,
  VersionResponse,
} from './types';

type WasmModule = {
  default?: () => Promise<unknown>;
  healthz: () => { ok: boolean };
  version: () => VersionResponse;
  importBytes: (filename: string, bytes: Uint8Array) => ImportResponse;
  generate: (request: GenerateRequest) => GenerateResponse;
  generateStreaming?: (
    request: GenerateRequest,
    onEvent: (event: PipelineEvent) => void,
  ) => GenerateResponse | null;
  renderText: (request: RenderTextRequest) => RenderTextResponse;
  computeHelixRadius: (request: HelixRadiusRequest) => HelixRadiusResponse;
};

let modPromise: Promise<WasmModule> | null = null;

async function loadModule(): Promise<WasmModule> {
  if (!modPromise) {
    modPromise = (async () => {
      // The pkg is produced by `wasm-pack build crates/wiac-wasm --target web`
      // and lives under crates/wiac-wasm/pkg/. Vite resolves it relative to
      // the frontend root once the symlink (or pnpm linked dep) is in place.
      const wasm = (await import(/* @vite-ignore */ 'wiac-wasm')) as WasmModule;
      if (typeof wasm.default === 'function') {
        await wasm.default();
      }
      return wasm;
    })();
  }
  return modPromise;
}

export class WasmWiacClient implements WiacClient {
  async health(): Promise<boolean> {
    const m = await loadModule();
    return m.healthz().ok === true;
  }

  async version(): Promise<VersionResponse> {
    const m = await loadModule();
    return m.version();
  }

  async importFile(file: File): Promise<ImportResponse> {
    const m = await loadModule();
    const bytes = new Uint8Array(await file.arrayBuffer());
    return m.importBytes(file.name, bytes);
  }

  async generate(request: GenerateRequest): Promise<GenerateResponse> {
    const m = await loadModule();
    return m.generate(request);
  }

  async generateStream(
    request: GenerateRequest,
    onProgress: (e: ProgressEvent) => void,
  ): Promise<GenerateResponse> {
    onProgress({ phase: 'import', fraction: 0.05, message: 'in-browser core' });
    const r = await this.generate(request);
    onProgress({ phase: 'done', fraction: 1.0, message: 'complete' });
    return r;
  }

  /**
   * WASM v1 is single-threaded — the Rust call holds the JS event
   * loop, so the cancel signal cannot fire mid-run. We still emit the
   * per-op event stream so the progress UI updates between ops, and
   * yield with `await Promise.resolve()` between events would require
   * the Rust→JS bridge to suspend (it can't here). Cancel support
   * arrives with web-worker threading in v2.
   */
  async generateStreaming(
    request: GenerateRequest,
    onEvent: (event: PipelineEvent) => void,
    cancelToken?: AbortSignal,
  ): Promise<GenerateResponse> {
    if (cancelToken?.aborted) throw new CancelledError();
    const m = await loadModule();
    if (!m.generateStreaming) {
      const r = m.generate(request);
      onEvent({ kind: 'done', op_count: r.stats?.offset_count ?? 0, total_time_s: 0 });
      return r;
    }
    const buffered: PipelineEvent[] = [];
    const r = m.generateStreaming(request, (ev) => {
      buffered.push(ev);
    });
    for (const ev of buffered) onEvent(ev);
    if (r === null) {
      onEvent({ kind: 'cancelled' });
      throw new CancelledError();
    }
    return r;
  }

  async renderText(request: RenderTextRequest): Promise<RenderTextResponse> {
    const m = await loadModule();
    return m.renderText(request);
  }

  async computeHelixRadius(request: HelixRadiusRequest): Promise<HelixRadiusResponse> {
    const m = await loadModule();
    return m.computeHelixRadius(request);
  }
}
