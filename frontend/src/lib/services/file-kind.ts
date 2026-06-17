/// File-kind routing for the unified Open (7jug.14). Kept rune-free (no
/// store imports) so it's unit-testable without the Svelte rune runtime —
/// `file_ops.ts` itself pulls in `$state` stores and can't load in plain
/// vitest.

/// A project file (vs a drawing). ivaCAM projects are JSON
/// (`*.ivac-project.json`, `*.vc-project.json`, or a bare `.json`);
/// drawings are `.dxf` / `.svg`. So a `.json` extension unambiguously means
/// "project" — the basis for the unified Open's routing.
export function isProjectPath(nameOrPath: string): boolean {
  return /\.json$/i.test(nameOrPath);
}
