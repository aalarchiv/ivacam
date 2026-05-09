// Tauri implementation of WiacClient. The desktop app is detected by the
// `__TAURI_INTERNALS__` global Tauri injects into the WebView; when absent
// we use the HTTP client instead. Methods proxy through `invoke` to the
// Rust commands defined in crates/wiac-tauri/src/commands.rs.

import { invoke } from '@tauri-apps/api/core';

import type { ProgressEvent, WiacClient } from './client';
import type {
  GenerateRequest,
  GenerateResponse,
  ImportResponse,
  RenderTextRequest,
  RenderTextResponse,
  VersionResponse,
} from './types';

// `isTauri()` lives in ./env.ts so callers can detect the shell without
// dragging in @tauri-apps/* (this file is meant to be code-split into its
// own chunk).

export class TauriWiacClient implements WiacClient {
  async health(): Promise<boolean> {
    const r = await invoke<{ ok: boolean }>('healthz');
    return r.ok === true;
  }

  async version(): Promise<VersionResponse> {
    return invoke<VersionResponse>('version');
  }

  /**
   * Tauri can take a real OS path. The web `File` object doesn't carry one,
   * so we fall back to writing a tempfile from the buffer when necessary.
   * Most call sites should use `importFromPath` directly when running
   * inside Tauri (gated by `isTauri()`).
   */
  async importFile(file: File, _format?: string): Promise<ImportResponse> {
    // Prefer the path attribute when present (e.g. drag-and-drop in Tauri
    // surfaces the absolute path on `File.path` in some setups). Fallback:
    // write an ArrayBuffer to a temp file via tauri-plugin-fs and import.
    const path = (file as File & { path?: string }).path;
    if (typeof path === 'string' && path.length > 0) {
      return this.importFromPath(path);
    }
    const { tempDir } = await import('@tauri-apps/api/path');
    const { writeFile } = await import('@tauri-apps/plugin-fs');
    const { join } = await import('@tauri-apps/api/path');
    const dir = await tempDir();
    const fname = `wiac-${Date.now()}-${file.name}`;
    const fullpath = await join(dir, fname);
    const data = new Uint8Array(await file.arrayBuffer());
    await writeFile(fullpath, data);
    return this.importFromPath(fullpath);
  }

  importFromPath(path: string): Promise<ImportResponse> {
    return invoke<ImportResponse>('import_path', { path });
  }

  async generate(request: GenerateRequest): Promise<GenerateResponse> {
    return invoke<GenerateResponse>('generate', { request });
  }

  // Tauri doesn't have HTTP-style streaming; the work happens in-process
  // and is fast enough that we synthesize start/done events around the
  // single invoke call. Frontend code uses the same callback signature.
  async generateStream(
    request: GenerateRequest,
    onProgress: (e: ProgressEvent) => void,
  ): Promise<GenerateResponse> {
    onProgress({ phase: 'import', fraction: 0.05, message: 'sending to native core' });
    const r = await invoke<GenerateResponse>('generate', { request });
    onProgress({ phase: 'done', fraction: 1.0, message: 'complete' });
    return r;
  }

  async renderText(request: RenderTextRequest): Promise<RenderTextResponse> {
    return invoke<RenderTextResponse>('render_text', { request });
  }
}
