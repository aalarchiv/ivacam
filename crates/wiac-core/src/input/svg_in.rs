//! SVG importer: walks the usvg tree, applies absolute transforms, and
//! flattens cubic / quadratic Béziers into our LINE/ARC segment shape.
//!
//! Coverage is intentionally conservative — usvg already normalises
//! `<line>`, `<polyline>`, `<polygon>`, `<rect>`, `<circle>`, `<ellipse>`,
//! `<path>`, and `<g>` (with transforms) into a tree of `Path` nodes
//! whose data is a `tiny_skia_path::Path`. We treat every Path the same
//! way regardless of the SVG element it came from.
//!
//! Tessellation: cubic and quadratic curves are flattened by adaptive
//! de Casteljau subdivision until the chordal error is below
//! `ImportOptions::arc_max_step_or_default()`.

// # CAM/sim pedantic-lint exemptions
// usvg traversal walks bounded path-segment indices. Bézier de Casteljau
// subdivision uses (p1, p2, p3) control-point names + (p12, p23) midpoint
// names that follow the algorithm's textbook indices.
#![allow(clippy::cast_precision_loss, clippy::similar_names)]

use std::collections::BTreeMap;

use usvg::{
    tiny_skia_path::{PathSegment, Point as SkPoint},
    Node, Tree,
};

use crate::errors::{Error, Result};
use crate::geometry::{Point2, Segment};
use crate::input::{summarize_layers, ImportOptions, ImportOutput};
use crate::BBox;

const SVG_LAYER: &str = "svg";
const SVG_COLOR: i32 = 7;

pub fn import_svg_bytes(
    filename: String,
    bytes: &[u8],
    opts: &ImportOptions,
) -> Result<ImportOutput> {
    let tree = Tree::from_data(bytes, &usvg::Options::default()).map_err(|e| {
        Error::bad_input(format!("svg parse: {e}"))
            .with_hint("File is not a valid SVG — check for malformed XML or unsupported features.")
    })?;

    let unit_scale = if opts.scale > 0.0 { opts.scale } else { 1.0 };
    let mut ctx = SvgCtx {
        segments: Vec::new(),
        unit_scale,
        chord_step: opts.arc_max_step_or_default(),
        // SVG y axis points down; CAM expects y up. Flip on import using
        // the document height so output sits in [0, h].
        flip_y: tree.size().height(),
        layer_arc: std::sync::Arc::from(SVG_LAYER),
    };
    walk(tree.root(), &mut ctx);

    let segments = ctx.segments;
    let bbox = BBox::from_segments(&segments);
    let layers = summarize_layers(&segments, &BTreeMap::from([(SVG_LAYER.into(), SVG_COLOR)]));
    let (objects, object_meta) = super::object_index(&segments);
    Ok(ImportOutput {
        filename,
        format: "svg".into(),
        segments,
        layers,
        bbox,
        unit_scale,
        warnings: Vec::new(),
        objects,
        object_meta,
        text_entities: Vec::new(),
    })
}

struct SvgCtx {
    segments: Vec<Segment>,
    unit_scale: f64,
    chord_step: f64,
    flip_y: f32,
    /// mieu: pre-interned once at construction so every `Segment::line`
    /// below shares this `Arc` instead of allocating per call.
    layer_arc: std::sync::Arc<str>,
}

fn walk(group: &usvg::Group, ctx: &mut SvgCtx) {
    for child in group.children() {
        match child {
            Node::Group(g) => walk(g, ctx),
            Node::Path(p) => emit_path(p, ctx),
            // Image / Text are intentionally skipped — they don't carry
            // millable geometry. usvg will already have rasterised text
            // glyphs into paths if `text` feature was enabled.
            Node::Image(_) | Node::Text(_) => {}
        }
    }
}

fn emit_path(path: &usvg::Path, ctx: &mut SvgCtx) {
    if !path.is_visible() {
        return;
    }
    let xform = path.abs_transform();
    let unit = ctx.unit_scale;
    let flip = ctx.flip_y;
    let map = |x: f32, y: f32| -> Point2 {
        let mut p = SkPoint::from_xy(x, y);
        xform.map_point(&mut p);
        Point2::new(f64::from(p.x) * unit, f64::from(flip - p.y) * unit)
    };

    let mut current: Option<Point2> = None;
    let mut subpath_start: Option<Point2> = None;

    for seg in path.data().segments() {
        match seg {
            PathSegment::MoveTo(p) => {
                let pt = map(p.x, p.y);
                current = Some(pt);
                subpath_start = Some(pt);
            }
            PathSegment::LineTo(p) => {
                let to = map(p.x, p.y);
                if let Some(from) = current {
                    push_line(ctx, from, to);
                }
                current = Some(to);
            }
            PathSegment::QuadTo(c1, p) => {
                if let Some(from) = current {
                    let c = map(c1.x, c1.y);
                    let to = map(p.x, p.y);
                    flatten_quad(ctx, from, c, to);
                    current = Some(to);
                }
            }
            PathSegment::CubicTo(c1, c2, p) => {
                if let Some(from) = current {
                    let a = map(c1.x, c1.y);
                    let b = map(c2.x, c2.y);
                    let to = map(p.x, p.y);
                    flatten_cubic(ctx, from, a, b, to);
                    current = Some(to);
                }
            }
            PathSegment::Close => {
                if let (Some(from), Some(start)) = (current, subpath_start) {
                    if from != start {
                        push_line(ctx, from, start);
                    }
                }
                current = subpath_start;
            }
        }
    }
}

fn push_line(ctx: &mut SvgCtx, from: Point2, to: Point2) {
    if from.distance(to) < 1e-6 {
        return;
    }
    ctx.segments
        .push(Segment::line(from, to, ctx.layer_arc.clone(), SVG_COLOR));
}

/// Adaptive de Casteljau quadratic flattener. Subdivides until either the
/// chord length is below `chord_step` or recursion limit is hit.
fn flatten_quad(ctx: &mut SvgCtx, p0: Point2, p1: Point2, p2: Point2) {
    flatten_quad_rec(ctx, p0, p1, p2, 0);
}

fn flatten_quad_rec(ctx: &mut SvgCtx, p0: Point2, p1: Point2, p2: Point2, depth: u8) {
    let chord = p0.distance(p2);
    let max_dev = perp_distance(p1, p0, p2);
    if depth >= 16 || (chord <= ctx.chord_step && max_dev < ctx.chord_step * 0.25) {
        push_line(ctx, p0, p2);
        return;
    }
    let q0 = midpoint(p0, p1);
    let q1 = midpoint(p1, p2);
    let m = midpoint(q0, q1);
    flatten_quad_rec(ctx, p0, q0, m, depth + 1);
    flatten_quad_rec(ctx, m, q1, p2, depth + 1);
}

fn flatten_cubic(ctx: &mut SvgCtx, p0: Point2, p1: Point2, p2: Point2, p3: Point2) {
    flatten_cubic_rec(ctx, p0, p1, p2, p3, 0);
}

fn flatten_cubic_rec(ctx: &mut SvgCtx, p0: Point2, p1: Point2, p2: Point2, p3: Point2, depth: u8) {
    let chord = p0.distance(p3);
    let max_dev = perp_distance(p1, p0, p3).max(perp_distance(p2, p0, p3));
    if depth >= 16 || (chord <= ctx.chord_step && max_dev < ctx.chord_step * 0.25) {
        push_line(ctx, p0, p3);
        return;
    }
    let p01 = midpoint(p0, p1);
    let p12 = midpoint(p1, p2);
    let p23 = midpoint(p2, p3);
    let p012 = midpoint(p01, p12);
    let p123 = midpoint(p12, p23);
    let p0123 = midpoint(p012, p123);
    flatten_cubic_rec(ctx, p0, p01, p012, p0123, depth + 1);
    flatten_cubic_rec(ctx, p0123, p123, p23, p3, depth + 1);
}

fn midpoint(a: Point2, b: Point2) -> Point2 {
    Point2::new((a.x + b.x) * 0.5, (a.y + b.y) * 0.5)
}

/// Perpendicular distance from `p` to the line through `a` and `b`.
fn perp_distance(p: Point2, a: Point2, b: Point2) -> f64 {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1e-12 {
        return p.distance(a);
    }
    ((p.x - a.x) * dy - (p.y - a.y) * dx).abs() / len
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_imports_into_four_segments() {
        let svg = br"<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 100 50'>
            <rect x='10' y='10' width='80' height='30'/>
        </svg>";
        let out = import_svg_bytes("rect.svg".into(), svg, &ImportOptions::default()).unwrap();
        assert_eq!(out.format, "svg");
        // <rect> closes back to start, so we get 4 line segments around the box.
        assert_eq!(out.segments.len(), 4, "got {:?}", out.segments);
        // The flipped Y means y=10 (top of SVG) becomes y=40 in CAM space
        // (50 - 10), and y=40 (bottom of SVG) becomes y=10.
        let xs: Vec<f64> = out.segments.iter().map(|s| s.start.x).collect();
        let ys: Vec<f64> = out.segments.iter().map(|s| s.start.y).collect();
        assert!(xs.iter().any(|&x| (x - 10.0).abs() < 0.01));
        assert!(xs.iter().any(|&x| (x - 90.0).abs() < 0.01));
        assert!(ys.iter().any(|&y| (y - 10.0).abs() < 0.01));
        assert!(ys.iter().any(|&y| (y - 40.0).abs() < 0.01));
    }

    #[test]
    fn path_with_curves_flattens_within_tolerance() {
        // Quarter-circle approximated by a single cubic Bézier.
        let svg = br"<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 200 200'>
            <path d='M 100 100 C 100 44.77 144.77 0 200 0' fill='none' stroke='black'/>
        </svg>";
        let out = import_svg_bytes("curve.svg".into(), svg, &ImportOptions::default()).unwrap();
        assert!(
            out.segments.len() > 4,
            "expected multiple flattened chord segments, got {}",
            out.segments.len()
        );
        // Adjacent segments should connect (chord polyline).
        for w in out.segments.windows(2) {
            let gap = w[0].end.distance(w[1].start);
            assert!(gap < 1e-3, "gap between flattened chords: {gap}");
        }
    }

    #[test]
    fn nested_group_transform_is_applied() {
        let svg = br"<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 100 100'>
            <g transform='translate(50 0)'>
                <line x1='0' y1='10' x2='0' y2='90'/>
            </g>
        </svg>";
        let out = import_svg_bytes("g.svg".into(), svg, &ImportOptions::default()).unwrap();
        assert_eq!(out.segments.len(), 1);
        let s = &out.segments[0];
        // The line's x should pick up the +50 translate; y flipped from
        // (10..90) to (10..90) reversed (90..10) — order doesn't matter,
        // just check the X is 50.
        assert!((s.start.x - 50.0).abs() < 0.01);
        assert!((s.end.x - 50.0).abs() < 0.01);
    }
}
