// Pure-TypeScript op-type aliases. Lives outside `project.svelte.ts` so
// modules without a Svelte runtime (helpers, vitest specs) can import the
// shapes without dragging in `$state`.

import type {
  CutDirection,
  DrillCycle,
  HalfpipeProfile,
  PatternConfig,
  PlungeStrategy,
  PocketStrategy,
  TabPlacement,
  TabPlacementMode,
} from './project.svelte';

export type ToolKind =
  | 'endmill'
  | 'ball_nose'
  | 'v_bit'
  | 'engraver'
  | 'drag_knife'
  | 'drill'
  | 'laser_beam'
  | 'bull_nose'
  | 'compression'
  | 'form_profile'
  | 'cone'
  | 'thread_mill'
  | 'plasma_torch';

/// The machine's primary mode — what the gcode emitter targets.
/// Mirrors the Rust `MachineMode` wire enum
/// (crates/ivac-core/src/project/machine.rs). Shared by
/// `MachineSettings` and the tool↔mode compatibility table in
/// tool_family.ts.
export type MachineMode = 'mill' | 'laser' | 'drag' | 'plasma';

export type OpKind =
  | 'profile'
  | 'pocket'
  | 'drill'
  | 'thread'
  | 'chamfer'
  | 'engrave'
  | 'drag_knife'
  | 't_slot'
  | 'dovetail'
  | 'vcarve'
  | 'pause'
  | 'homing'
  | 'probe'
  | 'cycle_marker'
  | 'gcode_include'
  | 'relief_mill'
  | 'raster_engrave';

/// The program-only op family — Pause, Homing, Probe,
/// CycleMarker, GcodeInclude. These ops emit fixed gcode
/// sequences (M0 / G28 / G38.2 / a comment / an included file)
/// and never touch a cutter, so the tool-existence validator
/// must NOT flag them as `tool_id: 0 missing`. Mirrors the Rust
/// `Op::is_program_only()` predicate at
/// `crates/ivac-core/src/project/op.rs`. Keep both in sync when
/// new program-only kinds land.
export function isProgramOnlyOp(kind: OpKind): boolean {
  return (
    kind === 'pause' ||
    kind === 'homing' ||
    kind === 'probe' ||
    kind === 'cycle_marker' ||
    kind === 'gcode_include'
  );
}

/// Axis selector for ProbeOp. Wire is the bare lowercase
/// letter for direct concatenation into the G38.2 word.
export type ProbeAxis = 'x' | 'y' | 'z';

export type ScanDirection = 'along_x' | 'along_y';

/// Brightness → laser-power (`S`) mapping for raster engraving.
/// Tagged union on `kind`, mirroring the wire `PowerCurve` 1:1 — the
/// only field-name difference is bayer's `matrixSize` ↔ wire
/// `matrix_size`, translated in build-project.ts. Convention: dark
/// pixels burn hotter (see `crates/ivac-core/src/cam/raster.rs`).
///   - linear:         power lerps `max`@black → `min`@white
///   - threshold:      binary — darker than `level` burns at `power`
///   - floyd_steinberg error-diffusion dither to on/off at `level`
///   - bayer:          ordered dither; `matrixSize` ∈ {2,4,8}
export type PowerCurve =
  | { kind: 'linear'; min: number; max: number }
  | { kind: 'threshold'; level: number; power: number }
  | { kind: 'floyd_steinberg'; level: number; power: number }
  | { kind: 'bayer'; matrixSize: number; power: number };

export type PowerCurveKind = PowerCurve['kind'];

/// How consecutive raster rows are connected — lift-between
/// (every row same direction) vs boustrophedon (alternating, no lift).
export type RasterLink = 'lift_between' | 'bidirectional';

export type ProfileOffset = 'outside' | 'inside' | 'on';
export type SourceCombine = 'auto' | 'union' | 'difference' | 'intersection' | 'xor' | 'none';
export type FrameShape = 'rectangle' | 'rounded_rectangle';

export type TabType = 'rectangle' | 'ramp';
export type LeadKind = 'off' | 'straight' | 'arc';

/// Fields every op carries, regardless of kind. New op kinds extend
/// this; per-kind fields live on the kind-specific interfaces.
export interface OpBase {
  id: number;
  name: string;
  enabled: boolean;
  toolId: number;
  /// Source selection:
  ///   null            → all imported geometry
  ///   string[]        → only chains whose layer name is listed
  /// `sourceObjects` (if set) narrows further to specific chain ids.
  sourceLayers: string[] | null;
  sourceObjects?: number[];
  sourceCombine?: SourceCombine;
  depth: number;
  startDepth: number;
  /// Per-pass Z step (negative). null = inherit from the assigned
  /// tool's `defaultStep`.
  step: number | null;
  plunge?: PlungeStrategy;
  feedRateOverride?: number;
  plungeRateOverride?: number;
  cornerFeedReduction?: number;
  throughDepth?: number;
  /// Explicit ordered list of Z depths (negative). When non-empty,
  /// overrides `step` / `finishStep` / `throughDepth`.
  depthList?: number[];
  pattern?: PatternConfig;
  /// Optional group label. Consecutive enabled ops sharing the
  /// same value belong to the same logical phase ("rough",
  /// "finish", …); the pipeline emits a `; === GROUP: <name> ===`
  /// boundary marker in the G-code at every transition. Empty
  /// string is treated the same as undefined.
  group?: string;
  /// Pin this op's position when the project-level
  /// `groupOpsByTool` reorder is on. A pinned op (like any program-only
  /// op) is a fixed barrier: it keeps its slot and grouping never moves
  /// another op across it. Use it to lock a stability-critical cut order
  /// (tabs, thin walls). Ignored when grouping is off. Default false.
  pinOrder?: boolean;
}

/// Fields shared by closed-contour ops (profile + pocket): cut
/// direction, lead-in / lead-out, tabs, and the approach point.
export interface ContourFields {
  cutDirection?: CutDirection;
  finishCutDirection?: CutDirection;
  leadInKind?: LeadKind;
  leadOutKind?: LeadKind;
  /// Lead-in size in mm. Length when `leadInKind=straight`, arc radius
  /// when `leadInKind=arc`. Ignored when `leadInKind=off`.
  leadIn?: number;
  leadOut?: number;
  /// Optional smaller step for the FINAL Z pass (cleaner bottom).
  finishStep?: number;
  tabType?: TabType;
  tabRampAngleDeg?: number;
  tabWidth?: number;
  tabHeight?: number;
  tabsActive?: boolean;
  tabMode?: TabPlacementMode;
  tabPlacements?: TabPlacement[];
  /// User-picked XY where the cutter enters
  /// each closed offset.
  approachPoint?: [number, number];
}

/// Profile (contour) op — cuts the boundary of selected closed shapes
/// or stamps open polylines verbatim. `offset` picks which side of the
/// line the cutter rides.
export interface ProfileOp extends OpBase, ContourFields {
  kind: 'profile';
  offset: ProfileOffset;
}

/// Pocket op — clears the inside of selected closed shapes.
/// `pocketStrategy` picks the fill pattern. `pocketStrategy: null` is
/// a legacy value from very old saves and is treated as 'cascade'.
export interface PocketOp extends OpBase, ContourFields {
  kind: 'pocket';
  pocketStrategy: PocketStrategy | null;
  /// XY overlap fraction in (0.05, 0.95) — drives cascade step
  /// (= tool_diameter * (1 - overlap)) and zigzag stride.
  xyOverlap?: number;
  /// Trochoidal engagement angle in degrees.
  engagementAngleDeg?: number;
  /// Trochoidal loop radius as a fraction of tool radius.
  loopRadiusFactor?: number;
  /// Zigzag raster angle in degrees. 0 (default) = horizontal
  /// sweeps; 90 = vertical; 45 = diagonal. Honored when
  /// `pocketStrategy === 'zigzag'`. Wire-compatible: 0 serialises as
  /// the bare `"zigzag"` string, non-zero as a tagged object.
  pocketZigzagAngleDeg?: number;
  /// Halfpipe cross-section profile. Honored when
  /// `pocketStrategy === 'halfpipe'`.
  halfpipeProfile?: HalfpipeProfile;
  /// Dual-tool finish: when set to a tool distinct from `toolId`, the
  /// pipeline emits a toolchange after the rough cascade and cuts the
  /// wall ring with the finish tool's finish-set feed/speed.
  finishToolId?: number;
  /// XY stock allowance (positive, mm) left UNCUT at the wall by the
  /// roughing pass, removed by a dedicated finish ring.
  finishXyAllowanceMm?: number;
  /// Pocket-Outside: when set, the op carves the area between
  /// a synthetic frame and the source selection.
  frameShape?: FrameShape;
  framePaddingMm?: number;
  frameCornerRadiusMm?: number;
}

/// Drill op — runs a drill cycle (simple / peck / chip-break) at each
/// selected POINT or small-circle entity. `drillCycle.kind` picks the
/// G-code variant emitted (G81 / G83 / G73).
export interface DrillOp extends OpBase {
  kind: 'drill';
  drillCycle: DrillCycle;
  /// Post-drill rim chamfer width. After the
  /// drill cycle, the cutter walks a constant-Z revolution at each
  /// hole's edge at `z = -width / tan(tipAngle / 2)`.
  chamferAfterWidthMm?: number;
  /// Dedicated chamfer tool for Stufenfase. When unset, the drill
  /// tool itself is used.
  finishToolId?: number;
  /// Optional spot-drill pre-pass — a shallow centre spot with a
  /// stiffer tool before the main drill, to stop a twist drill walking
  /// on hard / polished stock. Undefined = no spot pass. `spotDepthMm`
  /// is negative (depth below stock).
  spotFirst?: { spotDepthMm: number; spotToolId: number };
}

/// Chamfer op — single-pass constant-Z bevel along the
/// selected closed contour, depth driven by V-bit cone math
/// (`z = -width / tan(tipAngle / 2)`).
export interface ChamferOp extends OpBase {
  kind: 'chamfer';
  /// Chamfer width on the workpiece, mm. Optional so the UI can
  /// clear the value (user types 0 ⇒ unset); pipeline treats unset
  /// as zero-width = no chamfer.
  chamferWidthMm?: number;
  /// Optional second pass at finish-set feed/speed for surface
  /// quality.
  chamferFinishPass?: boolean;
}

/// V-Carve op — medial-axis carve where Z depth at each path point is
/// determined by the inscribed-circle radius (V-bit cone math).
export interface VCarveOp extends OpBase {
  kind: 'vcarve';
  /// Cap on the inscribed-circle radius (mm). Undefined = no cap.
  carveMaxWidthMm?: number;
  /// Multi-pass refinement toggle.
  multiPassRefine?: boolean;
  /// Trace the full medial axis. Default (undefined / false) =
  /// Estlcam-style perimeter-only — the cutter traces the boundary
  /// offset inward by `R = effective_r_cap` at constant depth, leaving
  /// the centre plateau untouched. Set true for the rare "carve a depth
  /// gradient across the entire interior" workflow — the full medial
  /// axis (Aspire-style relief).
  fullMedialAxis?: boolean;
  /// Extra inward offset applied to the source region BEFORE
  /// the V-Carve pass. Used to build the "plug" side of an inlay pair —
  /// the plug is `sourceInsetMm` smaller per side than the pocket so
  /// it wedges into the pocket walls with that clearance when glued in.
  /// Pocket side leaves this undefined / 0; plug side sets it to the
  /// shared gap (typical 0.05–0.2 mm).
  sourceInsetMm?: number;
}

/// Thread op — helical pass cutting an internal or external
/// thread at the given pitch.
export interface ThreadOp extends OpBase {
  kind: 'thread';
  /// Z descent per full helix revolution (mm). Optional so the UI
  /// can clear the value (user types 0 ⇒ unset); pipeline warns
  /// when unset rather than emitting a zero-pitch helix.
  threadPitchMm?: number;
  /// `true` = internal (tap-style, inside the bore); `false` =
  /// external (around a stud).
  threadInternal?: boolean;
  /// Climb (CCW) vs conventional (CW). Default conventional.
  threadClimb?: boolean;
}

/// Engrave op — traces selected paths verbatim at the configured
/// depth. Always uses `offset: 'on'`.
export interface EngraveOp extends OpBase {
  kind: 'engrave';
  offset: ProfileOffset;
}

/// Drag-knife op — single-line cuts for vinyl cutters. `offset` is
/// always `'on'` and the dragoff geometry comes from the tool entry.
export interface DragKnifeOp extends OpBase {
  kind: 'drag_knife';
  offset: ProfileOffset;
}

/// T-slot / undercut op — drives a T-slot / keyway cutter along
/// the source path as the slot centerline, at a single floor Z, so its
/// wide head carves the undercut. `offset` is always `'on'`; the head
/// width comes from the tool. Behaviorally a single-Z centerline follow
/// (like Engrave). Requires a `t_slot` tool and a pre-cut stem slot ≥
/// the neck width.
export interface TSlotOp extends OpBase {
  kind: 't_slot';
  offset: ProfileOffset;
}

/// Dovetail / form-profile undercut op — drives a `form_profile`
/// cutter (e.g. a dovetail bit, widest at the bottom) along the source
/// path as the groove centerline, at a single floor Z, so its angled
/// flanks carve the undercut walls. `offset` is always `'on'`; the
/// groove width comes from the tool profile. Behaviorally a single-Z
/// centerline follow (like Engrave). Requires a `form_profile` tool and
/// a pre-cut roughing channel ≥ the profile's narrowest width.
export interface DovetailOp extends OpBase {
  kind: 'dovetail';
  offset: ProfileOffset;
}

/// Program-level optional-stop op. Emits M5 → M0 → M3 at the
/// op's slot in the operations list, with the message rendered as a
/// gcode comment. No tool, no source — the op exists purely to pause
/// the controller so the operator can intervene (manual tool change,
/// inspect cut, flip stock for double-sided work).
export interface PauseOp extends OpBase {
  kind: 'pause';
  /// One-line message shown on the operator console. Empty string is
  /// allowed; the M0 stop still emits.
  message: string;
}

/// Machine-home building block. Emits `G28` then (by default) a
/// rapid retract to the op's safe Z. No tool / source / cut schedule —
/// program-only scaffolding so a project can express its shop
/// workflow (start of program, mid-program parking, end of program)
/// without writing G-code by hand.
export interface HomingOp extends OpBase {
  kind: 'homing';
  /// When true, the post follows `G28` with a rapid `G0 Z<safe>` to
  /// the op's `fastMoveZ`. Default true; most controllers don't end
  /// up at a useful Z after G28.
  retractToSafeZ: boolean;
}

/// Touch-probe building block. Emits a single `G38.2 <axis>
/// <distance> F<feed>` line — probing move that halts when the
/// trigger fires. Used at program start (zero WCS Z to the stock
/// top), between ops, or as a repeatability sanity check.
export interface ProbeOp extends OpBase {
  kind: 'probe';
  axis: ProbeAxis;
  /// Search distance in mm. Sign convention follows the controller —
  /// NEGATIVE Z to probe down into stock, positive X / Y for an
  /// edge-finder cycle from outside.
  distanceMm: number;
  /// Probe feedrate in mm/min. Typical 50–200 for a touch-trigger probe.
  feedMmMin: number;
}

/// Navigation marker. Emits ONLY a wrapped comment line at the
/// op's slot — no controller motion, no modal change. Pendants and
/// gcode viewers that index by program line can jump to the next
/// marker; also useful as a long-form note ("Flip stock NOW") that
/// survives gcode regeneration.
export interface CycleMarkerOp extends OpBase {
  kind: 'cycle_marker';
  /// Label text. Empty string is allowed but pointless.
  label: string;
}

/// External G-code include block. Splices an externally-
/// authored gcode file into the program stream at the op's slot,
/// with `{x}` / `{y}` / `{z}` / `{f}` / `{s}` / `{safe_z}` token
/// substitution against the post's live state. Program-only kind —
/// no tool, no source, no Z schedule.
///
/// Sim coverage: the heightmap-side simulator classifies the
/// included body line-by-line. G0/G1/G2/G3 + canned cycles
/// G73/G81/G82/G83 are carved by the unified preview-interpret pass;
/// everything else fires a counted `gcode_include_lines_skipped`
/// summary warning. When `verboseUnsimWarnings` is set, each
/// skipped line additionally fires a `gcode_include_unsim_line`
/// warning so the user can pinpoint exactly which lines were
/// skipped and why.
export interface GcodeIncludeOp extends OpBase {
  kind: 'gcode_include';
  /// Display-only path the file was loaded from. Empty string is
  /// allowed (the user can edit `content` by hand).
  path: string;
  /// The G-code body to splice in, verbatim except for `{name}`
  /// variable substitution at emit time.
  content: string;
  /// When true, fan out one `gcode_include_unsim_line` warning
  /// per skipped line in addition to the
  /// `gcode_include_lines_skipped` summary. Off by default so the
  /// warnings panel doesn't drown on a multi-skip block.
  verboseUnsimWarnings?: boolean;
}

/// 3-axis ball-nose relief surfacing. Finishes a curved Z(x,y)
/// surface (a `ReliefSource` referenced by `sourceId`, e.g. a grayscale
/// image) with a ball-nose cutter. The source's brightness maps to Z in
/// `[zMinMm, zMaxMm]`; `scallopHeightMm` drives the stepover unless
/// `stepoverMm` overrides. Has no source-geometry / offset semantics —
/// the surface comes from the relief source, not the imported chains.
export interface ReliefMillOp extends OpBase {
  kind: 'relief_mill';
  /// Id of the `ReliefSource` (in `project.data.reliefSources`) this op cuts.
  sourceId: number;
  /// Deepest cut Z (mm, negative) — where the darkest pixels map.
  zMinMm: number;
  /// Shallowest cut Z (mm) — where the brightest pixels map (usually 0).
  zMaxMm: number;
  /// Invert the brightness→Z mapping (dark = high instead of low).
  invert: boolean;
  /// Target scallop height between adjacent passes (mm).
  scallopHeightMm: number;
  /// Explicit stepover override (mm). null = derive from scallop.
  stepoverMm: number | null;
  /// Raster scanline direction.
  scanDirection: ScanDirection;
  /// Sampling pitch along each scanline (mm).
  alongStepMm: number;
}

/// Laser raster engraving. Burns a grayscale image (a
/// `ReliefSource` referenced by `sourceId`) row-by-row, modulating
/// laser power (`S`) per pixel through `powerCurve`. Like ReliefMill it
/// follows an image-derived field rather than source geometry, so it has
/// no offset / contour semantics. Laser-capability gated in the op
/// picker (OP_REQUIRES: ['laser']); the backend emits M3/S/M5 gcode.
export interface RasterEngraveOp extends OpBase {
  kind: 'raster_engrave';
  /// Id of the `ReliefSource` (in `project.data.reliefSources`) to engrave.
  sourceId: number;
  /// Per-pixel scan resolution (mm). The brightness grid is resampled to
  /// this pitch at planning time. 0 = use the source's native cell size.
  resolutionMm: number;
  /// Brightness → laser-power (`S`) mapping.
  powerCurve: PowerCurve;
  /// Raster scanline direction.
  scanDirection: ScanDirection;
  /// How consecutive rows connect (lift-between vs boustrophedon).
  link: RasterLink;
  /// Overscan past each row edge as a fraction of the row length (≥ 0)
  /// so the head reaches commanded power before crossing pixels.
  overscanFactor: number;
}

/// Tagged-union over every op kind. TypeScript narrows the variant
/// on `op.kind === '<value>'` so reads of kind-specific fields are
/// only valid inside the matching branch — wrong-kind reads (e.g.
/// `op.chamferWidthMm` on a ProfileOp) become compile-time errors
/// instead of silently undefined.
export type OpEntry =
  | ProfileOp
  | PocketOp
  | DrillOp
  | ChamferOp
  | VCarveOp
  | ThreadOp
  | EngraveOp
  | DragKnifeOp
  | TSlotOp
  | DovetailOp
  | PauseOp
  | HomingOp
  | ProbeOp
  | CycleMarkerOp
  | GcodeIncludeOp
  | ReliefMillOp
  | RasterEngraveOp;

/// Patch type for `project.updateOperation`. A patch covers the full
/// variant-specific shape — callers may pass `{ depth: -3 }` against
/// any op (an OpBase field) but `{ chamferWidthMm: 4 }` only matches
/// a ChamferOp. `Partial<OpEntry>` distributes into the union so
/// mixed-kind patches (e.g. `{ depth: -3, chamferWidthMm: 4 }`)
/// must satisfy at least one variant.
export type OpPatch = Partial<OpEntry>;

/// Type-level accessor for "the variant of OpEntry whose kind is K".
/// Used inside `project.svelte.ts` to type per-kind patch operations.
export type OpOfKind<K extends OpKind> = Extract<OpEntry, { kind: K }>;

/// Union of every field name across every variant of OpEntry —
/// `keyof OpEntry` alone gives only the intersection (= OpBase
/// fields), so kind-specific keys like `xyOverlap` or
/// `chamferWidthMm` get filtered out. The conditional distributes
/// the union before applying `keyof`, capturing every variant's
/// keys.
export type OpField = OpEntry extends infer T ? (T extends OpEntry ? keyof T : never) : never;

/// Value type for an OpEntry field across the variants that carry
/// it. For shared fields (e.g. `'depth'`) the distribution collapses
/// to one type; for variant-only fields (`'xyOverlap'`) it returns
/// the value as declared on its owning variant. Used by the
/// kind-aware `patch(field, value)` helper in OpPropertiesPanel.
export type OpFieldValue<K extends OpField> = OpEntry extends infer T
  ? T extends OpEntry
    ? K extends keyof T
      ? T[K]
      : never
    : never
  : never;

/// Predicate / type guard: this op is a closed-contour cutter
/// (profile or pocket), so it carries the ContourFields set —
/// lead-in / lead-out, tabs, cut direction, approach point.
export function isContourOp(op: OpEntry): op is ProfileOp | PocketOp {
  return op.kind === 'profile' || op.kind === 'pocket';
}

/// Predicate / type guard: this op rides the boundary of selected
/// objects rather than carving area / drilling points. Used by
/// rendering / tooling that highlights cut paths but not fills.
export function isPathOp(
  op: OpEntry,
): op is ProfileOp | EngraveOp | DragKnifeOp | TSlotOp | DovetailOp | VCarveOp {
  return (
    op.kind === 'profile' ||
    op.kind === 'engrave' ||
    op.kind === 'drag_knife' ||
    op.kind === 't_slot' ||
    op.kind === 'dovetail' ||
    op.kind === 'vcarve'
  );
}
