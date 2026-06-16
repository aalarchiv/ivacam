// Guard against shipping a stale `ivac-wasm` pkg.
//
// The frontend bundles `crates/ivac-wasm/pkg` through a pnpm `link:` dep,
// but that pkg is a gitignored build artifact — nothing rebuilds it when
// `ivac-wasm` / `ivac-core` sources change. A stale pkg has shipped twice:
// missing sim methods (`clear_checkpoints` → runtime error at Generate) and
// an SVG-units fix that never reached the desktop bundles.
//
// Every Tauri bundle's `beforeBuildCommand` runs `pnpm --dir ../frontend
// build`, so the frontend build (and dev) script is the single chokepoint.
// This guard runs there: it rebuilds the pkg via `wasm-pack` ONLY when the
// compiled wasm is older than the Rust sources, and is a cheap directory
// mtime-walk no-op when everything is fresh.
//
// Node (not a shell script) so it works on Windows native builds too, where
// `cargo tauri build` likewise drives `pnpm build`.

import { readdirSync, statSync, existsSync } from 'node:fs';
import { join, dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

const TAG = '[ensure-wasm]';
// frontend/scripts/ → repo root
const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '../..');

// The compiled artifact the frontend actually loads. wasm-pack writes the
// `.wasm` and its JS/.d.ts bindings together, so this one file's freshness
// stands in for the whole pkg.
const pkgWasm = join(repoRoot, 'crates/ivac-wasm/pkg/ivac_wasm_bg.wasm');

// Everything that compiles into the pkg: both crate `src/` trees plus their
// manifests (a dep bump in Cargo.toml changes the output too).
const inputs = [
  join(repoRoot, 'crates/ivac-wasm/src'),
  join(repoRoot, 'crates/ivac-core/src'),
  join(repoRoot, 'crates/ivac-wasm/Cargo.toml'),
  join(repoRoot, 'crates/ivac-core/Cargo.toml'),
];

/** Newest mtime (ms) under a file or directory tree. */
function newestMtime(path) {
  const st = statSync(path);
  if (!st.isDirectory()) return st.mtimeMs;
  let newest = st.mtimeMs;
  for (const entry of readdirSync(path, { withFileTypes: true })) {
    const child = join(path, entry.name);
    newest = Math.max(newest, entry.isDirectory() ? newestMtime(child) : statSync(child).mtimeMs);
  }
  return newest;
}

let newestSrc = 0;
for (const p of inputs) {
  if (existsSync(p)) newestSrc = Math.max(newestSrc, newestMtime(p));
}

const pkgExists = existsSync(pkgWasm);
const stale = !pkgExists || statSync(pkgWasm).mtimeMs < newestSrc;

if (!stale) {
  console.log(`${TAG} ivac-wasm pkg is up to date`);
  process.exit(0);
}

function hasWasmPack() {
  return spawnSync('wasm-pack --version', { stdio: 'ignore', shell: true }).status === 0;
}

if (!hasWasmPack()) {
  if (pkgExists) {
    // A stale-but-present pkg still builds; don't block a frontend-only dev
    // who has no Rust toolchain. Make the risk loud.
    console.warn(
      `${TAG} WARNING: ivac-wasm pkg is STALE but wasm-pack is not installed — ` +
        `bundling an outdated wasm.\n` +
        `  Install it and rebuild:  cargo install wasm-pack --locked`,
    );
    process.exit(0);
  }
  console.error(
    `${TAG} ERROR: ivac-wasm pkg is missing and wasm-pack is not installed.\n` +
      `  Install it:  cargo install wasm-pack --locked`,
  );
  process.exit(1);
}

console.log(
  `${TAG} ivac-wasm pkg ${pkgExists ? 'is stale' : 'is missing'} — rebuilding ` +
    `(wasm-pack build crates/ivac-wasm --target web --release)…`,
);
// cwd carries any spaces in the repo path; the command string itself has no
// spaced arguments, so shell:true is safe cross-platform.
const res = spawnSync('wasm-pack build crates/ivac-wasm --target web --release', {
  cwd: repoRoot,
  stdio: 'inherit',
  shell: true,
});
process.exit(res.status ?? 1);
