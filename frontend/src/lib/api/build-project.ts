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
  dragoff?: number;
  flutes: number;
  speed: number;
  plunge_rate: number;
  feed_rate: number;
  coolant: 'off' | 'mist' | 'flood';
}

interface WireMachine {
  unit: 'mm' | 'inch';
  mode: 'mill' | 'laser' | 'drag';
  comments: boolean;
  arcs: boolean;
  supports_toolchange: boolean;
}

type WireOpKind =
  | { type: 'profile'; offset: 'outside' | 'inside' | 'on' | 'none' }
  | { type: 'pocket'; strategy: 'cascade' | 'zigzag' | 'spiral' }
  | { type: 'drill' }
  | { type: 'thread' }
  | { type: 'chamfer' }
  | { type: 'engrave' }
  | { type: 'drag_knife' }
  | { type: 'helix' };

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
    step: number;
    fast_move_z: number;
    helix: boolean;
    reverse: boolean;
    objectorder: 'nearest' | 'per_object' | 'unordered';
    overcut: boolean;
    pocket_islands: boolean;
    pocket_nocontour: boolean;
    pocket_insideout: boolean;
    tabs: { active: boolean; width: number; height: number; tab_type: 'rectangle' | 'ramp' };
    leads: { in: 'off' | 'straight' | 'arc'; out: 'off' | 'straight' | 'arc'; in_lenght: number; out_lenght: number };
    cut_direction?: 'conventional' | 'climb';
    finish_cut_direction?: 'conventional' | 'climb';
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
    ...(t.dragoff !== undefined ? { dragoff: t.dragoff } : {}),
    flutes: t.flutes,
    speed: t.speed,
    plunge_rate: t.plungeRate,
    feed_rate: t.feedRate,
    coolant: t.coolant,
  };
}

function buildOpKind(op: OpEntry): WireOpKind {
  switch (op.kind) {
    case 'profile':
      return { type: 'profile', offset: op.offset };
    case 'pocket':
      return { type: 'pocket', strategy: op.pocketStrategy ?? 'cascade' };
    default:
      return { type: op.kind } as WireOpKind;
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
      step: op.step,
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
      tabs: { active: anyTabs, width: 10, height: 1, tab_type: 'rectangle' },
      leads: { in: 'off', out: 'off', in_lenght: 5, out_lenght: 5 },
      // Only emit when ≠ conventional so the wire stays small and the
      // Rust side falls back to the serde default.
      ...(op.cutDirection && op.cutDirection !== 'conventional'
        ? { cut_direction: op.cutDirection }
        : {}),
      ...(op.finishCutDirection && op.finishCutDirection !== 'conventional'
        ? { finish_cut_direction: op.finishCutDirection }
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
