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

fn default_rapid_subkind() -> RapidCollisionSubkind {
    RapidCollisionSubkind::Tip
}

/// 50eq: which part of the tool struck stock during a rapid — the
/// flutes/tip (the typical "rapid past retract plane" failure) or
/// the shank/holder (broken-collet scenario: cutter tip is in air
/// but the shank drags through tall walls).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RapidCollisionSubkind {
    Tip,
    Shank,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SimWarning {
    RapidThroughMaterial {
        segment_idx: usize,
        worst_x: f64,
        worst_y: f64,
        worst_cell_z: f32,
        rapid_pz: f64,
        /// 50eq: defaults to `Tip` so older serialized warnings
        /// deserialize cleanly. `Shank` flags shank/holder hits — the
        /// broken-collet G0-through-stock pattern.
        #[serde(default = "default_rapid_subkind")]
        subkind: RapidCollisionSubkind,
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
        /// 24ht: every cell that exceeded the holder envelope on this
        /// segment, sorted worst-first. Element 0 mirrors
        /// `worst_x/worst_y/wall_z/required_clearance_mm` for back-compat
        /// callers that only need the worst cell. Older serialized
        /// warnings without this field deserialize to an empty vec via
        /// `serde(default)`.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        cells: Vec<crate::sim::holder_check::HolderCollisionCell>,
    },
    /// wpzm: the simulator coarsened `cell_size` to fit the user's
    /// `maxSimulationCells` budget. Without this surfaced, tool-engagement
    /// issues and small features get silently smoothed away — the user has
    /// no signal that the sim accuracy dropped. The warning carries both
    /// the requested cell size and the actual one so the UI can hint at
    /// "increase Max simulation cells to see this at full resolution".
    CellSizeCoarsened {
        original_cell_size_mm: f64,
        coarsened_cell_size_mm: f64,
        /// Why the coarsening fired — `max_simulation_cells` for the
        /// budget cap (the canonical case), other strings reserved
        /// for future paths.
        reason: String,
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
        // wpzm: coarsening is purely informational — the sim still runs,
        // just at coarser resolution. Surface it so the user knows what
        // happened, but don't escalate to warning/critical.
        SimWarning::CellSizeCoarsened { .. } => Severity::Info,
    }
}

#[must_use]
pub fn kind_str(w: &SimWarning) -> &'static str {
    match w {
        SimWarning::RapidThroughMaterial { .. } => "rapid_through_material",
        SimWarning::FixtureCollision { .. } => "fixture_collision",
        SimWarning::HolderCollision { .. } => "holder_collision",
        SimWarning::CellSizeCoarsened { .. } => "cell_size_coarsened",
    }
}

/// 03zx: end-of-run telemetry for a single sim invocation. Captured at
/// the close of each sim run (driver hook) and logged via
/// `tracing::info`. The frontend can later persist these alongside
/// the actual machine-side time so the user has a quantitative basis
/// for trusting the next prediction (the audit's "sim agrees with
/// reality" gap).
///
/// The shape is deliberately minimal — counts only — so it
/// survives JSON round-trip without dragging in the heightmap data.
/// `warnings_by_kind_count` is a flat (`kind_str` -> count) map so the
/// caller can compare counts without reflection over the discriminant.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SimRunSummary {
    /// Cells whose Z was lowered at least once during the run
    /// (`heightmap.dirty_aabb` cardinality at the end of the run is a
    /// usable upper bound; the caller may pass the exact lowered-count
    /// from `sweep_range` when tracked).
    pub cells_carved: u64,
    /// Per-warning-kind counts (`kind_str` -> count). Stable kind keys
    /// match `kind_str(&SimWarning)` so downstream consumers can
    /// dispatch by kind.
    pub warnings_by_kind_count: std::collections::BTreeMap<String, u32>,
    /// Wall-clock seconds the run took (set by the driver). 0.0 when
    /// the driver doesn't time the run.
    pub total_seconds: f64,
}

impl SimRunSummary {
    /// Compose a summary from a `SimDiagnostics` snapshot plus the
    /// per-run aggregates the caller tracks externally (`cells_carved`
    /// from the sweep loop, `total_seconds` from a wall clock).
    #[must_use]
    pub fn from_diagnostics(
        diagnostics: &SimDiagnostics,
        cells_carved: u64,
        total_seconds: f64,
    ) -> Self {
        let mut counts: std::collections::BTreeMap<String, u32> = std::collections::BTreeMap::new();
        for w in &diagnostics.warnings {
            *counts.entry(kind_str(w).to_string()).or_insert(0) += 1;
        }
        Self {
            cells_carved,
            warnings_by_kind_count: counts,
            total_seconds,
        }
    }

    /// Emit the summary via `tracing::info` so the run gets a single
    /// telemetry line per sim invocation. Keys map to the struct fields
    /// 1:1. The caller is responsible for setting up a tracing
    /// subscriber (the WASM bridge and the CLI both have one).
    pub fn log(&self) {
        // Format the warnings map up front — tracing's structured
        // fields don't natively render BTreeMap, and we want a stable
        // string ordering for log diffing.
        let warnings_str: String = self
            .warnings_by_kind_count
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(",");
        tracing::info!(
            target: "wiac_core::sim::summary",
            cells_carved = self.cells_carved,
            total_seconds = self.total_seconds,
            warnings = %warnings_str,
            "sim run complete",
        );
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SimDiagnostics {
    pub warnings: Vec<SimWarning>,
    /// f1z3: per-`advance` idempotency token for the `partial_advance`
    /// warning gate in `sweep_segment_partial`. When the driver subdivides
    /// a segment finely near `t=0` (e.g. `[0, 1e-10]` then `[1e-10, 0.5]`),
    /// `lo <= 1e-9` is true on both chunks and the warning pass would
    /// fire twice on the same segment. We stash the last `segment_idx` that
    /// fired the gate so the second-or-later chunk against the same
    /// segment is a no-op. Cleared implicitly when the driver moves on
    /// to the next `segment_idx`.
    #[serde(default, skip)]
    pub last_partial_warn_segment_idx: Option<usize>,
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
        self.warnings.iter().filter(|w| kind_str(w) == kind).count()
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
                subkind: RapidCollisionSubkind::Tip,
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
            subkind: RapidCollisionSubkind::Tip,
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
            cells: vec![],
        });
        let s = serde_json::to_string(&d).unwrap();
        let back: SimDiagnostics = serde_json::from_str(&s).unwrap();
        assert_eq!(back.warnings.len(), d.warnings.len());
        assert_eq!(back.count("rapid_through_material"), 1);
        assert_eq!(back.count("fixture_collision"), 1);
        assert_eq!(back.count("holder_collision"), 1);
        assert_eq!(back.critical_count(), 3);
    }

    #[test]
    fn is_clean_empty_and_after_push() {
        let mut d = SimDiagnostics::new();
        assert!(d.is_clean());
        d.push(SimWarning::FixtureCollision {
            segment_idx: 0,
            fixture_id: 1,
            nearest_x: 0.0,
            nearest_y: 0.0,
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
                subkind: RapidCollisionSubkind::Tip,
            }),
            Severity::Critical,
        );
        assert_eq!(
            severity(&SimWarning::CellSizeCoarsened {
                original_cell_size_mm: 0.5,
                coarsened_cell_size_mm: 1.0,
                reason: "max_simulation_cells".into(),
            }),
            Severity::Info,
        );
    }

    /// 03zx: sim-run summary aggregates from a `SimDiagnostics`
    /// snapshot. Counts every kind correctly and passes through
    /// `cells_carved` + `total_seconds` from the caller-tracked aggregates.
    #[test]
    fn sim_run_summary_aggregates_diagnostics() {
        let mut d = SimDiagnostics::new();
        d.push(SimWarning::RapidThroughMaterial {
            segment_idx: 0,
            worst_x: 0.0,
            worst_y: 0.0,
            worst_cell_z: 0.0,
            rapid_pz: 0.0,
            subkind: RapidCollisionSubkind::Tip,
        });
        d.push(SimWarning::FixtureCollision {
            segment_idx: 3,
            fixture_id: 1,
            nearest_x: 0.0,
            nearest_y: 0.0,
        });
        let summary = SimRunSummary::from_diagnostics(&d, 4_200, 12.5);
        assert_eq!(summary.cells_carved, 4_200);
        assert!((summary.total_seconds - 12.5).abs() < 1e-9);
        // BTreeMap counts mirror the kind_str dispatch.
        assert_eq!(
            summary.warnings_by_kind_count.get("rapid_through_material"),
            Some(&1)
        );
        assert_eq!(
            summary.warnings_by_kind_count.get("fixture_collision"),
            Some(&1)
        );
        // Round-trip through JSON survives.
        let json = serde_json::to_string(&summary).unwrap();
        let back: SimRunSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(back, summary);
    }

    #[test]
    fn sim_run_summary_clean_run_is_empty() {
        // Empty diagnostics → no warnings, warnings_by_kind_count
        // empty. The single info-level log call should still fire safely.
        let d = SimDiagnostics::new();
        let s = SimRunSummary::from_diagnostics(&d, 0, 0.0);
        assert_eq!(s.cells_carved, 0);
        assert!(s.warnings_by_kind_count.is_empty());
        // log() must not panic on the empty path.
        s.log();
    }
}
