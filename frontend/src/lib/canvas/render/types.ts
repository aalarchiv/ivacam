/// Shared signature for the data→canvas projection closure produced by
/// `computeViewportTransform` (lib/canvas/viewport.ts). Render modules
/// take it as a parameter instead of reading component state, so they
/// stay pure and testable under vitest's node environment.
export type ProjectFn = (x: number, y: number) => [number, number];
