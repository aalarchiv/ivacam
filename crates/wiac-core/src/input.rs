//! Input pipeline: reads vector files, returns flat [`Segment`]s plus
//! layer metadata + bounding box. The shape mirrors the bridge's
//! `/import` JSON response.

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
    /// Single-line / engraving fonts (RhSS, OSIFont, Hershey ports) are
    /// auto-detected — see [`crate::input::text::is_single_line_font`].
    pub font_path: Option<std::path::PathBuf>,
}

impl ImportOptions {
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
}

/// Dispatch to the right importer based on file extension.
pub fn import_path(path: &Path, opts: &ImportOptions) -> Result<ImportOutput> {
    let suffix = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
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
        other => Err(crate::error::Error::UnsupportedFormat(other.into())),
    }
}

/// Bytes-based dispatch — used by transports that don't have a filesystem
/// (browser WASM) or that already hold the upload buffer in memory.
pub fn import_bytes(filename: &str, bytes: &[u8], opts: &ImportOptions) -> Result<ImportOutput> {
    let suffix = std::path::Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    match suffix.as_str() {
        "dxf" => dxf_in::import_dxf_bytes(filename.to_string(), bytes, opts),
        "svg" => svg_in::import_svg_bytes(filename.to_string(), bytes, opts),
        other => Err(crate::error::Error::UnsupportedFormat(other.into())),
    }
}

/// Build the per-layer summary used by the import response.
pub(crate) fn summarize_layers(
    segments: &[Segment],
    seed_colors: &BTreeMap<String, i32>,
) -> Vec<Layer> {
    let mut counts: BTreeMap<String, (i32, usize)> = BTreeMap::new();
    for seg in segments {
        let entry = counts
            .entry(seg.layer.clone())
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
