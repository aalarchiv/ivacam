import { describe, it, expect } from 'vitest';
import {
  ACTIVITY_ORDER,
  activityFor,
  tabPaneForActivity,
  nextActivity,
  prevActivity,
  activityLabel,
  activityAbbrev,
  type Activity,
} from './activities';

describe('activityFor / tabPaneForActivity round-trip', () => {
  it('maps the Project tab to 2D/3D activities by pane', () => {
    expect(activityFor('project', '2d')).toBe('project-2d');
    expect(activityFor('project', '3d')).toBe('project-3d');
  });

  it('maps whole-screen tabs straight across, ignoring the pane', () => {
    for (const t of ['machine', 'tools', 'settings', 'help'] as const) {
      expect(activityFor(t, '2d')).toBe(t);
      expect(activityFor(t, '3d')).toBe(t);
    }
  });

  it('tabPaneForActivity inverts activityFor', () => {
    expect(tabPaneForActivity('project-2d')).toEqual({ mainTab: 'project', pane: '2d' });
    expect(tabPaneForActivity('project-3d')).toEqual({ mainTab: 'project', pane: '3d' });
    expect(tabPaneForActivity('machine')).toEqual({ mainTab: 'machine', pane: null });
    expect(tabPaneForActivity('settings')).toEqual({ mainTab: 'settings', pane: null });
  });
});

describe('nextActivity / prevActivity', () => {
  it('steps through the order', () => {
    expect(nextActivity('project-2d')).toBe('project-3d');
    expect(nextActivity('project-3d')).toBe('machine');
    expect(prevActivity('machine')).toBe('project-3d');
    expect(prevActivity('project-3d')).toBe('project-2d');
  });

  it('clamps at the ends without wrapping', () => {
    const first = ACTIVITY_ORDER[0];
    const last = ACTIVITY_ORDER[ACTIVITY_ORDER.length - 1];
    expect(prevActivity(first)).toBe(first);
    expect(nextActivity(last)).toBe(last);
  });

  it('walking next from the first reaches the last in order', () => {
    let a: Activity = ACTIVITY_ORDER[0];
    const walked: Activity[] = [a];
    for (let i = 0; i < ACTIVITY_ORDER.length + 2; i++) {
      a = nextActivity(a);
      walked.push(a);
    }
    // Distinct activities visited equals the full ordered set.
    expect([...new Set(walked)]).toEqual([...ACTIVITY_ORDER]);
  });
});

describe('labels', () => {
  it('gives every activity a non-empty label and abbrev', () => {
    for (const a of ACTIVITY_ORDER) {
      expect(activityLabel(a).length).toBeGreaterThan(0);
      expect(activityAbbrev(a).length).toBeGreaterThan(0);
    }
  });
});
