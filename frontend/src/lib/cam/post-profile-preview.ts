/// TS mirror of the post-profile renderer. Used ONLY for the
/// PostProcessor editor's live preview pane — every keystroke
/// re-renders a short representative program, and round-tripping that
/// through the backend on every key would be wasteful. The output is
/// close-but-not-bit-identical to what `ivac_core::gcode::linuxcnc`
/// emits; the preview's job is to communicate "your knobs do this",
/// not to be a byte-for-byte gcode generator.

import type { AxesConfig, AxisFormat, PostProfile } from '../state/project.svelte';

/// Mirror of `ivac_core::gcode::post_profile::TokenCtx`.
export interface PreviewTokenCtx {
  version: string;
  unit: 'mm' | 'in';
  toolNumber: number;
  toolName: string;
  toolDiameter: number;
  feed: number;
  spindle: number;
  opName: string;
  projectName: string;
  /// Full multi-line tool-library listing (one tool per line), the
  /// same shape Rust's `TokenCtx.tools_listing` produces. Callers
  /// feed `project.data.tools` in here for an accurate preview of the
  /// `<tools>` token; the default falls back to a single sample row.
  toolsListing: string;
}

const DEFAULT_PREVIEW_CTX: PreviewTokenCtx = {
  version: '0.1.0',
  unit: 'mm',
  toolNumber: 1,
  toolName: '3mm endmill',
  toolDiameter: 3.0,
  feed: 800,
  spindle: 18000,
  opName: 'Profile (preview)',
  projectName: 'preview',
  toolsListing: 'T1 (3mm endmill) ⌀3.000',
};

/// Drop-in TS port of `post_profile::substitute`. Case-insensitive,
/// unknown tokens pass through unchanged.
export function substitute(template: string, ctx: PreviewTokenCtx): string {
  const pairs: [string, string][] = [
    ['<version>', ctx.version],
    ['<unit>', ctx.unit],
    ['<t>', String(ctx.toolNumber)],
    ['<n>', ctx.toolName],
    ['<d>', ctx.toolDiameter.toFixed(3)],
    ['<f>', String(ctx.feed)],
    ['<s>', String(ctx.spindle)],
    ['<op>', ctx.opName],
    ['<project>', ctx.projectName],
    ['<tools>', ctx.toolsListing],
    ['<nl>', '\n'],
  ];
  let out = template;
  for (const [token, value] of pairs) {
    const tokenLower = token.toLowerCase();
    // Walk the string, replacing case-insensitively.
    let buf = '';
    let rest = out;
    let pos = rest.toLowerCase().indexOf(tokenLower);
    while (pos !== -1) {
      buf += rest.slice(0, pos);
      buf += value;
      rest = rest.slice(pos + token.length);
      pos = rest.toLowerCase().indexOf(tokenLower);
    }
    buf += rest;
    out = buf;
  }
  return out;
}

/// Split a multi-line substituted template into individual gcode
/// lines. Mirrors `template_lines` in Rust.
export function templateLines(template: string, ctx: PreviewTokenCtx): string[] {
  return substitute(template, ctx).split('\n');
}

/// TS port of `format_axis_value`. Subset of printf: only handles
/// `%[flags][width][.precision]<f|d|g|e>`. Empty / `%`-less strings
/// fall back to `%.3f`. Unknown type ⇒ treated as `f`.
export function formatAxisValue(format: string, value: number): string {
  if (format.length === 0) return value.toFixed(3);
  const pct = format.indexOf('%');
  if (pct === -1) return format;

  let i = pct + 1;
  let zeroPad = false;
  let plusSign = false;
  let leftAlign = false;
  while (i < format.length) {
    const c = format[i];
    if (c === '0') zeroPad = true;
    else if (c === '-') leftAlign = true;
    else if (c === '+') plusSign = true;
    else if (c === ' ') {
      // noop space flag
    } else break;
    i++;
  }
  let width = 0;
  while (i < format.length && format[i] >= '0' && format[i] <= '9') {
    width = width * 10 + (format.charCodeAt(i) - 48);
    i++;
  }
  let precision: number | null = null;
  if (i < format.length && format[i] === '.') {
    i++;
    let p = 0;
    let hasDigits = false;
    while (i < format.length && format[i] >= '0' && format[i] <= '9') {
      p = p * 10 + (format.charCodeAt(i) - 48);
      i++;
      hasDigits = true;
    }
    precision = hasDigits ? p : 0;
  }
  const typ = i < format.length ? format[i] : 'f';
  const afterType = i < format.length ? i + 1 : i;
  const prefix = format.slice(0, pct);
  const suffix = format.slice(afterType);

  const prec = precision ?? 3;
  let body: string;
  switch (typ) {
    case 'd':
    case 'i': {
      const n = Math.round(value);
      body = plusSign && n >= 0 ? `+${n}` : String(n);
      break;
    }
    case 'e':
    case 'E': {
      const s = value.toExponential(prec);
      body = typ === 'E' ? s.toUpperCase() : s;
      break;
    }
    case 'g':
    case 'G': {
      const raw = value.toFixed(prec);
      const dot = raw.indexOf('.');
      if (dot === -1) {
        body = raw;
      } else {
        const trimmed = raw.slice(dot).replace(/0+$/, '').replace(/\.$/, '');
        body = trimmed.length === 0 ? raw.slice(0, dot) : `${raw.slice(0, dot)}${trimmed}`;
      }
      break;
    }
    default: {
      const s = value.toFixed(prec);
      body = plusSign && value >= 0 ? `+${s}` : s;
    }
  }
  if (width > body.length) {
    const pad = width - body.length;
    const ch = zeroPad && !leftAlign ? '0' : ' ';
    const padding = ch.repeat(pad);
    body = leftAlign ? `${body}${padding}` : `${padding}${body}`;
  }
  return `${prefix}${body}${suffix}`;
}

/// Render an axis word (e.g. `X1.000`) given an optional per-axis
/// config. When `af` is undefined or unset, falls back to the natural
/// letter + default format. Returns null when the axis is disabled.
export function renderAxisWord(
  af: AxisFormat | undefined,
  defaultLetter: string,
  value: number,
  defaultFormat: string,
): string | null {
  if (!af) {
    return `${defaultLetter}${formatAxisValue(defaultFormat, value)}`;
  }
  if (!af.enabled) return null;
  return `${af.name}${formatAxisValue(af.format, value * af.scale)}`;
}

/// Build a representative ~12-line program that exercises every
/// configurable section of the profile (header / footer / toolchange /
/// coolant / per-axis X Y Z I J F S). Used by the live preview pane
/// in PostProcessorEditor.
export function previewGcode(profile: PostProfile, ctx: Partial<PreviewTokenCtx> = {}): string {
  const c: PreviewTokenCtx = { ...DEFAULT_PREVIEW_CTX, ...ctx };
  const axes = profile.axes;
  const out: string[] = [];

  // PROGRAM START
  if (profile.program_start) {
    out.push(...templateLines(profile.program_start, c));
  } else {
    out.push('(generated by ivaCAM)');
  }

  // Op-name comment (matches what the pipeline emits between ops).
  out.push(`(op: ${c.opName})`);

  // TOOL CHANGE
  if (profile.tool_change) {
    out.push(...templateLines(profile.tool_change, c));
  } else {
    out.push(`T${c.toolNumber} M6`);
  }

  // Unit + absolute (these are not configurable in v2 yet).
  out.push(c.unit === 'in' ? 'G20' : 'G21');
  out.push('G90');

  // FEED + SPINDLE
  const feedWord = renderAxisWord(axes?.feed, 'F', c.feed, '%d');
  if (feedWord) out.push(feedWord);
  const speedWord = renderAxisWord(axes?.speed, 'S', c.spindle, '%d');
  out.push(`M3${speedWord ? ` ${speedWord}` : ''}`);

  // COOLANT ON
  if (profile.coolant_flood_on) {
    out.push(...templateLines(profile.coolant_flood_on, c));
  } else {
    out.push('M8');
  }

  // Body: a rapid + a linear + an arc, demonstrating XYZIJ formatting.
  const moveLine = (g: string, x: number, y: number, z: number): string => {
    const parts: string[] = [g];
    const xw = renderAxisWord(axes?.x, 'X', x, '%.3f');
    const yw = renderAxisWord(axes?.y, 'Y', y, '%.3f');
    const zw = renderAxisWord(axes?.z, 'Z', z, '%.3f');
    if (xw) parts.push(xw);
    if (yw) parts.push(yw);
    if (zw) parts.push(zw);
    return parts.join(' ');
  };
  const arcLine = (g: string, x: number, y: number, z: number, i: number, j: number): string => {
    const parts: string[] = [g];
    const xw = renderAxisWord(axes?.x, 'X', x, '%.3f');
    const yw = renderAxisWord(axes?.y, 'Y', y, '%.3f');
    const zw = renderAxisWord(axes?.z, 'Z', z, '%.3f');
    const iw = renderAxisWord(axes?.i, 'I', i, '%.3f');
    const jw = renderAxisWord(axes?.j, 'J', j, '%.3f');
    if (xw) parts.push(xw);
    if (yw) parts.push(yw);
    if (zw) parts.push(zw);
    if (iw) parts.push(iw);
    if (jw) parts.push(jw);
    return parts.join(' ');
  };
  out.push(moveLine('G0', 10, 10, 3));
  out.push(moveLine('G1', 10, 10, -2));
  out.push(arcLine('G2', 20, 10, -2, 5, 0));
  out.push(moveLine('G0', 20, 10, 3));

  // COOLANT OFF
  if (profile.coolant_flood_off) {
    out.push(...templateLines(profile.coolant_flood_off, c));
  } else {
    out.push('M9');
  }
  out.push('M5');

  // PROGRAM END
  if (profile.program_end) {
    out.push(...templateLines(profile.program_end, c));
  } else {
    out.push('M30');
  }

  // Apply line endings (visual only — the preview pane is plain text,
  // so we just convert CRLF to a visible inline marker so the user
  // sees the choice without it making the preview pane jump).
  const sep = profile.line_ending === '\r\n' ? ' ↵\n' : '\n';
  return out.join(sep);
}

export function defaultPreviewCtx(): PreviewTokenCtx {
  return { ...DEFAULT_PREVIEW_CTX };
}

/// Convenience axes accessor matching backend defaults — used by the
/// editor to compute "summary" diffs vs. the default config.
export const AXIS_DEFAULTS: Record<keyof AxesConfig, { letter: string; format: string }> = {
  x: { letter: 'X', format: '%.3f' },
  y: { letter: 'Y', format: '%.3f' },
  z: { letter: 'Z', format: '%.3f' },
  i: { letter: 'I', format: '%.3f' },
  j: { letter: 'J', format: '%.3f' },
  feed: { letter: 'F', format: '%d' },
  speed: { letter: 'S', format: '%d' },
};
