//! DXF importer. Builds on the `dxf` crate (path: `refs/dxf-rs`) which is a
//! pure parser — block expansion, NURBS evaluation, polyline bulge → arcs,
//! and ellipse flattening live here.
//!
//! Closely parallels `viaconstructor/input_plugins/dxfread.py` so the JSON
//! output is byte-for-byte identical for shapes both importers handle.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use dxf::entities::{
    Arc, Circle, Ellipse, Entity, EntityType, Line, LwPolyline, MLine, MText, ModelPoint, Polyline,
    Spline, Text, Vertex,
};
use dxf::enums::Units;
use dxf::Drawing;

use crate::errors::{Error, SourceSpan};
use crate::geometry::{BBox, Point2, Segment};
use crate::input::nurbs;
use crate::input::{summarize_layers, ImportOptions, ImportOutput};
use crate::math;

/// Minimum chord distance below which we drop a degenerate segment.
/// Matches `viaconstructor/input_plugins/dxfread.py` `MIN_DIST = 0.0001`.
const MIN_DIST: f64 = 1e-4;

/// Reserved layer that carries CAM directives in MTEXT entities.
const CAMCFG_LAYER: &str = "_CAMCFG";

/// Top-level entry: read a DXF file from disk and tessellate to segments.
pub fn import_dxf_path(path: &Path, opts: &ImportOptions) -> crate::Result<ImportOutput> {
    let bytes = std::fs::read(path).map_err(|e| {
        Error::io(format!("read {}: {e}", path.display()))
            .with_hint("Check the file exists and is readable.")
    })?;
    let filename = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    import_dxf_bytes(filename, &bytes, opts)
}

/// Bytes-based DXF import. Two passes: first hand the buffer to dxf-rs
/// for the entities it natively supports, then text-mode-scan the same
/// buffer for HATCH entities (which dxf-rs 0.6 silently swallows) and
/// append their boundary geometry to the result.
pub fn import_dxf_bytes(
    filename: String,
    bytes: &[u8],
    opts: &ImportOptions,
) -> crate::Result<ImportOutput> {
    let drawing = Drawing::load(&mut std::io::Cursor::new(bytes)).map_err(|e| {
        let mut err = Error::bad_input(format!("dxf parse: {e}"))
            .with_hint("File is not a valid DXF — try re-exporting from your CAD tool.");
        if let Some(span) = parse_error_span(&filename, &e) {
            err = err.with_span(span);
        }
        err
    })?;
    let mut out = import_drawing(&drawing, filename, opts)?;

    // HATCH boundary recovery pass — only meaningful for text-mode DXFs.
    if let Ok(text) = std::str::from_utf8(bytes) {
        let mut hatch_segments = Vec::new();
        let n = super::hatch::extract_hatch_boundaries(
            text,
            out.unit_scale,
            &mut hatch_segments,
            &mut out.warnings,
        );
        if n > 0 {
            // BBox + per-layer counts need a refresh.
            out.segments.extend(hatch_segments);
            out.bbox = crate::geometry::BBox::from_segments(&out.segments);
            out.layers = super::summarize_layers(
                &out.segments,
                &drawing
                    .layers()
                    .map(|l| (l.name.clone(), color_to_aci(&l.color, 7)))
                    .collect(),
            );
            out.warnings
                .push(format!("recovered {n} HATCH boundary path(s)"));
        }
    }
    Ok(out)
}

/// Importer entry point that takes an already-parsed `Drawing`. Useful for
/// tests and for the bytes-based import path.
pub fn import_drawing(
    drawing: &Drawing,
    filename: String,
    opts: &ImportOptions,
) -> crate::Result<ImportOutput> {
    let mut warnings = Vec::new();

    let unit_scale = if opts.scale > 0.0 {
        opts.scale
    } else {
        unit_scale_from_drawing(drawing).unwrap_or(1.0)
    };

    // Layer color seed: every named layer with its ACI color.
    let mut layer_colors: BTreeMap<String, i32> = BTreeMap::new();
    for layer in drawing.layers() {
        layer_colors.insert(layer.name.clone(), color_to_aci(&layer.color, 7));
    }

    let select_set: Option<std::collections::HashSet<&str>> = if opts.select_layers.is_empty() {
        None
    } else {
        Some(opts.select_layers.iter().map(String::as_str).collect())
    };

    // Eagerly load the font once per import. Empty bytes => no rendering
    // (Phase 4 removed the inline TEXT/MTEXT renderer — text entities
    // now flow through ImportedTextEntity into the frontend, where the
    // user picks a font. The legacy font_bytes load is gone.)
    let mut ctx = ImportCtx {
        opts,
        unit_scale,
        warnings: &mut warnings,
        segments: Vec::new(),
        layer_colors: &layer_colors,
        select: select_set.as_ref(),
        cam_setup: String::new(),
        text_entities: Vec::new(),
    };

    // Build a block index for INSERT expansion. Block lookup is by the
    // block's `name` field.
    let blocks: HashMap<String, &dxf::Block> =
        drawing.blocks().map(|b| (b.name.clone(), b)).collect();

    for entity in drawing.entities() {
        // Drop entities living on the CAM-config layer; their MTEXT carries
        // user-supplied CAM directives we expose via `cam_setup`.
        if entity.common.layer == CAMCFG_LAYER {
            if let EntityType::MText(m) = &entity.specific {
                ctx.cam_setup.push_str(&m.text.replace("\\P", "\n"));
                ctx.cam_setup.push('\n');
            }
            continue;
        }
        ctx.add_entity(entity, &blocks, &Transform2D::identity(), 0);
    }

    let cam_setup_len = ctx.cam_setup.len();
    let segments = std::mem::take(&mut ctx.segments);
    let text_entities = std::mem::take(&mut ctx.text_entities);
    drop(ctx);
    if cam_setup_len > 0 {
        warnings.push(format!(
            "_CAMCFG layer carries {cam_setup_len} bytes of CAM directives (parser stub)"
        ));
    }
    let bbox = BBox::from_segments(&segments);
    let layers = summarize_layers(&segments, &layer_colors);
    let (objects, object_meta) = super::object_index(&segments);

    Ok(ImportOutput {
        filename,
        format: "dxf".into(),
        segments,
        layers,
        bbox,
        unit_scale,
        warnings,
        objects,
        object_meta,
        text_entities,
    })
}

fn unit_scale_from_drawing(drawing: &Drawing) -> Option<f64> {
    let units = drawing.header.default_drawing_units;
    let factor = match units {
        Units::Unitless => return None,
        Units::Inches => 25.4,
        Units::Feet => 304.8,
        Units::Miles => 1_609_344.0,
        Units::Millimeters => 1.0,
        Units::Centimeters => 10.0,
        Units::Meters => 1000.0,
        Units::Kilometers => 1_000_000.0,
        Units::Microinches => 0.0254 / 1000.0,
        Units::Mils => 0.0254,
        Units::Yards => 914.4,
        Units::Angstroms => 1e-7,
        Units::Nanometers => 1e-6,
        Units::Microns => 1e-3,
        Units::Decimeters => 100.0,
        Units::Decameters => 10_000.0,
        Units::Hectometers => 100_000.0,
        Units::Gigameters => 1e12,
        Units::AstronomicalUnits => 1.496e14,
        Units::LightYears => 9.461e18,
        Units::Parsecs => 3.086e19,
        Units::USSurveyFeet => 304.8006096,
        Units::USSurveyInch => 25.40005080,
        Units::USSurveyYard => 914.4018288,
        Units::USSurveyMile => 1_609_347.218694,
    };
    Some(factor)
}

struct ImportCtx<'a> {
    opts: &'a ImportOptions,
    unit_scale: f64,
    warnings: &'a mut Vec<String>,
    segments: Vec<Segment>,
    layer_colors: &'a BTreeMap<String, i32>,
    select: Option<&'a std::collections::HashSet<&'a str>>,
    cam_setup: String,
    /// TEXT / MTEXT metadata captured during import. Emitted to
    /// `ImportOutput.text_entities` so the frontend can turn each into
    /// an editable `TextLayer` instead of consuming pre-rendered glyphs.
    text_entities: Vec<crate::input::ImportedTextEntity>,
}

impl ImportCtx<'_> {
    fn resolve_layer_and_color(&self, entity: &Entity) -> (String, i32) {
        let common = &entity.common;
        let layer = common.layer.clone();
        // BYLAYER: inherit from the layer table.
        let color = if common.color.is_by_layer() {
            *self.layer_colors.get(&layer).unwrap_or(&7)
        } else {
            color_to_aci(&common.color, 7)
        };
        let layer = if self.opts.color_layers {
            format!("{layer}-c{color}")
        } else {
            layer
        };
        (layer, color)
    }

    fn layer_selected(&self, layer: &str) -> bool {
        self.select.map_or(true, |sel| sel.contains(layer))
    }

    fn add_entity(
        &mut self,
        entity: &Entity,
        blocks: &HashMap<String, &dxf::Block>,
        xform: &Transform2D,
        depth: usize,
    ) {
        if depth > 32 {
            self.warnings
                .push("INSERT depth limit (32) reached; cycle?".into());
            return;
        }
        let (layer, color) = self.resolve_layer_and_color(entity);
        if !self.layer_selected(&layer) {
            return;
        }
        match &entity.specific {
            EntityType::Line(line) => self.emit_line(line, &layer, color, xform),
            EntityType::Circle(c) => self.emit_circle(c, &layer, color, xform),
            EntityType::Arc(a) => self.emit_arc(a, &layer, color, xform),
            EntityType::Ellipse(e) => self.emit_ellipse(e, &layer, color, xform),
            EntityType::ModelPoint(p) => self.emit_point(p, &layer, color, xform),
            EntityType::LwPolyline(p) => self.emit_lwpolyline(p, &layer, color, xform),
            EntityType::Polyline(p) => self.emit_polyline(p, &layer, color, xform),
            EntityType::Spline(s) => self.emit_spline(s, &layer, color, xform),
            EntityType::Insert(insert) => {
                let Some(block) = blocks.get(&insert.name) else {
                    self.warnings
                        .push(format!("INSERT references missing block '{}'", insert.name));
                    return;
                };
                let block_xform = xform.compose(&Transform2D::insert(
                    insert.location.x,
                    insert.location.y,
                    insert.x_scale_factor,
                    insert.y_scale_factor,
                    insert.rotation.to_radians(),
                ));
                for sub in &block.entities {
                    self.add_entity(sub, blocks, &block_xform, depth + 1);
                }
            }
            EntityType::MLine(m) => self.emit_mline(m, &layer, color, xform),
            EntityType::Text(t) if !self.opts.no_text => self.emit_text(t, &layer, color, xform),
            EntityType::MText(t) if !self.opts.no_text => self.emit_mtext(t, &layer, color, xform),
            EntityType::AttributeDefinition(_) => {
                // Attribute definitions are template-only; ignored.
            }
            other => {
                self.warnings.push(format!(
                    "unsupported entity {} on layer '{layer}'",
                    entity_type_name(other)
                ));
            }
        }
    }

    fn push_line(&mut self, start: Point2, end: Point2, layer: &str, color: i32) {
        if start.distance(end) > MIN_DIST {
            self.segments.push(Segment::line(start, end, layer, color));
        }
    }

    fn push_arc_segments(
        &mut self,
        start: Point2,
        end: Point2,
        bulge: f64,
        center: Option<Point2>,
        layer: &str,
        color: i32,
    ) {
        if start.distance(end) <= MIN_DIST && bulge.abs() < 1e-12 {
            return;
        }
        let step = self.opts.arc_max_step_or_default();
        if step >= std::f64::consts::TAU || bulge.abs() < 1e-9 {
            // Single segment with bulge — preferred for the CAM core.
            self.segments
                .push(Segment::arc(start, end, bulge, center, layer, color));
            return;
        }
        let pts = math::tessellate_arc(start, end, bulge, step);
        // Decompose each subdivision into a small bulge arc preserving curvature.
        let (_c, a0, a1, _r) = math::bulge_to_arc(start, end, bulge);
        let mut cumulative = a0;
        let total_sweep = {
            let mut s = a1 - a0;
            if bulge > 0.0 && s < 0.0 {
                s += std::f64::consts::TAU;
            }
            if bulge < 0.0 && s > 0.0 {
                s -= std::f64::consts::TAU;
            }
            s
        };
        let per_step = total_sweep / (pts.len() - 1) as f64;
        let center_known = center.or_else(|| Some(math::bulge_to_arc(start, end, bulge).0));
        for i in 0..pts.len() - 1 {
            let s = pts[i];
            let e = pts[i + 1];
            cumulative += per_step;
            let sub_bulge = (per_step * 0.25).tan();
            self.segments
                .push(Segment::arc(s, e, sub_bulge, center_known, layer, color));
        }
        let _ = cumulative; // appeasing the borrow checker on shadowed math
    }

    fn emit_line(&mut self, line: &Line, layer: &str, color: i32, xform: &Transform2D) {
        let s = xform.apply(line.p1.x, line.p1.y);
        let e = xform.apply(line.p2.x, line.p2.y);
        self.push_line(self.scale(s), self.scale(e), layer, color);
    }

    fn emit_point(&mut self, p: &ModelPoint, layer: &str, color: i32, xform: &Transform2D) {
        let pt = self.scale(xform.apply(p.location.x, p.location.y));
        self.segments.push(Segment::point(pt, layer, color));
    }

    fn emit_circle(&mut self, circle: &Circle, layer: &str, color: i32, xform: &Transform2D) {
        let center_world = xform.apply(circle.center.x, circle.center.y);
        let center = self.scale(center_world);
        let radius = circle.radius * self.unit_scale * xform.uniform_scale_factor();
        if radius < MIN_DIST {
            return;
        }
        // Encode as a CIRCLE (start == end at angle 0, bulge 1 on a half).
        // For the renderer/CAM we expand to two semicircles to keep our
        // single-segment-with-bulge invariant simple.
        let p_right = Point2::new(center.x + radius, center.y);
        let p_left = Point2::new(center.x - radius, center.y);
        // Two CCW semicircles via bulge=1.
        let half1 = Segment {
            kind: crate::geometry::SegmentKind::Circle,
            start: p_right,
            end: p_left,
            bulge: 1.0,
            center: Some(center),
            layer: layer.into(),
            color,
        };
        let half2 = Segment {
            kind: crate::geometry::SegmentKind::Circle,
            start: p_left,
            end: p_right,
            bulge: 1.0,
            center: Some(center),
            layer: layer.into(),
            color,
        };
        self.segments.push(half1);
        self.segments.push(half2);
    }

    fn emit_arc(&mut self, arc: &Arc, layer: &str, color: i32, xform: &Transform2D) {
        let center_world = xform.apply(arc.center.x, arc.center.y);
        let center = self.scale(center_world);
        let radius = arc.radius * self.unit_scale * xform.uniform_scale_factor();
        if radius < MIN_DIST {
            return;
        }
        let mut a0 = arc.start_angle.to_radians() + xform.rotation;
        let mut a1 = arc.end_angle.to_radians() + xform.rotation;
        // Keep CCW orientation; if equal -> full circle.
        if (a1 - a0).abs() < 1e-12 {
            a1 = a0 + std::f64::consts::TAU;
        }
        // Normalize so a1 > a0 (DXF arcs are always CCW from start to end).
        while a1 < a0 {
            a1 += std::f64::consts::TAU;
        }
        // Subdivide at most every 45° to keep bulge precision good.
        let step = self.opts.arc_max_step_or_default();
        let total = a1 - a0;
        let n = ((total / step).ceil() as usize).max(1);
        let per = total / n as f64;
        let mut t = a0;
        for _ in 0..n {
            let next = t + per;
            let (s, e, bulge) = math::arc_to_bulge(center, t, next, radius);
            self.segments
                .push(Segment::arc(s, e, bulge, Some(center), layer, color));
            t = next;
        }
        let _ = (xform, &mut a0); // silence unused warning patterns
    }

    fn emit_ellipse(&mut self, e: &Ellipse, layer: &str, color: i32, xform: &Transform2D) {
        // Parametric flattening: r(t) = center + cos(t)*major + sin(t)*minor,
        // t in [start, end].
        let center_world = xform.apply(e.center.x, e.center.y);
        let center = self.scale(center_world);
        // Major axis vector in world space (scaled).
        let mx = e.major_axis.x * self.unit_scale;
        let my = e.major_axis.y * self.unit_scale;
        // Apply xform rotation only (no translation) to the axis vector.
        let (cos_r, sin_r) = (xform.rotation.cos(), xform.rotation.sin());
        let mxp = mx * cos_r - my * sin_r;
        let myp = mx * sin_r + my * cos_r;
        let major_len = (mxp * mxp + myp * myp).sqrt();
        if major_len < MIN_DIST {
            return;
        }
        let minor_len = major_len * e.minor_axis_ratio;
        let phi = myp.atan2(mxp); // axis orientation
        let t0 = e.start_parameter;
        let t1 = if (e.end_parameter - e.start_parameter).abs() < 1e-12 {
            e.start_parameter + std::f64::consts::TAU
        } else {
            e.end_parameter
        };
        let step = self.opts.arc_max_step_or_default();
        let total = t1 - t0;
        let n = ((total.abs() / step).ceil() as usize).max(8);
        let mut prev = ellipse_point(center, major_len, minor_len, phi, t0);
        for i in 1..=n {
            let t = t0 + total * i as f64 / n as f64;
            let p = ellipse_point(center, major_len, minor_len, phi, t);
            self.push_line(prev, p, layer, color);
            prev = p;
        }
    }

    fn emit_lwpolyline(&mut self, poly: &LwPolyline, layer: &str, color: i32, xform: &Transform2D) {
        let n = poly.vertices.len();
        if n < 2 {
            return;
        }
        let closed = poly.is_closed();
        let pts: Vec<(Point2, f64)> = poly
            .vertices
            .iter()
            .map(|v| (self.scale(xform.apply(v.x, v.y)), v.bulge))
            .collect();
        let last = if closed { n } else { n - 1 };
        for i in 0..last {
            let (s, bulge) = pts[i];
            let (e, _) = pts[(i + 1) % n];
            if bulge.abs() > 1e-12 {
                self.push_arc_segments(s, e, bulge, None, layer, color);
            } else {
                self.push_line(s, e, layer, color);
            }
        }
    }

    fn emit_polyline(&mut self, poly: &Polyline, layer: &str, color: i32, xform: &Transform2D) {
        let vertices: Vec<&Vertex> = poly.vertices().collect();
        let n = vertices.len();
        if n < 2 {
            return;
        }
        let closed = poly.is_closed();
        let pts: Vec<(Point2, f64)> = vertices
            .iter()
            .map(|v| (self.scale(xform.apply(v.location.x, v.location.y)), v.bulge))
            .collect();
        let last = if closed { n } else { n - 1 };
        for i in 0..last {
            let (s, bulge) = pts[i];
            let (e, _) = pts[(i + 1) % n];
            if bulge.abs() > 1e-12 {
                self.push_arc_segments(s, e, bulge, None, layer, color);
            } else {
                self.push_line(s, e, layer, color);
            }
        }
    }

    /// Flatten an MLINE into the spine (centerline) plus two parallel
    /// offsets at ±scale_factor/2 along each vertex's miter direction. Style
    /// table support (per-element offsets, caps, joints) is intentionally
    /// out of scope; the typical 2-element default style emerges naturally.
    fn emit_mline(&mut self, m: &MLine, layer: &str, color: i32, xform: &Transform2D) {
        if m.vertices.len() < 2 {
            return;
        }
        let closed = (m.flags & 2) != 0;
        let half = (m.scale_factor * 0.5).abs();
        let n = m.vertices.len();

        // Spine: connect vertices directly.
        let last = if closed { n } else { n - 1 };
        for i in 0..last {
            let v0 = &m.vertices[i];
            let v1 = &m.vertices[(i + 1) % n];
            let a = self.scale(xform.apply(v0.x, v0.y));
            let b = self.scale(xform.apply(v1.x, v1.y));
            self.push_line(a, b, layer, color);
        }

        if half < 1e-9 || m.miter_directions.is_empty() {
            return;
        }

        // Two parallel offsets at ±half along each vertex's miter direction.
        // We expect miter_directions.len() == n; if not, skip the offsets to
        // avoid index-out-of-range surprises with malformed files.
        if m.miter_directions.len() < n {
            self.warnings.push(format!(
                "MLINE on layer '{layer}' had {} vertices but {} miter directions; skipping parallel offsets",
                n,
                m.miter_directions.len()
            ));
            return;
        }
        for sign in [-1.0_f64, 1.0_f64] {
            for i in 0..last {
                let v0 = &m.vertices[i];
                let v1 = &m.vertices[(i + 1) % n];
                let m0 = &m.miter_directions[i];
                let m1 = &m.miter_directions[(i + 1) % n];
                let a =
                    self.scale(xform.apply(v0.x + sign * half * m0.x, v0.y + sign * half * m0.y));
                let b =
                    self.scale(xform.apply(v1.x + sign * half * m1.x, v1.y + sign * half * m1.y));
                self.push_line(a, b, layer, color);
            }
        }
    }

    fn emit_text(&mut self, t: &Text, layer: &str, _color: i32, xform: &Transform2D) {
        let origin_world = xform.apply(t.location.x, t.location.y);
        let origin = self.scale(origin_world);
        let height = t.text_height * self.unit_scale * xform.uniform_scale_factor();
        if height < 1e-6 || t.value.is_empty() {
            return;
        }
        // Phase 4: emit editable metadata instead of rendering glyphs.
        // The frontend turns each entry into a TextLayer (with a default
        // bundled font) so the user can rewrite the content / swap the
        // font / re-place. The pipeline renders glyphs at Generate time.
        let rot_deg = (t.rotation + xform.rotation.to_degrees()) % 360.0;
        self.text_entities.push(crate::input::ImportedTextEntity {
            kind: crate::input::ImportedTextKind::Text,
            source_layer: layer.to_string(),
            text: t.value.clone(),
            size_mm: height,
            origin: (origin.x, origin.y),
            rotation_deg: rot_deg,
        });
    }

    fn emit_mtext(&mut self, t: &MText, layer: &str, _color: i32, xform: &Transform2D) {
        let origin_world = xform.apply(t.insertion_point.x, t.insertion_point.y);
        let origin = self.scale(origin_world);
        let height = t.initial_text_height * self.unit_scale * xform.uniform_scale_factor();
        if height < 1e-6 || t.text.is_empty() {
            return;
        }
        // Normalize MTEXT's `\P` line break sentinel to `\n` — TextLayer
        // uses the standard newline for multi-line content (the pipeline
        // splits on `\n` at render time).
        let text = t.text.replace("\\P", "\n");
        let rot_deg = xform.rotation.to_degrees() % 360.0;
        self.text_entities.push(crate::input::ImportedTextEntity {
            kind: crate::input::ImportedTextKind::Mtext,
            source_layer: layer.to_string(),
            text,
            size_mm: height,
            origin: (origin.x, origin.y),
            rotation_deg: rot_deg,
        });
    }

    fn emit_spline(&mut self, spline: &Spline, layer: &str, color: i32, xform: &Transform2D) {
        let degree = spline.degree_of_curve as usize;
        let knots: Vec<f64> = spline.knot_values.clone();
        let cps: Vec<(f64, f64, f64)> = spline
            .control_points
            .iter()
            .map(|p| (p.x, p.y, 1.0))
            .collect();
        let weights: Vec<f64> = if spline.weight_values.len() == cps.len() {
            spline.weight_values.clone()
        } else {
            vec![1.0; cps.len()]
        };
        let pts = nurbs::flatten(degree, &knots, &cps, &weights, 64);
        let pts: Vec<Point2> = pts
            .into_iter()
            .map(|(x, y)| self.scale(xform.apply(x, y)))
            .collect();
        for w in pts.windows(2) {
            self.push_line(w[0], w[1], layer, color);
        }
    }

    fn scale(&self, p: Point2) -> Point2 {
        Point2::new(p.x * self.unit_scale, p.y * self.unit_scale)
    }
}

fn ellipse_point(center: Point2, major: f64, minor: f64, phi: f64, t: f64) -> Point2 {
    // Position on an ellipse with semi-axes (major, minor) rotated by `phi`.
    let cos_t = t.cos();
    let sin_t = t.sin();
    let x = major * cos_t;
    let y = minor * sin_t;
    let cos_p = phi.cos();
    let sin_p = phi.sin();
    Point2::new(
        center.x + x * cos_p - y * sin_p,
        center.y + x * sin_p + y * cos_p,
    )
}

fn entity_type_name(t: &EntityType) -> &'static str {
    match t {
        EntityType::Body(_) => "BODY",
        EntityType::Face3D(_) => "3DFACE",
        EntityType::Helix(_) => "HELIX",
        EntityType::Image(_) => "IMAGE",
        EntityType::Leader(_) => "LEADER",
        EntityType::Light(_) => "LIGHT",
        EntityType::MLine(_) => "MLINE",
        EntityType::Ole2Frame(_) => "OLE2FRAME",
        EntityType::OleFrame(_) => "OLEFRAME",
        EntityType::Solid3D(_) => "3DSOLID",
        EntityType::ProxyEntity(_) => "PROXY",
        EntityType::Ray(_) => "RAY",
        EntityType::Region(_) => "REGION",
        EntityType::Section(_) => "SECTION",
        EntityType::Shape(_) => "SHAPE",
        EntityType::Solid(_) => "SOLID",
        EntityType::Tolerance(_) => "TOLERANCE",
        EntityType::Trace(_) => "TRACE",
        EntityType::Wipeout(_) => "WIPEOUT",
        EntityType::XLine(_) => "XLINE",
        _ => "UNKNOWN",
    }
}

/// Convert a `dxf::Color` to an AutoCAD ACI integer (1..=255). For special
/// values (BYLAYER, BYBLOCK, off, BYENTITY) returns the supplied default.
fn color_to_aci(color: &dxf::Color, default: i32) -> i32 {
    if let Some(idx) = color.index() {
        idx as i32
    } else {
        default
    }
}

/// 2D affine transform used to expand INSERT block references.
#[derive(Debug, Clone, Copy)]
struct Transform2D {
    /// Column-major 2x3 matrix: [a, b, tx; c, d, ty]
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    tx: f64,
    ty: f64,
    rotation: f64,
}

impl Transform2D {
    fn identity() -> Self {
        Self {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            tx: 0.0,
            ty: 0.0,
            rotation: 0.0,
        }
    }

    fn insert(tx: f64, ty: f64, sx: f64, sy: f64, rot: f64) -> Self {
        let cos_r = rot.cos();
        let sin_r = rot.sin();
        Self {
            a: sx * cos_r,
            b: -sy * sin_r,
            c: sx * sin_r,
            d: sy * cos_r,
            tx,
            ty,
            rotation: rot,
        }
    }

    fn apply(&self, x: f64, y: f64) -> Point2 {
        Point2::new(
            self.a * x + self.b * y + self.tx,
            self.c * x + self.d * y + self.ty,
        )
    }

    fn compose(&self, other: &Self) -> Self {
        // self ∘ other: first apply `other`, then `self`.
        let a = self.a * other.a + self.b * other.c;
        let b = self.a * other.b + self.b * other.d;
        let c = self.c * other.a + self.d * other.c;
        let d = self.c * other.b + self.d * other.d;
        let tx = self.a * other.tx + self.b * other.ty + self.tx;
        let ty = self.c * other.tx + self.d * other.ty + self.ty;
        Self {
            a,
            b,
            c,
            d,
            tx,
            ty,
            rotation: self.rotation + other.rotation,
        }
    }

    fn uniform_scale_factor(&self) -> f64 {
        // Effective isotropic scale (sqrt of det). Used to scale circle/arc
        // radii through INSERT transforms.
        (self.a * self.d - self.b * self.c).abs().sqrt()
    }
}

/// dxf-rs surfaces line numbers in its `Display` text but not as a
/// structured field. Pull a "line N" hint when present so the frontend
/// can point at the offending line.
fn parse_error_span(filename: &str, err: &dxf::DxfError) -> Option<SourceSpan> {
    let s = err.to_string();
    let line = s
        .split(|c: char| !c.is_ascii_digit())
        .filter_map(|t| t.parse::<u32>().ok())
        .find(|n| *n > 0)?;
    Some(SourceSpan {
        file: filename.to_string(),
        line,
        column: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("refs/viaconstructor/tests/data");
        root.join(name)
    }

    #[test]
    fn imports_simple_dxf() {
        let path = fixture("simple.dxf");
        let opts = ImportOptions::default();
        let out = import_dxf_path(&path, &opts).expect("import");
        assert!(!out.segments.is_empty(), "should have segments");
        assert!(out.bbox.width() > 0.0);
        assert!(out.bbox.height() > 0.0);
        assert_eq!(out.format, "dxf");
    }

    #[test]
    fn imports_all_dxf_emits_text_entities() {
        // Phase 4: TEXT/MTEXT entities flow through ImportOutput
        // .text_entities (editable metadata) instead of being rendered
        // to opaque polylines. all.dxf contains a single "Via" TEXT
        // entity at the origin.
        let path = fixture("all.dxf");
        let opts = ImportOptions::default();
        let out = import_dxf_path(&path, &opts).expect("import");
        assert!(
            !out.text_entities.is_empty(),
            "all.dxf should expose at least one editable text entity"
        );
        let entity = &out.text_entities[0];
        assert!(
            !entity.text.is_empty(),
            "text entity should carry the original string"
        );
        assert!(entity.size_mm > 0.0, "size_mm must be positive");
    }

    #[test]
    fn bad_dxf_returns_structured_io_error() {
        let opts = ImportOptions::default();
        let err = import_dxf_bytes("garbage.dxf".into(), b"not a dxf file at all", &opts)
            .expect_err("bad dxf should error");
        assert!(
            matches!(
                err.kind,
                crate::errors::ErrorKind::BadInput | crate::errors::ErrorKind::Io
            ),
            "kind={:?}",
            err.kind
        );
        assert!(
            err.message.to_lowercase().contains("dxf"),
            "message={}",
            err.message
        );
    }
}
