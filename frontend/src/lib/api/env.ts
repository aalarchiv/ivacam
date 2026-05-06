// Tiny standalone helper so callers can synchronously check whether they
// are running inside the Tauri shell without dragging the @tauri-apps/*
// import graph (and therefore the Tauri client) into the main chunk.
//
// Keep this file dependency-free — the bundler can then ship the heavy
// Tauri implementation as a separate dynamic chunk.

export function isTauri(): boolean {
  if (typeof window === 'undefined') return false;
  return typeof (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ !== 'undefined';
}
