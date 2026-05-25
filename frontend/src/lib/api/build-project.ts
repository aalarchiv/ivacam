// Adapter from the frontend ProjectState shape to the wire Project the
// wiac_core pipeline consumes. Camel-case → snake-case, and the
// kind-specific Operation params get materialized from the per-op
// entry's flat fields.

import type {
  ToolEntry as FrontToolEntry,
  OpEntry,
  MachineSettings,
  TextAlignment,
  TextLayer,
  TextLayerKind,
  WorkOffset,
} from '../state/project.svelte';
import { isDefaultWorkOffset } from '../state/project-types';
import type {
  ChamferOp,
  ContourFields,
  DrillOp,
  OpBase,
  OpKind,
  PocketOp,
  ProfileOp,
  ProfileOffset,
  ThreadOp,
  VCarveOp,
} from '../state/op_types';
import type { GenerateRequest, ImportResponse } from './types';

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
  // PocketOp (rt1.9)
  pocketZigzagAngleDeg?: number;
  // ThreadOp
  threadPitchMm?: ThreadOp['threadPitchMm'];
  threadInternal?: ThreadOp['threadInternal'];
  threadClimb?: ThreadOp['threadClimb'];
}

type WireToolKind =
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

/// Wire-side holder shape. Mirrors `wiac_core::project::HolderShape`'s
/// `#[serde(tag = "kind")]` discriminator.
export type WireHolderShape =
  | { kind: 'cylinder'; diameter_mm: number; length_mm: number }
  | { kind: 'cone'; bottom_diameter_mm: number; top_diameter_mm: number; length_mm: number }
  | {
      kind: 'stepped';
      cylinder_diameter_mm: number;
      cylinder_length_mm: number;
      cone_top_diameter_mm: number;
      cone_length_mm: number;
    };

interface WireToolEntry {
  id: number;
  name: string;
  kind: WireToolKind;
  diameter: number;
  tip_diameter?: number;
  /// V-bit full apex angle in degrees. Required by V-Carve to compute
  /// `z = -R / tan(tip_angle / 2)`. Default 60 in the Rust struct.
  tip_angle_deg?: number;
  dragoff?: number;
  flutes: number;
  speed: number;
  plunge_rate: number;
  feed_rate: number;
  coolant: 'off' | 'mist' | 'flood';
  /// Per-pass rate overrides (rt1.27). Omit when unset so the Rust
  /// side falls back to the general triplet.
  speed_finish?: number;
  plunge_rate_finish?: number;
  feed_rate_finish?: number;
  speed_drill?: number;
  plunge_rate_drill?: number;
  feed_rate_drill?: number;
  default_peck_step_mm?: number;
  z_shift_mm?: number;
  laser_pierce_sec?: number;
  laser_lead_in_mm?: number;
  corner_radius_mm?: number;
  tslot_neck_diameter_mm?: number;
  tslot_neck_length_mm?: number;
  wirbeln?: boolean;
  wirbeln_stepover_mm?: number;
  wirbeln_extra_width_mm?: number;
  wirbeln_osc_mm?: number;
  /// Spindle warmup pause (seconds) emitted as G4 P<n> after every
  /// spindle_cw / spindle_ccw. Default 1.
  pause?: number;
  default_step?: number;
  default_xy_overlap?: number;
  comment?: string;
  flute_length_mm?: number;
  shank_diameter_mm?: number;
  /// q0kc: stickout between flute top and collet bottom (mm). Omit
  /// when 0 / undefined so the wire payload stays compact.
  stickout_length_mm?: number;
  /// mmu8: laser kerf width (mm). Honored only when kind === 'laser_beam'.
  /// Omit when undefined so the Rust sim falls back to its 0.15 mm
  /// default.
  kerf_mm?: number;
  /// z1y0: spindle direction. Omit when default ('cw') so legacy
  /// projects round-trip unchanged.
  spindle_direction?: 'cw' | 'ccw';
  holder?: WireHolderShape;
}

interface WireAxisLimits {
  x: number;
  y: number;
  z: number;
}

interface WireMachine {
  unit: 'mm' | 'inch';
  mode: 'mill' | 'laser' | 'drag';
  comments: boolean;
  arcs: boolean;
  supports_toolchange: boolean;
  accel?: WireAxisLimits;
  jerk?: WireAxisLimits;
  toolchange_s?: number;
  rapid_speed?: number;
  work_area?: WireAxisLimits;
  arc_fit_tolerance_mm?: number;
  decimal_separator?: '.' | ',';
  line_number_start?: number;
  plot_mode_z?: boolean;
  post_profile?: {
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
    // hev: per-axis output config. Must mirror
    // `wiac_core::gcode::post_profile::AxesConfig` so a refactor that
    // type-picks instead of spread-copies the post profile doesn't
    // silently drop the user's axis customisations.
    axes?: {
      x: WireAxisFormat;
      y: WireAxisFormat;
      z: WireAxisFormat;
      i: WireAxisFormat;
      j: WireAxisFormat;
      feed: WireAxisFormat;
      speed: WireAxisFormat;
    };
  };
  /// 3nnj: machine spindle clamps. Omit when undefined so the
  /// Rust serde default (no clamp) round-trips for legacy projects.
  spindle_rpm_min?: number;
  spindle_rpm_max?: number;
  /// Spindle warmup / spindown dwells (seconds) emitted around M6
  /// tool changes. Omit when undefined so the Rust 0.5 s defaults
  /// apply.
  spindle_start_dwell_sec?: number;
  spindle_stop_dwell_sec?: number;
  /// syol: park-at-home flag + optional explicit park XY (mm). When
  /// `park_at_home` is true the program_end footer emits
  /// `G53 G0 X0 Y0`; when false (and `park_xy` is None) it falls
  /// back to `G0 X0 Y0` in the current WCS. When `park_xy` is
  /// set the head routes to that explicit point instead. Omit
  /// when at default so legacy projects round-trip unchanged.
  park_at_home?: boolean;
  park_xy?: [number, number];
}

interface WireAxisFormat {
  enabled: boolean;
  name: string;
  format: string;
  scale: number;
}

type WireDrillCycle =
  | { kind: 'simple'; dwell_sec?: number }
  | { kind: 'peck'; peck_step_mm: number; dwell_sec?: number }
  | { kind: 'chip_break'; peck_step_mm: number; dwell_sec?: number };

type WirePocketStrategy =
  | 'cascade'
  | 'zigzag'
  | 'spiral'
  | { kind: 'zigzag'; angle_deg: number }
  | { kind: 'trochoidal'; engagement_angle_deg: number; loop_radius_factor: number }
  | {
      kind: 'halfpipe';
      profile:
        | { kind: 'circular_arc'; radius_mm: number }
        | { kind: 'v_bottom'; included_angle_deg: number };
    };

/// kbx5 step 3: per-kind params live inside the kind discriminator,
/// not on the parent `WireOp.params` bag. The shape mirrors
/// `crate::project::OpKind` 1:1.
interface WireContourParams {
  tabs?: {
    active: boolean;
    width: number;
    height: number;
    tab_type: 'rectangle' | 'ramp';
    ramp_angle_deg?: number;
  };
  tab_mode?:
    | { kind: 'off' }
    | { kind: 'auto'; count: number }
    | { kind: 'manual' }
    | { kind: 'mixed'; auto_count: number };
  tab_placements?: {
    object_id: number;
    t: number;
    width_override_mm?: number;
    height_override_mm?: number;
  }[];
  leads?: {
    in: 'off' | 'straight' | 'arc';
    out: 'off' | 'straight' | 'arc';
    in_lenght: number;
    out_lenght: number;
  };
  cut_direction?: 'conventional' | 'climb';
  finish_cut_direction?: 'conventional' | 'climb';
  corner_feed_reduction?: number;
  approach_point?: [number, number];
}

interface WirePocketParams {
  xy_overlap?: number;
  pocket_islands?: boolean;
  pocket_nocontour?: boolean;
  pocket_insideout?: boolean;
  finish_xy_allowance_mm?: number;
  frame_shape?: 'rectangle' | 'rounded_rectangle';
  frame_padding_mm?: number;
  frame_corner_radius_mm?: number;
}

interface WireProfileParams {
  overcut?: boolean;
  reverse?: boolean;
  helix?: boolean;
}

interface WireVCarveParams {
  carve_max_width_mm?: number;
  multi_pass_refine?: boolean;
}

interface WirePatternConfig {
  kind: 'linear' | 'grid' | 'polar';
  // narrow shape varies by kind; keep permissive on the wire side.
  [key: string]: unknown;
}

type WireOpKind =
  | {
      type: 'profile';
      offset: 'outside' | 'inside' | 'on' | 'none';
      contour: WireContourParams;
      profile: WireProfileParams;
    }
  | {
      type: 'pocket';
      strategy: WirePocketStrategy;
      contour: WireContourParams;
      pocket: WirePocketParams;
    }
  | {
      type: 'drill';
      cycle: WireDrillCycle;
      chamfer_after_width_mm?: number;
      pattern?: WirePatternConfig;
    }
  | { type: 'thread'; pitch_mm?: number; internal?: boolean; climb?: boolean }
  | { type: 'chamfer'; width_mm?: number; finish_pass?: boolean }
  | { type: 'engrave'; contour: WireContourParams }
  | { type: 'drag_knife'; contour: WireContourParams }
  | { type: 'helix' }
  | { type: 'v_carve'; carve: WireVCarveParams }
  | { type: 'pause'; message: string };

type WireSourceCombine = 'auto' | 'union' | 'difference' | 'intersection' | 'xor' | 'none';
type WireSource =
  | { kind: 'all' }
  | { kind: 'layers'; layers: string[]; combine?: WireSourceCombine }
  | { kind: 'objects'; ids: number[]; combine?: WireSourceCombine };

/// kbx5 step 3: `WireOp.params` is universal-only (depth schedule,
/// plunge, feed overrides). Every other field — tabs, leads, cut
/// direction, pocket flags, frame, vcarve cap, drill chamfer-after,
/// pattern — moved to its appropriate `WireOpKind` variant struct.
interface WireOp {
  id: number;
  name: string;
  enabled: boolean;
  kind: WireOpKind;
  tool_id: number;
  finish_tool_id?: number;
  source: WireSource;
  params: {
    depth: number;
    start_depth: number;
    step?: number;
    fast_move_z: number;
    objectorder: 'nearest' | 'per_object' | 'unordered';
    plunge?:
      | { kind: 'direct' }
      | { kind: 'ramp'; angle_deg: number }
      | { kind: 'helix'; angle_deg: number; radius_mm: number | null };
    feed_rate_override?: number;
    plunge_rate_override?: number;
    finish_step?: number;
    through_depth?: number;
    depth_list?: number[];
  };
}

/// Fixture wire shape mirrors `wiac_core::project::FixtureKind` (snake_case
/// `shape` discriminator). Vertices for `polygon` are origin-relative, the
/// other shapes carry their dims directly.
export type WireFixtureKind =
  | { shape: 'box'; width: number; depth: number }
  | { shape: 'cylinder'; radius: number }
  | { shape: 'polygon'; vertices: [number, number][] };

export interface WireFixture {
  id: number;
  name: string;
  kind: WireFixtureKind;
  origin: [number, number];
  z_bottom: number;
  z_top: number;
  color: number;
}

interface WireTextLayer {
  id: number;
  kind: TextLayerKind;
  name: string;
  text: string;
  /// TTF/OTF bytes encoded as a JSON array of integers — matches the
  /// existing render_text request shape.
  font_bytes: number[];
  size_mm: number;
  origin: [number, number];
  rotation_deg: number;
  letter_spacing_mm: number;
  line_spacing_mm: number;
  alignment: TextAlignment;
  width_scale: number;
}

/// i5g4: wire shape for `wiac_core::project::WorkOffset`. Every field
/// is serde-`skip_serializing_if = is_zero / is_default`, so we always
/// omit the field entirely when at default (zero offset + G54) rather
/// than emit `{x_mm:0, y_mm:0, ...}` — keeps payloads small and
/// matches what the Rust side serializes.
export interface WireWorkOffset {
  x_mm?: number;
  y_mm?: number;
  z_mm?: number;
  wcs?: 'G54' | 'G55' | 'G56' | 'G57' | 'G58' | 'G59';
}

export interface WireProject {
  segments: ImportResponse['segments'];
  machine: WireMachine;
  tools: WireToolEntry[];
  operations: WireOp[];
  fixtures?: WireFixture[];
  text_layers?: WireTextLayer[];
  /// i5g4: program-level WCS offset. Omitted when default (all-zero @
  /// G54) so the Rust serde-default round-trip matches.
  work_offset?: WireWorkOffset;
}

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
}

/// Base64 → byte array. Used for embedded TTF/OTF font payloads on the
/// pipeline request. `atob` returns each byte as a UTF-16 char code so
/// charCodeAt() yields the raw 0-255 value the JSON serializer expects.
function decodeFontBytes(b64: string): number[] {
  const binary = atob(b64);
  const out: number[] = new Array(binary.length);
  for (let i = 0; i < binary.length; i++) out[i] = binary.charCodeAt(i);
  return out;
}

function buildTextLayer(layer: TextLayer): WireTextLayer {
  return {
    id: layer.id,
    kind: layer.kind,
    name: layer.name,
    text: layer.text,
    font_bytes: decodeFontBytes(layer.fontSource.bytes_b64),
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
    supports_toolchange: m.supportsToolchange,
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
    ...(m.postProfile ? { post_profile: m.postProfile } : {}),
    ...(m.workArea ? { work_area: { x: m.workArea.x, y: m.workArea.y, z: m.workArea.z } } : {}),
    // 3nnj: spindle RPM clamps. Skip on undefined so the Rust serde
    // default (no clamp) applies; zero is a meaningful explicit setting
    // (everything clamps up to 0 → effectively disabled spindle) but we
    // pass it through verbatim because the user typed it.
    ...(m.spindleRpmMin !== undefined ? { spindle_rpm_min: m.spindleRpmMin } : {}),
    ...(m.spindleRpmMax !== undefined ? { spindle_rpm_max: m.spindleRpmMax } : {}),
    // Spindle warmup / spindown dwells. Skip on undefined so the
    // Rust 0.5 s default applies.
    ...(m.spindleStartDwellSec !== undefined
      ? { spindle_start_dwell_sec: m.spindleStartDwellSec }
      : {}),
    ...(m.spindleStopDwellSec !== undefined
      ? { spindle_stop_dwell_sec: m.spindleStopDwellSec }
      : {}),
    // syol: park-at-home flag. Skip on the default (false) so legacy
    // projects round-trip unchanged.
    ...(m.parkAtHome ? { park_at_home: true } : {}),
    // syol: explicit park XY. Per the audit spec we only emit this when
    // `parkAtHome === false` — when parkAtHome is true the G53 path
    // already pins the head to machine home, so an explicit WCS XY
    // would be ambiguous (and the Rust side ignores it anyway).
    ...(!m.parkAtHome && m.parkXy ? { park_xy: m.parkXy } : {}),
  };
}

function buildTool(t: FrontToolEntry): WireToolEntry {
  return {
    id: t.id,
    name: t.name,
    kind: t.kind,
    diameter: t.diameter,
    ...(t.tipDiameter !== undefined ? { tip_diameter: t.tipDiameter } : {}),
    ...(t.tipAngleDeg !== undefined ? { tip_angle_deg: t.tipAngleDeg } : {}),
    ...(t.dragoff !== undefined ? { dragoff: t.dragoff } : {}),
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
    ...(t.cornerRadiusMm !== undefined && t.cornerRadiusMm > 0
      ? { corner_radius_mm: t.cornerRadiusMm }
      : {}),
    ...(t.tslotNeckDiameterMm !== undefined && t.tslotNeckDiameterMm > 0
      ? { tslot_neck_diameter_mm: t.tslotNeckDiameterMm }
      : {}),
    ...(t.tslotNeckLengthMm !== undefined && t.tslotNeckLengthMm > 0
      ? { tslot_neck_length_mm: t.tslotNeckLengthMm }
      : {}),
    ...(t.wirbeln ? { wirbeln: true } : {}),
    ...(t.wirbelnStepoverMm !== undefined && t.wirbelnStepoverMm > 0
      ? { wirbeln_stepover_mm: t.wirbelnStepoverMm }
      : {}),
    ...(t.wirbelnExtraWidthMm !== undefined && t.wirbelnExtraWidthMm > 0
      ? { wirbeln_extra_width_mm: t.wirbelnExtraWidthMm }
      : {}),
    ...(t.wirbelnOscMm !== undefined && t.wirbelnOscMm > 0
      ? { wirbeln_osc_mm: t.wirbelnOscMm }
      : {}),
    // Spindle warmup pause (seconds). Omit when at backend default
    // (1) so we don't bloat the wire payload for the common case.
    ...(t.pause !== undefined && t.pause !== 1 ? { pause: t.pause } : {}),
    ...(t.defaultStep !== undefined ? { default_step: t.defaultStep } : {}),
    ...(t.defaultXyOverlap !== undefined
      ? { default_xy_overlap: t.defaultXyOverlap }
      : {}),
    ...(t.comment !== undefined && t.comment !== '' ? { comment: t.comment } : {}),
    ...(t.fluteLengthMm !== undefined ? { flute_length_mm: t.fluteLengthMm } : {}),
    ...(t.shankDiameterMm !== undefined ? { shank_diameter_mm: t.shankDiameterMm } : {}),
    // q0kc: stickout (mm). Skip on zero so the wire payload stays
    // compact for the legacy "collet sits on flutes" common case.
    ...(t.stickoutLengthMm !== undefined && t.stickoutLengthMm > 0
      ? { stickout_length_mm: t.stickoutLengthMm }
      : {}),
    // mmu8: laser kerf width (mm). Skip on zero/undefined so the Rust
    // sim falls back to its 0.15 mm default.
    ...(t.kerfMm !== undefined && t.kerfMm > 0 ? { kerf_mm: t.kerfMm } : {}),
    // z1y0: spindle direction. Skip default ('cw') so legacy projects
    // round-trip unchanged on the wire.
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
    c.tab_mode = op.tabMode;
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
        const profile = op.halfpipeProfile ?? { kind: 'circular_arc' as const, radius_mm: 5 };
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
      // Pattern repetition (kbx5: Drill-only now).
      if (op.pattern && (op.pattern as { kind?: string }).kind) {
        drill.pattern = op.pattern;
      }
      return drill as WireOpKind;
    }
    case 'vcarve':
      return { type: 'v_carve', carve: buildVCarveParams(op) } as WireOpKind;
    case 'engrave':
      return { type: 'engrave', contour: buildContourParams(op) } as WireOpKind;
    case 'drag_knife':
      return { type: 'drag_knife', contour: buildContourParams(op) } as WireOpKind;
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
  }
}

function mapDrillCycle(c: DrillOp['drillCycle']): WireDrillCycle {
  switch (c.kind) {
    case 'simple':
      return { kind: 'simple', ...(c.dwell_sec ? { dwell_sec: c.dwell_sec } : {}) };
    case 'peck':
      return {
        kind: 'peck',
        peck_step_mm: c.peck_step_mm,
        ...(c.dwell_sec ? { dwell_sec: c.dwell_sec } : {}),
      };
    case 'chip_break':
      return {
        kind: 'chip_break',
        peck_step_mm: c.peck_step_mm,
        ...(c.dwell_sec ? { dwell_sec: c.dwell_sec } : {}),
      };
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
      depth: op.depth,
      start_depth: op.startDepth,
      ...(op.step !== null && op.step !== undefined ? { step: op.step } : {}),
      fast_move_z: machine.fastMoveZ,
      objectorder: 'nearest',
      ...(op.plunge && op.plunge.kind !== 'direct' ? { plunge: op.plunge } : {}),
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
  };
}

/// Type alias for callers who want the GenerateRequest with the new
/// project field as an opaque object (the schema generator hasn't
/// added it to the typed wire shape yet).
export type GenerateRequestWithProject = GenerateRequest & {
  project?: WireProject;
};
