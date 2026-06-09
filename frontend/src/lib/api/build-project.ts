// Adapter from the frontend ProjectState shape to the wire Project the
// ivac_core pipeline consumes. Camel-case → snake-case, and the
// kind-specific Operation params get materialized from the per-op
// entry's flat fields.

import type {
  ToolEntry as FrontToolEntry,
  OpEntry,
  MachineSettings,
  TextLayer,
  WorkOffset,
} from '../state/project.svelte';
import { isDefaultWorkOffset } from '../state/project-types';
import type { StockConfig, ReliefSource } from '../state/project-types';
import { computeFootprint } from '../sim/footprint';
import type {
  ChamferOp,
  ContourFields,
  DrillOp,
  OpBase,
  OpKind,
  PocketOp,
  PowerCurve,
  ProfileOffset,
  RasterEngraveOp,
  ReliefMillOp,
  ThreadOp,
  VCarveOp,
} from '../state/op_types';
import type { GenerateRequest, ImportResponse } from './types';
import type { components } from './generated';

/// Wire shapes are the generated schema types — single source of truth.
/// Adding/renaming a field on the Rust wire type + regenerating flows
/// straight through here, so this adapter can never silently drift from
/// the schema. The builders below map the camelCase UI model onto these.
/// (Where the frontend intentionally OMITS a field that the Rust struct
/// fills via `#[serde(default)]`, the schema still marks it required, so
/// a few builders cast at the boundary — flagged inline.)
type Schemas = components['schemas'];

/// Permissive view of an `OpEntry` that exposes every variant's optional
/// fields. The wire format flattens kind-specific params into one
/// `OperationParams` bag on the backend (`project.rs`, deserialized via
/// `#[serde(default, skip_serializing_if = ...)]`), so this adapter
/// reads across variants without per-kind branching. The narrowed
/// `OpEntry` is fine for app logic but fights this seam; take the cast
/// once at the function boundary and treat the rest as a flat read.
///
/// We can't write `OpEntry & Partial<PocketOp & ProfileOp & …>` because
/// the variants' `kind` literal types intersect to `never` and collapse
/// the whole type. Spelling the merged shape explicitly keeps `kind`
/// usable as the discriminator while every other variant-specific
/// field reads as optional.
interface FlatOp extends OpBase, ContourFields {
  kind: OpKind;
  // ProfileOp / EngraveOp / DragKnifeOp
  offset?: ProfileOffset;
  // PocketOp
  pocketStrategy?: PocketOp['pocketStrategy'];
  xyOverlap?: number;
  engagementAngleDeg?: number;
  loopRadiusFactor?: number;
  halfpipeProfile?: PocketOp['halfpipeProfile'];
  finishToolId?: number;
  finishXyAllowanceMm?: number;
  frameShape?: PocketOp['frameShape'];
  framePaddingMm?: number;
  frameCornerRadiusMm?: number;
  // DrillOp
  drillCycle?: DrillOp['drillCycle'];
  chamferAfterWidthMm?: number;
  spotFirst?: DrillOp['spotFirst'];
  // ChamferOp
  chamferWidthMm?: ChamferOp['chamferWidthMm'];
  chamferFinishPass?: ChamferOp['chamferFinishPass'];
  // VCarveOp
  carveMaxWidthMm?: VCarveOp['carveMaxWidthMm'];
  multiPassRefine?: VCarveOp['multiPassRefine'];
  fullMedialAxis?: VCarveOp['fullMedialAxis'];
  sourceInsetMm?: VCarveOp['sourceInsetMm'];
  // PauseOp
  message?: string;
  // 8n4k: HomingOp
  retractToSafeZ?: boolean;
  // 8n4k: ProbeOp
  axis?: 'x' | 'y' | 'z';
  distanceMm?: number;
  feedMmMin?: number;
  // 8n4k: CycleMarkerOp
  label?: string;
  // rxm9: GcodeIncludeOp
  path?: string;
  content?: string;
  // xi2g: GcodeIncludeOp verbose-warning flag
  verboseUnsimWarnings?: boolean;
  // PocketOp (rt1.9)
  pocketZigzagAngleDeg?: number;
  // ThreadOp
  threadPitchMm?: ThreadOp['threadPitchMm'];
  threadInternal?: ThreadOp['threadInternal'];
  threadClimb?: ThreadOp['threadClimb'];
  // ReliefMillOp (f60x)
  sourceId?: ReliefMillOp['sourceId'];
  zMinMm?: ReliefMillOp['zMinMm'];
  zMaxMm?: ReliefMillOp['zMaxMm'];
  invert?: ReliefMillOp['invert'];
  scallopHeightMm?: ReliefMillOp['scallopHeightMm'];
  stepoverMm?: ReliefMillOp['stepoverMm'];
  scanDirection?: ReliefMillOp['scanDirection'];
  alongStepMm?: ReliefMillOp['alongStepMm'];
  // RasterEngraveOp (rt1.12) — sourceId / scanDirection shared above.
  resolutionMm?: RasterEngraveOp['resolutionMm'];
  powerCurve?: RasterEngraveOp['powerCurve'];
  link?: RasterEngraveOp['link'];
  overscanFactor?: RasterEngraveOp['overscanFactor'];
}

export type WireToolKind = Schemas['ToolKind'];

/// Frontend ToolKind → wire (Rust ToolKind variant) value. The German
/// `kegel` wire name lives on for backend compatibility; the frontend
/// uses `cone` everywhere else (8njb). Apply this at EVERY seam where a
/// tool kind crosses into Rust — build-project's `buildTool` AND the
/// WASM sim driver, the two we know of. A regression test sits next to
/// each call site; if you add a third seam, add a third test.
export function toWireToolKind(kind: import('../state/op_types').ToolKind): WireToolKind {
  return kind === 'cone' ? 'kegel' : kind;
}

/// Wire-side holder shape. Mirrors `ivac_core::project::HolderShape`'s
/// `#[serde(tag = "kind")]` discriminator.
export type WireHolderShape = Schemas['HolderShape'];

type WireToolEntry = Schemas['ToolEntry'];

type WireMachine = Schemas['MachineConfig'];

type WireDrillCycle = Schemas['DrillCycle'];

/// kbx5 step 3: per-kind params live inside the kind discriminator,
/// not on the parent `WireOp.params` bag. The shape mirrors
/// `crate::project::OpKind` 1:1.

type WireOpKind = Schemas['OpKind'];

/// rt1.12: wire form of `PowerCurve` — tagged on `kind` with snake_case
/// variant names. Identical to the app `PowerCurve` except bayer's
/// `matrix_size` (app: `matrixSize`); see `buildPowerCurve`.
type WirePowerCurve = Schemas['PowerCurve'];

type WireSourceCombine = Schemas['SourceCombine'];
type WireSource = Schemas['OpSource'];

/// kbx5 step 3: `WireOp.params` is universal-only (depth schedule,
/// plunge, feed overrides). Every other field — tabs, leads, cut
/// direction, pocket flags, frame, vcarve cap, drill chamfer-after,
/// pattern — moved to its appropriate `WireOpKind` variant struct.
type WireOp = Schemas['Op'];

/// Fixture wire shape mirrors `ivac_core::project::FixtureKind` (snake_case
/// `shape` discriminator). Vertices for `polygon` are origin-relative, the
/// other shapes carry their dims directly.
export type WireFixtureKind = Schemas['FixtureKind'];

export type WireFixture = Schemas['Fixture'];

type WireTextLayer = Schemas['TextLayer'];

/// i5g4: wire shape for `ivac_core::project::WorkOffset`. Every field
/// is serde-`skip_serializing_if = is_zero / is_default`, so we always
/// omit the field entirely when at default (zero offset + G54) rather
/// than emit `{x_mm:0, y_mm:0, ...}` — keeps payloads small and
/// matches what the Rust side serializes.
export type WireWorkOffset = Schemas['WorkOffset'];

export type WireProject = Schemas['Project'];

/// f60x: wire shape of `ivac_core::project::ReliefSource`. `origin` is the
/// min-corner Point2 `{x, y}`; `brightness` is row-major normalized [0, 1].
export type WireReliefSource = Schemas['ReliefSource'];

/// vrrr: wire shape of `ivac_core::project::StockConfig` — an
/// axis-aligned stock box resolved in the geometry frame. `origin` is the
/// min corner (x, y); the body spans z ∈ [top_z_mm − thickness_mm, top_z_mm].
export type WireStock = Schemas['StockConfig'];

interface ProjectStateView {
  /// File-transform-applied combined view of every import (wrsu Phase 2).
  /// The wire payload always sends this so the pipeline sees the same
  /// geometry the user sees on the canvas.
  transformedImport: ImportResponse | null;
  /// 8jce: `transformedImport` plus the synthetic selectable stock
  /// outline (when stock is shown). Preferred for the wire payload so an
  /// op can cut the workpiece edge. Optional — falls back to
  /// `transformedImport` (so existing tests / callers without it work).
  geometryView?: ImportResponse | null;
  machine: MachineSettings;
  tools: FrontToolEntry[];
  operations: OpEntry[];
  fixtures?: WireFixture[];
  textLayers?: TextLayer[];
  /// i5g4 / j4tv: program-level WCS offset. Forwarded to the pipeline so
  /// the sim can align its heightmap to the WCS frame.
  workOffset?: WorkOffset;
  /// vrrr: stock UI config. Resolved against `transformedImport` (NOT the
  /// stock-augmented `geometryView` — that would balloon the auto-mode
  /// bbox around the stock outline itself) + the machine work area into
  /// the wire `stock` box. Optional so legacy callers / tests that don't
  /// model stock simply omit the `out_of_stock` scan.
  stock?: StockConfig;
  /// f60x: relief surface sources forwarded so `relief_mill` ops can
  /// resolve their target surface. Optional.
  reliefSources?: ReliefSource[];
  /// l8lk: opt-in tool-change-order optimization (group ops by tool).
  /// Optional — defaults to declared order.
  groupOpsByTool?: boolean;
}

function buildTextLayer(layer: TextLayer): WireTextLayer {
  return {
    id: layer.id,
    kind: layer.kind,
    name: layer.name,
    text: layer.text,
    // dya2: font rides as the base64 string the source already holds, not a
    // decoded integer array.
    font_bytes: layer.fontSource.bytes_b64,
    size_mm: layer.sizeMm,
    origin: [layer.origin.x, layer.origin.y],
    rotation_deg: layer.rotationDeg,
    letter_spacing_mm: layer.letterSpacingMm,
    line_spacing_mm: layer.lineSpacingMm,
    alignment: layer.alignment,
    width_scale: layer.widthScale,
  };
}

function buildMachine(m: MachineSettings): WireMachine {
  return {
    unit: m.unit,
    mode: m.mode,
    comments: m.comments,
    arcs: m.arcs,
    tool_change: m.toolchangeStrategy,
    ...(m.accel ? { accel: { x: m.accel.x, y: m.accel.y, z: m.accel.z } } : {}),
    ...(m.jerk ? { jerk: { x: m.jerk.x, y: m.jerk.y, z: m.jerk.z } } : {}),
    ...(m.toolchangeS !== undefined && m.toolchangeS !== 5 ? { toolchange_s: m.toolchangeS } : {}),
    ...(m.rapidSpeed !== undefined ? { rapid_speed: m.rapidSpeed } : {}),
    ...(m.arcFitToleranceMm !== undefined ? { arc_fit_tolerance_mm: m.arcFitToleranceMm } : {}),
    ...(m.decimalSeparator === ',' ? { decimal_separator: ',' as const } : {}),
    ...(m.lineNumberStart !== undefined && m.lineNumberStart > 0
      ? { line_number_start: m.lineNumberStart }
      : {}),
    ...(m.plotModeZ ? { plot_mode_z: true } : {}),
    // The frontend PostProfile model types several string fields as
    // optional that the schema marks required (Rust serde-defaults them);
    // the shape is otherwise identical, so cast at this single boundary.
    ...(m.postProfile ? { post_profile: m.postProfile as Schemas['PostProfile'] } : {}),
    ...(m.workArea ? { work_area: { x: m.workArea.x, y: m.workArea.y, z: m.workArea.z } } : {}),
    // 3nnj: spindle RPM clamps. Skip on undefined so the Rust serde
    // default (no clamp) applies; zero is a meaningful explicit setting
    // (everything clamps up to 0 → effectively disabled spindle) but we
    // pass it through verbatim because the user typed it.
    ...(m.spindleRpmMin !== undefined ? { spindle_rpm_min: m.spindleRpmMin } : {}),
    ...(m.spindleRpmMax !== undefined ? { spindle_rpm_max: m.spindleRpmMax } : {}),
    ...(m.maxFeedMmMin !== undefined ? { max_feed_mm_min: m.maxFeedMmMin } : {}),
    // Spindle warmup / spindown dwells. Skip on undefined so the
    // Rust 0.5 s default applies.
    ...(m.spindleStartDwellSec !== undefined
      ? { spindle_start_dwell_sec: m.spindleStartDwellSec }
      : {}),
    ...(m.spindleStopDwellSec !== undefined
      ? { spindle_stop_dwell_sec: m.spindleStopDwellSec }
      : {}),
    // syol: park-at-home flag. Skip on the default (false) to keep the
    // wire compact.
    ...(m.parkAtHome ? { park_at_home: true } : {}),
    // syol: explicit park XY. Per the audit spec we only emit this when
    // `parkAtHome === false` — when parkAtHome is true the G53 path
    // already pins the head to machine home, so an explicit WCS XY
    // would be ambiguous (and the Rust side ignores it anyway).
    ...(!m.parkAtHome && m.parkXy ? { park_xy: m.parkXy } : {}),
    // 4lq5: optional-stop (M1) flag. Skip on the default (false).
    ...(m.optionalStop ? { optional_stop: true } : {}),
    // z9zh: GRBL dynamic-power (M4) laser mode. Skip on the default.
    ...(m.laserDynamicPower ? { laser_dynamic_power: true } : {}),
  };
}

function buildTool(t: FrontToolEntry): WireToolEntry {
  return {
    id: t.id,
    name: t.name,
    // The cone tool kind keeps its German wire value `kegel` (the Rust
    // ToolKind variant); only the frontend identifier is English. Apply
    // via `toWireToolKind` so every wire seam shares the mapping.
    kind: toWireToolKind(t.kind),
    diameter: t.diameter,
    ...(t.tipDiameter !== undefined ? { tip_diameter: t.tipDiameter } : {}),
    // tip_angle_deg is required by the schema (Rust serde-defaults it to
    // 60, which schemars marks required), so emit it explicitly; 60
    // round-trips identically to omitting it.
    tip_angle_deg: t.tipAngleDeg ?? 60,
    ...(t.dragoff !== undefined ? { dragoff: t.dragoff } : {}),
    ...(t.dragKnifeSelfAlignAngleDeg !== undefined
      ? { drag_knife_self_align_angle_deg: t.dragKnifeSelfAlignAngleDeg }
      : {}),
    flutes: t.flutes,
    speed: t.speed,
    plunge_rate: t.plungeRate,
    feed_rate: t.feedRate,
    coolant: t.coolant,
    ...(t.speedFinish !== undefined ? { speed_finish: t.speedFinish } : {}),
    ...(t.plungeRateFinish !== undefined ? { plunge_rate_finish: t.plungeRateFinish } : {}),
    ...(t.feedRateFinish !== undefined ? { feed_rate_finish: t.feedRateFinish } : {}),
    ...(t.speedDrill !== undefined ? { speed_drill: t.speedDrill } : {}),
    ...(t.plungeRateDrill !== undefined ? { plunge_rate_drill: t.plungeRateDrill } : {}),
    ...(t.feedRateDrill !== undefined ? { feed_rate_drill: t.feedRateDrill } : {}),
    ...(t.defaultPeckStepMm !== undefined ? { default_peck_step_mm: t.defaultPeckStepMm } : {}),
    ...(t.zShiftMm !== undefined && t.zShiftMm !== 0 ? { z_shift_mm: t.zShiftMm } : {}),
    ...(t.laserPierceSec !== undefined && t.laserPierceSec > 0
      ? { laser_pierce_sec: t.laserPierceSec }
      : {}),
    ...(t.laserLeadInMm !== undefined && t.laserLeadInMm > 0
      ? { laser_lead_in_mm: t.laserLeadInMm }
      : {}),
    // sm59: plasma pierce/cut-height entry sequence.
    ...(t.pierceHeightMm !== undefined ? { pierce_height_mm: t.pierceHeightMm } : {}),
    ...(t.cutHeightMm !== undefined ? { cut_height_mm: t.cutHeightMm } : {}),
    ...(t.pierceDelaySec !== undefined ? { pierce_delay_sec: t.pierceDelaySec } : {}),
    ...(t.cornerRadiusMm !== undefined && t.cornerRadiusMm > 0
      ? { corner_radius_mm: t.cornerRadiusMm }
      : {}),
    // 1wit: form-profile samples. Emit only ≥2 rows (a single sample
    // isn't an interpolation domain — the sim falls back to its taper).
    ...(t.kind === 'form_profile' && t.formProfileMm !== undefined && t.formProfileMm.length >= 2
      ? { form_profile_mm: t.formProfileMm.map((s) => ({ z_mm: s.zMm, r_mm: s.rMm })) }
      : {}),
    // Whirling overlay (ob3e: wire fields are now English `whirl*`,
    // matching the renamed Rust serde fields).
    ...(t.whirl ? { whirl: true } : {}),
    ...(t.whirlStepoverMm !== undefined && t.whirlStepoverMm > 0
      ? { whirl_stepover_mm: t.whirlStepoverMm }
      : {}),
    ...(t.whirlExtraWidthMm !== undefined && t.whirlExtraWidthMm > 0
      ? { whirl_extra_width_mm: t.whirlExtraWidthMm }
      : {}),
    ...(t.whirlOscMm !== undefined && t.whirlOscMm > 0 ? { whirl_osc_mm: t.whirlOscMm } : {}),
    // Spindle warmup pause (seconds). Omit when at backend default
    // (1) so we don't bloat the wire payload for the common case.
    ...(t.pause !== undefined && t.pause !== 1 ? { pause: t.pause } : {}),
    ...(t.defaultStep !== undefined ? { default_step: t.defaultStep } : {}),
    ...(t.defaultXyOverlap !== undefined ? { default_xy_overlap: t.defaultXyOverlap } : {}),
    ...(t.comment !== undefined && t.comment !== '' ? { comment: t.comment } : {}),
    ...(t.fluteLengthMm !== undefined ? { flute_length_mm: t.fluteLengthMm } : {}),
    ...(t.lengthMm !== undefined ? { length_mm: t.lengthMm } : {}),
    ...(t.compressionTransitionMm !== undefined
      ? { compression_transition_mm: t.compressionTransitionMm }
      : {}),
    ...(t.threadPitchMm !== undefined ? { thread_pitch_mm: t.threadPitchMm } : {}),
    ...(t.shankDiameterMm !== undefined ? { shank_diameter_mm: t.shankDiameterMm } : {}),
    // q0kc: stickout (mm). Skip on zero so the wire payload stays
    // compact for the legacy "collet sits on flutes" common case.
    ...(t.stickoutLengthMm !== undefined && t.stickoutLengthMm > 0
      ? { stickout_length_mm: t.stickoutLengthMm }
      : {}),
    // mmu8: laser kerf width (mm). Skip on zero/undefined so the Rust
    // sim falls back to its 0.15 mm default.
    ...(t.kerfMm !== undefined && t.kerfMm > 0 ? { kerf_mm: t.kerfMm } : {}),
    // z1y0: spindle direction. Skip the default ('cw') to keep the wire
    // compact.
    ...(t.spindleDirection === 'ccw' ? { spindle_direction: 'ccw' as const } : {}),
    ...(t.holder !== undefined ? { holder: t.holder } : {}),
  };
}

/// Build the `contour` sub-object embedded in every closed-contour
/// kind (Profile / Pocket / Engrave / DragKnife). kbx5 step 3: tabs /
/// leads / cut direction / approach point all live on the kind now.
function buildContourParams(op: FlatOp): Record<string, unknown> {
  const c: Record<string, unknown> = {};
  if (
    op.tabsActive ||
    op.tabWidth !== undefined ||
    op.tabHeight !== undefined ||
    op.tabType !== undefined ||
    op.tabRampAngleDeg !== undefined
  ) {
    const tabs: Record<string, unknown> = {
      active: op.tabsActive ?? false,
      width: op.tabWidth ?? 10,
      height: op.tabHeight ?? 1,
      tab_type: op.tabType ?? 'rectangle',
    };
    if (op.tabType === 'ramp' && op.tabRampAngleDeg !== undefined && op.tabRampAngleDeg !== 30) {
      tabs.ramp_angle_deg = op.tabRampAngleDeg;
    }
    c.tabs = tabs;
  }
  if (op.tabMode && op.tabMode.kind !== 'off') {
    // 0euf: the FE op model is camelCase (autoCount); the wire stays
    // snake_case. Convert here so the boundary is the network call.
    c.tab_mode =
      op.tabMode.kind === 'mixed'
        ? { kind: 'mixed', auto_count: op.tabMode.autoCount }
        : op.tabMode;
  }
  if (op.tabPlacements && op.tabPlacements.length > 0) {
    c.tab_placements = op.tabPlacements.map((p) => ({
      object_id: p.objectId,
      t: p.t,
      ...(p.widthOverrideMm !== undefined ? { width_override_mm: p.widthOverrideMm } : {}),
      ...(p.heightOverrideMm !== undefined ? { height_override_mm: p.heightOverrideMm } : {}),
    }));
  }
  if (op.leadInKind || op.leadOutKind || op.leadIn !== undefined || op.leadOut !== undefined) {
    c.leads = {
      in: op.leadInKind ?? 'off',
      out: op.leadOutKind ?? 'off',
      in_lenght: op.leadIn ?? 5,
      out_lenght: op.leadOut ?? 5,
    };
  }
  if (op.cutDirection && op.cutDirection !== 'conventional') {
    c.cut_direction = op.cutDirection;
  }
  if (op.finishCutDirection && op.finishCutDirection !== 'conventional') {
    c.finish_cut_direction = op.finishCutDirection;
  }
  if (op.cornerFeedReduction !== undefined && op.cornerFeedReduction > 0) {
    c.corner_feed_reduction = op.cornerFeedReduction;
  }
  if (op.approachPoint !== undefined) {
    c.approach_point = op.approachPoint;
  }
  return c;
}

/// Pocket-only fields: xy overlap, pocket flags, finish allowance, the
/// Pocket-Outside frame triple.
function buildPocketParams(op: FlatOp): Record<string, unknown> {
  const p: Record<string, unknown> = {};
  if (op.xyOverlap !== undefined && op.xyOverlap > 0) p.xy_overlap = op.xyOverlap;
  // Selection-driven islands: when the user picks outer + inner closed
  // contours together, the inner ones become islands automatically. The
  // wire flag matters only for legacy `source = All` flows.
  p.pocket_islands = true;
  if (op.finishXyAllowanceMm !== undefined && op.finishXyAllowanceMm > 0) {
    p.finish_xy_allowance_mm = op.finishXyAllowanceMm;
  }
  if (op.frameShape !== undefined) p.frame_shape = op.frameShape;
  if (op.framePaddingMm !== undefined) p.frame_padding_mm = op.framePaddingMm;
  if (op.frameCornerRadiusMm !== undefined) p.frame_corner_radius_mm = op.frameCornerRadiusMm;
  return p;
}

/// VCarve-only fields.
function buildVCarveParams(op: FlatOp): Record<string, unknown> {
  const v: Record<string, unknown> = {};
  if (op.carveMaxWidthMm !== undefined && op.carveMaxWidthMm > 0) {
    v.carve_max_width_mm = op.carveMaxWidthMm;
  }
  if (op.multiPassRefine) v.multi_pass_refine = true;
  if (op.fullMedialAxis) v.full_medial_axis = true;
  if (op.sourceInsetMm !== undefined && op.sourceInsetMm > 0) {
    v.source_inset_mm = op.sourceInsetMm;
  }
  return v;
}

function buildOpKind(opIn: OpEntry): WireOpKind {
  const op = opIn as FlatOp;
  switch (opIn.kind) {
    case 'profile':
      return {
        type: 'profile',
        offset: op.offset,
        contour: buildContourParams(op),
        // ProfileParams (overcut / reverse / helix) — emit at default.
        profile: {},
      } as WireOpKind;
    case 'pocket': {
      const strategy = op.pocketStrategy ?? 'cascade';
      const contour = buildContourParams(op);
      const pocket = buildPocketParams(op);
      if (strategy === 'zigzag' && op.pocketZigzagAngleDeg && op.pocketZigzagAngleDeg !== 0) {
        return {
          type: 'pocket',
          strategy: { kind: 'zigzag', angle_deg: op.pocketZigzagAngleDeg },
          contour,
          pocket,
        } as WireOpKind;
      }
      if (strategy === 'trochoidal') {
        return {
          type: 'pocket',
          strategy: {
            kind: 'trochoidal',
            engagement_angle_deg: op.engagementAngleDeg ?? 30,
            loop_radius_factor: op.loopRadiusFactor ?? 0.6,
          },
          contour,
          pocket,
        } as WireOpKind;
      }
      if (strategy === 'halfpipe') {
        const fe = op.halfpipeProfile ?? { kind: 'circular_arc' as const, radiusMm: 5 };
        const profile =
          fe.kind === 'circular_arc'
            ? { kind: 'circular_arc', radius_mm: fe.radiusMm }
            : { kind: 'v_bottom', included_angle_deg: fe.includedAngleDeg };
        return {
          type: 'pocket',
          strategy: {
            kind: 'halfpipe',
            profile,
          },
          contour,
          pocket,
        } as WireOpKind;
      }
      return { type: 'pocket', strategy, contour, pocket } as WireOpKind;
    }
    case 'drill': {
      const cycle: WireDrillCycle = op.drillCycle
        ? mapDrillCycle(op.drillCycle)
        : { kind: 'simple', dwell_sec: 0 };
      const drill: Record<string, unknown> = { type: 'drill', cycle };
      if (op.chamferAfterWidthMm !== undefined && op.chamferAfterWidthMm > 0) {
        drill.chamfer_after_width_mm = op.chamferAfterWidthMm;
      }
      // Pattern repetition (kbx5: Drill-only now). 0euf: FE model is
      // camelCase; convert to the snake_case wire shape here.
      if (op.pattern && (op.pattern as { kind?: string }).kind) {
        const p = op.pattern;
        drill.pattern =
          p.kind === 'grid'
            ? { kind: 'grid', count_x: p.countX, count_y: p.countY, dx: p.dx, dy: p.dy }
            : p.kind === 'polar'
              ? {
                  kind: 'polar',
                  count: p.count,
                  center_x: p.centerX,
                  center_y: p.centerY,
                  angle_step_deg: p.angleStepDeg,
                  ...(p.startAngleDeg !== undefined ? { start_angle_deg: p.startAngleDeg } : {}),
                }
              : p;
      }
      // r2af / u64o: spot-drill pre-pass.
      if (op.spotFirst && op.spotFirst.spotToolId > 0) {
        drill.spot_first = {
          spot_depth_mm: op.spotFirst.spotDepthMm,
          spot_tool_id: op.spotFirst.spotToolId,
        };
      }
      return drill as WireOpKind;
    }
    case 'vcarve':
      return { type: 'v_carve', carve: buildVCarveParams(op) } as WireOpKind;
    case 'engrave':
      return { type: 'engrave', contour: buildContourParams(op) } as WireOpKind;
    case 'drag_knife':
      return { type: 'drag_knife', contour: buildContourParams(op) } as WireOpKind;
    case 't_slot':
      return { type: 't_slot', contour: buildContourParams(op) } as WireOpKind;
    case 'dovetail':
      return { type: 'dovetail', contour: buildContourParams(op) } as WireOpKind;
    case 'chamfer':
      return {
        type: 'chamfer',
        ...(op.chamferWidthMm !== undefined && op.chamferWidthMm > 0
          ? { width_mm: op.chamferWidthMm }
          : {}),
        ...(op.chamferFinishPass ? { finish_pass: true } : {}),
      } as WireOpKind;
    case 'thread':
      return {
        type: 'thread',
        ...(op.threadPitchMm !== undefined && op.threadPitchMm > 0
          ? { pitch_mm: op.threadPitchMm }
          : {}),
        ...(op.threadInternal === false ? { internal: false } : {}),
        ...(op.threadClimb ? { climb: true } : {}),
      } as WireOpKind;
    case 'pause':
      return { type: 'pause', message: op.message ?? '' } as WireOpKind;
    case 'homing':
      return {
        type: 'homing',
        retract_to_safe_z: op.retractToSafeZ ?? true,
      } as WireOpKind;
    case 'probe':
      return {
        type: 'probe',
        axis: op.axis ?? 'z',
        distance_mm: op.distanceMm ?? -10,
        feed_mm_min: op.feedMmMin ?? 100,
      } as WireOpKind;
    case 'cycle_marker':
      return {
        type: 'cycle_marker',
        label: op.label ?? '',
      } as WireOpKind;
    case 'gcode_include':
      return {
        type: 'gcode_include',
        path: op.path ?? '',
        content: op.content ?? '',
        verbose_unsim_warnings: op.verboseUnsimWarnings ?? false,
      } as WireOpKind;
    case 'relief_mill':
      return {
        type: 'relief_mill',
        source_id: op.sourceId ?? 0,
        z_min_mm: op.zMinMm ?? -2,
        z_max_mm: op.zMaxMm ?? 0,
        invert: op.invert ?? false,
        scallop_height_mm: op.scallopHeightMm ?? 0.05,
        ...(op.stepoverMm != null && op.stepoverMm > 0 ? { stepover_mm: op.stepoverMm } : {}),
        scan_direction: op.scanDirection ?? 'along_x',
        along_step_mm: op.alongStepMm ?? 0.5,
      } as WireOpKind;
    case 'raster_engrave':
      return {
        type: 'raster_engrave',
        source_id: op.sourceId ?? 0,
        resolution_mm: op.resolutionMm ?? 0.1,
        power_curve: buildPowerCurve(op.powerCurve),
        scan_direction: op.scanDirection ?? 'along_x',
        link: op.link ?? 'lift_between',
        overscan_factor: op.overscanFactor ?? 0,
      } as WireOpKind;
  }
}

/// rt1.12: map an app `PowerCurve` to its wire shape. The only
/// translation is bayer's `matrixSize` → `matrix_size`; every other
/// variant passes through 1:1. `undefined` (a legacy save without the
/// field) falls back to the GRBL-default linear ramp.
function buildPowerCurve(c: PowerCurve | undefined): WirePowerCurve {
  if (c == null) return { kind: 'linear', min: 0, max: 1000 };
  switch (c.kind) {
    case 'linear':
      return { kind: 'linear', min: c.min, max: c.max };
    case 'threshold':
      return { kind: 'threshold', level: c.level, power: c.power };
    case 'floyd_steinberg':
      return { kind: 'floyd_steinberg', level: c.level, power: c.power };
    case 'bayer':
      return { kind: 'bayer', matrix_size: c.matrixSize, power: c.power };
  }
}

function mapDrillCycle(c: DrillOp['drillCycle']): WireDrillCycle {
  // dwell_sec is required by the schema (the Rust field has a serde
  // default of 0, which schemars still marks required), so emit it
  // explicitly rather than omitting — 0 round-trips identically.
  switch (c.kind) {
    case 'simple':
      return { kind: 'simple', dwell_sec: c.dwellSec ?? 0 };
    case 'peck':
      return { kind: 'peck', peck_step_mm: c.peckStepMm, dwell_sec: c.dwellSec ?? 0 };
    case 'chip_break':
      return { kind: 'chip_break', peck_step_mm: c.peckStepMm, dwell_sec: c.dwellSec ?? 0 };
  }
}

function buildSource(op: OpEntry): WireSource {
  // Only attach a `combine` field when the user picked something other
  // than the default — keeps wire payloads small and lets the Rust side
  // fall back to SourceCombine::Auto via serde default.
  const combine: WireSourceCombine | undefined =
    op.sourceCombine && op.sourceCombine !== 'auto'
      ? (op.sourceCombine as WireSourceCombine)
      : undefined;
  if (op.sourceObjects && op.sourceObjects.length > 0) {
    return { kind: 'objects', ids: op.sourceObjects, ...(combine ? { combine } : {}) };
  }
  if (op.sourceLayers === null || op.sourceLayers.length === 0) return { kind: 'all' };
  return { kind: 'layers', layers: op.sourceLayers, ...(combine ? { combine } : {}) };
}

function buildOp(opIn: OpEntry, machine: MachineSettings): WireOp {
  // `op` reads the flat-permissive view of every variant's optional
  // fields without per-kind narrowing; `opIn` keeps the narrow union
  // for helpers that dispatch on `kind`. Per-kind fields (tabs, leads,
  // xy_overlap, etc.) flow into the kind discriminator's nested
  // `contour` / `pocket` / `vcarve` / drill `pattern` sub-objects via
  // `buildOpKind`. `params` carries only the universal depth-schedule +
  // overrides bag (kbx5 step 3 cleanup).
  const op = opIn as FlatOp;
  return {
    id: opIn.id,
    name: opIn.name,
    enabled: opIn.enabled,
    kind: buildOpKind(opIn),
    tool_id: opIn.toolId,
    ...(op.finishToolId !== undefined && op.finishToolId !== op.toolId
      ? { finish_tool_id: op.finishToolId }
      : {}),
    source: buildSource(opIn),
    params: {
      // 4dxb: program-only ops (Pause, Homing, Probe, CycleMarker,
      // GcodeInclude) construct without `depth` / `startDepth` —
      // they have no meaningful depth schedule. Emit explicit 0
      // fallbacks so the wire payload is well-formed even for
      // older Rust binaries that pre-date the corresponding
      // `#[serde(default)]` annotation on `OpParamsCommon`. The
      // pipeline ignores params for program-only ops anyway.
      depth: op.depth ?? 0,
      start_depth: op.startDepth ?? 0,
      ...(op.step !== null && op.step !== undefined ? { step: op.step } : {}),
      fast_move_z: machine.fastMoveZ,
      objectorder: 'nearest',
      ...(op.plunge && op.plunge.kind === 'ramp'
        ? { plunge: { kind: 'ramp', angle_deg: op.plunge.angleDeg } }
        : op.plunge && op.plunge.kind === 'helix'
          ? {
              plunge: {
                kind: 'helix',
                angle_deg: op.plunge.angleDeg,
                radius_mm: op.plunge.radiusMm,
              },
            }
          : {}),
      ...(op.feedRateOverride !== undefined && op.feedRateOverride > 0
        ? { feed_rate_override: op.feedRateOverride }
        : {}),
      ...(op.plungeRateOverride !== undefined && op.plungeRateOverride > 0
        ? { plunge_rate_override: op.plungeRateOverride }
        : {}),
      ...(op.finishStep !== undefined && op.finishStep !== 0 ? { finish_step: op.finishStep } : {}),
      ...(op.throughDepth !== undefined && op.throughDepth > 0
        ? { through_depth: op.throughDepth }
        : {}),
      ...(op.depthList && op.depthList.length > 0 ? { depth_list: op.depthList } : {}),
    },
    // dp6b: emit `group` only when set and non-empty. The Rust serde
    // tag is `skip_serializing_if = "Option::is_none"`, so omit it
    // entirely when unset.
    ...(op.group && op.group.length > 0 ? { group: op.group } : {}),
    // l8lk: emit pin_order only when set (Rust skips the default false).
    ...(op.pinOrder ? { pin_order: true } : {}),
  };
}

/// i5g4 / j4tv: convert the FE WorkOffset into its wire shape. Emits
/// only the non-default scalars + a `wcs` field when not at G54 — the
/// Rust serde derive uses `skip_serializing_if = is_zero_f64` /
/// `Wcs::is_default` so this mirrors the canonical Rust payload.
/// Returns null when the offset is fully at default; the caller drops
/// the whole `work_offset` key in that case.
function buildWorkOffset(w: WorkOffset): WireWorkOffset | null {
  if (isDefaultWorkOffset(w)) return null;
  const out: WireWorkOffset = {};
  if (w.x_mm !== 0) out.x_mm = w.x_mm;
  if (w.y_mm !== 0) out.y_mm = w.y_mm;
  if (w.z_mm !== 0) out.z_mm = w.z_mm;
  if (w.wcs !== 'G54') out.wcs = w.wcs;
  return out;
}

/// vrrr: resolve the frontend stock UI config into the wire stock box.
/// Mirrors the old `GenerateBar.boundsScan` footprint exactly — resolved
/// against `transformedImport` (the raw geometry, NOT the stock-augmented
/// `geometryView`) + the machine work area, with the same
/// `max(0.01, thickness)` floor. Returns null when no stock is modeled so
/// the caller omits the key (the core then skips the `out_of_stock` scan).
function buildStock(state: ProjectStateView): WireStock | null {
  const stock = state.stock;
  if (!stock) return null;
  const fp = computeFootprint(state.transformedImport, stock, state.machine.workArea);
  return {
    origin: [fp.minX, fp.minY],
    width_mm: fp.maxX - fp.minX,
    height_mm: fp.maxY - fp.minY,
    thickness_mm: Math.max(0.01, stock.thickness),
    // ya00: stock-top Z placement. Omit at the default (0) to keep the
    // wire compact.
    ...(stock.offsetZ ? { top_z_mm: stock.offsetZ } : {}),
  };
}

/// f60x: map a frontend ReliefSource to its wire shape (origin object →
/// `[x, y]` tuple; everything else passes through).
function buildReliefSource(rs: ReliefSource): WireReliefSource {
  return {
    id: rs.id,
    name: rs.name,
    // Schema-correct: ReliefSource.origin is a Point2 ({x,y}). The prior
    // hand wire type declared a [x,y] tuple, which only round-tripped
    // because serde accepts a struct-from-sequence — aligning to the
    // generated Point2 shape surfaced + fixes that drift.
    origin: { x: rs.origin.x, y: rs.origin.y },
    cell: rs.cell,
    cols: rs.cols,
    rows: rs.rows,
    brightness: rs.brightness,
  };
}

/// Construct the wire `project` field for PipelineRequest. Returns null
/// if the frontend has no operations defined yet — caller should fall
/// back to the legacy segments+setup path.
export function buildProject(state: ProjectStateView): WireProject | null {
  if (state.operations.length === 0) return null;
  // 8jce: prefer the augmented view (imports + selectable stock outline)
  // so an op targeting the stock edge is cut. Falls back to the raw
  // import for callers/tests that don't supply geometryView.
  const imp = state.geometryView ?? state.transformedImport;
  if (!imp) return null;
  const workOffset = state.workOffset ? buildWorkOffset(state.workOffset) : null;
  const stock = buildStock(state);
  return {
    segments: imp.segments,
    machine: buildMachine(state.machine),
    tools: state.tools.map(buildTool),
    operations: state.operations.map((op) => buildOp(op, state.machine)),
    ...(state.fixtures && state.fixtures.length > 0 ? { fixtures: state.fixtures } : {}),
    ...(state.textLayers && state.textLayers.length > 0
      ? { text_layers: state.textLayers.map(buildTextLayer) }
      : {}),
    ...(workOffset ? { work_offset: workOffset } : {}),
    ...(stock ? { stock } : {}),
    ...(state.reliefSources && state.reliefSources.length > 0
      ? { relief_sources: state.reliefSources.map(buildReliefSource) }
      : {}),
    ...(state.groupOpsByTool ? { group_ops_by_tool: true } : {}),
  };
}

/// Type alias for callers who want the GenerateRequest with the new
/// project field as an opaque object (the schema generator hasn't
/// added it to the typed wire shape yet).
export type GenerateRequestWithProject = GenerateRequest & {
  project?: WireProject;
};
