//! 2.5D cutting simulator primitives. The heightmap stores Z(x,y) over
//! the stock footprint and tool Z-profiles describe what radius of the
//! cutter surface reaches how far down at a given radial offset.
//!
//! # Arc chord-error floor (biot)
//!
//! The sim sees arcs as pre-tessellated chord [`gcode::preview::ToolpathSegment`]s.
//! `preview::interpret_with_index` walks G2/G3 at ~2° per chord, which gives
//! a chord error of `r · (1 − cos(1°)) ≈ 0.00015 · r` — well below the
//! finishing-pass surface tolerances on hobby CNCs (sub-µm on a 10 mm
//! arc). For finishing-pass scallop inspection on heightmaps, choose a
//! `cell_size` no smaller than this chord error or the visible scallop
//! becomes a sim artifact rather than a real machining outcome.
//!
//! The arc tessellation step is `~2°` in [`crate::gcode::preview`];
//! tighter floors require the preview to emit `Arc` primitives rather
//! than chord [`gcode::preview::ToolpathSegment`]s, which the sim's
//! sweep loop doesn't model today.

pub mod diagnostics;
pub mod fixture_check;
pub mod heightmap;
pub mod holder;
pub mod holder_check;
pub mod rapid_check;
pub mod stl;
pub mod sweep;
pub mod timing;

pub use diagnostics::{kind_str, severity, Severity, SimDiagnostics, SimWarning};
pub use fixture_check::{check_segment_against_fixtures, FixtureCheck};
pub use holder::HolderProfile;
pub use holder_check::{check_segment_holder_against_walls, HolderCheck};
