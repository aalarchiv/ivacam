import { describe, expect, it } from 'vitest';
import { togglePane, revealPane, type PaneState } from './sidebar-pane';

describe('togglePane (caret-click toggle)', () => {
  it('opens a different pane and remembers the one left as prev', () => {
    const s: PaneState = { active: 'layers', prev: 'operations' };
    expect(togglePane(s, 'operations')).toEqual({ active: 'operations', prev: 'layers' });
    expect(togglePane(s, 'stock')).toEqual({ active: 'stock', prev: 'layers' });
  });

  it('swaps active <-> prev when the already-active pane is clicked', () => {
    const s: PaneState = { active: 'operations', prev: 'layers' };
    expect(togglePane(s, 'operations')).toEqual({ active: 'layers', prev: 'operations' });
  });

  it('toggling the same pane twice returns to the start (clean pair toggle)', () => {
    let s: PaneState = { active: 'operations', prev: 'stock' };
    s = togglePane(s, 'operations'); // -> stock active
    expect(s).toEqual({ active: 'stock', prev: 'operations' });
    s = togglePane(s, 'stock'); // -> operations active again
    expect(s).toEqual({ active: 'operations', prev: 'stock' });
  });
});

describe('revealPane (programmatic, non-toggling — ervd)', () => {
  it('opens the target when a different pane is active', () => {
    const s: PaneState = { active: 'layers', prev: 'stock' };
    expect(revealPane(s, 'operations')).toEqual({ active: 'operations', prev: 'layers' });
  });

  it('is a no-op when the target is already active (does NOT bounce to prev)', () => {
    const s: PaneState = { active: 'operations', prev: 'layers' };
    expect(revealPane(s, 'operations')).toBe(s);
  });

  it('repeated reveals of the same pane keep it open — the ervd regression', () => {
    // Reproduces "add op, add op again": each canvas add reveals
    // Operations. Pre-fix the 2nd reveal toggled away to Layers.
    let s: PaneState = { active: 'layers', prev: 'operations' };
    s = revealPane(s, 'operations'); // 1st add: Operations opens
    expect(s.active).toBe('operations');
    s = revealPane(s, 'operations'); // 2nd add: stays on Operations
    expect(s.active).toBe('operations');
    s = revealPane(s, 'operations'); // 3rd add: still Operations
    expect(s.active).toBe('operations');
  });
});
