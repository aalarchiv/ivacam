<script lang="ts">
  import FileUpload from './lib/components/FileUpload.svelte';
  import ModeSwitchNotice from './lib/components/ModeSwitchNotice.svelte';
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
  type MachineWorkspaceComp = typeof import('./lib/components/MachineWorkspace.svelte').default;
  let MachineWorkspace = $state<MachineWorkspaceComp | null>(null);
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
  type HelpAboutComp = typeof import('./lib/components/HelpAbout.svelte').default;
  let HelpAbout = $state<HelpAboutComp | null>(null);
  let helpAboutLoading = false;
  type ReportDialogComp = typeof import('./lib/components/ReportDialog.svelte').default;
  let ReportDialog = $state<ReportDialogComp | null>(null);
  let reportDialogLoading = false;
  import RecentMenu from './lib/components/RecentMenu.svelte';
  import LoadingOverlay from './lib/components/LoadingOverlay.svelte';
  import Splitter from './lib/components/Splitter.svelte';

  /// Top-level main-window tab. Machine and Tool library are
  /// first-class tabs (not modals); their panels stay mounted once
  /// loaded so in-progress drafts survive tab switches.
  let mainTab = $state<'project' | 'machine' | 'tools' | 'settings' | 'help'>('project');
  let addTextOpen = $state(false);
  let reportOpen = $state(false);
  /// Build-time version stamp baked by vite.config.ts. Surfaces in
  /// the window title and the Help → About dialog so users can
  /// paste an exact build identifier into bug reports.
  const buildVersion =
    typeof __IVAC_BUILD_VERSION__ === 'string' ? __IVAC_BUILD_VERSION__ : 'unknown';

  /// Window title carries the build version so a screenshot pins the
  /// report to the exact binary. Format: "ivaCAM v<pkg>
  /// (<git-describe>)" — package version comes from
  /// frontend/package.json via the `__IVAC_PKG_VERSION__` define
  /// baked by vite.config.ts, git-describe via
  /// `__IVAC_BUILD_VERSION__`. `document.title` updates on every
  /// paint that touches the effect, but it's cheap.
  const pkgVersion = typeof __IVAC_PKG_VERSION__ === 'string' ? __IVAC_PKG_VERSION__ : '0.0.0';
  $effect(() => {
    if (buildVersion && buildVersion !== 'unknown') {
      document.title = `ivaCAM v${pkgVersion} (${buildVersion})`;
    } else {
      document.title = `ivaCAM v${pkgVersion}`;
    }
  });
  // Open the Tool library dialog when OpPropertiesPanel's "edit this
  // tool" icon requests focus on a specific tool row. The dialog reads
  // project.sel.toolsDialogFocusId and handles scroll/highlight.
  $effect(() => {
    if (project.sel.toolsDialogFocusId != null) {
      mainTab = 'tools';
    }
  });

  // G-code panel visibility. The playback bar always sits below the
  // 3D canvas; the gcode panel opens as an extra row beneath it so
  // the user sees the toolpath, the playhead, and the program text
  // simultaneously and can drive each from the others.
  let gcodeOpen = $state(false);
  import { project } from './lib/state/project.svelte';
  import { workspace } from './lib/state/workspace.svelte';
  import ConfirmPrompt from './lib/components/ConfirmPrompt.svelte';
  import { openFile, openProject, saveProject } from './lib/services/file_ops';
  import {
    sessionUi,
    loadWorkspaceAndMaybeReopen,
    acceptReopen,
    dismissReopen,
    dismissReopenOnceLoaded,
    persistPerProjectStateOnChange,
    mirrorMachineProfileOnChange,
    openRecentProject,
    wireSourceWatch,
    wireCloseConfirm,
    unwireSession,
    onDragEnter,
    onDragOver,
    onDragLeave,
    onDrop,
  } from './lib/services/workspace-session.svelte';
  import { onMount } from 'svelte';
  import { logErrorToStderr, isDebugSession } from './lib/state/desktop';
  import { computeFootprint } from './lib/sim/driver';
  import { togglePane, revealPane, type SidebarPane } from './lib/state/sidebar-pane';
  import { resolveShortcut } from './lib/state/app-menu';

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
  // used by programmatic flows.
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
    const cfg = project.data.stock;
    const fp = computeFootprint(project.transformedImport, cfg, project.data.machine.workArea);
    const x = Math.max(0, fp.maxX - fp.minX);
    const y = Math.max(0, fp.maxY - fp.minY);
    const z = Math.max(0, cfg.thickness);
    const f = (n: number) => (Number.isFinite(n) ? n.toFixed(0) : '0');
    return `${f(x)} × ${f(y)} × ${f(z)} mm`;
  });

  onMount(() => {
    document.documentElement.dataset.theme = project.data.settings.theme;

    // Global error capture. Silent throws inside Svelte 5 $effect bodies
    // can abort the reactivity scheduler — every button still fires its
    // onclick, but visible state stops updating. Surface every uncaught
    // error two ways:
    //
    //   1. ALWAYS route through `logErrorToStderr` so terminal users
    //      running the AppImage see the failure on stderr and
    //      journald / log aggregators can capture it.
    //   2. WHEN `IVAC_DEBUG=1` was set on launch, also render a
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
      host.id = 'ivac-error-banner';
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
    return () => unwireSession();
  });

  // Auto-dismiss the reopen banner once a project / drawing is loaded by
  // any path — body (and WHY) in workspace-session; the synchronous
  // project reads inside it register this effect's subscriptions.
  $effect(() => {
    dismissReopenOnceLoaded();
  });

  /// Persist per-project workspace state when the user adjusts visible
  /// layers / selected op / playhead (subscriptions registered inside).
  $effect(() => {
    persistPerProjectStateOnChange();
  });

  /// Mirror machine + tool edits back into the referenced workspace
  /// machine profile (subscriptions registered inside).
  $effect(() => {
    mirrorMachineProfileOnChange();
  });

  $effect(() => {
    document.documentElement.dataset.theme = project.data.settings.theme;
  });

  let activePane = $state<'2d' | '3d'>('2d');

  /// 3D button label cycles with the preview mode: 'both' → "3D",
  /// 'wireframe' → "3Dwire", 'solid' → "3Dsolid". The button does
  /// double duty — first click in 2D mode switches to 3D (keeping the
  /// current preview mode); subsequent clicks cycle modes. Shift+click
  /// reverses the cycle.
  const PREVIEW_CYCLE: ('both' | 'wireframe' | 'solid')[] = ['both', 'wireframe', 'solid'];
  const threeDLabel = $derived.by<string>(() => {
    const m = project.data.settings.previewMode;
    if (m === 'wireframe') return '3Dwire';
    if (m === 'solid') return '3Dsolid';
    return '3D';
  });
  function onClick3dButton(e: MouseEvent) {
    if (activePane !== '3d') {
      activePane = '3d';
      return;
    }
    const i = PREVIEW_CYCLE.indexOf(project.data.settings.previewMode);
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
    if (project.gen.generated) activePane = '3d';
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
    if (mainTab === 'machine' && !MachineWorkspace && !machineDialogLoading) {
      machineDialogLoading = true;
      void import('./lib/components/MachineWorkspace.svelte').then((m) => {
        MachineWorkspace = m.default;
        machineDialogLoading = false;
      });
    }
  });
  $effect(() => {
    if (mainTab === 'tools' && !ToolLibraryDialog && !toolLibraryDialogLoading) {
      toolLibraryDialogLoading = true;
      void import('./lib/components/ToolLibraryDialog.svelte').then((m) => {
        ToolLibraryDialog = m.default;
        toolLibraryDialogLoading = false;
      });
    }
  });
  $effect(() => {
    if (mainTab === 'settings' && !SettingsDialog && !settingsDialogLoading) {
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
    if (mainTab === 'help' && !HelpAbout && !helpAboutLoading) {
      helpAboutLoading = true;
      void import('./lib/components/HelpAbout.svelte').then((m) => {
        HelpAbout = m.default;
        helpAboutLoading = false;
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

  // Keyboard shortcut dispatch. The decision ("which action?") is the pure
  // `resolveShortcut` in lib/state/app-menu.ts (unit-tested); App.svelte is
  // the shell that performs the component-coupled effect for each action.
  function onKeyDown(e: KeyboardEvent) {
    const res = resolveShortcut(e);
    if (!res) return;
    if (res.preventDefault) e.preventDefault();
    switch (res.action) {
      case 'undo':
        project.undo();
        break;
      case 'redo':
        project.redo();
        break;
      case 'open':
        void openFile();
        break;
      case 'save':
        void saveProject();
        break;
      case 'escape':
        if (project.sel.selectedEntities.size > 0) project.sel.selectedEntities = new Set();
        break;
      case 'add-text':
        addTextOpen = true;
        break;
      case 'shortcut-help':
        mainTab = 'help';
        break;
    }
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
    project.sel.selectedOpId == null
      ? null
      : (project.data.operations.find((o) => o.id === project.sel.selectedOpId) ?? null),
  );
  const tabPlacementForHint = $derived(
    !!selectedOpForHint &&
      (selectedOpForHint.kind === 'profile' || selectedOpForHint.kind === 'pocket') &&
      (selectedOpForHint.tabMode?.kind === 'manual' || selectedOpForHint.tabMode?.kind === 'mixed'),
  );
  const modalStatusHint = $derived.by<string | null>(() => {
    if (
      project.sel.pickMode?.kind === 'approach-point' &&
      project.sel.pickMode.opId === project.sel.selectedOpId
    ) {
      return 'Picking approach point — click in canvas to place · Shift = disable snap · ESC = finalize';
    }
    if (tabPlacementForHint) {
      return 'Tab placement active — click contour to add a tab · click an existing tab to remove · set Tabs = Off to exit';
    }
    return null;
  });
  /// When the canvas selection is non-empty, the status bar shows the
  /// union bbox of selected objects as (center · L × W). Empty
  /// selection falls back to the import-wide bbox + segment count so
  /// the user still sees the drawing's extent.
  const statusInfoText = $derived.by<string>(() => {
    const imp = project.transformedImport;
    if (!imp) return 'Ready';
    const meta = imp.object_meta ?? [];
    const sel = project.sel.selectedObjects;
    if (sel.size > 0 && meta.length > 0) {
      // Object ids are NOT a dense 1-based index into `meta` —
      // combineImports namespaces later drawings' ids by an offset, so
      // `meta[id - 1]` reads the wrong (or no) entry once a second drawing
      // is added. Resolve by id, like seriesSelectTo does.
      const byId = new Map<number, (typeof meta)[number]>();
      for (const m of meta) byId.set(m.id, m);
      let minX = Infinity;
      let minY = Infinity;
      let maxX = -Infinity;
      let maxY = -Infinity;
      let counted = 0;
      for (const id of sel) {
        const m = byId.get(id);
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
    if (project.sel.selectedEntities.size > 0) {
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
  ondragenter={onDragEnter}
  ondragover={onDragOver}
  ondragleave={onDragLeave}
  ondrop={onDrop}
  onresize={onWindowResize}
/>

<div class="app">
  <!-- ============== MAIN TABS ================================= -->
  <nav class="main-tabs" aria-label="Main areas">
    <button
      type="button"
      class="main-tab"
      class:active={mainTab === 'project'}
      onclick={() => (mainTab = 'project')}>Project</button
    >
    <button
      type="button"
      class="main-tab"
      class:active={mainTab === 'machine'}
      onclick={() => (mainTab = 'machine')}
      title="Active machine, its tooling, and its settings">Machine</button
    >
    <button
      type="button"
      class="main-tab"
      class:active={mainTab === 'tools'}
      onclick={() => (mainTab = 'tools')}
      title="The shop's tool inventory">Tool library</button
    >
    <span class="main-tabs-flex"></span>
    <button
      type="button"
      class="main-tab"
      class:active={mainTab === 'settings'}
      onclick={() => (mainTab = 'settings')}
      title="App settings — theme, sim safety, performance">Settings</button
    >
    <button
      type="button"
      class="main-tab"
      class:active={mainTab === 'help'}
      onclick={() => (mainTab = 'help')}
      title="Keyboard shortcuts, usage help, version and license">Help/About</button
    >
  </nav>

  <!-- ============== TOOLBAR (single row) ====================== -->
  <div class="toolbar" class:tab-hidden={mainTab !== 'project'}>
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
      title="Open a saved .ivac-project.json"
    >
      Open project
    </button>
    <RecentMenu onOpen={(path) => void openRecentProject(path)} />
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
      onclick={() => (reportOpen = true)}
      title="Report a problem — collects version + project context for a bug report"
      aria-label="Report a problem"
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
        <path d="M4 15s1-1 4-1 5 2 8 2 4-1 4-1V3s-1 1-4 1-5-2-8-2-4 1-4 1z"></path>
        <line x1="4" y1="22" x2="4" y2="15"></line>
      </svg>
      <span>Report</span>
    </button>
    <span class="tb-sep"></span>
    <button
      class="tb-btn icon"
      onclick={() => project.undo()}
      disabled={!project.canUndo()}
      title={project.canUndo() ? `Undo ${project.undoLabel() ?? ''} (Ctrl+Z)` : 'Nothing to undo'}
      aria-label="Undo"
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
        <polyline points="9 14 4 9 9 4"></polyline>
        <path d="M20 20v-7a4 4 0 0 0-4-4H4"></path>
      </svg>
    </button>
    <button
      class="tb-btn icon"
      onclick={() => project.redo()}
      disabled={!project.canRedo()}
      title={project.canRedo()
        ? `Redo ${project.redoLabel() ?? ''} (Ctrl+Shift+Z)`
        : 'Nothing to redo'}
      aria-label="Redo"
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
        <polyline points="15 14 20 9 15 4"></polyline>
        <path d="M4 20v-7a4 4 0 0 1 4-4h12"></path>
      </svg>
    </button>
    <span class="tb-sep"></span>
    <GenerateBar />
    <span class="tb-flex"></span>
    {#if project.gen.generated && project.gen.generated.regions && project.gen.generated.regions.length > 0}
      <label
        class="region-toggle"
        title="Show / hide the translucent fill that marks each pocket operation's machined region."
      >
        <input
          type="checkbox"
          checked={project.data.regionsVisible}
          onchange={(e) =>
            (project.data.regionsVisible = (e.currentTarget as HTMLInputElement).checked)}
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
  <ModeSwitchNotice />

  <!-- ============== SPLIT VIEW ================================ -->
  <main
    class="split"
    class:tab-hidden={mainTab !== 'project'}
    style:--sidebar-width="{sidebarWidth}px"
  >
    <section class="viewport">
      <div class="canvas-area">
        <div class:pane-hidden={activePane !== '2d'} class="pane">
          <EntityCanvas2D
            onShowHelp={() => (mainTab = 'help')}
            onActivateSidebarPane={revealSidebarPane}
          />
        </div>
        {#if Scene3D}
          {@const C = Scene3D}
          <div class:pane-hidden={activePane !== '3d'} class="pane">
            <C onActivateSidebarPane={revealSidebarPane} />
          </div>
        {:else if activePane === '3d'}
          <p class="loading-3d">Loading 3D…</p>
        {/if}
        <LoadingOverlay visible={project.loading} message={project.loadingMessage} />
      </div>
      {#if project.gen.generated}
        <PlaybackBar />
        <div class="gcode-toggle">
          <button
            class:active={gcodeOpen}
            onclick={() => (gcodeOpen = !gcodeOpen)}
            title="Show / hide the G-code text panel. Click a line to scrub the playhead; the playhead's current line scrolls into view."
          >
            {gcodeOpen ? '▼' : '▶'}
            G-code
            <span class="hint">{project.gen.generated.gcode.split('\n').length} lines</span>
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
          <span
            class="stock-dims"
            title="Current stock dimensions (Length × Width × Thickness) in mm"
          >
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
          onActivate={() => activateSidebarPane('layers')}
          onOpenFileClick={() => openFile()}
          onAddTextClick={() => (addTextOpen = true)}
          reopenPrompt={sessionUi.reopenPrompt}
          onReopenAccept={acceptReopen}
          onReopenDismiss={dismissReopen}
        />
      </div>
      <div class="text-list-host" class:active={activeSidebarPane === 'text'}>
        <TextList
          active={activeSidebarPane === 'text'}
          onActivate={() => activateSidebarPane('text')}
          onAddText={() => (addTextOpen = true)}
        />
      </div>
      <div class="ops-host" class:active={activeSidebarPane === 'operations'}>
        <OperationsList
          active={activeSidebarPane === 'operations'}
          onActivate={() => activateSidebarPane('operations')}
        />
      </div>
    </aside>
  </main>

  <!-- ============== MACHINE / TOOLS TAB PANELS ================ -->
  {#if MachineWorkspace}
    {@const MachinePanel = MachineWorkspace}
    <main class="tab-panel" class:tab-hidden={mainTab !== 'machine'}>
      <MachinePanel />
    </main>
  {:else if mainTab === 'machine'}
    <main class="tab-panel"><p class="tab-loading">Loading machine panel…</p></main>
  {/if}
  {#if ToolLibraryDialog}
    {@const ToolsPanel = ToolLibraryDialog}
    <main class="tab-panel" class:tab-hidden={mainTab !== 'tools'}>
      <ToolsPanel embedded source="inventory" open={false} onClose={() => (mainTab = 'project')} />
    </main>
  {:else if mainTab === 'tools'}
    <main class="tab-panel"><p class="tab-loading">Loading tool library…</p></main>
  {/if}
  {#if SettingsDialog}
    {@const SettingsPanel = SettingsDialog}
    <main class="tab-panel" class:tab-hidden={mainTab !== 'settings'}>
      <SettingsPanel embedded open={false} onClose={() => (mainTab = 'project')} />
    </main>
  {:else if mainTab === 'settings'}
    <main class="tab-panel"><p class="tab-loading">Loading settings…</p></main>
  {/if}
  {#if HelpAbout}
    {@const HelpPanel = HelpAbout}
    <main class="tab-panel" class:tab-hidden={mainTab !== 'help'}>
      <HelpPanel />
    </main>
  {:else if mainTab === 'help'}
    <main class="tab-panel"><p class="tab-loading">Loading help…</p></main>
  {/if}

  {#if AddTextDialog}
    {@const C = AddTextDialog}
    <C open={addTextOpen} onClose={() => (addTextOpen = false)} />
  {/if}
  {#if reportOpen && ReportDialog}
    {@const C = ReportDialog}
    <C open={reportOpen} onClose={() => (reportOpen = false)} />
  {/if}
  <ConfirmPrompt />

  <footer class:footer-pick={modalStatusHint != null} title={modalStatusHint ?? statusBarText}>
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
  {#if sessionUi.dragOver}
    <div class="drop-overlay" aria-hidden="true">
      <div class="drop-card">
        <div class="drop-glyph">⤓</div>
        <div class="drop-title">Drop to open</div>
        <div class="drop-sub">DXF / SVG drawings · .ivac-project files</div>
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
  /* .menubar lives in MenuBar.svelte — :global so App's flex rule
     still reaches it across the component scope boundary. */
  .app > :global(.menubar),
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

  /* ---------- toolbar ------------------------------------------ */
  /* Top-level main-window tabs (Project | Machine | Tool library). */
  .main-tabs {
    display: flex;
    gap: 0.15rem;
    padding: 0.2rem 0.7rem 0;
    background: var(--bg-elevated);
    border-bottom: 1px solid var(--border);
  }
  .main-tabs-flex {
    flex: 1;
  }
  .main-tab {
    background: none;
    border: 1px solid transparent;
    border-bottom: none;
    border-radius: 4px 4px 0 0;
    padding: 0.3rem 0.85rem;
    font-size: 0.82rem;
    color: var(--text-muted);
    cursor: pointer;
  }
  .main-tab:hover {
    color: var(--text);
  }
  .main-tab.active {
    background: var(--bg-panel);
    border-color: var(--border);
    color: var(--text-strong);
    /* Visually merge with the content below by sitting on the strip's
       bottom border. */
    margin-bottom: -1px;
  }
  /* Machine / Tool-library tab panels — fill the main area like
     .split does; the embedded dialog shells scroll internally. */
  .tab-panel {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    background: var(--bg-panel);
  }
  .tab-loading {
    margin: 2rem auto;
    color: var(--text-muted);
  }
  /* Keep inactive top-level areas mounted (canvas + draft state
     survive tab switches) but fully hidden. */
  .tab-hidden {
    display: none !important;
  }
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
  /* Single-glyph config-dialog buttons (M / T / ⚙). Slightly bigger
     glyph + square button so they read as icons rather than truncated
     text. */
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
    /* Active canvas-pick mode — accent-tinted status bar grabs the eye
       so the user knows the canvas isn't in its normal selection mode. */
    background: color-mix(in srgb, var(--accent) 18%, var(--bg-panel));
    color: var(--text);
    font-weight: 600;
  }
  /* Drop overlay while user is dragging a file over the window. */
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
