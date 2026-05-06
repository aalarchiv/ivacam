# Building wiaConstructor from source

Targeted at users who'd rather not run a prebuilt binary they didn't
compile themselves. The project is a Cargo workspace plus a Vite frontend;
the desktop bundle is produced by Tauri 2.

## 1. Prerequisites

| Tool        | Version          | Notes                                          |
|-------------|------------------|------------------------------------------------|
| Rust        | pinned in [`rust-toolchain.toml`](./rust-toolchain.toml) | rustup picks up the pin automatically the first time you run cargo in this repo. |
| Node.js     | ≥ 20             | Any LTS works; we use it only to drive Vite + svelte-check. |
| pnpm or npm | npm ≥ 10 / pnpm ≥ 9 | Lockfile is `frontend/pnpm-lock.yaml`; npm works against it but pnpm is faster. |
| Tauri CLI   | 2.x              | Install via `cargo install tauri-cli --version "^2" --locked` once you're set up for desktop builds. |
| wasm-pack   | ≥ 0.14           | Install via `cargo install wasm-pack --locked` for the WASM crate. |
| cargo-deny  | ≥ 0.19           | Optional but recommended; CI runs it. `cargo install cargo-deny --locked`. |

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

## 2. Clone

The repo expects a few sibling reference checkouts under `refs/`. Pull
them yourself; CI does the same.

```sh
git clone https://github.com/<your-org>/wiaconstructor.git
cd wiaconstructor

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
cargo test --workspace --tests   # 56+ unit + integration tests
```

### Web frontend (browser)

```sh
cd frontend
pnpm install                 # or npm install
pnpm dev                     # http://localhost:5173, hot reload
pnpm build                   # static bundle to frontend/dist/
```

The full stack on top of `wiac-server` (Rust HTTP):

```sh
# in another shell
cargo run -p wiac-server     # listens on 127.0.0.1:8766
```

`vite.config.ts` already proxies `/api/*` to that port, so the dev
server's "Open file" / Generate UI just works.

### Browser-only (WASM, no backend)

```sh
wasm-pack build crates/wiac-wasm --target web --release
# pkg/ is published as a local dep — pnpm picks it up via
#   "wiac-wasm": "file:../crates/wiac-wasm/pkg"
cd frontend && pnpm install && pnpm dev
# visit http://localhost:5173/?api=wasm
```

The browser downloads the wiac-wasm pkg lazily and runs the entire
CAM pipeline client-side — no Rust server, no Python, no anything.

### Desktop bundle (Tauri)

```sh
cd crates/wiac-tauri
cargo tauri build            # produces a release bundle for your platform
```

Outputs land under `target/release/bundle/`:

| Platform | Artifact                                                   |
|----------|------------------------------------------------------------|
| Linux    | `bundle/deb/*.deb`, `bundle/rpm/*.rpm`, `bundle/appimage/*.AppImage` |
| macOS    | `bundle/dmg/wiaConstructor.dmg`, `bundle/macos/wiaConstructor.app`   |
| Windows  | `bundle/msi/wiaConstructor.msi`                            |

## 4. Verify

```sh
# Workspace tests must be green:
cargo test --workspace

# Frontend type check (svelte-check) must be clean:
cd frontend && npm run check

# Smoke test the binary you just built:
./target/release/wiac --help
./target/release/wiac generate refs/viaconstructor/tests/data/simple.dxf > out.json
```

For Tauri builds, launching the bundled app should open a window titled
*wiaConstructor* with the same UI you saw under `npm run dev`.

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
