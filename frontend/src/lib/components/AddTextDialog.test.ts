// Logic-level tests for the AddTextDialog flow. The dialog itself is a
// thin Svelte 5 shell over `describeStyleOp` + `appendImportedSegments`;
// we cover the table-driven mapping here and rely on the e2e build to
// catch UI regressions.

import { describe, expect, it } from 'vitest';
import { STYLE_TABLE, describeStyleOp, engravingMismatch, type TextStyle } from './text_style';

describe('describeStyleOp', () => {
  const TOOL_DIAMETER = 3;
  const TOOL_ID = 7;
  const DEPTH = -2.0;
  const IDS_TWO = [11, 12];
  const IDS_ONE = [11];

  it('engraving → engrave op with on-line offset', () => {
    const d = describeStyleOp('engraving', IDS_TWO, TOOL_ID, TOOL_DIAMETER, -0.5);
    expect(d).toMatchObject({
      kind: 'engrave',
      offset: 'on',
      depth: -0.5,
      toolId: TOOL_ID,
      sourceObjects: IDS_TWO,
    });
    expect(d!.frameShape).toBeUndefined();
  });

  it('carve_inside → vcarve, union when multi-letter', () => {
    const d = describeStyleOp('carve_inside', IDS_TWO, TOOL_ID, TOOL_DIAMETER, -3);
    expect(d).toMatchObject({ kind: 'vcarve', sourceCombine: 'union' });
  });

  it('carve_inside → vcarve, auto when single object', () => {
    const d = describeStyleOp('carve_inside', IDS_ONE, TOOL_ID, TOOL_DIAMETER, -3);
    expect(d).toMatchObject({ kind: 'vcarve', sourceCombine: 'auto' });
  });

  it('carve_outside → vcarve + frame, padding 3*toolD', () => {
    const d = describeStyleOp('carve_outside', IDS_TWO, TOOL_ID, TOOL_DIAMETER, -3);
    expect(d).toMatchObject({
      kind: 'vcarve',
      sourceCombine: 'difference',
      frameShape: 'rectangle',
      framePaddingMm: 9,
    });
  });

  it('pocket_inside → pocket auto', () => {
    const d = describeStyleOp('pocket_inside', IDS_TWO, TOOL_ID, TOOL_DIAMETER, DEPTH);
    expect(d).toMatchObject({ kind: 'pocket', sourceCombine: 'auto' });
    expect(d!.frameShape).toBeUndefined();
  });

  it('pocket_outside → pocket + frame', () => {
    const d = describeStyleOp('pocket_outside', IDS_TWO, TOOL_ID, TOOL_DIAMETER, DEPTH);
    expect(d).toMatchObject({
      kind: 'pocket',
      sourceCombine: 'difference',
      frameShape: 'rectangle',
      framePaddingMm: 9,
    });
  });

  it('outline_inside → profile inside', () => {
    const d = describeStyleOp('outline_inside', IDS_TWO, TOOL_ID, TOOL_DIAMETER, DEPTH);
    expect(d).toMatchObject({ kind: 'profile', offset: 'inside' });
  });

  it('outline_outside → profile outside', () => {
    const d = describeStyleOp('outline_outside', IDS_TWO, TOOL_ID, TOOL_DIAMETER, DEPTH);
    expect(d).toMatchObject({ kind: 'profile', offset: 'outside' });
  });

  it('plain → null (no op)', () => {
    const d = describeStyleOp('plain', IDS_TWO, TOOL_ID, TOOL_DIAMETER, 0);
    expect(d).toBeNull();
  });

  it('every style has a STYLE_TABLE entry with sane defaults', () => {
    const styles: TextStyle[] = [
      'engraving',
      'carve_inside',
      'carve_outside',
      'pocket_inside',
      'pocket_outside',
      'outline_inside',
      'outline_outside',
      'plain',
    ];
    for (const s of styles) {
      const e = STYLE_TABLE[s];
      expect(e).toBeTruthy();
      expect(typeof e.label).toBe('string');
      expect(typeof e.help).toBe('string');
    }
  });

  it('snapshot-matches the per-style descriptors for "AB"-equivalent input', () => {
    const styles: TextStyle[] = [
      'engraving',
      'carve_inside',
      'carve_outside',
      'pocket_inside',
      'pocket_outside',
      'outline_inside',
      'outline_outside',
      'plain',
    ];
    const ids = [1, 2];
    const snap = styles.map((s) => ({
      style: s,
      out: describeStyleOp(s, ids, TOOL_ID, TOOL_DIAMETER, STYLE_TABLE[s].defaultDepth ?? 0),
    }));
    expect(snap).toMatchInlineSnapshot(`
      [
        {
          "out": {
            "depth": -0.5,
            "kind": "engrave",
            "name": "Engrave Text",
            "offset": "on",
            "sourceObjects": [
              1,
              2,
            ],
            "toolId": 7,
          },
          "style": "engraving",
        },
        {
          "out": {
            "depth": -3,
            "kind": "vcarve",
            "name": "V-Carve Text (inside)",
            "sourceCombine": "union",
            "sourceObjects": [
              1,
              2,
            ],
            "toolId": 7,
          },
          "style": "carve_inside",
        },
        {
          "out": {
            "depth": -3,
            "framePaddingMm": 9,
            "frameShape": "rectangle",
            "kind": "vcarve",
            "name": "V-Carve Text (outside)",
            "sourceCombine": "difference",
            "sourceObjects": [
              1,
              2,
            ],
            "toolId": 7,
          },
          "style": "carve_outside",
        },
        {
          "out": {
            "depth": -2,
            "kind": "pocket",
            "name": "Pocket Text (inside)",
            "sourceCombine": "auto",
            "sourceObjects": [
              1,
              2,
            ],
            "toolId": 7,
          },
          "style": "pocket_inside",
        },
        {
          "out": {
            "depth": -2,
            "framePaddingMm": 9,
            "frameShape": "rectangle",
            "kind": "pocket",
            "name": "Pocket Text (outside)",
            "sourceCombine": "difference",
            "sourceObjects": [
              1,
              2,
            ],
            "toolId": 7,
          },
          "style": "pocket_outside",
        },
        {
          "out": {
            "depth": -2,
            "kind": "profile",
            "name": "Outline Text (inside)",
            "offset": "inside",
            "sourceObjects": [
              1,
              2,
            ],
            "toolId": 7,
          },
          "style": "outline_inside",
        },
        {
          "out": {
            "depth": -2,
            "kind": "profile",
            "name": "Outline Text (outside)",
            "offset": "outside",
            "sourceObjects": [
              1,
              2,
            ],
            "toolId": 7,
          },
          "style": "outline_outside",
        },
        {
          "out": null,
          "style": "plain",
        },
      ]
    `);
  });
});

describe('engravingMismatch', () => {
  it('flags filled-outline + engraving + non-empty preview', () => {
    expect(engravingMismatch('engraving', false, 4)).toBe(true);
  });
  it('does not flag single-line + engraving', () => {
    expect(engravingMismatch('engraving', true, 4)).toBe(false);
  });
  it('does not flag any non-engraving style', () => {
    expect(engravingMismatch('carve_inside', false, 4)).toBe(false);
  });
  it('does not flag while preview is empty', () => {
    expect(engravingMismatch('engraving', false, 0)).toBe(false);
  });
  it('does not flag before classification arrives', () => {
    expect(engravingMismatch('engraving', null, 4)).toBe(false);
  });
});
