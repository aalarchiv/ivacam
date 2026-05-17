//! `TextLayer` + related text-engraving types. See [`super::Project`] for
//! how text layers slot into the project model.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Persistent editable text entity. Phase 2 of the text-engraving
/// rework: the pipeline renders these to segments at generate time so
/// edits propagate to gcode without re-baking.
///
/// Distinct from DXF TEXT/MTEXT entities currently parsed into
/// `project.segments` as opaque polylines (phase 4 will route those
/// through `TextLayer` too).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TextLayer {
    pub id: u32,
    pub kind: TextLayerKind,
    /// Display name in the sidebar list.
    pub name: String,
    /// Full string. For `Mtext`, lines are `\n`-separated.
    pub text: String,
    /// TTF/OTF font as a byte vector. JSON serialises as an array of
    /// integers — matches the [`crate::input::text::RenderTextRequest`]
    /// convention so the same transport-agnostic encoding applies.
    pub font_bytes: Vec<u8>,
    pub size_mm: f64,
    /// Anchor in stock XY (mm). Alignment offsets are applied relative
    /// to this point (see [`TextAlignment`]).
    pub origin: (f64, f64),
    #[serde(default)]
    pub rotation_deg: f64,
    /// Extra advance between glyphs in mm. `0.0` (default) = font's
    /// natural advance.
    #[serde(default)]
    pub letter_spacing_mm: f64,
    /// MTEXT line spacing in mm. Ignored when `kind == TextLayerKind::Text`.
    /// `0.0` (default) = ~1.2 × `size_mm`.
    #[serde(default)]
    pub line_spacing_mm: f64,
    #[serde(default = "default_alignment")]
    pub alignment: TextAlignment,
}

fn default_alignment() -> TextAlignment {
    TextAlignment::Left
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum TextLayerKind {
    #[serde(rename = "TEXT")]
    Text,
    #[serde(rename = "MTEXT")]
    Mtext,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TextAlignment {
    Left,
    Center,
    Right,
}

/// Reserved layer name pattern for TextLayer-rendered segments. Ops
/// can target a specific text layer via `OpSource::Layers(vec!["__text_<id>"])`.
#[must_use]
pub fn text_layer_synthetic_layer(id: u32) -> String {
    format!("__text_{id}")
}
