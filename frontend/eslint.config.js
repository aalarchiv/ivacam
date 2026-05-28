// Flat ESLint config for the wiaConstructor frontend (zizn).
//
// Scope: syntax-level lint only — no `parserOptions.project` / type-aware
// rules. svelte-check already runs the full TS type-check on src/**, and
// type-aware ESLint with TS 6.0 + typescript-eslint 8.x would mostly
// duplicate that work for limited extra value. Keep this fast.

import js from '@eslint/js';
import tseslint from 'typescript-eslint';
import svelte from 'eslint-plugin-svelte';
import svelteParser from 'svelte-eslint-parser';
import globals from 'globals';

export default [
  {
    ignores: [
      'dist/**',
      'node_modules/**',
      'src/lib/api/generated.ts', // codegen output — don't lint
      '.svelte-kit/**',
      'eslint.config.js',
    ],
  },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  ...svelte.configs['flat/recommended'],
  {
    languageOptions: {
      globals: {
        ...globals.browser,
        ...globals.node,
        // Vite's `define` replacement constants (see vite.config.ts).
        __WIAC_BUILD_VERSION__: 'readonly',
        __WIAC_PKG_VERSION__: 'readonly',
        __WIAC_BUILD_PROFILE__: 'readonly',
        __WIAC_BUILD_DATE__: 'readonly',
      },
    },
    rules: {
      // The frontend has historically used `_`-prefixed names for
      // intentionally-unused params; match that and don't double-flag
      // (TS noUnusedParameters already covers the rest in tsconfig).
      '@typescript-eslint/no-unused-vars': [
        'error',
        { argsIgnorePattern: '^_', varsIgnorePattern: '^_', caughtErrorsIgnorePattern: '^_' },
      ],
      // Tests + a small number of build helpers legitimately rely on
      // structural casts; turn the warning into an opt-in we can revisit
      // rather than a blanket error.
      '@typescript-eslint/no-explicit-any': 'warn',
      // Svelte 5 reactivity opinion: plain Map/Set instances aren't
      // reactive — components that need reactive collections should use
      // SvelteMap/SvelteSet. Most current uses are intentional non-
      // reactive caches; surfacing as a warning so they show up without
      // blocking the gate. Migrating reactive call sites is tracked as
      // a separate follow-up.
      'svelte/prefer-svelte-reactivity': 'warn',
      // Pointer-capture release/acquire genuinely needs a silent
      // try/catch: the call throws if the pointer is no longer captured,
      // which is the success state. Don't force noise on every site.
      'no-empty': ['error', { allowEmptyCatch: true }],
    },
  },
  {
    // Svelte components AND Svelte 5 runes modules (*.svelte.ts) both
    // need the svelte parser to understand $state / $derived / $props;
    // route inner TS through typescript-eslint's parser.
    files: ['**/*.svelte', '**/*.svelte.ts', '**/*.svelte.js'],
    languageOptions: {
      parser: svelteParser,
      parserOptions: { parser: tseslint.parser },
    },
  },
];
