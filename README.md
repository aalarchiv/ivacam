# ivaCAM

**Turn DXF, SVG and HPGL drawings into G-code for CNC mills, lasers, plasma cutters and drag knives.**

Free, open-source CAM for the hobby shop. One self-contained app for desktop and Android — no Python, no install hell — plus an optional self-hosted web service and a fully in-browser (WebAssembly) mode where your drawings never leave your machine.

## Use it

**Just want to cut something?** → [Quickstart](./docs/QUICKSTART.md)

- **Desktop** (Linux / macOS / Windows): build the AppImage / `.msi` / `.app` — see [Build from source](#build-from-source).
- **Android**: build a native APK (Tauri mobile) and install it on the device — see [Build from source](#build-from-source).
- **Browser**: serve the static build and open the URL. Nothing to install; everything runs client-side.

## What it does

- **Import**: DXF, SVG, HPGL/PLT.
- **Operations**: profile, pocket, drill (canned cycles), V-carve, engrave/text, chamfer, thread, dovetail, T-slot — with tabs, lead-in/out, and tool offsets.
- **Machines**: mill, laser, plasma (pierce + dwell), drag knife — each with its own post-processor.
- **Preview**: 2D drawing canvas plus a live 3D toolpath and material-removal simulation.
- **Shop setup**: per-tool library, reusable machine profiles, project save/load.

## Build from source

```sh
cargo build --workspace                        # core + CLI + server
cd frontend && pnpm install && pnpm dev        # web UI on :5173
cargo tauri build --bundles appimage           # desktop bundle
cargo tauri android build --apk                # Android APK
```

Per-platform prerequisites and the full workflow: [docs/BUILDING.md](./docs/BUILDING.md) (Windows: [docs/BUILDING_WINDOWS.md](./docs/BUILDING_WINDOWS.md), Android: [docs/BUILDING.md § Android](./docs/BUILDING.md)).

## Docs

- [Quickstart](./docs/QUICKSTART.md) — drawing → G-code in 5 minutes
- [Building](./docs/BUILDING.md) — build & package every transport
- [Architecture](./docs/ARCHITECTURE.md) · [Contributing](./docs/CONTRIBUTING.md) — for hacking on it

## License

GPL-3.0-or-later — see [`LICENSE`](./LICENSE). ivaCAM inherits this licence from its partial derivation of viaConstructor (GPLv3); see Acknowledgements.

## Acknowledgements

Stands on excellent open-source libraries — [`dxf-rs`](https://github.com/IxMilia/dxf-rs), [`cavalier_contours`](https://github.com/jbuckmccready/cavalier_contours), `clipper2-rust`, [`usvg`](https://github.com/linebender/resvg), [Svelte](https://svelte.dev/), [Three.js](https://threejs.org/) and [Tauri](https://tauri.app/).

ivaCAM's CAM core is in part a Rust port of [viaConstructor](https://github.com/multigcs/viaconstructor)'s geometry and toolpath routines (`calc.py`, `machine_cmd.py`, `setupdefaults.py`), and is distributed under the GPL accordingly. [Estlcam](https://www.estlcam.de/) inspired parts of the feature set and terminology, but is closed-source and contributes no code.
