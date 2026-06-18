/// Pure phone-navigation "activity" model, kept rune-free so it's unit
/// testable. On a phone the desktop main-tabs row and the 2D/3D pane
/// toggle collapse into ONE flat, horizontally-swipeable list of
/// activities (Android-style). This module is the mapping between that
/// flat list and the existing `(mainTab, activePane)` state — which stays
/// authoritative; the activity view just layers phone navigation on top.
///
/// Note the Project tab splits into TWO activities (2D and 3D are
/// separate, not a toggle), so the activity order interleaves them at the
/// front before the whole-screen tabs.

export type MainTab = 'project' | 'machine' | 'tools' | 'settings' | 'help' | 'about';
export type Pane = '2d' | '3d';

export type Activity =
  | 'project-2d'
  | 'project-3d'
  | 'machine'
  | 'tools'
  | 'settings'
  | 'help'
  | 'about';

/// Swipe order, left → right. ViewPager semantics: no wrap (see
/// `nextActivity` / `prevActivity`).
export const ACTIVITY_ORDER: readonly Activity[] = [
  'project-2d',
  'project-3d',
  'machine',
  'tools',
  'settings',
  'help',
  'about',
] as const;

/// Which `(mainTab, pane)` an activity maps onto. For the whole-screen
/// tabs the pane is irrelevant, so it's left `null` and the caller keeps
/// whatever pane was last active.
export function tabPaneForActivity(activity: Activity): { mainTab: MainTab; pane: Pane | null } {
  switch (activity) {
    case 'project-2d':
      return { mainTab: 'project', pane: '2d' };
    case 'project-3d':
      return { mainTab: 'project', pane: '3d' };
    default:
      return { mainTab: activity, pane: null };
  }
}

/// The activity currently shown for a given `(mainTab, pane)`. Only the
/// Project tab consults the pane; the others map straight across.
export function activityFor(mainTab: MainTab, pane: Pane): Activity {
  if (mainTab === 'project') return pane === '3d' ? 'project-3d' : 'project-2d';
  return mainTab;
}

/// Next activity to the right, clamped at the last (no wrap). Returns the
/// same activity when already at the end so a swipe past the edge is a
/// no-op rather than a jump back to the start.
export function nextActivity(activity: Activity): Activity {
  const i = ACTIVITY_ORDER.indexOf(activity);
  return ACTIVITY_ORDER[Math.min(i + 1, ACTIVITY_ORDER.length - 1)];
}

/// Previous activity to the left, clamped at the first (no wrap).
export function prevActivity(activity: Activity): Activity {
  const i = ACTIVITY_ORDER.indexOf(activity);
  return ACTIVITY_ORDER[Math.max(i - 1, 0)];
}

/// Full human label for the activity (top-app-bar title, a11y).
export function activityLabel(activity: Activity): string {
  switch (activity) {
    case 'project-2d':
      return '2D';
    case 'project-3d':
      return '3D';
    case 'machine':
      return 'Machine';
    case 'tools':
      return 'Tool-Lib';
    case 'settings':
      return 'Settings';
    case 'help':
      return 'Help';
    case 'about':
      return 'About';
  }
}

/// Short abbreviation for the compact top-app-bar chip when an icon isn't
/// used.
export function activityAbbrev(activity: Activity): string {
  switch (activity) {
    case 'project-2d':
      return '2D';
    case 'project-3d':
      return '3D';
    case 'machine':
      return 'Mch';
    case 'tools':
      return 'Tools';
    case 'settings':
      return 'Set';
    case 'help':
      return '?';
    case 'about':
      return 'ⓘ';
  }
}
