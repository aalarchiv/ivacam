/// Logic-level coverage for the ErrorToast helpers. Vitest is configured
/// without the Svelte plugin (see `frontend/vitest.config.ts`), so we
/// exercise the pieces the component depends on: structured-error parsing
/// and fix-label generation. End-to-end DOM rendering is out of scope here.

import { describe, expect, it } from 'vitest';
import { tryParseStructuredError } from '../api/client';
import { autoFixToCommand, type CommandTarget } from '../state/commands';
import type { WiacError } from '../api/types';

function blankCommandTarget(): CommandTarget {
  return {
    operations: [
      {
        id: 1,
        name: 'op1',
        enabled: true,
        kind: 'profile',
        toolId: 0,
        sourceLayers: null,
        depth: -1,
        startDepth: 0,
        step: -1,
        offset: 'outside',
      },
    ],
    tools: [],
    fixtures: [],
    machine: {} as CommandTarget['machine'],
    stock: {} as CommandTarget['stock'],
    settings: {} as CommandTarget['settings'],
    textLayers: [],
    reliefSources: [],
    imports: [],
    workOffset: { x_mm: 0, y_mm: 0, z_mm: 0, wcs: 'G54' },
    groupOpsByTool: false,
    machineProfileId: null,
    dirty: false,
  };
}

describe('tryParseStructuredError', () => {
  it('parses a JSON-encoded WiacError string', () => {
    const json = JSON.stringify({
      kind: 'misconfigured',
      message: 'op 1 references missing tool 5',
      recovery_hint: 'Pick a tool from the library.',
    });
    const err = tryParseStructuredError(json);
    expect(err).not.toBeNull();
    expect(err!.kind).toBe('misconfigured');
    expect(err!.recovery_hint).toContain('Pick a tool');
  });

  it('passes through a structured object', () => {
    const obj: WiacError = {
      kind: 'bad_input',
      message: 'oops',
    };
    expect(tryParseStructuredError(obj)).toEqual(obj);
  });

  it('returns null for plain strings', () => {
    expect(tryParseStructuredError('a normal error message')).toBeNull();
  });

  it('returns null for unknown kinds', () => {
    const json = JSON.stringify({ kind: 'gobbledygook', message: 'x' });
    expect(tryParseStructuredError(json)).toBeNull();
  });

  it('returns null for malformed JSON-looking strings', () => {
    expect(tryParseStructuredError('{not valid')).toBeNull();
  });

  it('handles null / undefined / numbers gracefully', () => {
    expect(tryParseStructuredError(null)).toBeNull();
    expect(tryParseStructuredError(undefined)).toBeNull();
    expect(tryParseStructuredError(42)).toBeNull();
  });
});

describe('autoFixToCommand wiring', () => {
  it('AssignTool applied via autoFixToCommand mutates op tool id', () => {
    const target = blankCommandTarget();
    const cmd = autoFixToCommand({
      kind: 'assign_tool',
      op_id: 1,
      suggested_tool_id: 99,
    });
    cmd.apply(target);
    expect(target.operations[0].toolId).toBe(99);
  });
});
