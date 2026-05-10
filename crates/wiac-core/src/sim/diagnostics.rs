//! Sim runtime diagnostics: per-segment warnings raised while sweeping the
//! toolpath against the heightmap, fixtures, holders, etc. Distinct from
//! `PipelineWarning` (which fires during gcode generation): sim warnings
//! are tied to playback positions so the playbar can mark them and the
//! 3D scene can flag the offending segment in real time.
//!
//! Severity is derived from the kind, not stored on each warning — the
//! mapping is fixed (collision = critical, heuristic = warning) and lives
//! in `severity()`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SimWarning {
    RapidThroughMaterial {
        segment_idx: usize,
        worst_x: f64,
        worst_y: f64,
        worst_cell_z: f32,
        rapid_pz: f64,
    },
    FixtureCollision {
        segment_idx: usize,
        fixture_id: u32,
        nearest_x: f64,
        nearest_y: f64,
    },
    HolderCollision {
        segment_idx: usize,
        worst_x: f64,
        worst_y: f64,
        wall_z: f32,
        required_clearance_mm: f32,
    },
    EngagementOverload {
        segment_idx: usize,
        engagement_pct: f32,
    },
    DraggingRapids {
        first_segment_idx: usize,
        count: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Critical,
    Warning,
    Info,
}

#[must_use]
pub fn severity(w: &SimWarning) -> Severity {
    match w {
        SimWarning::RapidThroughMaterial { .. }
        | SimWarning::FixtureCollision { .. }
        | SimWarning::HolderCollision { .. } => Severity::Critical,
        SimWarning::EngagementOverload { .. } | SimWarning::DraggingRapids { .. } => {
            Severity::Warning
        }
    }
}

#[must_use]
pub fn kind_str(w: &SimWarning) -> &'static str {
    match w {
        SimWarning::RapidThroughMaterial { .. } => "rapid_through_material",
        SimWarning::FixtureCollision { .. } => "fixture_collision",
        SimWarning::HolderCollision { .. } => "holder_collision",
        SimWarning::EngagementOverload { .. } => "engagement_overload",
        SimWarning::DraggingRapids { .. } => "dragging_rapids",
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SimDiagnostics {
    pub warnings: Vec<SimWarning>,
}

impl SimDiagnostics {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, w: SimWarning) {
        self.warnings.push(w);
    }

    #[must_use]
    pub fn count(&self, kind: &str) -> usize {
        self.warnings
            .iter()
            .filter(|w| kind_str(w) == kind)
            .count()
    }

    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.warnings.is_empty()
    }

    #[must_use]
    pub fn critical_count(&self) -> usize {
        self.warnings
            .iter()
            .filter(|w| severity(w) == Severity::Critical)
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_and_count() {
        let mut d = SimDiagnostics::new();
        for i in 0..5 {
            d.push(SimWarning::RapidThroughMaterial {
                segment_idx: i,
                worst_x: 1.0,
                worst_y: 2.0,
                worst_cell_z: -0.5,
                rapid_pz: 5.0,
            });
        }
        d.push(SimWarning::FixtureCollision {
            segment_idx: 9,
            fixture_id: 1,
            nearest_x: 0.0,
            nearest_y: 0.0,
        });
        assert_eq!(d.count("rapid_through_material"), 5);
        assert_eq!(d.count("fixture_collision"), 1);
        assert_eq!(d.count("holder_collision"), 0);
        assert_eq!(d.warnings.len(), 6);
    }

    #[test]
    fn round_trip() {
        let mut d = SimDiagnostics::new();
        d.push(SimWarning::RapidThroughMaterial {
            segment_idx: 1,
            worst_x: 12.5,
            worst_y: 8.0,
            worst_cell_z: -0.3,
            rapid_pz: 5.0,
        });
        d.push(SimWarning::FixtureCollision {
            segment_idx: 2,
            fixture_id: 7,
            nearest_x: 4.0,
            nearest_y: -1.0,
        });
        d.push(SimWarning::HolderCollision {
            segment_idx: 3,
            worst_x: 0.5,
            worst_y: 0.25,
            wall_z: -2.0,
            required_clearance_mm: 1.5,
        });
        d.push(SimWarning::EngagementOverload {
            segment_idx: 4,
            engagement_pct: 92.0,
        });
        d.push(SimWarning::DraggingRapids {
            first_segment_idx: 5,
            count: 3,
        });
        let s = serde_json::to_string(&d).unwrap();
        let back: SimDiagnostics = serde_json::from_str(&s).unwrap();
        assert_eq!(back.warnings.len(), d.warnings.len());
        assert_eq!(back.count("rapid_through_material"), 1);
        assert_eq!(back.count("fixture_collision"), 1);
        assert_eq!(back.count("holder_collision"), 1);
        assert_eq!(back.count("engagement_overload"), 1);
        assert_eq!(back.count("dragging_rapids"), 1);
        assert_eq!(back.critical_count(), 3);
    }

    #[test]
    fn is_clean_empty_and_after_push() {
        let mut d = SimDiagnostics::new();
        assert!(d.is_clean());
        d.push(SimWarning::DraggingRapids {
            first_segment_idx: 0,
            count: 2,
        });
        assert!(!d.is_clean());
    }

    #[test]
    fn severity_mapping() {
        assert_eq!(
            severity(&SimWarning::RapidThroughMaterial {
                segment_idx: 0,
                worst_x: 0.0,
                worst_y: 0.0,
                worst_cell_z: 0.0,
                rapid_pz: 0.0,
            }),
            Severity::Critical,
        );
        assert_eq!(
            severity(&SimWarning::EngagementOverload {
                segment_idx: 0,
                engagement_pct: 100.0,
            }),
            Severity::Warning,
        );
    }
}
