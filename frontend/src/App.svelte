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
  import LoadingOverlay from './lib/components/LoadingOverlay.svelte';
  import Splitter from './lib/components/Splitter.svelte';

  let machineOpen = $state(false);
  let toolsOpen = $state(false);
  let settingsOpen = $state(false);
  let addTextOpen = $state(false);
  let shortcutHelpOpen = $state(false);
  /// Startup banner: when set, the user was previously editing a
  /// project and we offer to reopen it. Styled in-app instead of a
  /// native window.confirm so the first impression of the app isn't
  /// an unstyled OS dialog (audit C10).
  let reopenPrompt = $state<{ path: string; filename: string } | null>(null);

  /// qjec: in-app confirmation shown when the user tries to close the
  /// window with unsaved work. The desktop shell intercepts close,
  /// emits `app:close_requested`, and we either confirm immediately
  /// (no unsaved work) or arm this prompt and wait for the user.
  let closePrompt = $state(false);

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
  import { _ } from 'svelte-i18n';
  import { locale } from './lib/i18n';
  import {
    isDesktop,
    wireSourceWatch as wireDesktopSourceWatch,
    wireCloseRequested,
    confirmClose,
    logErrorToStderr,
    isDebugSession,
    runUpdateCheck,
  } from './lib/state/desktop';
  import { computeFootprint } from './lib/sim/driver';

  /// Live label for the Stock panel summary — shows the current
  /// dimensions inline so the user sees the workpiece size at a glance
  /// without expanding the panel. Uses `computeFootprint` so the
  /// numbers match what Scene3D / sim use (auto mode follows imported
  /// bbox; manual = customX/Y; no-import fallback = machine work area).
  /// Collapsible state for the Stock panel — matches the LayerList /
  /// OperationsList caret-collapse pattern. Default open so a fresh
  /// project shows the stock settings prominently.
  let stockExpanded = $state(true);
  const stockDimsLabel = $derived.by<string>(() => {
    const cfg = project.stock;
    const fp = computeFootprint(project.transformedImport, cfg, project.machine.workArea);
    const x = Math.max(0, fp.maxX - fp.minX);
    const y = Math.max(0, fp.maxY - fp.minY);
    const z = Math.max(0, cfg.thickness);
    const f = (n: number) => (Number.isFinite(n) ? n.toFixed(0) : '0');
    return `${f(x)} × ${f(y)} × ${f(z)} mm`;
  });

  // Keep the i18n locale in sync with the persisted setting on first
  // load. Subsequent changes go through SettingsDialog which calls
  // setLocale itself.
  $effect(() => {
    const cur = $locale;
    if ((cur === 'en' || cur === 'de') && cur !== project.settings.language) {
      // Defer the settings write off the effect flush so the localStorage
      // round-trip + dependent $state mutation don't run inside the
      // reactivity scheduler. Bad practice to mutate $state synchronously
      // from inside another effect — Svelte 5 silently aborts the
      // scheduler on the next throw if it happens during a flush.
      queueMicrotask(() => project.updateSettings({ language: cur }));
    }
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
      const line = `UI error: ${String(msg)}`;
      void logErrorToStderr(line);
      errorBanner?.push(line);
      try {
        project.setError(`UI error: ${String(msg).slice(0, 240)}`);
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
    void workspace.pruneMissingProjects();
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
    // The per-project workspace state restores the user's last layer-
    // visibility selection, but reopens are a fresh session — if the
    // user accidentally hid a layer right before closing they'd open
    // the app to a blank canvas with no obvious "show it" affordance.
    // Reset to all-visible on reopen so the geometry is visible
    // immediately; subsequent toggles still persist within the session.
    if (project.transformedImport) {
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
    void project.transformedImport;
    void project.activeProjectPath;
    if (reopenPrompt && (project.transformedImport || project.activeProjectPath)) {
      reopenPrompt = null;
    }
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
    unlistenCloseRequested = await wireCloseRequested(() => {
      closePrompt = true;
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
  // Sidebar width in px; clamped 240..720. Persisted in workspace so
  // the user's preferred ratio survives restart.
  const SIDEBAR_DEFAULT = 360;
  let sidebarWidth = $state<number>(SIDEBAR_DEFAULT);
  // Gcode panel height in px; clamped 120..720. Default ~35vh.
  const GCODE_DEFAULT = Math.round(window.innerHeight * 0.35);
  let gcodeHeight = $state<number>(GCODE_DEFAULT);

  // Restore persisted sizes from the workspace store once it has loaded.
  $effect(() => {
    void workspace.version;
    const panels = workspace.get().panels;
    if (panels.right_width > 0) sidebarWidth = clampSidebar(panels.right_width);
    if (panels.bottom_height > 0) gcodeHeight = clampGcode(panels.bottom_height);
  });

  function clampSidebar(v: number): number {
    return Math.max(240, Math.min(720, v));
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
  const statusInfoText = $derived.by<string>(() => {
    const imp = project.transformedImport;
    if (imp) {
      return $_('footer.bbox', {
        values: {
          minX: imp.bbox.min_x.toFixed(2),
          minY: imp.bbox.min_y.toFixed(2),
          maxX: imp.bbox.max_x.toFixed(2),
          maxY: imp.bbox.max_y.toFixed(2),
          count: imp.segments.length,
          unit: imp.unit_scale,
        },
      });
    }
    return $_('footer.ready');
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
        <div class="dropdown" role="menu" tabindex="-1" onmouseleave={closeAllMenus}>
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
        <div class="dropdown" role="menu" tabindex="-1" onmouseleave={closeAllMenus}>
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
        <div class="dropdown" role="menu" tabindex="-1" onmouseleave={closeAllMenus}>
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
        <div class="dropdown" role="menu" tabindex="-1" onmouseleave={closeAllMenus}>
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
        <div class="dropdown" role="menu" tabindex="-1" onmouseleave={closeAllMenus}>
          <button
            role="menuitem"
            class="item"
            onclick={() => pickMenu(() => (shortcutHelpOpen = true))}
          >
            <span class="label">Keyboard shortcuts…</span><span class="kbd">?</span>
          </button>
          {#if isDesktop()}
            <button role="menuitem" class="item" onclick={() => pickMenu(runUpdateCheck)}>
              <span class="label">Check for updates…</span>
            </button>
          {/if}
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
      title="App settings — language, sim safety, theme, auto-regenerate"
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
    <div class="pane-toggle" role="tablist" aria-label="Viewport mode">
      <button
        type="button"
        role="tab"
        aria-selected={activePane === '2d'}
        class:active={activePane === '2d'}
        onclick={() => (activePane = '2d')}>{$_('header.pane.2d')}</button
      >
      <button
        type="button"
        role="tab"
        aria-selected={activePane === '3d'}
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
          <EntityCanvas2D onShowHelp={() => (shortcutHelpOpen = true)} />
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
            {$_('bottom.gcode') ?? 'G-code'}
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
    <aside class="sidebar">
      <div class="stock-host">
        <button
          type="button"
          class="group-head"
          onclick={() => (stockExpanded = !stockExpanded)}
          aria-expanded={stockExpanded}
          title="Click to {stockExpanded ? 'collapse' : 'expand'} stock settings"
        >
          <span class="caret">{stockExpanded ? '▾' : '▸'}</span>
          <span class="stock-name">Stock</span>
          <span class="stock-dims" title="Current stock dimensions (Length × Width × Thickness) in mm">
            {stockDimsLabel}
          </span>
        </button>
        {#if stockExpanded}
          <div class="group-body">
            <StockPanel />
          </div>
        {/if}
      </div>
      <div class="layers-host">
        <LayerList
          onOpenFileClick={() => openFile()}
          onAddTextClick={() => (addTextOpen = true)}
          {reopenPrompt}
          onReopenAccept={acceptReopen}
          onReopenDismiss={dismissReopen}
        />
      </div>
      <div class="text-list-host">
        <TextList onAddText={() => (addTextOpen = true)} />
      </div>
      <div class="ops-host">
        <OperationsList />
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
  {#if closePrompt}
    <div
      class="close-prompt-overlay"
      role="dialog"
      aria-modal="true"
      aria-labelledby="close-prompt-title"
    >
      <div class="close-prompt-card">
        <h2 id="close-prompt-title">Quit wiaConstructor?</h2>
        {#if project.dirty}
          <p>You have unsaved changes. They will be lost if you quit now.</p>
        {:else}
          <p>Are you sure you want to quit?</p>
        {/if}
        <div class="close-prompt-actions">
          <button class="secondary" onclick={() => (closePrompt = false)}>
            {project.dirty ? 'Keep editing' : 'Cancel'}
          </button>
          <button
            class="danger"
            onclick={() => {
              closePrompt = false;
              void confirmClose();
            }}
          >
            {project.dirty ? 'Discard & quit' : 'Quit'}
          </button>
        </div>
      </div>
    </div>
  {/if}

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
    box-shadow: 0 6px 18px rgba(0, 0, 0, 0.3);
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
    max-width: 280px;
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
    /* Stock (auto) · Layers (auto) · Text (auto) · Operations (1fr).
       The stock panel sits at the top — it's the always-present
       workpiece every layer/op attaches to. */
    grid-template-rows: auto auto auto minmax(0, 1fr);
    min-height: 0;
    min-width: 0;
    overflow: hidden;
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
  .ops-host {
    /* Operations list is the 1fr row — its own internal panel needs to
       scroll, so re-clip here. */
    overflow: hidden;
  }
  .stock-host {
    background: var(--bg-panel);
    padding: 0.4rem 0.6rem 0.5rem;
    max-height: 50vh;
    overflow: auto;
    border-bottom: 1px solid var(--border);
  }
  /* Stock panel header mirrors LayerList's .group-head so all three
     sidebar panels (Stock / Layers / Operations) share one visual
     language. Caret in the leading slot · name · live dimensions
     readout pinned right. */
  .stock-host .group-head {
    display: grid;
    grid-template-columns: auto auto minmax(0, 1fr);
    gap: 0.3rem;
    align-items: center;
    width: 100%;
    padding: 0.2rem 0.35rem;
    border: 1px solid var(--border);
    border-radius: 3px;
    background: color-mix(in srgb, var(--accent) 6%, var(--bg-panel));
    font-size: 0.78rem;
    line-height: 1.2;
    min-height: 1.55rem;
    box-sizing: border-box;
    color: var(--text-strong);
    font-weight: 600;
    cursor: pointer;
    font-family: inherit;
    text-align: left;
  }
  .stock-host .group-head:hover {
    background: color-mix(in srgb, var(--accent) 12%, var(--bg-panel));
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
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.35);
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
  .close-prompt-overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.45);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 9999;
  }
  .close-prompt-card {
    background: var(--bg-elevated);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 1rem 1.25rem;
    max-width: 28rem;
    box-shadow: 0 8px 32px rgba(0, 0, 0, 0.35);
  }
  .close-prompt-card h2 {
    margin: 0 0 0.4rem 0;
    font-size: 1.05rem;
  }
  .close-prompt-card p {
    margin: 0 0 0.9rem 0;
    color: var(--text-muted);
  }
  .close-prompt-actions {
    display: flex;
    justify-content: flex-end;
    gap: 0.5rem;
  }
  .close-prompt-actions .secondary {
    background: transparent;
    color: var(--text);
    border: 1px solid var(--border);
    padding: 0.35rem 0.9rem;
    border-radius: 3px;
    cursor: pointer;
  }
  .close-prompt-actions .danger {
    background: var(--danger, #c0392b);
    color: white;
    border: 0;
    padding: 0.35rem 0.9rem;
    border-radius: 3px;
    cursor: pointer;
  }
</style>
