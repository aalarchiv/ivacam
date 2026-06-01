/// Coverage for `WiacClient.computeHelixRadius` across the HTTP transport.
/// The transport-agnostic interface lives in client.ts; here we mock
/// `fetch` so the test pins the wire shape (URL, method, body) and
/// confirms the response round-trips back to the caller.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { HttpWiacClient, resolveApiChoice } from './http';
import { tryParseStructuredError } from './client';
import type { HelixRadiusRequest, HelixRadiusResponse, WiacError } from './types';

describe('HttpWiacClient.computeHelixRadius', () => {
  const realFetch = globalThis.fetch;

  beforeEach(() => {
    globalThis.fetch = vi.fn();
  });

  afterEach(() => {
    globalThis.fetch = realFetch;
  });

  it('POSTs the request to /helix-radius and returns the parsed response', async () => {
    const req: HelixRadiusRequest = {
      segments: [],
      object_ids: [3, 5],
      tool_diameter_mm: 6,
    };
    const expected: HelixRadiusResponse = {
      radius_mm: 4.2,
      fallback_reason: null,
    };
    (globalThis.fetch as ReturnType<typeof vi.fn>).mockResolvedValueOnce(
      new Response(JSON.stringify(expected), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      }),
    );
    const client = new HttpWiacClient('http://example.test');
    const got = await client.computeHelixRadius(req);

    expect(globalThis.fetch).toHaveBeenCalledTimes(1);
    const [url, init] = (globalThis.fetch as ReturnType<typeof vi.fn>).mock.calls[0] as [
      string,
      RequestInit,
    ];
    expect(url).toBe('http://example.test/helix-radius');
    expect(init.method).toBe('POST');
    expect((init.headers as Record<string, string>)['content-type']).toBe('application/json');
    expect(JSON.parse(init.body as string)).toEqual(req);
    expect(got).toEqual(expected);
  });

  it('surfaces a fallback_reason when the backend declines to fit', async () => {
    const resp: HelixRadiusResponse = {
      radius_mm: null,
      fallback_reason: 'pocket too tight for tool',
    };
    (globalThis.fetch as ReturnType<typeof vi.fn>).mockResolvedValueOnce(
      new Response(JSON.stringify(resp), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      }),
    );
    const client = new HttpWiacClient('http://example.test');
    const got = await client.computeHelixRadius({
      segments: [],
      object_ids: [],
      tool_diameter_mm: 6,
    });
    expect(got.radius_mm).toBeNull();
    expect(got.fallback_reason).toBe('pocket too tight for tool');
  });

  it('throws when the server returns a non-2xx status', async () => {
    (globalThis.fetch as ReturnType<typeof vi.fn>).mockResolvedValueOnce(
      new Response(JSON.stringify({ error: 'bad input' }), {
        status: 400,
        headers: { 'content-type': 'application/json' },
      }),
    );
    const client = new HttpWiacClient('http://example.test');
    await expect(
      client.computeHelixRadius({ segments: [], object_ids: [], tool_diameter_mm: 6 }),
    ).rejects.toThrow(/helix-radius returned 400/);
  });

  // luf1: when the server returns the full structured `wiac_core::Error`
  // (post-luf1 envelope), the thrown Error.message is the JSON itself so
  // tryParseStructuredError() reconstructs every field — including
  // recovery_hint and auto_fix — for ErrorToast / GenerateBar to render.
  it('rethrows structured-error responses verbatim so the frontend recovers kind+hint+auto_fix', async () => {
    const wiac: WiacError = {
      kind: 'misconfigured',
      message: 'op 2 references missing tool 9',
      recovery_hint: 'Pick a tool from the library.',
      auto_fix: { kind: 'assign_tool', op_id: 2, suggested_tool_id: 1 },
    };
    (globalThis.fetch as ReturnType<typeof vi.fn>).mockResolvedValueOnce(
      new Response(JSON.stringify(wiac), {
        status: 400,
        headers: { 'content-type': 'application/json' },
      }),
    );
    const client = new HttpWiacClient('http://example.test');
    let captured: unknown;
    try {
      await client.computeHelixRadius({ segments: [], object_ids: [], tool_diameter_mm: 6 });
    } catch (e) {
      captured = e;
    }
    expect(captured).toBeInstanceOf(Error);
    const parsed = tryParseStructuredError((captured as Error).message);
    expect(parsed).toEqual(wiac);
  });
});

describe('resolveApiChoice (transport selection)', () => {
  const base = {
    hasTauri: false,
    envApi: undefined,
    queryApi: null,
    defaultWasm: false,
    serverUrl: 'http://localhost:8766',
  };

  it('Tauri shell wins over everything', () => {
    expect(
      resolveApiChoice({ ...base, hasTauri: true, envApi: 'wasm', queryApi: 'wasm' }),
    ).toEqual({ kind: 'tauri' });
  });

  it('VITE_WIAC_API URL is used when set', () => {
    expect(resolveApiChoice({ ...base, envApi: 'https://cam.example.com' })).toEqual({
      kind: 'http',
      url: 'https://cam.example.com',
    });
  });

  it('VITE_WIAC_API=wasm forces the in-browser engine', () => {
    expect(resolveApiChoice({ ...base, envApi: 'wasm' })).toEqual({ kind: 'wasm' });
  });

  it('?api=wasm forces the in-browser engine', () => {
    expect(resolveApiChoice({ ...base, queryApi: 'wasm' })).toEqual({ kind: 'wasm' });
  });

  it('?api=<url> points at an arbitrary server', () => {
    expect(resolveApiChoice({ ...base, queryApi: 'http://10.0.0.5:9000' })).toEqual({
      kind: 'http',
      url: 'http://10.0.0.5:9000',
    });
  });

  it('default in a production build → wasm (bare static deploy, no backend)', () => {
    expect(resolveApiChoice({ ...base, defaultWasm: true })).toEqual({ kind: 'wasm' });
  });

  it('default in dev → the local wiac-server', () => {
    expect(resolveApiChoice({ ...base, defaultWasm: false })).toEqual({
      kind: 'http',
      url: 'http://localhost:8766',
    });
  });

  it('an explicit VITE_WIAC_API URL overrides the production wasm default', () => {
    expect(
      resolveApiChoice({ ...base, envApi: 'https://cam.example.com', defaultWasm: true }),
    ).toEqual({ kind: 'http', url: 'https://cam.example.com' });
  });
});
