//! SVG 1.1 single-line font parser + renderer (e3kg).
//!
//! Single-line CAM fonts (ISO 3098, Hershey, Gerber Engineering) encode
//! each glyph as the centerline path a pen / engraver traces — exactly
//! one stroke per stroke, no closed outlines. The SVG 1.1 `<font>` /
//! `<glyph>` element family is the de-facto interchange format for
//! these. SVG 2 dropped these elements; we still parse them because
//! the existing CAM font corpus is published this way.
//!
//! Scope: the very small subset the bundled ISO 3098 Regular / Italic
//! fonts use:
//!
//! * `<font horiz-adv-x="…">` carries the default glyph advance.
//! * `<font-face units-per-em="…" ascent="…" descent="…" cap-height="…"
//!    x-height="…" font-family="…" />` carries the metrics.
//! * `<glyph unicode="&#xNN;" d="…" />` carries the per-glyph path
//!   (absolute `M` / `L` / `A` only — these fonts use no relative
//!   commands and no Bezier curves).
//! * Per-glyph `horiz-adv-x` is honored when present.
//! * `unicode="&#xNN;"` numeric character references AND literal
//!   characters (`unicode="A"`) are both accepted.
//!
//! Arcs are tessellated into chord segments at a default tolerance of
//! `units_per_em / 4000` (sub-µm on a 900-unit em → well below the
//! engraver's pen / spot footprint). The two endpoint-to-center
//! conversions follow the W3C SVG 1.1 implementation note (`Appendix
//! F.6.5`).
//!
//! This is NOT a general-purpose SVG parser. Anything we don't
//! understand is reported as an error so the caller can fall through
//! to the ttf-parser path.

use std::collections::HashMap;

use crate::geometry::{Point2, Segment};
use crate::{Error, Result};

/// A parsed SVG 1.1 single-line font. Glyphs are indexed by their
/// `unicode` codepoint; the missing-character case falls back to the
/// space advance + an empty stroke list.
#[derive(Debug, Clone)]
pub struct SvgFont {
    /// `font-face[font-family]` — populated for the family-name
    /// classification returned by `render_text_layer_api`.
    pub family_name: String,
    /// `font-face[units-per-em]`. All other metrics are in these
    /// units; the renderer scales to `size_mm` via
    /// `size_mm / units_per_em`.
    pub units_per_em: f64,
    /// `font[horiz-adv-x]` — default per-glyph advance for glyphs
    /// without their own `horiz-adv-x` override.
    pub default_advance_x: f64,
    /// `font-face[ascent]` (positive, em units above baseline).
    pub ascent: f64,
    /// `font-face[descent]` (negative, em units below baseline).
    pub descent: f64,
    /// `font-face[cap-height]`.
    pub cap_height: f64,
    /// `font-face[x-height]`.
    pub x_height: f64,
    /// Per-codepoint glyph table.
    pub glyphs: HashMap<char, Glyph>,
}

/// One glyph: its advance + a flat list of subpaths. Each subpath is a
/// chain of strokes (line segments) the engraver traces with the pen
/// down. New subpaths require a pen-up move.
#[derive(Debug, Clone)]
pub struct Glyph {
    pub advance_x: f64,
    /// Each `Vec<Point2>` is one continuous polyline ≥ 1 point. The
    /// renderer emits one engraver stroke per polyline (pen-down).
    pub subpaths: Vec<Vec<Point2>>,
}

/// Parse an SVG 1.1 font file. Returns `Error::misconfigured` when the
/// input doesn't look like an SVG font — the caller can fall through
/// to the ttf-parser path.
pub fn parse(bytes: &[u8]) -> Result<SvgFont> {
    let src = std::str::from_utf8(bytes).map_err(|e| {
        Error::misconfigured(format!("SVG font: not UTF-8 ({e})"))
            .with_hint("Pick a different font for this text layer.")
    })?;
    parse_str(src)
}

/// Heuristic: the bytes look like an SVG file (XML / SVG header before
/// any binary noise). Cheap enough to call before the real parser.
#[must_use]
pub fn looks_like_svg(bytes: &[u8]) -> bool {
    let prefix_len = bytes.len().min(512);
    let head = std::str::from_utf8(&bytes[..prefix_len]).unwrap_or("");
    let trimmed = head.trim_start();
    trimmed.starts_with("<?xml")
        || trimmed.starts_with("<!DOCTYPE svg")
        || trimmed.starts_with("<svg")
}

fn parse_str(src: &str) -> Result<SvgFont> {
    // Pull metrics from the first <font-face .../> tag.
    let face_open = src.find("<font-face").ok_or_else(missing_font_face)?;
    let face_close = src[face_open..].find('>').ok_or_else(missing_font_face)? + face_open;
    let face_attrs = &src[face_open + "<font-face".len()..face_close];
    let family_name = attr(face_attrs, "font-family").unwrap_or_else(|| "SVG font".into());
    let units_per_em = attr_f64(face_attrs, "units-per-em").unwrap_or(1000.0);
    let ascent = attr_f64(face_attrs, "ascent").unwrap_or(units_per_em * 0.8);
    let descent = attr_f64(face_attrs, "descent").unwrap_or(-units_per_em * 0.2);
    let cap_height = attr_f64(face_attrs, "cap-height").unwrap_or(ascent);
    let x_height = attr_f64(face_attrs, "x-height").unwrap_or(ascent * 0.5);

    // Default advance comes from the enclosing <font> tag.
    let default_advance_x = src
        .find("<font ")
        .and_then(|i| {
            let close = src[i..].find('>')?;
            attr_f64(&src[i.."<font ".len() + i + close], "horiz-adv-x")
        })
        .unwrap_or(units_per_em);

    // Walk every `<glyph …/>` in source order.
    let mut glyphs: HashMap<char, Glyph> = HashMap::new();
    let mut cursor = face_close;
    while let Some(g_open) = src[cursor..].find("<glyph") {
        let abs = cursor + g_open;
        let close = match src[abs..].find('>') {
            Some(i) => abs + i,
            None => break,
        };
        let attrs = &src[abs + "<glyph".len()..close];
        cursor = close + 1;

        let Some(ch) = parse_unicode_attr(attrs) else {
            continue;
        };
        let advance_x = attr_f64(attrs, "horiz-adv-x").unwrap_or(default_advance_x);
        let d = attr(attrs, "d").unwrap_or_default();
        let chord_tolerance = (units_per_em / 4000.0).max(0.05);
        let subpaths = parse_path_d(&d, chord_tolerance)?;
        glyphs.insert(
            ch,
            Glyph {
                advance_x,
                subpaths,
            },
        );
    }

    if glyphs.is_empty() {
        return Err(Error::misconfigured("SVG font: no glyphs parsed")
            .with_hint("Pick a different font for this text layer."));
    }

    Ok(SvgFont {
        family_name,
        units_per_em,
        default_advance_x,
        ascent,
        descent,
        cap_height,
        x_height,
        glyphs,
    })
}

fn missing_font_face() -> Error {
    Error::misconfigured("SVG font: missing <font-face> tag")
        .with_hint("This file isn't an SVG 1.1 font.")
}

/// Pull `name="value"` out of an attribute string. Returns the value
/// verbatim — the caller decodes XML entities (`&#x21;` etc.) if
/// needed.
fn attr(attrs: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=\"");
    let i = attrs.find(&needle)?;
    let rest = &attrs[i + needle.len()..];
    let j = rest.find('"')?;
    Some(rest[..j].to_string())
}

fn attr_f64(attrs: &str, name: &str) -> Option<f64> {
    attr(attrs, name).and_then(|s| s.parse::<f64>().ok())
}

/// Parse the SVG `unicode` attribute. Accepts numeric character
/// references (`&#x21;`, `&#33;`) and literal characters
/// (`unicode="A"`).
fn parse_unicode_attr(attrs: &str) -> Option<char> {
    let v = attr(attrs, "unicode")?;
    if let Some(hex) = v.strip_prefix("&#x").and_then(|s| s.strip_suffix(';')) {
        let cp = u32::from_str_radix(hex, 16).ok()?;
        return char::from_u32(cp);
    }
    if let Some(dec) = v.strip_prefix("&#").and_then(|s| s.strip_suffix(';')) {
        let cp: u32 = dec.parse().ok()?;
        return char::from_u32(cp);
    }
    // Literal character — only honor single-char attrs (multi-char
    // ligatures aren't modelled).
    let mut chars = v.chars();
    let first = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    Some(first)
}

/// Parse a `d="…"` path string into one polyline per `M…` subpath,
/// tessellating arcs (`A`) into chord segments at the given tolerance
/// (em units). Returns Err on unsupported commands so the caller can
/// fail loudly during font-load instead of silently producing wrong
/// strokes.
fn parse_path_d(d: &str, chord_tolerance: f64) -> Result<Vec<Vec<Point2>>> {
    let tokens = tokenize_path(d);
    let mut subpaths: Vec<Vec<Point2>> = Vec::new();
    let mut current: Vec<Point2> = Vec::new();
    let mut cursor = Point2::new(0.0, 0.0);
    let mut i = 0;
    while i < tokens.len() {
        let cmd = match tokens[i] {
            Token::Cmd(c) => c,
            Token::Num(_) => {
                return Err(Error::misconfigured(format!(
                    "SVG font: stray number at path token {i} in `{d}`"
                ))
                .with_hint("Pick a different font for this text layer."));
            }
        };
        i += 1;
        match cmd {
            'M' => {
                // New subpath.
                if !current.is_empty() {
                    subpaths.push(std::mem::take(&mut current));
                }
                let x = next_num(&tokens, &mut i, d)?;
                let y = next_num(&tokens, &mut i, d)?;
                cursor = Point2::new(x, y);
                current.push(cursor);
            }
            'L' => {
                let x = next_num(&tokens, &mut i, d)?;
                let y = next_num(&tokens, &mut i, d)?;
                cursor = Point2::new(x, y);
                current.push(cursor);
            }
            'A' => {
                let rx = next_num(&tokens, &mut i, d)?;
                let ry = next_num(&tokens, &mut i, d)?;
                let x_axis_rot = next_num(&tokens, &mut i, d)?;
                let large_arc = next_flag(&tokens, &mut i, d)?;
                let sweep = next_flag(&tokens, &mut i, d)?;
                let end_x = next_num(&tokens, &mut i, d)?;
                let end_y = next_num(&tokens, &mut i, d)?;
                let end = Point2::new(end_x, end_y);
                tessellate_arc(
                    cursor,
                    end,
                    rx,
                    ry,
                    x_axis_rot.to_radians(),
                    large_arc,
                    sweep,
                    chord_tolerance,
                    &mut current,
                );
                cursor = end;
            }
            other => {
                return Err(Error::misconfigured(format!(
                    "SVG font: unsupported path command `{other}` in `{d}`"
                ))
                .with_hint("Only M / L / A are supported by this loader."));
            }
        }
    }
    if !current.is_empty() {
        subpaths.push(current);
    }
    Ok(subpaths)
}

#[derive(Debug, Clone, Copy)]
enum Token {
    Cmd(char),
    Num(f64),
}

fn tokenize_path(d: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let bytes = d.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_alphabetic() {
            out.push(Token::Cmd(b as char));
            i += 1;
            continue;
        }
        if b == b',' || b.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        // Number (may start with -, ., or digit).
        if b == b'-' || b == b'+' || b == b'.' || b.is_ascii_digit() {
            let start = i;
            if b == b'-' || b == b'+' {
                i += 1;
            }
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'.' {
                i += 1;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
            }
            if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
                i += 1;
                if i < bytes.len() && (bytes[i] == b'-' || bytes[i] == b'+') {
                    i += 1;
                }
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
            }
            if let Ok(s) = std::str::from_utf8(&bytes[start..i]) {
                if let Ok(v) = s.parse::<f64>() {
                    out.push(Token::Num(v));
                }
            }
            continue;
        }
        // Unknown — skip a byte so we don't loop forever.
        i += 1;
    }
    out
}

fn next_num(tokens: &[Token], i: &mut usize, d: &str) -> Result<f64> {
    let Some(Token::Num(v)) = tokens.get(*i) else {
        return Err(Error::misconfigured(format!(
            "SVG font: expected number at token {} in `{d}`",
            *i
        ))
        .with_hint("Pick a different font for this text layer."));
    };
    *i += 1;
    Ok(*v)
}

fn next_flag(tokens: &[Token], i: &mut usize, d: &str) -> Result<bool> {
    let v = next_num(tokens, i, d)?;
    Ok(v.abs() > 0.5)
}

/// Convert an SVG endpoint-form arc (`A rx ry x-rot large-arc sweep x
/// y`) to its center-arc parameters and append chord-tessellated
/// points to `out`. Skips the leading endpoint when `out` already ends
/// at the start (which it always does — `M` / `L` always populate it).
///
/// Math follows W3C SVG 1.1 Appendix F.6.5: endpoint-form → center-form,
/// then sample uniformly in arc-parameter `θ` with the chord count
/// chosen to keep the chord-error under `tolerance`.
#[allow(clippy::too_many_arguments)]
fn tessellate_arc(
    start: Point2,
    end: Point2,
    rx_in: f64,
    ry_in: f64,
    phi: f64,
    large_arc: bool,
    sweep: bool,
    tolerance: f64,
    out: &mut Vec<Point2>,
) {
    // Degenerate radii → straight line.
    if rx_in.abs() < 1e-12 || ry_in.abs() < 1e-12 {
        out.push(end);
        return;
    }
    let rx = rx_in.abs();
    let ry = ry_in.abs();
    // F.6.5.1: compute (x1', y1') — the start point in the ellipse's
    // local frame translated to the chord midpoint.
    let dx = (start.x - end.x) * 0.5;
    let dy = (start.y - end.y) * 0.5;
    let cos_phi = phi.cos();
    let sin_phi = phi.sin();
    let x1p = cos_phi * dx + sin_phi * dy;
    let y1p = -sin_phi * dx + cos_phi * dy;
    // F.6.6.2: ensure radii are large enough.
    let lambda = (x1p * x1p) / (rx * rx) + (y1p * y1p) / (ry * ry);
    let (rx, ry) = if lambda > 1.0 {
        let s = lambda.sqrt();
        (rx * s, ry * s)
    } else {
        (rx, ry)
    };
    // F.6.5.2: center in local frame.
    let num = (rx * rx * ry * ry) - (rx * rx * y1p * y1p) - (ry * ry * x1p * x1p);
    let den = (rx * rx * y1p * y1p) + (ry * ry * x1p * x1p);
    let factor = (num.max(0.0) / den.max(1e-30)).sqrt();
    let sign = if large_arc == sweep { -1.0 } else { 1.0 };
    let cxp = sign * factor * (rx * y1p / ry);
    let cyp = sign * factor * -(ry * x1p / rx);
    // F.6.5.3: center in absolute coords.
    let cx = cos_phi * cxp - sin_phi * cyp + (start.x + end.x) * 0.5;
    let cy = sin_phi * cxp + cos_phi * cyp + (start.y + end.y) * 0.5;
    // F.6.5.5: angle θ₁ and Δθ.
    let theta1 = angle_between(1.0, 0.0, (x1p - cxp) / rx, (y1p - cyp) / ry);
    let mut dtheta = angle_between(
        (x1p - cxp) / rx,
        (y1p - cyp) / ry,
        (-x1p - cxp) / rx,
        (-y1p - cyp) / ry,
    );
    if !sweep && dtheta > 0.0 {
        dtheta -= std::f64::consts::TAU;
    } else if sweep && dtheta < 0.0 {
        dtheta += std::f64::consts::TAU;
    }

    // Pick chord count: chord-sag ε ≈ r·(1 − cos(Δθ_chord/2)).
    // Solve Δθ_chord = 2·acos(1 − ε/r). Use the smaller radius
    // (worst case for sag) and clamp ε/r ≤ 0.5 so acos stays valid.
    let r_min = rx.min(ry).max(1e-12);
    let cos_arg = (1.0 - (tolerance / r_min)).clamp(-1.0, 1.0);
    let chord_step = 2.0 * cos_arg.acos();
    // `steps` is the chord count over the arc — always a small positive
    // integer (≤ a few thousand even for the densest font tessellation
    // we care about), so the f64 → usize cast is by construction safe.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let steps = ((dtheta.abs() / chord_step).ceil() as usize).max(4);
    for k in 1..=steps {
        let theta = theta1 + dtheta * (k as f64 / steps as f64);
        let local_x = rx * theta.cos();
        let local_y = ry * theta.sin();
        let x = cos_phi * local_x - sin_phi * local_y + cx;
        let y = sin_phi * local_x + cos_phi * local_y + cy;
        out.push(Point2::new(x, y));
    }
}

/// SVG-spec angle: signed angle from (ux, uy) to (vx, vy). Returns in
/// `(-π, π]`.
fn angle_between(ux: f64, uy: f64, vx: f64, vy: f64) -> f64 {
    let dot = ux * vx + uy * vy;
    let det = ux * vy - uy * vx;
    det.atan2(dot)
}

/// Render a string of `text` in the given SVG font at `size_mm`. The
/// returned segments live in a left-baseline frame (`x` advances with
/// each glyph; `y` = 0 is the baseline, `y > 0` rises toward the
/// cap-height). The caller transforms into world coords (origin,
/// rotation, alignment, line spacing).
///
/// `letter_spacing_units` adds extra advance between consecutive
/// glyphs IN EM UNITS (so callers convert from mm via
/// `letter_spacing_mm * units_per_em / size_mm`); 0 = font-natural.
#[must_use]
pub fn render_line(
    font: &SvgFont,
    text: &str,
    size_mm: f64,
    letter_spacing_units: f64,
    width_scale: f64,
    layer: &str,
    color: i32,
) -> (Vec<Segment>, f64) {
    let scale = size_mm / font.units_per_em;
    let scale_x = scale * width_scale;
    let scale_y = scale;
    let mut out: Vec<Segment> = Vec::new();
    let mut x_cursor = 0.0;
    for ch in text.chars() {
        if let Some(g) = font.glyphs.get(&ch) {
            for subpath in &g.subpaths {
                for win in subpath.windows(2) {
                    let p0 = Point2::new(x_cursor + win[0].x * scale_x, win[0].y * scale_y);
                    let p1 = Point2::new(x_cursor + win[1].x * scale_x, win[1].y * scale_y);
                    if (p0.x - p1.x).hypot(p0.y - p1.y) > 1e-9 {
                        out.push(Segment::line(p0, p1, layer, color));
                    }
                }
            }
            x_cursor += g.advance_x * scale_x;
        } else {
            // Missing glyph: advance by default + emit nothing (matches
            // the way SVG single-line fonts treat unknown codepoints —
            // ASCII control chars and pre-Unicode-emoji glyphs both
            // pass through as silent advance).
            x_cursor += font.default_advance_x * scale_x;
        }
        x_cursor += letter_spacing_units * scale_x;
    }
    (out, x_cursor)
}

// ─────────────────────────────────────────────────────────────────────
// Bundled fonts. Both ISO 3098 weights are baked into the binary so
// the user can pick them without uploading anything.
// ─────────────────────────────────────────────────────────────────────

/// ISO 3098 Regular — the canonical engineering-drawing font in
/// single-line form. Bundled per e3kg.
pub const ISO3098_REGULAR_SVG: &[u8] = include_bytes!("../../assets/fonts/ISO3098-Regular.svg");

/// ISO 3098 Italic — the matching oblique. Bundled per e3kg.
pub const ISO3098_ITALIC_SVG: &[u8] = include_bytes!("../../assets/fonts/ISO3098-Italic.svg");

/// Convenience: registry of bundled SVG fonts the user can pick by
/// name. The frontend mirrors these labels in its dropdown.
#[must_use]
pub fn bundled_fonts() -> &'static [(&'static str, &'static [u8])] {
    &[
        ("ISO 3098 Regular", ISO3098_REGULAR_SVG),
        ("ISO 3098 Italic", ISO3098_ITALIC_SVG),
    ]
}

// ─────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_like_svg_sniff() {
        assert!(looks_like_svg(b"<?xml version=\"1.0\"?>\n<svg></svg>"));
        assert!(looks_like_svg(b"<svg></svg>"));
        assert!(looks_like_svg(b"  \n<?xml ?>"));
        assert!(!looks_like_svg(b"\x00\x01OTTO"));
        assert!(!looks_like_svg(b"\x00\x01\x00\x00")); // TTF magic
    }

    #[test]
    fn iso3098_regular_loads() {
        let font = parse(ISO3098_REGULAR_SVG).expect("parse Regular");
        assert!(
            font.glyphs.len() > 50,
            "want > 50 glyphs, got {}",
            font.glyphs.len()
        );
        assert!((font.units_per_em - 900.0).abs() < 1e-9);
        assert_eq!(font.family_name, "ISO 3098");
        // Spot-check: the letter A should be present and have ≥ 1 stroke.
        let a = font.glyphs.get(&'A').expect("glyph A present");
        assert!(!a.subpaths.is_empty(), "letter A has no strokes");
    }

    #[test]
    fn iso3098_italic_loads() {
        let font = parse(ISO3098_ITALIC_SVG).expect("parse Italic");
        assert!(font.glyphs.len() > 50);
        // The italic should slant — sample a vertical stem (`I` digit
        // would work but only if present). For now assert it loads.
    }

    /// `!` (`uni0021`) is `M550 0 L550 50 M550 300 L550 900` — two
    /// disjoint vertical strokes. Render should produce two line
    /// segments, NOT one zigzag (the `M` between them must break the
    /// polyline).
    #[test]
    fn exclamation_renders_as_two_strokes() {
        let font = parse(ISO3098_REGULAR_SVG).expect("parse");
        let (segs, advance) = render_line(&font, "!", 9.0, 0.0, 1.0, "0", 7);
        // size_mm = 9, units_per_em = 900 → 1 em unit = 0.01 mm.
        // Expected segments: (550,0)→(550,50) length 50·0.01 = 0.5 mm
        //                    (550,300)→(550,900) length 600·0.01 = 6 mm
        assert_eq!(
            segs.len(),
            2,
            "exclamation should produce 2 strokes, got {}",
            segs.len()
        );
        let lens: Vec<f64> = segs
            .iter()
            .map(|s| (s.end.x - s.start.x).hypot(s.end.y - s.start.y))
            .collect();
        let total: f64 = lens.iter().sum();
        assert!(
            (total - 6.5).abs() < 0.01,
            "expected total length 6.5 mm, got {total}"
        );
        // Advance: default 1100 em units × 0.01 mm/unit = 11 mm.
        assert!(
            (advance - 11.0).abs() < 0.01,
            "expected advance 11 mm, got {advance}"
        );
    }

    /// `O` is two semicircle arcs joined by two vertical straight
    /// strokes. Render should produce ≥ 20 chord segments (arc
    /// tessellations sub-mm at 9 mm cap-height) PLUS the two 4-mm
    /// vertical straight strokes. Sanity-check the arc tessellation
    /// is dense enough and the path-command dispatch correctly
    /// handles M / L / A in sequence.
    #[test]
    fn capital_o_tessellates_arcs() {
        let font = parse(ISO3098_REGULAR_SVG).expect("parse");
        let (segs, _) = render_line(&font, "O", 9.0, 0.0, 1.0, "0", 7);
        // 2 verticals + 2 arcs of ~30 chords each → roughly 60-80 segs.
        assert!(
            segs.len() > 20,
            "O should produce many segments (2 straight strokes + 2 tessellated arcs), got {}",
            segs.len(),
        );
        // Categorize: anything > 1 mm at this size is an L stroke
        // (the two verticals are 4 mm each); everything ≤ 1 mm is an
        // arc tessellation chord. Both vertical strokes are present
        // exactly once each — confirms M moves correctly separated
        // the subpaths instead of joining the arcs end-to-end.
        let straight: Vec<&Segment> = segs
            .iter()
            .filter(|s| (s.end.x - s.start.x).hypot(s.end.y - s.start.y) > 1.0)
            .collect();
        assert_eq!(
            straight.len(),
            2,
            "expected exactly 2 long L strokes (the verticals); got {}",
            straight.len(),
        );
        for s in &straight {
            let l = (s.end.x - s.start.x).hypot(s.end.y - s.start.y);
            assert!(
                (l - 4.0).abs() < 0.05,
                "vertical stroke should be 4 mm tall, got {l}"
            );
            // Both should be vertical (Δx ≈ 0).
            assert!(
                (s.end.x - s.start.x).abs() < 1e-9,
                "vertical stroke isn't vertical: ({},{}) -> ({},{})",
                s.start.x,
                s.start.y,
                s.end.x,
                s.end.y,
            );
        }
        // Arc chords sub-mm.
        let max_arc_chord = segs
            .iter()
            .map(|s| (s.end.x - s.start.x).hypot(s.end.y - s.start.y))
            .filter(|&l| l < 1.0)
            .fold(0.0_f64, f64::max);
        assert!(
            max_arc_chord < 0.5,
            "arc tessellation chord too long: {max_arc_chord} mm at 9 mm cap-height"
        );
    }

    /// Unknown / unmapped codepoints advance but emit no strokes —
    /// matches the behavior the user wants for ASCII control chars,
    /// emoji, etc. without erroring the render.
    #[test]
    fn unknown_glyph_advances_silently() {
        let font = parse(ISO3098_REGULAR_SVG).expect("parse");
        // U+E000 is in the Private Use Area — the font won't have it.
        let (segs, advance) = render_line(&font, "\u{E000}", 9.0, 0.0, 1.0, "0", 7);
        assert!(segs.is_empty(), "expected no strokes for unknown glyph");
        assert!(
            advance > 0.0,
            "unknown glyph should still advance the cursor"
        );
    }

    /// Width-scale stretches both x-coordinates AND advance — letters
    /// look wider but the spacing math still adds up.
    #[test]
    fn width_scale_stretches_advance() {
        let font = parse(ISO3098_REGULAR_SVG).expect("parse");
        let (_, base) = render_line(&font, "AA", 9.0, 0.0, 1.0, "0", 7);
        let (_, wide) = render_line(&font, "AA", 9.0, 0.0, 2.0, "0", 7);
        assert!(
            (wide - 2.0 * base).abs() < 0.01,
            "width_scale=2.0 should double advance: base={base}, wide={wide}"
        );
    }

    #[test]
    fn bundled_registry_lists_both_fonts() {
        let bundled = bundled_fonts();
        assert_eq!(bundled.len(), 2);
        assert_eq!(bundled[0].0, "ISO 3098 Regular");
        assert_eq!(bundled[1].0, "ISO 3098 Italic");
        for (_, bytes) in bundled {
            let _ = parse(bytes).expect("bundled font parses");
        }
    }

    #[test]
    fn parser_rejects_non_svg_bytes() {
        // Random non-SVG byte sequence — should error with the
        // "missing font-face" message so the caller knows to fall
        // through to ttf-parser.
        let bytes = b"not svg bytes\n";
        let err = parse(bytes).expect_err("should reject");
        assert!(format!("{err:?}").contains("font-face") || format!("{err:?}").contains("SVG"));
    }
}
