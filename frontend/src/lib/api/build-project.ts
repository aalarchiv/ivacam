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
  | 'laser_beam';

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
  default_step?: number;
}

interface WireMachine {
  unit: 'mm' | 'inch';
  mode: 'mill' | 'laser' | 'drag';
  comments: boolean;
  arcs: boolean;
  supports_toolchange: boolean;
}

type WireDrillCycle =
  | { kind: 'simple'; dwell_sec?: number }
  | { kind: 'peck'; peck_step_mm: number; dwell_sec?: number }
  | { kind: 'chip_break'; peck_step_mm: number; dwell_sec?: number };

type WirePocketStrategy =
  | 'cascade'
  | 'zigzag'
  | 'spiral'
  | { kind: 'trochoidal'; engagement_angle_deg: number; loop_radius_factor: number };

type WireOpKind =
  | { type: 'profile'; offset: 'outside' | 'inside' | 'on' | 'none' }
  | { type: 'pocket'; strategy: WirePocketStrategy }
  | { type: 'drill'; cycle: WireDrillCycle }
  | { type: 'thread' }
  | { type: 'chamfer' }
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

export interface WireProject {
  segments: ImportResponse['segments'];
  machine: WireMachine;
  tools: WireToolEntry[];
  operations: WireOp[];
  tabs: Record<number, { x: number; y: number }[]>;
}

interface ProjectStateView {
  imported: ImportResponse | null;
  machine: MachineSettings;
  tools: FrontToolEntry[];
  operations: OpEntry[];
  tabs: Record<number, { x: number; y: number }[]>;
}

function buildMachine(m: MachineSettings): WireMachine {
  return {
    unit: m.unit,
    mode: m.mode,
    comments: m.comments,
    arcs: m.arcs,
    supports_toolchange: m.supportsToolchange,
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
    ...(t.defaultStep !== undefined ? { default_step: t.defaultStep } : {}),
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

function buildOp(op: OpEntry, machine: MachineSettings, anyTabs: boolean): WireOp {
  return {
    id: op.id,
    name: op.name,
    enabled: op.enabled,
    kind: buildOpKind(op),
    tool_id: op.toolId,
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
        active: anyTabs,
        width: 10,
        height: 1,
        tab_type: op.tabType ?? 'rectangle',
        // Only emit ramp_angle_deg when ≠ default (30°) so old payloads
        // and the Rust serde default match — the field is
        // skip_serializing_if=is_default_ramp_angle on the wire too.
        ...(op.tabType === 'ramp' && op.tabRampAngleDeg !== undefined && op.tabRampAngleDeg !== 30
          ? { ramp_angle_deg: op.tabRampAngleDeg }
          : {}),
      },
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
  const anyTabs = Object.values(state.tabs).some((t) => t.length > 0);
  return {
    segments: state.imported.segments,
    machine: buildMachine(state.machine),
    tools: state.tools.map(buildTool),
    operations: state.operations.map((op) => buildOp(op, state.machine, anyTabs)),
    tabs: state.tabs,
  };
}

/// Type alias for callers who want the GenerateRequest with the new
/// project field as an opaque object (the schema generator hasn't
/// added it to the typed wire shape yet).
export type GenerateRequestWithProject = GenerateRequest & {
  tabs?: Record<number, { x: number; y: number }[]>;
  project?: WireProject;
};
