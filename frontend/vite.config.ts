import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import { execSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

/// Build-time version stamp. `git describe --always --dirty` so each
/// build carries the exact commit (and `-dirty` if the working tree
/// had uncommitted changes). When the build runs outside a git
/// checkout (release tarball, CI without git history), fall back to
/// `"unknown"` — the About chip then hides itself.
function gitVersion(): string {
  try {
    return execSync('git describe --always --dirty', {
      stdio: ['ignore', 'pipe', 'ignore'],
    })
      .toString()
      .trim();
  } catch {
    return 'unknown';
  }
}

/// Package version baked into the bundle so the window title + About
/// dialog show the real release identifier instead of a hardcoded
/// '0.0.0' (audit qcvl). Reads `package.json` at vite-config time so a
/// `pnpm version <next>` cuts a new build identity automatically.
function pkgVersion(): string {
  try {
    const pkgPath = fileURLToPath(new URL('./package.json', import.meta.url));
    const pkg = JSON.parse(readFileSync(pkgPath, 'utf8')) as { version?: string };
    return typeof pkg.version === 'string' && pkg.version.length > 0 ? pkg.version : '0.0.0';
  } catch {
    return '0.0.0';
  }
}

// https://vite.dev/config/
export default defineConfig({
  plugins: [svelte()],
  // The wiac-wasm pkg is wasm-bindgen `--target web` glue: its init
  // fetches `wiac_wasm_bg.wasm` relative to its own URL. If Vite's dep
  // optimizer pre-bundles it into `.vite/deps/`, that relative fetch
  // 404s → SPA fallback → "Response has unsupported MIME type ''".
  // Excluding it keeps the glue at its node_modules path so the wasm
  // resolves next to it (dev only; the prod build emits it as a proper
  // asset). Covers the main thread and the Web Worker.
  optimizeDeps: {
    exclude: ['wiac-wasm'],
  },
  define: {
    __WIAC_BUILD_VERSION__: JSON.stringify(gitVersion()),
    // ISO-8601 UTC timestamp at build time. Shown in the About
    // dialog alongside the git-describe identifier so users can
    // tell which day a binary was produced without scraping the
    // commit hash against the git log.
    __WIAC_BUILD_DATE__: JSON.stringify(new Date().toISOString()),
    // Package version from package.json so the window title shows the
    // real release identifier (qcvl). The git-describe value above is
    // the commit-level stamp; this is the human-facing semver.
    __WIAC_PKG_VERSION__: JSON.stringify(pkgVersion()),
  },
  build: {
    // Scene3D + three.js is a single intentional chunk (~540 KB);
    // anything bigger than that is the warning we actually want.
    chunkSizeWarningLimit: 600,
    rollupOptions: {
      output: {
        // Pin three.js (+ OrbitControls) to its own chunk so it stays
        // out of the main bundle. Scene3D is dynamic-imported (App.svelte
        // first-3D-switch); this chunk loads with it.
        manualChunks(id) {
          if (id.includes('node_modules/three')) return 'three';
        },
      },
    },
  },
  server: {
    host: '0.0.0.0',
    port: 5173,
    fs: {
      // node_modules/wiac-wasm is a symlink into ../crates/wiac-wasm/pkg.
      // Vite resolves the symlink to that real path and serves the glue +
      // wasm via /@fs/. With no pnpm-workspace.yaml the default allowed
      // root is just frontend/, so the crates/ path is blocked (403) and
      // the in-browser wasm fetch fails. Allow the repo root (one level
      // up) so the linked pkg is serveable. Dev only.
      allow: ['..'],
    },
    // Same-origin proxy to wiac-server so the frontend can ship without
    // bothering with CORS in dev. Override with VITE_WIAC_API at build
    // time or `?api=…` at runtime if you're pointing at a remote host.
    proxy: {
      '/api': {
        target: 'http://127.0.0.1:8766',
        changeOrigin: true,
        rewrite: (p) => p.replace(/^\/api/, ''),
      },
    },
  },
});
