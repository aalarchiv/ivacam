import type { OpEntry } from './op_types';

/// Rename a legacy snake_case key forward to its camelCase successor.
/// A file mid-migration could carry both; the camel key wins.
function renameKey(
  obj: Record<string, unknown>,
  oldKey: string,
  newKey: string,
): Record<string, unknown> {
  if (!(oldKey in obj)) return obj;
  const { [oldKey]: legacy, ...rest } = obj;
  return { ...rest, [newKey]: rest[newKey] ?? legacy };
}

/// Migrate op objects parsed from saved project files written before
/// the frontend op model was made uniformly camelCase (0euf). The
/// persisted file format IS the frontend shape, so older files carry
/// `tabMode: { kind: 'mixed', auto_count }` and snake_case PatternConfig
/// fields — map them forward on load. Idempotent — files written after
/// the rename pass through unchanged.
///
/// The wire contract to the Rust backend still uses snake_case; that
/// translation lives in build-project.ts, not here.
export function migrateLegacyOpFields(op: OpEntry): OpEntry {
  const o = op as unknown as Record<string, unknown>;
  let out = o;

  const tabMode = o.tabMode as Record<string, unknown> | undefined;
  if (tabMode && tabMode.kind === 'mixed' && 'auto_count' in tabMode) {
    out = { ...out, tabMode: renameKey(tabMode, 'auto_count', 'autoCount') };
  }

  const pattern = o.pattern as Record<string, unknown> | undefined;
  if (
    pattern &&
    ['count_x', 'count_y', 'center_x', 'center_y', 'angle_step_deg', 'start_angle_deg'].some(
      (k) => k in pattern,
    )
  ) {
    let p = pattern;
    p = renameKey(p, 'count_x', 'countX');
    p = renameKey(p, 'count_y', 'countY');
    p = renameKey(p, 'center_x', 'centerX');
    p = renameKey(p, 'center_y', 'centerY');
    p = renameKey(p, 'angle_step_deg', 'angleStepDeg');
    p = renameKey(p, 'start_angle_deg', 'startAngleDeg');
    out = { ...out, pattern: p };
  }

  return (out === o ? op : out) as unknown as OpEntry;
}
