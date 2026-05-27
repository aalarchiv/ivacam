import { describe, it, expect } from 'vitest';
import { migrateLegacyToolTerms } from './tool-migration';
import type { ToolEntry } from './project-types';

// Build a raw object shaped like a pre-rename saved tool (German keys),
// typed loosely since the old keys aren't on the current ToolEntry.
function legacy(extra: Record<string, unknown>): ToolEntry {
  const raw: Record<string, unknown> = {
    id: 1,
    name: 'T1',
    kind: 'endmill',
    diameter: 6,
    flutes: 2,
    ...extra,
  };
  return raw as unknown as ToolEntry;
}

describe('migrateLegacyToolTerms', () => {
  it('renames the kegel tool kind to cone', () => {
    const out = migrateLegacyToolTerms(legacy({ kind: 'kegel' }));
    expect(out.kind).toBe('cone');
  });

  it('renames wirbeln* fields to whirl* and drops the German keys', () => {
    const out = migrateLegacyToolTerms(
      legacy({
        wirbeln: true,
        wirbelnExtraWidthMm: 3,
        wirbelnStepoverMm: 0.75,
        wirbelnOscMm: 0.2,
      }),
    );
    expect(out.whirl).toBe(true);
    expect(out.whirlExtraWidthMm).toBe(3);
    expect(out.whirlStepoverMm).toBe(0.75);
    expect(out.whirlOscMm).toBe(0.2);
    const raw = out as unknown as Record<string, unknown>;
    expect('wirbeln' in raw).toBe(false);
    expect('wirbelnStepoverMm' in raw).toBe(false);
  });

  it('is idempotent — already-English tools pass through unchanged', () => {
    const modern = legacy({ kind: 'cone', whirl: true, whirlStepoverMm: 0.5 });
    const out = migrateLegacyToolTerms(modern);
    expect(out.kind).toBe('cone');
    expect(out.whirl).toBe(true);
    expect(out.whirlStepoverMm).toBe(0.5);
  });

  it('leaves unrelated tool kinds and fields untouched', () => {
    const out = migrateLegacyToolTerms(legacy({ kind: 'v_bit', diameter: 6 }));
    expect(out.kind).toBe('v_bit');
    expect(out.diameter).toBe(6);
  });
});
