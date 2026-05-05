# wiaConstructor

A CAM tool that converts DXF, SVG, HPGL and other vector formats into G-code for
CNC milling, laser cutting and drag-knife cutting.

This project is a Rust + web rewrite of the original Python
[viaConstructor](https://github.com/multigcs/viaconstructor) by Oliver Dippel
and contributors. It targets a single-binary desktop app (via
[Tauri](https://tauri.app/)) plus an optional self-hosted web service and a
fully client-side WebAssembly mode.

## Status

Pre-alpha. Bootstrapping the project skeleton; no functional code yet. See the
issue tracker (`bd ready`) for current work.

## Why a rewrite

- Native Wayland support: the original uses a Qt4-era OpenGL widget and
  immediate-mode GL that fail to acquire surfaces under Wayland compositors.
  A WebGL2 / Three.js renderer fixes this once and runs everywhere.
- No Python on the user's machine: the upstream ships per-platform prebuilt
  `.so` files pinned to specific CPython ABIs, which is brittle. A Rust core
  produces one static binary per OS.
- Cross-platform: web (browser-only via WASM), self-hosted server, and desktop
  (Linux / macOS / Windows / mobile webview) from a single codebase.

## Architecture

```
wiac-core      Rust library: DXF/SVG/... import, CAM math, gcode generation
wiac-cli       Rust binary: headless converter (file in → gcode out)
wiac-server    Rust binary: axum HTTP server exposing the JSON contract
wiac-tauri     Rust binary: desktop shell (Tauri 2 + WebKitGTK/WebView2/WKWebView)
wiac-wasm      Rust cdylib: browser bindings via wasm-bindgen
frontend/      TypeScript / Svelte / Vite web frontend (Three.js renderer)
```

A single OpenAPI contract describes the operations (`/import`, `/generate`,
`/defaults`, …). The frontend speaks to whichever transport is available:
HTTP for server mode, Tauri commands for desktop, in-process WASM for
browser-only.

## Building from source

See [`BUILDING.md`](./BUILDING.md) for prerequisites per platform and the
full clone → cargo → tauri workflow. Short version:

```sh
cargo build --workspace          # core + CLI + server
cd frontend && npm install && npm run dev   # web UI on :5173
cd crates/wiac-tauri && cargo tauri build   # desktop bundle
```

## License

GPL-3.0-or-later. See `LICENSE`. This project is a derivative work of
viaConstructor (also GPL-3.0-or-later); upstream copyright notices are
preserved in any files ported from the original.

## Acknowledgements

- Oliver Dippel / `multigcs` and contributors — original viaConstructor
- Brett Forsgren / `IxMilia` — `dxf-rs` crate (DXF parser)
- Jed Buckley / `jbuckmccready` — `cavalier_contours` (polyline-with-arcs offsetting)
- Angus Johnson — Clipper2 (polygon offsetting)
