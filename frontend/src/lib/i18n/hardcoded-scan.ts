/// The "did we miss one?" net for the i18n sweeps: scan `.svelte` markup for
/// user-facing string literals that are NOT wrapped in `t(…)`. Pure string
/// work (no fs) so the test can feed it file contents and so the same logic
/// drives both the assertion and the allowlist updater.
///
/// This is a heuristic, not a parser. It errs toward flagging — every flag is
/// either real i18n debt (extract it) or a stable non-string we park in the
/// allowlist (`hardcoded-allowlist.json`). The test asserts findings ===
/// allowlist exactly, so the list shrinks as extraction sweeps land.

/// Attributes whose literal values are shown to the user.
const HUMAN_ATTRS = ['title', 'placeholder', 'aria-label', 'alt', 'label'];

/// Drop `<script>` and `<style>` blocks — only markup carries display text.
function markupOnly(src: string): string {
  return src
    .replace(/<script\b[^>]*>[\s\S]*?<\/script>/g, '')
    .replace(/<style\b[^>]*>[\s\S]*?<\/style>/g, '');
}

/// Reject strings that read as code/markup rather than prose: object-literal
/// fragments, arrow bodies, operators. These slip in when a heuristic regex
/// straddles an expression; real UI text never contains them.
function looksLikeCode(s: string): boolean {
  return /=>|[;{}]|::|\bfunction\b|',|',\s*$|':\s|\)\s*\.|\$\{/.test(s);
}

/// A candidate counts as user-facing text if it holds a real word (≥2 letters
/// with a lowercase run — filters ALLCAPS enum-ish tokens and bare units) and
/// isn't code.
function isHumanText(s: string): boolean {
  if (!/\p{Ll}/u.test(s)) return false; // needs a lowercase letter
  if (!/\p{L}{2,}/u.test(s)) return false; // needs a 2+ letter word
  return !looksLikeCode(s);
}

/// Findings for one file's contents, as a sorted unique list of the offending
/// literal strings.
export function scanSvelte(contents: string): string[] {
  const markup = markupOnly(contents);
  const found = new Set<string>();

  // Text nodes: `>…<` with no tag/brace inside. `{…}` interpolations are
  // excluded by the character class, so a node that is purely `{t('…')}`
  // never matches; a node mixing static text with `{expr}` is split by the
  // braces and only its static runs are tested.
  for (const m of markup.matchAll(/>([^<>{}]+)</g)) {
    const text = m[1].replace(/\s+/g, ' ').trim();
    if (text && isHumanText(text)) found.add(text);
  }

  // Quoted literal attribute values (`title="…"`). `title={…}` is skipped:
  // the `=` is followed by `{`, not `"`.
  const attrRe = new RegExp(`\\b(?:${HUMAN_ATTRS.join('|')})="([^"]+)"`, 'g');
  for (const m of markup.matchAll(attrRe)) {
    const text = m[1].replace(/\s+/g, ' ').trim();
    if (text && isHumanText(text)) found.add(text);
  }

  return [...found].sort();
}
