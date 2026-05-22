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
        let len = (cols as usize) * (rows as usize);
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
        let cols = ((max_x - min_x) / cell).ceil().max(1.0) as u32;
        let rows = ((max_y - min_y) / cell).ceil().max(1.0) as u32;
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
            self.dirty = Some(match self.dirty {
                None => (ix, iy, ix + 1, iy + 1),
                Some((x0, y0, x1, y1)) => (x0.min(ix), y0.min(iy), x1.max(ix + 1), y1.max(iy + 1)),
            });
        }
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
#[derive(Debug, Clone, Copy)]
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
}

impl ToolProfile {
    #[must_use]
    pub fn radius(&self) -> f32 {
        match *self {
            ToolProfile::Endmill { r }
            | ToolProfile::BallNose { r }
            | ToolProfile::Drill { r }
            | ToolProfile::LaserBeam { r }
            | ToolProfile::VBit { r, .. }
            | ToolProfile::DragKnife { r, .. }
            | ToolProfile::BullNose { r, .. } => r,
            // 3oly: the head defines the XY footprint the cutter
            // actually carves (the neck is narrower and sits above
            // the head). The shank/holder check sees the neck via
            // `HolderProfile`.
            ToolProfile::TSlot { head_r, .. } => head_r,
        }
    }

    /// True for flat-bottomed profiles (Endmill / Drill / Laser /
    /// `DragKnife` / `TSlot`) — every cell within the cutter radius
    /// carves to the same `cutter_pz`, no per-r profile offset. The
    /// sweep can then skip both the sqrt and the `eval()` branch
    /// (audit-xnmp). 3oly: `TSlot`'s head bottom is flat too.
    #[must_use]
    pub fn is_flat_bottom(&self) -> bool {
        matches!(
            self,
            ToolProfile::Endmill { .. }
                | ToolProfile::Drill { .. }
                | ToolProfile::LaserBeam { .. }
                | ToolProfile::DragKnife { .. }
                | ToolProfile::TSlot { .. }
        )
    }

    #[must_use]
    pub fn eval(&self, r: f32) -> Option<f32> {
        match *self {
            ToolProfile::Endmill { r: rr }
            | ToolProfile::Drill { r: rr }
            | ToolProfile::LaserBeam { r: rr }
            | ToolProfile::DragKnife { r: rr, .. } => (r <= rr).then_some(0.0),
            ToolProfile::BallNose { r: rr } => {
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
                if r <= tip_r {
                    Some(0.0)
                } else if r <= rr {
                    Some((r - tip_r) * half_angle_rad.tan())
                } else {
                    None
                }
            }
            ToolProfile::BullNose { r: rr, corner_r } => {
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
                    let dx = r - plateau;
                    let inside = cr.mul_add(cr, -(dx * dx));
                    Some(cr - inside.max(0.0).sqrt())
                }
            }
            // 3oly: head's flat bottom carves the slot floor; the
            // heightmap eval returns 0 for every r ≤ head_r (no
            // vertical profile inside the head). The neck above the
            // head is non-cutting and handled by `HolderProfile`.
            ToolProfile::TSlot { head_r, .. } => (r <= head_r).then_some(0.0),
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
            ToolKind::VBit | ToolKind::Engraver => {
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
            // Laser kerf is effectively zero; give it a small finite radius
            // so the heightmap can still register etching.
            ToolKind::LaserBeam => ToolProfile::LaserBeam { r: 0.15 },
            // rbl: BullNose uses the per-tool corner_radius_mm for an
            // accurate fillet floor; the sim now models the rounded
            // corner instead of pretending it's a square endmill.
            // Falls back to flat Endmill when corner_radius is missing
            // / zero (same observable cross-section).
            ToolKind::BullNose => {
                let corner_r = (tool.corner_radius_mm.unwrap_or(0.0).max(0.0)) as f32;
                if corner_r > 0.0 {
                    ToolProfile::BullNose { r, corner_r }
                } else {
                    ToolProfile::Endmill { r }
                }
            }
            // Compression: identical floor profile to Endmill (the
            // up-cut / down-cut flute split affects chip evacuation,
            // not the cross-section). Track-issue follow-up
            // (wiaconstructor-tcmp): model up-cut/down-cut split for
            // edge-quality diagnostics.
            ToolKind::Compression => ToolProfile::Endmill { r },
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
                // Flute length doubles as head thickness when set;
                // otherwise the head is a thin disk at the tip
                // (head_z_top = 0) and we keep the old Endmill model.
                let head_z_top = tool.tslot_neck_length_mm.map_or(0.0, |_| {
                    tool.flute_length_mm.unwrap_or(0.0).max(0.0)
                }) as f32;
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
            // FormProfile: needs user-supplied profile geometry which
            // has no UI yet. Until 3oly's TSlot work is generalised
            // (track-issue wiaconstructor-tfrm), collapse to a flat
            // endmill at the head radius — same as before — but emit
            // an eprintln in debug so the user / dev notices the
            // missing model.
            ToolKind::FormProfile => {
                #[cfg(debug_assertions)]
                eprintln!(
                    "ToolKind::FormProfile sim model is unimplemented (3oly follow-up); \
                     falling back to flat endmill at head diameter."
                );
                ToolProfile::Endmill { r }
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
            holder: None,
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
                // f32 precision near the boundary leaves a residual of
                // ~3e-4 mm; the heightmap grid step is orders of
                // magnitude coarser than that so the slack is harmless.
                let lip = ToolProfile::BullNose { r, corner_r }.eval(3.0).unwrap();
                assert!((lip - 0.8).abs() < 1e-3, "lip = {lip}, expected ≈ 0.8");
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
}
