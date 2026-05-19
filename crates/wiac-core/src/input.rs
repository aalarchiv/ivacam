//! Input pipeline: reads vector files, returns flat [`Segment`]s plus
//! layer metadata + bounding box. The shape mirrors the bridge's
//! `/import` JSON response.

// # CAM/sim pedantic-lint exemptions
// Importer dispatch casts unit-system enum tags (small constants).
#![allow(clippy::cast_precision_loss)]

use crate::geometry::{BBox, Layer, Segment};
use crate::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

pub mod dxf_in;
pub mod hatch;
pub mod nurbs;
pub mod svg_in;
pub mod text;

/// Optional knobs for any importer. The `args.dxfread_*` flags from the
/// Python plugin map here.
#[derive(Debug, Clone, Default)]
pub struct ImportOptions {
    /// Override unit conversion. 0.0 => auto-detect from file's `$INSUNITS`.
    pub scale: f64,
    /// Skip TEXT/MTEXT entities entirely.
    pub no_text: bool,
    /// Append the DXF color name to layer names so colors become layers.
    pub color_layers: bool,
    /// Whitelist of layer names. Empty => accept all.
    pub select_layers: Vec<String>,
    /// Maximum sweep per arc subdivision step, in radians.
    /// 0.0 keeps arcs as single ARC segments (with bulge) — used for the
    /// CAM pipeline. The Python importer subdivides for rendering.
    pub arc_max_step_rad: f64,
    /// Path to a TTF for rendering TEXT/MTEXT entities. If `None`, the
    /// importer falls back to scanning a few well-known system locations
    /// (`/usr/share/fonts/...`, `/Library/Fonts/...`, `C:\Windows\Fonts\...`).
    /// Single-line / engraving fonts (`RhSS`, `OSIFont`, Hershey ports) are
    /// auto-detected — see [`crate::input::text::is_single_line_font`].
    pub font_path: Option<std::path::PathBuf>,
}

impl ImportOptions {
    #[must_use]
    pub fn arc_max_step_or_default(&self) -> f64 {
        if self.arc_max_step_rad > 0.0 {
            self.arc_max_step_rad
        } else {
            std::f64::consts::FRAC_PI_4 // 45°, matches dxfread.py
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ImportOutput {
    pub filename: String,
    pub format: String,
    pub segments: Vec<Segment>,
    pub layers: Vec<Layer>,
    pub bbox: BBox,
    pub unit_scale: f64,
    pub warnings: Vec<String>,
    /// Per-segment object id from the chaining pass — `objects[i]` is the
    /// 1-based id of the chained object that consumed `segments[i]` (0
    /// means "didn't chain into any object", e.g. an isolated point).
    /// The frontend uses this to do object-level selection in 2D.
    #[serde(default)]
    pub objects: Vec<u32>,
    /// Object metadata (closed flag, layer, color, bbox) keyed by the same
    /// 1-based id — `object_meta[id - 1]` is the entry for object `id`.
    #[serde(default)]
    pub object_meta: Vec<ImportedObject>,
    /// DXF TEXT / MTEXT entities — emitted as editable metadata instead
    /// of being rendered to opaque polylines at import time. The frontend
    /// converts each into a `TextLayer` so the user can edit the content,
    /// font, size, rotation, etc., and the pipeline re-renders them at
    /// Generate. Empty for formats that don't carry text entities (SVG).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub text_entities: Vec<ImportedTextEntity>,
}

/// Metadata for a TEXT / MTEXT entity captured during import. Carries
/// just enough for the frontend to construct a `TextLayer` — the
/// editable inputs the pipeline pre-pass uses to render glyphs.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ImportedTextEntity {
    pub kind: ImportedTextKind,
    /// Original DXF layer name — preserved so the user knows where the
    /// text came from (the synthetic `__text_<id>` name takes over at
    /// pipeline time).
    pub source_layer: String,
    /// Full string. For Mtext, lines are `\n`-separated.
    pub text: String,
    pub size_mm: f64,
    /// Anchor in stock XY (mm).
    pub origin: (f64, f64),
    /// CCW degrees around `origin`.
    #[serde(default)]
    pub rotation_deg: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ImportedTextKind {
    #[serde(rename = "TEXT")]
    Text,
    #[serde(rename = "MTEXT")]
    Mtext,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ImportedObject {
    pub id: u32,
    pub closed: bool,
    #[schemars(with = "String")]
    pub layer: std::sync::Arc<str>,
    pub color: i32,
    pub bbox: BBox,
}

/// Dispatch to the right importer based on file extension.
pub fn import_path(path: &Path, opts: &ImportOptions) -> Result<ImportOutput> {
    let suffix = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();

    match suffix.as_str() {
        "dxf" => dxf_in::import_dxf_path(path, opts),
        "svg" => {
            let bytes = std::fs::read(path)?;
            let filename = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            svg_in::import_svg_bytes(filename, &bytes, opts)
        }
        other => Err(crate::Error::unsupported(format!("file format '{other}'"))
            .with_hint("Supported import formats: DXF, SVG.")),
    }
}

/// Bytes-based dispatch — used by transports that don't have a filesystem
/// (browser WASM) or that already hold the upload buffer in memory.
pub fn import_bytes(filename: &str, bytes: &[u8], opts: &ImportOptions) -> Result<ImportOutput> {
    let suffix = std::path::Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    match suffix.as_str() {
        "dxf" => dxf_in::import_dxf_bytes(filename.to_string(), bytes, opts),
        "svg" => svg_in::import_svg_bytes(filename.to_string(), bytes, opts),
        other => Err(crate::Error::unsupported(format!("file format '{other}'"))
            .with_hint("Supported import formats: DXF, SVG.")),
    }
}

/// Run the chaining pass and produce the per-segment object id + the
/// object metadata table the frontend uses for 2D selection. Returns
/// `(objects, object_meta)` where `objects[i]` is the 1-based id of the
/// chained object containing `segments[i]` (0 = unchained / point /
/// degenerate); `object_meta[id - 1]` carries the chain's metadata.
pub(crate) fn object_index(segments: &[Segment]) -> (Vec<u32>, Vec<ImportedObject>) {
    use crate::cam::chaining::{classify_containment, segments_to_objects};

    let mut chains = segments_to_objects(segments);
    classify_containment(&mut chains);

    let mut objects = vec![0u32; segments.len()];
    let mut meta = Vec::with_capacity(chains.len());
    for (chain_idx, chain) in chains.iter().enumerate() {
        // chain_idx is bounded by chains.len() (file-segment count), well
        // under u32::MAX; the truncation can't fire in practice.
        #[allow(clippy::cast_possible_truncation)]
        let id = chain_idx as u32 + 1;
        // Map back from chain-segment to imported-segment by endpoint
        // coincidence; the chaining pass may flip/reorder edges so
        // direct index identity isn't reliable.
        for chain_seg in &chain.segments {
            for (seg_idx, src) in segments.iter().enumerate() {
                if objects[seg_idx] != 0 {
                    continue;
                }
                let same =
                    approx_pt(src.start, chain_seg.start) && approx_pt(src.end, chain_seg.end);
                let reverse =
                    approx_pt(src.start, chain_seg.end) && approx_pt(src.end, chain_seg.start);
                if same || reverse {
                    objects[seg_idx] = id;
                    break;
                }
            }
        }
        meta.push(ImportedObject {
            id,
            closed: chain.closed,
            layer: chain.layer.clone(),
            color: chain.color,
            bbox: BBox::from_segments(&chain.segments),
        });
    }
    (objects, meta)
}

fn approx_pt(a: crate::geometry::Point2, b: crate::geometry::Point2) -> bool {
    (a.x - b.x).abs() < 1e-6 && (a.y - b.y).abs() < 1e-6
}

/// Build the per-layer summary used by the import response.
pub(crate) fn summarize_layers(
    segments: &[Segment],
    seed_colors: &BTreeMap<String, i32>,
) -> Vec<Layer> {
    let mut counts: BTreeMap<String, (i32, usize)> = BTreeMap::new();
    for seg in segments {
        let entry = counts
            .entry(seg.layer.as_ref().to_owned())
            .or_insert_with(|| (seg.color, 0));
        entry.1 += 1;
    }
    // Backfill colors from the seed map for layers that exist in the DXF
    // but don't carry segments after filtering.
    for (name, color) in seed_colors {
        counts.entry(name.clone()).or_insert((*color, 0));
    }
    counts
        .into_iter()
        .map(|(name, (color, count))| Layer {
            name,
            color,
            segment_count: count,
        })
        .collect()
}
