// Walk a JSON Schema, resolving `$ref: '#/components/schemas/X'` against
// a definitions map. Schemars' Rust output uses this style after our xtask
// rewrite, so SchemaForm doesn't have to special-case anywhere.

import type { JsonSchema } from '../api/client';

export function resolveRef(
  schema: JsonSchema,
  defs: Record<string, JsonSchema>,
): JsonSchema {
  if (!schema.$ref) return schema;
  const m = /^#\/(?:components\/schemas|definitions)\/(.+)$/.exec(schema.$ref);
  if (!m) return schema;
  const target = defs[m[1]];
  if (!target) return schema;
  // Merge the in-place keys (description, title) over the resolved target
  // so a $ref site can still override description.
  const { $ref: _ignore, ...rest } = schema;
  return { ...target, ...rest };
}

export function setAt(
  obj: Record<string, unknown>,
  path: readonly string[],
  value: unknown,
): Record<string, unknown> {
  if (path.length === 0) return obj;
  const [head, ...tail] = path;
  const next = { ...obj };
  if (tail.length === 0) {
    next[head] = value;
  } else {
    const child = (obj[head] as Record<string, unknown>) ?? {};
    next[head] = setAt(child, tail, value);
  }
  return next;
}

export function getAt(
  obj: Record<string, unknown> | undefined,
  path: readonly string[],
): unknown {
  let cur: unknown = obj;
  for (const key of path) {
    if (cur && typeof cur === 'object' && key in (cur as Record<string, unknown>)) {
      cur = (cur as Record<string, unknown>)[key];
    } else {
      return undefined;
    }
  }
  return cur;
}
