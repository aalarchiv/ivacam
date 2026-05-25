/// Sidebar accordion pane-transition logic, extracted from App.svelte
/// so the toggle-vs-reveal distinction is unit-testable without the
/// Svelte rune runtime (the bug in ervd was that the programmatic
/// "show Operations after add" path reused the caret TOGGLE and
/// bounced an already-open pane away to `prev`).

export type SidebarPane = 'stock' | 'layers' | 'text' | 'operations';

export interface PaneState {
  /// The currently-expanded accordion pane.
  active: SidebarPane;
  /// The last non-active pane. Clicking the active pane's caret bounces
  /// back here (the "I jumped to Stock, take me back to Operations"
  /// flow).
  prev: SidebarPane;
}

/// Caret-click TOGGLE. Clicking a different pane opens it (remembering
/// the one we left as `prev`); clicking the ALREADY-active pane swaps
/// `active` ↔ `prev` so the pair toggles cleanly.
export function togglePane(state: PaneState, target: SidebarPane): PaneState {
  if (target === state.active) {
    return { active: state.prev, prev: state.active };
  }
  return { active: target, prev: state.active };
}

/// Non-toggling REVEAL. Ensure `target` is active no matter what — for
/// programmatic "show me this pane now" flows (e.g. bouncing to
/// Operations after an op is added from the canvas context menu).
/// A no-op when `target` is already active, so repeated reveals keep
/// the pane open instead of toggling away (ervd).
export function revealPane(state: PaneState, target: SidebarPane): PaneState {
  if (target === state.active) return state;
  return { active: target, prev: state.active };
}
