// Shared op-chapter parsing for the gcode panel and the playback bar.
//
// A "chapter" is a contiguous run of gcode lines that belong to a single
// operation. Boundaries are the `; OP <id>` markers the backend emits
// between ops. Anything before the first marker is the implicit
// "Program header" chapter (opId = 0).
//
// Pure: given the gcode lines and the project's current op list, returns
// the chapter ranges + display metadata. No reactive runes here — call
// from `$derived` in components.

import type { OpEntry } from './project.svelte';

export interface GcodeChapter {
  opId: number;
  name: string;
  /// 1-based line of the `; OP N` marker (chapter inclusive start).
  startLine: number;
  /// 1-based line, inclusive end.
  endLine: number;
  /// True if the owning op currently has `enabled === false`. UI uses
  /// this to render the chapter commented-out / dimmed.
  disabled: boolean;
}

/// Parse a single line as an op marker. Accepts `; OP 12`, `(OP 12)`,
/// case-insensitive `op`. Returns the op id or null when the line isn't
/// a marker.
function parseOpMarker(raw: string): number | null {
  const s = raw.trim();
  const body = s.startsWith(';')
    ? s.slice(1).trim()
    : s.startsWith('(') && s.endsWith(')')
      ? s.slice(1, -1).trim()
      : null;
  if (body === null) return null;
  const rest = body.startsWith('OP ')
    ? body.slice(3).trim()
    : body.startsWith('op ')
      ? body.slice(3).trim()
      : null;
  if (rest === null) return null;
  const n = parseInt(rest, 10);
  return Number.isFinite(n) && n > 0 ? n : null;
}

export function parseGcodeChapters(
  lines: readonly string[],
  operations: readonly OpEntry[],
): GcodeChapter[] {
  const out: GcodeChapter[] = [];
  if (lines.length === 0) return out;
  const opById = new Map(operations.map((o) => [o.id, o]));
  const nameFor = (id: number): string => {
    if (id === 0) return 'Program header';
    const op = opById.get(id);
    return op ? `#${op.id} ${op.name}` : `Op #${id}`;
  };
  const disabledFor = (id: number): boolean => {
    if (id === 0) return false;
    const op = opById.get(id);
    return op ? !op.enabled : false;
  };
  let curOp = 0;
  let curStart = 1;
  for (let i = 0; i < lines.length; i++) {
    const opId = parseOpMarker(lines[i]);
    if (opId != null) {
      if (i > 0) {
        out.push({
          opId: curOp,
          name: nameFor(curOp),
          startLine: curStart,
          endLine: i, // line just before this marker
          disabled: disabledFor(curOp),
        });
      }
      curOp = opId;
      curStart = i + 1; // marker line is the chapter start
    }
  }
  out.push({
    opId: curOp,
    name: nameFor(curOp),
    startLine: curStart,
    endLine: lines.length,
    disabled: disabledFor(curOp),
  });
  return out;
}

/// u32::MAX sentinel used by the backend `gcode_index.lines_to_segment`
/// for gcode lines that don't move the tool (comments, modal-only).
export const NO_SEGMENT = 4_294_967_295;

/// Find the first segment index produced by any line in
/// `[startLine, endLine]` (1-based, inclusive). Returns null when the
/// range is purely comment-only.
export function firstSegmentInRange(
  linesToSegment: readonly number[],
  startLine: number,
  endLine: number,
): number | null {
  const lo = Math.max(0, startLine - 1);
  const hi = Math.min(linesToSegment.length - 1, endLine - 1);
  for (let p = lo; p <= hi; p++) {
    const seg = linesToSegment[p];
    if (seg !== NO_SEGMENT) return seg;
  }
  return null;
}
