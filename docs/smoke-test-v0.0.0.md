# Shop smoke-test checklist — v0.0.0 AppImage (1lb0)

Run on the shop machine against the freshly-built AppImage. Focus is the
user-facing surface that changed in the 2026-06 refactor + fix wave; the
CAM math itself is covered by the automated suites. Check the About
dialog first — it should show `v0.0.0` (or `v0.0.0-N-g<hash>`).

## Startup & shell
- [ ] App starts; menu bar renders (File/Edit/View/Tools/Help) and all
      dropdowns open/close by click, hover-slide, and Escape.
- [ ] Ctrl+Z / Ctrl+Y on an empty project shake the Edit-menu items
      instead of doing nothing silently.
- [ ] Reopen prompt appears when a previous project path is in the
      workspace; both Accept and Dismiss behave.
- [ ] Open-recent list works; clearing it works.

## File lifecycle (rewritten session/dialog plumbing)
- [ ] Drag-drop a DXF onto the window: loads; dropping a second one asks
      before discarding unsaved work.
- [ ] Import a drawing, make any edit, then File → Open: the
      Save / Don't save / Cancel prompt appears and all three buttons do
      what they say.
- [ ] Save project, reopen it: ops/tools/stock/text layers intact.
      (Old saves from before ~2026-06-09 may reset tab-mode counts and
      drill/pattern fields — expected, no-migration policy.)
- [ ] Quit with unsaved work: confirm prompt; quit clean: no prompt.

## Dialogs (new shared draft/discard mechanism)
For EACH of Tool library, Machine settings, Post-processor editor,
Add text:
- [ ] Edit a field, press Escape/X → inline "discard changes?" bar
      appears; "keep editing" preserves the edit; second close discards.
- [ ] Edit + Save → closes (or stays per dialog) without prompting; a
      reopened dialog shows the saved value.
- [ ] Open and immediately close with NO edits → no discard prompt
      (Add text especially — it used to false-flag as dirty).

## 2D canvas (extracted render + input pipeline)
- [ ] Imported geometry draws; zoom (wheel), pan (middle-drag),
      fit (F / ⌖ button) all work; theme switch redraws correctly.
- [ ] Click-select, ctrl-click multi-select, shift-click series-select,
      box-select; right-click menu adds an op from selection.
- [ ] Right-click on empty canvas shows the select-hint at most once.
- [ ] Profile op with Manual/Mixed tabs: ghost tab follows the contour,
      click places/removes, right-click on a tab opens the size popover.
- [ ] Approach-point pick mode: crosshair, OSnap glyphs, click commits,
      ESC exits, dragging a placed marker works.
- [ ] Fixtures and pocket regions render; raster placement image (if
      used) drags.

## Generate & export
- [ ] Generate on a profile + pocket project: progress card, toolpath in
      3D, no rebuild while scrubbing the playhead.
- [ ] Warnings panel (new floating window): opens, drags by header,
      resizes from the corner, stays clamped on-screen, close button is
      the plain × (not an accent-filled button).
- [ ] Export .ngc; spot-check the file ends with a single M5 before M30
      (mill) and that a straight lead-in cuts to the contour start (look
      for the extra G1 onto the first point after the plunge).

## Laser / plasma (if a profile is configured)
- [ ] Laser machine mode + lead-in: program arms M3 S0 before the rapid,
      ramps at the lead point, single M5 at the end.
- [ ] Plasma mode: pierce-height rapid + dwell at the lead point, cut at
      cut-height, torch off after the lead-out.

## Result
- [ ] No console errors in the webview devtools during the above.
Record findings as bd issues; close 1lb0 when this list is green.
