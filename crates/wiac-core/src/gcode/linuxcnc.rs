//! `LinuxCNC` post-processor. Mirrors `output_plugins/gcode_linuxcnc.py`.

// # CAM/sim pedantic-lint exemptions
// LinuxCNC post emits `X`, `Y`, `Z`, `I`, `J`, `F`, `S` — the machine-control
// short names map 1:1 to gcode-word letters. The ToolOffset match enumerates
// every variant (G40/G41/G42) explicitly to mirror the gcode spec.
#![allow(clippy::many_single_char_names, clippy::match_same_arms)]

use crate::cam::setup::{ToolOffset, UnitSystem};
use crate::gcode::post_profile::{template_lines, AxisFormat, PostProfile, TokenCtx};
use crate::gcode::{
    configure_post_state, fmt_num, line_number_prefix, CapturedPostState, PostProcessor, PostState,
};

#[derive(Debug, Default)]
pub struct Post {
    /// Internal state — exposed `pub(crate)` so the GRBL post can
    /// check `state.profile` to decide whether to delegate to
    /// `LinuxCNC`'s template-driven `program_start` / _end / tool path.
    pub(crate) state: PostState,
    out: Vec<String>,
}

impl Post {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn write(&mut self, line: impl Into<String>) {
        let raw: String = line.into();
        let prefix = line_number_prefix(&mut self.state);
        if prefix.is_empty() {
            self.out.push(raw);
        } else {
            self.out.push(format!("{prefix}{raw}"));
        }
    }

    fn fmt(&self, v: f64) -> String {
        fmt_num(v, self.state.decimal_separator)
    }

    /// Same as `fmt` but converts mm → emit units (w9hd). Used for
    /// every value that represents a length / position in pipeline
    /// (mm) coordinates — X/Y/Z/I/J/R/Q + machine_offsets + Z-shift.
    /// Coordinates already in emit units (e.g. dwell seconds) keep
    /// using `fmt` directly.
    fn fmt_len(&self, v_mm: f64) -> String {
        fmt_num(v_mm * self.state.unit_scale, self.state.decimal_separator)
    }

    /// Format a single axis word. Consults the profile's per-axis
    /// config (hev) when set: disabled axes return None so the caller
    /// drops the word; renamed / reformatted / scaled axes are rendered
    /// per the user's spec, with the configured decimal separator
    /// applied last so `,`-locales keep working.
    ///
    /// w9hd: the input `v` is in mm (pipeline units). When
    /// `state.unit_scale != 1.0` (Inch project), the multiplication
    /// happens here at the boundary so the number that lands in the
    /// gcode text matches the G20 pragma.
    fn fmt_axis(&self, default: char, v: f64) -> Option<String> {
        let af = self
            .state
            .profile
            .as_ref()
            .and_then(|p| p.axes.as_ref())
            .map(|a| axis_for(default, a));
        let v_emit = v * self.state.unit_scale;
        let rendered = match af {
            Some(af) => af.render(v_emit)?,
            None => format!("{default}{}", fmt_num(v_emit, self.state.decimal_separator)),
        };
        if self.state.decimal_separator == '.' {
            Some(rendered)
        } else {
            Some(rendered.replace('.', &self.state.decimal_separator.to_string()))
        }
    }

    fn maybe(&self, coord: char, prev: Option<f64>, val: Option<f64>) -> Option<String> {
        let v = val?;
        if let Some(p) = prev {
            if (p - v).abs() < 1e-9 {
                return None;
            }
        }
        self.fmt_axis(coord, v)
    }

    /// Spindle-speed word (`S<rpm>`) with a leading space when emitted.
    /// Returns "" when the speed axis is disabled so callers can just
    /// concatenate it onto `M3` / `M4`.
    fn render_speed(&self, speed: u32) -> String {
        let af = self
            .state
            .profile
            .as_ref()
            .and_then(|p| p.axes.as_ref())
            .map(|a| a.speed.clone());
        match af {
            Some(af) => af
                .render(f64::from(speed))
                .map_or(String::new(), |s| format!(" {s}")),
            None => format!(" S{speed}"),
        }
    }

    /// Emit a single G2/G3 line (no full-circle splitting). The
    /// public `arc_cw` / `arc_ccw` wrap this with the split-detection
    /// for 3p7v.
    fn emit_arc_raw(
        &mut self,
        g: &str,
        x: Option<f64>,
        y: Option<f64>,
        z: Option<f64>,
        i: Option<f64>,
        j: Option<f64>,
    ) {
        let body = self.coords(x, y, z);
        let i = i
            .and_then(|v| self.fmt_axis('I', v))
            .map(|s| format!(" {s}"))
            .unwrap_or_default();
        let j = j
            .and_then(|v| self.fmt_axis('J', v))
            .map(|s| format!(" {s}"))
            .unwrap_or_default();
        self.write(format!("{g} {body}{i}{j}").trim().to_string());
    }

    fn coords(&mut self, x: Option<f64>, y: Option<f64>, z: Option<f64>) -> String {
        let last_x = self.state.last_x;
        let last_y = self.state.last_y;
        let last_z = self.state.last_z;
        let mut parts = Vec::with_capacity(3);
        if let Some(s) = self.maybe('X', last_x, x) {
            parts.push(s);
        }
        if let Some(s) = self.maybe('Y', last_y, y) {
            parts.push(s);
        }
        if let Some(s) = self.maybe('Z', last_z, z) {
            parts.push(s);
        }
        if let Some(v) = x {
            self.state.last_x = Some(v);
        }
        if let Some(v) = y {
            self.state.last_y = Some(v);
        }
        if let Some(v) = z {
            self.state.last_z = Some(v);
        }
        parts.join(" ")
    }
}

/// 3p7v: detect a full-circle arc (start XY ≈ target XY, with a
/// non-trivial I/J vector to the center) and return the midpoint XY
/// (diametrically opposite the start across the center) so the
/// caller can split the arc into two halves. Returns None when the
/// arc is a normal partial sweep, or when start / target / center
/// aren't well-defined (no prior position, missing target / I / J,
/// or a degenerate zero-radius circle).
fn full_circle_midpoint(
    state: &PostState,
    x: Option<f64>,
    y: Option<f64>,
    i: Option<f64>,
    j: Option<f64>,
) -> Option<(f64, f64)> {
    let last_x = state.last_x?;
    let last_y = state.last_y?;
    // Target XY: explicit value or "same as previous" (modal). When
    // both are missing the arc is degenerate; treat as not a circle.
    let target_x = x.unwrap_or(last_x);
    let target_y = y.unwrap_or(last_y);
    let i = i?;
    let j = j?;
    // Same-point check: start XY ≈ target XY within a generous
    // tolerance (1e-6 mm — well below CAM precision). Compared
    // squared to avoid a hypot call.
    const EPS: f64 = 1e-6;
    let dx = target_x - last_x;
    let dy = target_y - last_y;
    if dx * dx + dy * dy > EPS * EPS {
        return None;
    }
    // Trivial I/J (no offset to center) → degenerate "arc" with
    // zero radius; nothing to split. The post would emit a
    // syntactically valid but geometrically meaningless line anyway.
    if i * i + j * j < EPS * EPS {
        return None;
    }
    // Midpoint: start + 2·(I, J) lands diametrically opposite on the
    // circle so each half-arc sweeps a clean 180°.
    Some((last_x + 2.0 * i, last_y + 2.0 * j))
}

/// Pick the matching `AxisFormat` from a profile's `AxesConfig` given
/// the default axis letter the post wants to emit. Returns a clone
/// because the call sites hold `&self.state` and we need to release
/// it before calling `self.fmt(v)`.
fn axis_for(letter: char, axes: &crate::gcode::post_profile::AxesConfig) -> AxisFormat {
    match letter {
        'X' => axes.x.clone(),
        'Y' => axes.y.clone(),
        'Z' => axes.z.clone(),
        'I' => axes.i.clone(),
        'J' => axes.j.clone(),
        _ => AxisFormat::coord(&letter.to_string()),
    }
}

impl PostProcessor for Post {
    fn separation(&mut self) {
        self.write("");
    }
    fn raw(&mut self, cmd: &str) {
        self.write(cmd);
    }
    fn comment(&mut self, text: &str) {
        self.write(format!("({text})"));
    }
    fn unit(&mut self, unit: UnitSystem) {
        self.write(match unit {
            UnitSystem::Mm => "G21",
            UnitSystem::Inch => "G20",
        });
    }
    fn absolute(&mut self, active: bool) {
        if active {
            self.state.absolute = true;
            self.write("G90");
        } else {
            self.state.absolute = false;
            self.write("G91");
        }
    }
    fn feedrate(&mut self, rate: u32) {
        if self.state.last_rate != Some(rate) {
            // w9hd: feedrate is mm/min in pipeline; emit-units = mm/min × unit_scale.
            // For Inch projects that's in/min — matches G20 mode (controllers
            // interpret F in the current unit system).
            let rate_emit = f64::from(rate) * self.state.unit_scale;
            let af = self
                .state
                .profile
                .as_ref()
                .and_then(|p| p.axes.as_ref())
                .map(|a| a.feed.clone());
            match af {
                Some(af) => {
                    if let Some(s) = af.render(rate_emit) {
                        self.write(s);
                    }
                }
                None => {
                    // Preserve integer F<rate> in the default-mm case; switch to
                    // a fractional render only when the scale actually applies,
                    // so the linuxcnc/grbl golden snapshots in mm don't drift.
                    if (self.state.unit_scale - 1.0).abs() < 1e-12 {
                        self.write(format!("F{rate}"));
                    } else {
                        self.write(format!(
                            "F{}",
                            fmt_num(rate_emit, self.state.decimal_separator)
                        ));
                    }
                }
            }
            self.state.last_rate = Some(rate);
        }
    }
    fn program_start(&mut self) {
        if let Some(template) = self
            .state
            .profile
            .as_ref()
            .and_then(|p| p.program_start.clone())
        {
            for line in template_lines(&template, &self.state.token_ctx) {
                self.write(line);
            }
        } else {
            self.write("(generated by wiaConstructor)");
        }
    }
    fn program_end(&mut self) {
        if let Some(template) = self
            .state
            .profile
            .as_ref()
            .and_then(|p| p.program_end.clone())
        {
            for line in template_lines(&template, &self.state.token_ctx) {
                self.write(line);
            }
        } else {
            self.write("M30");
        }
    }
    fn tool(&mut self, n: u32) {
        // Refresh tool-number token before rendering so the template
        // sees the FUTURE tool's number even mid-program.
        self.state.token_ctx.tool_number = n;
        if let Some(template) = self
            .state
            .profile
            .as_ref()
            .and_then(|p| p.tool_change.clone())
        {
            for line in template_lines(&template, &self.state.token_ctx) {
                self.write(line);
            }
        } else {
            self.write(format!("T{n} M6"));
        }
    }
    fn tool_offsets(&mut self, offset: ToolOffset) {
        match offset {
            ToolOffset::None => self.write("G40"),
            ToolOffset::Inside => self.write("G42"),
            ToolOffset::Outside => self.write("G41"),
            ToolOffset::On => self.write("G40"),
        }
    }
    fn machine_offsets(&mut self, offsets: (f64, f64, f64), _soft: bool) {
        self.write(format!(
            "G92 X{} Y{} Z{}",
            self.fmt_len(offsets.0),
            self.fmt_len(offsets.1),
            self.fmt_len(offsets.2)
        ));
    }
    fn coolant_mist(&mut self) {
        if let Some(template) = self
            .state
            .profile
            .as_ref()
            .and_then(|p| p.coolant_mist_on.clone())
        {
            for line in template_lines(&template, &self.state.token_ctx) {
                self.write(line);
            }
        } else {
            self.write("M7");
        }
    }
    fn coolant_flood(&mut self) {
        if let Some(template) = self
            .state
            .profile
            .as_ref()
            .and_then(|p| p.coolant_flood_on.clone())
        {
            for line in template_lines(&template, &self.state.token_ctx) {
                self.write(line);
            }
        } else {
            self.write("M8");
        }
    }
    fn coolant_off(&mut self) {
        // Pick the off template based on which coolant variant we
        // think is still on (state.last_speed is unreliable here).
        // For v1: prefer flood-off if either is set, else mist-off.
        let off_template = self.state.profile.as_ref().and_then(|p| {
            p.coolant_flood_off
                .clone()
                .or_else(|| p.coolant_mist_off.clone())
        });
        if let Some(template) = off_template {
            for line in template_lines(&template, &self.state.token_ctx) {
                self.write(line);
            }
        } else {
            self.write("M9");
        }
    }
    fn spindle_off(&mut self) {
        self.write("M5");
        self.state.last_speed = None;
    }
    fn spindle_cw(&mut self, speed: u32, pause: u32) {
        if self.state.last_speed != Some(speed) {
            let rendered = self.render_speed(speed);
            self.write(format!("M3{rendered}"));
            self.state.last_speed = Some(speed);
            if pause > 0 {
                self.write(format!("G4 P{pause}"));
            }
        }
    }
    fn spindle_ccw(&mut self, speed: u32, pause: u32) {
        if self.state.last_speed != Some(speed) {
            let rendered = self.render_speed(speed);
            self.write(format!("M4{rendered}"));
            self.state.last_speed = Some(speed);
            if pause > 0 {
                self.write(format!("G4 P{pause}"));
            }
        }
    }
    fn move_to(&mut self, x: Option<f64>, y: Option<f64>, z: Option<f64>) {
        let body = self.coords(x, y, z);
        if !body.is_empty() {
            self.write(format!("G0 {body}"));
        }
    }
    fn linear(&mut self, x: Option<f64>, y: Option<f64>, z: Option<f64>) {
        let body = self.coords(x, y, z);
        if !body.is_empty() {
            self.write(format!("G1 {body}"));
        }
    }
    fn arc_cw(
        &mut self,
        x: Option<f64>,
        y: Option<f64>,
        z: Option<f64>,
        i: Option<f64>,
        j: Option<f64>,
    ) {
        if let Some((mid_x, mid_y)) = full_circle_midpoint(&self.state, x, y, i, j) {
            // 3p7v: split a start==end arc into two halves around the
            // shared center. GRBL rejects full-circles outright
            // (error:33); LinuxCNC accepts them in some configs but
            // splitting is universally safe.
            self.emit_arc_raw("G2", Some(mid_x), Some(mid_y), z, i, j);
            // I/J for the second half are the vector from the midpoint
            // to the same center — that's the negation of the first
            // half's I/J because the midpoint is diametrically opposite.
            self.emit_arc_raw("G2", x, y, z, i.map(|v| -v), j.map(|v| -v));
            return;
        }
        self.emit_arc_raw("G2", x, y, z, i, j);
    }
    fn arc_ccw(
        &mut self,
        x: Option<f64>,
        y: Option<f64>,
        z: Option<f64>,
        i: Option<f64>,
        j: Option<f64>,
    ) {
        if let Some((mid_x, mid_y)) = full_circle_midpoint(&self.state, x, y, i, j) {
            self.emit_arc_raw("G3", Some(mid_x), Some(mid_y), z, i, j);
            self.emit_arc_raw("G3", x, y, z, i.map(|v| -v), j.map(|v| -v));
            return;
        }
        self.emit_arc_raw("G3", x, y, z, i, j);
    }
    fn drill_simple(&mut self, x: f64, y: f64, z: f64, r: f64, dwell_sec: f64) {
        // LinuxCNC G81 / G82 (G82 is the dwell variant). Use G82 when dwell > 0,
        // G81 otherwise, so machinists who watch the canned cycle code see what
        // they expect.
        // w9hd: R is a length (retract plane in pipeline mm) — `fmt_len`
        // applies the inch scale. Dwell stays in seconds (no unit conversion).
        let dwell = if dwell_sec > 0.0 {
            format!(" P{}", self.fmt(dwell_sec))
        } else {
            String::new()
        };
        let g = if dwell_sec > 0.0 { "G82" } else { "G81" };
        let body = self.coords(Some(x), Some(y), Some(z));
        self.write(format!("{g} {body} R{}{dwell}", self.fmt_len(r)));
        // Canned cycles leave Z at R after each cycle, but the post state
        // already records Z=z above. Sync it so subsequent moves don't
        // emit redundant Z words.
        self.state.last_z = Some(r);
    }
    fn drill_peck(&mut self, x: f64, y: f64, z: f64, r: f64, q: f64, dwell_sec: f64) {
        // w9hd: R + Q (peck step) are lengths — scale via `fmt_len`.
        let dwell = if dwell_sec > 0.0 {
            format!(" P{}", self.fmt(dwell_sec))
        } else {
            String::new()
        };
        let body = self.coords(Some(x), Some(y), Some(z));
        self.write(format!(
            "G83 {body} R{} Q{}{dwell}",
            self.fmt_len(r),
            self.fmt_len(q.abs())
        ));
        self.state.last_z = Some(r);
    }
    fn drill_chip_break(&mut self, x: f64, y: f64, z: f64, r: f64, q: f64, dwell_sec: f64) {
        // w9hd: R + Q (peck step) are lengths — scale via `fmt_len`.
        let dwell = if dwell_sec > 0.0 {
            format!(" P{}", self.fmt(dwell_sec))
        } else {
            String::new()
        };
        let body = self.coords(Some(x), Some(y), Some(z));
        self.write(format!(
            "G73 {body} R{} Q{}{dwell}",
            self.fmt_len(r),
            self.fmt_len(q.abs())
        ));
        self.state.last_z = Some(r);
    }
    fn finish(&self) -> String {
        self.out.join("\n") + "\n"
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
        self.state.last_x = None;
        self.state.last_y = None;
        self.state.last_z = None;
        self.state.last_rate = None;
        self.state.last_speed = None;
    }
    fn capture_state(&self) -> CapturedPostState {
        CapturedPostState {
            last_x: self.state.last_x,
            last_y: self.state.last_y,
            last_z: self.state.last_z,
            last_rate: self.state.last_rate,
            last_speed: self.state.last_speed,
        }
    }
    fn restore_state(&mut self, s: &CapturedPostState) {
        self.state.last_x = s.last_x;
        self.state.last_y = s.last_y;
        self.state.last_z = s.last_z;
        self.state.last_rate = s.last_rate;
        self.state.last_speed = s.last_speed;
    }
    fn configure(
        &mut self,
        decimal_separator: char,
        line_number_start: Option<u32>,
        unit: UnitSystem,
    ) {
        configure_post_state(&mut self.state, decimal_separator, line_number_start, unit);
    }
    fn tool_z_shift(&mut self, shift_mm: f64) {
        if shift_mm.abs() < 1e-9 {
            return;
        }
        // G92 sets the work-coordinate offset; here we pin the work Z
        // to the configured shift so the new tool's tip lines up with
        // the reference tool's Z=0. The `;` comment is bracketed so
        // grep'ing for the offset is easy in CAM-review.
        // w9hd: shift comes in as mm; emit in machine units (inch on G20).
        let s = self.fmt_len(shift_mm);
        self.write(format!("(z-shift: {s})"));
        self.write(format!("G92 Z{s}"));
        // The G92 leaves the controller's internal "last_z" unknown
        // to our delta encoder — flushing it forces the next Z move
        // to re-emit explicitly.
        self.state.last_z = None;
    }
    fn dwell(&mut self, seconds: f64) {
        if seconds <= 0.0 {
            return;
        }
        let s = self.fmt(seconds);
        self.write(format!("G4 P{s}"));
    }
    fn plane_xy(&mut self) {
        self.write("G17");
    }
    fn cutter_comp_off(&mut self) {
        self.write("G40");
    }
    fn feed_per_minute(&mut self) {
        self.write("G94");
    }
    fn cancel_canned_cycle(&mut self) {
        self.write("G80");
        // G80 cancels any canned cycle. The drill cycles set
        // `last_z = r` so subsequent moves know where the head is —
        // that's still accurate after G80, so we don't touch state.
    }
    fn set_post_profile(&mut self, profile: Option<&PostProfile>) {
        self.state.profile = profile.cloned();
    }
    fn set_token_ctx(&mut self, ctx: &TokenCtx) {
        self.state.token_ctx = ctx.clone();
    }
}
