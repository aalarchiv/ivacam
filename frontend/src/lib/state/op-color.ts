/// Per-op color, shared by the 2D canvas source-assignment overlay, the
/// 3D wireframe source tint, and the 3D toolpath — so an op's source
/// objects and its toolpath read as the SAME hue across both views
/// (source-assignment visibility, issue w5wx follow-up).
///
/// Hue is a golden-ratio walk of the color wheel keyed by op id, so even
/// adjacent ids land far apart and stay distinguishable. The `emphasis`
/// variant (used for the currently-selected op) bumps saturation +
/// lightness so its sources pop above the others.

const PHI_CONJUGATE = 0.6180339887498949;

/// Deterministic hue in [0, 1) for an op id.
export function opHue(opId: number): number {
  return (((opId * PHI_CONJUGATE) % 1) + 1) % 1;
}

/// Normalized HSL (each component in [0, 1]) for an op's assignment
/// tint. `emphasis` ⇒ the selected op.
export function opSourceHsl(opId: number, emphasis: boolean): [number, number, number] {
  return [opHue(opId), emphasis ? 0.8 : 0.62, emphasis ? 0.6 : 0.5];
}

/// CSS `hsl()` string form, for canvas `strokeStyle`.
export function opSourceCss(opId: number, emphasis: boolean): string {
  const [h, s, l] = opSourceHsl(opId, emphasis);
  return `hsl(${Math.round(h * 360)} ${Math.round(s * 100)}% ${Math.round(l * 100)}%)`;
}
