# Building ivaCAM for Windows

A focused companion to [`BUILDING.md`](./BUILDING.md) for producing a
Windows desktop bundle (`.msi` + `.exe` installer). Read `BUILDING.md`
first for the cross-platform picture; this doc only covers what's
Windows-specific.

## Native Windows vs. cross-compiling from Linux

**Recommended for releases: build natively on Windows** (this doc,
§1–§5). It's the path Tauri fully supports and signs, and it produces
both the MSI and the NSIS installer.

But Linux→Windows cross-compilation is **not** the dead end earlier
revisions of this doc claimed — we tested it on this repo and it works
for a runnable binary *and* an NSIS installer. The honest breakdown,
empirically verified (see [§7](#7-cross-compiling-from-linux-experimental)
for the recipe):

| Output | Native on Windows | Cross-compiled from Linux (`cargo-xwin`) |
|--------|:-:|:-:|
| `ivac-desktop.exe` (with icon + manifest) | ✅ | ✅ verified |
| NSIS `…-setup.exe` installer | ✅ | ✅ verified (needs `makensis`) |
| MSI installer | ✅ | ❌ hard-gated off (`Wrong package type msi for platform Linux`) |
| Code signing | ✅ | ❌ Windows-host only (or a custom `sign_command`) |
| Runtime-verified | ✅ | ⚠️ build only — Tauri marks cross-builds **experimental**; we have not launched the artifact |

So cross-compiling is great for a **fast CI smoke build or a throwaway
test installer** from a Linux box, but the artifact is unsigned and
hasn't been run on real Windows — don't ship it to users without a
native (or VM, [§6](#6-building-from-a-windows-vm)) build that you've
actually launched.

For **code signing** (so testers don't hit a SmartScreen wall), see
[`SIGNING.md`](./SIGNING.md) — a planned (not yet configured) setup to
sign the native Windows build in CI with a free SignPath Foundation
certificate.

Either way the codebase is already portable: the only Linux-specific
Rust (`crates/ivac-tauri/src/main.rs` — the WebKit-child SIGKILL and the
GStreamer media-backend disable) is gated behind
`#[cfg(target_os = "linux")]`, so it compiles out for Windows with **zero
code changes**. Windows uses the OS's built-in **WebView2** instead of
WebKitGTK — no GTK, no GStreamer, none of the AppImage media-stripping
dance.

## 1. Prerequisites

| Tool | Version | Install (PowerShell) | Notes |
|------|---------|----------------------|-------|
| Git | any | `winget install Git.Git` | Enable long paths — see [§8](#8-troubleshooting). |
| Visual Studio 2022 **Build Tools** | 2022 | see below | Must include the **Desktop development with C++** workload (gives `link.exe`, MSVC, the Windows SDK). |
| WebView2 Runtime | current | `winget install Microsoft.EdgeWebView2Runtime` | Preinstalled on Windows 11; required on Windows 10. |
| Rust (rustup) | pinned by [`rust-toolchain.toml`](../rust-toolchain.toml) (1.88.0) | `winget install Rustlang.Rustup` | Installs the **MSVC** host toolchain by default. The repo's toolchain file is picked up automatically on first `cargo` run. |
| Node.js | ≥ 20 LTS | `winget install OpenJS.NodeJS.LTS` | Drives Vite + svelte-check only. |
| pnpm | ≥ 10 | `npm install -g pnpm` | Required (the frontend uses a `link:` dep npm can't resolve). |
| Tauri CLI | 2.x | `cargo install tauri-cli --version "^2" --locked` | After Rust is installed. |
| sccache | ≥ 0.15 | `cargo install sccache --locked` | **Required** — `.cargo/config.toml` sets it as `rustc-wrapper` on every platform. Install it, or delete the `[build]` block from `.cargo/config.toml` to opt out. |

### Installing the C++ Build Tools

Either install the full Visual Studio 2022 and tick **Desktop development
with C++**, or install just the Build Tools from PowerShell and add the
workload:

```powershell
winget install --id Microsoft.VisualStudio.2022.BuildTools `
  --override "--quiet --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
```

If the `--override` flag gives you trouble, install
`Microsoft.VisualStudio.2022.BuildTools` plainly, launch **Visual Studio
Installer**, click *Modify*, and check **Desktop development with C++**.

> **WiX / NSIS:** you do **not** install these by hand. The Tauri bundler
> downloads WiX 3.x and NSIS automatically on the first `cargo tauri
> build`. (For *signed* MSIs you'll eventually want your own WiX + a code
> signing cert, but that's out of scope here.)

After installing Rust, open a **fresh** terminal so `cargo`/`rustc` are on
`PATH`, then confirm the toolchain resolves:

```powershell
rustc -V          # should report 1.88.0 (the pinned channel)
cargo tauri info  # sanity-check the Tauri environment
```

## 2. Get the code

Identical to `BUILDING.md` §2 — clone the repo plus the read-only
reference checkouts under `refs/`:

```powershell
git clone <repo-url> ivacam
cd ivacam
git clone https://github.com/multigcs/viaconstructor refs/viaconstructor
git clone https://github.com/IxMilia/dxf-rs           refs/dxf-rs
```

## 3. Build the desktop bundle

```powershell
# 1. Install frontend deps once (pnpm, not npm).
cd frontend
pnpm install
cd ..

# 2. Build the bundle. `cargo tauri build` runs the frontend's
#    `pnpm build` for you via beforeBuildCommand, then compiles the
#    Rust and packages the installers.
cd crates\ivac-tauri
cargo tauri build
```

The first build is slow (it compiles the whole workspace and downloads
WiX + NSIS); subsequent builds are fast thanks to sccache.

## 4. Output artifacts

Because `tauri.conf.json` sets `"targets": "all"`, a Windows build emits
**both** installer formats under `target\release\bundle\`:

| Format | Path (`<ver>` is the version in `tauri.conf.json`) |
|--------|----------------------------------------------------|
| MSI (WiX) | `target\release\bundle\msi\ivaCAM_<ver>_x64_en-US.msi` |
| NSIS `.exe` | `target\release\bundle\nsis\ivaCAM_<ver>_x64-setup.exe` |

The standalone executable (un-bundled) is
`target\release\ivac-tauri.exe`.

Hand a tester the **NSIS `.exe`** for the simplest per-user install, or
the **MSI** for managed/Group-Policy deployment.

## 5. Verify

```powershell
# Workspace tests (same suite as every platform):
cargo test --workspace

# Frontend type check must be clean:
cd frontend; pnpm check; cd ..

# Install one of the bundles, then launch — you should get a window
# titled "ivaCAM" with the same UI as `pnpm dev`.
```

The `.dxf` / `.svg` / `.ngc` / `.ivac-project.json` file associations
declared in `tauri.conf.json` register on install; double-clicking such a
file should open it in ivaCAM.

## 6. Building from a Windows VM

If you develop on Linux (this project's primary host runs Proxmox), the
cleanest path is a disposable Windows 11 VM as your "release box":

1. Create a Windows 11 VM (Proxmox: upload the Win11 ISO + the VirtIO
   driver ISO, give it ≥ 4 vCPU / 8 GB RAM / 60 GB disk for comfortable
   Rust builds).
2. Inside the VM, walk through [§1](#1-prerequisites)–[§4](#4-output-artifacts).
3. Copy the `.msi` / `.exe` out via a shared folder, SMB, or `scp`.

Keep the VM around and you have a repeatable Windows build environment
that mirrors what a `windows-latest` CI runner would do — the local
stand-in for the (dormant, remote-less) `.github/workflows/release-desktop.yml`
Windows lane.

## 7. Cross-compiling from Linux (experimental)

Verified working on this repo from an Ubuntu host (clang/LLD 20.1.8,
rustc 1.88.0, `cargo-xwin` 0.19.2, NSIS 3.10) — it produced a valid
`ivac-desktop.exe` and a `ivaCAM_<ver>_x64-setup.exe` NSIS installer
without touching a Windows machine. See the caveats in
[§Native vs. cross](#native-windows-vs-cross-compiling-from-linux)
before you rely on it — chiefly: **no MSI, no signing, and the artifact
is build-verified only, not runtime-verified.**

### Toolchain

```sh
# LLVM provides the cross linker + resource compiler (Debian/Ubuntu):
sudo apt-get install -y clang lld llvm   # clang-cl, lld-link, llvm-rc, llvm-lib
# NSIS, only if you want the installer (not just the bare .exe):
sudo apt-get install -y nsis             # provides makensis

rustup target add x86_64-pc-windows-msvc

# cargo-xwin downloads the MSVC CRT + Windows SDK on first build
# (~1.1 GB, cached under ~/.cache/cargo-xwin).
# NOTE: cargo-xwin ≥ 0.20 needs rustc ≥ 1.89; this repo pins 1.88
# (rust-toolchain.toml), so install the last 1.88-compatible release:
cargo install cargo-xwin --version 0.19.2 --locked
export XWIN_ACCEPT_LICENSE=1   # accept the Microsoft SDK license non-interactively
```

### Just the binary

```sh
cd frontend && pnpm install && pnpm build && cd ..   # static dist/ (platform-agnostic)
cargo xwin build --release -p ivac-tauri --target x86_64-pc-windows-msvc
# → target/x86_64-pc-windows-msvc/release/ivac-desktop.exe
#   (PE32+ x64 GUI, with the icon + app manifest embedded via llvm-rc)
```

First build ≈ 15 min cold (it compiles the whole Windows dep tree:
tauri, wry, windows-*, …); re-links are a few minutes.

### The NSIS installer too

`cargo tauri build` drives the bundler; point its `--runner` at
`cargo-xwin` and restrict bundles to NSIS (MSI can't be built here):

```sh
export XWIN_ACCEPT_LICENSE=1
cargo tauri build --runner cargo-xwin \
      --target x86_64-pc-windows-msvc --bundles nsis
# → target/x86_64-pc-windows-msvc/release/bundle/nsis/ivaCAM_<ver>_x64-setup.exe
```

If you omit `--bundles nsis`, Tauri will *try* MSI first, log
`Wrong package type msi for platform Linux … ignoring msi`, and carry on
to NSIS — harmless, but `--bundles nsis` keeps the output clean. Tauri
prints a `Cross-platform compilation is experimental` warning on every
run; that's expected.

## 8. Troubleshooting

- **`sccache` not found / build fails immediately** — the repo pins
  `rustc-wrapper = "sccache"` in `.cargo/config.toml` for *all* platforms.
  `cargo install sccache --locked`, or delete that `[build]` block if you
  don't want the compile cache.
- **`link.exe` not found / `error: linker 'link.exe' not found`** — the
  C++ Build Tools workload isn't installed (or the terminal predates the
  install). Install **Desktop development with C++** and open a new
  terminal. You do *not* need the GNU toolchain — stick with the default
  MSVC host target.
- **WebView2 missing at runtime** — install
  `Microsoft.EdgeWebView2Runtime`; dev builds don't embed it.
- **`pnpm install` errors on the `ivac-wasm` link dep** — the desktop
  build doesn't need the wasm package, but if the link resolution
  complains, build it once: `cargo install wasm-pack --locked` then
  `wasm-pack build crates/ivac-wasm --target web --release`, and re-run
  `pnpm install`.
- **Path-length / `MAX_PATH` errors during compile** — enable long paths:
  `git config --global core.longpaths true`, and set the
  `LongPathsEnabled` registry DWORD to 1 under
  `HKLM\SYSTEM\CurrentControlSet\Control\FileSystem`. A short checkout
  path (e.g. `C:\src\ivacam`) also helps.
- **rustc version mismatch** — delete `target\` and let `rustup`
  re-install the pinned channel. Don't override `rust-toolchain.toml`.

If something else trips you up, capture `rustc -V`, `node -v`, and
`cargo tauri info` along with the failing command when you open an issue.
