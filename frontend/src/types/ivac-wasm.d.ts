// Ambient type for the wasm-pack-generated module that ships at
// crates/ivac-wasm/pkg/. The actual module is only present after
// `wasm-pack build crates/ivac-wasm --target web --release` and gets
// linked into node_modules via `pnpm add file:../crates/ivac-wasm/pkg`.
// We declare it here so the dynamic import in src/lib/api/wasm.ts
// type-checks even before the module is generated.

declare module 'ivac-wasm' {
  export default function init(): Promise<unknown>;
  export function healthz(): { ok: boolean };
  export function version(): {
    version: string;
    transport: string;
    git_sha?: string;
  };
  export function importBytes(filename: string, bytes: Uint8Array): unknown;
  export function generate(request: unknown): unknown;
}

/// Build-time version stamp injected by vite (see `vite.config.ts`).
/// `git describe --always --dirty` at build time, or `"unknown"` when
/// the build runs outside a git checkout.
declare const __IVAC_BUILD_VERSION__: string;

/// ISO-8601 UTC timestamp of the vite build. Shown in the About
/// dialog so users can tell when a binary was produced.
declare const __IVAC_BUILD_DATE__: string;

/// Package version baked from `frontend/package.json` at vite-build
/// time. Shown in the window title alongside the
/// git-describe build version.
declare const __IVAC_PKG_VERSION__: string;

/// Compile-time About copy: repo-root `ABOUT.md` with the `%%VERSION%%`,
/// `%%PKG_VERSION%%`, and `%%DATE%%` tokens substituted, exposed as a
/// virtual module by the `ivac-about-md` plugin in `vite.config.ts`.
declare module 'virtual:about' {
  const md: string;
  export default md;
}
