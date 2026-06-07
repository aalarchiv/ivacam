//! `LinuxCNC` post-processor. Mirrors `output_plugins/gcode_linuxcnc.py`.

// # CAM/sim pedantic-lint exemptions
// LinuxCNC post emits `X`, `Y`, `Z`, `I`, `J`, `F`, `S` — the machine-control
// short names map 1:1 to gcode-word letters. The ToolOffset match enumerates
// every variant (G40/G41/G42) explicitly to mirror the gcode spec.
#![allow(clippy::many_single_char_names, clippy::match_same_arms)]

use crate::gcode::post_profile::{template_lines, AxisFormat, PostProfile, TokenCtx};
use crate::gcode::{
    configure_post_state, fmt_num_dp, line_number_prefix, CapturedPostState, CoolantState,
    PostProcessor, PostState,
};
use crate::project::tool::SpindleDirection;
use crate::project::{ToolOffset, UnitSystem};

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
        fmt_num_dp(v, self.state.decimal_separator, self.state.decimals())
    }

    /// nxn0: render a dwell value in the active post's time unit.
    /// Pipeline always passes seconds; LinuxCNC/Smoothie keep them as
    /// seconds, Mach3/Mach4/Centroid (and any profile that opts in)
    /// emit milliseconds = seconds * 1000. Integer-rendered when the
    /// scaled value lands on an integer so the typical "500 ms"
    /// doesn't read "500.0000" on the line.
    fn fmt_dwell_p(&self, seconds: f64) -> String {
        use crate::gcode::post_profile::DwellUnit;
        let unit = self
            .state
            .profile
            .as_ref()
            .and_then(|p| p.dwell_unit)
            .unwrap_or(DwellUnit::Seconds);
        let v = match unit {
            DwellUnit::Seconds => seconds,
            DwellUnit::Milliseconds => seconds * 1000.0,
        };
        // Integer-friendly render when the value is whole; matches
        // the way most posts emit "P500" rather than "P500.0000".
        if (v.round() - v).abs() < 1e-9 && unit == DwellUnit::Milliseconds {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let n = v.round() as i64;
            return n.to_string();
        }
        self.fmt(v)
    }

    /// Same as `fmt` but converts mm → emit units (w9hd). Used for
    /// every value that represents a length / position in pipeline
    /// (mm) coordinates — X/Y/Z/I/J/R/Q + `machine_offsets` + Z-shift.
    /// Coordinates already in emit units (e.g. dwell seconds) keep
    /// using `fmt` directly.
    fn fmt_len(&self, v_mm: f64) -> String {
        fmt_num_dp(
            v_mm * self.state.unit_scale,
            self.state.decimal_separator,
            self.state.decimals(),
        )
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
            None => format!(
                "{default}{}",
                fmt_num_dp(v_emit, self.state.decimal_separator, self.state.decimals())
            ),
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
// juvx: local `const EPS` lives near its use; hoisting would split
// the same-point-check block that documents the tolerance choice.
#[allow(clippy::items_after_statements)]
fn full_circle_midpoint(
    state: &PostState,
    x: Option<f64>,
    y: Option<f64>,
    i: Option<f64>,
    j: Option<f64>,
) -> Option<(f64, f64)> {
    // 1e-6 mm — well below CAM precision; squared to avoid a hypot
    // call below.
    const EPS_SQ: f64 = 1e-6 * 1e-6;
    let last_x = state.last_x?;
    let last_y = state.last_y?;
    // Target XY: explicit value or "same as previous" (modal). When
    // both are missing the arc is degenerate; treat as not a circle.
    let target_x = x.unwrap_or(last_x);
    let target_y = y.unwrap_or(last_y);
    let i = i?;
    let j = j?;
    // Same-point check: start XY ≈ target XY within tolerance.
    let dx = target_x - last_x;
    let dy = target_y - last_y;
    if dx * dx + dy * dy > EPS_SQ {
        return None;
    }
    // Trivial I/J (no offset to center) → degenerate "arc" with
    // zero radius; nothing to split. The post would emit a
    // syntactically valid but geometrically meaningless line anyway.
    if i * i + j * j < EPS_SQ {
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
///
/// r164: I/J offsets are tied to the X/Y plane respectively. When the
/// user renames X (e.g. X → A for a rotary-as-linear setup) and leaves
/// the I axis at its built-in default name "I", auto-track the X
/// rename so the offset letter follows the coordinate letter — most
/// controllers expect them consistent. The user can still override the
/// I/J name explicitly to opt out of the auto-track.
fn axis_for(letter: char, axes: &crate::gcode::post_profile::AxesConfig) -> AxisFormat {
    match letter {
        'X' => axes.x.clone(),
        'Y' => axes.y.clone(),
        'Z' => axes.z.clone(),
        'I' => {
            let mut af = axes.i.clone();
            if af.name == "I" && axes.x.name != "X" {
                af.name.clone_from(&axes.x.name);
            }
            af
        }
        'J' => {
            let mut af = axes.j.clone();
            if af.name == "J" && axes.y.name != "Y" {
                af.name.clone_from(&axes.y.name);
            }
            af
        }
        _ => AxisFormat::coord(&letter.to_string()),
    }
}

impl PostProcessor for Post {
    fn fmt_dwell_post(&self, seconds: f64) -> String {
        // pxyt: honour the active profile's dwell_unit when the
        // trait-default drill_simple / drill_peck / drill_chip_break
        // dispatch through here. LinuxCNC's own drill overrides go
        // through `fmt_dwell_p` directly; this method exists so a
        // GRBL or HPGL post inheriting the trait default still gets
        // ms-rendered dwell when the user picked a Mach3-style profile.
        self.fmt_dwell_p(seconds)
    }
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
        // 4nj6: never emit F0. LinuxCNC raises "negative or zero feed
        // rate" and halts; GRBL returns error:11. A default-constructed
        // or misconfigured tool with rate_v=0 or rate_h=0 can reach
        // this path even when pipeline validation tries to catch it.
        // Skip and leave the modal F at whatever the prior cut set —
        // worse than a perfect F-anchor, but better than killing the
        // program at the first G1. The upstream pipeline warning
        // (`zero_feed_rate_attempt`) lets the user fix the root cause.
        if rate == 0 {
            return;
        }
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
                            fmt_num_dp(
                                rate_emit,
                                self.state.decimal_separator,
                                self.state.decimals(),
                            )
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
            self.write("(generated by ivaCAM)");
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
        // f78z: dedupe — the per-offset cut-block calls coolant_mist
        // unconditionally before each cut, but the controller only
        // wants the M7 on a state CHANGE. Suppress when we already
        // commanded Mist.
        if self.state.last_coolant == CoolantState::Mist {
            return;
        }
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
        self.state.last_coolant = CoolantState::Mist;
    }
    fn coolant_flood(&mut self) {
        // f78z: same dedupe as coolant_mist — only emit M8 on a state
        // change.
        if self.state.last_coolant == CoolantState::Flood {
            return;
        }
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
        self.state.last_coolant = CoolantState::Flood;
    }
    fn coolant_off(&mut self) {
        // f78z: skip when coolant is already off. The Unknown initial
        // state still emits — a defensive M9 at program-end ensures
        // the spindle/pump shuts down even if no on-line was emitted
        // (e.g. tool with coolant=Off followed by an explicit
        // shutdown).
        if self.state.last_coolant == CoolantState::Off {
            return;
        }
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
        self.state.last_coolant = CoolantState::Off;
    }
    fn spindle_off(&mut self) {
        self.write("M5");
        self.state.last_speed = None;
        // sulg: clear the tracked direction so the next spindle_on
        // (M3 / M4) re-asserts it explicitly — matches the cache
        // semantics in CapturedPostState.last_spindle_dir.
        self.state.last_spindle_dir = None;
    }
    fn spindle_cw(&mut self, speed: u32, pause: u32) {
        // sulg: re-emit M3 when EITHER the speed changed OR the
        // direction differs from what's tracked. Otherwise a prior
        // Ccw op (M4) followed by a same-speed Cw op would silently
        // leave the spindle running backward.
        let need_emit = self.state.last_speed != Some(speed)
            || self.state.last_spindle_dir != Some(SpindleDirection::Cw);
        if need_emit {
            let rendered = self.render_speed(speed);
            self.write(format!("M3{rendered}"));
            self.state.last_speed = Some(speed);
            self.state.last_spindle_dir = Some(SpindleDirection::Cw);
            if pause > 0 {
                self.write(format!("G4 P{pause}"));
            }
        }
    }
    fn spindle_ccw(&mut self, speed: u32, pause: u32) {
        // sulg: same direction-aware dedupe as spindle_cw.
        let need_emit = self.state.last_speed != Some(speed)
            || self.state.last_spindle_dir != Some(SpindleDirection::Ccw);
        if need_emit {
            let rendered = self.render_speed(speed);
            self.write(format!("M4{rendered}"));
            self.state.last_speed = Some(speed);
            self.state.last_spindle_dir = Some(SpindleDirection::Ccw);
            if pause > 0 {
                self.write(format!("G4 P{pause}"));
            }
        }
    }
    fn laser_on(&mut self, power: u32) {
        // 20y5: fire the laser at the configured power. Emit `M3 S<power>`
        // — `M3` matches what Lightburn / T2Laser / Estlcam laser emit
        // by default. GRBL's dynamic-laser `M4` is an alternative, but
        // `M3` works in both GRBL `$32=1` (laser-mode) and standard
        // mill mode, so it's the portable choice. Skip emission when
        // the post's last_speed already matches — the cut block re-arms
        // the laser around every rapid traverse, but consecutive arms
        // at the same power are no-ops.
        if self.state.last_speed != Some(power) {
            let rendered = self.render_speed(power);
            // z9zh: GRBL dynamic-power mode fires with M4 (S ramps with
            // feed); default M3 elsewhere (portable; LinuxCNC M4 is
            // spindle-CCW, so only the GRBL post sets `laser_dynamic`).
            let m = if self.state.laser_dynamic { "M4" } else { "M3" };
            self.write(format!("{m}{rendered}"));
            self.state.last_speed = Some(power);
        }
    }
    fn laser_arm(&mut self) {
        // xkvv: arm the laser at zero power before the rapid traverse.
        // Same M3 modal as `laser_on` but S0 — the controller carries
        // the laser-on state through the rapid (so spindle-bound axes
        // and `$32=1` GRBL modes don't fight) while no power means no
        // burn. The pierce-time `laser_on(power)` re-emits S<power>
        // since `last_speed` is now Some(0).
        if self.state.last_speed != Some(0) {
            let rendered = self.render_speed(0);
            let m = if self.state.laser_dynamic { "M4" } else { "M3" };
            self.write(format!("{m}{rendered}"));
            self.state.last_speed = Some(0);
        }
    }
    fn laser_off(&mut self) {
        // 20y5: emit M5 to drop the beam before rapid traversal.
        // Clear last_speed so the next laser_on re-emits M3 S<power>
        // — otherwise the delta-encoded state would suppress the
        // re-arm and the beam would stay off through subsequent cuts.
        self.write("M5");
        self.state.last_speed = None;
    }
    fn move_to(&mut self, x: Option<f64>, y: Option<f64>, z: Option<f64>) {
        let body = self.coords(x, y, z);
        if !body.is_empty() {
            self.write(format!("G0 {body}"));
        }
    }
    fn rapid_machine_xy(&mut self, x_mm: f64, y_mm: f64) {
        // ad0v: machine-coords rapid to the tool-change station. Build
        // the X/Y words through `fmt_axis` so they honor the configured
        // decimal separator, inch scale, and any per-axis profile
        // rename/disable — exactly like a normal rapid. Unlike `coords`,
        // there is NO delta suppression: a G53 line always restates both
        // axes (the previous WCS position says nothing about the machine
        // position we're commanding).
        let mut body = String::from("G53 G0");
        let mut emitted = false;
        if let Some(w) = self.fmt_axis('X', x_mm) {
            body.push(' ');
            body.push_str(&w);
            emitted = true;
        }
        if let Some(w) = self.fmt_axis('Y', y_mm) {
            body.push(' ');
            body.push_str(&w);
            emitted = true;
        }
        if emitted {
            self.write(body);
        }
        // The head now sits at a MACHINE XY we can't express in the
        // active WCS. Drop the tracked WCS position so the next motion
        // re-emits X/Y/Z explicitly instead of suppressing an axis
        // against this now-meaningless snapshot. Leave rate / spindle /
        // coolant modal state alone — a reposition doesn't change them.
        self.state.last_x = None;
        self.state.last_y = None;
        self.state.last_z = None;
    }
    fn rapid_machine_z(&mut self, z_mm: f64) {
        // hat3: machine-coords Z rapid (G53 G0 Z<z>) — the safe approach
        // height above a fixed sensor. Same fmt_axis path + position-
        // cache invalidation as `rapid_machine_xy`.
        if let Some(w) = self.fmt_axis('Z', z_mm) {
            self.write(format!("G53 G0 {w}"));
        }
        self.state.last_x = None;
        self.state.last_y = None;
        self.state.last_z = None;
    }
    fn probe_toward_z(&mut self, distance_mm: f64, feed_mm_min: u32) {
        // hat3: probing-feed move; controller halts at the trigger.
        // w9hd: distance is a length (mm) — fmt_len applies the inch
        // scale. Feed stays integer mm/min, matching the Probe op.
        let d = self.fmt_len(distance_mm);
        self.write(format!("G38.2 Z{d} F{feed_mm_min}"));
        // The head stops at an unknown trigger Z — flush the position
        // cache so the next move re-emits X/Y/Z explicitly.
        self.state.last_x = None;
        self.state.last_y = None;
        self.state.last_z = None;
    }
    fn apply_probed_tool_length(&mut self) {
        // hat3: LinuxCNC stores the probed Z in #5063. G43.1 applies it
        // as a dynamic tool-length offset; combined with the reference
        // tool's WCS Z0 this re-establishes the new tool's tip. The
        // difference math is the controller's — we just wire the
        // measured value in.
        self.write("G43.1 Z[#5063]");
        self.state.last_z = None;
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
    fn drill_simple(&mut self, x: f64, y: f64, z: f64, r: f64, _rate_v: u32, dwell_sec: f64) {
        // LinuxCNC G81 / G82 (G82 is the dwell variant). Use G82 when dwell > 0,
        // G81 otherwise, so machinists who watch the canned cycle code see what
        // they expect.
        // w9hd: R is a length (retract plane in pipeline mm) — `fmt_len`
        // applies the inch scale.
        // nxn0: Dwell is in seconds at the pipeline boundary; the post
        // converts to milliseconds when the active profile asks for it
        // (Mach3/Mach4/Centroid). LinuxCNC default keeps seconds.
        let dwell = if dwell_sec > 0.0 {
            format!(" P{}", self.fmt_dwell_p(dwell_sec))
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
    fn drill_peck(&mut self, x: f64, y: f64, z: f64, r: f64, q: f64, _rate_v: u32, dwell_sec: f64) {
        // w9hd: R + Q (peck step) are lengths — scale via `fmt_len`.
        // nxn0: P (dwell) follows the profile's dwell_unit.
        let dwell = if dwell_sec > 0.0 {
            format!(" P{}", self.fmt_dwell_p(dwell_sec))
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
    fn drill_chip_break(
        &mut self,
        x: f64,
        y: f64,
        z: f64,
        r: f64,
        q: f64,
        _rate_v: u32,
        dwell_sec: f64,
    ) {
        // w9hd: R + Q (peck step) are lengths — scale via `fmt_len`.
        // nxn0: P (dwell) follows the profile's dwell_unit.
        let dwell = if dwell_sec > 0.0 {
            format!(" P{}", self.fmt_dwell_p(dwell_sec))
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
        // sulg: a reset forces the next motion to re-emit X/Y/Z/F/S
        // explicitly. The same applies to the spindle direction —
        // the pipeline's Pause handler at pipeline.rs:748 relies on
        // this so the next op's spindle_on (whether M3 or M4)
        // re-fires. Coolant is intentionally NOT cleared here: M0
        // doesn't shut off coolant, and forcing a re-emit would
        // double-print M7/M8 mid-program.
        self.state.last_spindle_dir = None;
    }
    fn capture_state(&self) -> CapturedPostState {
        CapturedPostState {
            last_x: self.state.last_x,
            last_y: self.state.last_y,
            last_z: self.state.last_z,
            last_rate: self.state.last_rate,
            last_speed: self.state.last_speed,
            // sulg: ferry the live coolant + spindle-direction modal
            // state across the cache boundary. Without this, a
            // cached op N+1 would replay against a stale "Unknown"
            // initial state and either re-emit a redundant M7/M8/M3
            // or — worse — skip a state change the live emitter
            // would have made.
            last_coolant: self.state.last_coolant,
            last_spindle_dir: self.state.last_spindle_dir,
        }
    }
    fn restore_state(&mut self, s: &CapturedPostState) {
        self.state.last_x = s.last_x;
        self.state.last_y = s.last_y;
        self.state.last_z = s.last_z;
        self.state.last_rate = s.last_rate;
        self.state.last_speed = s.last_speed;
        self.state.last_coolant = s.last_coolant;
        self.state.last_spindle_dir = s.last_spindle_dir;
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
    fn set_work_z_here(&mut self, z_mm: f64) {
        // hat3: same G92-Z mechanism as `tool_z_shift`, but always
        // emitted (a 0 mm touch plate still needs Z re-zeroed here).
        let s = self.fmt_len(z_mm);
        self.write(format!("(set work Z: {s})"));
        self.write(format!("G92 Z{s}"));
        self.state.last_z = None;
    }
    fn tool_length_offset(&mut self, h: u32) {
        // llkf: apply tool-table length offset H<h>. G43 shifts the
        // active Z offset frame, so flush tracked Z — the next move
        // re-emits explicitly rather than eliding against a stale frame.
        self.write(format!("G43 H{h}"));
        self.state.last_z = None;
    }
    fn tool_length_offset_off(&mut self) {
        // llkf: cancel tool-length comp at program end.
        self.write("G49");
        self.state.last_z = None;
    }
    fn dwell(&mut self, seconds: f64) {
        if seconds <= 0.0 {
            return;
        }
        // nxn0: honor profile's dwell_unit for G4 P as well — same
        // controller will read the dwell word with the same unit
        // semantics whether it sits on a G4 line or a canned cycle.
        let s = self.fmt_dwell_p(seconds);
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
    fn select_wcs(&mut self, wcs: crate::project::Wcs) {
        // e2mq: pin the active WCS in the prologue. Emit the explicit
        // `G54..G59` word so the controller doesn't run against a
        // stale modal left by a prior program. Pin into `PostState.wcs`
        // so `tool_z_shift` can build its `G10 L20 P<n>` against the
        // right table — G54=P1 .. G59=P6.
        self.state.wcs = wcs;
        self.write(wcs.gcode_word());
    }
    fn set_post_profile(&mut self, profile: Option<&PostProfile>) {
        self.state.profile = profile.cloned();
    }
    fn set_token_ctx(&mut self, ctx: &TokenCtx) {
        self.state.token_ctx = ctx.clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gcode::post_profile::{AxesConfig, AxisFormat, PostProfile};

    #[test]
    fn r164_i_letter_auto_tracks_x_rename() {
        // When X is renamed (X → A) but I is left at its default
        // letter, the post must auto-track so the offset word uses
        // the SAME letter as the coordinate it references. Otherwise
        // the arc emits `A1.000 I-1.000` and a controller that
        // expects `A1.000 I-1.000` per spec is fine, but most users
        // who rename X expect the offset to come along (FANUC-style
        // controllers with arbitrary axis remapping).
        let mut post = Post::new();
        let mut profile = PostProfile::linuxcnc_default();
        let mut axes = AxesConfig::default();
        axes.x.name = "A".into();
        // Leave axes.i at the default letter — auto-track should fire.
        profile.axes = Some(axes);
        post.set_post_profile(Some(&profile));
        // Seed last_x/y so the arc body emits the offset word.
        post.state.last_x = Some(0.0);
        post.state.last_y = Some(0.0);
        post.arc_ccw(Some(1.0), Some(0.0), None, Some(0.5), Some(0.0));
        let out = post.finish();
        assert!(
            out.contains(" A0.500"),
            "I should auto-track X rename — expected `A0.500` for I offset, got: {out}",
        );
    }

    #[test]
    fn r164_i_letter_explicit_override_preserved() {
        // If the user explicitly renames I, that overrides the
        // X-tracking — we don't second-guess them.
        let mut post = Post::new();
        let mut profile = PostProfile::linuxcnc_default();
        let mut axes = AxesConfig::default();
        axes.x.name = "A".into();
        axes.i = AxisFormat {
            enabled: true,
            name: "II".into(),
            format: "%.3f".into(),
            scale: 1.0,
        };
        profile.axes = Some(axes);
        post.set_post_profile(Some(&profile));
        post.state.last_x = Some(0.0);
        post.state.last_y = Some(0.0);
        post.arc_ccw(Some(1.0), Some(0.0), None, Some(0.5), Some(0.0));
        let out = post.finish();
        assert!(
            out.contains(" II0.500"),
            "explicit I rename should win — expected `II0.500`, got: {out}",
        );
    }
}
