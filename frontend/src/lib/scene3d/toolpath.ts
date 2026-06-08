/// Generated toolpath wireframe + direction-arrow chevrons. Rebuilds on a
/// new pipeline run / op enable toggle / raster-source change / arrow
/// density change. The playhead fade + sim-warning tints mutate the color
/// attribute in place (applyFade) instead of rebuilding, so scrubbing the
/// playhead is O(delta) per tick — which also stops the camera resetting
/// mid-playback.
///
/// Extracted from Scene3D.svelte (4w2f). Owns its THREE.Group.

import * as THREE from 'three';
import { LineSegments2 } from 'three/addons/lines/LineSegments2.js';
import type { GenerateResponse, SimWarning } from '../api/types';
import type { OpEntry, ReliefSource } from '../state/project.svelte';
import {
  playheadToSegment,
  simWarningSegmentIdx,
  simWarningSeverity,
} from '../state/project.svelte';
import { opHue } from '../state/op-color';
import {
  computeArrowChevron,
  arrowSpacingMm,
  resolveSegmentColor,
  fadeColor,
} from './toolpath_buffers';
import { powerGrid, maxPower } from '../cam/raster_preview';
import { powerAtWorld, heatColor, type HeatGrid } from './raster_heatmap';
import { buildFatLines } from './fat_lines';
import type { BuilderContext, CssColor, LineOwner, PickableLineBuilder } from './builder';

export interface ToolpathInput {
  generated: GenerateResponse | null;
  operations: OpEntry[];
  reliefSources: ReliefSource[];
  /// settings.toolMoveArrowDensity — drives arrow spacing (0 disables).
  arrowDensity: number;
  lineWidth: number;
  width: number;
  height: number;
  wireVisible: boolean;
  /// Playhead + arc-length tables, so build() can re-apply the fade to the
  /// freshly-baked colors.
  playhead: number;
  cumLen: Float64Array | null;
  totalLen: number;
}

/// Per-segment baked color: `start` is the vertex index of the segment's
/// first vertex, `base` its un-faded color.
type ToolpathColor = { start: number; base: [number, number, number] };

export class ToolpathBuilder implements PickableLineBuilder {
  readonly group = new THREE.Group();
  private lines: LineSegments2 | undefined;
  private arrows: LineSegments2 | undefined;
  private owners: LineOwner[] = [];
  private colors: ToolpathColor[] = [];
  /// Head index the fade currently reflects.
  private appliedHead = -1;
  /// Per-segment override colors driven by sim warnings.
  private warningSegmentColors = new Map<number, [number, number, number]>();

  constructor(
    private ctx: BuilderContext,
    private cssColor: CssColor,
  ) {
    ctx.scene.add(this.group);
  }

  get pickable(): LineSegments2 | undefined {
    return this.lines;
  }
  get lineOwners(): LineOwner[] {
    return this.owners;
  }

  private teardown() {
    for (const o of [this.lines, this.arrows]) {
      if (!o) continue;
      this.group.remove(o);
      o.geometry.dispose();
      (o.material as THREE.Material).dispose();
    }
    this.lines = undefined;
    this.arrows = undefined;
  }

  build(input: ToolpathInput) {
    this.teardown();
    this.owners = [];
    this.colors = [];
    this.appliedHead = -1;
    const gen = input.generated;
    if (!gen) return;

    const positions: number[] = [];
    const colors: number[] = [];
    // Direction-arrow geometry — separate buffer so it doesn't interfere
    // with the playhead-fade range math on the main toolpath buffer.
    const arrowPositions: number[] = [];
    const arrowColors: number[] = [];
    // 7iej.17: chevron geometry lives in toolpath_buffers.ts (pure +
    // unit-tested); the buffer assembly + spacing bookkeeping stay here.
    const ARROW_PARAMS = {
      minLen: 1.0, // mm; shorter segments never get an arrow
      maxSize: 4.0, // mm; absolute cap on arrow size
      sizeFrac: 0.2, // arrow size relative to segment length
      halfWing: Math.tan((30 * Math.PI) / 180), // ±30° wings
    };
    // Arrow spacing is user-tunable (Settings → arrow density): higher
    // density packs arrows closer. density 0 ⇒ Infinity spacing ⇒ no
    // segment ever qualifies, so arrows are disabled.
    const ARROW_MIN_SPACING = arrowSpacingMm(input.arrowDensity);
    let lenSinceLastArrow = ARROW_MIN_SPACING; // emit on first qualifying segment
    const moveTints: Record<string, THREE.Color> = {
      rapid: this.cssColor('--toolpath-rapid', 0x35a2ff),
      cut: this.cssColor('--toolpath-cut', 0xff5555),
      plunge: this.cssColor('--toolpath-plunge', 0xffd23a),
      retract: this.cssColor('--toolpath-retract', 0x5fd06e),
      arc: this.cssColor('--toolpath-arc', 0xff8a3a),
    };
    // Per-op enable filter: disabling an op via OperationsList hides its
    // segments without forcing a re-Generate.
    const disabledOpIds = new Set<number>();
    for (const o of input.operations) {
      if (!o.enabled) disabledOpIds.add(o.id);
    }

    // rt1.12 (nrob): per-raster-op power grids for the toolpath heatmap.
    // The wire toolpath carries no `S`, so re-derive it from the source
    // brightness through the same power curve the backend emits from, then
    // colour each cut span by the power at its midpoint.
    const rasterHeat = new Map<number, { grid: HeatGrid; powers: number[]; peak: number }>();
    for (const o of input.operations) {
      if (o.kind !== 'raster_engrave' || !o.enabled) continue;
      const src = input.reliefSources.find((s) => s.id === o.sourceId);
      if (!src || src.cols <= 0 || src.rows <= 0) continue;
      const powers = powerGrid(o.powerCurve, src.brightness, src.cols, src.rows);
      if (powers.length === 0) continue;
      rasterHeat.set(o.id, {
        grid: {
          originX: src.origin.x,
          originY: src.origin.y,
          cell: src.cell,
          cols: src.cols,
          rows: src.rows,
        },
        powers,
        peak: Math.max(1, maxPower(o.powerCurve)),
      });
    }
    // Dithered curves emit ~one span per pixel, so a large engrave can run
    // to millions of segments. Downsample the heat spans to a fixed budget
    // (~10k) by striding — the fat-line buffer stays bounded and the
    // heatmap still reads. Non-raster moves are never dropped.
    const RASTER_HEAT_BUDGET = 10000;
    let rasterHeatTotal = 0;
    if (rasterHeat.size > 0) {
      for (let i = 0; i < gen.toolpath.length; i++) {
        const s = gen.toolpath[i];
        const oid = s.op_id ?? 0;
        if (oid > 0 && disabledOpIds.has(oid)) continue;
        if (rasterHeat.has(oid) && (s.kind === 'cut' || s.kind === 'arc')) rasterHeatTotal++;
      }
    }
    const rasterStride =
      rasterHeatTotal > RASTER_HEAT_BUDGET ? Math.ceil(rasterHeatTotal / RASTER_HEAT_BUDGET) : 1;
    let rasterCutSeen = 0;

    const total = gen.toolpath.length;
    for (let i = 0; i < total; i++) {
      const seg = gen.toolpath[i];
      const opId = seg.op_id ?? 0;
      if (opId > 0 && disabledOpIds.has(opId)) continue;

      // rt1.12 (nrob): raster-engrave cut spans get a power heatmap instead
      // of the op-hue colour. Travel moves (rapid) keep the normal tint.
      // Dense engraves stride down to the segment budget.
      const heat = rasterHeat.get(opId);
      const isHeat = heat != null && (seg.kind === 'cut' || seg.kind === 'arc');
      if (isHeat) {
        const keep = rasterCutSeen % rasterStride === 0;
        rasterCutSeen++;
        if (!keep) continue;
      }

      let r: number;
      let g: number;
      let b: number;
      if (isHeat && heat) {
        const mx = (seg.from.x + seg.to.x) * 0.5;
        const my = (seg.from.y + seg.to.y) * 0.5;
        const p = powerAtWorld(mx, my, heat.grid, heat.powers);
        const t = p == null ? 0 : Math.min(1, p / heat.peak);
        [r, g, b] = heatColor(t);
      } else {
        const moveTint = moveTints[seg.kind] ?? moveTints.cut;
        const opHueV = opId === 0 ? 0.0 : opHue(opId);
        const opCol = new THREE.Color().setHSL(opHueV, 0.55, 0.5);
        // 7iej.20: THREE/theme resolution stays here; the op_id-0 vs
        // boosted-hue channel math lives in the pure resolveSegmentColor.
        [r, g, b] = resolveSegmentColor(
          opId,
          seg.kind,
          [moveTint.r, moveTint.g, moveTint.b],
          [opCol.r, opCol.g, opCol.b],
        );
      }
      const startVertex = positions.length / 3;
      positions.push(seg.from.x, seg.from.y, seg.from.z, seg.to.x, seg.to.y, seg.to.z);
      colors.push(r, g, b, r, g, b);
      this.owners.push({ kind: 'toolpath', segIdx: i });
      this.colors.push({ start: startVertex, base: [r, g, b] });

      // Direction-arrow chevron at the segment midpoint when qualifying.
      // Rapids skip — feed direction matters only for material-cutting
      // moves. The cumulative-spacing guard prevents arrow noise on dense
      // raster pockets.
      const dx = seg.to.x - seg.from.x;
      const dy = seg.to.y - seg.from.y;
      const dz = seg.to.z - seg.from.z;
      const len = Math.sqrt(dx * dx + dy * dy + dz * dz);
      if (len > 0) lenSinceLastArrow += len;
      // Spacing + move-kind eligibility stays here (caller state); the
      // chevron geometry (incl. the per-segment minLen gate) is pure.
      // rt1.12 (nrob): no direction arrows on raster heat spans — they'd
      // swamp the heatmap and the scan direction is already obvious.
      const spacingOk = lenSinceLastArrow >= ARROW_MIN_SPACING && seg.kind !== 'rapid' && !isHeat;
      const chev = spacingOk ? computeArrowChevron(seg.from, seg.to, ARROW_PARAMS) : null;
      if (chev) {
        const { mid, wing1, wing2 } = chev;
        arrowPositions.push(mid[0], mid[1], mid[2], wing1[0], wing1[1], wing1[2]);
        arrowPositions.push(mid[0], mid[1], mid[2], wing2[0], wing2[1], wing2[2]);
        // Slight brightness boost so arrows pop on top of the base line.
        const ar = Math.min(1, r * 1.25);
        const ag = Math.min(1, g * 1.25);
        const ab = Math.min(1, b * 1.25);
        arrowColors.push(ar, ag, ab, ar, ag, ab, ar, ag, ab, ar, ag, ab);
        lenSinceLastArrow = 0;
      }
    }

    if (positions.length > 0) {
      this.lines = buildFatLines(positions, colors, input.lineWidth, input.width, input.height);
      this.lines.visible = input.wireVisible;
      this.group.add(this.lines);
    }
    if (arrowPositions.length > 0) {
      this.arrows = buildFatLines(
        arrowPositions,
        arrowColors,
        input.lineWidth,
        input.width,
        input.height,
      );
      this.arrows.visible = input.wireVisible;
      // Render after the base line so the chevron sits on top.
      this.arrows.renderOrder = 1;
      this.group.add(this.arrows);
    }
    // Re-apply the past/future fade to the freshly-baked colors so the
    // playhead tint is correct even when no playhead change triggered this
    // rebuild.
    this.applyFade(input.playhead, input.cumLen, input.totalLen);
  }

  /// Rebuild the sim-warning tint map (segIdx → severity color), consumed
  /// by applyFade. Resets the applied head so the next fade repaints every
  /// segment (warnings can affect any past/future segment).
  setWarnings(warnings: SimWarning[]) {
    this.warningSegmentColors = new Map();
    for (const w of warnings) {
      const idx = simWarningSegmentIdx(w);
      const sev = simWarningSeverity(w);
      // Critical wins over warning if both fired on the same segment.
      const existing = this.warningSegmentColors.get(idx);
      if (existing && sev !== 'critical') continue;
      const tint: [number, number, number] =
        sev === 'critical' ? [0.9, 0.28, 0.28] : [0.94, 0.75, 0.13];
      this.warningSegmentColors.set(idx, tint);
    }
    this.appliedHead = -1;
  }

  /// Mutate the color attribute in place to reflect the current playhead —
  /// segments before the head get their base color, after get faded. Walks
  /// only the slice between the applied head and the new head, so 60fps
  /// playback is O(playhead delta) per tick.
  applyFade(playhead: number, cumLen: Float64Array | null, totalLen: number) {
    if (!this.lines || this.colors.length === 0) return;
    const total = this.colors.length;
    // Arc-length mapping: head = segIdx + 1 so the segment currently under
    // the cutter (segIdx) renders as "past" (fully colored) and everything
    // strictly after is faded.
    const { segIdx } = playheadToSegment(playhead, cumLen, totalLen);
    const head =
      segIdx < 0
        ? Math.max(0, Math.min(total, Math.round(playhead * total)))
        : Math.max(0, Math.min(total, segIdx + 1));
    if (head === this.appliedHead) return;
    // LineSegmentsGeometry stores colors as one interleaved instance buffer
    // (6 floats / segment: start-rgb, end-rgb), so the `start * 3` offset
    // math indexes it identically to the old flat color array.
    const colorAttr = this.lines.geometry.getAttribute(
      'instanceColorStart',
    ) as THREE.InterleavedBufferAttribute;
    const arr = colorAttr.array as Float32Array;
    const f = 0.25; // fade factor for future moves
    const fade_offset = 0.05;
    const lo = this.appliedHead < 0 ? 0 : Math.min(this.appliedHead, head);
    const hi = this.appliedHead < 0 ? total : Math.max(this.appliedHead, head);
    for (let i = lo; i < hi; i++) {
      const tc = this.colors[i];
      const past = i < head;
      // 7iej.20: a warning-tinted segment fades from its tint, else from its
      // base color; the past/future offset math is the pure fadeColor.
      const tint = this.warningSegmentColors.get(i);
      const [r, g, b] = fadeColor(tint ?? tc.base, past, f, fade_offset);
      const off = tc.start * 3;
      arr[off] = r;
      arr[off + 1] = g;
      arr[off + 2] = b;
      arr[off + 3] = r;
      arr[off + 4] = g;
      arr[off + 5] = b;
    }
    colorAttr.data.needsUpdate = true;
    this.appliedHead = head;
  }

  boundingSphere(): THREE.Sphere | null {
    if (!this.lines) return null;
    this.lines.geometry.computeBoundingSphere();
    return this.lines.geometry.boundingSphere?.clone() ?? null;
  }

  setLineWidth(lw: number) {
    for (const o of [this.lines, this.arrows]) {
      const m = o?.material as LineMaterialLike | undefined;
      if (m) m.linewidth = lw;
    }
  }

  setResolution(w: number, h: number) {
    for (const o of [this.lines, this.arrows]) {
      (o?.material as LineMaterialLike | undefined)?.resolution.set(w, h);
    }
  }

  setWireVisible(visible: boolean) {
    if (this.lines) this.lines.visible = visible;
    if (this.arrows) this.arrows.visible = visible;
  }

  dispose() {
    this.teardown();
    this.ctx.scene.remove(this.group);
  }
}

/// The fat-line material exposes linewidth + a resolution Vector2; typed
/// structurally so we don't pull the LineMaterial addon type in here.
type LineMaterialLike = { linewidth: number; resolution: { set(w: number, h: number): void } };
