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
  | 't_slot'
  | 'form_profile';

export type OpKind =
  | 'profile'
  | 'pocket'
  | 'drill'
  | 'thread'
  | 'chamfer'
  | 'engrave'
  | 'drag_knife'
  | 'vcarve';

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
  /// Anfahrpunkt (rt1.26): user-picked XY where the cutter enters
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
  /// Halfpipe cross-section profile (rt1.19). Honored when
  /// `pocketStrategy === 'halfpipe'`.
  halfpipeProfile?: HalfpipeProfile;
  /// Dual-tool finish: when set to a tool distinct from `toolId`, the
  /// pipeline emits a toolchange after the rough cascade and cuts the
  /// wall ring with the finish tool's finish-set feed/speed.
  finishToolId?: number;
  /// XY stock allowance (positive, mm) left UNCUT at the wall by the
  /// roughing pass, removed by a dedicated finish ring (rt1.24).
  finishXyAllowanceMm?: number;
  /// Pocket-Outside (rt1.3): when set, the op carves the area between
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
  /// Stufenfase (rt1.20): drilled-hole rim chamfer width. After the
  /// drill cycle, the cutter walks a constant-Z revolution at each
  /// hole's edge at `z = -width / tan(tipAngle / 2)`.
  chamferAfterWidthMm?: number;
  /// Dedicated chamfer tool for Stufenfase. When unset, the drill
  /// tool itself is used.
  finishToolId?: number;
}

/// Chamfer op (rt1.18) — single-pass constant-Z bevel along the
/// selected closed contour, depth driven by V-bit cone math
/// (`z = -width / tan(tipAngle / 2)`).
export interface ChamferOp extends OpBase {
  kind: 'chamfer';
  /// Chamfer width on the workpiece, mm. Optional so the UI can
  /// clear the value (user types 0 ⇒ unset); pipeline treats unset
  /// as zero-width = no chamfer.
  chamferWidthMm?: number;
  /// Optional second pass at finish-set feed/speed for surface
  /// quality (rt1.27).
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
  /// r8ut: trace the full medial axis. Default (undefined / false) =
  /// Estlcam-style perimeter-only — the cutter traces the boundary
  /// offset inward by `R = effective_r_cap` at constant depth, leaving
  /// the centre plateau untouched. Set true to recover the prior wiac
  /// behaviour for the rare "carve a depth gradient across the entire
  /// interior" workflow (Aspire-style relief).
  fullMedialAxis?: boolean;
  /// rt1.7: extra inward offset applied to the source region BEFORE
  /// the V-Carve pass. Used to build the "plug" side of an inlay pair —
  /// the plug is `sourceInsetMm` smaller per side than the pocket so
  /// it wedges into the pocket walls with that clearance when glued in.
  /// Pocket side leaves this undefined / 0; plug side sets it to the
  /// shared gap (typical 0.05–0.2 mm).
  sourceInsetMm?: number;
}

/// Thread op (rt1.17) — helical pass cutting an internal or external
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

/// Tagged-union over every op kind. TypeScript narrows the variant
/// on `op.kind === '<value>'` so reads of kind-specific fields are
/// only valid inside the matching branch — wrong-kind reads (e.g.
/// `op.chamferWidthMm` on a ProfileOp) become compile-time errors
/// instead of silently undefined (audit-sue).
export type OpEntry =
  | ProfileOp
  | PocketOp
  | DrillOp
  | ChamferOp
  | VCarveOp
  | ThreadOp
  | EngraveOp
  | DragKnifeOp;

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
export type OpField = OpEntry extends infer T
  ? T extends OpEntry
    ? keyof T
    : never
  : never;

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
export function isPathOp(op: OpEntry): op is ProfileOp | EngraveOp | DragKnifeOp | VCarveOp {
  return (
    op.kind === 'profile' ||
    op.kind === 'engrave' ||
    op.kind === 'drag_knife' ||
    op.kind === 'vcarve'
  );
}
