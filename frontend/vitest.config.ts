// Logic-only test config: skips the Svelte plugin so vitest 2.x's
// bundled vite 5 doesn't trip over @sveltejs/vite-plugin-svelte 7's
// vite 8 expectations. The current AddTextDialog.test.ts only exercises
// pure helpers (`text_style.ts`); when a test starts touching Svelte
// runes / components we'll need to upgrade vitest + spin up jsdom.
import { defineConfig } from 'vitest/config';

export default defineConfig({
  test: {
    include: ['src/**/*.{test,spec}.ts'],
    environment: 'node',
    css: false,
  },
});
