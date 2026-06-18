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

/// True when running in the Android System WebView. Used to special-case the
/// SAF file picker: Android maps the dialog's extension filters to MIME types
/// and silently drops any extension with no registered MIME (.dxf, .ngc,
/// .plt, …), which makes those files unselectable. On Android we drop the
/// filters and content-sniff the bytes instead. UA sniffing is sufficient
/// here — the WebView UA always contains "Android".
export function isAndroid(): boolean {
  if (typeof navigator === 'undefined') return false;
  return /android/i.test(navigator.userAgent);
}
