import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import { execSync } from 'node:child_process';

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

// https://vite.dev/config/
export default defineConfig({
  plugins: [svelte()],
  define: {
    __WIAC_BUILD_VERSION__: JSON.stringify(gitVersion()),
    // ISO-8601 UTC timestamp at build time. Shown in the About
    // dialog alongside the git-describe identifier so users can
    // tell which day a binary was produced without scraping the
    // commit hash against the git log.
    __WIAC_BUILD_DATE__: JSON.stringify(new Date().toISOString()),
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
