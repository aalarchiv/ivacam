//! TEXT / MTEXT → outline path tessellation.
//!
//! Two render strategies:
//! 1. **Outline mode** (the default for normal display fonts) — walks each
//!    glyph's outline and emits filled-shape boundary segments. Good for
//!    profile cuts.
//! 2. **Single-line mode** (auto-detected for engraving fonts like `RhSS`,
//!    Hershey ports, OneLine.ttf, etc.) — these fonts have *no* enclosed
//!    area; their glyphs are stroked centerlines. Walking the outline of
//!    them produces a thin pair of forward+backward strokes that, after
//!    Clipper's offset routine, collapses into a tooth-pattern artifact.
//!    For these fonts we emit the centerline directly.
//!
//! The detection heuristic: a font is single-line if its average glyph's
//! filled area is much smaller than its bounding box would imply (i.e. the
//! glyph is a network of thin curves, not a filled shape). See
//! `is_single_line_font` for the threshold + tests.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ttf_parser::{Face, OutlineBuilder};

use crate::errors::Error;
use crate::geometry::{Point2, Segment};
use crate::math;
use crate::project::{text_layer_synthetic_layer, TextAlignment, TextLayer, TextLayerKind};

/// Request payload for the cross-transport `/text` endpoint. The frontend
/// hands us TTF bytes (uploaded by the user or pulled from
/// `frontend/public/fonts/`) plus a string + placement parameters; we
/// return flattened [`Segment`]s and a single-line / outline classification
/// the dialog uses to drive the engraving warning chip.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RenderTextRequest {
    /// The font file as bytes (TTF / OTF). Encoded as a JSON `Vec<u8>`
    /// (i.e. an array of byte values) so the contract works equally well
    /// over HTTP/Tauri/WASM without picking a base64 layer.
    pub font_bytes: Vec<u8>,
    pub text: String,
    pub origin: Point2,
    pub height_mm: f64,
    #[serde(default = "default_layer")]
    pub layer: String,
    #[serde(default = "default_color")]
    pub color: i32,
}

fn default_layer() -> String {
    "TEXT".into()
}
fn default_color() -> i32 {
    7
}

/// Response payload — the rendered geometry plus metadata the dialog uses
/// to warn the user when an outline font is paired with the Engraving
/// style.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RenderTextResponse {
    pub segments: Vec<Segment>,
    /// True if the font is a single-line / engraving / Hershey-port
    /// style font. Drives the dialog's "use a single-line font" chip.
    pub single_line: bool,
    /// Family / style names (best-effort). Useful for showing what the
    /// user actually loaded next to the dropdown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family_name: Option<String>,
}

/// Live-preview response — the rendered `TextLayer` segments plus the
/// cached single-line classification. The pipeline produces the same
/// segments at Generate time; this endpoint lets the frontend show the
/// text on the 2D canvas without round-tripping a full pipeline run.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RenderTextLayerResponse {
    pub segments: Vec<Segment>,
    pub single_line: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family_name: Option<String>,
}

/// Cross-transport entry point for live preview. Takes a full
/// [`TextLayer`] (with embedded font bytes) and returns the same
/// segments the pipeline pre-pass would produce, plus the
/// single-line / family-name metadata the UI uses to label the layer.
pub fn render_text_layer_api(layer: &TextLayer) -> crate::Result<RenderTextLayerResponse> {
    let face = Face::parse(&layer.font_bytes, 0).map_err(|e| {
        Error::misconfigured(format!("ttf parse: {e}"))
            .with_hint("Pick a different font for this text layer.")
    })?;
    let single_line = is_single_line_font(&face);
    let family_name = face_family_name(&face);
    let segments = render_text_layer(layer)?;
    Ok(RenderTextLayerResponse {
        segments,
        single_line,
        family_name,
    })
}

/// Cross-transport entry point: parses the font, renders, returns
/// segments + the single-line classification. Errors map to the standard
/// structured [`Error`] (kind=Misconfigured) when the bytes don't parse
/// as a font — the user can recover by picking a different font or
/// installing one.
pub fn render_text_api(req: &RenderTextRequest) -> crate::Result<RenderTextResponse> {
    let face = Face::parse(&req.font_bytes, 0).map_err(|e| {
        Error::misconfigured(format!("ttf parse: {e}"))
            .with_hint("Pick a different font or install one.")
    })?;
    let single_line = is_single_line_font(&face);
    let family_name = face_family_name(&face);
    let segments = render_text(
        &req.font_bytes,
        &req.text,
        req.origin,
        req.height_mm,
        &req.layer,
        req.color,
    )?;
    Ok(RenderTextResponse {
        segments,
        single_line,
        family_name,
    })
}

fn face_family_name(face: &Face) -> Option<String> {
    for entry in face.names() {
        if entry.name_id == ttf_parser::name_id::FAMILY {
            if let Some(s) = entry.to_string() {
                return Some(s);
            }
        }
    }
    None
}

/// Outline-builder accumulator. Splits cubic/quadratic Béziers into line
/// segments via fixed-step subdivision.
struct Walker<'a> {
    /// All accumulated polylines for this glyph. Each contour is its own
    /// inner Vec (`move_to` opens a new one).
    contours: &'a mut Vec<Vec<Point2>>,
    current: Vec<Point2>,
    last: Point2,
    /// Affine transform applied per output point (pixels-per-em scaling +
    /// translation for the glyph's pen position).
    scale: f64,
    origin: Point2,
}

impl<'a> Walker<'a> {
    fn new(contours: &'a mut Vec<Vec<Point2>>, scale: f64, origin: Point2) -> Self {
        Self {
            contours,
            current: Vec::new(),
            last: Point2::new(0.0, 0.0),
            scale,
            origin,
        }
    }

    fn point(&self, x: f32, y: f32) -> Point2 {
        Point2::new(
            self.origin.x + f64::from(x) * self.scale,
            self.origin.y + f64::from(y) * self.scale,
        )
    }

    fn finish_contour(&mut self) {
        if self.current.len() >= 2 {
            self.contours.push(std::mem::take(&mut self.current));
        } else {
            self.current.clear();
        }
    }
}

impl OutlineBuilder for Walker<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.finish_contour();
        let p = self.point(x, y);
        self.last = p;
        self.current.push(p);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.point(x, y);
        self.current.push(p);
        self.last = p;
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        // Sample t∈[0,1] in N steps for a fixed-quality flattening.
        let p1 = self.point(x1, y1);
        let p2 = self.point(x, y);
        let p0 = self.last;
        let n = 16;
        for i in 1..=n {
            let t = f64::from(i) / f64::from(n);
            let mt = 1.0 - t;
            let x = mt * mt * p0.x + 2.0 * mt * t * p1.x + t * t * p2.x;
            let y = mt * mt * p0.y + 2.0 * mt * t * p1.y + t * t * p2.y;
            self.current.push(Point2::new(x, y));
        }
        self.last = p2;
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let p1 = self.point(x1, y1);
        let p2 = self.point(x2, y2);
        let p3 = self.point(x, y);
        let p0 = self.last;
        let n = 24;
        for i in 1..=n {
            let t = f64::from(i) / f64::from(n);
            let mt = 1.0 - t;
            let mt2 = mt * mt;
            let t2 = t * t;
            let x = mt2 * mt * p0.x + 3.0 * mt2 * t * p1.x + 3.0 * mt * t2 * p2.x + t2 * t * p3.x;
            let y = mt2 * mt * p0.y + 3.0 * mt2 * t * p1.y + 3.0 * mt * t2 * p2.y + t2 * t * p3.y;
            self.current.push(Point2::new(x, y));
        }
        self.last = p3;
    }
    fn close(&mut self) {
        if let Some(first) = self.current.first().copied() {
            if let Some(last) = self.current.last().copied() {
                if last.distance(first) > 1e-6 {
                    self.current.push(first);
                }
            }
        }
        self.finish_contour();
    }
}

/// Render a string as flat segments at `origin` (the bottom-left of the
/// first glyph's baseline) at `height` mm. `layer` and `color` decorate the
/// output segments.
pub fn render_text(
    font_bytes: &[u8],
    text: &str,
    origin: Point2,
    height: f64,
    layer: &str,
    color: i32,
) -> crate::Result<Vec<Segment>> {
    let face = Face::parse(font_bytes, 0).map_err(|e| {
        Error::misconfigured(format!("ttf parse: {e}"))
            .with_hint("Pick a different font or install one.")
    })?;
    let units = f64::from(face.units_per_em().max(1));
    let scale = height / units;
    let single_line = is_single_line_font(&face);
    let mut pen = origin;
    let mut out = Vec::new();
    for ch in text.chars() {
        let Some(glyph_id) = face.glyph_index(ch) else {
            // Treat unknown char as a wide space.
            pen.x += height * 0.4;
            continue;
        };
        let mut contours: Vec<Vec<Point2>> = Vec::new();
        {
            let mut walker = Walker::new(&mut contours, scale, pen);
            face.outline_glyph(glyph_id, &mut walker);
            walker.finish_contour();
        }
        if single_line {
            for c in &contours {
                push_polyline_unclosed(c, layer, color, &mut out);
            }
        } else {
            for c in &contours {
                push_polyline_closed(c, layer, color, &mut out);
            }
        }
        let advance = f64::from(face.glyph_hor_advance(glyph_id).unwrap_or(0)) * scale;
        pen.x += advance;
    }
    Ok(out)
}

/// Render a full `TextLayer` (text + font + size + alignment + transform)
/// to flat segments. Handles MTEXT line breaks (`\n`), per-line
/// alignment, letter spacing, line spacing, and a rotation pivot at the
/// layer's `origin`. The output segments live on the synthetic layer
/// `__text_<id>` so ops can target them via `OperationSource::Layers`.
pub fn render_text_layer(layer: &TextLayer) -> crate::Result<Vec<Segment>> {
    let face = Face::parse(&layer.font_bytes, 0).map_err(|e| {
        Error::misconfigured(format!("ttf parse: {e}"))
            .with_hint("Pick a different font for this text layer.")
    })?;
    let single_line = is_single_line_font(&face);
    let units = f64::from(face.units_per_em().max(1));
    let scale = layer.size_mm / units;
    let layer_name = text_layer_synthetic_layer(layer.id);
    // BYLAYER — the canvas uses the assigned-op tint anyway, so the
    // glyph color is mostly cosmetic.
    let color = 7;

    let lines: Vec<&str> = if matches!(layer.kind, TextLayerKind::Mtext) {
        layer.text.split('\n').collect()
    } else {
        vec![layer.text.as_str()]
    };
    let line_height = if layer.line_spacing_mm > 0.0 {
        layer.line_spacing_mm
    } else {
        layer.size_mm * 1.2
    };

    let mut out: Vec<Segment> = Vec::new();
    for (line_idx, line) in lines.iter().enumerate() {
        let line_width =
            measure_line_width(&face, line, scale, layer.size_mm, layer.letter_spacing_mm);
        let x_shift = match layer.alignment {
            TextAlignment::Left => 0.0,
            TextAlignment::Center => -line_width / 2.0,
            TextAlignment::Right => -line_width,
        };
        let line_y = -(line_idx as f64) * line_height;

        let mut pen = Point2::new(x_shift, line_y);
        for ch in line.chars() {
            let Some(glyph_id) = face.glyph_index(ch) else {
                // Unknown char → wide-space placeholder.
                pen.x += layer.size_mm * 0.4 + layer.letter_spacing_mm;
                continue;
            };
            let mut contours: Vec<Vec<Point2>> = Vec::new();
            {
                let mut walker = Walker::new(&mut contours, scale, pen);
                face.outline_glyph(glyph_id, &mut walker);
                walker.finish_contour();
            }
            if single_line {
                for c in &contours {
                    push_polyline_unclosed(c, &layer_name, color, &mut out);
                }
            } else {
                for c in &contours {
                    push_polyline_closed(c, &layer_name, color, &mut out);
                }
            }
            let advance = f64::from(face.glyph_hor_advance(glyph_id).unwrap_or(0)) * scale;
            pen.x += advance + layer.letter_spacing_mm;
        }
    }

    // World transform: rotate around (0, 0) by `rotation_deg`, then
    // translate to layer.origin.
    let pivot = Point2::new(layer.origin.0, layer.origin.1);
    let theta = layer.rotation_deg.to_radians();
    let (cos, sin) = if layer.rotation_deg.abs() > 1e-9 {
        (theta.cos(), theta.sin())
    } else {
        (1.0, 0.0)
    };
    for seg in &mut out {
        seg.start = transform_text_point(seg.start, pivot, cos, sin);
        seg.end = transform_text_point(seg.end, pivot, cos, sin);
    }
    Ok(out)
}

fn measure_line_width(
    face: &Face,
    line: &str,
    scale: f64,
    size_mm: f64,
    letter_spacing_mm: f64,
) -> f64 {
    let mut width = 0.0;
    let mut count = 0usize;
    for ch in line.chars() {
        let advance = if let Some(gid) = face.glyph_index(ch) {
            f64::from(face.glyph_hor_advance(gid).unwrap_or(0)) * scale
        } else {
            size_mm * 0.4
        };
        width += advance;
        count += 1;
    }
    // letter_spacing_mm applies between glyphs (count - 1 gaps).
    if count > 1 {
        width += letter_spacing_mm * (count - 1) as f64;
    }
    width
}

fn transform_text_point(p: Point2, origin: Point2, cos: f64, sin: f64) -> Point2 {
    Point2::new(
        origin.x + p.x * cos - p.y * sin,
        origin.y + p.x * sin + p.y * cos,
    )
}

fn push_polyline_closed(pts: &[Point2], layer: &str, color: i32, out: &mut Vec<Segment>) {
    for w in pts.windows(2) {
        if w[0].distance(w[1]) > 1e-6 {
            out.push(Segment::line(w[0], w[1], layer, color));
        }
    }
    if let (Some(first), Some(last)) = (pts.first(), pts.last()) {
        if first.distance(*last) > 1e-6 {
            out.push(Segment::line(*last, *first, layer, color));
        }
    }
}

fn push_polyline_unclosed(pts: &[Point2], layer: &str, color: i32, out: &mut Vec<Segment>) {
    // For single-line fonts: emit segments without auto-closing the loop.
    // Walking the outline forwards already gives us the centerline; closing
    // it would draw the same path back on itself (the artifact we're
    // detecting around).
    for w in pts.windows(2) {
        if w[0].distance(w[1]) > 1e-6 {
            out.push(Segment::line(w[0], w[1], layer, color));
        }
    }
}

/// True if `face` is a single-line / engraving-only font.
///
/// Two signals — either is sufficient:
///
/// 1. **Zero-area contour signature.** In a normal display font, every
///    glyph contour is a closed loop enclosing a non-trivial filled area.
///    Single-line fonts that have **open** strokes (e.g. the diagonals of
///    'A', 'V', 'Y') encode them as out-and-back retraces, which collapse
///    to zero signed area. Any glyph with such a near-zero-area contour
///    is the smoking gun.
///
/// 2. **Family-name marker.** Common engraving fonts mark themselves in
///    their `family_name` / `full_name` / `postscript_name`: "single-line",
///    "single line", "stick", "engrave", "hershey", "`OSIFont`", etc.
#[must_use] pub fn is_single_line_font(face: &Face) -> bool {
    if family_name_says_single_line(face) {
        return true;
    }
    // Sample more chars + look for any retraced (zero-area) contour.
    const SAMPLE_CHARS: [char; 12] = ['A', 'V', 'X', 'Y', 'Z', 'M', 'N', 'K', 'i', 'l', 'j', '7'];
    let units = f64::from(face.units_per_em().max(1));
    let scale = 1.0 / units;
    let mut samples = 0usize;
    let mut zero_area_glyphs = 0usize;
    for ch in SAMPLE_CHARS {
        let Some(gid) = face.glyph_index(ch) else {
            continue;
        };
        let mut contours: Vec<Vec<Point2>> = Vec::new();
        {
            let mut walker = Walker::new(&mut contours, scale, Point2::new(0.0, 0.0));
            face.outline_glyph(gid, &mut walker);
            walker.finish_contour();
        }
        if contours.is_empty() {
            continue;
        }
        let bbox = bbox_of_contours(&contours);
        let bbox_area = (bbox.2 - bbox.0).abs() * (bbox.3 - bbox.1).abs();
        if bbox_area < 1e-9 {
            continue;
        }
        // A "retraced" contour has signed-area magnitude << its bbox area.
        // We compare to per-contour bbox so a small contour inside a wide
        // glyph still triggers the signal.
        let any_retraced = contours.iter().any(|c| {
            if c.len() < 3 {
                return false;
            }
            let local_bbox = bbox_of_contours(std::slice::from_ref(c));
            let la = (local_bbox.2 - local_bbox.0).abs() * (local_bbox.3 - local_bbox.1).abs();
            if la < 1e-9 {
                return false;
            }
            polygon_area(c).abs() / la < 0.05
        });
        if any_retraced {
            zero_area_glyphs += 1;
        }
        samples += 1;
    }
    // At least one zero-area contour from at least one glyph.
    samples > 0 && zero_area_glyphs > 0
}

fn family_name_says_single_line(face: &Face) -> bool {
    let needle = [
        "single line",
        "single-line",
        "singleline",
        "single stroke",
        "single-stroke",
        "singlestroke",
        "engrav",
        "stick font",
        "stickfont",
        "hershey",
        "rhss",
        "osifont",
        "1-line",
        "one-line",
    ];
    for table_id in [
        ttf_parser::name_id::FAMILY,
        ttf_parser::name_id::FULL_NAME,
        ttf_parser::name_id::POST_SCRIPT_NAME,
        ttf_parser::name_id::TYPOGRAPHIC_FAMILY,
    ] {
        for entry in face.names() {
            if entry.name_id != table_id {
                continue;
            }
            if let Some(name) = entry.to_string() {
                let lc = name.to_ascii_lowercase();
                if needle.iter().any(|n| lc.contains(n)) {
                    return true;
                }
            }
        }
    }
    false
}

fn polygon_area(pts: &[Point2]) -> f64 {
    if pts.len() < 3 {
        return 0.0;
    }
    let mut sum = 0.0;
    for i in 0..pts.len() {
        let a = pts[i];
        let b = pts[(i + 1) % pts.len()];
        sum += a.x * b.y - b.x * a.y;
    }
    sum * 0.5
}

fn bbox_of_contours(contours: &[Vec<Point2>]) -> (f64, f64, f64, f64) {
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for c in contours {
        for p in c {
            if p.x < min_x {
                min_x = p.x;
            }
            if p.y < min_y {
                min_y = p.y;
            }
            if p.x > max_x {
                max_x = p.x;
            }
            if p.y > max_y {
                max_y = p.y;
            }
        }
    }
    (min_x, min_y, max_x, max_y)
}

#[allow(dead_code)]
fn _math_unused() {
    let _ = math::TWO_PI;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("tests/fonts")
            .join(name)
    }

    #[test]
    fn rhss_is_detected_as_single_line() {
        let bytes = std::fs::read(fixture("RhSS.ttf")).expect("RhSS.ttf");
        let face = Face::parse(&bytes, 0).expect("parse");
        assert!(
            is_single_line_font(&face),
            "RhSS should auto-detect as single-line"
        );
    }

    #[test]
    fn dejavu_sans_is_not_single_line() {
        // Regular display font: closed glyph contours, no retraced strokes.
        let bytes = std::fs::read(fixture("DejaVuSans.ttf")).expect("DejaVuSans.ttf");
        let face = Face::parse(&bytes, 0).expect("parse");
        assert!(
            !is_single_line_font(&face),
            "DejaVuSans should NOT auto-detect as single-line"
        );
    }

    #[test]
    fn render_text_api_returns_classification() {
        let bytes = std::fs::read(fixture("DejaVuSans.ttf")).expect("DejaVuSans.ttf");
        let req = RenderTextRequest {
            font_bytes: bytes,
            text: "AB".into(),
            origin: Point2::new(0.0, 0.0),
            height_mm: 12.0,
            layer: "TEXT".into(),
            color: 7,
        };
        let resp = render_text_api(&req).expect("render");
        assert!(!resp.segments.is_empty());
        assert!(!resp.single_line, "DejaVu should classify as outline");

        let rhss = std::fs::read(fixture("RhSS.ttf")).expect("RhSS.ttf");
        let req2 = RenderTextRequest {
            font_bytes: rhss,
            text: "AB".into(),
            origin: Point2::new(0.0, 0.0),
            height_mm: 12.0,
            layer: "TEXT".into(),
            color: 7,
        };
        let resp2 = render_text_api(&req2).expect("render rhss");
        assert!(resp2.single_line, "RhSS should classify as single-line");
    }

    #[test]
    fn rhss_renders_engraving_strokes() {
        let bytes = std::fs::read(fixture("RhSS.ttf")).expect("RhSS.ttf");
        let segs = render_text(&bytes, "AB", Point2::new(0.0, 0.0), 10.0, "0", 7).unwrap();
        assert!(!segs.is_empty(), "should produce strokes");
        // For a single-line font, segments should not close back on themselves.
        // We can't easily assert that without picking the contour back apart;
        // the smoke test (>0 segments + sane bounding box) is enough.
        let mut min_x = f64::INFINITY;
        let mut max_x = f64::NEG_INFINITY;
        for s in &segs {
            min_x = min_x.min(s.start.x).min(s.end.x);
            max_x = max_x.max(s.start.x).max(s.end.x);
        }
        let width = max_x - min_x;
        assert!(
            width > 5.0 && width < 30.0,
            "AB at h=10mm: got width {width}"
        );
    }

    fn dejavu_layer(text: &str) -> TextLayer {
        let bytes = std::fs::read(fixture("DejaVuSans.ttf")).expect("DejaVuSans.ttf");
        TextLayer {
            id: 1,
            kind: TextLayerKind::Text,
            name: "test".into(),
            text: text.into(),
            font_bytes: bytes,
            size_mm: 10.0,
            origin: (0.0, 0.0),
            rotation_deg: 0.0,
            letter_spacing_mm: 0.0,
            line_spacing_mm: 0.0,
            alignment: TextAlignment::Left,
        }
    }

    #[test]
    fn render_text_layer_tags_synthetic_layer_name() {
        let layer = dejavu_layer("AB");
        let segs = render_text_layer(&layer).expect("render");
        assert!(!segs.is_empty());
        assert!(
            segs.iter().all(|s| s.layer == "__text_1"),
            "all segments should land on the synthetic text layer"
        );
    }

    #[test]
    fn render_text_layer_mtext_stacks_lines() {
        let mut layer = dejavu_layer("AB\nCD");
        layer.kind = TextLayerKind::Mtext;
        layer.size_mm = 10.0;
        let segs = render_text_layer(&layer).expect("render");
        // MTEXT with two lines spans more vertical extent than a single
        // line — minimum y should be below -size_mm * (1.2 line-height - 1)
        // because line 2 sits at y ≈ -12 (default line spacing).
        let min_y = segs
            .iter()
            .flat_map(|s| [s.start.y, s.end.y])
            .fold(f64::INFINITY, f64::min);
        assert!(
            min_y < -8.0,
            "two MTEXT lines should reach y < -8 (got {min_y})"
        );
    }

    #[test]
    fn render_text_layer_alignment_shifts_origin() {
        let left = render_text_layer(&dejavu_layer("AB")).expect("left");
        let mut centered = dejavu_layer("AB");
        centered.alignment = TextAlignment::Center;
        let center = render_text_layer(&centered).expect("center");
        let min_left = left
            .iter()
            .flat_map(|s| [s.start.x, s.end.x])
            .fold(f64::INFINITY, f64::min);
        let min_center = center
            .iter()
            .flat_map(|s| [s.start.x, s.end.x])
            .fold(f64::INFINITY, f64::min);
        // Centered text's leftmost glyph sits to the LEFT of left-aligned.
        assert!(
            min_center < min_left,
            "centered min_x ({min_center}) should be left of left-aligned min_x ({min_left})"
        );
    }

    #[test]
    fn render_text_layer_rotation_is_applied_around_origin() {
        let mut layer = dejavu_layer("A");
        layer.rotation_deg = 90.0;
        let segs = render_text_layer(&layer).expect("rotated");
        // After 90° rotation around (0, 0), the 'A' glyph (originally in
        // +x quadrant) lands mostly in the +y quadrant.
        let max_y = segs
            .iter()
            .flat_map(|s| [s.start.y, s.end.y])
            .fold(f64::NEG_INFINITY, f64::max);
        assert!(
            max_y > 4.0,
            "rotated glyph should extend upward (got {max_y})"
        );
    }
}
