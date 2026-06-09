/// Decision rule for "would discarding the current project lose work?".
///
/// Pure + framework-free so it's unit-testable — `project.svelte.ts` is a
/// `$state` rune class vitest's node config can't construct. The project
/// getter `hasUnsavedWork` just feeds it the live flags.
///
/// Why this is broader than `dirty` alone: `dirty` means "edited since the
/// last save or load" and is reset to false right after EVERY load —
/// including a raw drawing import. A freshly imported DXF/SVG/sample is
/// therefore `dirty === false`, yet it has never been written to a project
/// file, so opening another file would silently throw it away. We treat
/// that as unsaved work too (the "has not been saved" case).
export interface UnsavedWorkState {
  /// Project has no geometry, operations, text layers, or relief sources
  /// — nothing a load could destroy.
  empty: boolean;
  /// Edited since the last save or load (drives gcode/sim staleness too).
  dirty: boolean;
  /// The current content lives in a saved `.ivac-project` file — it was
  /// either loaded from one or written to one. A raw import is NOT
  /// saved-to-project.
  savedToProject: boolean;
}

export function computeUnsavedWork(s: UnsavedWorkState): boolean {
  if (s.empty) return false; // nothing loaded → nothing to lose
  // Non-empty: unsaved if edited, or if it was never saved as a project.
  return s.dirty || !s.savedToProject;
}
