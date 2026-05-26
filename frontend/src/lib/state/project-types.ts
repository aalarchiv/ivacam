// Pure-TypeScript type definitions for the project's data shape:
// fixture / stock / tool / machine config + the kind-tagged enums
// (pocket strategy, pattern config, drill cycle, …) the OpEntry union
// references. Lives outside `project.svelte.ts` so vitest specs and
// non-Svelte helpers can import the shapes without booting the Svelte
// rune runtime, and so `project.svelte.ts` stays focused on the
// reactive `ProjectState` class itself (audit 6cpl).
//
// Re-exported from `project.svelte.ts` for backwards compatibility —
// existing call sites that import from `state/project.svelte` continue
// to work.

import type { ImportResponse } from '../api/types';
import type { OpEntry, OpKind, ToolKind } from './op_types';

export function prettyOpKind(kind: OpKind): string {
  switch (kind) {
    case 'profile':
      return 'Profile';
    case 'pocket':
      return 'Pocket';
    case 'drill':
      return 'Drill';
    case 'thread':
      return 'Thread';
    case 'chamfer':
      return 'Chamfer';
    case 'engrave':
      return 'Engraving';
    case 'drag_knife':
      return 'Drag-knife';
    case 't_slot':
      return 'T-Slot';
    case 'dovetail':
      return 'Dovetail';
    case 'vcarve':
      return 'V-Carve';
    case 'pause':
      return 'Pause';
  }
}

/// Mirrors `wiac_core::project::FixtureKind`. The `shape` discriminator
/// is the wire-side serde tag; vertex coords for `polygon` are local
/// (origin-relative) so the fixture can be moved by editing `origin`.
export type FixtureKind =
  | { shape: 'box'; width: number; depth: number }
  | { shape: 'cylinder'; radius: number }
  | { shape: 'polygon'; vertices: [number, number][] };

export interface Fixture {
  id: number;
  name: string;
  kind: FixtureKind;
  origin: [number, number];
  z_bottom: number;
  z_top: number;
  color: number;
}

/// Default packed RGBA color: amber, ~75% alpha.
export const DEFAULT_FIXTURE_COLOR = 0xffa050c0;

export function defaultFixtureName(kind: FixtureKind, id: number): string {
  switch (kind.shape) {
    case 'box':
      return `Clamp ${id}`;
    case 'cylinder':
      return `Dog ${id}`;
    case 'polygon':
      return `Fixture ${id}`;
  }
}

export interface StockConfig {
  visible: boolean;
  mode: 'auto' | 'manual';
  margin: number;
  thickness: number;
  customX: number;
  customY: number;
  /// Origin offsets in mm. In auto mode the stock anchor is the
  /// imported bbox (or the work-area corner when no drawing is loaded);
  /// the offsets translate the stock relative to that anchor. In manual
  /// mode the anchor is (0, 0) so offsets are absolute.
  offsetX?: number;
  offsetY?: number;
  /// Z offset is for future use (currently the pipeline assumes stock
  /// top at z = 0). Plumbed through the UI now so the field exists
  /// before the sim refactor lands.
  offsetZ?: number;
}

export type CoolantMode = 'off' | 'mist' | 'flood';

/// z1y0: per-tool spindle direction. `cw` (M3) is the default for most
/// right-hand cutters; `ccw` (M4) is for left-hand / reverse-thread /
/// mirror-helix tooling. Mirror of `wiac_core::project::SpindleDirection`,
/// serde-`rename_all = "lowercase"`.
export type SpindleDirection = 'cw' | 'ccw';

/// 1wit: one cross-section sample of a form / profile cutter outline,
/// measured up from the cutting tip. Mirror of
/// `wiac_core::project::FormProfileSample`.
export interface FormProfileSample {
  /// Height above the cutting tip (mm). 0 is the bottom face.
  zMm: number;
  /// Cutter radius at this height (mm).
  rMm: number;
}

export interface ToolEntry {
  id: number;
  name: string;
  kind: ToolKind;
  diameter: number;
  tipDiameter?: number;
  /// V-bit full apex angle in degrees. Drives the V-Carve depth math
  /// (`z = -R / tan(tipAngleDeg / 2)`); ignored for non-V tools.
  /// Optional in TS for back-compat with old project files; the wire
  /// payload omits it when undefined and the Rust side defaults to 60°.
  tipAngleDeg?: number;
  dragoff?: number;
  flutes: number;
  speed: number;
  plungeRate: number;
  feedRate: number;
  coolant: CoolantMode;
  /// Per-pass overrides (rt1.27): when set, the finish ring of a
  /// Pocket op consumes these instead of the general values. Drill ops
  /// consume the _drill variants throughout. Undefined = inherit the
  /// general value.
  speedFinish?: number;
  plungeRateFinish?: number;
  feedRateFinish?: number;
  speedDrill?: number;
  plungeRateDrill?: number;
  feedRateDrill?: number;
  /// Default peck step (positive, mm) for Peck / ChipBreak drill
  /// cycles whose op leaves `peck_step_mm` at 0.
  defaultPeckStepMm?: number;
  /// Per-tool Z origin offset (rt1.30): for machines without auto
  /// tool-length probing, pre-measure each tool's tip Z relative to a
  /// reference tool and record the delta here. Positive = sticks out
  /// further; negative = shorter. mm.
  zShiftMm?: number;
  /// Laser pierce dwell (rt1.29): seconds the beam waits at the
  /// entry point with the laser on before the cut starts so it burns
  /// through stock. Honored only when kind === 'laser_beam'.
  laserPierceSec?: number;
  /// Laser lead-in distance (rt1.29): mm of approach travel along the
  /// entry tangent to reduce edge entry burn. Honored only when
  /// kind === 'laser_beam'. (Wire field reserved; emit logic ships in
  /// a follow-up.)
  laserLeadInMm?: number;
  /// Bull-nose corner radius (rt1.28): rounded transition at the
  /// floor edge. Honored only when kind === 'bull_nose'.
  cornerRadiusMm?: number;
  /// T-slot cutter neck diameter (rt1.28). Honored only when
  /// kind === 't_slot'.
  tslotNeckDiameterMm?: number;
  /// T-slot cutter neck length (rt1.28). Honored only when
  /// kind === 't_slot'.
  tslotNeckLengthMm?: number;
  /// 1wit: form / profile cutter cross-section, tip → top. Each sample
  /// is { zMm: height above the cutting tip, rMm: radius there }. The
  /// sim carves the interpolated radius per Z slice when ≥2 samples are
  /// present; otherwise it falls back to a tip→diameter taper. Honored
  /// only when kind === 'form_profile'. Generated from a dovetail
  /// preset or hand-entered for cove / ogee / custom bits.
  formProfileMm?: FormProfileSample[];
  /// Spindle warmup pause (seconds). After each spindle_cw / spindle_ccw
  /// the post inserts a G4 P<pause> dwell so the spindle reaches
  /// commanded RPM before the cut starts. Critical for hand-controllers
  /// without spindle-at-speed feedback. Default 1.
  pause?: number;
  /// Wirbeln (rt1.25 / 3e5): per-tool helical-spiral overlay flag.
  /// When enabled with `wirbelnExtraWidthMm > 0`, every cut move
  /// using this tool is subdivided and the cutter centerline spirals
  /// around the toolpath — engagement bounded at each point.
  /// Default false.
  wirbeln?: boolean;
  /// Wirbeln spiral diameter (Estlcam Wirbelzusatzbreite, 3e5): mm.
  /// Net cut width becomes `diameter + wirbelnExtraWidthMm`. None /
  /// 0 ⇒ overlay disabled (Wirbeln is a no-op).
  wirbelnExtraWidthMm?: number;
  /// Wirbeln stride along the toolpath per full spiral revolution
  /// (Estlcam T_Wirbel_Stepover, 3e5): mm. None ⇒ half the spiral
  /// radius (one-revolution overlap).
  wirbelnStepoverMm?: number;
  /// Wirbeln Z-wobble amplitude (Estlcam T_Osc, 3e5): mm. Overlay
  /// adds a `cos(3θ)·osc − osc` Z ripple between revolutions for
  /// chip evacuation. None / 0 ⇒ flat.
  wirbelnOscMm?: number;
  /// Default depth-per-pass (negative, mm). Operations using this tool
  /// inherit this when their own `step` is unset.
  defaultStep?: number;
  /// Default XY overlap (0..1) for pocket / cascade ops that don't set
  /// their own `xyOverlap`. Mirrors `defaultStep` (dr5). Undefined =
  /// fall through to the global 0.5 default.
  defaultXyOverlap?: number;
  /// Free-text comment / description (rt1.31). Surfaced as the tooltip
  /// on the tool select in OpPropertiesPanel and as a multi-line text
  /// area in ToolLibraryDialog. Doesn't affect any pipeline output.
  comment?: string;
  /// Length of cutting flutes in mm. Undefined = treat the entire tool
  /// as cutting (legacy behavior — no holder collision check is done).
  fluteLengthMm?: number;
  /// Shank diameter in mm. Undefined = same as `diameter`
  /// (parallel-shank bit). Drives the holder/shank collision sweep.
  shankDiameterMm?: number;
  /// q0kc: free shank length between the top of the cutting flutes and
  /// the bottom of the holder/collet (mm). Models reach-extension
  /// tooling where the collet doesn't grip right above the flutes.
  /// Undefined / 0 = legacy behavior (collet sits directly on flutes).
  stickoutLengthMm?: number;
  /// mmu8: laser kerf width (mm) — the heightmap-side spot radius the
  /// sim carves at. Honored only when kind === 'laser_beam'. Undefined
  /// = the legacy 0.15 mm default in the Rust sim.
  kerfMm?: number;
  /// z1y0: spindle direction the post commands when this tool is
  /// selected. Default 'cw' (M3); 'ccw' (M4) for left-hand cutters /
  /// reverse-thread / mirror-helix tooling. Skipped on the wire when
  /// at default so legacy projects round-trip unchanged.
  spindleDirection?: SpindleDirection;
  /// Holder geometry above the shank. Undefined = no holder check.
  holder?: HolderShape;
}

/// Tool holder geometry above the shank. Mirrors
/// `wiac_core::project::HolderShape`. v1 treats every holder as
/// cylindrically symmetric — set-screw flats and asymmetric ER nuts
/// are bounded by their enclosing cylinder/cone.
export type HolderShape =
  | { kind: 'cylinder'; diameter_mm: number; length_mm: number }
  | { kind: 'cone'; bottom_diameter_mm: number; top_diameter_mm: number; length_mm: number }
  | {
      kind: 'stepped';
      cylinder_diameter_mm: number;
      cylinder_length_mm: number;
      cone_top_diameter_mm: number;
      cone_length_mm: number;
    };

export interface AxisLimits {
  x: number;
  y: number;
  z: number;
}

export interface MachineSettings {
  /// h0tx: free-text identifier for this machine ("Shop CNC",
  /// "Garage MPCNC"). Surfaces in the MachineDialog header + the
  /// .wiac-machine.json save file. Empty by default.
  name?: string;
  /// h0tx: which op kinds the machine can run. Drives the
  /// OpKindPicker's filter — a laser-only machine doesn't show
  /// milling ops. Empty array = implicitly `[mode]` (back-compat
  /// for projects that predate this field).
  capabilities?: ('mill' | 'laser' | 'drag')[];
  unit: 'mm' | 'inch';
  mode: 'mill' | 'laser' | 'drag';
  comments: boolean;
  arcs: boolean;
  supportsToolchange: boolean;
  fastMoveZ: number;
  /// Per-axis acceleration (mm/s²). Optional — empty means defaults
  /// (250 mm/s² per axis, LinuxCNC convention).
  accel?: AxisLimits;
  /// Per-axis jerk (mm/s³). Optional — empty means trapezoidal-only
  /// profiling (S-curve is Phase 2).
  jerk?: AxisLimits;
  /// Tool-change time in seconds (default 5).
  toolchangeS?: number;
  /// Rapid (G0) speed in mm/min (default 5000).
  rapidSpeed?: number;
  /// Machine work-area envelope in mm — drives the stock's auto-mode
  /// fallback when no geometry is imported (the stock sizes to this
  /// XY footprint). Default 200×300×50 (a typical hobby gantry).
  workArea?: AxisLimits;
  /// Maximum chord-to-arc deviation (mm) when collapsing line runs into
  /// G2/G3 on emit. Only consulted when `arcs == true`. undefined ⇒
  /// 0.01 mm (the backend default).
  arcFitToleranceMm?: number;
  /// Output gcode dialect / post-processor. Chosen per-machine (a
  /// controller speaks one dialect) rather than per-run. `linuxcnc` =
  /// standard RS-274; `grbl` = hobby-CNC subset; `hpgl` = plotter /
  /// drag-knife. Undefined ⇒ fall back to the last-used / linuxcnc.
  gcodeDialect?: 'linuxcnc' | 'grbl' | 'hpgl';
  /// Decimal separator for emitted numbers (rt1.36). Default '.';
  /// switch to ',' for European Siemens / Heidenhain controllers.
  decimalSeparator?: '.' | ',';
  /// Starting line number for `N<n>` prefixes (rt1.36). Undefined
  /// disables numbering. `10` produces `N10`, `N20`, … on every line.
  lineNumberStart?: number;
  /// Plot-mode Z (rt1.35): when true, the pipeline collapses every
  /// cut to ONE pass at the op's cut depth and skips multi-step
  /// descent / ramp / helix. Z values written into gcode are
  /// restricted to fast_move_z (pen up) and cut depth (pen down).
  /// Right setting for laser / plasma / pen plotter / 3D-printer
  /// extrusion / drag-knife controllers.
  plotModeZ?: boolean;
  /// User-configurable post-processor profile (rt1.15). When set,
  /// the built-in posts (linuxcnc / grbl) use its template strings
  /// instead of their hard-coded program_start / program_end /
  /// tool_change / coolant lines. Undefined ⇒ defaults.
  postProfile?: PostProfile;
  /// 3nnj: lower bound on the spindle RPM the controller will accept.
  /// Tool / op RPMs below this clamp UP to the min and emit a
  /// `spindle_speed_clamped_below_min` warning. Undefined disables
  /// the floor (default; back-compat).
  spindleRpmMin?: number;
  /// 3nnj: upper bound on the spindle RPM the controller will accept.
  /// Tool / op RPMs above this clamp DOWN to the max and emit a
  /// `spindle_speed_clamped_above_max` warning. Undefined disables
  /// the ceiling (default; back-compat).
  spindleRpmMax?: number;
  /// jcmx: upper bound on the cutting / plunge feed (mm/min) the machine
  /// can drive. Feeds above this clamp DOWN to the max and emit a
  /// `feed_clamped_above_max` warning. Undefined disables the ceiling
  /// (default; back-compat).
  maxFeedMmMin?: number;
  /// Spindle-start dwell (seconds) inserted into the M6 toolchange
  /// envelope after `M3 S<rpm>`. Lets the new tool come up to
  /// commanded RPM before the next cut. Undefined ⇒ 0.5 s default.
  spindleStartDwellSec?: number;
  /// Spindle-stop dwell (seconds) inserted into the M6 toolchange
  /// envelope between `M5` and the actual `T<n> M6`. Gives the
  /// spindle time to spin down before the chuck is touched.
  /// Undefined ⇒ 0.5 s default.
  spindleStopDwellSec?: number;
  /// syol: when true, the program_end footer adds a `G53 G0 X0 Y0`
  /// retract-to-machine-home before the spindle-off + M30 sequence.
  /// When false, falls back to a `G0 X0 Y0` in the current WCS
  /// (work zero). Both modes lift to `fast_move_z` first. Default
  /// false.
  parkAtHome?: boolean;
  /// syol: optional explicit park XY (mm, in WCS coordinates). When
  /// set, the program_end footer routes the head to this point after
  /// the safe-Z lift, overriding the machine-home / work-zero
  /// fallback. Only meaningful when `parkAtHome` is false (the WCS
  /// fallback path). Emitted as `[x, y]` on the wire.
  parkXy?: [number, number];
}

/// Mirror of `wiac_core::gcode::post_profile::PostProfile` (rt1.15).
/// Every template field is optional — `None` keeps the built-in
/// emitter's hard-coded behavior. Templates accept token markers
/// substituted at emit time: `<version>`, `<unit>`, `<t>` (tool
/// number), `<n>` (tool name), `<d>` (tool diameter), `<f>` (feed),
/// `<s>` (spindle), `<op>` (op name), `<nl>` (newline).
export interface PostProfile {
  name?: string;
  file_extension?: string;
  line_ending?: string;
  program_start?: string;
  program_end?: string;
  tool_change?: string;
  coolant_flood_on?: string;
  coolant_flood_off?: string;
  coolant_mist_on?: string;
  coolant_mist_off?: string;
  /// Per-axis output formatting (hev). When set, replaces the
  /// hard-coded `X{val} Y{val} Z{val}` / `F{rate}` / `S{rpm}`
  /// emission with the user's axis names + printf-ish format +
  /// scale. Disabled axes drop out of the output entirely.
  axes?: AxesConfig;
}

/// Mirror of `wiac_core::gcode::post_profile::AxisFormat`. The
/// printf-ish `format` string supports `%[flags][width][.precision]<f|d|g|e>`.
/// `scale` is applied before formatting (`-1.0` flips Z-down for a
/// Z-up controller; `25.4` ad-hoc converts inch→mm).
export interface AxisFormat {
  enabled: boolean;
  name: string;
  format: string;
  scale: number;
}

/// Mirror of `wiac_core::gcode::post_profile::AxesConfig`. All seven
/// axes are required so the Rust deserializer doesn't need to
/// reconstruct defaults — the FE always sends a complete bundle.
export interface AxesConfig {
  x: AxisFormat;
  y: AxisFormat;
  z: AxisFormat;
  i: AxisFormat;
  j: AxisFormat;
  feed: AxisFormat;
  speed: AxisFormat;
}

/// Helper: an axes config that exactly matches the legacy hand-written
/// behavior (X/Y/Z with three decimals, F/S as integers, identity
/// scale, all enabled). Use this as the starting point when a user
/// switches the per-axis section on for the first time.
export function defaultAxesConfig(): AxesConfig {
  const coord = (name: string): AxisFormat => ({
    enabled: true,
    name,
    format: '%.3f',
    scale: 1.0,
  });
  const int = (name: string): AxisFormat => ({
    enabled: true,
    name,
    format: '%d',
    scale: 1.0,
  });
  return {
    x: coord('X'),
    y: coord('Y'),
    z: coord('Z'),
    i: coord('I'),
    j: coord('J'),
    feed: int('F'),
    speed: int('S'),
  };
}

export type PocketStrategy = 'cascade' | 'zigzag' | 'spiral' | 'trochoidal' | 'halfpipe';

/// Halfpipe cross-section profile (rt1.19). `circular_arc` for a
/// ball-bottom slot with the given radius; `v_bottom` for a V-bottom
/// slot with the given included angle (equivalent to V-Carve).
export type HalfpipeProfile =
  | { kind: 'circular_arc'; radius_mm: number }
  | { kind: 'v_bottom'; included_angle_deg: number };

/// Pattern repetition for an Operation. Mirrors
/// `wiac_core::project::PatternConfig`. Each tagged variant matches
/// the Rust snake_case discriminator. The (0, 0) / 0° instance is
/// the original geometry, so a single-count pattern is identical to
/// no pattern.
export type PatternConfig =
  | { kind: 'linear'; count: number; dx: number; dy: number }
  | { kind: 'grid'; count_x: number; count_y: number; dx: number; dy: number }
  | {
      kind: 'polar';
      count: number;
      center_x: number;
      center_y: number;
      angle_step_deg: number;
      /// First-instance angle offset around the center (degrees).
      /// Default 0 — instance 0 sits at angle_step_deg * 0 + start.
      start_angle_deg?: number;
    };

/// Per-op tab placement mode (rt1.10). Maps to
/// `wiac_core::project::TabPlacementMode`.
export type TabPlacementMode =
  | { kind: 'off' }
  | { kind: 'auto'; count: number }
  | { kind: 'manual' }
  | { kind: 'mixed'; auto_count: number };

/// A user-placed tab anchored geometry-relative (rt1.10). The
/// `objectId` is 1-based to match `sourceObjects`; `t ∈ [0, 1)` is
/// the arc-length parameter along the chained object.
export interface TabPlacement {
  objectId: number;
  t: number;
  /// Optional per-tab width override (mm).
  widthOverrideMm?: number;
  /// Optional per-tab height override (mm).
  heightOverrideMm?: number;
}
/// Cut direction for milling. `conventional` is the safer default —
/// cutter rotation opposes the feed at the contact point so chip starts
/// thin and grows; works on machines with backlash. `climb` is rotation
/// with feed → better surface finish but needs a rigid stiff machine.
/// See wiac_core::project::CutDirection for the winding rules.
export type CutDirection = 'conventional' | 'climb';

/// Plunge entry strategy. `direct` is a straight Z dive (current
/// behavior); `ramp` walks forward along the path while descending Z so
/// the cutter takes a chip in both directions simultaneously — required
/// for non-center-cutting bits and for harder materials. `helix` is a
/// start-of-cut spiral descent on a small circle inside the closed
/// pocket boundary — the standard for non-center-cutting endmills and
/// harder materials. Angles are in degrees, conservative default 3°.
/// Helix `radius_mm` is the spiral radius; pick something larger than
/// the tool radius so the helix carves a small clearance hole inside
/// the pocket. Sane default: 1.5 × tool radius. Set to null to auto-fit
/// the helix to the largest inscribed circle of the pocket boundary.
export type PlungeStrategy =
  | { kind: 'direct' }
  | { kind: 'ramp'; angle_deg: number }
  | { kind: 'helix'; angle_deg: number; radius_mm: number | null };
/// Drill cycle for an OperationKind::Drill op. Mirrors wiac_core::project::DrillCycle.
/// `simple` → G81; `peck` → G83 (full retract between pecks); `chip_break` → G73
/// (small partial retract between pecks). `dwell_sec` is the dwell at bottom in
/// seconds (0 = no dwell). `peck_step_mm` is the per-peck Z step.
export type DrillCycle =
  | { kind: 'simple'; dwell_sec?: number }
  | { kind: 'peck'; peck_step_mm: number; dwell_sec?: number }
  | { kind: 'chip_break'; peck_step_mm: number; dwell_sec?: number };

/// Thin frontend mirror of wiac_core::project::Operation. Tracks just
/// what the UI needs to show + edit; the wire format expands to the
/// full Operation when Generate ships.

/// Non-destructive file-level transform (bww). Applied to the entire
/// imported drawing as a layout convenience — translates, rotates, scales,
/// and / or mirrors every segment so the user can position the part on
/// stock for good material use without re-exporting from CAD.
///
/// All non-translate ops use a fixed pivot: the ORIGINAL (untransformed)
/// file bbox center. Application order: scale → mirrors → rotate → translate.
/// Bulge handling follows `crates/wiac-core/src/cam.rs` — only mirrors flip
/// it; scale / rotate / translate leave it unchanged.
///
/// `identityFileTransform()` returns the no-op identity; consumers should
/// short-circuit and return the original `ImportResponse` reference when
/// the transform compares equal to it (cheap deep-equal in
/// `applyFileTransform`).
export interface FileTransform {
  translate: { x: number; y: number };
  rotateDeg: number;
  scale: number;
  mirrorX: boolean;
  mirrorY: boolean;
}

export function identityFileTransform(): FileTransform {
  return {
    translate: { x: 0, y: 0 },
    rotateDeg: 0,
    scale: 1,
    mirrorX: false,
    mirrorY: false,
  };
}

export function isIdentityFileTransform(t: FileTransform): boolean {
  return (
    t.translate.x === 0 &&
    t.translate.y === 0 &&
    t.rotateDeg === 0 &&
    t.scale === 1 &&
    !t.mirrorX &&
    !t.mirrorY
  );
}

/// One slot in `project.imports[]` (wrsu Phase 1). Each entry holds the
/// imported drawing, its own non-destructive layout transform (bww),
/// and the absolute path on disk for the source-file watcher.
/// Multi-file workflows (wrsu Phase 2+) just push more entries onto
/// the array; today the typical project has 0 or 1.
export interface ImportEntry {
  /// 1-based id assigned at import time; stable across save/load. Future
  /// per-entry mutations (transform edits, removal) key off this rather
  /// than array index so reordering / undo works cleanly.
  id: number;
  source: ImportResponse;
  fileTransform: FileTransform;
  /// Absolute path on disk to the source DXF/SVG. Drives the
  /// source-file watcher (auto-reload toast on change). `null` for
  /// imports created via paste / drop / Add Text rather than file load.
  lastImportPath?: string | null;
}

/// i5g4: gcode work-coordinate system identifier. Mirror of
/// `wiac_core::project::Wcs` (serde `rename_all = "UPPERCASE"`).
export type Wcs = 'G54' | 'G55' | 'G56' | 'G57' | 'G58' | 'G59';

/// i5g4: program-level work-coordinate offset between the geometry
/// frame (where the DXF / SVG was drawn) and the gcode WCS origin
/// (where the user zeros the spindle on the real machine). All-zeros
/// + G54 = "geometry origin = WCS origin", the legacy default.
/// Mirror of `wiac_core::project::WorkOffset`.
export interface WorkOffset {
  x_mm: number;
  y_mm: number;
  z_mm: number;
  wcs: Wcs;
}

export function defaultWorkOffset(): WorkOffset {
  return { x_mm: 0, y_mm: 0, z_mm: 0, wcs: 'G54' };
}

export function isDefaultWorkOffset(w: WorkOffset): boolean {
  return w.x_mm === 0 && w.y_mm === 0 && w.z_mm === 0 && w.wcs === 'G54';
}

/// Pick a `WorkOffset` for a freshly-imported drawing such that the
/// gcode WCS origin sits at the geometry's bottom-left corner — the
/// canonical CNC zeroing convention (audit gldc). Without this auto-
/// default, drawings drawn off-origin in CAD (e.g. a part bbox of
/// (5.76, 5.79) → (24.22, 24.24)) fire the
/// `stock_origin_outside_geometry_bbox` pipeline warning, because the
/// pipeline thinks the operator will zero at (0, 0) in geometry space
/// while every realistic operator will zero at a stock CORNER.
///
/// Respects user intent: leaves `current` unchanged if (a) the user
/// already moved away from the default offset, (b) the bbox is
/// degenerate / non-finite, or (c) the bbox ALREADY contains the
/// origin (so the default WCS-at-origin is already correct).
///
/// Pure / dependency-free so the inference can be unit-tested without
/// loading the Svelte rune compiler.
export function inferDefaultWorkOffset(
  bbox: { min_x: number; min_y: number; max_x: number; max_y: number } | null,
  current: WorkOffset,
): WorkOffset {
  if (!isDefaultWorkOffset(current)) return current;
  if (!bbox) return current;
  const { min_x, min_y, max_x, max_y } = bbox;
  if (
    !Number.isFinite(min_x) ||
    !Number.isFinite(min_y) ||
    !Number.isFinite(max_x) ||
    !Number.isFinite(max_y)
  ) {
    return current;
  }
  if (max_x < min_x || max_y < min_y) return current; // degenerate
  // 1e-3 mm slack so paths drawn exactly to the origin edge don't
  // trigger an offset — matches the slack in pipeline/warnings.rs.
  const slack = 1e-3;
  const containsOrigin =
    min_x - slack <= 0 && 0 <= max_x + slack && min_y - slack <= 0 && 0 <= max_y + slack;
  if (containsOrigin) return current;
  return { ...current, x_mm: min_x, y_mm: min_y };
}

export interface ProjectFile {
  kind: 'wiac-project';
  version: 1;
  imports: ImportEntry[];
  visibleLayers: string[];
  selectedEntities: number[];
  stock?: StockConfig;
  tools?: ToolEntry[];
  machine?: MachineSettings;
  operations?: OpEntry[];
  fixtures?: Fixture[];
  textLayers?: TextLayer[];
  /// i5g4: program-level WCS offset. Undefined / all-zero @ G54 means
  /// "geometry origin = WCS origin" (the legacy default; round-trips
  /// for legacy files lacking the field).
  workOffset?: WorkOffset;
}

/// Persistent text entity — editable text + typography + transform.
/// Phase 1 of the text-engraving rework: the pipeline (phase 2) will
/// render these to segments at generate time so edits propagate to
/// gcode without re-baking. Distinct from DXF TEXT/MTEXT segments that
/// currently land in `imported` as opaque polylines (phase 4 will route
/// those through TextLayer too).
export type TextAlignment = 'left' | 'center' | 'right';
export type TextLayerKind = 'TEXT' | 'MTEXT';
/// Font payload for a TextLayer. The `kind` tag drives display labelling
/// (bundled-font dropdown vs. user-uploaded filename) but TTF/OTF bytes
/// are stored as base64 in BOTH variants so the build-project payload
/// doesn't need async font resolution at every Generate. The caller is
/// responsible for fetching the bundled .ttf once and stashing the
/// bytes here.
export type TextFontSource =
  | { kind: 'bundled'; path: string; bytes_b64: string }
  | { kind: 'user'; filename: string; bytes_b64: string };
export interface TextLayer {
  id: number;
  kind: TextLayerKind;
  /// Display name in the sidebar list. Defaults to e.g. `TEXT — "Hello"`
  /// but the user can rename via the inline edit form (phase 3).
  name: string;
  /// Full string. For MTEXT, `\n` separates lines.
  text: string;
  fontSource: TextFontSource;
  sizeMm: number;
  origin: { x: number; y: number };
  rotationDeg: number;
  letterSpacingMm: number;
  /// MTEXT line spacing in mm. Ignored when kind === 'TEXT'. 0 = default
  /// (~1.2 * sizeMm — the renderer picks the value).
  lineSpacingMm: number;
  alignment: TextAlignment;
  /// Horizontal stretch factor (969h). 1.0 = font natural width; UI
  /// exposes 0.5–2.0 (50–200 %). Backend clamps so legacy files without
  /// the field (deserialised as default 1.0) render unchanged.
  widthScale: number;
  /// Detection from `is_single_line_font` on the most recent render —
  /// cached so the UI can show "single-line" without re-fetching the
  /// font. Refreshed when fontSource changes.
  singleLine: boolean;
}
