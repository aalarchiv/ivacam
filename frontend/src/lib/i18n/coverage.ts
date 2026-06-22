/// Pure helpers behind the i18n coverage tests (`coverage.test.ts`). Kept
/// runtime-free (no Svelte runes, no fs) so they unit-test under the
/// logic-only vitest config and can be reused by tooling. The test file
/// owns the fs/glob plumbing and feeds these functions plain data.

/// `{token}` placeholders a template interpolates, as a sorted unique list.
/// Mirrors the `{name}` substitution `lookup()` performs in i18n-core.
export function placeholders(value: string): string[] {
  const out = new Set<string>();
  for (const m of value.matchAll(/\{(\w+)\}/g)) out.add(m[1]);
  return [...out].sort();
}

/// Keys present in `base` but absent from `other`, and vice versa. Used for
/// locale parity: a shippable locale must have neither.
export function keyDiff(
  base: Record<string, string>,
  other: Record<string, string>,
): { missing: string[]; extra: string[] } {
  const baseKeys = new Set(Object.keys(base));
  const otherKeys = new Set(Object.keys(other));
  const missing = [...baseKeys].filter((k) => !otherKeys.has(k)).sort();
  const extra = [...otherKeys].filter((k) => !baseKeys.has(k)).sort();
  return { missing, extra };
}

/// Keys whose value is empty/whitespace-only — never a valid translation.
export function emptyValues(cat: Record<string, string>): string[] {
  return Object.entries(cat)
    .filter(([, v]) => v.trim() === '')
    .map(([k]) => k)
    .sort();
}

/// Keys present in both catalogs whose placeholder sets differ. A German
/// string that drops or renames a `{token}` would interpolate wrong, so
/// this is enforced for every translated key regardless of locale status.
export function placeholderMismatches(
  base: Record<string, string>,
  other: Record<string, string>,
): { key: string; base: string[]; other: string[] }[] {
  const out: { key: string; base: string[]; other: string[] }[] = [];
  for (const [key, value] of Object.entries(other)) {
    const baseValue = base[key];
    if (baseValue === undefined) continue; // parity test owns missing/extra
    const a = placeholders(baseValue);
    const b = placeholders(value);
    if (a.join('|') !== b.join('|')) out.push({ key, base: a, other: b });
  }
  return out;
}

/// Static prefixes of keys built at runtime, e.g. `t(`ops.kind.${kind}`)`
/// yields the prefix `ops.kind.`. Any catalog key under such a prefix is
/// considered live by the dead-key check (a plain text scan can't see it).
export function dynamicKeyPrefixes(src: string): string[] {
  const out = new Set<string>();
  // t(`<prefix>${ … — capture the literal text before the first ${.
  for (const m of src.matchAll(/\bt\(\s*`([^`$]*)\$\{/g)) {
    if (m[1]) out.add(m[1]);
  }
  return [...out].sort();
}

/// Keys defined in the catalog but never referenced in source — neither as
/// a literal `'a.b.c'` nor under a `dynamicKeyPrefixes()` prefix. A non-empty
/// result means the catalog (and the German translators' work) carries dead
/// weight.
export function deadKeys(allKeys: readonly string[], src: string): string[] {
  const prefixes = dynamicKeyPrefixes(src);
  return allKeys.filter((k) => !src.includes(k) && !prefixes.some((p) => k.startsWith(p))).sort();
}
