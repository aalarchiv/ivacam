//! 2.5D cutting simulator primitives. The heightmap stores Z(x,y) over
//! the stock footprint and tool Z-profiles describe what radius of the
//! cutter surface reaches how far down at a given radial offset.

pub mod diagnostics;
pub mod fixture_check;
pub mod heightmap;
pub mod sweep;
pub mod timing;

pub use diagnostics::{kind_str, severity, Severity, SimDiagnostics, SimWarning};
pub use fixture_check::{check_segment_against_fixtures, FixtureCheck};
