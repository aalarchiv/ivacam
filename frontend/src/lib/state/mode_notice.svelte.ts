/// Singleton slice holding the active machine-mode-switch notice (or
/// null). `project.setMachine()` fills it after a mode change via
/// `assessModeSwitch`; the ModeSwitchNotice component renders it and
/// the dismiss / assign / seed actions clear it. Kept out of
/// ProjectState so the notice is UI state — it doesn't dirty the
/// project, isn't undoable, and doesn't persist.

import type { ModeSwitchAssessment } from './mode_switch';

class ModeNoticeState {
  current = $state<ModeSwitchAssessment | null>(null);

  dismiss() {
    this.current = null;
  }
}

export const modeNotice = new ModeNoticeState();
