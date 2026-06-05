//! ivaCAM core: DXF/SVG import, CAM math, gcode generation.
//!
//! The public surface mirrors `schema/openapi.yaml` so a single set of types
//! drives the JSON contract across HTTP / Tauri / WASM transports.

#![forbid(unsafe_code)]
// iynx: the pre-release gate runs `clippy -W clippy::pedantic`. `doc_markdown`
// is the one pedantic lint not worth satisfying here: a CNC/CAM codebase
// mentions GRBL, LinuxCNC, G-code, G53, M3, DXF, SVG, HPGL etc. in nearly
// every doc comment, and backticking every product/word would add far more
// noise than it removes (and re-redden on each new comment). Allow it
// crate-wide; the rest of pedantic stays enforced.
#![allow(clippy::doc_markdown)]

pub mod cam;
pub mod errors;
pub mod gcode;
pub mod geometry;
pub mod input;
pub mod math;
pub mod pipeline;
pub mod pipeline_cache;
pub mod project;
pub mod schema;
pub mod sim;
pub mod testing;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use errors::{AutoFix, Error, ErrorKind, Result, SourceSpan};
pub use geometry::{BBox, Layer, Point2, Segment, SegmentKind};
pub use input::{ImportOptions, ImportOutput};
pub use sim::heightmap::{Heightmap, ToolProfile};

/// Cross-transport request for the helix auto-fit preview. The frontend
/// sends imported segments + the operation's selected object ids + the
/// active tool diameter; the backend walks the same chaining/combine
/// pipeline the gcode generator does and returns the largest inscribed
/// circle that fits inside the resulting pocket boundary.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct HelixRadiusRequest {
    /// Source segments to derive the closed-pocket boundary from.
    pub segments: Vec<crate::geometry::Segment>,
    /// Object ids from the import that should participate in the
    /// pocket boundary computation. Same shape as `OpSource::Objects.ids`.
    /// Empty = "use all segments as one region".
    pub object_ids: Vec<u32>,
    pub tool_diameter_mm: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct HelixRadiusResponse {
    /// Some(r) when an inscribed circle fits; None when the auto path
    /// would fall back to Ramp/Direct.
    pub radius_mm: Option<f64>,
    /// Short reason string when None (so the UI can show "Pocket too
    /// small for tool" instead of just an empty Auto state).
    pub fallback_reason: Option<String>,
}

/// Stand-alone helix auto-fit preview — the same computation
/// `pipeline::resolve_auto_helix_radius` performs at generation time,
/// exposed for the `OpPropertiesPanel` to display "Auto (detected: 4.2 mm)"
/// before the user clicks Generate.
// `HelixRadiusRequest` carries an owned `Vec<Segment>` that gets borrowed
// in `segments_to_objects`; passing by value is the cleanest signature
// because callers always construct one and discard it, so the borrow
// vs. move is irrelevant — clippy's pass-by-ref suggestion would force
// callers to keep the request alive.
#[allow(clippy::needless_pass_by_value)]
#[must_use]
pub fn compute_helix_radius(req: HelixRadiusRequest) -> HelixRadiusResponse {
    use crate::cam::chaining::{classify_containment, segments_to_objects};
    use crate::cam::source_combine::combine_source_regions;
    use crate::pipeline::fit_helix_radius_for_selection;
    use crate::project::SourceCombine;

    let mut objects = segments_to_objects(&req.segments);
    classify_containment(&mut objects);

    let selected: Vec<usize> = if req.object_ids.is_empty() {
        (0..objects.len()).collect()
    } else {
        req.object_ids
            .iter()
            .filter_map(|id| {
                let idx = (*id as usize).checked_sub(1)?;
                objects.get(idx).map(|_| idx)
            })
            .collect()
    };

    // Run combine_source_regions once just for the "no closed boundary"
    // diagnostic — the shared fit kernel re-runs it internally. Cheap
    // (regions cache key already populated by the per-op path) and the
    // alternative is leaking an empty-vs-None signal through the
    // helper's public surface for one preview-only message.
    if combine_source_regions(&objects, &selected, SourceCombine::Auto).is_empty() {
        return HelixRadiusResponse {
            radius_mm: None,
            fallback_reason: Some("no closed pocket boundary in selection".into()),
        };
    }

    let tool_radius = req.tool_diameter_mm * 0.5;
    match fit_helix_radius_for_selection(&objects, &selected, SourceCombine::Auto, tool_radius) {
        Some(r) => HelixRadiusResponse {
            radius_mm: Some(r),
            fallback_reason: None,
        },
        None => HelixRadiusResponse {
            radius_mm: None,
            fallback_reason: Some("pocket too tight for tool".into()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{compute_helix_radius, HelixRadiusRequest};
    use crate::geometry::{Point2, Segment};

    #[test]
    fn compute_helix_radius_for_50x30_rect() {
        let segments = vec![
            Segment::line(Point2::new(0.0, 0.0), Point2::new(50.0, 0.0), "0", 7),
            Segment::line(Point2::new(50.0, 0.0), Point2::new(50.0, 30.0), "0", 7),
            Segment::line(Point2::new(50.0, 30.0), Point2::new(0.0, 30.0), "0", 7),
            Segment::line(Point2::new(0.0, 30.0), Point2::new(0.0, 0.0), "0", 7),
        ];
        let resp = compute_helix_radius(HelixRadiusRequest {
            segments,
            object_ids: Vec::new(),
            tool_diameter_mm: 6.0,
        });
        let r = resp.radius_mm.expect("expected an inscribed-circle fit");
        assert!(
            (r - 11.5).abs() < 0.1,
            "expected ~11.5 mm helix radius, got {r}",
        );
        assert!(resp.fallback_reason.is_none());
    }

    #[test]
    fn compute_helix_radius_for_tiny_pocket() {
        let segments = vec![
            Segment::line(Point2::new(0.0, 0.0), Point2::new(5.0, 0.0), "0", 7),
            Segment::line(Point2::new(5.0, 0.0), Point2::new(5.0, 5.0), "0", 7),
            Segment::line(Point2::new(5.0, 5.0), Point2::new(0.0, 5.0), "0", 7),
            Segment::line(Point2::new(0.0, 5.0), Point2::new(0.0, 0.0), "0", 7),
        ];
        let resp = compute_helix_radius(HelixRadiusRequest {
            segments,
            object_ids: Vec::new(),
            tool_diameter_mm: 6.0,
        });
        assert!(resp.radius_mm.is_none());
        let reason = resp.fallback_reason.expect("expected a fallback reason");
        assert!(!reason.is_empty(), "fallback_reason should be non-empty");
    }

    #[test]
    fn compute_helix_radius_open_polyline_returns_none() {
        let segments = vec![
            Segment::line(Point2::new(0.0, 0.0), Point2::new(50.0, 0.0), "0", 7),
            Segment::line(Point2::new(50.0, 0.0), Point2::new(50.0, 30.0), "0", 7),
            Segment::line(Point2::new(50.0, 30.0), Point2::new(0.0, 30.0), "0", 7),
        ];
        let resp = compute_helix_radius(HelixRadiusRequest {
            segments,
            object_ids: Vec::new(),
            tool_diameter_mm: 6.0,
        });
        assert!(resp.radius_mm.is_none());
        let reason = resp.fallback_reason.expect("expected a fallback reason");
        assert!(!reason.is_empty(), "fallback_reason should be non-empty");
    }
}
