/// EntityCanvas2D selection-reducer tests (774f). Pinned matrix:
///
///   modifier × hit-vs-empty → action list
///
/// The reducer is pure (no project / canvas dependency) so these tests
/// cover the modifier semantics end-to-end without mounting Svelte.

import { describe, expect, it } from 'vitest';
import {
  modeFromModifiers,
  reduceCanvasClick,
  type SelectionClick,
} from './entity-selection';

const noMods = { shiftKey: false, ctrlKey: false, metaKey: false };

function clickAt(hitObjectId: number | null, mods: Partial<SelectionClick> = {}): SelectionClick {
  return { hitObjectId, ...noMods, ...mods };
}

describe('modeFromModifiers', () => {
  it('plain click → replace', () => {
    expect(modeFromModifiers(noMods)).toBe('replace');
  });
  it('shift → series (takes priority over ctrl/meta)', () => {
    expect(modeFromModifiers({ ...noMods, shiftKey: true })).toBe('series');
    expect(modeFromModifiers({ ...noMods, shiftKey: true, ctrlKey: true })).toBe('series');
  });
  it('ctrl → toggle', () => {
    expect(modeFromModifiers({ ...noMods, ctrlKey: true })).toBe('toggle');
  });
  it('meta → toggle (Mac Cmd)', () => {
    expect(modeFromModifiers({ ...noMods, metaKey: true })).toBe('toggle');
  });
});

describe('reduceCanvasClick — hit on object', () => {
  it('replace: select + active-op switch (when op-membership known)', () => {
    const ops = new Map<number, readonly number[]>([[42, [7, 8]]]);
    const actions = reduceCanvasClick(clickAt(42), ops);
    expect(actions).toEqual([
      { kind: 'select-objects', ids: [42], mode: 'replace' },
      { kind: 'set-active-op', opId: 7 },
    ]);
  });

  it('replace: no active-op switch when object has no ops attached', () => {
    const ops = new Map<number, readonly number[]>();
    const actions = reduceCanvasClick(clickAt(42), ops);
    expect(actions).toEqual([{ kind: 'select-objects', ids: [42], mode: 'replace' }]);
  });

  it('toggle: select with toggle mode, no active-op switch', () => {
    const ops = new Map<number, readonly number[]>([[42, [7]]]);
    const actions = reduceCanvasClick(clickAt(42, { ctrlKey: true }), ops);
    expect(actions).toEqual([{ kind: 'select-objects', ids: [42], mode: 'toggle' }]);
  });

  it('series: dispatches series-select-to, no active-op switch', () => {
    const ops = new Map<number, readonly number[]>([[42, [7]]]);
    const actions = reduceCanvasClick(clickAt(42, { shiftKey: true }), ops);
    expect(actions).toEqual([{ kind: 'series-select-to', id: 42 }]);
  });

  it('objectId 0 hit is treated as no-op', () => {
    expect(reduceCanvasClick(clickAt(0))).toEqual([]);
  });
});

describe('reduceCanvasClick — empty space', () => {
  it('replace: clear selection + fixture, then arm replace-box', () => {
    expect(reduceCanvasClick(clickAt(null))).toEqual([
      { kind: 'clear-selection' },
      { kind: 'clear-fixture-selection' },
      { kind: 'arm-box-select', mode: 'replace' },
    ]);
  });

  it('toggle: preserve selection, arm toggle-box', () => {
    expect(reduceCanvasClick(clickAt(null, { ctrlKey: true }))).toEqual([
      { kind: 'arm-box-select', mode: 'toggle' },
    ]);
  });

  it('series on empty falls back to additive box-select', () => {
    expect(reduceCanvasClick(clickAt(null, { shiftKey: true }))).toEqual([
      { kind: 'arm-box-select', mode: 'add' },
    ]);
  });
});
