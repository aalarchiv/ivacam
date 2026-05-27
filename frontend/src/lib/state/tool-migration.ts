import type { ToolEntry } from './project-types';

/// Migrate tool objects parsed from saved project / toolset files that
/// were written before the German tool identifiers were anglicized
/// (kegel → cone, wirbeln* → whirl*). The persisted file format IS the
/// frontend shape, so older files carry the German keys; map them forward
/// on load. Idempotent — files written after the rename already use the
/// English names and pass through unchanged.
///
/// Note this is purely about the *frontend/persistence* identifiers. The
/// wire contract to the Rust backend still uses the German names; that
/// translation lives in build-project.ts, not here.
export function migrateLegacyToolTerms(tool: ToolEntry): ToolEntry {
  const t = { ...(tool as unknown as Record<string, unknown>) };
  if (t.kind === 'kegel') t.kind = 'cone';
  const rename = (oldKey: string, newKey: string): void => {
    if (oldKey in t) {
      // Don't clobber a value already stored under the new key (a file
      // mid-migration could carry both); the English key wins.
      if (!(newKey in t)) t[newKey] = t[oldKey];
      delete t[oldKey];
    }
  };
  rename('wirbeln', 'whirl');
  rename('wirbelnExtraWidthMm', 'whirlExtraWidthMm');
  rename('wirbelnStepoverMm', 'whirlStepoverMm');
  rename('wirbelnOscMm', 'whirlOscMm');
  return t as unknown as ToolEntry;
}
