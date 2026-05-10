//! HPGL post-processor — pen-up/pen-down style for plotters and drag knives.
//! Mirrors output_plugins/hpgl.py.

use crate::cam::setup::{ToolOffset, UnitSystem};
use crate::gcode::{CapturedPostState, PostProcessor};

#[derive(Debug, Default)]
pub struct Post {
    out: Vec<String>,
    pen_down: bool,
    last_x: Option<i64>,
    last_y: Option<i64>,
}

impl Post {
    pub fn new() -> Self {
        Self::default()
    }

    fn fmt_xy(x: f64, y: f64) -> (i64, i64) {
        ((x * 40.0).round() as i64, (y * 40.0).round() as i64)
    }

    fn write(&mut self, s: impl Into<String>) {
        self.out.push(s.into());
    }
}

impl PostProcessor for Post {
    fn unit(&mut self, _unit: UnitSystem) {
        // HPGL is plotter-units (40 per mm); units handled by fmt_xy.
    }
    fn feedrate(&mut self, _rate: u32) {}
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
        _i: Option<f64>,
        _j: Option<f64>,
    ) {
        // Linearize — HPGL has AA (arc absolute) but we keep this simple.
        self.linear(x, y, None);
    }
    fn arc_ccw(
        &mut self,
        x: Option<f64>,
        y: Option<f64>,
        _z: Option<f64>,
        _i: Option<f64>,
        _j: Option<f64>,
    ) {
        self.linear(x, y, None);
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
