//! HPGL post-processor — pen-up/pen-down style for plotters and drag knives.
//! Mirrors `output_plugins/hpgl.py`.

// # CAM/sim pedantic-lint exemptions
// HPGL plotter post emits 2D coordinates in plu (plotter units): `f64 → i64`
// conversions are domain-bounded by the plotter's addressable plane.
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]

use crate::cam::setup::{ToolOffset, UnitSystem};
use crate::gcode::{CapturedPostState, PostProcessor};

#[derive(Debug, Default)]
pub struct Post {
    out: Vec<String>,
    pen_down: bool,
    last_x: Option<i64>,
    last_y: Option<i64>,
    /// p9ji: last emitted plotter velocity (`VS<v>;`) in cm/s. Tracked
    /// so we only re-emit when the value changes; the plotter remembers
    /// the last VS until it sees a new one or `IN;` resets it.
    last_vs: Option<u32>,
}

impl Post {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn fmt_xy(x: f64, y: f64) -> (i64, i64) {
        ((x * 40.0).round() as i64, (y * 40.0).round() as i64)
    }

    fn write(&mut self, s: impl Into<String>) {
        self.out.push(s.into());
    }

    /// Tessellate a G2/G3 arc into pen-down chord polyline at ~5° per
    /// chord (audit 1pcz). HPGL has an `AA` (arc absolute) opcode but
    /// support varies between controllers; tessellation is universal
    /// and visually indistinguishable on the 40 plu/mm grid at 5°
    /// (chord error r·(1-cos(2.5°)) ≈ 0.001·r ≈ 0.04 mm on a 40 mm
    /// arc, well under the plotter's resolution of 0.025 mm).
    ///
    /// The old code linearized to a single chord — a full DXF circle
    /// emitted as a zero-length move (start == end), a quarter-arc
    /// from (10, 0) to (0, 10) became a straight diagonal. Drag knives
    /// cut a triangle where the operator drew a curve.
    fn tessellated_arc(
        &mut self,
        x: Option<f64>,
        y: Option<f64>,
        i: Option<f64>,
        j: Option<f64>,
        cw: bool,
    ) {
        // We need a start point. Use the last emitted position if the
        // arc is partial in either coordinate. Fall back to linearize
        // when we can't recover both (no last position) — same shape
        // as the old single-chord behavior, but only for the
        // unreachable edge case.
        let (Some(sx), Some(sy)) = (
            self.last_x.map(|v| v as f64 / 40.0),
            self.last_y.map(|v| v as f64 / 40.0),
        ) else {
            self.linear(x, y, None);
            return;
        };
        let ex = x.unwrap_or(sx);
        let ey = y.unwrap_or(sy);
        let ii = i.unwrap_or(0.0);
        let jj = j.unwrap_or(0.0);
        let cx = sx + ii;
        let cy = sy + jj;
        let r = ((sx - cx).powi(2) + (sy - cy).powi(2)).sqrt();
        if r < 1e-9 {
            self.linear(x, y, None);
            return;
        }
        let theta_start = (sy - cy).atan2(sx - cx);
        let theta_end = (ey - cy).atan2(ex - cx);
        const TAU: f64 = std::f64::consts::TAU;
        let mut sweep = theta_end - theta_start;
        let coincident = (sx - ex).abs() < 1e-9 && (sy - ey).abs() < 1e-9;
        if cw {
            // G2 = CW = decreasing theta
            if coincident {
                sweep = -TAU;
            } else if sweep >= -1e-9 {
                sweep -= TAU;
            }
        } else {
            // G3 = CCW = increasing theta
            if coincident {
                sweep = TAU;
            } else if sweep <= 1e-9 {
                sweep += TAU;
            }
        }
        // ~5° per chord; min 8 chords (so even tiny arcs draw curved).
        let step = 5f64.to_radians();
        let n = (sweep.abs() / step).ceil().max(8.0) as usize;
        if !self.pen_down {
            self.write("PD;");
            self.pen_down = true;
        }
        for k in 1..=n {
            let theta = theta_start + sweep * (k as f64) / (n as f64);
            let (px, py) = if k == n {
                (ex, ey)
            } else {
                (cx + r * theta.cos(), cy + r * theta.sin())
            };
            let (xi, yi) = Self::fmt_xy(px, py);
            self.write(format!("PA{xi},{yi};"));
            self.last_x = Some(xi);
            self.last_y = Some(yi);
        }
    }
}

impl PostProcessor for Post {
    fn unit(&mut self, _unit: UnitSystem) {
        // HPGL is plotter-units (40 per mm); units handled by fmt_xy.
    }
    fn feedrate(&mut self, rate: u32) {
        // p9ji: HPGL exposes plotter velocity via `VS<v>;` (cm/s).
        // Without an explicit VS, the plotter falls back to its boot
        // default — wrong for drag-knife setups where a slow first cut
        // prevents the marker from dragging through the workpiece.
        // Map our mm/min feed → cm/s (divide by 600), clamp ≥1, and
        // de-dup against the last emitted velocity. `rate==0` means
        // "don't care" — leave the plotter's previous velocity.
        if rate == 0 {
            return;
        }
        // mm/min ÷ 600 = cm/s, rounded to the nearest integer (HPGL VS
        // is integer cm/s on every plotter we've seen). Clamp ≥ 1 so a
        // tiny mm/min input doesn't emit VS0 (which means "default").
        let vs = ((f64::from(rate) / 600.0).round() as u32).max(1);
        if self.last_vs == Some(vs) {
            return;
        }
        self.write(format!("VS{vs};"));
        self.last_vs = Some(vs);
    }
    fn program_start(&mut self) {
        self.write("IN;SP1;");
    }
    fn program_end(&mut self) {
        self.write("PU;SP0;IN;");
    }
    fn spindle_cw(&mut self, _speed: u32, _pause: u32) {}
    fn spindle_ccw(&mut self, _speed: u32, _pause: u32) {}
    fn move_to(&mut self, x: Option<f64>, y: Option<f64>, _z: Option<f64>) {
        // Pen up + move.
        if self.pen_down {
            self.write("PU;");
            self.pen_down = false;
        }
        if let (Some(x), Some(y)) = (x, y) {
            let (xi, yi) = Self::fmt_xy(x, y);
            self.write(format!("PA{xi},{yi};"));
            self.last_x = Some(xi);
            self.last_y = Some(yi);
        }
    }
    fn linear(&mut self, x: Option<f64>, y: Option<f64>, _z: Option<f64>) {
        if !self.pen_down {
            self.write("PD;");
            self.pen_down = true;
        }
        let cx = x.or_else(|| self.last_x.map(|v| v as f64 / 40.0));
        let cy = y.or_else(|| self.last_y.map(|v| v as f64 / 40.0));
        if let (Some(cx), Some(cy)) = (cx, cy) {
            let (xi, yi) = Self::fmt_xy(cx, cy);
            self.write(format!("PA{xi},{yi};"));
            self.last_x = Some(xi);
            self.last_y = Some(yi);
        }
    }
    fn arc_cw(
        &mut self,
        x: Option<f64>,
        y: Option<f64>,
        _z: Option<f64>,
        i: Option<f64>,
        j: Option<f64>,
    ) {
        self.tessellated_arc(x, y, i, j, true);
    }
    fn arc_ccw(
        &mut self,
        x: Option<f64>,
        y: Option<f64>,
        _z: Option<f64>,
        i: Option<f64>,
        j: Option<f64>,
    ) {
        self.tessellated_arc(x, y, i, j, false);
    }
    fn tool_offsets(&mut self, _offset: ToolOffset) {}
    fn finish(&self) -> String {
        self.out.join("") + "\n"
    }
    fn out_lines_count(&self) -> usize {
        self.out.len()
    }
    fn out_lines_clone_from(&self, start: usize) -> Vec<String> {
        if start >= self.out.len() {
            Vec::new()
        } else {
            self.out[start..].to_vec()
        }
    }
    fn out_extend_lines(&mut self, lines: &[String]) {
        self.out.extend_from_slice(lines);
    }
    fn reset_state(&mut self) {
        // HPGL pen-state can stay; the cache replays absolute PA moves
        // either way. Resetting last_x/y forces an explicit jump on
        // the next move, matching the LinuxCNC reset semantics.
        self.last_x = None;
        self.last_y = None;
    }
    fn capture_state(&self) -> CapturedPostState {
        CapturedPostState {
            last_x: self.last_x.map(|v| v as f64 / 40.0),
            last_y: self.last_y.map(|v| v as f64 / 40.0),
            last_z: None,
            last_rate: None,
            last_speed: None,
        }
    }
    fn restore_state(&mut self, s: &CapturedPostState) {
        self.last_x = s.last_x.map(|v| (v * 40.0).round() as i64);
        self.last_y = s.last_y.map(|v| (v * 40.0).round() as i64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hpgl_circle_renders_as_polygon() {
        // 1pcz: A full circle (start == end with I/J center offset)
        // must produce a curved polyline, not a zero-length move.
        let mut post = Post::new();
        post.program_start();
        // Pen up move to start: (10, 0)
        post.move_to(Some(10.0), Some(0.0), None);
        // Full CCW circle: start = end = (10, 0), I = -10, J = 0 → center (0, 0).
        post.arc_ccw(Some(10.0), Some(0.0), None, Some(-10.0), Some(0.0));
        let out = post.finish();
        // Count `PA<x>,<y>;` tokens.
        let pa_count = out.matches("PA").count();
        assert!(
            pa_count >= 30,
            "full circle should tessellate to ≥30 PA tokens (got {pa_count}): {out}",
        );
    }

    #[test]
    fn p9ji_feedrate_emits_vs() {
        // Drag-knife setup: the first feedrate call must emit VS<n>
        // so the plotter doesn't traverse at its boot-default speed.
        let mut post = Post::new();
        post.program_start();
        post.feedrate(600); // 600 mm/min → VS1
        post.move_to(Some(10.0), Some(10.0), None);
        let out = post.finish();
        assert!(
            out.contains("VS1;"),
            "expected VS1; in HPGL output for feedrate=600 mm/min, got: {out}",
        );
    }

    #[test]
    fn p9ji_repeat_feedrate_deduped() {
        // Repeated same-velocity feedrate calls should emit VS exactly
        // once (the plotter keeps the last value until IN; resets).
        let mut post = Post::new();
        post.feedrate(600);
        post.feedrate(600);
        post.feedrate(600);
        let out = post.finish();
        let vs_count = out.matches("VS").count();
        assert_eq!(vs_count, 1, "expected one VS emission, got {vs_count}: {out}");
    }

    #[test]
    fn p9ji_feedrate_change_emits_new_vs() {
        // Slower cut after a fast jog: must emit VS again at the new
        // velocity so drag-knife / plotter changes show up.
        let mut post = Post::new();
        post.feedrate(6000); // 10 cm/s
        post.feedrate(600); // 1 cm/s
        let out = post.finish();
        assert!(out.contains("VS10;"), "missing VS10 from initial fast feed: {out}");
        assert!(out.contains("VS1;"), "missing VS1 from slow cut: {out}");
    }

    #[test]
    fn p9ji_feedrate_zero_is_noop() {
        // rate=0 means "don't care" — leaves prior VS intact.
        let mut post = Post::new();
        post.feedrate(0);
        let out = post.finish();
        assert!(
            !out.contains("VS"),
            "rate=0 should not emit VS; got: {out}",
        );
    }

    #[test]
    fn hpgl_quarter_arc_renders_as_curve() {
        // 1pcz: A quarter-arc from (10, 0) to (0, 10) must draw as a
        // tessellated curve, not a single diagonal chord.
        let mut post = Post::new();
        post.program_start();
        post.move_to(Some(10.0), Some(0.0), None);
        // CCW arc: I=-10, J=0 → center (0,0). Endpoint (0,10).
        post.arc_ccw(Some(0.0), Some(10.0), None, Some(-10.0), Some(0.0));
        let out = post.finish();
        let pa_count = out.matches("PA").count();
        // 90° at 5° per chord = 18 chords, with min 8.
        assert!(
            pa_count >= 8,
            "quarter-arc should tessellate to ≥8 chords (got {pa_count}): {out}",
        );
        // Sanity: at least one waypoint near (7.07, 7.07) — i.e. the
        // 45° midpoint of the arc — should be present. PA units are
        // mm × 40 → ~283.
        let approx_45 = "PA283,283;";
        let approx_45_alt = "PA282,283;";
        let approx_45_alt2 = "PA283,282;";
        assert!(
            out.contains(approx_45)
                || out.contains(approx_45_alt)
                || out.contains(approx_45_alt2),
            "expected a 45° midpoint waypoint near PA283,283; in: {out}",
        );
    }
}
