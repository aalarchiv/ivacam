/// Pure helpers for operation grouping (rt1.21). Lives outside
/// `OperationsList.svelte` so the bucketing logic is unit-testable
/// without booting the Svelte rune runtime — the test config runs
/// in pure-node and any import of project.svelte trips up.
///
/// "Op" is a structural subset of `OpEntry`: every consumer in the
/// real app types ops through the broader interface, but only `id`
/// and `group` matter here.

export interface GroupableOp {
  id: number;
  group?: string;
  enabled?: boolean;
}

export interface GroupBucket<T extends GroupableOp> {
  /// Empty string ⇒ ungrouped bucket (rendered as "Other" in the UI).
  name: string;
  ops: T[];
}

/// Split an ordered list of ops into buckets preserving the
/// insertion order of group labels. Ungrouped ops fall into a
/// single `''`-named bucket that always renders LAST so newly-added
/// ops keep showing up at the bottom (matching the flat-list past).
/// An empty bucket is still emitted when the input has no
/// ungrouped ops, so the UI can show "no groups yet" affordance.
export function groupOperations<T extends GroupableOp>(ops: T[]): GroupBucket<T>[] {
  const buckets = new Map<string, T[]>();
  for (const op of ops) {
    const key = op.group ?? '';
    const arr = buckets.get(key) ?? [];
    arr.push(op);
    buckets.set(key, arr);
  }
  const ungrouped = buckets.get('') ?? [];
  buckets.delete('');
  const out: GroupBucket<T>[] = [...buckets.entries()].map(([name, list]) => ({
    name,
    ops: list,
  }));
  if (ungrouped.length > 0 || out.length === 0) {
    out.push({ name: '', ops: ungrouped });
  }
  return out;
}

/// True when EVERY op in the bucket is enabled. Used to drive the
/// header's checkbox state.
export function isGroupAllEnabled<T extends GroupableOp>(ops: T[]): boolean {
  return ops.length > 0 && ops.every((o) => o.enabled === true);
}
