// Pure helpers shared between AddTextDialog.svelte and tests.
// Maps a style choice to the OperationKind + patch the dialog applies
// after rendering text geometry. Keeping it free of Svelte runes lets
// vitest cover the table-driven mapping without spinning up the runtime.

import type { FrameShape, OpKind, ProfileOffset, SourceCombine } from '../state/op_types';
import type { ToolKind } from '../state/op_types';

export type TextStyle =
  | 'engraving'
  | 'carve_inside'
  | 'carve_outside'
  | 'pocket_inside'
  | 'pocket_outside'
  | 'outline_inside'
  | 'outline_outside'
  | 'plain';

export interface StyleSpec {
  label: string;
  toolKind: ToolKind | null;
  defaultDepth: number | null;
  help: string;
}

export const STYLE_TABLE: Record<TextStyle, StyleSpec> = {
  engraving: {
    label: 'Engraving',
    toolKind: 'engraver',
    defaultDepth: -0.5,
    help: 'Single-line engrave along the centerline of each glyph. Best with a single-line / Hershey font.',
  },
  carve_inside: {
    label: 'Carve Inside',
    toolKind: 'v_bit',
    defaultDepth: -3.0,
    help: 'V-Carve the closed letter regions — variable-depth medial-axis carving.',
  },
  carve_outside: {
    label: 'Carve Outside',
    toolKind: 'v_bit',
    defaultDepth: -3.0,
    help: 'V-Carve the area between a frame and the text outlines.',
  },
  pocket_inside: {
    label: 'Pocket Inside',
    toolKind: 'endmill',
    defaultDepth: -2.0,
    help: 'Clear the closed letter regions with an endmill.',
  },
  pocket_outside: {
    label: 'Pocket Outside',
    toolKind: 'endmill',
    defaultDepth: -2.0,
    help: 'Clear the area between a frame and the text outlines (raised text).',
  },
  outline_inside: {
    label: 'Outline Inside',
    toolKind: 'endmill',
    defaultDepth: -2.0,
    help: 'Profile cut on the INSIDE of each letter outline.',
  },
  outline_outside: {
    label: 'Outline Outside',
    toolKind: 'endmill',
    defaultDepth: -2.0,
    help: 'Profile cut on the OUTSIDE of each letter outline.',
  },
  plain: {
    label: 'Plain (no op)',
    toolKind: null,
    defaultDepth: null,
    help: 'Adds the text to the geometry layer only — no CAM op is created.',
  },
};

export interface StyleOpDescriptor {
  kind: OpKind;
  name: string;
  toolId: number;
  depth: number;
  sourceObjects?: number[];
  sourceCombine?: SourceCombine;
  offset?: ProfileOffset;
  frameShape?: FrameShape;
  framePaddingMm?: number;
}

/// Build the descriptor of the op the dialog should add for a given
/// style. Returns null for `plain`. `objectIds` is the list returned by
/// `appendImportedSegments`; `toolDiameter` drives the auto-padding for
/// the *Outside frames.
export function describeStyleOp(
  style: TextStyle,
  objectIds: number[],
  toolId: number,
  toolDiameter: number,
  depth: number,
): StyleOpDescriptor | null {
  const sources = objectIds.length > 0 ? objectIds : undefined;
  switch (style) {
    case 'engraving':
      return {
        kind: 'engrave',
        name: 'Engrave Text',
        toolId,
        depth,
        sourceObjects: sources,
        offset: 'on',
      };
    case 'carve_inside':
      return {
        kind: 'vcarve',
        name: 'V-Carve Text (inside)',
        toolId,
        depth,
        sourceObjects: sources,
        sourceCombine: objectIds.length > 1 ? 'union' : 'auto',
      };
    case 'carve_outside':
      return {
        kind: 'vcarve',
        name: 'V-Carve Text (outside)',
        toolId,
        depth,
        sourceObjects: sources,
        sourceCombine: 'difference',
        frameShape: 'rectangle',
        framePaddingMm: 3 * toolDiameter,
      };
    case 'pocket_inside':
      return {
        kind: 'pocket',
        name: 'Pocket Text (inside)',
        toolId,
        depth,
        sourceObjects: sources,
        sourceCombine: 'auto',
      };
    case 'pocket_outside':
      return {
        kind: 'pocket',
        name: 'Pocket Text (outside)',
        toolId,
        depth,
        sourceObjects: sources,
        sourceCombine: 'difference',
        frameShape: 'rectangle',
        framePaddingMm: 3 * toolDiameter,
      };
    case 'outline_inside':
      return {
        kind: 'profile',
        name: 'Outline Text (inside)',
        toolId,
        depth,
        sourceObjects: sources,
        offset: 'inside',
      };
    case 'outline_outside':
      return {
        kind: 'profile',
        name: 'Outline Text (outside)',
        toolId,
        depth,
        sourceObjects: sources,
        offset: 'outside',
      };
    case 'plain':
      return null;
  }
}

/// True when the chosen font is filled-outline but the user picked the
/// Engraving style — the dialog renders a chip suggesting a single-line
/// font swap.
export function engravingMismatch(
  style: TextStyle,
  singleLine: boolean | null,
  previewLength: number,
): boolean {
  return style === 'engraving' && singleLine === false && previewLength > 0;
}
