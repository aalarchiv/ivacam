// Ambient type for the wasm-pack-generated module that ships at
// crates/wiac-wasm/pkg/. The actual module is only present after
// `wasm-pack build crates/wiac-wasm --target web --release` and gets
// linked into node_modules via `pnpm add file:../crates/wiac-wasm/pkg`.
// We declare it here so the dynamic import in src/lib/api/wasm.ts
// type-checks even before the module is generated.

declare module 'wiac-wasm' {
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
declare const __WIAC_BUILD_VERSION__: string;
