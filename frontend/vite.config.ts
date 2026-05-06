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
    // Same-origin proxy to the Stage-1 FastAPI bridge so the frontend can
    // ship without bothering with CORS in dev.
    proxy: {
      '/api': {
        target: 'http://127.0.0.1:8765',
        changeOrigin: true,
        rewrite: (p) => p.replace(/^\/api/, ''),
      },
    },
  },
});
