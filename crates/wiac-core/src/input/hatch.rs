//! Minimal HATCH boundary extractor — `dxf-rs` 0.6 silently drops HATCH
//! entities, so we walk the raw text DXF stream ourselves and emit each
//! boundary path as a chain of LINE / ARC segments. Pattern data (lines /
//! crosshatch / solid fill) is intentionally skipped: for CAM the only
//! useful part of a HATCH is its outline.
//!
//! Format reference: AutoCAD DXF — HATCH entity (groups 91/92/93/72/73/
//! 10/20/11/21/40/50/51 et al.). We support:
//!
//! * Polyline boundary paths (path-type bit 2) with straight + bulge
//!   segments, honoring the `73` closed flag.
//! * Line edges (edge type 1) and circular arc edges (edge type 2).
//! * The common entity layer (group 8) is propagated to every emitted
//!   segment so the per-layer toggle in the UI works.
//!
//! Spline / ellipse boundary edges and binary DXF are out of scope —
//! they emit a warning and skip.

use std::f64::consts::PI;

use crate::geometry::{Point2, Segment};

const HATCH_LAYER_FALLBACK: &str = "HATCH";
const HATCH_COLOR_FALLBACK: i32 = 7;

/// Walk `text` (raw DXF text mode) and append any HATCH boundary segments
/// found into `out`. Layer warnings go into `warnings`. Returns the count
/// of HATCH entities consumed (so the caller can emit a summary warning).
pub fn extract_hatch_boundaries(
    text: &str,
    unit_scale: f64,
    out: &mut Vec<Segment>,
    warnings: &mut Vec<String>,
) -> usize {
    let pairs = pairs_from_text(text);
    let mut i = 0;
    let mut hatch_count = 0;
    while i < pairs.len() {
        let (code, value) = &pairs[i];
        // Group 0 marks an entity boundary; value is the entity type name.
        if *code == 0 && value.eq_ignore_ascii_case("HATCH") {
            i += 1;
            let consumed = parse_hatch(&pairs[i..], unit_scale, out, warnings);
            i += consumed;
            hatch_count += 1;
            continue;
        }
        i += 1;
    }
    hatch_count
}

/// Parse a single HATCH entity body starting at `pairs[0]` (the pair right
/// after the `0/HATCH` marker). Returns the number of pairs consumed —
/// stops when the next `0/...` marker is encountered.
fn parse_hatch(
    pairs: &[(i32, String)],
    unit_scale: f64,
    out: &mut Vec<Segment>,
    warnings: &mut Vec<String>,
) -> usize {
    let mut layer = HATCH_LAYER_FALLBACK.to_string();
    let mut color = HATCH_COLOR_FALLBACK;
    let mut paths_seen = 0;
    let mut i = 0;

    while i < pairs.len() {
        let (code, value) = &pairs[i];
        if *code == 0 {
            // Next entity — give it back to the outer loop.
            return i;
        }
        match *code {
            8 => layer = value.clone(),
            62 => color = value.parse().unwrap_or(HATCH_COLOR_FALLBACK),
            91 => {
                let path_count: i32 = value.parse().unwrap_or(0);
                i += 1;
                while paths_seen < path_count && i < pairs.len() {
                    if pairs[i].0 == 0 {
                        return i;
                    }
                    if pairs[i].0 == 92 {
                        let path_flag: i32 = pairs[i].1.parse().unwrap_or(0);
                        i += 1;
                        let consumed = if path_flag & 2 != 0 {
                            parse_polyline_path(&pairs[i..], unit_scale, &layer, color, out)
                        } else {
                            parse_edge_path(
                                &pairs[i..],
                                unit_scale,
                                &layer,
                                color,
                                out,
                                warnings,
                            )
                        };
                        i += consumed;
                        paths_seen += 1;
                    } else {
                        i += 1;
                    }
                }
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    i
}

/// Polyline-style boundary: `72` has-bulge flag, `73` is-closed flag, `93`
/// vertex count, then sequences of `10/20` (xy) optionally followed by
/// `42` (bulge). Stops at the next field that doesn't belong (typically
/// `97` for the source-boundary handle list).
fn parse_polyline_path(
    pairs: &[(i32, String)],
    unit_scale: f64,
    layer: &str,
    color: i32,
    out: &mut Vec<Segment>,
) -> usize {
    let mut i = 0;
    let mut has_bulge = false;
    let mut is_closed = false;
    let mut vertices: Vec<(f64, f64, f64)> = Vec::new();
    let mut current: Option<(f64, f64, f64)> = None; // x, y, bulge

    while i < pairs.len() {
        let (code, value) = &pairs[i];
        if *code == 0 {
            break;
        }
        match *code {
            72 => has_bulge = value.parse::<i32>().unwrap_or(0) != 0,
            73 => is_closed = value.parse::<i32>().unwrap_or(0) != 0,
            93 => { /* vertex count — just informational */ }
            10 => {
                if let Some(v) = current.take() {
                    vertices.push(v);
                }
                let x: f64 = value.parse().unwrap_or(0.0);
                current = Some((x, 0.0, 0.0));
            }
            20 => {
                if let Some(mut v) = current {
                    v.1 = value.parse().unwrap_or(0.0);
                    current = Some(v);
                }
            }
            42 if has_bulge => {
                if let Some(mut v) = current {
                    v.2 = value.parse().unwrap_or(0.0);
                    current = Some(v);
                }
            }
            // Anything past the vertex stream — done.
            97 | 75 | 76 | 52 | 77 | 78 | 47 | 98 | 91 => break,
            _ => {}
        }
        i += 1;
    }
    if let Some(v) = current.take() {
        vertices.push(v);
    }

    // Emit segments.
    let n = vertices.len();
    if n < 2 {
        return i;
    }
    let last = if is_closed { n } else { n - 1 };
    for k in 0..last {
        let (sx, sy, sb) = vertices[k];
        let (ex, ey, _) = vertices[(k + 1) % n];
        let s = Point2::new(sx * unit_scale, sy * unit_scale);
        let e = Point2::new(ex * unit_scale, ey * unit_scale);
        if s.distance(e) < 1e-9 {
            continue;
        }
        if sb.abs() > 1e-9 {
            out.push(Segment::arc(s, e, sb, None, layer, color));
        } else {
            out.push(Segment::line(s, e, layer, color));
        }
    }
    i
}

/// Edge-style boundary path: `93` edge count, then per-edge `72` edge
/// type plus the type-specific fields. We honor:
///   * type 1 — line: `10/20` start, `11/21` end
///   * type 2 — circular arc: `10/20` center, `40` radius, `50/51`
///     start/end angle, `73` is-CCW
///
/// Edge types 3 (ellipse) and 4 (spline) emit a warning and skip.
fn parse_edge_path(
    pairs: &[(i32, String)],
    unit_scale: f64,
    layer: &str,
    color: i32,
    out: &mut Vec<Segment>,
    warnings: &mut Vec<String>,
) -> usize {
    let mut i = 0;
    let mut expected_edges: i32 = 0;
    let mut edges_done: i32 = 0;
    let mut edge_kind: i32 = 0;
    let mut sx = 0.0_f64;
    let mut sy = 0.0_f64;
    let mut ex = 0.0_f64;
    let mut ey = 0.0_f64;
    let mut radius = 0.0_f64;
    let mut start_ang = 0.0_f64;
    let mut end_ang = 0.0_f64;
    let mut is_ccw = true;

    while i < pairs.len() {
        let (code, value) = &pairs[i];
        if *code == 0 {
            break;
        }
        match *code {
            93 => expected_edges = value.parse().unwrap_or(0),
            72 => {
                // Flush previous edge before reading the next type.
                emit_edge(
                    edge_kind,
                    sx,
                    sy,
                    ex,
                    ey,
                    radius,
                    start_ang,
                    end_ang,
                    is_ccw,
                    unit_scale,
                    layer,
                    color,
                    out,
                    warnings,
                );
                if edge_kind != 0 {
                    edges_done += 1;
                }
                edge_kind = value.parse().unwrap_or(0);
                radius = 0.0;
                start_ang = 0.0;
                end_ang = 0.0;
                is_ccw = true;
            }
            10 => sx = value.parse::<f64>().unwrap_or(0.0),
            20 => sy = value.parse::<f64>().unwrap_or(0.0),
            11 => ex = value.parse::<f64>().unwrap_or(0.0),
            21 => ey = value.parse::<f64>().unwrap_or(0.0),
            40 => radius = value.parse::<f64>().unwrap_or(0.0),
            50 => start_ang = value.parse::<f64>().unwrap_or(0.0),
            51 => end_ang = value.parse::<f64>().unwrap_or(0.0),
            73 => is_ccw = value.parse::<i32>().unwrap_or(1) != 0,
            // Source-boundary handles or pattern fields → done with this path.
            97 | 75 | 76 | 52 | 77 | 78 | 47 | 98 | 91 => break,
            _ => {}
        }
        i += 1;
    }
    // Flush the trailing edge.
    if edge_kind != 0 {
        emit_edge(
            edge_kind,
            sx,
            sy,
            ex,
            ey,
            radius,
            start_ang,
            end_ang,
            is_ccw,
            unit_scale,
            layer,
            color,
            out,
            warnings,
        );
        edges_done += 1;
    }
    if edges_done < expected_edges {
        warnings.push(format!(
            "HATCH boundary on layer '{layer}' expected {expected_edges} edges, parsed {edges_done}"
        ));
    }
    i
}

#[allow(clippy::too_many_arguments)]
fn emit_edge(
    kind: i32,
    sx: f64,
    sy: f64,
    ex: f64,
    ey: f64,
    radius: f64,
    start_ang_deg: f64,
    end_ang_deg: f64,
    is_ccw: bool,
    unit_scale: f64,
    layer: &str,
    color: i32,
    out: &mut Vec<Segment>,
    warnings: &mut Vec<String>,
) {
    match kind {
        0 => { /* nothing buffered yet */ }
        1 => {
            let s = Point2::new(sx * unit_scale, sy * unit_scale);
            let e = Point2::new(ex * unit_scale, ey * unit_scale);
            if s.distance(e) > 1e-9 {
                out.push(Segment::line(s, e, layer, color));
            }
        }
        2 => {
            let center = Point2::new(sx * unit_scale, sy * unit_scale);
            let r = radius * unit_scale;
            let a0 = start_ang_deg.to_radians();
            let a1 = end_ang_deg.to_radians();
            let s = Point2::new(center.x + r * a0.cos(), center.y + r * a0.sin());
            let e = Point2::new(center.x + r * a1.cos(), center.y + r * a1.sin());
            // Bulge = tan(included_angle / 4); sign carries direction.
            let mut sweep = a1 - a0;
            if !is_ccw {
                sweep = -sweep;
            }
            // Normalise to (0, 2π).
            while sweep <= 0.0 {
                sweep += 2.0 * PI;
            }
            while sweep > 2.0 * PI {
                sweep -= 2.0 * PI;
            }
            let mut bulge = (sweep / 4.0).tan();
            if !is_ccw {
                bulge = -bulge;
            }
            out.push(Segment::arc(s, e, bulge, Some(center), layer, color));
        }
        3 | 4 => {
            warnings.push(format!(
                "HATCH boundary on layer '{layer}' uses ellipse/spline edge — skipping (out of scope)"
            ));
        }
        other => {
            warnings.push(format!(
                "HATCH boundary on layer '{layer}' uses unknown edge type {other} — skipping"
            ));
        }
    }
}

/// Tokenise a text-mode DXF into `(code, value)` pairs. Each pair is two
/// adjacent lines; lines are CR/LF tolerant. Codes that fail to parse as
/// integer are silently dropped — those would corrupt the stream anyway.
fn pairs_from_text(text: &str) -> Vec<(i32, String)> {
    let mut out = Vec::new();
    let mut iter = text.lines();
    while let Some(code_line) = iter.next() {
        let code = match code_line.trim().parse::<i32>() {
            Ok(c) => c,
            Err(_) => continue,
        };
        let Some(val_line) = iter.next() else {
            break;
        };
        out.push((code, val_line.trim_end_matches('\r').trim().to_string()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair_lines(pairs: &[(i32, &str)]) -> String {
        let mut out = String::new();
        for (code, value) in pairs {
            out.push_str(&code.to_string());
            out.push('\n');
            out.push_str(value);
            out.push('\n');
        }
        out
    }

    #[test]
    fn polyline_hatch_round_trips() {
        // Closed quad polyline boundary, no bulges.
        let text = pair_lines(&[
            (0, "HATCH"),
            (8, "INSULATION"),
            (62, "5"),
            (91, "1"),  // 1 path
            (92, "7"),  // path flag = polyline | external | outermost
            (72, "0"),  // no bulge
            (73, "1"),  // closed
            (93, "4"),  // 4 vertices
            (10, "0"),
            (20, "0"),
            (10, "10"),
            (20, "0"),
            (10, "10"),
            (20, "5"),
            (10, "0"),
            (20, "5"),
            (97, "0"),
            (0, "ENDSEC"),
        ]);
        let mut out = Vec::new();
        let mut warns = Vec::new();
        let n = extract_hatch_boundaries(&text, 1.0, &mut out, &mut warns);
        assert_eq!(n, 1);
        assert_eq!(out.len(), 4, "expected 4 line segments, got {out:?}");
        assert_eq!(out[0].layer, "INSULATION");
        assert_eq!(out[0].color, 5);
    }

    #[test]
    fn line_edge_hatch_emits_segments() {
        let text = pair_lines(&[
            (0, "HATCH"),
            (8, "0"),
            (91, "1"),
            (92, "1"),  // path flag = external (no polyline bit)
            (93, "2"),  // 2 edges
            (72, "1"),  // line edge
            (10, "0"),
            (20, "0"),
            (11, "10"),
            (21, "0"),
            (72, "1"),  // another line edge
            (10, "10"),
            (20, "0"),
            (11, "10"),
            (21, "10"),
            (97, "0"),
            (0, "ENDSEC"),
        ]);
        let mut out = Vec::new();
        let mut warns = Vec::new();
        extract_hatch_boundaries(&text, 1.0, &mut out, &mut warns);
        assert_eq!(out.len(), 2);
        assert!((out[0].end.x - 10.0).abs() < 1e-6);
        assert!((out[1].end.y - 10.0).abs() < 1e-6);
    }

    #[test]
    fn unit_scale_applies_to_vertex_data() {
        let text = pair_lines(&[
            (0, "HATCH"),
            (91, "1"),
            (92, "2"),  // polyline
            (72, "0"),
            (73, "1"),
            (93, "3"),
            (10, "0"),
            (20, "0"),
            (10, "1"),
            (20, "0"),
            (10, "0"),
            (20, "1"),
            (97, "0"),
        ]);
        let mut out = Vec::new();
        let mut warns = Vec::new();
        extract_hatch_boundaries(&text, 25.4, &mut out, &mut warns);
        assert!(!out.is_empty());
        assert!((out[0].end.x - 25.4).abs() < 1e-6);
    }
}
