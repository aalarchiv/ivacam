/// Recording stub for CanvasRenderingContext2D so render modules can be
/// exercised under vitest's node environment (no real canvas). Records
/// every method call + the style values in effect at call time; property
/// writes are kept so painters can read back what they set.
export interface CtxCall {
  method: string;
  args: unknown[];
  strokeStyle: unknown;
  fillStyle: unknown;
  lineWidth: number;
  globalAlpha: number;
}

export interface StubCtx {
  calls: CtxCall[];
  /// Convenience: the recorded calls for one method name.
  ops(method: string): CtxCall[];
  ctx: CanvasRenderingContext2D;
}

export function stubCtx(): StubCtx {
  const calls: CtxCall[] = [];
  const state = {
    strokeStyle: '' as unknown,
    fillStyle: '' as unknown,
    lineWidth: 0,
    globalAlpha: 1,
    imageSmoothingEnabled: true,
  };
  const target: Record<string | symbol, unknown> = {};
  const proxy = new Proxy(target, {
    get(_t, prop) {
      if (prop in state) return state[prop as keyof typeof state];
      return (...args: unknown[]) => {
        calls.push({
          method: String(prop),
          args,
          strokeStyle: state.strokeStyle,
          fillStyle: state.fillStyle,
          lineWidth: state.lineWidth,
          globalAlpha: state.globalAlpha,
        });
        return undefined;
      };
    },
    set(_t, prop, value) {
      (state as Record<string | symbol, unknown>)[prop] = value;
      return true;
    },
  });
  return {
    calls,
    ops: (method: string) => calls.filter((c) => c.method === method),
    ctx: proxy as unknown as CanvasRenderingContext2D,
  };
}

/// Identity-ish projection used across render tests: data → canvas with
/// scale 1 and a y-flip (canvas grows downward).
export function flipY(x: number, y: number): [number, number] {
  return [x, -y];
}
