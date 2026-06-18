/// Lightweight runtime diagnostics surfaced on the About screen.
///
/// Built to debug the Android "file dialogs do nothing, and no error
/// toast appears" blocker (wiaconstructor-0gu0): on the phone layout the
/// ErrorToast may be off-screen, so a failing plugin call is invisible.
/// This module makes the relevant runtime state legible WITHOUT a cable —
/// which transport is active, whether the Tauri bridge is injected, and
/// the last error we surfaced — so a tester can read it off the device and
/// report back.
///
/// Intentionally dependency-light and side-effect-free except for the
/// module-level `lastError` latch written by `recordDiagnosticError`.

import { currentApiChoice } from '../api/http';
import { isTauri } from '../api/env';

let lastError: { message: string; at: string } | null = null;

/// Latch the most recent error surfaced to the user. Called from
/// `reportError` so the message is retained for the About readout even
/// after the (possibly hidden) toast is dismissed or auto-cleared.
export function recordDiagnosticError(message: string): void {
  lastError = { message, at: new Date().toLocaleTimeString() };
}

export interface RuntimeDiagnostics {
  /// `isTauri()` — the gate every native file op branches on.
  isTauri: boolean;
  /// Whether `window.__TAURI_INTERNALS__` is present (what `isTauri`
  /// actually keys off — injected by the shell regardless of
  /// `withGlobalTauri`).
  hasTauriInternals: boolean;
  /// Whether the `window.__TAURI__` global is present (only with
  /// `withGlobalTauri: true`; absent here by config).
  hasGlobalTauri: boolean;
  /// The transport the client resolves to: `tauri` / `wasm` / `http(url)`.
  transport: string;
  /// Navigator UA — confirms this is the Android System WebView.
  userAgent: string;
  /// Most recent error surfaced via `reportError`, or `null`.
  lastError: { message: string; at: string } | null;
}

/// Snapshot the current runtime signals for display. Pure read — call it
/// whenever the About panel renders.
export function runtimeDiagnostics(): RuntimeDiagnostics {
  const w =
    typeof window !== 'undefined' ? (window as unknown as Record<string, unknown>) : undefined;
  const choice = currentApiChoice();
  return {
    isTauri: isTauri(),
    hasTauriInternals: !!w && typeof w.__TAURI_INTERNALS__ !== 'undefined',
    hasGlobalTauri: !!w && typeof w.__TAURI__ !== 'undefined',
    transport: choice.kind === 'http' ? `http (${choice.url})` : choice.kind,
    userAgent: typeof navigator !== 'undefined' ? navigator.userAgent : '',
    lastError,
  };
}
