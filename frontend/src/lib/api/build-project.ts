// Adapter from the frontend ProjectState shape to the wire Project the
// wiac_core pipeline consumes. Camel-case → snake-case, and the
// kind-specific Operation params get materialized from the per-op
// entry's flat fields.

import type {
  ToolEntry as FrontToolEntry,
  OpEntry,
  MachineSettings,
} from '../state/project.svelte';
import type { GenerateRequest, ImportResponse } from './types';

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
  default_step?: number;
  flute_length_mm?: number;
  shank_diameter_mm?: number;
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
  };
}

type WireDrillCycle =
  | { kind: 'simple'; dwell_sec?: number }
  | { kind: 'peck'; peck_step_mm: number; dwell_sec?: number }
  | { kind: 'chip_break'; peck_step_mm: number; dwell_sec?: number };

type WirePocketStrategy =
  | 'cascade'
  | 'zigzag'
  | 'spiral'
  | { kind: 'trochoidal'; engagement_angle_deg: number; loop_radius_factor: number }
  | {
      kind: 'halfpipe';
      profile:
        | { kind: 'circular_arc'; radius_mm: number }
        | { kind: 'v_bottom'; included_angle_deg: number };
    };

type WireOpKind =
  | { type: 'profile'; offset: 'outside' | 'inside' | 'on' | 'none' }
  | { type: 'pocket'; strategy: WirePocketStrategy }
  | { type: 'drill'; cycle: WireDrillCycle }
  | { type: 'thread'; pitch_mm?: number; internal?: boolean; climb?: boolean }
  | { type: 'chamfer'; width_mm?: number; finish_pass?: boolean }
  | { type: 'engrave' }
  | { type: 'drag_knife' }
  | { type: 'helix' }
  | { type: 'v_carve' };

type WireSourceCombine =
  | 'auto'
  | 'union'
  | 'difference'
  | 'intersection'
  | 'xor'
  | 'none';
type WireSource =
  | { kind: 'all' }
  | { kind: 'layers'; layers: string[]; combine?: WireSourceCombine }
  | { kind: 'objects'; ids: number[]; combine?: WireSourceCombine };

interface WireOp {
  id: number;
  name: string;
  enabled: boolean;
  kind: WireOpKind;
  tool_id: number;
  finish_tool_id?: number;
  /// Optional UI grouping label (rt1.21). Preserved through save/load
  /// so reopening a project restores the user's group layout.
  group?: string;
  source: WireSource;
  params: {
    depth: number;
    start_depth: number;
    step?: number;
    fast_move_z: number;
    helix: boolean;
    reverse: boolean;
    objectorder: 'nearest' | 'per_object' | 'unordered';
    overcut: boolean;
    pocket_islands: boolean;
    pocket_nocontour: boolean;
    pocket_insideout: boolean;
    tabs: {
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
    leads: { in: 'off' | 'straight' | 'arc'; out: 'off' | 'straight' | 'arc'; in_lenght: number; out_lenght: number };
    cut_direction?: 'conventional' | 'climb';
    finish_cut_direction?: 'conventional' | 'climb';
    plunge?:
      | { kind: 'direct' }
      | { kind: 'ramp'; angle_deg: number }
      | { kind: 'helix'; angle_deg: number; radius_mm: number | null };
    xy_overlap?: number;
    feed_rate_override?: number;
    plunge_rate_override?: number;
    corner_feed_reduction?: number;
    finish_step?: number;
    finish_xy_allowance_mm?: number;
    chamfer_after_width_mm?: number;
    approach_point?: [number, number];
    through_depth?: number;
    depth_list?: number[];
    /// V-Carve: optional cap on the inscribed-circle radius. None = no
    /// cap. When set, the V-bit doesn't carve any wider than this even
    /// where the medial axis would call for it.
    carve_max_width_mm?: number;
    /// V-Carve: when true, run a refinement pass that re-cuts the
    /// points whose first pass was depth-limited. Off by default.
    multi_pass_refine?: boolean;
    /// Pocket-Outside (rt1.3): when set, the pipeline auto-prepends a
    /// synthetic frame VcObject around the op's selection and runs
    /// SourceCombine::Difference. The frame is computed at generate time
    /// from these params — not persisted as project geometry.
    frame_shape?: 'rectangle' | 'rounded_rectangle';
    frame_padding_mm?: number;
    frame_corner_radius_mm?: number;
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

export interface WireProject {
  segments: ImportResponse['segments'];
  machine: WireMachine;
  tools: WireToolEntry[];
  operations: WireOp[];
  fixtures?: WireFixture[];
}

interface ProjectStateView {
  imported: ImportResponse | null;
  machine: MachineSettings;
  tools: FrontToolEntry[];
  operations: OpEntry[];
  fixtures?: WireFixture[];
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
    ...(t.defaultStep !== undefined ? { default_step: t.defaultStep } : {}),
    ...(t.fluteLengthMm !== undefined ? { flute_length_mm: t.fluteLengthMm } : {}),
    ...(t.shankDiameterMm !== undefined ? { shank_diameter_mm: t.shankDiameterMm } : {}),
    ...(t.holder !== undefined ? { holder: t.holder } : {}),
  };
}

function buildOpKind(op: OpEntry): WireOpKind {
  switch (op.kind) {
    case 'profile':
      return { type: 'profile', offset: op.offset };
    case 'pocket': {
      const strategy = op.pocketStrategy ?? 'cascade';
      if (strategy === 'trochoidal') {
        return {
          type: 'pocket',
          strategy: {
            kind: 'trochoidal',
            engagement_angle_deg: op.engagementAngleDeg ?? 30,
            loop_radius_factor: op.loopRadiusFactor ?? 0.6,
          },
        };
      }
      if (strategy === 'halfpipe') {
        const profile = op.halfpipeProfile ?? { kind: 'circular_arc' as const, radius_mm: 5 };
        return {
          type: 'pocket',
          strategy: {
            kind: 'halfpipe',
            profile,
          },
        };
      }
      return { type: 'pocket', strategy };
    }
    case 'drill': {
      const cycle: WireDrillCycle = op.drillCycle
        ? mapDrillCycle(op.drillCycle)
        : { kind: 'simple', dwell_sec: 0 };
      return { type: 'drill', cycle };
    }
    case 'vcarve':
      return { type: 'v_carve' };
    case 'chamfer':
      return {
        type: 'chamfer',
        ...(op.chamferWidthMm !== undefined && op.chamferWidthMm > 0
          ? { width_mm: op.chamferWidthMm }
          : {}),
        ...(op.chamferFinishPass ? { finish_pass: true } : {}),
      };
    case 'thread':
      return {
        type: 'thread',
        ...(op.threadPitchMm !== undefined && op.threadPitchMm > 0
          ? { pitch_mm: op.threadPitchMm }
          : {}),
        ...(op.threadInternal === false ? { internal: false } : {}),
        ...(op.threadClimb ? { climb: true } : {}),
      };
    default:
      return { type: op.kind } as WireOpKind;
  }
}

function mapDrillCycle(c: NonNullable<OpEntry['drillCycle']>): WireDrillCycle {
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

function buildOp(op: OpEntry, machine: MachineSettings): WireOp {
  return {
    id: op.id,
    name: op.name,
    enabled: op.enabled,
    kind: buildOpKind(op),
    tool_id: op.toolId,
    ...(op.finishToolId !== undefined && op.finishToolId !== op.toolId
      ? { finish_tool_id: op.finishToolId }
      : {}),
    ...(op.group ? { group: op.group } : {}),
    source: buildSource(op),
    params: {
      depth: op.depth,
      start_depth: op.startDepth,
      ...(op.step !== null && op.step !== undefined ? { step: op.step } : {}),
      fast_move_z: machine.fastMoveZ,
      helix: false,
      reverse: false,
      objectorder: 'nearest',
      overcut: false,
      // Pocket islands are now driven by the selection itself: when the
      // user picks an outer + inner closed contour, the inner one is
      // automatically treated as an island (see pipeline.rs's
      // selected_set logic). The wire flag stays at its default and only
      // matters for legacy `source = All` flows.
      pocket_islands: true,
      pocket_nocontour: false,
      pocket_insideout: false,
      tabs: {
        active: op.tabsActive ?? false,
        width: op.tabWidth ?? 10,
        height: op.tabHeight ?? 1,
        tab_type: op.tabType ?? 'rectangle',
        // Only emit ramp_angle_deg when ≠ default (30°) so old payloads
        // and the Rust serde default match — the field is
        // skip_serializing_if=is_default_ramp_angle on the wire too.
        ...(op.tabType === 'ramp' && op.tabRampAngleDeg !== undefined && op.tabRampAngleDeg !== 30
          ? { ramp_angle_deg: op.tabRampAngleDeg }
          : {}),
      },
      ...(op.tabMode && op.tabMode.kind !== 'off' ? { tab_mode: op.tabMode } : {}),
      ...(op.tabPlacements && op.tabPlacements.length > 0
        ? {
            tab_placements: op.tabPlacements.map((p) => ({
              object_id: p.objectId,
              t: p.t,
              ...(p.widthOverrideMm !== undefined
                ? { width_override_mm: p.widthOverrideMm }
                : {}),
              ...(p.heightOverrideMm !== undefined
                ? { height_override_mm: p.heightOverrideMm }
                : {}),
            })),
          }
        : {}),
      leads: {
        in: op.leadInKind ?? 'off',
        out: op.leadOutKind ?? 'off',
        in_lenght: op.leadIn ?? 5,
        out_lenght: op.leadOut ?? 5,
      },
      // Only emit when ≠ conventional so the wire stays small and the
      // Rust side falls back to the serde default.
      ...(op.cutDirection && op.cutDirection !== 'conventional'
        ? { cut_direction: op.cutDirection }
        : {}),
      ...(op.finishCutDirection && op.finishCutDirection !== 'conventional'
        ? { finish_cut_direction: op.finishCutDirection }
        : {}),
      ...(op.plunge && op.plunge.kind !== 'direct'
        ? { plunge: op.plunge }
        : {}),
      // Only emit xy_overlap when set; the Rust default kicks in on 0.
      ...(op.xyOverlap !== undefined && op.xyOverlap > 0
        ? { xy_overlap: op.xyOverlap }
        : {}),
      ...(op.feedRateOverride !== undefined && op.feedRateOverride > 0
        ? { feed_rate_override: op.feedRateOverride }
        : {}),
      ...(op.plungeRateOverride !== undefined && op.plungeRateOverride > 0
        ? { plunge_rate_override: op.plungeRateOverride }
        : {}),
      ...(op.cornerFeedReduction !== undefined && op.cornerFeedReduction > 0
        ? { corner_feed_reduction: op.cornerFeedReduction }
        : {}),
      ...(op.finishStep !== undefined && op.finishStep !== 0
        ? { finish_step: op.finishStep }
        : {}),
      ...(op.finishXyAllowanceMm !== undefined && op.finishXyAllowanceMm > 0
        ? { finish_xy_allowance_mm: op.finishXyAllowanceMm }
        : {}),
      ...(op.chamferAfterWidthMm !== undefined && op.chamferAfterWidthMm > 0
        ? { chamfer_after_width_mm: op.chamferAfterWidthMm }
        : {}),
      ...(op.approachPoint !== undefined
        ? { approach_point: op.approachPoint }
        : {}),
      ...(op.throughDepth !== undefined && op.throughDepth > 0
        ? { through_depth: op.throughDepth }
        : {}),
      ...(op.depthList && op.depthList.length > 0
        ? { depth_list: op.depthList }
        : {}),
      ...(op.carveMaxWidthMm !== undefined && op.carveMaxWidthMm > 0
        ? { carve_max_width_mm: op.carveMaxWidthMm }
        : {}),
      ...(op.multiPassRefine ? { multi_pass_refine: true } : {}),
      ...(op.frameShape !== undefined ? { frame_shape: op.frameShape } : {}),
      ...(op.framePaddingMm !== undefined
        ? { frame_padding_mm: op.framePaddingMm }
        : {}),
      ...(op.frameCornerRadiusMm !== undefined
        ? { frame_corner_radius_mm: op.frameCornerRadiusMm }
        : {}),
    },
  };
}

/// Construct the wire `project` field for PipelineRequest. Returns null
/// if the frontend has no operations defined yet — caller should fall
/// back to the legacy segments+setup path.
export function buildProject(state: ProjectStateView): WireProject | null {
  if (state.operations.length === 0) return null;
  if (!state.imported) return null;
  return {
    segments: state.imported.segments,
    machine: buildMachine(state.machine),
    tools: state.tools.map(buildTool),
    operations: state.operations.map((op) => buildOp(op, state.machine)),
    ...(state.fixtures && state.fixtures.length > 0 ? { fixtures: state.fixtures } : {}),
  };
}

/// Type alias for callers who want the GenerateRequest with the new
/// project field as an opaque object (the schema generator hasn't
/// added it to the typed wire shape yet).
export type GenerateRequestWithProject = GenerateRequest & {
  project?: WireProject;
};
