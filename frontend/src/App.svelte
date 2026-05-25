<script lang="ts">
  import FileUpload from './lib/components/FileUpload.svelte';
  import EntityCanvas2D from './lib/components/EntityCanvas2D.svelte';
  // Scene3D pulls in the entire three.js graph (~600 KB pre-min) — keep
  // it out of the initial bundle by dynamic-importing on first 3D switch.
  type Scene3DComp = typeof import('./lib/components/Scene3D.svelte').default;
  let Scene3D = $state<Scene3DComp | null>(null);
  let scene3dLoading = $state(false);
  import LayerList from './lib/components/LayerList.svelte';
  import TextList from './lib/components/TextList.svelte';
  import OperationsList from './lib/components/OperationsList.svelte';
  import StockPanel from './lib/components/StockPanel.svelte';
  import GenerateBar from './lib/components/GenerateBar.svelte';
  import PlaybackBar from './lib/components/PlaybackBar.svelte';
  // Heavy / seldom-touched components — dynamic-imported on first
  // open so the main bundle stays light. Each gets a $state slot and
  // an $effect below that triggers the import on first open-flag flip.
  type GcodePanelComp = typeof import('./lib/components/GcodePanel.svelte').default;
  let GcodePanel = $state<GcodePanelComp | null>(null);
  let gcodePanelLoading = false;
  type MachineDialogComp = typeof import('./lib/components/MachineDialog.svelte').default;
  let MachineDialog = $state<MachineDialogComp | null>(null);
  let machineDialogLoading = false;
  type ToolLibraryDialogComp = typeof import('./lib/components/ToolLibraryDialog.svelte').default;
  let ToolLibraryDialog = $state<ToolLibraryDialogComp | null>(null);
  let toolLibraryDialogLoading = false;
  type SettingsDialogComp = typeof import('./lib/components/SettingsDialog.svelte').default;
  let SettingsDialog = $state<SettingsDialogComp | null>(null);
  let settingsDialogLoading = false;
  type AddTextDialogComp = typeof import('./lib/components/AddTextDialog.svelte').default;
  let AddTextDialog = $state<AddTextDialogComp | null>(null);
  let addTextDialogLoading = false;
  type ShortcutHelpComp = typeof import('./lib/components/ShortcutHelp.svelte').default;
  let ShortcutHelp = $state<ShortcutHelpComp | null>(null);
  let shortcutHelpLoading = false;
  type ReportDialogComp = typeof import('./lib/components/ReportDialog.svelte').default;
  let ReportDialog = $state<ReportDialogComp | null>(null);
  let reportDialogLoading = false;
  type AboutDialogComp = typeof import('./lib/components/AboutDialog.svelte').default;
  let AboutDialog = $state<AboutDialogComp | null>(null);
  let aboutDialogLoading = false;
  import LoadingOverlay from './lib/components/LoadingOverlay.svelte';
  import Splitter from './lib/components/Splitter.svelte';

  let machineOpen = $state(false);
  let toolsOpen = $state(false);
  let settingsOpen = $state(false);
  let addTextOpen = $state(false);
  let shortcutHelpOpen = $state(false);
  let reportOpen = $state(false);
  let aboutOpen = $state(false);
  /// Build-time version stamp baked by vite.config.ts. Surfaces in
  /// the window title and the Help → About dialog so users can
  /// paste an exact build identifier into bug reports.
  const buildVersion =
    typeof __WIAC_BUILD_VERSION__ === 'string' ? __WIAC_BUILD_VERSION__ : 'unknown';

  /// Window title carries the build version so a screenshot pins the
  /// report to the exact binary. Format: "wiaConstructor v<pkg>
  /// (<git-describe>)" — package version comes from
  /// frontend/package.json via the `__WIAC_PKG_VERSION__` define
  /// baked by vite.config.ts (audit qcvl), git-describe via
  /// `__WIAC_BUILD_VERSION__`. `document.title` updates on every
  /// paint that touches the effect, but it's cheap.
  const pkgVersion =
    typeof __WIAC_PKG_VERSION__ === 'string' ? __WIAC_PKG_VERSION__ : '0.0.0';
  $effect(() => {
    if (buildVersion && buildVersion !== 'unknown') {
      document.title = `wiaConstructor v${pkgVersion} (${buildVersion})`;
    } else {
      document.title = `wiaConstructor v${pkgVersion}`;
    }
  });
  /// Startup banner: when set, the user was previously editing a
  /// project and we offer to reopen it. Styled in-app instead of a
  /// native window.confirm so the first impression of the app isn't
  /// an unstyled OS dialog (audit C10).
  let reopenPrompt = $state<{ path: string; filename: string } | null>(null);

  // Open the Tool library dialog when OpPropertiesPanel's "edit this
  // tool" icon requests focus on a specific tool row. The dialog reads
  // project.toolsDialogFocusId and handles scroll/highlight.
  $effect(() => {
    if (project.toolsDialogFocusId != null) {
      toolsOpen = true;
    }
  });

  // G-code panel visibility. The playback bar always sits below the
  // 3D canvas; the gcode panel opens as an extra row beneath it so
  // the user sees the toolpath, the playhead, and the program text
  // simultaneously and can drive each from the others.
  let gcodeOpen = $state(false);
  import { project } from './lib/state/project.svelte';
  import { workspace } from './lib/state/workspace.svelte';
  import { confirmStore } from './lib/state/confirm.svelte';
  import ConfirmPrompt from './lib/components/ConfirmPrompt.svelte';
  import {
    openFile,
    openProject,
    loadFromPath,
    loadProjectPath,
    loadFile,
    loadProjectFile,
    loadSample,
    saveProject,
    exportGeneratedGcode,
    SAMPLES,
  } from './lib/state/file_ops';
  import { onMount } from 'svelte';
  import {
    isDesktop,
    wireSourceWatch as wireDesktopSourceWatch,
    wireCloseRequested,
    confirmClose,
    logErrorToStderr,
    isDebugSession,
  } from './lib/state/desktop';
  import { computeFootprint } from './lib/sim/driver';
  import { togglePane, revealPane, type SidebarPane } from './lib/state/sidebar-pane';

  /// Live label for the Stock panel summary — shows the current
  /// dimensions inline so the user sees the workpiece size at a glance
  /// without expanding the panel. Uses `computeFootprint` so the
  /// numbers match what Scene3D / sim use (auto mode follows imported
  /// bbox; manual = customX/Y; no-import fallback = machine work area).
  /// Accordion-style sidebar (replaces the per-panel `collapsed`
  /// state): exactly one of Stock / Layers / Text / Operations is
  /// active at a time. Clicking another panel's header makes it the
  /// active one and the previous active collapses to its header
  /// strip. Default = 'layers' so the file-open + reopen affordances
  /// are visible on cold start (the typical first-action is "open
  /// drawing"). Each panel still shows its summary chip when
  /// collapsed (Stock dims, layer count + filename, text count), so
  /// the user can read working numbers at a glance without
  /// switching panes.
  ///
  /// Click the ACTIVE panel's caret to bounce back to the previously
  /// active pane — the typical "I jumped to Stock to tweak dims, now
  /// take me back to Operations" flow. `activateSidebarPane(p)` swaps
  /// `prev` ↔ `active` when the same pane is clicked twice, so the
  /// pair toggles cleanly.
  // Pane-transition logic lives in lib/state/sidebar-pane.ts (pure +
  // unit-tested). `activateSidebarPane` is the caret-click TOGGLE;
  // `revealSidebarPane` is the non-toggling "show me this pane now"
  // used by programmatic flows (ervd).
  let activeSidebarPane = $state<SidebarPane>('layers');
  /// Last non-current pane. Initial value matches "user hasn't
  /// switched yet but wants Operations" — the most likely return
  /// destination for a Layers-default startup.
  let prevSidebarPane = $state<SidebarPane>('operations');
  function activateSidebarPane(target: SidebarPane) {
    const next = togglePane({ active: activeSidebarPane, prev: prevSidebarPane }, target);
    activeSidebarPane = next.active;
    prevSidebarPane = next.prev;
  }
  function revealSidebarPane(target: SidebarPane) {
    const next = revealPane({ active: activeSidebarPane, prev: prevSidebarPane }, target);
    activeSidebarPane = next.active;
    prevSidebarPane = next.prev;
  }
  const stockDimsLabel = $derived.by<string>(() => {
    const cfg = project.stock;
    const fp = computeFootprint(project.transformedImport, cfg, project.machine.workArea);
    const x = Math.max(0, fp.maxX - fp.minX);
    const y = Math.max(0, fp.maxY - fp.minY);
    const z = Math.max(0, cfg.thickness);
    const f = (n: number) => (Number.isFinite(n) ? n.toFixed(0) : '0');
    return `${f(x)} × ${f(y)} × ${f(z)} mm`;
  });

  onMount(() => {
    document.documentElement.dataset.theme = project.settings.theme;

    // Global error capture. Silent throws inside Svelte 5 $effect bodies
    // can abort the reactivity scheduler — every button still fires its
    // onclick, but visible state stops updating. Surface every uncaught
    // error two ways:
    //
    //   1. ALWAYS route through `logErrorToStderr` so terminal users
    //      running the AppImage see the failure on stderr and
    //      journald / log aggregators can capture it.
    //   2. WHEN `WIAC_DEBUG=1` was set on launch, also render a
    //      direct-DOM banner that bypasses Svelte's reactivity (it
    //      stays visible even when the scheduler is dead). Production
    //      users get clean UI; debugging sessions get loud, visible
    //      diagnostics on top of everything.
    //
    // The banner is created lazily — if the user isn't in a debug
    // session we never insert the DOM nodes at all.
    let errorBanner: { push: (msg: string) => void } | null = null;
    void isDebugSession().then((dbg) => {
      if (!dbg) return;
      const host = document.createElement('div');
      host.id = 'wiac-error-banner';
      host.style.cssText =
        'position:fixed;top:0;left:0;right:0;z-index:2147483647;background:#7a0000;color:#fff;font:11px monospace;padding:6px 10px;max-height:40vh;overflow:auto;pointer-events:auto;display:none;white-space:pre-wrap;';
      const dismiss = document.createElement('button');
      dismiss.textContent = '×';
      dismiss.style.cssText =
        'position:absolute;top:2px;right:6px;background:transparent;border:0;color:#fff;cursor:pointer;font-size:16px;';
      dismiss.onclick = () => {
        host.style.display = 'none';
        host.replaceChildren(dismiss);
      };
      host.appendChild(dismiss);
      document.body.appendChild(host);
      errorBanner = {
        push(msg: string) {
          host.style.display = 'block';
          const line = document.createElement('div');
          line.textContent = msg;
          host.appendChild(line);
        },
      };
    });

    window.addEventListener('error', (ev) => {
      const msg = ev.error?.stack ?? ev.error?.message ?? ev.message ?? 'unknown error';
      const text = String(msg);
      // Benign browser warning: ResizeObserver fires a "loop completed
      // with undelivered notifications" event when an observer callback
      // mutates the layout it was observing. Our canvases coalesce
      // resize work via rAF (EntityCanvas2D + Scene3D) so this should
      // never fire from our code anymore; but Chromium still
      // periodically surfaces it from inside third-party iframes / dev
      // overlays during HMR. Log to stderr for diagnostics, don't
      // toast — it's pure noise.
      if (text.startsWith('ResizeObserver loop')) {
        void logErrorToStderr(`benign: ${text}`);
        return;
      }
      const line = `UI error: ${text}`;
      void logErrorToStderr(line);
      errorBanner?.push(line);
      try {
        project.setError(`UI error: ${text.slice(0, 240)}`);
      } catch {
        // setError might itself fail if the scheduler is dead; the
        // stderr log and (in debug) the banner are the fallback.
      }
    });
    window.addEventListener('unhandledrejection', (ev) => {
      const reason = ev.reason;
      const msg =
        reason instanceof Error
          ? (reason.stack ?? reason.message)
          : typeof reason === 'string'
            ? reason
            : JSON.stringify(reason);
      const line = `async error: ${String(msg)}`;
      void logErrorToStderr(line);
      errorBanner?.push(line);
      try {
        project.setError(`async error: ${String(msg).slice(0, 240)}`);
      } catch {
        // see comment above.
      }
    });

    void wireSourceWatch();
    void wireCloseConfirm();
    void loadWorkspaceAndMaybeReopen();
    return () => {
      unlistenSourceWatch?.();
      unlistenSourceWatch = null;
      unlistenCloseRequested?.();
      unlistenCloseRequested = null;
    };
  });

  /// Pull persisted workspace state at startup. After load completes,
  /// prune any per-project / recent entries pointing at files that have
  /// disappeared (desktop only — both `pruneMissingProjects` and the
  /// reopen prompt self-guard via the workspace API, which returns null
  /// for `last_project` on web because there's no filesystem path).
  async function loadWorkspaceAndMaybeReopen() {
    try {
      await workspace.load();
    } catch {
      // ignore — defaults are fine.
    }
    // Await prune so a deleted-last-project entry has already been
    // dropped by the time we read `last_project` below. Without the
    // await, the reopen banner can appear for a path that prune is
    // about to remove — clicking Reopen then falls into an import-path
    // error toast for a file the user no longer has.
    try {
      await workspace.pruneMissingProjects();
    } catch {
      // ignore — best-effort cleanup.
    }
    if (isDesktop()) {
      const last = workspace.get().last_project;
      if (last) {
        const filename = last.split(/[\\/]/).pop() ?? last;
        reopenPrompt = { path: last, filename };
      }
    }
  }

  async function acceptReopen() {
    if (!reopenPrompt) return;
    const path = reopenPrompt.path;
    reopenPrompt = null;
    const isProjectFile = /\.(wiac|vc)-project\.json$|\.json$/i.test(path);
    if (isProjectFile) await loadProjectPath(path);
    else await loadFromPath(path);
    // If the project file already restored layer-visibility from
    // per-project workspace state, leave it alone — overwriting was
    // the previous behavior (audit zxee). If the user had every layer
    // hidden when they closed (rare but possible), expand to
    // all-visible so the user isn't staring at an empty canvas.
    if (project.transformedImport && project.visibleLayers.size === 0) {
      project.visibleLayers = new Set(project.transformedImport.layers.map((l) => l.name));
    }
  }
  function dismissReopen() {
    reopenPrompt = null;
  }

  // Auto-dismiss the reopen banner once a project / drawing is loaded by
  // any path (the user clicked Open, dragged a file, or accepted the
  // banner). The banner only makes sense as a startup affordance.
  $effect(() => {
    const hasImport = project.transformedImport;
    const hasPath = project.activeProjectPath;
    if (!hasImport && !hasPath) return;
    // Deferred so the prompt clear runs outside the effect scheduler.
    // Inline mutation would self-trigger this effect (it reads
    // `reopenPrompt` itself), which works but is fragile to refactor.
    // queueMicrotask matches the locale-sync effect above.
    queueMicrotask(() => {
      if (reopenPrompt) reopenPrompt = null;
    });
  });

  /// Persist per-project workspace state when the user adjusts visible
  /// layers / selected op / playhead.
  $effect(() => {
    void project.visibleLayers;
    void project.selectedOpId;
    void project.playhead;
    if (project.activeProjectPath) {
      project.persistPerProjectState();
    }
  });

  /// Reactive view of the workspace recent list. `void workspace.version`
  /// subscribes the derived to the store's mutation counter.
  const recentProjects = $derived.by(() => {
    void workspace.version;
    return workspace.get().recent_projects;
  });

  async function clickRecent(path: string) {
    closeAllMenus();
    // Dirty-check once for the Recent click so we don't double-prompt
    // when loadFromPath / loadProjectPath also vet it. `openFile` /
    // `openProject` do their own check; the path variants don't,
    // because the OS file-association launch + reopen banner cases
    // intentionally skip the prompt.
    if (project.dirty) {
      const ok = window.confirm(
        'Your project has unsaved changes. Continue and load the recent project? (Your unsaved work will be lost.)',
      );
      if (!ok) return;
    }
    const isProjectFile = /\.(wiac|vc)-project\.json$|\.json$/i.test(path);
    if (isProjectFile) await loadProjectPath(path);
    else await loadFromPath(path);
  }
  function clickClearRecents() {
    closeAllMenus();
    workspace.clearRecentProjects();
  }

  /// Subscribe to backend `source-file-changed` events emitted by the
  /// project watcher. Stored so onMount's cleanup can disable the watch
  /// on HMR / component-tree teardown — without it the listener leaks
  /// every time App.svelte is reloaded during dev. Implementation lives
  /// in `lib/state/desktop.ts`; this local trampoline preserves the
  /// HMR-safe cleanup binding.
  let unlistenSourceWatch: (() => void) | null = null;
  async function wireSourceWatch() {
    unlistenSourceWatch = await wireDesktopSourceWatch();
  }

  /// qjec: desktop close interception. Always confirm — accidental
  /// closes lose work even on a "clean" project (camera, panel sizes,
  /// in-progress text not yet committed via Add). The double-click
  /// escape hatch in the Tauri backend covers the case where the user
  /// really wants out fast.
  let unlistenCloseRequested: (() => void) | null = null;
  async function wireCloseConfirm() {
    unlistenCloseRequested = await wireCloseRequested(async () => {
      const dirty = project.dirty;
      const ok = await confirmStore.ask({
        title: 'Quit wiaConstructor?',
        body: dirty
          ? 'You have unsaved changes. They will be lost if you quit now.'
          : 'Are you sure you want to quit?',
        primaryLabel: dirty ? 'Discard & quit' : 'Quit',
        cancelLabel: dirty ? 'Keep editing' : 'Cancel',
        danger: dirty,
      });
      if (ok) void confirmClose();
    });
  }

  $effect(() => {
    document.documentElement.dataset.theme = project.settings.theme;
  });

  let activePane = $state<'2d' | '3d'>('2d');

  /// 3D button label cycles with the preview mode: 'both' → "3D",
  /// 'wireframe' → "3Dwire", 'solid' → "3Dsolid". The button does
  /// double duty — first click in 2D mode switches to 3D (keeping the
  /// current preview mode); subsequent clicks cycle modes. Shift+click
  /// reverses the cycle.
  const PREVIEW_CYCLE: ('both' | 'wireframe' | 'solid')[] = ['both', 'wireframe', 'solid'];
  const threeDLabel = $derived.by<string>(() => {
    const m = project.settings.previewMode;
    if (m === 'wireframe') return '3Dwire';
    if (m === 'solid') return '3Dsolid';
    return '3D';
  });
  function onClick3dButton(e: MouseEvent) {
    if (activePane !== '3d') {
      activePane = '3d';
      return;
    }
    const i = PREVIEW_CYCLE.indexOf(project.settings.previewMode);
    const step = e.shiftKey ? -1 : 1;
    const next = PREVIEW_CYCLE[(i + step + PREVIEW_CYCLE.length) % PREVIEW_CYCLE.length];
    project.updateSettings({ previewMode: next });
  }
  /// WAI-ARIA tablist arrow-key nav: ArrowLeft/Right toggles activePane,
  /// Home/End jump to 2D/3D. Roving tabindex on the buttons themselves
  /// keeps Tab order tidy (only the active tab is in the normal flow).
  function onPaneTablistKey(e: KeyboardEvent) {
    if (e.key === 'ArrowLeft' || e.key === 'Home') {
      activePane = '2d';
      e.preventDefault();
    } else if (e.key === 'ArrowRight' || e.key === 'End') {
      activePane = '3d';
      e.preventDefault();
    } else return;
    // Move focus along with selection so the visible focus ring tracks.
    queueMicrotask(() => {
      (e.currentTarget as HTMLElement | null)
        ?.querySelector<HTMLElement>(`[role="tab"][aria-selected="true"]`)
        ?.focus();
    });
  }

  // Auto-switch to 3D when /generate returns; people want to see the toolpath.
  $effect(() => {
    if (project.generated) activePane = '3d';
  });

  // Pull Scene3D in on first activation. The dynamic import becomes its
  // own Vite chunk, so the initial main chunk doesn't carry three.js.
  $effect(() => {
    if (activePane === '3d' && !Scene3D && !scene3dLoading) {
      scene3dLoading = true;
      void import('./lib/components/Scene3D.svelte').then((m) => {
        Scene3D = m.default;
        scene3dLoading = false;
      });
    }
  });

  // Lazy-load each heavy dialog on first open. Each triggers its own
  // dynamic import; subsequent opens just toggle the open flag (the
  // component is already in memory).
  $effect(() => {
    if (machineOpen && !MachineDialog && !machineDialogLoading) {
      machineDialogLoading = true;
      void import('./lib/components/MachineDialog.svelte').then((m) => {
        MachineDialog = m.default;
        machineDialogLoading = false;
      });
    }
  });
  $effect(() => {
    if (toolsOpen && !ToolLibraryDialog && !toolLibraryDialogLoading) {
      toolLibraryDialogLoading = true;
      void import('./lib/components/ToolLibraryDialog.svelte').then((m) => {
        ToolLibraryDialog = m.default;
        toolLibraryDialogLoading = false;
      });
    }
  });
  $effect(() => {
    if (settingsOpen && !SettingsDialog && !settingsDialogLoading) {
      settingsDialogLoading = true;
      void import('./lib/components/SettingsDialog.svelte').then((m) => {
        SettingsDialog = m.default;
        settingsDialogLoading = false;
      });
    }
  });
  $effect(() => {
    if (addTextOpen && !AddTextDialog && !addTextDialogLoading) {
      addTextDialogLoading = true;
      void import('./lib/components/AddTextDialog.svelte').then((m) => {
        AddTextDialog = m.default;
        addTextDialogLoading = false;
      });
    }
  });
  $effect(() => {
    if (shortcutHelpOpen && !ShortcutHelp && !shortcutHelpLoading) {
      shortcutHelpLoading = true;
      void import('./lib/components/ShortcutHelp.svelte').then((m) => {
        ShortcutHelp = m.default;
        shortcutHelpLoading = false;
      });
    }
  });
  $effect(() => {
    if (reportOpen && !ReportDialog && !reportDialogLoading) {
      reportDialogLoading = true;
      void import('./lib/components/ReportDialog.svelte').then((m) => {
        ReportDialog = m.default;
        reportDialogLoading = false;
      });
    }
  });
  $effect(() => {
    if (aboutOpen && !AboutDialog && !aboutDialogLoading) {
      aboutDialogLoading = true;
      void import('./lib/components/AboutDialog.svelte').then((m) => {
        AboutDialog = m.default;
        aboutDialogLoading = false;
      });
    }
  });
  // GcodePanel pulls in syntax-highlighter assets — defer until the
  // user opens the panel (it's collapsed by default).
  $effect(() => {
    if (gcodeOpen && !GcodePanel && !gcodePanelLoading) {
      gcodePanelLoading = true;
      void import('./lib/components/GcodePanel.svelte').then((m) => {
        GcodePanel = m.default;
        gcodePanelLoading = false;
      });
    }
  });

  /// Bumped to `performance.now()` whenever an undo/redo is attempted on
  /// an empty stack — drives the shake animation on the Edit-menu items.
  let undoShakeAt = $state(0);
  let redoShakeAt = $state(0);
  function shake(which: 'undo' | 'redo') {
    if (which === 'undo') undoShakeAt = performance.now();
    else redoShakeAt = performance.now();
  }

  function isTypingTarget(t: EventTarget | null): boolean {
    const el = t as HTMLElement | null;
    if (!el) return false;
    const tag = el.tagName ?? '';
    const editable = el.isContentEditable;
    return tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT' || !!editable;
  }

  function onKeyDown(e: KeyboardEvent) {
    const mod = e.ctrlKey || e.metaKey;
    if (mod && !e.altKey) {
      const k = e.key.toLowerCase();
      if (k === 'z' && !e.shiftKey) {
        if (isTypingTarget(e.target)) return;
        e.preventDefault();
        if (!project.undo()) shake('undo');
        return;
      }
      if ((k === 'y' && !e.shiftKey) || (k === 'z' && e.shiftKey)) {
        if (isTypingTarget(e.target)) return;
        e.preventDefault();
        if (!project.redo()) shake('redo');
        return;
      }
      if (k === 'o' && !e.shiftKey) {
        if (isTypingTarget(e.target)) return;
        e.preventDefault();
        void openFile();
        return;
      }
      if (k === 's' && !e.shiftKey) {
        if (isTypingTarget(e.target)) return;
        e.preventDefault();
        void saveProject();
        return;
      }
    }
    if (e.key === 'Escape') {
      if (project.selectedEntities.size > 0) project.selectedEntities = new Set();
      closeAllMenus();
      return;
    }
    if ((e.key === 't' || e.key === 'T') && !e.ctrlKey && !e.metaKey && !e.altKey) {
      if (isTypingTarget(e.target)) return;
      addTextOpen = true;
      e.preventDefault();
    }
    if ((e.key === '?' || e.key === 'F1') && !e.ctrlKey && !e.metaKey && !e.altKey) {
      if (isTypingTarget(e.target)) return;
      shortcutHelpOpen = true;
      e.preventDefault();
    }
  }

  const undoLabel = $derived(project.undoLabel());
  const redoLabel = $derived(project.redoLabel());
  const canUndo = $derived(project.canUndo());
  const canRedo = $derived(project.canRedo());

  // ---- Menu bar ---------------------------------------------------------
  type MenuId = 'file' | 'edit' | 'view' | 'tools' | 'help';
  let openMenu = $state<MenuId | null>(null);
  function toggleMenu(id: MenuId) {
    openMenu = openMenu === id ? null : id;
  }
  function closeAllMenus() {
    openMenu = null;
  }
  function onWindowClick(e: MouseEvent) {
    if (openMenu == null) return;
    const target = e.target as HTMLElement | null;
    // Clicks inside the menu (button or dropdown) keep it open — the
    // item handlers themselves call closeAllMenus when they should.
    if (target?.closest('.menu')) return;
    closeAllMenus();
  }

  /// Arrow-key / Home / End nav inside an open menubar dropdown. Wired
  /// to the dropdown div's onkeydown — keeps focus inside the menu and
  /// matches the WAI-ARIA pattern for `role="menu"`. ESC is handled at
  /// the window level (which already calls closeAllMenus).
  function onMenuKey(e: KeyboardEvent) {
    const dropdown = (e.currentTarget as HTMLElement) ?? null;
    if (!dropdown) return;
    const items = Array.from(
      dropdown.querySelectorAll<HTMLElement>('button[role="menuitem"]:not(:disabled)'),
    );
    if (items.length === 0) return;
    const active = document.activeElement as HTMLElement | null;
    const idx = active ? items.indexOf(active) : -1;
    let next = idx;
    if (e.key === 'ArrowDown') next = idx < 0 ? 0 : (idx + 1) % items.length;
    else if (e.key === 'ArrowUp') next = idx <= 0 ? items.length - 1 : idx - 1;
    else if (e.key === 'Home') next = 0;
    else if (e.key === 'End') next = items.length - 1;
    else return;
    e.preventDefault();
    items[next]?.focus();
  }
  /// Svelte action that auto-focuses the first menuitem inside the
  /// dropdown on mount. Without it, keyboard users opening the File menu
  /// would have to Tab past every preceding control to reach the first
  /// item — combined with `onMenuKey` above, arrow keys then walk items.
  function focusFirstMenuItemAction(node: HTMLElement) {
    queueMicrotask(() => {
      const first = node.querySelector<HTMLElement>(
        'button[role="menuitem"]:not(:disabled)',
      );
      first?.focus();
    });
  }

  // dteo: window-level drag-and-drop import. Accept .dxf / .svg
  // (loadFile) and .wiac-project.json / .json (loadProjectFile). The
  // overlay only paints while a drag with a `Files` payload is over
  // the window; we count enter / leave to avoid flicker when the
  // cursor crosses child elements.
  let dragOver = $state(false);
  let dragDepth = 0;
  function hasFiles(e: DragEvent): boolean {
    return !!e.dataTransfer && Array.from(e.dataTransfer.types).includes('Files');
  }
  function onDragEnter(e: DragEvent) {
    if (!hasFiles(e)) return;
    dragDepth += 1;
    dragOver = true;
  }
  function onDragOver(e: DragEvent) {
    if (!hasFiles(e)) return;
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = 'copy';
  }
  function onDragLeave(e: DragEvent) {
    if (!hasFiles(e)) return;
    dragDepth = Math.max(0, dragDepth - 1);
    if (dragDepth === 0) dragOver = false;
  }
  async function onDrop(e: DragEvent) {
    if (!hasFiles(e)) return;
    e.preventDefault();
    dragOver = false;
    dragDepth = 0;
    const file = e.dataTransfer?.files?.[0];
    if (!file) return;
    const name = file.name.toLowerCase();
    if (name.endsWith('.wiac-project.json') || name.endsWith('-project.json')) {
      await loadProjectFile(file);
    } else if (name.endsWith('.json')) {
      // Bare .json — also treat as a project file (loadProjectFile
      // validates the kind: 'wiac-project' field and rejects otherwise).
      await loadProjectFile(file);
    } else {
      // .dxf / .svg / anything else the importer recognizes.
      await loadFile(file);
    }
  }
  function pickMenu<T>(fn: () => T): T {
    closeAllMenus();
    return fn();
  }
  function doUndo() {
    closeAllMenus();
    if (!project.undo()) shake('undo');
  }
  function doRedo() {
    closeAllMenus();
    if (!project.redo()) shake('redo');
  }

  async function exportGcode() {
    // Read the last-used post processor from the workspace store so the
    // File-menu export matches the toolbar's Download button without
    // having to reach across the DOM (was querySelector('button.download')
    // .click() — a 'a40m' audit item).
    const raw = workspace.get().last_post_processor;
    const post: 'linuxcnc' | 'grbl' | 'hpgl' =
      raw === 'grbl' || raw === 'hpgl' ? raw : 'linuxcnc';
    await exportGeneratedGcode(post);
  }

  // ---- Resizable layout ------------------------------------------------
  // Sidebar width in px; clamped against the current viewport in
  // `clampSidebar`. Persisted in workspace so the user's preferred ratio
  // survives restart. Window resize re-clamps both panels via the
  // listener below so a restored 720 px sidebar can't eat an 800 px-wide
  // viewport, and a 60 %-tall gcode panel can't run off a shrunk window.
  const SIDEBAR_DEFAULT = 360;
  let sidebarWidth = $state<number>(SIDEBAR_DEFAULT);
  // Gcode panel height: default ~35 % of viewport. `$state` so the
  // default tracks resize until the user drags the splitter (after
  // which the persisted value takes precedence via `clampGcode`).
  let gcodeHeight = $state<number>(Math.round(window.innerHeight * 0.35));

  // Restore persisted sizes from the workspace store once it has loaded.
  $effect(() => {
    void workspace.version;
    const panels = workspace.get().panels;
    if (panels.right_width > 0) sidebarWidth = clampSidebar(panels.right_width);
    if (panels.bottom_height > 0) gcodeHeight = clampGcode(panels.bottom_height);
  });

  function clampSidebar(v: number): number {
    // Hard floor stays at 240 px (under that the OperationsList grid
    // overlaps); ceiling tracks viewport so a too-wide persisted value
    // can't crowd the canvas to zero on a smaller monitor.
    const ceiling = Math.max(240, Math.min(720, Math.round(window.innerWidth * 0.6)));
    return Math.max(240, Math.min(ceiling, v));
  }
  function clampGcode(v: number): number {
    return Math.max(120, Math.min(Math.round(window.innerHeight * 0.7), v));
  }

  function persistLayout() {
    try {
      workspace.setPanels({ right_width: sidebarWidth, bottom_height: gcodeHeight });
    } catch (e) {
      console.warn('persist layout:', e);
    }
  }

  function onSidebarResize(delta: number) {
    sidebarWidth = clampSidebar(sidebarWidth - delta); // splitter is LEFT of sidebar → drag-right shrinks sidebar
    persistLayout();
  }
  function resetSidebar() {
    sidebarWidth = SIDEBAR_DEFAULT;
    persistLayout();
  }
  function onGcodeResize(delta: number) {
    gcodeHeight = clampGcode(gcodeHeight - delta); // splitter is ABOVE gcode → drag-down shrinks
    persistLayout();
  }
  function resetGcode() {
    gcodeHeight = Math.round(window.innerHeight * 0.35);
    persistLayout();
  }

  // Re-clamp panel sizes on viewport changes — without this, a persisted
  // 720 px sidebar restored on an 800 px window would leave 80 px for the
  // canvas, and a 600 px gcode panel on a 700 px-tall window would crowd
  // the 3D scene to nothing. Listener is installed once at mount and torn
  // down on destroy. The persist call is debounced via the workspace's
  // own write debounce — no rAF needed.
  function onWindowResize() {
    const oldSide = sidebarWidth;
    const oldGcode = gcodeHeight;
    const newSide = clampSidebar(sidebarWidth);
    const newGcode = clampGcode(gcodeHeight);
    if (newSide !== oldSide) sidebarWidth = newSide;
    if (newGcode !== oldGcode) gcodeHeight = newGcode;
    if (newSide !== oldSide || newGcode !== oldGcode) persistLayout();
  }

  /// Status bar text — three layers.
  ///   1. `modalStatusHint`: when a modal click-tool is active (approach
  ///      pick, tab placement), render its instructions verbatim and skip
  ///      the rest. Modal hints take precedence because the user needs
  ///      to know how to exit the mode.
  ///   2. `statusInfoText`: idle context — bbox + segment count if a
  ///      drawing is loaded, otherwise the "Ready" message.
  ///   3. `statusShortcutHints`: trailing shortcut reminder appropriate
  ///      to current state (selection multi-modifiers when there's an
  ///      active selection, context-menu hint while drawing-only).
  const selectedOpForHint = $derived(
    project.selectedOpId == null
      ? null
      : (project.operations.find((o) => o.id === project.selectedOpId) ?? null),
  );
  const tabPlacementForHint = $derived(
    !!selectedOpForHint &&
      (selectedOpForHint.kind === 'profile' || selectedOpForHint.kind === 'pocket') &&
      (selectedOpForHint.tabMode?.kind === 'manual' ||
        selectedOpForHint.tabMode?.kind === 'mixed'),
  );
  const modalStatusHint = $derived.by<string | null>(() => {
    if (
      project.pickMode?.kind === 'approach-point' &&
      project.pickMode.opId === project.selectedOpId
    ) {
      return 'Picking approach point — click in canvas to place · Shift = disable snap · ESC = finalize';
    }
    if (tabPlacementForHint) {
      return 'Tab placement active — click contour to add a tab · click an existing tab to remove · set Tabs = Off to exit';
    }
    return null;
  });
  /// pbi4: when the canvas selection is non-empty, the status bar
  /// shows the union bbox of selected objects as (center · L × W).
  /// Empty selection falls back to the import-wide bbox + segment
  /// count so the user still sees the drawing's extent.
  const statusInfoText = $derived.by<string>(() => {
    const imp = project.transformedImport;
    if (!imp) return 'Ready';
    const meta = imp.object_meta ?? [];
    const sel = project.selectedObjects;
    if (sel.size > 0 && meta.length > 0) {
      let minX = Infinity;
      let minY = Infinity;
      let maxX = -Infinity;
      let maxY = -Infinity;
      let counted = 0;
      for (const id of sel) {
        const m = meta[id - 1];
        if (!m) continue;
        if (m.bbox.min_x < minX) minX = m.bbox.min_x;
        if (m.bbox.min_y < minY) minY = m.bbox.min_y;
        if (m.bbox.max_x > maxX) maxX = m.bbox.max_x;
        if (m.bbox.max_y > maxY) maxY = m.bbox.max_y;
        counted += 1;
      }
      if (counted > 0) {
        const cx = (minX + maxX) * 0.5;
        const cy = (minY + maxY) * 0.5;
        const w = Math.max(0, maxX - minX);
        const h = Math.max(0, maxY - minY);
        const tag = counted === 1 ? '1 object' : `${counted} objects`;
        return `${tag} · center=(${cx.toFixed(2)}, ${cy.toFixed(2)}) · ${w.toFixed(2)} × ${h.toFixed(2)} mm`;
      }
    }
    const minX = imp.bbox.min_x.toFixed(2);
    const minY = imp.bbox.min_y.toFixed(2);
    const maxX = imp.bbox.max_x.toFixed(2);
    const maxY = imp.bbox.max_y.toFixed(2);
    return `bbox=(${minX},${minY})–(${maxX},${maxY}) · ${imp.segments.length} segments · unit_scale=${imp.unit_scale}`;
  });
  const statusShortcutHints = $derived.by<string | null>(() => {
    if (!project.transformedImport) return null;
    if (project.selectedEntities.size > 0) {
      return 'Shift = add range · Ctrl/⌘ = toggle · ESC = clear · right-click for context menu';
    }
    return 'Click to select · Shift/Ctrl to multi-select · right-click for context menu · ? for shortcuts';
  });
  const statusBarText = $derived.by<string>(() => {
    if (statusShortcutHints) return `${statusInfoText} · ${statusShortcutHints}`;
    return statusInfoText;
  });
</script>

<svelte:window
  onkeydown={onKeyDown}
  onclick={onWindowClick}
  ondragenter={onDragEnter}
  ondragover={onDragOver}
  ondragleave={onDragLeave}
  ondrop={onDrop}
  onresize={onWindowResize}
/>

<div class="app">
  <!-- ============== MENU BAR =================================== -->
  <nav class="menubar" aria-label="Main menu">
    <div class="menu" class:open={openMenu === 'file'}>
      <button
        type="button"
        class="menu-btn"
        onclick={() => toggleMenu('file')}
        aria-haspopup="menu"
        aria-expanded={openMenu === 'file'}>File</button
      >
      {#if openMenu === 'file'}
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <div class="dropdown" role="menu" tabindex="-1" onmouseleave={closeAllMenus} onkeydown={onMenuKey} use:focusFirstMenuItemAction>
          <button role="menuitem" class="item" onclick={() => pickMenu(openFile)}>
            <span class="label">Open file…</span><span class="kbd">Ctrl+O</span>
          </button>
          <button role="menuitem" class="item" onclick={() => pickMenu(openProject)}>
            <span class="label">Open project…</span>
          </button>
          <button
            role="menuitem"
            class="item"
            disabled={!project.transformedImport}
            onclick={() => pickMenu(saveProject)}
          >
            <span class="label">Save project…</span><span class="kbd">Ctrl+S</span>
          </button>
          <button
            role="menuitem"
            class="item"
            disabled={!project.generated}
            onclick={() => pickMenu(exportGcode)}
          >
            <span class="label">Export G-code…</span>
          </button>
          <button
            role="menuitem"
            class="item"
            onclick={() => pickMenu(() => (reportOpen = true))}
            title="Printable project summary — toolpath stats, time estimate, tools, ops, warnings."
          >
            <span class="label">Report…</span>
          </button>
          <div class="divider"></div>
          <div class="submenu">
            <div class="sub-head">Samples</div>
            {#each SAMPLES as s (s.url)}
              <button
                role="menuitem"
                class="item"
                onclick={() => pickMenu(() => loadSample(s.url))}
              >
                <span class="label">{s.label}</span>
              </button>
            {/each}
          </div>
          <div class="divider"></div>
          <div class="submenu">
            <div class="sub-head">Recent projects</div>
            {#if recentProjects.length === 0}
              <div class="item empty">No recent projects</div>
            {:else}
              {#each recentProjects as r (r.path)}
                <button
                  role="menuitem"
                  class="item"
                  title={r.path}
                  onclick={() => clickRecent(r.path)}
                >
                  <span class="label">{r.filename}</span>
                </button>
              {/each}
              <button
                role="menuitem"
                class="item subtle"
                onclick={clickClearRecents}
                title="Clear the recent projects list"
              >
                <span class="label">Clear recent projects</span>
              </button>
            {/if}
          </div>
        </div>
      {/if}
    </div>

    <div class="menu" class:open={openMenu === 'edit'}>
      <button
        type="button"
        class="menu-btn"
        onclick={() => toggleMenu('edit')}
        aria-haspopup="menu"
        aria-expanded={openMenu === 'edit'}>Edit</button
      >
      {#if openMenu === 'edit'}
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <div class="dropdown" role="menu" tabindex="-1" onmouseleave={closeAllMenus} onkeydown={onMenuKey} use:focusFirstMenuItemAction>
          <button
            role="menuitem"
            class="item"
            class:shake={undoShakeAt > 0}
            onanimationend={() => (undoShakeAt = 0)}
            disabled={!canUndo}
            onclick={doUndo}
          >
            <span class="label">Undo{undoLabel ? `: ${undoLabel}` : ''}</span>
            <span class="kbd">Ctrl+Z</span>
          </button>
          <button
            role="menuitem"
            class="item"
            class:shake={redoShakeAt > 0}
            onanimationend={() => (redoShakeAt = 0)}
            disabled={!canRedo}
            onclick={doRedo}
          >
            <span class="label">Redo{redoLabel ? `: ${redoLabel}` : ''}</span>
            <span class="kbd">Ctrl+Y</span>
          </button>
        </div>
      {/if}
    </div>

    <div class="menu" class:open={openMenu === 'view'}>
      <button
        type="button"
        class="menu-btn"
        onclick={() => toggleMenu('view')}
        aria-haspopup="menu"
        aria-expanded={openMenu === 'view'}>View</button
      >
      {#if openMenu === 'view'}
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <div class="dropdown" role="menu" tabindex="-1" onmouseleave={closeAllMenus} onkeydown={onMenuKey} use:focusFirstMenuItemAction>
          <button
            role="menuitem"
            class="item"
            class:checked={activePane === '2d'}
            onclick={() => pickMenu(() => (activePane = '2d'))}
          >
            <span class="label">2D view</span>
          </button>
          <button
            role="menuitem"
            class="item"
            class:checked={activePane === '3d'}
            onclick={() => pickMenu(() => (activePane = '3d'))}
          >
            <span class="label">3D view</span>
          </button>
          <div class="divider"></div>
          <button
            role="menuitem"
            class="item"
            class:checked={gcodeOpen}
            disabled={!project.generated}
            onclick={() => pickMenu(() => (gcodeOpen = !gcodeOpen))}
          >
            <span class="label">G-code panel</span>
          </button>
        </div>
      {/if}
    </div>

    <div class="menu" class:open={openMenu === 'tools'}>
      <button
        type="button"
        class="menu-btn"
        onclick={() => toggleMenu('tools')}
        aria-haspopup="menu"
        aria-expanded={openMenu === 'tools'}>Tools</button
      >
      {#if openMenu === 'tools'}
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <div class="dropdown" role="menu" tabindex="-1" onmouseleave={closeAllMenus} onkeydown={onMenuKey} use:focusFirstMenuItemAction>
          <button role="menuitem" class="item" onclick={() => pickMenu(() => (toolsOpen = true))}>
            <span class="label">Tool library…</span>
          </button>
          <button role="menuitem" class="item" onclick={() => pickMenu(() => (machineOpen = true))}>
            <span class="label">Machine…</span>
          </button>
          <button
            role="menuitem"
            class="item"
            onclick={() => pickMenu(() => (settingsOpen = true))}
          >
            <span class="label">Settings…</span>
          </button>
        </div>
      {/if}
    </div>

    <div class="menu" class:open={openMenu === 'help'}>
      <button
        type="button"
        class="menu-btn"
        onclick={() => toggleMenu('help')}
        aria-haspopup="menu"
        aria-expanded={openMenu === 'help'}>Help</button
      >
      {#if openMenu === 'help'}
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <div class="dropdown" role="menu" tabindex="-1" onmouseleave={closeAllMenus} onkeydown={onMenuKey} use:focusFirstMenuItemAction>
          <button
            role="menuitem"
            class="item"
            onclick={() => pickMenu(() => (shortcutHelpOpen = true))}
          >
            <span class="label">Keyboard shortcuts…</span><span class="kbd">?</span>
          </button>
          <button
            role="menuitem"
            class="item"
            onclick={() => pickMenu(() => (aboutOpen = true))}
          >
            <span class="label">About wiaConstructor…</span>
          </button>
        </div>
      {/if}
    </div>
  </nav>

  <!-- ============== TOOLBAR (single row) ====================== -->
  <div class="toolbar">
    <button
      class="tb-btn primary"
      onclick={() => openFile()}
      disabled={project.loading}
      title="Open a DXF or SVG file (Ctrl+O)"
    >
      Open file
    </button>
    <button
      class="tb-btn"
      onclick={() => openProject()}
      disabled={project.loading}
      title="Open a saved .wiac-project.json"
    >
      Open project
    </button>
    <button
      class="tb-btn"
      onclick={() => saveProject()}
      disabled={!project.transformedImport}
      title="Save the current project (Ctrl+S)"
    >
      Save
    </button>
    <span class="tb-sep"></span>
    <button
      class="tb-btn icon"
      onclick={() => (addTextOpen = true)}
      title="Add text geometry (T)"
      aria-label="Add Text"
    >
      <svg
        viewBox="0 0 24 24"
        width="14"
        height="14"
        fill="none"
        stroke="currentColor"
        stroke-width="2"
        stroke-linecap="round"
        stroke-linejoin="round"
        aria-hidden="true"
      >
        <path d="M12 19l7-7 3 3-7 7-3-3z"></path>
        <path d="M18 13l-1.5-7.5L2 2l3.5 14.5L13 18l5-5z"></path>
        <path d="M2 2l7.586 7.586"></path>
        <circle cx="11" cy="11" r="2"></circle>
      </svg>
      <span>Text</span>
    </button>
    <span class="tb-sep"></span>
    <!-- anvm: always-visible entries for the three config dialogs that
         otherwise only live under the Tools menu. Frequent edits, easy
         to miss behind menu bar. Menu items stay for keyboard / muscle
         memory. -->
    <button
      class="tb-btn icon config"
      onclick={() => (machineOpen = true)}
      title="Machine settings — work area, units, post-processor"
      aria-label="Open Machine dialog"
    >
      M
    </button>
    <button
      class="tb-btn icon config"
      onclick={() => (toolsOpen = true)}
      title="Tool library — cutters, feeds, speeds"
      aria-label="Open Tools dialog"
    >
      T
    </button>
    <button
      class="tb-btn icon config"
      onclick={() => (settingsOpen = true)}
      title="App settings — theme, sim safety, cutting preview, auto-regenerate"
      aria-label="Open Settings dialog"
    >
      ⚙
    </button>
    <span class="tb-sep"></span>
    <GenerateBar />
    <span class="tb-flex"></span>
    {#if project.generated && project.generated.regions && project.generated.regions.length > 0}
      <label
        class="region-toggle"
        title="Show / hide the translucent fill that marks each pocket operation's machined region."
      >
        <input
          type="checkbox"
          checked={project.regionsVisible}
          onchange={(e) =>
            (project.regionsVisible = (e.currentTarget as HTMLInputElement).checked)}
        />
        <span>Regions</span>
      </label>
    {/if}
    <div
      class="pane-toggle"
      role="tablist"
      aria-label="Viewport mode"
      tabindex="-1"
      onkeydown={onPaneTablistKey}
    >
      <button
        type="button"
        role="tab"
        aria-selected={activePane === '2d'}
        tabindex={activePane === '2d' ? 0 : -1}
        class:active={activePane === '2d'}
        onclick={() => (activePane = '2d')}>2D</button
      >
      <button
        type="button"
        role="tab"
        aria-selected={activePane === '3d'}
        tabindex={activePane === '3d' ? 0 : -1}
        class:active={activePane === '3d'}
        onclick={onClick3dButton}
        title="Click to switch to 3D. Click again to cycle preview mode: both → wireframe → solid. Shift+click reverses."
      >
        {threeDLabel}
      </button>
    </div>
  </div>

  <FileUpload />

  <!-- ============== SPLIT VIEW ================================ -->
  <main class="split" style:--sidebar-width="{sidebarWidth}px">
    <section class="viewport">
      <div class="canvas-area">
        <div class:pane-hidden={activePane !== '2d'} class="pane">
          <EntityCanvas2D
            onShowHelp={() => (shortcutHelpOpen = true)}
            onActivateSidebarPane={revealSidebarPane}
          />
        </div>
        {#if Scene3D}
          {@const C = Scene3D}
          <div class:pane-hidden={activePane !== '3d'} class="pane">
            <C />
          </div>
        {:else if activePane === '3d'}
          <p class="loading-3d">Loading 3D…</p>
        {/if}
        <LoadingOverlay visible={project.loading} message={project.loadingMessage} />
      </div>
      {#if project.generated}
        <PlaybackBar />
        <div class="gcode-toggle">
          <button
            class:active={gcodeOpen}
            onclick={() => (gcodeOpen = !gcodeOpen)}
            title="Show / hide the G-code text panel. Click a line to scrub the playhead; the playhead's current line scrolls into view."
          >
            {gcodeOpen ? '▼' : '▶'}
            G-code
            <span class="hint">{project.generated.gcode.split('\n').length} lines</span>
          </button>
        </div>
        {#if gcodeOpen}
          <Splitter
            direction="vertical"
            onResize={onGcodeResize}
            onReset={resetGcode}
            title="Drag to resize the G-code panel · double-click to reset"
          />
          <div class="gcode-row" style:height="{gcodeHeight}px">
            {#if GcodePanel}
              {@const C = GcodePanel}
              <C />
            {/if}
          </div>
        {/if}
      {/if}
    </section>
    <Splitter
      direction="horizontal"
      onResize={onSidebarResize}
      onReset={resetSidebar}
      title="Drag to resize the side panel · double-click to reset"
    />
    <aside class="sidebar" data-active={activeSidebarPane}>
      <div class="stock-host" class:active={activeSidebarPane === 'stock'}>
        <button
          type="button"
          class="group-head"
          onclick={() => activateSidebarPane('stock')}
          aria-expanded={activeSidebarPane === 'stock'}
          title={activeSidebarPane === 'stock'
            ? 'Collapse stock (return to previous panel)'
            : 'Expand stock settings'}
        >
          <span class="caret">{activeSidebarPane === 'stock' ? '▾' : '▸'}</span>
          <span class="stock-name">Stock</span>
          <span class="stock-dims" title="Current stock dimensions (Length × Width × Thickness) in mm">
            {stockDimsLabel}
          </span>
        </button>
        {#if activeSidebarPane === 'stock'}
          <div class="group-body">
            <StockPanel />
          </div>
        {/if}
      </div>
      <div class="layers-host" class:active={activeSidebarPane === 'layers'}>
        <LayerList
          active={activeSidebarPane === 'layers'}
          onActivate={() => (activateSidebarPane('layers'))}
          onOpenFileClick={() => openFile()}
          onAddTextClick={() => (addTextOpen = true)}
          {reopenPrompt}
          onReopenAccept={acceptReopen}
          onReopenDismiss={dismissReopen}
        />
      </div>
      <div class="text-list-host" class:active={activeSidebarPane === 'text'}>
        <TextList
          active={activeSidebarPane === 'text'}
          onActivate={() => (activateSidebarPane('text'))}
          onAddText={() => (addTextOpen = true)}
        />
      </div>
      <div class="ops-host" class:active={activeSidebarPane === 'operations'}>
        <OperationsList
          active={activeSidebarPane === 'operations'}
          onActivate={() => (activateSidebarPane('operations'))}
        />
      </div>
    </aside>
  </main>

  {#if MachineDialog}
    {@const C = MachineDialog}
    <C open={machineOpen} onClose={() => (machineOpen = false)} />
  {/if}
  {#if ToolLibraryDialog}
    {@const C = ToolLibraryDialog}
    <C
      open={toolsOpen}
      onClose={() => {
        toolsOpen = false;
        project.toolsDialogFocusId = null;
      }}
    />
  {/if}
  {#if SettingsDialog}
    {@const C = SettingsDialog}
    <C open={settingsOpen} onClose={() => (settingsOpen = false)} />
  {/if}
  {#if AddTextDialog}
    {@const C = AddTextDialog}
    <C open={addTextOpen} onClose={() => (addTextOpen = false)} />
  {/if}
  {#if shortcutHelpOpen && ShortcutHelp}
    {@const C = ShortcutHelp}
    <C onClose={() => (shortcutHelpOpen = false)} />
  {/if}
  {#if reportOpen && ReportDialog}
    {@const C = ReportDialog}
    <C open={reportOpen} onClose={() => (reportOpen = false)} />
  {/if}
  {#if aboutOpen && AboutDialog}
    {@const C = AboutDialog}
    <C onClose={() => (aboutOpen = false)} />
  {/if}
  <ConfirmPrompt />

  <footer
    class:footer-pick={modalStatusHint != null}
    title={modalStatusHint ?? statusBarText}
  >
    {#if modalStatusHint}
      {modalStatusHint}
    {:else}
      <span class="status-info">{statusInfoText}</span>
      {#if statusShortcutHints}
        <span class="status-sep">·</span>
        <span class="status-shortcuts">{statusShortcutHints}</span>
      {/if}
    {/if}
  </footer>
  {#if dragOver}
    <div class="drop-overlay" aria-hidden="true">
      <div class="drop-card">
        <div class="drop-glyph">⤓</div>
        <div class="drop-title">Drop to open</div>
        <div class="drop-sub">DXF / SVG drawings · .wiac-project files</div>
      </div>
    </div>
  {/if}
</div>

<style>
  .app {
    /* Flex column instead of grid so optional rows (reopen banner,
       dialogs rendered as direct children) can't shift the 1fr slot
       onto the footer. main.split owns the flex:1; everything else is
       fixed-height auto. */
    display: flex;
    flex-direction: column;
    height: 100vh;
    width: 100vw;
  }
  .app > .menubar,
  .app > .toolbar {
    flex: 0 0 auto;
  }
  .app > main.split {
    flex: 1 1 auto;
    min-height: 0;
  }
  .app > footer {
    flex: 0 0 auto;
  }

  /* ---------- menu bar ----------------------------------------- */
  .menubar {
    display: flex;
    align-items: stretch;
    background: var(--bg-panel);
    border-bottom: 1px solid var(--border);
    padding: 0 0.25rem;
    min-height: 1.85rem;
  }
  .menu {
    position: relative;
  }
  .menu-btn {
    background: transparent;
    color: var(--text);
    border: 0;
    padding: 0.25rem 0.7rem;
    font-size: 0.82rem;
    cursor: pointer;
    border-radius: 3px;
    line-height: 1.3;
  }
  .menu-btn:hover {
    background: var(--bg-elevated);
  }
  .menu.open .menu-btn {
    background: var(--bg-elevated);
    color: var(--text-strong);
  }
  .dropdown {
    position: absolute;
    top: 100%;
    left: 0;
    min-width: 240px;
    background: var(--bg-elevated);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 4px;
    box-shadow: 0 6px 18px var(--shadow-modal);
    padding: 0.2rem;
    z-index: var(--z-dropdown);
    display: flex;
    flex-direction: column;
    gap: 0.05rem;
  }
  .dropdown .item {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 0.7rem;
    background: transparent;
    color: var(--text);
    border: 0;
    padding: 0.3rem 0.55rem;
    font-size: 0.78rem;
    border-radius: 3px;
    cursor: pointer;
    text-align: left;
    width: 100%;
  }
  .dropdown .item:hover:not(:disabled) {
    background: color-mix(in srgb, var(--accent) 16%, transparent);
  }
  .dropdown .item:disabled {
    color: var(--text-faint);
    cursor: not-allowed;
  }
  .dropdown .item.checked .label::before {
    content: '✓ ';
    color: var(--accent);
  }
  .dropdown .item.empty {
    color: var(--text-faint);
    font-style: italic;
    cursor: default;
  }
  .dropdown .item.subtle {
    color: var(--text-muted);
    font-size: 0.72rem;
  }
  .dropdown .label {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    /* Cap relative to viewport so wide windows can show longer filenames
       (Recent Projects in particular); narrow windows still ellipsis. */
    max-width: min(420px, 80vw);
  }
  .dropdown .kbd {
    color: var(--text-muted);
    font-size: 0.7rem;
    font-variant-numeric: tabular-nums;
  }
  .dropdown .divider {
    height: 1px;
    background: var(--border);
    margin: 0.2rem 0.05rem;
  }
  .dropdown .submenu {
    display: flex;
    flex-direction: column;
    gap: 0.05rem;
  }
  .dropdown .sub-head {
    padding: 0.25rem 0.55rem 0.05rem;
    font-size: 0.62rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-muted);
  }
  /* Shake animation on undo/redo when stack is empty. */
  @keyframes wiac-undo-shake {
    0% {
      transform: translateX(0);
    }
    25% {
      transform: translateX(-3px);
    }
    50% {
      transform: translateX(3px);
    }
    75% {
      transform: translateX(-2px);
    }
    100% {
      transform: translateX(0);
    }
  }
  .dropdown .item.shake {
    animation: wiac-undo-shake 100ms ease-in-out;
  }

  /* ---------- toolbar ------------------------------------------ */
  .toolbar {
    display: flex;
    align-items: center;
    gap: 0.45rem;
    padding: 0.35rem 0.7rem;
    background: var(--bg-panel);
    border-bottom: 1px solid var(--border);
    flex-wrap: wrap;
  }
  .toolbar :global(.bar) {
    /* GenerateBar's inner `.bar` — flatten its panel background so it
       inherits the toolbar styling and doesn't render a second band. */
    background: transparent;
    border: 0;
    padding: 0;
    gap: 0.45rem;
  }
  .tb-btn {
    background: var(--bg-elevated);
    color: var(--text);
    border: 1px solid var(--border);
    padding: 0.28rem 0.7rem;
    border-radius: 3px;
    font-size: 0.78rem;
    cursor: pointer;
    line-height: 1.15;
    white-space: nowrap;
  }
  .tb-btn:hover:not(:disabled) {
    background: color-mix(in srgb, var(--accent) 14%, var(--bg-elevated));
    border-color: var(--accent);
    color: var(--text-strong);
  }
  .tb-btn:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }
  .tb-btn.primary {
    background: var(--accent);
    color: white;
    border-color: var(--accent);
  }
  .tb-btn.primary:hover:not(:disabled) {
    background: var(--accent-strong);
    border-color: var(--accent-strong);
    color: white;
  }
  .tb-btn.icon {
    display: inline-flex;
    align-items: center;
    gap: 0.35rem;
  }
  /* anvm: single-glyph config-dialog buttons (M / T / ⚙). Slightly
     bigger glyph + square button so they read as icons rather than
     truncated text. */
  .tb-btn.icon.config {
    min-width: 1.8rem;
    justify-content: center;
    font-weight: 600;
    padding: 0.3rem 0.4rem;
  }
  .tb-sep {
    width: 1px;
    height: 1.4rem;
    background: var(--border);
    margin: 0 0.1rem;
  }
  .tb-flex {
    flex: 1;
  }
  .pane-toggle {
    display: inline-flex;
    border: 1px solid var(--border);
    border-radius: 4px;
    overflow: hidden;
  }
  .pane-toggle button {
    background: var(--bg-elevated);
    color: var(--text-muted);
    border: 0;
    padding: 0.3rem 0.7rem;
    font-size: 0.78rem;
    cursor: pointer;
  }
  .pane-toggle button.active {
    background: var(--accent);
    color: white;
  }

  /* ---------- split view ------------------------------------- */
  .split {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto var(--sidebar-width, 360px);
    overflow: hidden;
    min-height: 0;
  }
  .viewport {
    position: relative;
    overflow: hidden;
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .canvas-area {
    flex: 1;
    min-height: 0;
    position: relative;
  }
  .pane {
    width: 100%;
    height: 100%;
  }
  .pane-hidden {
    display: none;
  }
  .loading-3d {
    display: flex;
    height: 100%;
    align-items: center;
    justify-content: center;
    color: var(--text-muted);
    font-size: 0.85rem;
  }
  .gcode-toggle {
    display: flex;
    border-top: 1px solid var(--border);
    background: var(--bg-panel);
  }
  .gcode-toggle button {
    background: transparent;
    color: var(--text-muted);
    border: 0;
    padding: 0.2rem 0.7rem;
    font-size: 0.72rem;
    cursor: pointer;
    display: inline-flex;
    align-items: center;
    gap: 0.5rem;
  }
  .gcode-toggle button.active {
    color: var(--text-strong);
  }
  .gcode-toggle .hint {
    color: var(--text-faint);
    font-size: 0.7rem;
  }
  .gcode-row {
    background: var(--bg-input);
    overflow: hidden;
    min-height: 0;
  }
  .sidebar {
    display: grid;
    /* Accordion: exactly one of the four hosts is `.active` — that
       row gets the 1fr space, the others collapse to their header
       strip (`auto`). `data-active` drives which row goes wide via
       the four matched `grid-template-rows` declarations below. */
    grid-template-rows: auto auto auto auto;
    min-height: 0;
    min-width: 0;
    overflow: hidden;
  }
  .sidebar[data-active='stock'] {
    grid-template-rows: minmax(0, 1fr) auto auto auto;
  }
  .sidebar[data-active='layers'] {
    grid-template-rows: auto minmax(0, 1fr) auto auto;
  }
  .sidebar[data-active='text'] {
    grid-template-rows: auto auto minmax(0, 1fr) auto;
  }
  .sidebar[data-active='operations'] {
    grid-template-rows: auto auto auto minmax(0, 1fr);
  }
  .stock-host,
  .layers-host,
  .text-list-host,
  .ops-host {
    min-height: 0;
    min-width: 0;
    /* overflow: visible so per-panel dropdowns (Add+ etc.) escape the
       row boundary. The sidebar itself + inner scrollable lists handle
       their own clipping where it matters. */
    overflow: visible;
  }
  /* When a host is `.active` (1fr row) we need to clip overflow on the
     host wrapper so the inner list scrolls instead of pushing the
     sibling hosts off-screen. */
  .stock-host.active,
  .layers-host.active,
  .text-list-host.active,
  .ops-host.active {
    overflow: hidden;
  }
  .stock-host {
    background: var(--bg-panel);
    padding: 0.4rem 0.6rem 0.5rem;
    overflow: visible;
    border-bottom: 1px solid var(--border);
  }
  .stock-host.active {
    overflow: auto;
  }
  /* Base `.group-head` shape lives in app.css; stock-host only sets the
     per-panel grid (caret · name · dims-readout). The button-tag adds
     `font-family: inherit` so the <button>-as-group-head doesn't render
     as system-monospace. */
  .stock-host .group-head {
    grid-template-columns: auto auto minmax(0, 1fr);
    width: 100%;
    color: var(--text-strong);
    font-weight: 600;
    font-family: inherit;
    text-align: left;
  }
  .stock-host .caret {
    color: var(--text-muted);
    font-size: 0.85rem;
    line-height: 1;
  }
  .stock-name {
    color: var(--text-strong);
  }
  .stock-dims {
    color: var(--text-muted);
    font-weight: 500;
    font-variant-numeric: tabular-nums;
    font-size: 0.72rem;
    text-align: right;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .stock-host .group-body {
    margin: 0.2rem 0 0 0.5rem;
    padding-left: 0.3rem;
    border-left: 2px solid color-mix(in srgb, var(--accent) 30%, transparent);
  }
  .region-toggle {
    /* Lives in the toolbar next to the 2D/3D switch (visible only
       when a Generate has produced regions). Compact label so it
       reads as a peer of the pane-toggle pills. */
    display: inline-flex;
    align-items: center;
    gap: 0.3rem;
    font-size: 0.74rem;
    color: var(--text-muted);
    cursor: pointer;
    padding: 0.18rem 0.4rem;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg-elevated);
    user-select: none;
  }
  .region-toggle:hover {
    color: var(--text-strong);
    border-color: var(--accent);
  }
  .region-toggle input[type='checkbox'] {
    accent-color: var(--accent);
  }
  footer {
    /* Fixed-height single-line status bar — never grows. Long content
       truncates with ellipsis; the full text is on the title tooltip. */
    height: 1.6rem;
    line-height: 1.6rem;
    padding: 0 0.9rem;
    background: var(--bg-panel);
    border-top: 1px solid var(--border);
    font-size: 0.75rem;
    color: var(--text-muted);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  footer.footer-pick {
    /* n79: active canvas-pick mode — accent-tinted status bar
       grabs the eye so the user knows the canvas isn't in its
       normal selection mode. */
    background: color-mix(in srgb, var(--accent) 18%, var(--bg-panel));
    color: var(--text);
    font-weight: 600;
  }
  /* dteo: drop overlay while user is dragging a file over the window. */
  .drop-overlay {
    position: fixed;
    inset: 0;
    background: color-mix(in srgb, var(--bg-app) 70%, transparent);
    backdrop-filter: blur(2px);
    z-index: var(--z-floating);
    display: flex;
    align-items: center;
    justify-content: center;
    pointer-events: none;
  }
  .drop-card {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.4rem;
    padding: 2rem 3rem;
    border: 2px dashed var(--accent);
    border-radius: 12px;
    background: color-mix(in srgb, var(--bg-elevated) 92%, transparent);
    box-shadow: 0 6px 18px var(--shadow-modal);
  }
  .drop-glyph {
    font-size: 2.4rem;
    color: var(--accent);
    line-height: 1;
  }
  .drop-title {
    font-size: 1.1rem;
    font-weight: 600;
    color: var(--text-strong);
  }
  .drop-sub {
    font-size: 0.82rem;
    color: var(--text-muted);
  }
  footer .status-sep {
    margin: 0 0.5rem;
    opacity: 0.55;
  }
  footer .status-shortcuts {
    opacity: 0.75;
  }
</style>
