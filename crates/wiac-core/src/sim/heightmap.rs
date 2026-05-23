//! Heightmap Z(x,y) over the stock footprint, plus tool Z-profile functions.
//!
//! The heightmap is row-major `cols * rows` f32 cells. Mutations are tracked
//! through a dirty AABB so a downstream WASM bridge can upload only the
//! touched sub-rectangle to the WebGL mesh each frame.

// f64 ↔ u32 grid coordinate plumbing means a lot of intentional casts.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_lossless,
    clippy::cast_possible_wrap
)]

use crate::geometry::Point2;
use crate::project::{ToolEntry, ToolKind};

/// 2.5D heightmap covering the stock footprint. `data[iy * cols + ix]` is
/// the lowest Z value the cutter has reached over cell `(ix, iy)`.
#[derive(Debug, Clone)]
pub struct Heightmap {
    pub origin: Point2,
    pub cell: f64,
    pub cols: u32,
    pub rows: u32,
    pub top_z: f32,
    pub data: Vec<f32>,
    // Half-open dirty rectangle, in cell indices. None = no mutations
    // since the last clear_dirty().
    dirty: Option<(u32, u32, u32, u32)>,
}

impl Heightmap {
    #[must_use]
    pub fn new(origin: Point2, cell: f64, cols: u32, rows: u32, top_z: f32) -> Self {
        assert!(cell > 0.0, "Heightmap cell size must be > 0");
        assert!(cols > 0 && rows > 0, "Heightmap dimensions must be > 0");
        // eexa: WASM32 `usize` is `u32`, so the product `cols * rows`
        // wraps once it exceeds 2^32 — silently allocating a tiny vec
        // and corrupting every later index. Use `checked_mul` so the
        // overflow trips loudly even in release builds.
        let len = (cols as usize)
            .checked_mul(rows as usize)
            .expect("heightmap dim overflow");
        Self {
            origin,
            cell,
            cols,
            rows,
            top_z,
            data: vec![top_z; len],
            dirty: None,
        }
    }

    #[must_use]
    pub fn from_bbox(
        min_x: f64,
        min_y: f64,
        max_x: f64,
        max_y: f64,
        cell: f64,
        top_z: f32,
    ) -> Self {
        assert!(cell > 0.0, "Heightmap cell size must be > 0");
        assert!(
            max_x > min_x && max_y > min_y,
            "Heightmap bbox must be non-empty"
        );
        // vc6i: bilinear `sample()` returns `top_z` whenever the query
        // point's fractional cell index `fx = (x - origin)/cell - 0.5`
        // exceeds `cols - 1` — i.e. the upper half of the last column
        // is "off the grid" for sampling purposes. With a tight ceil()
        // sizing the bbox max-corner lands at `fx = cols - 0.5`, which
        // is outside that bound, so probes at the +x/+y stock edge
        // read `top_z` even after carving. Pad by one extra cell on
        // each axis so the max-corner lies safely inside the
        // sampleable region (cell-center grid math is unchanged; we
        // just guarantee at least a half-cell of slack past the
        // bbox max).
        let cols = (((max_x - min_x) / cell).ceil() as u32).saturating_add(1).max(1);
        let rows = (((max_y - min_y) / cell).ceil() as u32).saturating_add(1).max(1);
        Self::new(Point2::new(min_x, min_y), cell, cols, rows, top_z)
    }

    pub fn lower_at(&mut self, ix: u32, iy: u32, z: f32) {
        if ix >= self.cols || iy >= self.rows {
            return;
        }
        self.lower_at_unchecked(ix, iy, z);
    }

    /// Same contract as `lower_at`, minus the bounds check. The sweep
    /// loop pre-clamps to the heightmap's cell rectangle so the safe
    /// `lower_at` path's branch is redundant on every cell write
    /// (audit-5el3). Public callers should prefer `lower_at`.
    #[inline]
    pub fn lower_at_unchecked(&mut self, ix: u32, iy: u32, z: f32) {
        let idx = (iy as usize) * (self.cols as usize) + (ix as usize);
        if z < self.data[idx] {
            self.data[idx] = z;
            self.mark_dirty(ix, iy);
        }
    }

    /// Mark the dirty AABB to include cell `(ix, iy)` without writing to
    /// the height data. Internal helper extracted so the lower /
    /// lower-or-record paths share the AABB update math.
    #[inline]
    fn mark_dirty(&mut self, ix: u32, iy: u32) {
        self.dirty = Some(match self.dirty {
            None => (ix, iy, ix + 1, iy + 1),
            Some((x0, y0, x1, y1)) => (x0.min(ix), y0.min(iy), x1.max(ix + 1), y1.max(iy + 1)),
        });
    }

    /// ikh8: visit-tracking write — same as `lower_at` for the data side
    /// (only lowers when `z` is strictly less than the current value),
    /// but ALSO marks the cell dirty even when no value change happens.
    /// Lets downstream coverage / visit heatmaps see "the cutter was
    /// here" cells that are already at or below `z`. Bounds-checked; a
    /// no-op for cells outside the heightmap.
    pub fn lower_at_or_record(&mut self, ix: u32, iy: u32, z: f32) {
        if ix >= self.cols || iy >= self.rows {
            return;
        }
        let idx = (iy as usize) * (self.cols as usize) + (ix as usize);
        if z < self.data[idx] {
            self.data[idx] = z;
        }
        // Always mark dirty — that's the point of the visit-tracking path.
        self.mark_dirty(ix, iy);
    }

    /// Bilinear sample at world XY. Cell `(i, j)`'s center is at
    /// `origin + (i + 0.5) * cell`; positions outside the grid return `top_z`.
    #[must_use]
    pub fn sample(&self, x: f64, y: f64) -> f32 {
        let fx = (x - self.origin.x) / self.cell - 0.5;
        let fy = (y - self.origin.y) / self.cell - 0.5;
        if !fx.is_finite() || !fy.is_finite() {
            return self.top_z;
        }
        let cols_max = self.cols as f64 - 1.0;
        let rows_max = self.rows as f64 - 1.0;
        if fx < 0.0 || fy < 0.0 || fx > cols_max || fy > rows_max {
            return self.top_z;
        }
        let i0 = fx.floor();
        let j0 = fy.floor();
        let tx = (fx - i0) as f32;
        let ty = (fy - j0) as f32;
        let i0 = i0 as usize;
        let j0 = j0 as usize;
        let i1 = (i0 + 1).min(self.cols as usize - 1);
        let j1 = (j0 + 1).min(self.rows as usize - 1);
        let cols = self.cols as usize;
        let v00 = self.data[j0 * cols + i0];
        let v10 = self.data[j0 * cols + i1];
        let v01 = self.data[j1 * cols + i0];
        let v11 = self.data[j1 * cols + i1];
        let a = v00 * (1.0 - tx) + v10 * tx;
        let b = v01 * (1.0 - tx) + v11 * tx;
        a * (1.0 - ty) + b * ty
    }

    pub fn reset(&mut self) {
        for c in &mut self.data {
            *c = self.top_z;
        }
        self.dirty = None;
    }

    #[must_use]
    pub fn dirty_aabb(&self) -> Option<(u32, u32, u32, u32)> {
        self.dirty
    }

    pub fn clear_dirty(&mut self) {
        self.dirty = None;
    }

    #[must_use]
    pub fn data_ptr(&self) -> *const f32 {
        self.data.as_ptr()
    }

    #[must_use]
    pub fn data_len(&self) -> usize {
        self.data.len()
    }
}

/// Tool Z-profile: for radial offset `r` from the cutter axis, returns
/// how much above the tip Z the cutter surface sits, or `None` if `r` is
/// outside the cutting radius.
// pxv8 / legj: ToolProfile no longer derives Copy because FormProfile
// carries a Vec of sample points. All sim entry points take `&ToolProfile`
// now; the profile is built once per advance() and walked many times,
// so the ref pattern is cheaper than cloning per segment.
#[derive(Debug, Clone)]
pub enum ToolProfile {
    Endmill {
        r: f32,
    },
    BallNose {
        r: f32,
    },
    VBit {
        r: f32,
        tip_r: f32,
        half_angle_rad: f32,
    },
    DragKnife {
        r: f32,
        dragoff: f32,
    },
    Drill {
        r: f32,
    },
    LaserBeam {
        r: f32,
    },
    /// rt1.28 / rbl: flat-bottom endmill with a rounded corner fillet
    /// between the floor and the side. Floor up to `r ≤ r_outer -
    /// corner_r`; a quarter-arc of radius `corner_r` from there to the
    /// full radius.
    BullNose {
        /// Outer cutter radius (= tool diameter / 2).
        r: f32,
        /// Corner-fillet radius in mm. 0 reduces to a flat endmill;
        /// `corner_r == r` reduces to a ball-nose.
        corner_r: f32,
    },
    /// 3oly: T-slot / undercut cutter with a wide flat-bottom head at
    /// the tip and a narrower neck above. The heightmap only models the
    /// cut surface (the head's flat bottom), but the neck above is
    /// represented here so the holder/collision check can see that
    /// above `head_z_top` the cutter is narrower (`neck_r` instead of
    /// `head_r`) — the slot kerf the head leaves is wider than the
    /// neck, so the neck clears it when the cutter retracts.
    TSlot {
        /// Cutting head radius (= tool diameter / 2). This is what
        /// the heightmap carves with.
        head_r: f32,
        /// Z height (above the tip) at the top of the head. Above
        /// this, the cutter has radius `neck_r` instead of `head_r`.
        head_z_top: f32,
        /// Neck radius above the head — narrower than `head_r`.
        neck_r: f32,
    },
    /// pxv8: compression / up-down spiral endmill. Cross-section is
    /// uniform at `r` so the carved heightmap is visually identical to
    /// `Endmill { r }`. Distinguished from Endmill so the simulator can
    /// tag warnings that the up-cut / down-cut flute split (which
    /// affects chip evacuation direction but NOT cross-section) is not
    /// modeled. Follow-up: per-edge top/bottom finish tracking.
    Compression {
        r: f32,
    },
    /// pxv8: form / profile cutter with a non-uniform cross-section.
    /// `segments` is a sorted sample list `(z_above_tip_mm, r_mm)` that
    /// describes the cutter outline; the sweep carves at the appropriate
    /// radius for each Z slice. Empty / single-sample list collapses to
    /// a flat endmill at `r=segments[0].r`. Sample list assumed monotone
    /// in `z_above_tip` so the linear interp is well-defined.
    FormProfile {
        /// Sample list, low Z (tip) → high Z. `(z_above_tip_mm,
        /// radius_mm)` pairs.
        segments: Vec<(f32, f32)>,
    },
    /// legj: tip-only engraver — narrow flat at the tip with a wide
    /// cone above. Distinguished from `VBit` because only the tip flat
    /// is the cutting edge: extending farther up the cone is the
    /// non-cutting tapered shoulder, not a cutting surface. We expose
    /// the engagement depth so the heightmap can refuse to carve past
    /// the cutter's reach.
    Engraver {
        /// Tip flat radius (mm) — the cutting edge.
        tip_r: f32,
        /// Half-angle of the cone above the tip (radians) — encodes
        /// the conical body of the engraver.
        cone_half_angle: f32,
        /// Maximum engagement depth (mm) — the cutter's reach into
        /// stock from the tip plane. The sim refuses to carve past
        /// this depth even if the toolpath drives the tip lower
        /// (the operator would snap the bit in reality).
        max_engagement_depth: f32,
    },
}

impl ToolProfile {
    #[must_use]
    pub fn radius(&self) -> f32 {
        match self {
            ToolProfile::Endmill { r }
            | ToolProfile::BallNose { r }
            | ToolProfile::Drill { r }
            | ToolProfile::LaserBeam { r }
            | ToolProfile::VBit { r, .. }
            | ToolProfile::DragKnife { r, .. }
            | ToolProfile::BullNose { r, .. }
            | ToolProfile::Compression { r } => *r,
            // 3oly: the head defines the XY footprint the cutter
            // actually carves (the neck is narrower and sits above
            // the head). The shank/holder check sees the neck via
            // `HolderProfile`.
            ToolProfile::TSlot { head_r, .. } => *head_r,
            // pxv8: form cutter — XY footprint is the largest sample
            // radius (conservative; the sweep AABB must cover the
            // whole cross-section).
            ToolProfile::FormProfile { segments } => segments
                .iter()
                .map(|(_, r)| *r)
                .fold(0.0_f32, f32::max),
            // legj: engraver — the XY footprint that actually carves
            // is the tip flat. The cone above is non-cutting.
            ToolProfile::Engraver { tip_r, .. } => *tip_r,
        }
    }

    /// True for flat-bottomed profiles (Endmill / Drill / Laser /
    /// `DragKnife` / `TSlot` / `Compression`) — every cell within the
    /// cutter radius carves to the same `cutter_pz`, no per-r profile
    /// offset. The sweep can then skip both the sqrt and the `eval()`
    /// branch (audit-xnmp). 3oly: `TSlot`'s head bottom is flat too.
    /// pxv8: Compression's cross-section is identical to Endmill.
    /// FormProfile / Engraver are NOT flat-bottom (per-r profile).
    #[must_use]
    pub fn is_flat_bottom(&self) -> bool {
        matches!(
            self,
            ToolProfile::Endmill { .. }
                | ToolProfile::Drill { .. }
                | ToolProfile::LaserBeam { .. }
                | ToolProfile::DragKnife { .. }
                | ToolProfile::TSlot { .. }
                | ToolProfile::Compression { .. }
        )
    }

    /// 4mp1: maximum reach (mm) the cutter can engage into stock below the
    /// stock-top plane. `None` means "no profile-imposed limit" (the
    /// toolpath alone bounds the depth). Engraver is the only profile
    /// that exposes this today — the cone above its tip flat is non-
    /// cutting shoulder, so the bit can't survive cuts deeper than the
    /// configured engagement depth. The sweep clamps `cutter_pz` against
    /// `heightmap.top_z - max_engagement_depth` for these profiles to
    /// refuse over-deep carves the operator would never get away with
    /// in reality.
    #[must_use]
    pub fn max_engagement_depth(&self) -> Option<f32> {
        match self {
            ToolProfile::Engraver {
                max_engagement_depth,
                ..
            } => Some(*max_engagement_depth),
            _ => None,
        }
    }

    #[must_use]
    pub fn eval(&self, r: f32) -> Option<f32> {
        match self {
            ToolProfile::Endmill { r: rr }
            | ToolProfile::Drill { r: rr }
            | ToolProfile::LaserBeam { r: rr }
            | ToolProfile::DragKnife { r: rr, .. } => (r <= *rr).then_some(0.0),
            // pxv8: Compression has the same flat-bottom cross-section
            // as Endmill — see ToolKind::Compression for the rationale.
            ToolProfile::Compression { r: rr } => (r <= *rr).then_some(0.0),
            ToolProfile::BallNose { r: rr } => {
                let rr = *rr;
                if r > rr {
                    None
                } else {
                    let inside = rr.mul_add(rr, -(r * r));
                    Some(rr - inside.max(0.0).sqrt())
                }
            }
            ToolProfile::VBit {
                r: rr,
                tip_r,
                half_angle_rad,
            } => {
                let rr = *rr;
                let tip_r = *tip_r;
                let half_angle_rad = *half_angle_rad;
                if r <= tip_r {
                    Some(0.0)
                } else if r <= rr {
                    Some((r - tip_r) * half_angle_rad.tan())
                } else {
                    None
                }
            }
            ToolProfile::BullNose { r: rr, corner_r } => {
                let rr = *rr;
                let corner_r = *corner_r;
                if r > rr {
                    return None;
                }
                // Flat-bottom plateau out to (rr - corner_r); from there
                // a quarter-arc rises to corner_r at r = rr. Clamp
                // corner_r to the cutter radius so a misconfigured tool
                // collapses to ball-nose rather than blowing up.
                let cr = corner_r.clamp(0.0, rr);
                let plateau = rr - cr;
                if r <= plateau || cr <= 0.0 {
                    Some(0.0)
                } else {
                    // 6i9r: do the entire corner-radius math in f64 so
                    // an f32 subtraction near the rim (where `dx ≈ cr`,
                    // i.e. `cr² - dx² ≈ 0`) doesn't accumulate ~3e-4 mm
                    // of error before the sqrt. Promoting `r`, `rr` and
                    // `cr` to f64 BEFORE the subtraction lets the rim
                    // lip evaluate within sub-µm of the analytic value
                    // and eliminates speckle around the rim in close-up
                    // surface renderings.
                    let r_f64 = f64::from(r);
                    let rr_f64 = f64::from(rr);
                    let cr64 = f64::from(cr);
                    let plateau_f64 = rr_f64 - cr64;
                    let dx = r_f64 - plateau_f64;
                    let inside = cr64.mul_add(cr64, -(dx * dx)).max(0.0);
                    Some((cr64 - inside.sqrt()) as f32)
                }
            }
            // 3oly: head's flat bottom carves the slot floor; the
            // heightmap eval returns 0 for every r ≤ head_r (no
            // vertical profile inside the head). The neck above the
            // head is non-cutting and handled by `HolderProfile`.
            ToolProfile::TSlot { head_r, .. } => (r <= *head_r).then_some(0.0),
            // pxv8: FormProfile — linear-interp the (z, r) sample list
            // by RADIUS to recover the depth offset above the tip. The
            // sample list is monotone in z_above_tip from tip up; the
            // INVERSE mapping (largest z whose r >= query_r minus the
            // lowest z whose r >= query_r) describes a non-monotone
            // r(z) curve in general. For sim correctness we evaluate
            // by walking the segments and finding the lowest z whose
            // r reaches the query: that's the depth offset (`dz`) the
            // cell sees above the tip. Cells outside any sample
            // radius return None (cutter doesn't reach them).
            ToolProfile::FormProfile { segments } => {
                if segments.is_empty() {
                    return (r <= 0.0).then_some(0.0);
                }
                // Largest radius along the profile.
                let max_r = segments
                    .iter()
                    .map(|(_, rr)| *rr)
                    .fold(0.0_f32, f32::max);
                if r > max_r {
                    return None;
                }
                // Find the lowest `z_above_tip` (= depth offset above
                // tip) where the profile's radius reaches `r`. Walks
                // the sample list from the tip up.
                if segments[0].1 >= r {
                    return Some(segments[0].0);
                }
                for w in segments.windows(2) {
                    let (z0, r0) = w[0];
                    let (z1, r1) = w[1];
                    if r1 >= r && r0 < r {
                        if (r1 - r0).abs() < 1e-6 {
                            return Some(z0.min(z1));
                        }
                        let t = (r - r0) / (r1 - r0);
                        return Some(z0 + t * (z1 - z0));
                    }
                    if r0 >= r {
                        return Some(z0);
                    }
                }
                None
            }
            // legj: engraver — only the tip flat is the cutting edge.
            // Inside `tip_r` the cutter carves flat (dz = 0). Beyond
            // `tip_r` we're on the non-cutting cone shoulder; the sim
            // refuses to carve there and returns None so the sweep
            // skips the cell (it's not contact with material via a
            // cutting edge — at best it's a rubbing shoulder, which
            // breaks bits in real life).
            ToolProfile::Engraver { tip_r, .. } => (r <= *tip_r).then_some(0.0),
        }
    }

    /// Build a profile from a project tool entry. V-bit / engraver use
    /// `tool.tip_angle_deg` (full included angle) — sim depth before
    /// this used a hard-coded 60° regardless of the configured bit,
    /// so a 90° V-bit carved at half the correct depth in preview. A
    /// missing / unreasonable angle falls back to 60° (the default
    /// the backend assigns to a new tool).
    // BullNose / Compression / TSlot / FormProfile each collapse to an
    // Endmill profile here, but the explicit per-variant arm is the
    // documentation that they're known kinds (just deferred work, not
    // forgotten kinds). Adding a new ToolKind forces a deliberate
    // dispatch decision.
    #[allow(clippy::match_same_arms)]
    #[must_use]
    pub fn from_tool(tool: &ToolEntry) -> Self {
        let r = (tool.diameter * 0.5) as f32;
        match tool.kind {
            ToolKind::Endmill => ToolProfile::Endmill { r },
            ToolKind::BallNose => ToolProfile::BallNose { r },
            // legj: Engraver mapped to VBit at full bit diameter was
            // wrong — the engraver's cutting edge is the tip flat
            // only; the cone above is non-cutting shoulder. Distinct
            // ToolProfile arm.
            ToolKind::Engraver => {
                let tip_r = (tool.tip_diameter.unwrap_or(0.0) * 0.5).max(0.0) as f32;
                let included = if tool.tip_angle_deg > 0.0 && tool.tip_angle_deg < 180.0 {
                    tool.tip_angle_deg
                } else {
                    60.0
                };
                let cone_half_angle = ((included * 0.5).to_radians()) as f32;
                // Reach into stock is bounded by flute length when set;
                // otherwise default to a generous 5 mm — better to
                // refuse very deep cuts than rubber-stamp them.
                let max_engagement_depth =
                    tool.flute_length_mm.map_or(5.0, |v| v.max(0.0)) as f32;
                ToolProfile::Engraver {
                    tip_r,
                    cone_half_angle,
                    max_engagement_depth,
                }
            }
            ToolKind::VBit => {
                let tip_r = (tool.tip_diameter.unwrap_or(0.0) * 0.5) as f32;
                let included = if tool.tip_angle_deg > 0.0 && tool.tip_angle_deg < 180.0 {
                    tool.tip_angle_deg
                } else {
                    60.0
                };
                let half_angle_rad = ((included * 0.5).to_radians()) as f32;
                ToolProfile::VBit {
                    r,
                    tip_r,
                    half_angle_rad,
                }
            }
            ToolKind::DragKnife => ToolProfile::DragKnife {
                r,
                dragoff: tool.dragoff.unwrap_or(0.0) as f32,
            },
            ToolKind::Drill => ToolProfile::Drill { r },
            // mmu8: laser kerf comes from the configured
            // `tool.kerf_mm` (legacy default = 0.15 mm — matches the
            // historical hard-coded radius). Floor at 0.05 mm so a
            // zero / negative entry still registers some carve
            // instead of a degenerate zero-radius cutter the sweep
            // would skip. The field is the spot-radius (half-kerf)
            // — matching the prior `r: 0.15` semantics so old
            // projects round-trip unchanged.
            ToolKind::LaserBeam => {
                let kerf = tool.kerf_mm.unwrap_or(0.15).max(0.05);
                let r = kerf as f32;
                ToolProfile::LaserBeam { r }
            }
            // rbl: BullNose uses the per-tool corner_radius_mm for an
            // accurate fillet floor; the sim now models the rounded
            // corner instead of pretending it's a square endmill.
            // Falls back to flat Endmill when corner_radius is missing
            // / zero (same observable cross-section).
            // z0x0: when corner_r >= r the BullNose is geometrically
            // identical to a BallNose at the same radius (the plateau
            // collapses to zero and the corner-arc spans the full
            // cross-section). Emit BallNose directly so the sweep takes
            // the cheaper closed-form eval path instead of re-deriving
            // the same math through the f64-promoted corner-fillet
            // branch on every cell.
            ToolKind::BullNose => {
                let corner_r = (tool.corner_radius_mm.unwrap_or(0.0).max(0.0)) as f32;
                if corner_r >= r && r > 0.0 {
                    ToolProfile::BallNose { r }
                } else if corner_r > 0.0 {
                    ToolProfile::BullNose { r, corner_r }
                } else {
                    ToolProfile::Endmill { r }
                }
            }
            // pxv8: Compression — distinct ToolProfile arm with the
            // SAME cross-section as Endmill. Up/down flute split
            // affects chip evacuation direction, not the carved
            // surface; the simulator still flags the missing split
            // model in the warnings stream. Follow-up:
            // wiaconstructor-tcmp.
            ToolKind::Compression => ToolProfile::Compression { r },
            // 3oly: T-slot / undercut cutter — wide head, narrow neck
            // above. Heightmap carves with the head radius; the neck
            // is encoded in HolderProfile so the collision pass sees
            // it. When neck geometry is missing, fall back to a flat
            // endmill at head_r (same observable cross-section, no
            // false-negative regression vs the old code path).
            ToolKind::TSlot => {
                let neck_r = tool
                    .tslot_neck_diameter_mm
                    .map_or(f64::from(r), |d| (d * 0.5).max(0.0))
                    as f32;
                // ranj: flute_length doubles as head thickness whenever
                // the tool advertises any neck geometry (diameter OR
                // length). Previously we keyed the head_z_top derivation
                // on `tslot_neck_length_mm.map_or` — a T-slot with a
                // narrower neck diameter but no explicit neck-length
                // (legacy projects with only one of the two fields set)
                // silently degraded to a flat Endmill. Now ANY neck-
                // diameter-or-length presence promotes the head_z_top
                // to flute_length so the head/neck split survives, and
                // the holder pass (which still needs BOTH fields to
                // emit a neck segment) is free to keep its stricter
                // requirement without dragging the heightmap profile
                // down with it.
                let has_neck_hint = tool.tslot_neck_diameter_mm.is_some()
                    || tool.tslot_neck_length_mm.is_some();
                let head_z_top = if has_neck_hint {
                    tool.flute_length_mm.unwrap_or(0.0).max(0.0) as f32
                } else {
                    0.0
                };
                if neck_r < r && head_z_top > 0.0 {
                    ToolProfile::TSlot {
                        head_r: r,
                        head_z_top,
                        neck_r,
                    }
                } else {
                    // No neck info — same as old behaviour (flat
                    // endmill at head_r). Holder check still sees the
                    // shank/holder above the flutes if those are set.
                    ToolProfile::Endmill { r }
                }
            }
            // pxv8: FormProfile — non-uniform cross-section. When the
            // tool entry doesn't carry a sample list, fall back to a
            // single-point profile (flat endmill at head radius) and
            // emit a debug eprintln; otherwise carve at the appropriate
            // radius for each Z slice via the `segments` sample list.
            // The UI for entering form profiles is a follow-up
            // (wiaconstructor-tfrm); the sim path is ready for it.
            ToolKind::FormProfile => {
                #[cfg(debug_assertions)]
                eprintln!(
                    "ToolKind::FormProfile sim model uses tip_diameter+diameter \
                     2-segment fallback (full FormProfile sample list is a UI \
                     follow-up — wiaconstructor-tfrm)."
                );
                // Fallback: derive a minimal 2-sample profile from
                // (tip_diameter, diameter, flute_length_mm). Tip flat at
                // tip_r at z=0 up to (flute_top, r). This carves a
                // truncated cone at the right base radius — strictly
                // more accurate than Endmill { r } for form bits where
                // the diameters differ.
                let tip_r = (tool.tip_diameter.unwrap_or(tool.diameter) * 0.5).max(0.0) as f32;
                let flute_top = tool.flute_length_mm.unwrap_or(0.0).max(0.0) as f32;
                ToolProfile::FormProfile {
                    segments: vec![(0.0_f32, tip_r), (flute_top, r)],
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{Coolant, ToolEntry, ToolKind};

    const EPS: f32 = 1e-5;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < EPS
    }

    fn make_tool(kind: ToolKind, diameter: f64) -> ToolEntry {
        ToolEntry {
            id: 1,
            name: "t".into(),
            kind,
            diameter,
            tip_diameter: None,
            tip_angle_deg: 60.0,
            dragoff: None,
            flutes: 2,
            speed: 18_000,
            plunge_rate: 100,
            feed_rate: 800,
            coolant: Coolant::Off,
            speed_finish: None,
            plunge_rate_finish: None,
            feed_rate_finish: None,
            speed_drill: None,
            plunge_rate_drill: None,
            feed_rate_drill: None,
            default_peck_step_mm: None,
            default_step: None,
            default_xy_overlap: None,
            comment: None,
            z_shift_mm: None,
            laser_pierce_sec: None,
            laser_lead_in_mm: None,
            kerf_mm: None,
            corner_radius_mm: None,
            tslot_neck_diameter_mm: None,
            tslot_neck_length_mm: None,
            wirbeln: false,
            wirbeln_stepover_mm: None,
            wirbeln_extra_width_mm: None,
            wirbeln_osc_mm: None,
            pause: 1,
            flute_length_mm: None,
            shank_diameter_mm: None,
            stickout_length_mm: None,
            holder: None,
            spindle_direction: crate::project::SpindleDirection::default(),
            drag_knife_self_align_angle_deg: None,
            pierce_height_mm: None,
            cut_height_mm: None,
            pierce_delay_sec: None,
            vcarve_lead_in_angle_deg: None,
        }
    }

    #[test]
    fn new_initializes_to_top_z() {
        let hm = Heightmap::new(Point2::new(0.0, 0.0), 0.5, 4, 3, 1.5);
        assert_eq!(hm.data.len(), 12);
        assert!(hm.data.iter().all(|&v| approx(v, 1.5)));
        assert_eq!(hm.dirty_aabb(), None);
    }

    #[test]
    fn from_bbox_rounds_up_to_cover() {
        let hm = Heightmap::from_bbox(0.0, 0.0, 10.0, 10.0, 0.7, 0.0);
        let expected = (10.0_f64 / 0.7).ceil() as u32;
        assert!(hm.cols >= expected);
        assert!(hm.rows >= expected);
        assert_eq!(hm.origin, Point2::new(0.0, 0.0));
    }

    #[test]
    fn lower_at_only_writes_when_strictly_lower() {
        let mut hm = Heightmap::new(Point2::new(0.0, 0.0), 1.0, 4, 4, 0.0);
        hm.lower_at(2, 2, -1.0);
        hm.lower_at(2, 2, -0.5);
        let idx = 2_usize * 4 + 2;
        assert!(approx(hm.data[idx], -1.0));
    }

    #[test]
    fn lower_at_out_of_bounds_is_noop() {
        let mut hm = Heightmap::new(Point2::new(0.0, 0.0), 1.0, 4, 4, 0.0);
        hm.lower_at(10, 0, -1.0);
        hm.lower_at(0, 10, -1.0);
        assert!(hm.data.iter().all(|&v| approx(v, 0.0)));
        assert_eq!(hm.dirty_aabb(), None);
    }

    #[test]
    fn lower_at_or_record_marks_dirty_even_without_value_change() {
        // ikh8: visit-tracking write — same cell visited twice with the
        // same depth should record TWO dirty events. The strict `<`
        // write only records the first; `lower_at_or_record` always
        // does even when the value doesn't change.
        let mut hm = Heightmap::new(Point2::new(0.0, 0.0), 1.0, 4, 4, 0.0);
        hm.lower_at(2, 2, -1.0);
        let first = hm.dirty_aabb();
        assert!(first.is_some());
        hm.clear_dirty();
        assert_eq!(hm.dirty_aabb(), None);
        // Same cell, same depth → strict lower_at would NOT mark dirty.
        hm.lower_at(2, 2, -1.0);
        assert_eq!(hm.dirty_aabb(), None, "lower_at must skip same-Z write");
        // lower_at_or_record DOES mark the visit even though z didn't
        // change.
        hm.lower_at_or_record(2, 2, -1.0);
        assert_eq!(hm.dirty_aabb(), Some((2, 2, 3, 3)));
        // And it still lowers when given a strictly lower value.
        hm.lower_at_or_record(2, 2, -2.0);
        let idx = 2_usize * 4 + 2;
        assert!(approx(hm.data[idx], -2.0));
    }

    #[test]
    fn lower_at_or_record_out_of_bounds_is_noop() {
        // ikh8: bounds-check still applies — never panic on stray
        // indices and never mark dirty for cells outside the grid.
        let mut hm = Heightmap::new(Point2::new(0.0, 0.0), 1.0, 4, 4, 0.0);
        hm.lower_at_or_record(10, 0, -1.0);
        hm.lower_at_or_record(0, 10, -1.0);
        assert_eq!(hm.dirty_aabb(), None);
    }

    #[test]
    fn dirty_aabb_tracks_min_max_indices() {
        let mut hm = Heightmap::new(Point2::new(0.0, 0.0), 1.0, 16, 16, 0.0);
        hm.lower_at(3, 5, -1.0);
        hm.lower_at(7, 2, -1.0);
        assert_eq!(hm.dirty_aabb(), Some((3, 2, 8, 6)));
    }

    #[test]
    fn reset_returns_top_z_and_clears_dirty() {
        let mut hm = Heightmap::new(Point2::new(0.0, 0.0), 1.0, 4, 4, 0.25);
        hm.lower_at(1, 1, -2.0);
        assert!(hm.dirty_aabb().is_some());
        hm.reset();
        assert!(hm.data.iter().all(|&v| approx(v, 0.25)));
        assert!(approx(hm.sample(2.5, 2.5), 0.25));
        assert_eq!(hm.dirty_aabb(), None);
    }

    #[test]
    fn sample_bilinear_at_cell_center_returns_cell_value() {
        // 2x2 grid, cell = 1.0, origin (0,0). Cell centers: (0.5, 0.5),
        // (1.5, 0.5), (0.5, 1.5), (1.5, 1.5).
        let mut hm = Heightmap::new(Point2::new(0.0, 0.0), 1.0, 2, 2, 0.0);
        hm.data[0] = 1.0;
        hm.data[1] = 2.0;
        hm.data[2] = 3.0;
        hm.data[3] = 4.0;
        assert!(approx(hm.sample(0.5, 0.5), 1.0));
        assert!(approx(hm.sample(1.5, 0.5), 2.0));
        assert!(approx(hm.sample(0.5, 1.5), 3.0));
        assert!(approx(hm.sample(1.5, 1.5), 4.0));
    }

    #[test]
    fn sample_out_of_bounds_returns_top_z() {
        let hm = Heightmap::new(Point2::new(0.0, 0.0), 1.0, 4, 4, 7.0);
        assert!(approx(hm.sample(-5.0, 0.0), 7.0));
        assert!(approx(hm.sample(0.0, -5.0), 7.0));
        assert!(approx(hm.sample(100.0, 0.0), 7.0));
        assert!(approx(hm.sample(0.0, 100.0), 7.0));
    }

    #[test]
    fn endmill_profile_zero_inside_r_none_outside() {
        let p = ToolProfile::Endmill { r: 2.0 };
        assert_eq!(p.eval(0.0), Some(0.0));
        assert_eq!(p.eval(1.0), Some(0.0));
        assert_eq!(p.eval(2.0), Some(0.0));
        assert_eq!(p.eval(2.001), None);
    }

    #[test]
    fn ball_nose_profile_matches_analytic() {
        let r = 2.0_f32;
        let p = ToolProfile::BallNose { r };
        let half = p.eval(r / 2.0).expect("inside radius");
        let expected = r * (1.0 - (3.0_f32).sqrt() / 2.0);
        assert!(approx(half, expected), "{half} vs {expected}");
        assert!(approx(p.eval(0.0).unwrap(), 0.0));
        assert!(approx(p.eval(r).unwrap(), r));
        assert_eq!(p.eval(r + 0.001), None);
    }

    #[test]
    fn vbit_profile_with_tip_flat() {
        let half_angle = 0.4_f32;
        let p = ToolProfile::VBit {
            r: 3.0,
            tip_r: 0.5,
            half_angle_rad: half_angle,
        };
        assert_eq!(p.eval(0.25), Some(0.0));
        let at_outer = p.eval(3.0).unwrap();
        let expected = (3.0_f32 - 0.5) * half_angle.tan();
        assert!(approx(at_outer, expected));
        assert_eq!(p.eval(3.001), None);
    }

    #[test]
    fn from_tool_endmill_uses_diameter() {
        let t = make_tool(ToolKind::Endmill, 6.0);
        let p = ToolProfile::from_tool(&t);
        assert!(matches!(p, ToolProfile::Endmill { .. }));
        assert!(approx(p.radius(), 3.0));
    }

    #[test]
    fn from_tool_ballnose_and_drill() {
        let t = make_tool(ToolKind::BallNose, 4.0);
        assert!(matches!(
            ToolProfile::from_tool(&t),
            ToolProfile::BallNose { .. }
        ));
        let t = make_tool(ToolKind::Drill, 5.0);
        assert!(matches!(
            ToolProfile::from_tool(&t),
            ToolProfile::Drill { .. }
        ));
    }

    /// Regression: V-bit half-angle was hard-coded to 30° (60°
    /// included) regardless of `tool.tip_angle_deg`. A 90° bit
    /// carved at half its true depth in the sim. The profile now
    /// reads the tool's configured angle.
    #[test]
    fn from_tool_vbit_uses_tip_angle_deg() {
        for &(angle_deg, expected_half_rad) in &[
            (60.0_f64, std::f32::consts::FRAC_PI_6), // baseline default
            (90.0_f64, std::f32::consts::FRAC_PI_4),
            (30.0_f64, (15.0_f32).to_radians()),
        ] {
            let mut t = make_tool(ToolKind::VBit, 6.0);
            t.tip_angle_deg = angle_deg;
            match ToolProfile::from_tool(&t) {
                ToolProfile::VBit { half_angle_rad, .. } => {
                    assert!(
                        approx(half_angle_rad, expected_half_rad),
                        "angle {angle_deg}: got {half_angle_rad}, expected {expected_half_rad}",
                    );
                }
                other => panic!("expected VBit profile, got {other:?}"),
            }
        }
    }

    /// Defense-in-depth: an out-of-range `tip_angle_deg` (e.g. an old
    /// project before the field was required) falls back to 60°
    /// instead of producing a NaN or zero-angle cone.
    #[test]
    fn from_tool_vbit_falls_back_to_60_on_invalid_angle() {
        let mut t = make_tool(ToolKind::VBit, 6.0);
        t.tip_angle_deg = 0.0;
        if let ToolProfile::VBit { half_angle_rad, .. } = ToolProfile::from_tool(&t) {
            assert!(approx(half_angle_rad, std::f32::consts::FRAC_PI_6));
        } else {
            panic!("expected VBit profile");
        }
    }

    /// rbl: `BullNose` with `corner_radius_mm` builds a fillet profile;
    /// without it (or with 0) collapses to a flat endmill. Eval at the
    /// rim equals `corner_r` (the lip rises by exactly the fillet radius);
    /// eval inside the plateau equals 0.
    #[test]
    fn from_tool_bullnose_uses_corner_radius() {
        let mut t = make_tool(ToolKind::BullNose, 6.0);
        t.corner_radius_mm = Some(0.8);
        match ToolProfile::from_tool(&t) {
            ToolProfile::BullNose { r, corner_r } => {
                assert!(approx(r, 3.0));
                assert!(approx(corner_r, 0.8));
                // r=0 → centre of flat bottom → depth 0
                assert_eq!(ToolProfile::BullNose { r, corner_r }.eval(0.0), Some(0.0));
                // r at the plateau edge (rr - corner_r = 2.2) → still 0
                assert!(approx(
                    ToolProfile::BullNose { r, corner_r }.eval(2.2).unwrap(),
                    0.0,
                ));
                // r at the rim (r = rr) → lip has risen by corner_r.
                // 6i9r: the BullNose eval now does the corner-radius
                // math in f64 before snapping to f32, so the residual
                // at the rim is well under 1e-5 mm instead of the old
                // ~3e-4 mm. Tighten the tolerance to lock that in.
                let lip = ToolProfile::BullNose { r, corner_r }.eval(3.0).unwrap();
                assert!((lip - 0.8).abs() < 1e-5, "lip = {lip}, expected ≈ 0.8");
                // r > rr → outside the cutter
                assert!(ToolProfile::BullNose { r, corner_r }.eval(3.5).is_none());
            }
            other => panic!("expected BullNose profile, got {other:?}"),
        }
    }

    #[test]
    fn from_tool_bullnose_no_corner_radius_collapses_to_endmill() {
        let t = make_tool(ToolKind::BullNose, 6.0); // corner_radius_mm = None
        assert!(matches!(
            ToolProfile::from_tool(&t),
            ToolProfile::Endmill { .. }
        ));
    }

    /// 6i9r: sample the BullNose profile at a fine grid of rims close
    /// to the cutter edge. The lip height must be uniform within 1e-5
    /// mm across every neighboring rim sample — previously f32 jitter
    /// at the boundary produced ~3e-4 mm speckle in close-up surface
    /// renderings.
    #[test]
    fn bullnose_rim_lip_consistent_across_neighboring_cells() {
        let r = 3.0_f32;
        let corner_r = 0.8_f32;
        let p = ToolProfile::BullNose { r, corner_r };
        // Sample 21 points along the corner-fillet arc, including the
        // exact rim at r = 3.0.
        let mut samples = Vec::with_capacity(21);
        for k in 0..=20 {
            let probe = r - corner_r + (corner_r * (k as f32) / 20.0);
            samples.push(p.eval(probe).expect("inside rim"));
        }
        // The lip values should rise monotonically from 0 at the
        // plateau-edge to corner_r at the rim — with no speckle
        // (adjacent samples differ by less than the analytic step).
        let analytic_rim = corner_r;
        let observed_rim = *samples.last().unwrap();
        assert!(
            (observed_rim - analytic_rim).abs() < 1e-5,
            "rim residual {observed_rim} vs analytic {analytic_rim}",
        );
        // Check monotonicity (no jitter that flips a sample below its
        // predecessor).
        for w in samples.windows(2) {
            assert!(
                w[1] >= w[0] - 1e-6,
                "non-monotone bullnose rim samples: {} → {}",
                w[0],
                w[1],
            );
        }
    }

    /// vc6i: bilinear `sample()` at the bbox max-corner must return
    /// the carved cell value, not `top_z`. The previous tight `ceil()`
    /// sizing left the max-corner half a cell off the sampleable
    /// region; the +1-cell pad guarantees the corner is reachable.
    #[test]
    fn sample_at_max_corner_returns_cell_value_not_top_z() {
        // 10×10 mm bbox at 1mm cell. With the +1-cell pad cols = 11.
        let mut hm = Heightmap::from_bbox(0.0, 0.0, 10.0, 10.0, 1.0, 0.0);
        // Carve every cell to z = -3 so any in-range sample returns -3.
        for v in &mut hm.data {
            *v = -3.0;
        }
        // The max-corner (10.0, 10.0) must read -3.0, not top_z = 0.0.
        let probed = hm.sample(10.0, 10.0);
        assert!(
            (probed - -3.0).abs() < 1e-5,
            "max-corner sample returned {probed}, expected -3.0 (vc6i regression)",
        );
        // Also probe just inside the max-corner — same expectation.
        let probed_inside = hm.sample(9.99, 9.99);
        assert!((probed_inside - -3.0).abs() < 1e-5);
    }

    /// mmu8: laser kerf radius reads from `tool.kerf_mm` instead of
    /// being hard-coded to 0.15. Tools with different kerf widths
    /// produce different sim radii; missing kerf_mm collapses to the
    /// legacy 0.15 mm default; near-zero kerf is floored at 0.05 mm
    /// so the sweep doesn't bail on a degenerate zero-radius cutter.
    #[test]
    fn laser_kerf_uses_configured_diameter() {
        let mut fine = make_tool(ToolKind::LaserBeam, 0.0);
        fine.kerf_mm = Some(0.05);
        let mut wide = make_tool(ToolKind::LaserBeam, 0.0);
        wide.kerf_mm = Some(0.4);
        let default = make_tool(ToolKind::LaserBeam, 0.0); // kerf_mm = None
        let mut zero = make_tool(ToolKind::LaserBeam, 0.0);
        zero.kerf_mm = Some(0.0);
        match ToolProfile::from_tool(&fine) {
            ToolProfile::LaserBeam { r } => assert!((r - 0.05).abs() < 1e-5, "got {r}"),
            other => panic!("expected LaserBeam, got {other:?}"),
        }
        match ToolProfile::from_tool(&wide) {
            ToolProfile::LaserBeam { r } => assert!((r - 0.4).abs() < 1e-5, "got {r}"),
            other => panic!("expected LaserBeam, got {other:?}"),
        }
        match ToolProfile::from_tool(&default) {
            ToolProfile::LaserBeam { r } => assert!(
                (r - 0.15).abs() < 1e-5,
                "default kerf must match legacy 0.15, got {r}",
            ),
            other => panic!("expected LaserBeam, got {other:?}"),
        }
        // 0 kerf is floored at the 0.05 mm safety floor.
        match ToolProfile::from_tool(&zero) {
            ToolProfile::LaserBeam { r } => assert!(
                (r - 0.05).abs() < 1e-5,
                "zero kerf should floor at 0.05, got {r}",
            ),
            other => panic!("expected LaserBeam, got {other:?}"),
        }
    }

    /// z0x0: BullNose with `corner_radius_mm >= diameter/2` collapses to
    /// a BallNose at the same outer radius. The geometry is identical
    /// (plateau width = 0; the corner-arc spans the whole cross-section)
    /// but the BullNose eval routes through the f64-promoted corner-
    /// fillet branch on every cell; BallNose hits the cheaper closed
    /// form. Pure perf — the carve shape stays the same.
    #[test]
    fn from_tool_bullnose_corner_eq_r_collapses_to_ballnose() {
        let mut t = make_tool(ToolKind::BullNose, 6.0);
        // Equal: corner_r == r.
        t.corner_radius_mm = Some(3.0);
        assert!(matches!(
            ToolProfile::from_tool(&t),
            ToolProfile::BallNose { .. }
        ));
        // Larger than r — still collapse to BallNose (the BullNose eval
        // would otherwise clamp corner_r to r internally and produce the
        // same shape, but a half-tick slower).
        t.corner_radius_mm = Some(4.0);
        assert!(matches!(
            ToolProfile::from_tool(&t),
            ToolProfile::BallNose { .. }
        ));
        // Strictly less — still BullNose.
        t.corner_radius_mm = Some(2.0);
        assert!(matches!(
            ToolProfile::from_tool(&t),
            ToolProfile::BullNose { .. }
        ));
    }

    /// ranj: T-slot cutter with a narrower neck diameter but no explicit
    /// `tslot_neck_length_mm` previously degraded to a flat Endmill
    /// because `head_z_top` keyed off `tslot_neck_length_mm.map_or`.
    /// Now any neck-hint (diameter OR length) promotes the head/neck
    /// split — head_z_top is derived from `flute_length_mm`.
    #[test]
    fn from_tool_tslot_neck_diameter_only_still_emits_tslot_profile() {
        let mut t = make_tool(ToolKind::TSlot, 16.0);
        t.tslot_neck_diameter_mm = Some(4.0);
        t.tslot_neck_length_mm = None;
        t.flute_length_mm = Some(4.0);
        match ToolProfile::from_tool(&t) {
            ToolProfile::TSlot {
                head_r,
                head_z_top,
                neck_r,
            } => {
                assert!(approx(head_r, 8.0));
                assert!(approx(neck_r, 2.0));
                assert!(
                    approx(head_z_top, 4.0),
                    "head_z_top should come from flute_length when only the neck diameter is set, got {head_z_top}",
                );
            }
            other => panic!("expected TSlot profile, got {other:?}"),
        }
    }

    /// ranj companion: legacy "no neck info at all" still falls back to
    /// the flat Endmill model. Only the diameter-or-length-set hint
    /// flips the head/neck split on.
    #[test]
    fn from_tool_tslot_no_neck_info_still_endmill() {
        let mut t = make_tool(ToolKind::TSlot, 16.0);
        t.tslot_neck_diameter_mm = None;
        t.tslot_neck_length_mm = None;
        t.flute_length_mm = Some(4.0);
        assert!(matches!(
            ToolProfile::from_tool(&t),
            ToolProfile::Endmill { .. }
        ));
    }

    /// 4mp1: Engraver advertises a non-None `max_engagement_depth` so the
    /// sweep can refuse to carve past the cutter's reach.
    #[test]
    fn engraver_advertises_max_engagement_depth() {
        let mut t = make_tool(ToolKind::Engraver, 6.0);
        t.tip_diameter = Some(0.5);
        t.flute_length_mm = Some(2.5);
        let p = ToolProfile::from_tool(&t);
        assert!(matches!(p, ToolProfile::Engraver { .. }));
        assert_eq!(p.max_engagement_depth(), Some(2.5));
        // Sanity: non-Engraver profiles return None.
        let em = make_tool(ToolKind::Endmill, 6.0);
        assert_eq!(ToolProfile::from_tool(&em).max_engagement_depth(), None);
    }
}
