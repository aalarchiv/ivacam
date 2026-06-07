//! Pipeline output-format version.
//!
//! Folded into every cache key — the per-op toolpath cache
//! (`pipeline_cache`) AND the geometry caches in `cam`. It lives in a
//! leaf module so the pure-math `cam` layer can fold it into its own
//! cache keys without depending *upward* on `pipeline_cache`.
//! Re-exported from `pipeline_cache` for existing call sites.

/// Bumped when ANY pipeline output format changes — toolpath segment
/// shape, gcode formatting, comment style, post-processor output,
/// anything observable. Folded into every cache key so a format change
/// cleanly invalidates every entry across every running process, and we
/// never serve stale shapes from before the change.
pub const PIPELINE_VERSION: u32 = 47;
