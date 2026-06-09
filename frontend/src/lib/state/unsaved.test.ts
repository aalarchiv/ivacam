import { describe, it, expect } from 'vitest';
import { computeUnsavedWork } from './unsaved';

describe('computeUnsavedWork', () => {
  it('empty project is never unsaved (fresh app start, nothing to lose)', () => {
    expect(computeUnsavedWork({ empty: true, dirty: false, savedToProject: false })).toBe(false);
    // Even if flags say otherwise, emptiness wins.
    expect(computeUnsavedWork({ empty: true, dirty: true, savedToProject: false })).toBe(false);
  });

  it('edited project is unsaved regardless of saved-to-project', () => {
    expect(computeUnsavedWork({ empty: false, dirty: true, savedToProject: true })).toBe(true);
    expect(computeUnsavedWork({ empty: false, dirty: true, savedToProject: false })).toBe(true);
  });

  it('freshly imported drawing (clean, never saved as a project) is unsaved', () => {
    // The reported bug: import a DXF, open another file → must warn.
    expect(computeUnsavedWork({ empty: false, dirty: false, savedToProject: false })).toBe(true);
  });

  it('clean project loaded from / saved to a .ivac-project is NOT unsaved', () => {
    // Re-opening from a saved, unedited project must not nag.
    expect(computeUnsavedWork({ empty: false, dirty: false, savedToProject: true })).toBe(false);
  });
});
