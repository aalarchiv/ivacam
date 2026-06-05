//! Fixtures — user-declared physical obstacles on the stock the cutter
//! must avoid throughout the program.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A user-declared physical obstacle on the stock the cutter must miss.
/// Lives in stock-relative XY (same frame as the imported geometry) and
/// occupies a Z range; the sim collision test gates on that range first
/// then falls back to a per-shape XY swept-region check.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Fixture {
    pub id: u32,
    pub name: String,
    pub kind: FixtureKind,
    /// Center of the fixture in stock XY (mm).
    pub origin: (f64, f64),
    /// Z range the fixture occupies (relative to stock-top = 0). Typically
    /// `z_top` is positive (a clamp standing above stock); both can be
    /// negative for cleats below.
    pub z_bottom: f64,
    pub z_top: f64,
    /// Visual color in 2D / 3D previews, packed RGBA (0xRRGGBBAA).
    #[serde(default = "default_fixture_color")]
    pub color: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "shape", rename_all = "snake_case")]
pub enum FixtureKind {
    /// Axis-aligned rectangle centered on `origin`.
    Box { width: f64, depth: f64 },
    /// Cylinder centered on `origin`.
    Cylinder { radius: f64 },
    /// Polygon outline in fixture-local coordinates (origin-relative).
    Polygon { vertices: Vec<(f64, f64)> },
}

fn default_fixture_color() -> u32 {
    0xFFA0_50C0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_default_color_when_absent() {
        let json = r#"{
            "id": 5, "name": "x", "kind": {"shape": "cylinder", "radius": 3.0},
            "origin": [1.0, 2.0], "z_bottom": 0.0, "z_top": 5.0
        }"#;
        let f: Fixture = serde_json::from_str(json).unwrap();
        assert_eq!(f.color, default_fixture_color());
    }
}
