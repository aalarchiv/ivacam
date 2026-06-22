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
  import BottomSheet from './lib/components/BottomSheet.svelte';
  import PullToRefresh from './lib/components/PullToRefresh.svelte';
  import GcodeSubtitles from './lib/components/GcodeSubtitles.svelte';
  import { bottomPanels } from './lib/state/bottom-panels.svelte';
  import { generateBus } from './lib/state/generate-bus.svelte';
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
  // Help and About are now SEPARATE screens (About wants prominent space
  // for the logo). ShortcutHelp is tiny → static import; AboutDialog pulls
  // in the about markdown → keep it lazy.
  type AboutDialogComp = typeof import('./lib/components/AboutDialog.svelte').default;
  let AboutPanel = $state<AboutDialogComp | null>(null);
  let aboutLoading = false;
  type ReportDialogComp = typeof import('./lib/components/ReportDialog.svelte').default;
  let ReportDialog = $state<ReportDialogComp | null>(null);
  let reportDialogLoading = false;
  import RecentMenu from './lib/components/RecentMenu.svelte';
  import AppBarOverflowMenu from './lib/components/AppBarOverflowMenu.svelte';
  import LoadingOverlay from './lib/components/LoadingOverlay.svelte';
  import Splitter from './lib/components/Splitter.svelte';
  import EdgeSwipeNav from './lib/components/EdgeSwipeNav.svelte';

  /// Top-level main-window tab. Machine and Tool library are
  /// first-class tabs (not modals); their panels stay mounted once
  /// loaded so in-progress drafts survive tab switches.
  let mainTab = $state<'project' | 'machine' | 'tools' | 'settings' | 'help' | 'about'>('project');
  let addTextOpen = $state(false);
  let reportOpen = $state(false);
  /// Build-time version stamp baked by vite.config.ts. Surfaces in
  /// the window title and the Help → About dialog so users can
  /// paste an exact build identifier into bug reports.
  const buildVersion =
    typeof __IVAC_BUILD_VERSION__ === 'string' ? __IVAC_BUILD_VERSION__ : 'unknown';

  // Phone Save dropdown (punch-list 10): project / G-code / carved STL.
  let saveMenuOpen = $state(false);
  const gcodeDialect = $derived<'linuxcnc' | 'grbl' | 'hpgl'>(
    project.data.machine.gcodeDialect ?? 'linuxcnc',
  );
  function closeSaveMenu() {
    saveMenuOpen = false;
  }

  // Phone screen-name dropdown: tap the centre title to jump to any screen
  // (in addition to the ◂ ▸ chevrons and the top-panel swipe).
  let screenMenuOpen = $state(false);
  function closeScreenMenu() {
    screenMenuOpen = false;
  }

  // Accordion state for the phone 2D "S+L" bottom sheet — which of
  // Stock / Layers / Text is expanded (null = all collapsed to headers).
  // Independent of the sidebar's activeSidebarPane so the sheet and the
  // (legacy) sidebar overlay don't fight over one slot. Defaults to Layers.
  let slPane = $state<'stock' | 'layers' | 'text' | null>('layers');
  function toggleSl(p: 'stock' | 'layers' | 'text') {
    slPane = slPane === p ? null : p;
  }

  /// Quit the app (phone has no window chrome to close it). Prompts to
  /// save when there are unsaved changes, then exits the process.
  async function exitApp() {
    if (!(await confirmDiscardIfDirty('exit ivaCAM'))) return;
    try {
      const { exit } = await import('@tauri-apps/plugin-process');
      await exit(0);
    } catch (e) {
      console.warn('exit failed:', e);
    }
  }

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
  import { isTauri } from './lib/api/env';
  const isTauriEnv = isTauri();
  import { project } from './lib/state/project.svelte';
  import { i18n, resolveLocale } from './lib/i18n';
  import { workspace } from './lib/state/workspace.svelte';
  import ConfirmPrompt from './lib/components/ConfirmPrompt.svelte';
  import {
    addDrawing,
    openAny,
    saveProject,
    exportGeneratedGcode,
    exportSimulatedStockStl,
    confirmDiscardIfDirty,
  } from './lib/services/file_ops';
  import PhoneWarnings from './lib/components/PhoneWarnings.svelte';
  import ShortcutHelp from './lib/components/ShortcutHelp.svelte';
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
    confirmExitApp,
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
  import { swipeHorizontal } from './lib/actions/swipe-horizontal';
  import { layout } from './lib/state/layout.svelte';
  import {
    ACTIVITY_ORDER,
    activityFor,
    tabPaneForActivity,
    nextActivity,
    prevActivity,
    activityLabel,
    type Activity,
  } from './lib/state/activities';

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
    // On phone/tablet the Operations pane is the bottom sheet, not the
    // sidebar overlay (7jug.16) — a canvas tap that jumps to its op opens
    // the sheet instead of raising the overlay. Other panes still use the
    // overlay.
    if (layout.isNarrow && target === 'operations') {
      opsSheetOpenSignal++;
      return;
    }
    const next = revealPane({ active: activeSidebarPane, prev: prevSidebarPane }, target);
    activeSidebarPane = next.active;
    prevSidebarPane = next.prev;
    // On narrow layouts the sidebar is a full-screen overlay over the
    // canvas, so a programmatic "show me this pane" (e.g. a canvas tap
    // that jumps to its operation) must also raise the overlay.
    if (layout.isNarrow) mobilePanelOpen = true;
  }
  // Bumped to ask the Operations bottom sheet to open (7jug.16).
  let opsSheetOpenSignal = $state(0);
  // Narrow-layout (<1024px) only: the sidebar collapses out of the
  // 3-column grid and shows as a full-screen overlay over the canvas.
  // `mobilePanelOpen` toggles that overlay; on desktop it's inert.
  let mobilePanelOpen = $state(false);
  const stockDimsLabel = $derived.by<string>(() => {
    const cfg = project.data.stock;
    const fp = computeFootprint(project.stockSizingImport, cfg, project.data.machine.workArea);
    const x = Math.max(0, fp.maxX - fp.minX);
    const y = Math.max(0, fp.maxY - fp.minY);
    const z = Math.max(0, cfg.thickness);
    const f = (n: number) => (Number.isFinite(n) ? n.toFixed(0) : '0');
    return `${f(x)} × ${f(y)} × ${f(z)} mm`;
  });

  onMount(() => {
    document.documentElement.dataset.theme = project.data.settings.theme;
    i18n.setPreference(project.data.settings.language);
    document.documentElement.lang = resolveLocale(project.data.settings.language);

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

    // Android system-back (ivac-h0ai). MainActivity intercepts the system
    // back gesture and dispatches this DOM event instead of finishing the
    // activity — on web/desktop it simply never fires. From any non-first
    // screen, back returns to the first activity; on the first screen it
    // confirms before quitting the process.
    const onAndroidBack = () => {
      const first = ACTIVITY_ORDER[0];
      if (currentActivity !== first) {
        goToActivity(first);
        return;
      }
      void confirmExitApp();
    };
    window.addEventListener('android-back', onAndroidBack);

    return () => {
      window.removeEventListener('android-back', onAndroidBack);
      unwireSession();
    };
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

  // Apply the language preference live. Reads only settings.language (not
  // i18n.locale) so the idempotent setPreference write doesn't re-trigger
  // this effect. `auto` resolves against the system/browser locale.
  $effect(() => {
    const pref = project.data.settings.language;
    i18n.setPreference(pref);
    document.documentElement.lang = resolveLocale(pref);
  });

  let activePane = $state<'2d' | '3d'>('2d');

  // Phone navigation: the desktop main-tabs + 2D/3D pane toggle collapse
  // into one swipeable activity list (activities.ts). `currentActivity`
  // is a read-only view over the authoritative mainTab/activePane state;
  // `goToActivity` writes back through them.
  const currentActivity = $derived<Activity>(activityFor(mainTab, activePane));
  function goToActivity(a: Activity) {
    const { mainTab: mt, pane } = tabPaneForActivity(a);
    mainTab = mt;
    if (pane) activePane = pane;
  }

  // Phone bottom panels (Operations — 7jug.9 — and G-code — 7jug.11): the
  // hot-path surfaces. Shown on the Project 2D/3D activities only, and
  // never while the (other-panes) sidebar overlay is open. Operations
  // replaces routing through that overlay; Stock/Layers/Text still use it.
  const showBottomPanels = $derived(
    layout.isNarrow &&
      !mobilePanelOpen &&
      (currentActivity === 'project-2d' || currentActivity === 'project-3d'),
  );
  // Persisted per-panel open snaps (workspace), read reactively.
  const opsSnap = $derived.by(() => {
    void workspace.version;
    return workspace.get().panels.ops_fold_snap;
  });
  const gcodeSnap = $derived.by(() => {
    void workspace.version;
    return workspace.get().panels.gcode_fold_snap;
  });
  /// When the Operations sheet opens from folded, surface the MRU op — the
  /// current selection, else the most-recently-added (last) op — so the
  /// user lands on something editable.
  function surfaceMruOp() {
    if (project.sel.selectedOpId != null) return;
    const last = project.data.operations.at(-1);
    if (last) project.sel.selectedOpId = last.id;
  }

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
    if (mainTab === 'about' && !AboutPanel && !aboutLoading) {
      aboutLoading = true;
      void import('./lib/components/AboutDialog.svelte').then((m) => {
        AboutPanel = m.default;
        aboutLoading = false;
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
    // Load on the desktop toggle, or when the phone G-code bottom sheet is
    // unfolded (7jug.11). The left 'gcode' slot hosts Stock+Layers in 2D, so
    // only the 3D case is a real G-code open — don't pull the assets in 2D.
    const gcodeSheetOpen = bottomPanels.active === 'gcode' && activePane === '3d';
    if ((gcodeOpen || gcodeSheetOpen) && !GcodePanel && !gcodePanelLoading) {
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
  /// Tauri only: suppress the webview's built-in right-click menu
  /// (Back/Reload/Inspect) — a workshop user right-clicking the canvas
  /// or a panel should see OUR menus or nothing, not browser
  /// internals. Editable fields keep the native menu (copy/paste);
  /// canvas components call preventDefault first and open their own
  /// menus, unaffected. Devtools stay reachable for diagnostics via
  /// keyboard only.
  function onContextMenu(e: MouseEvent) {
    if (!isTauriEnv) return;
    const t = e.target as HTMLElement | null;
    if (t && t.closest('input, textarea, [contenteditable="true"]') != null) return;
    e.preventDefault();
  }

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
        void openAny();
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
  oncontextmenu={onContextMenu}
  onresize={onWindowResize}
/>

<div class="app" class:narrow={layout.isNarrow}>
  <!-- ===== MOBILE TOP APP BAR (narrow only) ================== -->
  <!-- Replaces the desktop main-tabs + toolbar on narrow screens: a
       single row with the current activity (◂ label ▸ to move between
       the swipeable activities) on the left and the core actions on the
       right. The ☰ Panels button is an interim hook to the (superseded)
       sidebar overlay so Operations/Layers/Stock stay reachable until
       the on-canvas affordances (.15) + Operations bottom panel (.9)
       land. -->
  {#if layout.isNarrow}
    <header class="mobile-appbar">
      <!-- Fixed 3-slot nav: ◂ left, name centre, ▸ right — the chevrons
           never move regardless of screen-name length (punch-list 6). -->
      <!-- Swipe/flick the top screen panel to move between screens. This is
           the reliable phone path: the on-canvas EdgeSwipeNav zones sit on
           the screen edges, which Android's system back gesture owns, so
           those swipes never reach the WebView. The chevron buttons still
           handle taps (a tap produces no horizontal travel). -->
      <div
        class="activity-nav"
        aria-label="Screen"
        use:swipeHorizontal={{
          onLeft: () => goToActivity(nextActivity(currentActivity)),
          onRight: () => goToActivity(prevActivity(currentActivity)),
        }}
      >
        <button
          type="button"
          class="ab-btn ab-chevron"
          onclick={() => goToActivity(prevActivity(currentActivity))}
          disabled={currentActivity === ACTIVITY_ORDER[0]}
          aria-label="Previous screen"
        >
          ◂
        </button>
        <div class="screen-menu">
          <button
            type="button"
            class="activity-title"
            aria-haspopup="menu"
            aria-expanded={screenMenuOpen}
            onclick={() => (screenMenuOpen = !screenMenuOpen)}
          >
            {activityLabel(currentActivity)} ▾
          </button>
          {#if screenMenuOpen}
            <button
              type="button"
              class="screen-backdrop"
              aria-label="Close screen menu"
              onclick={closeScreenMenu}
            ></button>
            <div class="screen-pop" role="menu" aria-label="Go to screen">
              {#each ACTIVITY_ORDER as a (a)}
                <button
                  type="button"
                  class="screen-item"
                  role="menuitem"
                  aria-current={a === currentActivity}
                  class:active={a === currentActivity}
                  onclick={() => {
                    closeScreenMenu();
                    goToActivity(a);
                  }}
                >
                  {activityLabel(a)}
                </button>
              {/each}
            </div>
          {/if}
        </div>
        <button
          type="button"
          class="ab-btn ab-chevron"
          onclick={() => goToActivity(nextActivity(currentActivity))}
          disabled={currentActivity === ACTIVITY_ORDER[ACTIVITY_ORDER.length - 1]}
          aria-label="Next screen"
        >
          ▸
        </button>
      </div>

      <!-- Centre: generate/warnings status chip. Tap opens a panel with a
           Generate/Re-Generate button + the warnings list (the desktop
           GenerateBar is hidden on phone). Shown whenever there's geometry
           to generate, including the idle "Generate" state. -->
      <div class="appbar-center">
        {#if project.geometryView != null}
          <PhoneWarnings />
        {/if}
      </div>

      <!-- Right: primary actions. ☰ panels button retired — Stock/Layers/
           Text are on-canvas chips and Operations is the bottom sheet. -->
      <div class="appbar-actions">
        <!-- Inline primary actions — hidden on really narrow phones, where
             they collapse into the ☰ overflow menu (see .wide-actions CSS
             + AppBarOverflowMenu compact items). -->
        <div class="wide-actions">
          <button type="button" class="ab-btn" onclick={() => openAny()} disabled={project.loading}>
            Open
          </button>
          <div class="save-menu">
            <button
              type="button"
              class="ab-btn"
              aria-haspopup="menu"
              aria-expanded={saveMenuOpen}
              onclick={() => (saveMenuOpen = !saveMenuOpen)}
              disabled={!project.transformedImport}
            >
              Save ▾
            </button>
            {#if saveMenuOpen}
              <button
                type="button"
                class="save-backdrop"
                aria-label="Close save menu"
                onclick={closeSaveMenu}
              ></button>
              <div class="save-pop" role="menu" aria-label="Save options">
                <button
                  type="button"
                  class="save-item"
                  role="menuitem"
                  onclick={() => {
                    closeSaveMenu();
                    void saveProject();
                  }}
                >
                  Save project
                </button>
                <button
                  type="button"
                  class="save-item"
                  role="menuitem"
                  disabled={!project.gen.generated}
                  onclick={() => {
                    closeSaveMenu();
                    void exportGeneratedGcode(gcodeDialect);
                  }}
                >
                  Save G-code (.{gcodeDialect === 'hpgl' ? 'plt' : 'ngc'})
                </button>
                <button
                  type="button"
                  class="save-item"
                  role="menuitem"
                  disabled={!project.gen.generated}
                  onclick={() => {
                    closeSaveMenu();
                    void exportSimulatedStockStl();
                  }}
                >
                  Save carved STL
                </button>
              </div>
            {/if}
          </div>
          <button
            type="button"
            class="ab-btn"
            onclick={() => (reportOpen = true)}
            aria-label="Project report"
          >
            Report
          </button>
        </div>
        <AppBarOverflowMenu
          onOpenRecent={(path) => void openRecentProject(path)}
          onOpen={() => openAny()}
          onSaveProject={() => void saveProject()}
          onSaveGcode={() => void exportGeneratedGcode(gcodeDialect)}
          onSaveStl={() => void exportSimulatedStockStl()}
          onReport={() => (reportOpen = true)}
          canSave={!!project.transformedImport}
          hasProgram={!!project.gen.generated}
          loading={project.loading}
          gcodeExt={gcodeDialect === 'hpgl' ? '.plt' : '.ngc'}
          onExit={() => void exitApp()}
        />
      </div>
    </header>
  {/if}

  <!-- Edge-swipe between activities (narrow only; inert while the sidebar
       overlay is open so a panel swipe doesn't also flip activities). -->
  {#if layout.isNarrow && !mobilePanelOpen}
    <EdgeSwipeNav
      onPrev={() => goToActivity(prevActivity(currentActivity))}
      onNext={() => goToActivity(nextActivity(currentActivity))}
    />
  {/if}

  <!-- Phone bottom panels — G-code (left) + Operations (right) share one
       folded strip (.9/.11). Fixed to the bottom edge over the canvas; the
       split reserves padding for their always-visible handles (see
       `.split.with-ops-sheet`). G-code only appears once a program exists. -->
  {#if showBottomPanels}
    <!-- Left bottom slot is view-dependent: Stock + Layers in 2D (replaces
         the floating canvas chips), G-code in 3D where it's meaningful. Both
         share the 'gcode' slot key (left panel) + its persisted fold snap. -->
    {#if activePane === '2d'}
      <BottomSheet
        key="gcode"
        label="Stock & Layers"
        code="S+L"
        side="left"
        savedSnap={gcodeSnap}
        onPersistSnap={(s) => workspace.setPanels({ gcode_fold_snap: s })}
      >
        <div class="sl-sheet" data-active={slPane ?? 'none'}>
          <div class="stock-host" class:active={slPane === 'stock'}>
            <button
              type="button"
              class="group-head"
              onclick={() => toggleSl('stock')}
              aria-expanded={slPane === 'stock'}
              title={slPane === 'stock' ? 'Collapse stock' : 'Expand stock settings'}
            >
              <span class="caret">{slPane === 'stock' ? '▾' : '▸'}</span>
              <span class="stock-name">Stock</span>
              <span class="stock-dims" title="Stock dimensions (Length × Width × Thickness) in mm">
                {stockDimsLabel}
              </span>
            </button>
            {#if slPane === 'stock'}
              <div class="group-body"><StockPanel /></div>
            {/if}
          </div>
          <div class="layers-host" class:active={slPane === 'layers'}>
            <LayerList
              active={slPane === 'layers'}
              onActivate={() => toggleSl('layers')}
              onAddDrawingClick={() => addDrawing()}
              onAddTextClick={() => (addTextOpen = true)}
              reopenPrompt={sessionUi.reopenPrompt}
              onReopenAccept={acceptReopen}
              onReopenDismiss={dismissReopen}
            />
          </div>
          <div class="text-list-host" class:active={slPane === 'text'}>
            <TextList
              active={slPane === 'text'}
              onActivate={() => toggleSl('text')}
              onAddText={() => (addTextOpen = true)}
            />
          </div>
        </div>
      </BottomSheet>
    {:else if project.gen.generated}
      <BottomSheet
        key="gcode"
        label="G-code"
        code="NGC"
        side="left"
        count={project.gen.generated.gcode.split('\n').length}
        savedSnap={gcodeSnap}
        onPersistSnap={(s) => workspace.setPanels({ gcode_fold_snap: s })}
      >
        {#if GcodePanel}
          {@const C = GcodePanel}
          <C />
        {:else}
          <p class="loading-3d">Loading G-code…</p>
        {/if}
      </BottomSheet>
    {/if}
    <BottomSheet
      key="ops"
      label="Operations"
      code="OPS"
      side="right"
      count={project.data.operations.length}
      savedSnap={opsSnap}
      onPersistSnap={(s) => workspace.setPanels({ ops_fold_snap: s })}
      onOpen={surfaceMruOp}
      openSignal={opsSheetOpenSignal}
    >
      <OperationsList active={true} onActivate={() => {}} />
    </BottomSheet>
  {/if}

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
      title="Keyboard &amp; mouse shortcuts and usage help">Help</button
    >
    <button
      type="button"
      class="main-tab"
      class:active={mainTab === 'about'}
      onclick={() => (mainTab = 'about')}
      title="Version, license, acknowledgements">About</button
    >
  </nav>

  <!-- ============== TOOLBAR (single row) ====================== -->
  <div class="toolbar" class:tab-hidden={mainTab !== 'project'}>
    <button
      class="tb-btn primary"
      onclick={() => openAny()}
      disabled={project.loading}
      title="Open a drawing (DXF / SVG) or a saved .ivac-project.json (Ctrl+O)"
    >
      Open
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
      title="Project report — printable job summary (ops, tools, times) to hand to the shop floor"
      aria-label="Project report"
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
        <path d="M11 4H4a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7"></path>
        <path d="M18.5 2.5a2.121 2.121 0 0 1 3 3L12 15l-4 1 1-4 9.5-9.5z"></path>
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
    class:narrow={layout.isNarrow}
    class:with-ops-sheet={showBottomPanels}
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
            <!-- Phone experiment (7jug.17): G-code "subtitles" synced to
                 the playhead, captioned over the running 3D sim. -->
            {#if layout.isNarrow}
              <GcodeSubtitles />
            {/if}
          </div>
        {:else if activePane === '3d'}
          <p class="loading-3d">Loading 3D…</p>
        {/if}
        <LoadingOverlay visible={project.loading} message={project.loadingMessage} />
        <!-- Phone: pull down from the top of the canvas to (re-)generate —
             the Generate button is desktop-only on narrow (7jug.12/.2). -->
        {#if showBottomPanels}
          <PullToRefresh onRefresh={() => generateBus.request()} />
        {/if}
      </div>
      {#if project.gen.generated && activePane === '3d'}
        <!-- Preview navigation belongs to the 3D toolpath sim; in 2D it has
             nothing to scrub, so it's hidden there. -->
        <PlaybackBar />
      {/if}
      {#if project.gen.generated}
        <!-- Desktop inline G-code (toggle + resizable row). On phone this
             is the G-code bottom sheet instead (7jug.11), so hide it. -->
        {#if !layout.isNarrow}
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
      {/if}
    </section>
    {#if !layout.isNarrow}
      <Splitter
        direction="horizontal"
        onResize={onSidebarResize}
        onReset={resetSidebar}
        title="Drag to resize the side panel · double-click to reset"
      />
    {/if}
    {#if layout.isNarrow && mobilePanelOpen}
      <button
        type="button"
        class="mobile-panel-close"
        onclick={() => (mobilePanelOpen = false)}
        aria-label="Back to drawing"
        title="Back to drawing"
      >
        ✕
      </button>
    {/if}
    <aside
      class="sidebar"
      class:narrow={layout.isNarrow}
      class:mobile-open={mobilePanelOpen}
      data-active={activeSidebarPane}
    >
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
          onAddDrawingClick={() => addDrawing()}
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
      <!-- On narrow layouts Operations lives in the bottom sheet
           (BottomSheet, 7jug.9/.16), not this overlay accordion —
           Stock/Layers/Text stay here. -->
      {#if !layout.isNarrow}
        <div class="ops-host" class:active={activeSidebarPane === 'operations'}>
          <OperationsList
            active={activeSidebarPane === 'operations'}
            onActivate={() => activateSidebarPane('operations')}
          />
        </div>
      {/if}
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
  <main class="tab-panel" class:tab-hidden={mainTab !== 'help'}>
    <ShortcutHelp embedded onClose={() => {}} />
  </main>
  {#if AboutPanel}
    {@const AboutComp = AboutPanel}
    <main class="tab-panel" class:tab-hidden={mainTab !== 'about'}>
      <AboutComp embedded onClose={() => {}} />
    </main>
  {:else if mainTab === 'about'}
    <main class="tab-panel"><p class="tab-loading">Loading about…</p></main>
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

  <footer
    class:footer-pick={modalStatusHint != null}
    class:above-sheets={layout.isNarrow && showBottomPanels}
    title={modalStatusHint ?? statusBarText}
  >
    {#if modalStatusHint}
      {modalStatusHint}
    {:else}
      <span class="status-info">{statusInfoText}</span>
      <!-- Phone: the keyboard/mouse shortcut hints are irrelevant on touch
           and overflowed the narrow footer (already crowded by the bottom
           panels). Show only the essential info there. -->
      {#if statusShortcutHints && !layout.isNarrow}
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

  /* ---------- mobile top app bar (narrow only) ----------------- */
  /* The activity-swipe app bar replaces the desktop tabs + toolbar on
     narrow screens, so hide those there. */
  .app.narrow > .main-tabs,
  .app.narrow > .toolbar {
    display: none;
  }
  .mobile-appbar {
    flex: 0 0 auto;
    display: flex;
    align-items: center;
    gap: 0.4rem;
    /* Pad the top by the device status-bar inset (notch / Android status
       bar) so the bar isn't hidden behind it. Needs viewport-fit=cover in
       the viewport meta to expose env(). */
    padding: calc(0.3rem + env(safe-area-inset-top, 0px)) 0.5rem 0.3rem;
    background: var(--bg-panel);
    border-bottom: 1px solid var(--border);
  }
  /* Fixed-width 3-slot nav: ◂ | name | ▸. The name column flexes and
     truncates so the right chevron stays put regardless of label length. */
  .mobile-appbar .activity-nav {
    flex: 0 0 auto;
    display: grid;
    grid-template-columns: 2.5rem 1fr 2.5rem;
    align-items: center;
    width: 9.5rem;
  }
  /* Middle grid cell: relative anchor for the screen dropdown. */
  .mobile-appbar .screen-menu {
    position: relative;
    min-width: 0;
  }
  .mobile-appbar .activity-title {
    width: 100%;
    text-align: center;
    font-weight: 600;
    font-size: 0.9rem;
    color: var(--text-strong);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    background: none;
    border: none;
    padding: 0.2rem 0;
    cursor: pointer;
  }
  .mobile-appbar .screen-backdrop {
    position: fixed;
    inset: 0;
    z-index: var(--z-dropdown);
    background: none;
    border: none;
    cursor: default;
  }
  .mobile-appbar .screen-pop {
    position: absolute;
    top: calc(100% + 0.3rem);
    left: 0;
    z-index: calc(var(--z-dropdown) + 1);
    min-width: 9rem;
    padding: 0.3rem;
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: 8px;
    box-shadow: 0 6px 22px rgb(0 0 0 / 35%);
  }
  .mobile-appbar .screen-item {
    display: flex;
    align-items: center;
    width: 100%;
    min-height: 44px;
    padding: 0 0.6rem;
    background: none;
    border: none;
    border-radius: 5px;
    color: var(--text);
    font-size: 0.88rem;
    text-align: left;
    cursor: pointer;
  }
  .mobile-appbar .screen-item:hover {
    background: color-mix(in srgb, var(--accent) 14%, var(--bg-elevated));
    color: var(--text-strong);
  }
  .mobile-appbar .screen-item.active {
    color: var(--text-strong);
    font-weight: 600;
  }
  /* Centre column holds the status chip, centred between nav and actions. */
  .mobile-appbar .appbar-center {
    flex: 1 1 auto;
    display: flex;
    justify-content: center;
    min-width: 0;
  }
  .mobile-appbar .appbar-actions {
    flex: 0 0 auto;
    display: flex;
    align-items: center;
    gap: 0.4rem;
  }
  /* Inline Open / Save / Report — hidden on really narrow phones, where
     they move into the ☰ overflow menu (matching breakpoint there). */
  .mobile-appbar .wide-actions {
    display: flex;
    align-items: center;
    gap: 0.4rem;
  }
  @media (max-width: 430px) {
    .mobile-appbar .wide-actions {
      display: none;
    }
  }
  /* Save dropdown. */
  .mobile-appbar .save-menu {
    position: relative;
    display: inline-flex;
  }
  .mobile-appbar .save-backdrop {
    position: fixed;
    inset: 0;
    z-index: var(--z-dropdown);
    background: none;
    border: none;
    cursor: default;
  }
  .mobile-appbar .save-pop {
    position: absolute;
    top: calc(100% + 0.3rem);
    right: 0;
    z-index: calc(var(--z-dropdown) + 1);
    min-width: 12rem;
    padding: 0.3rem;
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: 8px;
    box-shadow: 0 6px 22px rgb(0 0 0 / 35%);
  }
  .mobile-appbar .save-item {
    display: flex;
    align-items: center;
    width: 100%;
    min-height: 44px;
    padding: 0 0.6rem;
    background: none;
    border: none;
    border-radius: 5px;
    color: var(--text);
    font-size: 0.88rem;
    text-align: left;
    cursor: pointer;
  }
  .mobile-appbar .save-item:hover:not(:disabled) {
    background: color-mix(in srgb, var(--accent) 14%, var(--bg-elevated));
    color: var(--text-strong);
  }
  .mobile-appbar .save-item:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }
  .mobile-appbar .ab-btn {
    min-height: 40px;
    min-width: 40px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    padding: 0 0.6rem;
    background: var(--bg-elevated);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 5px;
    font-size: 0.85rem;
    cursor: pointer;
  }
  .mobile-appbar .ab-btn.ab-chevron {
    padding: 0;
    font-size: 1.1rem;
  }
  .mobile-appbar .ab-btn:hover:not(:disabled) {
    background: color-mix(in srgb, var(--accent) 14%, var(--bg-elevated));
    border-color: var(--accent);
    color: var(--text-strong);
  }
  .mobile-appbar .ab-btn:disabled {
    opacity: 0.4;
    cursor: not-allowed;
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
    /* Anchor for the narrow-layout sidebar overlay (.sidebar.narrow). */
    position: relative;
  }
  /* Narrow layout (<1024px): collapse the 3-column grid to a single
     canvas column. The splitters are not rendered (see markup) and the
     sidebar becomes a full-screen overlay toggled by `mobilePanelOpen`. */
  .split.narrow {
    grid-template-columns: minmax(0, 1fr);
  }
  /* Reserve the Operations-sheet handle strip (44px) at the bottom so the
     PlaybackBar / G-code toggle in the canvas column aren't hidden behind
     the fixed sheet (.9/.11). Matches HANDLE_PX in BottomSheet.svelte. */
  .split.with-ops-sheet {
    padding-bottom: 44px;
  }
  /* Phone: also clear the status footer, which is lifted to sit just above
     the 44px handle strip (see footer.above-sheets) instead of behind it. */
  .split.narrow.with-ops-sheet {
    padding-bottom: calc(44px + 1.6rem);
  }
  /* Stock + Layers + Text bottom-sheet body: a collapsible accordion (one
     pane open at a time, or all collapsed to headers). Mirrors the sidebar
     grid — the open pane takes the 1fr row and scrolls internally; the
     others are auto-height headers. Reuses the sidebar host/header styles
     (.stock-host / .group-head / …), which aren't .sidebar-scoped. */
  .sl-sheet {
    display: grid;
    height: 100%;
    min-height: 0;
    overflow: hidden;
    grid-template-rows: auto auto auto;
  }
  .sl-sheet[data-active='stock'] {
    grid-template-rows: minmax(0, 1fr) auto auto;
  }
  .sl-sheet[data-active='layers'] {
    grid-template-rows: auto minmax(0, 1fr) auto;
  }
  .sl-sheet[data-active='text'] {
    grid-template-rows: auto auto minmax(0, 1fr);
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
    /* Positioned ancestor for the phone G-code subtitles overlay (7jug.17)
       and other absolutely-positioned pane children. */
    position: relative;
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
  /* Narrow layout: lift the sidebar out of the (now single-column) grid
     and float it over the canvas as a full-screen overlay. The accordion
     inside is unchanged — only its host box changes. Hidden until the
     user opens it via the Panels button (`.mobile-open`). */
  .sidebar.narrow {
    position: absolute;
    inset: 0;
    z-index: var(--z-mobile-panel);
    background: var(--bg-app);
  }
  .sidebar.narrow:not(.mobile-open) {
    display: none;
  }
  /* Close ("back to drawing") affordance for the narrow sidebar overlay,
     pinned top-right above the panel so it never collides with the
     accordion's per-host grid rows. */
  .mobile-panel-close {
    position: absolute;
    top: 0.35rem;
    right: 0.4rem;
    z-index: calc(var(--z-mobile-panel) + 1);
    width: 2rem;
    height: 2rem;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg-elevated);
    color: var(--text-strong);
    font-size: 1rem;
    cursor: pointer;
  }
  .mobile-panel-close:hover {
    border-color: var(--accent);
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
  /* Phone with the bottom sheets present: the folded handle strip (44px,
     fixed) used to cover the in-flow footer, and the empty half of the
     strip let the status text show through. Lift the footer to sit just
     ABOVE the handle strip, full-width and opaque. An OPEN sheet
     (z-floating + 1) still covers it, which is fine — the panel has focus
     then. */
  .app.narrow > footer.above-sheets {
    position: fixed;
    left: 0;
    right: 0;
    bottom: 44px;
    z-index: var(--z-floating);
    border-bottom: 1px solid var(--border);
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
