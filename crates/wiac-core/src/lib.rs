//! wiaConstructor core: DXF/SVG import, CAM math, gcode generation.
//!
//! The public surface mirrors `schema/openapi.yaml` so a single set of types
//! drives the JSON contract across HTTP / Tauri / WASM transports.

#![forbid(unsafe_code)]

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
    /// pocket boundary computation. Same shape as `OperationSource::Objects.ids`.
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
#[must_use] pub fn compute_helix_radius(req: HelixRadiusRequest) -> HelixRadiusResponse {
    use crate::cam::chaining::{classify_containment, segments_to_objects};
    use crate::cam::source_combine::combine_source_regions;
    use crate::cam::vcarve::VcRegion;
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

    let regions = combine_source_regions(&objects, &selected, SourceCombine::Auto);
    if regions.is_empty() {
        return HelixRadiusResponse {
            radius_mm: None,
            fallback_reason: Some("no closed pocket boundary in selection".into()),
        };
    }

    let tool_radius = req.tool_diameter_mm * 0.5;
    let mut best: Option<f64> = None;
    for region in &regions {
        if region.boundary.len() < 3 {
            continue;
        }
        let vc_region = VcRegion {
            outer: region.boundary.clone(),
            holes: region.holes.clone(),
        };
        if let Some((_, _, r)) = crate::cam::inscribed::inscribed_circle(&vc_region, tool_radius) {
            best = Some(best.map_or(r, |prev| prev.max(r)));
        }
    }

    match best {
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
