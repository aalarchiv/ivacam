# Building ivaCAM from source

Targeted at users who'd rather not run a prebuilt binary they didn't
compile themselves. The project is a Cargo workspace plus a Vite frontend;
the desktop bundle is produced by Tauri 2.

## 1. Prerequisites

| Tool        | Version          | Notes                                          |
|-------------|------------------|------------------------------------------------|
| Rust        | pinned in [`rust-toolchain.toml`](../rust-toolchain.toml) | rustup picks up the pin automatically the first time you run cargo in this repo. |
| Node.js     | ≥ 20             | Any LTS works; we use it only to drive Vite + svelte-check. |
| pnpm        | ≥ 10             | Required. The frontend pulls in the local `ivac-wasm` package via pnpm's `link:` protocol, which npm can't resolve; lockfile is `frontend/pnpm-lock.yaml`. |
| Tauri CLI   | 2.x              | Install via `cargo install tauri-cli --version "^2" --locked` once you're set up for desktop builds. |
| wasm-pack   | ≥ 0.14           | Install via `cargo install wasm-pack --locked` for the WASM crate. |
| cargo-deny  | ≥ 0.19           | Optional but recommended; CI runs it. `cargo install cargo-deny --locked`. |
| sccache     | ≥ 0.15           | Required — `.cargo/config.toml` sets it as `rustc-wrapper` (compile cache; big win after `cargo clean` / across worktrees). `cargo install sccache --locked`, or delete the `[build]` block from `.cargo/config.toml` to opt out. |

### Linux (Debian/Ubuntu)

```sh
sudo apt-get install -y \
    build-essential pkg-config libssl-dev \
    libgtk-3-dev libwebkit2gtk-4.1-dev libayatana-appindicator3-dev \
    librsvg2-dev libfreetype6-dev
```

`webkit2gtk-4.1` is the version Tauri 2 expects; on Ubuntu 22.04 you may
need the `webkit2gtk-4.0-dev` package instead. Either way, the Tauri 2
release notes are authoritative if your distro lags.

### Linux (Fedora/RHEL)

```sh
sudo dnf install -y \
    @development-tools openssl-devel \
    gtk3-devel webkit2gtk4.1-devel libappindicator-gtk3-devel \
    librsvg2-devel freetype-devel
```

### macOS

```sh
xcode-select --install   # CommandLine Tools
```

That's it — the frameworks Tauri needs (WebKit, security, etc.) come from
the system. If you intend to notarize a build for distribution, also
install `gon` or use `tauri-cli`'s built-in signing flow.

### Windows

- Visual Studio 2022 Build Tools with the **C++ workload** (provides
  `link.exe`, MSVC, the Windows SDK).
- [WebView2 Runtime](https://developer.microsoft.com/microsoft-edge/webview2/)
  — usually preinstalled on Windows 11; install on Windows 10 if missing.
- For installer signing (optional): the WiX 3.x toolset.

See [`BUILDING_WINDOWS.md`](./BUILDING_WINDOWS.md) for the full
Windows walkthrough (prerequisites, concrete build steps, artifact
paths, and the Windows-VM route for Linux developers).

## 2. Clone

The repo expects a few sibling reference checkouts under `refs/`. Pull
them yourself; CI does the same.

```sh
git clone https://github.com/<your-org>/ivacam.git
cd ivacam

git clone https://github.com/multigcs/viaconstructor refs/viaconstructor
git clone https://github.com/IxMilia/dxf-rs           refs/dxf-rs
```

These are read-only references — the Rust port reimplements the parts it
needs. The viaConstructor checkout supplies DXF fixtures under
`refs/viaconstructor/tests/data/` that the workspace smoke test exercises.

## 3. Build

### Headless workspace (no UI)

```sh
cargo build --workspace
cargo test --workspace --tests   # full Rust unit + integration suite
```

### Web frontend (browser)

```sh
cd frontend
pnpm install                 # pnpm required (link: dep, see prerequisites)
pnpm dev                     # http://localhost:5173, hot reload
pnpm build                   # static bundle to frontend/dist/
```

The full stack on top of `ivac-server` (Rust HTTP):

```sh
# in another shell
cargo run -p ivac-server     # listens on 127.0.0.1:8766
```

`vite.config.ts` already proxies `/api/*` to that port, so the dev
server's "Open file" / Generate UI just works.

### Browser-only (WASM, no backend)

```sh
wasm-pack build crates/ivac-wasm --target web --release
# pkg/ is published as a local dep — pnpm picks it up via
#   "ivac-wasm": "link:../crates/ivac-wasm/pkg"
cd frontend && pnpm install && pnpm dev
# visit http://localhost:5173/?api=wasm
```

The browser downloads the ivac-wasm pkg lazily and runs the entire
CAM pipeline client-side — no Rust server, no Python, no anything.

> **Pkg freshness is automatic.** `pnpm dev` / `pnpm build` (and thus
> every `cargo tauri build/dev`, which drives `pnpm build`) run
> `frontend/scripts/ensure-wasm-fresh.mjs` first: it rebuilds the pkg via
> `wasm-pack` only when the compiled wasm is older than the
> `ivac-wasm`/`ivac-core` sources, and is a fast no-op otherwise. So you
> normally don't run `wasm-pack` by hand — the explicit command above is
> just for a clean first build or to refresh the pkg in isolation. If
> `wasm-pack` isn't installed but a pkg already exists, the guard warns
> and proceeds (a frontend-only dev can keep working); it only errors when
> the pkg is missing entirely.

#### Install-free trial channel (OS-agnostic)

This is the lowest-friction way to let someone *try* ivaCAM:
ship a static bundle to any CDN / static host and hand out a URL — it
runs in the browser on any OS, nothing to install, and the user's
geometry never leaves their machine.

```sh
wasm-pack build crates/ivac-wasm --target web --release
cd frontend && pnpm install && pnpm build   # → frontend/dist/
# host frontend/dist/ on GitHub Pages / S3+CloudFront / Netlify / …
# share:  https://your.site/
```

A **production build defaults to the in-browser wasm engine** when no
backend is configured, so the bare URL just works for a static deploy.
`?api=wasm` forces it explicitly (handy in dev); set
`VITE_IVAC_API=https://your-server` at build time to point at a
`ivac-server` instead. The wasm chunk stays lazy — only fetched when the
wasm transport is actually selected. No CORS, no API server, no
TLS-on-API, no auth to manage, because there is no backend.

Deploy the built `dist/` behind a normal static host — never expose the
Vite **dev** server publicly (its `server.fs.allow` opens the repo for
local convenience).

Copy-paste server configs (with the `.wasm` MIME type, immutable asset
caching, and an `index.html` fallback already set) live in
[`deploy/`](../deploy/): [`Caddyfile`](../deploy/Caddyfile),
[`nginx.conf`](../deploy/nginx.conf), and a [`README`](../deploy/README.md)
covering the hard requirements (serve `.wasm` as `application/wasm`, serve
from the domain root, no COOP/COEP needed, HTTPS optional).

Known limitations of this mode (track before leaning on it as the
headline trial path):

- **Generate runs in a Web Worker** (`ivac-5ue0`) — the CAM
  pipeline runs off the UI thread, so a heavy generate doesn't freeze
  the tab, and aborting a run cancels it for real (the worker
  is terminated and respawned). Falls back to the main-thread client
  where module workers aren't available. The 3D **sim** still runs on
  the main thread (its per-frame heightfield would need transferring
  every frame). To keep it smooth there, `?api=wasm` mode auto-caps the
  sim heightfield to a coarser grid (`WASM_TRIAL_SIM_CELL_CAP`, ~250k
  cells) — a lower-res carve *preview*, deliberately traded for
  zero-latency scrubbing (`ivac-5v1b`). Server / Tauri keep
  full sim fidelity.
- **Touch is supported** (`ivac-bwt7`) — the 2D canvas does
  pinch-zoom, two-finger pan, tap- and box-select, and a long-press
  opens the context menu; the 3D view uses OrbitControls (one-finger
  rotate, two-finger pan/zoom) with the same long-press menu. A
  touch-only ⧉ toggle enables keyboardless multi-select. Mouse +
  keyboard is unchanged.
- **wasm32 limits** — single-threaded, ~2–4 GB address space, plus a
  one-time module download; large jobs are slower than the native
  `ivac-server` and may hit memory ceilings sooner.

### Desktop bundle (Tauri)

```sh
cd crates/ivac-tauri
cargo tauri build            # produces a release bundle for your platform
```

Outputs land under `target/release/bundle/`:

| Platform | Artifact                                                   |
|----------|------------------------------------------------------------|
| Linux    | `bundle/deb/*.deb`, `bundle/rpm/*.rpm`, `bundle/appimage/*.AppImage` |
| macOS    | `bundle/dmg/ivaCAM.dmg`, `bundle/macos/ivaCAM.app`   |
| Windows  | `bundle/msi/ivaCAM.msi`                            |

After bundling the AppImage, run

```sh
scripts/strip-appimage-media.sh
```

It removes the GStreamer *core libraries* that linuxdeploy bundles as
WebKit transitive deps and re-packs the AppImage. The app uses no
media (the webview's media backend is disabled at runtime), and the
bundled copies actively cause harm: they shadow the host's GStreamer,
so the host's WebKit loads OUR core against the HOST's plugins —
absent or version-mismatched plugins then print `GStreamer element
appsink not found. Please install it.` on every launch. With the
bundled core stripped, the loader falls back to the host's own
GStreamer (a hard dependency of every webkit2gtk package), keeping
the stack version-consistent and the AppImage smaller. Hosts that
genuinely lack the GStreamer plugin packages may still print the
warning — accepted: it's harmless stderr noise, and the alternative
(`bundleMediaFramework: true`) costs +14 MB for a media stack the app
never uses.

### Android (Tauri mobile)

ivaCAM runs on Android as a native Tauri 2 app: the Svelte UI loads from
the embedded `dist/` in the System WebView and calls the **native**
`ivac-core` commands over Tauri IPC (not the wasm path — `ivac-core` is
pure Rust with zero `*-sys`/C deps, so it cross-compiles to the Android
ABIs with just `rustup target add` + the NDK linker).

**One-time toolchain setup.** You need the Android SDK + NDK and JDK 21
on top of the desktop prerequisites:

```sh
# 1. SDK via cmdline-tools (sdkmanager). Point ANDROID_HOME wherever you
#    keep it; ~/Android/Sdk matches Android Studio's default.
export ANDROID_HOME="$HOME/Android/Sdk"
sdkmanager --install \
    "platform-tools" \
    "platforms;android-34" \
    "build-tools;34.0.0" \
    "ndk;27.2.12479018" \
    "cmake;3.22.1"
sdkmanager --licenses        # accept

# 2. Env the Tauri/Gradle build reads:
export NDK_HOME="$ANDROID_HOME/ndk/27.2.12479018"
export JAVA_HOME="/usr/lib/jvm/java-21-openjdk-amd64"   # JDK 21 (Gradle needs it)

# 3. Rust targets for the four Android ABIs:
rustup target add \
    aarch64-linux-android armv7-linux-androideabi \
    i686-linux-android x86_64-linux-android
```

**Scaffold + build.** From the Tauri crate:

```sh
cd crates/ivac-tauri
cargo tauri android init          # scaffolds gen/android (gradle, manifest, MainActivity)

# Debug APK for a 64-bit ARM device/emulator:
cargo tauri android build --debug --apk --target aarch64
# → gen/android/app/build/outputs/apk/universal/debug/app-universal-debug.apk
#   (contains lib/arm64-v8a/libivac_tauri_lib.so — the native core)

# Or live-reload onto a connected device / running emulator:
cargo tauri android dev
```

**Staging for testers.** The build tools each drop output in their own
tool-specific tree (the APK path above; AppImages under
`target/release/bundle/appimage/`). Do **not** hand those out or copy
them by hand — names and locations drift. Instead run, from the repo
root, after a build:

```sh
scripts/stage-artifacts.sh android     # or: appimage | all
```

It copies the freshest build into **`dist-test/`** (the one
git-ignored directory testers are handed) under a version-stamped
canonical name read from `tauri.conf.json`:
`ivaCAM-<ver>-debug-arm64.apk` / `ivaCAM-<ver>-amd64.AppImage`.
`dist-test/` is the single source of truth for "shippable package" —
nothing else should accumulate loose APKs/AppImages.

`gen/android/` is **git-ignored** (`.gitignore` → `crates/ivac-tauri/gen`)
while the port settles — `cargo tauri android init` regenerates it
deterministically, so you re-scaffold rather than clone it. Once the
Android build is validated on-device we'll commit it (Tauri's
recommendation) so the build is reproducible without re-running `init`.

Notes / gotchas:

- **versionName ≥ 0.0.1.** Android's manifest merger rejects `0.0.0`, so
  the manifests carry the real semver (currently `0.2.0`) — see the
  `version` field in `tauri.conf.json` / `Cargo.toml`.
- **JDK 21** is required by the Gradle plugin; JDK 17 may work but is
  untested here. Expect deprecation warnings (source/target 8) — benign.
- **`beforeBuildCommand` (`pnpm build`) still runs**, so the static
  `dist/` is embedded in the APK. `ensure-wasm-fresh` also runs and
  bundles the wasm pkg even though the native path doesn't use it on
  Android — harmless, trimmed later.
- **Desktop-only plugins** (`window-state`) and `tauri.conf.json`'s
  `app.windows` are `#[cfg(desktop)]`-gated / ignored on mobile.
- **WebGL/Three.js 3D-sim performance** in the Android System WebView is
  the open risk — validate early; it may force a lower sim fidelity on
  mobile.

This is **build-verified, not yet runtime-verified** — the APK assembles
and links the native core, but on-device IPC / UI / file access (SAF)
validation is still pending hardware.

## 4. Verify

```sh
# Workspace tests must be green:
cargo test --workspace

# Frontend type check (svelte-check) must be clean:
cd frontend && pnpm check

# Smoke test the binary you just built:
./target/release/ivac --help
./target/release/ivac generate refs/viaconstructor/tests/data/simple.dxf > out.json
```

For Tauri builds, launching the bundled app should open a window titled
*ivaCAM* with the same UI you saw under `pnpm dev`.

## 5. Troubleshooting

- **rustc version mismatch** — delete `target/` and let `rustup` re-install
  the pinned channel. Don't override the `rust-toolchain.toml`.
- **`webkit2gtk-4.1` not found on Linux** — your distro still ships the
  4.0 ABI. Install `libwebkit2gtk-4.0-dev` and re-run; Tauri 2 falls back.
- **WebView2 missing on Windows** — install the runtime from Microsoft;
  the bundle won't embed it for you in dev builds.
- **macOS notarization complaints** — for personal builds, set the
  `signingIdentity` to `null` in `tauri.conf.json` to skip signing.

If something else trips you up that isn't listed, please open an issue
with the exact platform, toolchain versions (`rustc -V`, `node -v`,
`cargo tauri info`) and the failing command.
