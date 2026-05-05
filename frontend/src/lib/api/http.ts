// HTTP implementation of WiacClient. Talks to whichever server is wired
// up — Stage-1 Python FastAPI now, Rust axum later. Same shape.

import type { WiacClient } from './client';
import type {
  GenerateRequest,
  GenerateResponse,
  ImportResponse,
  VersionResponse,
} from './types';

export class HttpWiacClient implements WiacClient {
  constructor(private readonly base: string) {}

  async health(): Promise<boolean> {
    const res = await fetch(`${this.base}/healthz`);
    if (!res.ok) return false;
    const body = (await res.json()) as { ok?: boolean };
    return body.ok === true;
  }

  async version(): Promise<VersionResponse> {
    const res = await fetch(`${this.base}/version`);
    if (!res.ok) throw new Error(`/version returned ${res.status}`);
    return (await res.json()) as VersionResponse;
  }

  async importFile(file: File, format?: string): Promise<ImportResponse> {
    const form = new FormData();
    form.append('file', file);
    if (format) form.append('format', format);
    const res = await fetch(`${this.base}/import`, { method: 'POST', body: form });
    if (!res.ok) {
      let detail: unknown;
      try {
        detail = await res.json();
      } catch {
        detail = await res.text();
      }
      throw new Error(`/import returned ${res.status}: ${JSON.stringify(detail)}`);
    }
    return (await res.json()) as ImportResponse;
  }

  async generate(request: GenerateRequest): Promise<GenerateResponse> {
    const res = await fetch(`${this.base}/generate`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(request),
    });
    if (!res.ok) {
      let detail: unknown;
      try {
        detail = await res.json();
      } catch {
        detail = await res.text();
      }
      throw new Error(`/generate returned ${res.status}: ${JSON.stringify(detail)}`);
    }
    return (await res.json()) as GenerateResponse;
  }
}

export function defaultClient(): WiacClient {
  // Resolution order:
  //   1. VITE_WIAC_API at build time
  //   2. ?api=… query param at runtime (handy for demos)
  //   3. http://<host>:8765 — talks to the Stage-1 bridge directly
  const fromEnv = import.meta.env.VITE_WIAC_API as string | undefined;
  if (fromEnv) return new HttpWiacClient(fromEnv);

  if (typeof window !== 'undefined') {
    const params = new URLSearchParams(window.location.search);
    const fromQuery = params.get('api');
    if (fromQuery) return new HttpWiacClient(fromQuery);

    const { protocol, hostname } = window.location;
    // Default to the Rust server (port 8766). Stage-1 Python bridge on 8765
    // remains available via ?api=http://host:8765 for comparison.
    return new HttpWiacClient(`${protocol}//${hostname}:8766`);
  }

  return new HttpWiacClient('http://127.0.0.1:8765');
}
