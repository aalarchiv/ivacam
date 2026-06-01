// 5v1b: detect the in-browser wasm trial transport (`?api=wasm`) without
// pulling the full client module graph (http.ts + the transport classes)
// into whatever imports this. Kept tiny on purpose: the sim driver uses
// it to dial fidelity down so the single-threaded in-browser sim stays
// smooth, and that path must not drag the whole API layer into the sim
// chunk.
//
// `?api=wasm` is the *only* way the wasm transport is selected (see
// `defaultClient` in http.ts), so the query param is the definitive
// signal.
export function isWasmTransport(): boolean {
  if (typeof window === 'undefined') return false;
  try {
    return new URLSearchParams(window.location.search).get('api') === 'wasm';
  } catch {
    return false;
  }
}
