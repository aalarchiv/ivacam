/// Coverage for `WiacClient.computeHelixRadius` across the HTTP transport.
/// The transport-agnostic interface lives in client.ts; here we mock
/// `fetch` so the test pins the wire shape (URL, method, body) and
/// confirms the response round-trips back to the caller.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { HttpWiacClient } from './http';
import type { HelixRadiusRequest, HelixRadiusResponse } from './types';

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
});
