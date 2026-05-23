//! 7z7w: Backwards-compatibility re-export. The implementation lives
//! in [`super::face_mill_overlay`] now that the module name reflects
//! the actual operation (a face-mill helical-spiral overlay, not
//! thread-whirling). Existing callers (`gcode::wirbeln::*`,
//! serialized tool flags, frontend strings) keep working through this
//! thin alias.
//!
//! **True thread-whirling** — a multi-tooth ring of inserts chasing a
//! thread profile on a lathe — is a separate, deferred operation; see
//! follow-up issue filed against the audit.

pub use super::face_mill_overlay::{
    apply_wirbeln, apply_wirbeln_with_state, schritte_for_radius, WirbelnParams, WirbelnState,
};
