/// AutoCAD ACI (AutoCAD Color Index) → display color. Single source of
/// truth shared by the 2D canvas (`EntityCanvas2D`) and the 3D scene
/// (`Scene3D`), which previously kept divergent copies — the 3D copy was
/// missing ACI 9, so grey entities rendered as the faint fallback there
/// but grey in 2D.
///
/// The classification (which codes map to a fixed RGB vs a theme token) is
/// shared here; each renderer resolves the theme token its own way (the 2D
/// canvas to a CSS color string, the 3D scene to a `THREE.Color`), since
/// their theme-lookup primitives differ.

/// ACI codes with a fixed RGB (independent of the light/dark theme).
/// `7`/`256` ("white in dark, black in light"), `8` (muted), and any
/// unmapped code resolve to a theme token instead — see `resolveAci`.
export const ACI_FIXED: Readonly<Record<number, number>> = {
  1: 0xff0000,
  2: 0xffff00,
  3: 0x00ff00,
  4: 0x00ffff,
  5: 0x0000ff,
  6: 0xff00ff,
  9: 0x808080,
};

export type AciToken = '--text-strong' | '--text-muted' | '--text-faint';

export type AciResolved =
  | { kind: 'fixed'; hex: number }
  | { kind: 'token'; token: AciToken; fallback: number };

/// Resolve an ACI code to either a fixed RGB hex or a theme token (with a
/// numeric fallback the caller uses when the CSS var is unavailable).
/// ACI 7/256 render "strong" (white-on-dark / black-on-light, exactly how
/// AutoCAD renders them), 8 "muted", everything unmapped "faint".
export function resolveAci(c: number): AciResolved {
  if (c === 7 || c === 256) return { kind: 'token', token: '--text-strong', fallback: 0xe6e6e6 };
  if (c === 8) return { kind: 'token', token: '--text-muted', fallback: 0x888888 };
  const hex = ACI_FIXED[c];
  if (hex !== undefined) return { kind: 'fixed', hex };
  return { kind: 'token', token: '--text-faint', fallback: 0xbbbbbb };
}

/// Format a 24-bit RGB number as a `#rrggbb` CSS string.
export function hexToCss(n: number): string {
  return `#${(n & 0xffffff).toString(16).padStart(6, '0')}`;
}
