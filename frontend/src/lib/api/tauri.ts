// Tauri implementation of WiacClient. The desktop app is detected by the
// `__TAURI_INTERNALS__` global Tauri injects into the WebView; when absent
// we use the HTTP client instead. Methods proxy through `invoke` to the
// Rust commands defined in crates/wiac-tauri/src/commands.rs.

import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

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

  /**
   * Per-op streaming with cancellation. The Rust side spawns a worker
   * thread, returns immediately with a token id, and emits per-op
   * events on `generate-event:<token>`. The terminal frame lands on
   * `generate-result:<token>` (success), `generate-cancelled:<token>`
   * (cancellation), or `generate-error:<token>` (pipeline error).
   */
  async generateStreaming(
    request: GenerateRequest,
    onEvent: (event: PipelineEvent) => void,
    cancelToken?: AbortSignal,
  ): Promise<GenerateResponse> {
    const { token_id } = await invoke<{ token_id: number }>('generate_streaming_cmd', { request });

    let abortHandler: (() => void) | undefined;
    const unlistens: UnlistenFn[] = [];
    let resolve!: (resp: GenerateResponse) => void;
    let reject!: (err: unknown) => void;
    const result = new Promise<GenerateResponse>((res, rej) => {
      resolve = res;
      reject = rej;
    });

    const cleanup = () => {
      for (const u of unlistens) {
        try {
          u();
        } catch {
          // Best-effort.
        }
      }
      if (cancelToken && abortHandler) {
        cancelToken.removeEventListener('abort', abortHandler);
      }
    };

    // Register all four listeners and AWAIT their registration before
    // signaling the backend worker to start. The backend's
    // `generate_streaming_cmd` already spawned the worker but parked
    // it on a oneshot channel pending the ready handshake we send
    // below — without this gate, a fast pipeline (empty / 1-op
    // project) can emit `generate-result:<token>` before the FE's
    // listener for that event has finished registering, dropping
    // the terminal event and hanging the Generate UI in 'running'
    // (or 'cancelling' if the user clicks Cancel).
    const [u1, u2, u3, u4] = await Promise.all([
      listen<PipelineEvent>(`generate-event:${token_id}`, (msg) => {
        onEvent(msg.payload);
      }),
      listen<GenerateResponse>(`generate-result:${token_id}`, (msg) => {
        cleanup();
        resolve(msg.payload);
      }),
      listen<number>(`generate-cancelled:${token_id}`, () => {
        cleanup();
        onEvent({ kind: 'cancelled' });
        reject(new CancelledError());
      }),
      listen<string>(`generate-error:${token_id}`, (msg) => {
        cleanup();
        reject(new Error(msg.payload));
      }),
    ]);
    unlistens.push(u1, u2, u3, u4);

    if (cancelToken) {
      if (cancelToken.aborted) {
        void invoke('cancel_generate', { tokenId: token_id });
      } else {
        abortHandler = () => {
          void invoke('cancel_generate', { tokenId: token_id });
        };
        cancelToken.addEventListener('abort', abortHandler);
      }
    }

    // Listeners are live — tell the backend worker to proceed. If this
    // call fails (e.g. the command name isn't registered on an older
    // backend), the worker's 2-second timeout still lets the pipeline
    // run; the race is just not fixed in that case.
    try {
      await invoke('generate_streaming_ready_cmd', { tokenId: token_id });
    } catch {
      // Older backend without the ready handshake — fall through.
    }

    return result;
  }

  async renderText(request: RenderTextRequest): Promise<RenderTextResponse> {
    return invoke<RenderTextResponse>('render_text', { request });
  }

  async computeHelixRadius(request: HelixRadiusRequest): Promise<HelixRadiusResponse> {
    return invoke<HelixRadiusResponse>('compute_helix_radius_cmd', { req: request });
  }
}

/// Replace the active source-file watch set on the desktop shell. The
/// backend (crates/wiac-tauri/src/watcher.rs) emits `source-file-changed`
/// events whenever any of the supplied paths is rewritten.
export async function watchSourcePaths(paths: string[]): Promise<void> {
  await invoke('watch_source_paths', { paths });
}

/// Drop every watch slot. Called on project close.
export async function unwatchAll(): Promise<void> {
  await invoke('unwatch_all');
}

export interface SourceFileChangedPayload {
  path: string;
}

/// Subscribe to backend "source rewritten" notifications. Returns the
/// unlisten fn so callers can drop the subscription on project close.
export async function onSourceFileChanged(
  handler: (payload: SourceFileChangedPayload) => void,
): Promise<UnlistenFn> {
  return listen<SourceFileChangedPayload>('source-file-changed', (e) => handler(e.payload));
}
