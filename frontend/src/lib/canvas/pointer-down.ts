// Pure pointer-down reduction for EntityCanvas2D (ox9c). The canvas
// pointerdown handler was a ~250-line if-chain interleaving mode gates
// with hit-tests; the PRIORITY ORDER of those checks is the actual
// business logic (and the part that regressed historically — see the
// right-click note below). This module owns that order as a pure
// decision function; the component supplies lazy hit-test callbacks
// (so a cheap early branch never pays for an expensive later one) and
// performs the side effects the returned intent names.

/// Payload to start dragging a raster-engrave placement image (rt1.12 /
/// j7b4). `grabDX/DY` is the data-space offset between the pointer and
/// the source origin at grab time, so the origin tracks the cursor
/// without jumping.
export interface RasterGrab {
  opId: number;
  sourceId: number;
  grabDX: number;
  grabDY: number;
}

/// Payload to start dragging a text layer's origin (rt1.12 / ywf9).
export interface TextGrab {
  id: number;
  grabDX: number;
  grabDY: number;
}

/// Contour-relative point for toggling a tab placement (rt1.10).
export interface TabTogglePoint {
  objectId: number;
  t: number;
}

export interface PointerDownEnv {
  /// PointerEvent.button: 0 left, 1 middle, 2 right.
  button: number;
  /// Approach-point pick mode (n79) — the cursor IS the picker.
  approachPickActive: boolean;
  /// Selected op has Manual / Mixed tab mode (rt1.10) — the canvas is a
  /// tab-placement surface.
  tabPlacementActive: boolean;
  /// True when the cursor is inside the placed approach marker's hit
  /// circle for a selected profile / pocket op (n79 hybrid drag).
  approachMarkerHit: () => boolean;
  rasterHit: () => RasterGrab | null;
  textHit: () => TextGrab | null;
  /// Ghost-tab projection under the cursor, or null when too far from
  /// the selected op's contour.
  tabGhost: () => TabTogglePoint | null;
  /// Fixture under the cursor, or null.
  fixtureHit: () => number | null;
}

export type PointerDownIntent =
  /// Middle-button drag = pan.
  | { kind: 'pan' }
  /// n79: left-click in pick mode commits the cursor position into
  /// op.approachPoint and STAYS in pick mode (sticky — ESC exits).
  | { kind: 'approach-commit' }
  /// n79: right-click in pick mode bails out without committing.
  | { kind: 'approach-exit' }
  /// Not a gesture this surface handles (right-click outside pick mode
  /// is exclusively a context-menu trigger; forward / back buttons too).
  | { kind: 'ignore' }
  /// n79 hybrid: start dragging the already-placed approach marker.
  | { kind: 'approach-drag' }
  | { kind: 'raster-drag'; grab: RasterGrab }
  | { kind: 'text-drag'; grab: TextGrab }
  /// rt1.10: toggle a tab placement at the contour projection.
  | { kind: 'tab-toggle'; at: TabTogglePoint }
  /// Tab mode active but the cursor wasn't near the contour — swallow
  /// the click (no selection change while placing tabs).
  | { kind: 'tab-miss' }
  | { kind: 'fixture-select'; id: number }
  /// Fall through to the entity-selection reducer
  /// (lib/canvas/entity-selection.ts).
  | { kind: 'entity-click' };

export function reducePointerDown(env: PointerDownEnv): PointerDownIntent {
  if (env.button === 1) return { kind: 'pan' };

  if (env.approachPickActive && env.button === 0) return { kind: 'approach-commit' };
  if (env.approachPickActive && env.button === 2) return { kind: 'approach-exit' };

  // Past this point we only handle LEFT-click. Right-click (button 2)
  // is exclusively a context-menu trigger — onContextMenu runs next
  // and reads the current selection. Letting right-click fall through
  // into the hit-test + selection reducer collapsed multi-selections
  // (user report) and silently fired tab placements / approach-marker
  // drags. Forward / back navigation buttons (3, 4) also bail here.
  if (env.button !== 0) return { kind: 'ignore' };

  // n79: dragging an already-placed approach marker. Only reachable
  // when NOT in pick mode (pick mode committed above).
  if (env.approachMarkerHit()) return { kind: 'approach-drag' };

  // Raster / text grabs are gated out of the tab-placement mode so a
  // click near the contour can't be stolen by an overlapping placement.
  if (!env.tabPlacementActive) {
    // rt1.12 (j7b4): grab a raster-engrave placement image to drag it.
    // Clicking the image also selects its op (raster ops have no source
    // geometry, so the canvas is their only spatial handle).
    const raster = env.rasterHit();
    if (raster) return { kind: 'raster-drag', grab: raster };
    // fx06: click a text glyph stroke to select that layer AND start
    // dragging its origin in one gesture (precise stroke hit-test, so
    // the mostly-whitespace bbox doesn't hijack clicks meant for
    // geometry). Text selection is mutually exclusive with the
    // object / fixture selection.
    const text = env.textHit();
    if (text) return { kind: 'text-drag', grab: text };
  }

  // rt1.10: tab-placement mode — click toggles a placement at the
  // contour projection, Estlcam-style.
  if (env.tabPlacementActive) {
    const ghost = env.tabGhost();
    return ghost ? { kind: 'tab-toggle', at: ghost } : { kind: 'tab-miss' };
  }

  // Fixture hit-test runs before segment selection so clicking a fixture
  // outline snaps the right-hand panel's edit form to it.
  const fixture = env.fixtureHit();
  if (fixture != null) return { kind: 'fixture-select', id: fixture };

  return { kind: 'entity-click' };
}
