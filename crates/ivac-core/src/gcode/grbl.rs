//! GRBL post-processor. Mostly identical to `LinuxCNC`; differences:
//! - No G64 path-blending pragma
//! - Tool change is omitted (GRBL is single-tool)
//! - Coolant mist (M7) often unsupported; we still emit it for symmetry.

// # CAM/sim pedantic-lint exemptions
// GRBL post emits the same gcode-letter short names as the LinuxCNC post
// (X/Y/Z/I/J/F/S).
#![allow(clippy::many_single_char_names)]

use crate::gcode::post_profile::template_lines;
use crate::gcode::{linuxcnc, CapturedPostState, PostProcessor};
use crate::project::{ToolOffset, UnitSystem};

/// gcxl: GRBL doesn't accept paren-style `(text)` block comments — it
/// only recognises `;` line comments. Rewrite any `(...)` segments in
/// `line` to `; ...` so a paren comment leaking through `raw()` or a
/// user-defined template doesn't get rejected by the controller.
///
/// Behaviour:
/// - A line that's PURELY whitespace + `(...)` becomes `; <body>`.
/// - A line whose code-portion is followed by `(...)` strips the
///   paren block (it would still be rejected by GRBL when inline);
///   we drop it from the inline position and append it as a `;`
///   trailing comment on the same line.
/// - Mismatched / nested parens leave the line untouched — those are
///   almost certainly not intended as comments.
fn rewrite_paren_comments_for_grbl(line: &str) -> String {
    let trimmed = line.trim_end();
    // Reject inputs without a `(` quickly; most lines are pure gcode.
    if !trimmed.contains('(') {
        return line.to_string();
    }
    // Walk the line, splitting into a "code" portion (everything
    // before any `(`) and a list of bracketed bodies. Bail to the
    // original on any imbalance.
    let bytes = trimmed.as_bytes();
    let mut code = String::new();
    let mut bodies: Vec<String> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'(' {
            // Find matching `)` (no nesting; treat first `)` as close).
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] != b')' {
                j += 1;
            }
            if j >= bytes.len() {
                // Unbalanced — leave the line alone.
                return line.to_string();
            }
            let body = std::str::from_utf8(&bytes[i + 1..j]).unwrap_or("");
            bodies.push(body.trim().to_string());
            i = j + 1;
        } else {
            code.push(c as char);
            i += 1;
        }
    }
    let code_trimmed = code.trim();
    if code_trimmed.is_empty() {
        // Whole line was a comment.
        return bodies
            .iter()
            .map(|b| format!("; {b}"))
            .collect::<Vec<_>>()
            .join("\n");
    }
    // Code on the line — keep it, then append each comment as a
    // semicolon comment on a fresh line so the controller doesn't
    // see a paren on the same logical line.
    let mut out = code_trimmed.to_string();
    for b in bodies {
        out.push_str("\n; ");
        out.push_str(&b);
    }
    out
}

#[derive(Debug, Default)]
pub struct Post {
    inner: linuxcnc::Post,
}

impl Post {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// z9zh: construct a GRBL post in dynamic-power laser mode when
    /// `dynamic` is true. The laser arm/fire hooks then emit `M4`
    /// (power ramps with feed; rapids force S0) instead of `M3`. Wired
    /// from `MachineConfig.laser_dynamic_power` at pipeline construction.
    /// The flag lives on the inner post's `PostState` and `configure_post_state`
    /// mutates fields in place, so it survives `program_start`.
    #[must_use]
    pub fn with_dynamic_laser(dynamic: bool) -> Self {
        let mut p = Self::default();
        p.inner.state.laser_dynamic = dynamic;
        p
    }
}

impl PostProcessor for Post {
    fn fmt_dwell_post(&self, seconds: f64) -> String {
        // pxyt: delegate to the inner LinuxCNC post so the trait-default
        // drill_simple / drill_peck / drill_chip_break methods (GRBL
        // doesn't override them — no canned cycle support) honour the
        // active profile's dwell_unit. Without this, a GRBL build
        // running a Mach3-metric / Centroid profile (DwellUnit::Milliseconds)
        // would emit `G4 P0.5` (0.5 ms on that controller) for an
        // intended 500 ms dwell — a 1000x mismatch.
        self.inner.fmt_dwell_post(seconds)
    }
    fn separation(&mut self) {
        self.inner.separation();
    }
    fn raw(&mut self, cmd: &str) {
        // gcxl: GRBL rejects paren-style `(text)` comments. Rewrite
        // them to `; ...` line comments BEFORE handing off to the
        // inner linuxcnc post (which would emit them verbatim). The
        // rewriter is a no-op for paren-free lines so normal gcode
        // (G0/G1/M3/...) is untouched.
        let rewritten = rewrite_paren_comments_for_grbl(cmd);
        for line in rewritten.split('\n') {
            self.inner.raw(line);
        }
    }
    fn comment(&mut self, text: &str) {
        // GRBL strips parentheses-wrapped comments; emit as semicolon for safety.
        self.inner.raw(&format!("; {text}"));
    }
    fn unit(&mut self, unit: UnitSystem) {
        self.inner.unit(unit);
    }
    fn absolute(&mut self, active: bool) {
        self.inner.absolute(active);
    }
    fn feedrate(&mut self, rate: u32) {
        self.inner.feedrate(rate);
    }
    fn program_start(&mut self) {
        // gcxl: expand the template HERE (not in the inner linuxcnc
        // post) so we can rewrite paren-style comments to `; ...`
        // BEFORE they land in the output buffer. The inner post would
        // emit them verbatim, breaking GRBL.
        let template = self
            .inner
            .state
            .profile
            .as_ref()
            .and_then(|p| p.program_start.clone());
        if let Some(template) = template {
            let ctx = self.inner.state.token_ctx.clone();
            for line in template_lines(&template, &ctx) {
                let rewritten = rewrite_paren_comments_for_grbl(&line);
                for sub in rewritten.split('\n') {
                    self.inner.raw(sub);
                }
            }
        } else {
            self.inner.raw("; generated by ivaCAM (GRBL)");
        }
    }
    fn program_end(&mut self) {
        let template = self
            .inner
            .state
            .profile
            .as_ref()
            .and_then(|p| p.program_end.clone());
        if let Some(template) = template {
            let ctx = self.inner.state.token_ctx.clone();
            for line in template_lines(&template, &ctx) {
                let rewritten = rewrite_paren_comments_for_grbl(&line);
                for sub in rewritten.split('\n') {
                    self.inner.raw(sub);
                }
            }
        } else {
            self.inner.raw("M2");
        }
    }
    fn tool(&mut self, n: u32) {
        // GRBL is single-tool by default; emit nothing UNLESS the
        // user-configured profile has a tool_change template — they
        // may run modified GRBL with toolchange macros.
        let template = self
            .inner
            .state
            .profile
            .as_ref()
            .and_then(|p| p.tool_change.clone());
        if let Some(template) = template {
            // Refresh the future-tool token before rendering.
            self.inner.state.token_ctx.tool_number = n;
            let ctx = self.inner.state.token_ctx.clone();
            for line in template_lines(&template, &ctx) {
                let rewritten = rewrite_paren_comments_for_grbl(&line);
                for sub in rewritten.split('\n') {
                    self.inner.raw(sub);
                }
            }
        }
    }
    fn tool_offsets(&mut self, offset: ToolOffset) {
        self.inner.tool_offsets(offset);
    }
    fn machine_offsets(&mut self, offsets: (f64, f64, f64), soft: bool) {
        self.inner.machine_offsets(offsets, soft);
    }
    fn coolant_mist(&mut self) {
        self.inner.coolant_mist();
    }
    fn coolant_flood(&mut self) {
        self.inner.coolant_flood();
    }
    fn coolant_off(&mut self) {
        self.inner.coolant_off();
    }
    fn spindle_off(&mut self) {
        self.inner.spindle_off();
    }
    fn spindle_cw(&mut self, speed: u32, pause: u32) {
        self.inner.spindle_cw(speed, pause);
    }
    fn spindle_ccw(&mut self, speed: u32, pause: u32) {
        self.inner.spindle_ccw(speed, pause);
    }
    fn laser_on(&mut self, power: u32) {
        // 20y5: delegate to the inner LinuxCNC post, which emits
        // `M3 S<power>`. GRBL in laser-mode (`$32=1`) accepts the
        // same syntax and modally tracks S as the laser PWM duty.
        self.inner.laser_on(power);
    }
    fn laser_arm(&mut self) {
        // xkvv: delegate to LinuxCNC's `M3 S0`. GRBL laser-mode
        // (`$32=1`) tracks the modal S = 0 through the rapid, so the
        // pierce-time `laser_on(power)` re-emits the S<power> word.
        self.inner.laser_arm();
    }
    fn laser_off(&mut self) {
        self.inner.laser_off();
    }
    fn move_to(&mut self, x: Option<f64>, y: Option<f64>, z: Option<f64>) {
        self.inner.move_to(x, y, z);
    }
    fn rapid_machine_xy(&mut self, x_mm: f64, y_mm: f64) {
        // GRBL accepts G53 from v1.1 onward; delegate to the inner
        // LinuxCNC post for identical formatting + position-cache
        // invalidation (ad0v). Same reuse pattern as `move_to`.
        self.inner.rapid_machine_xy(x_mm, y_mm);
    }
    fn rapid_machine_z(&mut self, z_mm: f64) {
        // hat3: GRBL accepts G53 G0 Z; identical formatting via inner.
        self.inner.rapid_machine_z(z_mm);
    }
    fn tool_length_offset(&mut self, h: u32) {
        // llkf: grblHAL supports G43 H<n> (stock GRBL ignores it, but
        // the i185 footgun guard already steers stock-GRBL users to a
        // template / M0 instead). Same emission as inner.
        self.inner.tool_length_offset(h);
    }
    fn tool_length_offset_off(&mut self) {
        self.inner.tool_length_offset_off();
    }
    fn probe_toward_z(&mut self, distance_mm: f64, feed_mm_min: u32) {
        // hat3: GRBL / grblHAL support G38.2; same emission as inner.
        self.inner.probe_toward_z(distance_mm, feed_mm_min);
    }
    fn apply_probed_tool_length(&mut self) {
        // hat3 / 7iej.1: stock GRBL has no numbered-parameter system, so
        // LinuxCNC's `G43.1 Z[#5063]` (apply the probed Z) can't be
        // emitted here — and our own `G38.2` is NOT wired into grblHAL's
        // `$341` tool-measure cycle (that runs inside the controller's M6
        // macro, not from a hand-rolled probe). So this emits only a
        // comment: the offset is applied ONLY if the user's grblHAL build
        // performs `$341` in a tool-change macro template. The pipeline
        // fires a critical `grbl_fixed_sensor_no_offset` warning when no
        // such template exists, since otherwise the cut runs uncompensated.
        self.inner
            .raw("; tool length offset: relies on grblHAL $341 in the M6 macro (see grbl_fixed_sensor_no_offset)");
    }
    fn linear(&mut self, x: Option<f64>, y: Option<f64>, z: Option<f64>) {
        self.inner.linear(x, y, z);
    }
    fn arc_cw(
        &mut self,
        x: Option<f64>,
        y: Option<f64>,
        z: Option<f64>,
        i: Option<f64>,
        j: Option<f64>,
    ) {
        self.inner.arc_cw(x, y, z, i, j);
    }
    fn arc_ccw(
        &mut self,
        x: Option<f64>,
        y: Option<f64>,
        z: Option<f64>,
        i: Option<f64>,
        j: Option<f64>,
    ) {
        self.inner.arc_ccw(x, y, z, i, j);
    }
    fn finish(&self) -> String {
        self.inner.finish()
    }
    fn out_lines_count(&self) -> usize {
        self.inner.out_lines_count()
    }
    fn out_lines_clone_from(&self, start: usize) -> Vec<String> {
        self.inner.out_lines_clone_from(start)
    }
    fn out_extend_lines(&mut self, lines: &[String]) {
        self.inner.out_extend_lines(lines);
    }
    fn reset_state(&mut self) {
        self.inner.reset_state();
    }
    fn capture_state(&self) -> CapturedPostState {
        self.inner.capture_state()
    }
    fn restore_state(&mut self, s: &CapturedPostState) {
        self.inner.restore_state(s);
    }
    fn configure(
        &mut self,
        decimal_separator: char,
        line_number_start: Option<u32>,
        unit: UnitSystem,
    ) {
        self.inner
            .configure(decimal_separator, line_number_start, unit);
    }
    fn tool_z_shift(&mut self, shift_mm: f64) {
        // plau: GRBL's G92 semantics are firmware-revision-dependent —
        // some builds reset the G92 offset on power cycle / soft reset,
        // others persist it, and a few ignore the Z component
        // altogether. Use `G10 L20 P<n> Z<shift>` instead: that's the
        // GRBL-spec way to overwrite the active WCS Z origin, with
        // deterministic persistence (saved in EEPROM). Emit `G92.1`
        // first to clear any leftover G92 offset that a prior program
        // (or our own LinuxCNC peer) might have left active —
        // otherwise the new G10 stacks on top.
        //
        // e2mq: target the *active* WCS — `PostState.wcs` is pinned at
        // program_begin from `Setup.wcs` / `Project.work_offset.wcs`,
        // so G54=P1, G55=P2, ..., G59=P6. The prior code hardcoded P1
        // (G54) even when the user picked G55, silently writing the
        // per-tool z-shift into the wrong table.
        if shift_mm.abs() < 1e-9 {
            return;
        }
        // Reach into the inner state so we get the same fmt_len / line
        // numbering / decimal-separator handling LinuxCNC uses.
        let s = crate::gcode::fmt_num_dp(
            shift_mm * self.inner.state.unit_scale,
            self.inner.state.decimal_separator,
            self.inner.state.decimals(),
        );
        let p = self.inner.state.wcs.p_number();
        self.inner.raw(&format!("; z-shift: {s}"));
        self.inner.raw("G92.1");
        self.inner.raw(&format!("G10 L20 P{p} Z{s}"));
        // The new WCS origin invalidates our delta-encoded last_z;
        // mirror LinuxCNC's tool_z_shift bookkeeping.
        self.inner.state.last_z = None;
    }
    fn set_work_z_here(&mut self, z_mm: f64) {
        // hat3: same `G10 L20 P<n> Z` mechanism as `tool_z_shift`, but
        // always emitted (a 0 mm touch plate still re-zeros Z). G92.1
        // first clears any stale G92 offset so the new origin doesn't
        // stack, matching tool_z_shift.
        let s = crate::gcode::fmt_num_dp(
            z_mm * self.inner.state.unit_scale,
            self.inner.state.decimal_separator,
            self.inner.state.decimals(),
        );
        let p = self.inner.state.wcs.p_number();
        self.inner.raw(&format!("; set work Z: {s}"));
        self.inner.raw("G92.1");
        self.inner.raw(&format!("G10 L20 P{p} Z{s}"));
        self.inner.state.last_z = None;
    }
    fn dwell(&mut self, seconds: f64) {
        self.inner.dwell(seconds);
    }
    fn plane_xy(&mut self) {
        // GRBL accepts G17 (and only G17 — G18/G19 are rejected). Emit
        // through the inner post so the prologue is consistent.
        self.inner.plane_xy();
    }
    fn cutter_comp_off(&mut self) {
        // GRBL rejects G41 / G42 (no cutter compensation), but G40 is
        // accepted as a no-op. Emit it for symmetry — keeps the
        // modal-state defense in place if the user later switches to a
        // post that DOES emit comp.
        self.inner.cutter_comp_off();
    }
    fn feed_per_minute(&mut self) {
        self.inner.feed_per_minute();
    }
    // cancel_canned_cycle: GRBL doesn't support canned cycles — our
    // drill block was emitted via the default G0/G1 trait expansion —
    // so there's no modal state to cancel. Default no-op suffices.
    fn set_post_profile(&mut self, profile: Option<&crate::gcode::post_profile::PostProfile>) {
        self.inner.set_post_profile(profile);
    }
    fn set_token_ctx(&mut self, ctx: &crate::gcode::post_profile::TokenCtx) {
        self.inner.set_token_ctx(ctx);
    }
    fn select_wcs(&mut self, wcs: crate::project::Wcs) {
        // e2mq: delegate to the inner LinuxCNC post so the WCS word
        // and `PostState.wcs` are pinned identically — our overriden
        // `tool_z_shift` reads `inner.state.wcs` for its `G10 L20 P<n>`.
        self.inner.select_wcs(wcs);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gcode::post_profile::PostProfile;

    #[test]
    fn gcxl_raw_paren_comment_rewritten_to_semicolon() {
        // Bare paren-only line must become a `; ...` comment.
        let mut post = Post::new();
        post.raw("(generated by ivaCAM)");
        let out = post.finish();
        assert!(
            out.contains("; generated by ivaCAM"),
            "expected semicolon comment in output, got: {out}",
        );
        assert!(
            !out.contains('('),
            "no paren should remain in GRBL output: {out}",
        );
    }

    #[test]
    fn gcxl_raw_inline_paren_split_into_two_lines() {
        // Code + inline paren comment must split: code stays inline,
        // the comment moves to its own `;` line so the controller
        // never sees a paren on a code line.
        let mut post = Post::new();
        post.raw("G1 X10 (move to start)");
        let out = post.finish();
        assert!(
            out.contains("G1 X10"),
            "code portion must survive, got: {out}",
        );
        assert!(
            out.contains("; move to start"),
            "comment must move to a `;` line, got: {out}",
        );
        assert!(!out.contains('('), "no paren should remain: {out}",);
    }

    #[test]
    fn gcxl_template_program_start_paren_rewritten() {
        // gcxl: program_start templates containing paren-style
        // comments leak through. Verify the GRBL path rewrites them.
        let mut post = Post::new();
        let mut profile = PostProfile::grbl_default();
        profile.program_start = Some("(my header)\nG21 G90 (mm + abs)".into());
        post.set_post_profile(Some(&profile));
        post.program_start();
        let out = post.finish();
        assert!(
            !out.contains('('),
            "GRBL output must not contain paren comments: {out}",
        );
        assert!(
            out.contains("; my header"),
            "missing rewritten header: {out}"
        );
    }

    #[test]
    fn plau_z_shift_uses_g10_l20_with_g92_1_clear() {
        // GRBL's bare G92 has firmware-revision-dependent persistence;
        // emit G10 L20 P1 (set G54 origin) preceded by G92.1 (clear any
        // leftover G92) so the new origin sticks deterministically.
        let mut post = Post::new();
        post.tool_z_shift(1.5);
        let out = post.finish();
        assert!(
            out.contains("G92.1"),
            "expected G92.1 (clear G92) before G10 L20, got: {out}",
        );
        assert!(
            out.contains("G10 L20 P1 Z1.5"),
            "expected `G10 L20 P1 Z1.5` for tool_z_shift, got: {out}",
        );
        assert!(
            !out.contains("\nG92 Z"),
            "must not emit bare G92 Z form on GRBL: {out}",
        );
    }

    #[test]
    fn plau_z_shift_zero_is_noop() {
        // Same skip-on-zero contract LinuxCNC has.
        let mut post = Post::new();
        post.tool_z_shift(0.0);
        let out = post.finish();
        assert!(
            !out.contains("G10") && !out.contains("G92"),
            "zero shift should emit nothing, got: {out}",
        );
    }

    #[test]
    fn gcxl_no_rewrite_on_paren_free_input() {
        // Plain gcode lines must pass through unchanged — no spurious
        // semicolon prefix, no extra splits.
        let mut post = Post::new();
        post.raw("G1 X10 Y20 F500");
        let out = post.finish();
        let line = out.trim();
        assert_eq!(line, "G1 X10 Y20 F500", "got: {out}");
    }

    #[test]
    fn z9zh_default_laser_emits_m3() {
        // Portable default: arm + fire emit M3 (works in GRBL $32=1 and
        // mill mode).
        let mut post = Post::new();
        post.laser_arm();
        post.laser_on(800);
        let out = post.finish();
        assert!(out.contains("M3"), "default should emit M3, got:\n{out}");
        assert!(
            !out.contains("M4"),
            "default must NOT emit M4 (LinuxCNC M4 = spindle-CCW), got:\n{out}",
        );
    }

    #[test]
    fn z9zh_dynamic_laser_emits_m4() {
        // Opt-in dynamic-power mode: arm + fire emit M4 so GRBL ramps S
        // with feed (no corner/edge over-burn).
        let mut post = Post::with_dynamic_laser(true);
        post.laser_arm();
        post.laser_on(800);
        let out = post.finish();
        assert!(
            out.contains("M4"),
            "dynamic mode should emit M4, got:\n{out}",
        );
        assert!(
            !out.contains("M3"),
            "dynamic mode must not emit M3, got:\n{out}",
        );
        // Still drops the beam with M5 between cuts.
        post.laser_off();
        assert!(post.finish().contains("M5"));
    }
}
