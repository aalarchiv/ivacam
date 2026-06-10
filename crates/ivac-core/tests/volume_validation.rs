//! Consolidated integration-test binary for the volume-validation
//! harness.
//!
//! Cargo treats every `.rs` file directly under `tests/` as its own
//! integration-test binary — each gets its own link step. On the
//! single-CPU dev box the link cost dominates over execution, and
//! the 5 per-op volume-validation files were each landing ~30 s of
//! link work for ~50 s of test execution. Consolidating them into
//! one binary (each per-op test file becomes a child module under
//! `tests/volume_validation/`) reduces the 5 link steps to 1.
//!
//! `tests/volume_validation.rs` is the integration-test crate root.
//! Crate roots use sibling-style module resolution (`mod foo;`
//! resolves to `tests/foo.rs`), so the per-op files in the
//! `tests/volume_validation/` subdirectory are pulled in via
//! explicit `#[path]` attributes. The `tests/common/` directory
//! is unchanged — `mod common;` finds `tests/common/mod.rs` the
//! same way every other test binary does.
//!
//! Run them as before with:
//!
//!     cargo test -p ivac-core --test volume_validation
//!     cargo test -p ivac-core --test volume_validation chamfer
//!
//! and individual `#[test]` filters still work because `cargo test`
//! flattens module paths to dotted test names.

mod common;

#[path = "volume_validation/chamfer.rs"]
mod chamfer;
#[path = "volume_validation/drill.rs"]
mod drill;
#[path = "volume_validation/gcode_include.rs"]
mod gcode_include;
#[path = "volume_validation/pocket.rs"]
mod pocket;
#[path = "volume_validation/profile.rs"]
mod profile;
#[path = "volume_validation/v5az.rs"]
mod v5az;
