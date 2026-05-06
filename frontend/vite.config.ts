import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// https://vite.dev/config/
export default defineConfig({
  plugins: [svelte()],
  build: {
    // Scene3D + three.js is a single intentional chunk (~540 KB);
    // anything bigger than that is the warning we actually want.
    chunkSizeWarningLimit: 600,
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
