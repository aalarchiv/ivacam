/// Shared fold state for the phone bottom panels (Operations — `.9` — and
/// G-code — `.11`). The two panels share one bottom strip: when both are
/// folded their handles tile side-by-side (G-code left, Operations right);
/// opening one folds the other. Holding both open snaps here — with the
/// invariant that at most one is non-zero — makes that mutual exclusion
/// automatic, so the components stay dumb.
///
/// The snap *values* are fractions of viewport height (see
/// `bottom-panel-fold.ts`); this store only tracks which panel is open and
/// at what fraction. Persistence of the preferred open height lives in the
/// workspace store, keyed per panel.

export type BottomPanelKey = 'ops' | 'gcode';

class BottomPanelsState {
  /// Open fraction of each panel (0 = folded). Invariant: at most one is
  /// > 0 at any time.
  #ops = $state(0);
  #gcode = $state(0);

  /// Current open snap for a panel (0 when folded).
  snapOf(key: BottomPanelKey): number {
    return key === 'ops' ? this.#ops : this.#gcode;
  }

  /// Which panel is open, or null when both are folded.
  get active(): BottomPanelKey | null {
    if (this.#ops > 0) return 'ops';
    if (this.#gcode > 0) return 'gcode';
    return null;
  }

  /// Set a panel's open fraction. Opening one (snap > 0) folds the other,
  /// keeping the single-open invariant; folding just zeroes that panel.
  setSnap(key: BottomPanelKey, snap: number): void {
    if (key === 'ops') {
      this.#ops = snap;
      if (snap > 0) this.#gcode = 0;
    } else {
      this.#gcode = snap;
      if (snap > 0) this.#ops = 0;
    }
  }
}

export const bottomPanels = new BottomPanelsState();
