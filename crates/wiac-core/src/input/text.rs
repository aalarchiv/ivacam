//! TEXT / MTEXT → outline path tessellation.
//!
//! Two render strategies:
//! 1. **Outline mode** (the default for normal display fonts) — walks each
//!    glyph's outline and emits filled-shape boundary segments. Good for
//!    profile cuts.
//! 2. **Single-line mode** (auto-detected for engraving fonts like RhSS,
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

use ttf_parser::{Face, OutlineBuilder};

use crate::error::Error;
use crate::geometry::{Point2, Segment};
use crate::math;

/// Outline-builder accumulator. Splits cubic/quadratic Béziers into line
/// segments via fixed-step subdivision.
struct Walker<'a> {
    /// All accumulated polylines for this glyph. Each contour is its own
    /// inner Vec (move_to opens a new one).
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
            self.origin.x + (x as f64) * self.scale,
            self.origin.y + (y as f64) * self.scale,
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

impl<'a> OutlineBuilder for Walker<'a> {
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
            let t = (i as f64) / (n as f64);
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
            let t = (i as f64) / (n as f64);
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
    let face = Face::parse(font_bytes, 0).map_err(|e| Error::Malformed(format!("ttf: {e}")))?;
    let units = face.units_per_em().max(1) as f64;
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
        let advance = face.glyph_hor_advance(glyph_id).unwrap_or(0) as f64 * scale;
        pen.x += advance;
    }
    Ok(out)
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
///    "single line", "stick", "engrave", "hershey", "OSIFont", etc.
pub fn is_single_line_font(face: &Face) -> bool {
    if family_name_says_single_line(face) {
        return true;
    }
    // Sample more chars + look for any retraced (zero-area) contour.
    const SAMPLE_CHARS: [char; 12] = [
        'A', 'V', 'X', 'Y', 'Z', 'M', 'N', 'K', 'i', 'l', 'j', '7',
    ];
    let units = face.units_per_em().max(1) as f64;
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
        assert!(width > 5.0 && width < 30.0, "AB at h=10mm: got width {width}");
    }
}
