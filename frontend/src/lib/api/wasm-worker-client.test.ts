import { describe, it, expect } from 'vitest';
import { WasmWorkerClient, toCloneable, type WorkerLike } from './wasm-worker-client';
import { CancelledError, type PipelineEvent } from './client';
import type { WorkerRequest, WorkerResponse } from './wasm-worker-protocol';
import type { GenerateRequest } from './types';

/// A scriptable stand-in for the real Web Worker: each posted request is
/// handed to `script`, which decides what response(s) to post back. The
/// async hop is modelled with `queueMicrotask` so ordering matches a real
/// worker (postMessage never resolves synchronously).
type Script = (req: WorkerRequest, post: (r: WorkerResponse) => void) => void;

class FakeWorker implements WorkerLike {
  onmessage: ((ev: MessageEvent) => void) | null = null;
  onerror: ((ev: unknown) => void) | null = null;
  terminated = false;
  constructor(private readonly script: Script) {}
  postMessage(message: unknown) {
    if (this.terminated) return;
    const req = message as WorkerRequest;
    const post = (r: WorkerResponse) => {
      if (this.terminated) return;
      this.onmessage?.({ data: r } as MessageEvent);
    };
    queueMicrotask(() => this.script(req, post));
  }
  terminate() {
    this.terminated = true;
  }
}

const REQ = {} as GenerateRequest;

describe('WasmWorkerClient', () => {
  it('round-trips a generate through the worker', async () => {
    const value = { gcode: 'G0 X0', stats: { offset_count: 1 } };
    const client = new WasmWorkerClient(
      () =>
        new FakeWorker((req, post) => {
          if (req.method === 'generate') post({ id: req.id, type: 'result', value });
        }),
    );
    await expect(client.generate(REQ)).resolves.toEqual(value);
  });

  it('relays streamed events in order, then resolves with the result', async () => {
    const result = { gcode: '', stats: { offset_count: 2 } };
    const client = new WasmWorkerClient(
      () =>
        new FakeWorker((req, post) => {
          post({
            id: req.id,
            type: 'event',
            event: { kind: 'op_started', op_id: 1, idx: 0, total: 2, name: 'a' },
          });
          post({
            id: req.id,
            type: 'event',
            event: { kind: 'done', op_count: 2, total_time_s: 0.1 },
          });
          post({ id: req.id, type: 'result', value: result });
        }),
    );
    const events: PipelineEvent[] = [];
    await expect(client.generateStreaming(REQ, (e) => events.push(e))).resolves.toEqual(result);
    expect(events.map((e) => e.kind)).toEqual(['op_started', 'done']);
  });

  it('aborting a streaming run terminates the worker and rejects with CancelledError', async () => {
    let spawned: FakeWorker | undefined;
    const ac = new AbortController();
    const client = new WasmWorkerClient(() => {
      // Emit one event, then never resolve — a blocked run the only way
      // out of which is termination.
      spawned = new FakeWorker((req, post) => {
        post({
          id: req.id,
          type: 'event',
          event: { kind: 'op_started', op_id: 1, idx: 0, total: 1, name: 'x' },
        });
      });
      return spawned;
    });
    const seen: PipelineEvent[] = [];
    const p = client.generateStreaming(REQ, (e) => seen.push(e), ac.signal);
    await Promise.resolve(); // let the queued event flush
    expect(seen).toHaveLength(1);
    ac.abort();
    await expect(p).rejects.toBeInstanceOf(CancelledError);
    expect(spawned?.terminated).toBe(true);
  });

  it('a pre-aborted signal rejects without dispatching', async () => {
    const ac = new AbortController();
    ac.abort();
    const client = new WasmWorkerClient(() => new FakeWorker(() => {}));
    await expect(client.generateStreaming(REQ, () => {}, ac.signal)).rejects.toBeInstanceOf(
      CancelledError,
    );
  });

  it('surfaces a worker error as a rejected Error', async () => {
    const client = new WasmWorkerClient(
      () => new FakeWorker((req, post) => post({ id: req.id, type: 'error', error: 'boom' })),
    );
    await expect(client.generate(REQ)).rejects.toThrow('boom');
  });

  it('correlates concurrent calls by id', async () => {
    const client = new WasmWorkerClient(
      () => new FakeWorker((req, post) => post({ id: req.id, type: 'result', value: req.method })),
    );
    const [a, b] = await Promise.all([client.health(), client.version()]);
    expect(a as unknown).toBe('health');
    expect(b as unknown).toBe('version');
  });

  it('respawns a fresh worker after dispose', async () => {
    let spawnCount = 0;
    const client = new WasmWorkerClient(() => {
      spawnCount++;
      return new FakeWorker((req, post) => post({ id: req.id, type: 'result', value: true }));
    });
    expect(spawnCount).toBe(1); // eager construction
    client.dispose();
    await expect(client.health()).resolves.toBe(true);
    expect(spawnCount).toBe(2); // respawned on next call
  });
});

describe('toCloneable (Proxy → structured-clone-safe)', () => {
  it('de-proxies a Svelte-$state-like Proxy into a plain deep clone', () => {
    const target = { a: 1, nested: { b: [2, 3] } };
    const proxy = new Proxy(target, {}); // stands in for a $state proxy
    const out = toCloneable(proxy) as typeof target;
    expect(out).toEqual(target);
    expect(out).not.toBe(proxy); // a fresh plain object, not the proxy
    expect(out.nested).not.toBe(target.nested); // deep clone
    // The clone must survive structured clone (postMessage uses it).
    expect(() => structuredClone(out)).not.toThrow();
  });

  it('passes binary through untouched so it can transfer', () => {
    const bytes = new Uint8Array([1, 2, 3]);
    expect(toCloneable(bytes)).toBe(bytes);
    const buf = bytes.buffer;
    expect(toCloneable(buf)).toBe(buf);
  });

  it('returns primitives as-is', () => {
    expect(toCloneable('dxf.dxf')).toBe('dxf.dxf');
    expect(toCloneable(42)).toBe(42);
    expect(toCloneable(null)).toBe(null);
    expect(toCloneable(undefined)).toBe(undefined);
  });
});
