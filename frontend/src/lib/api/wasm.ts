// WASM implementation of WiacClient. Loads the wiac-wasm pkg lazily so it
// only ships when the user opts in via `?api=wasm`. Useful for offline
// demos and CI smoke tests; the same JSON contract the HTTP / Tauri
// transports speak.

import type { DefaultsResponse, ProgressEvent, WiacClient } from './client';
import type {
  GenerateRequest,
  GenerateResponse,
  ImportResponse,
  VersionResponse,
} from './types';

type WasmModule = {
  default?: () => Promise<unknown>;
  healthz: () => { ok: boolean };
  version: () => VersionResponse;
  importBytes: (filename: string, bytes: Uint8Array) => ImportResponse;
  generate: (request: GenerateRequest) => GenerateResponse;
  defaults: () => DefaultsResponse;
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

  async defaults(): Promise<DefaultsResponse> {
    const m = await loadModule();
    return m.defaults();
  }
}
