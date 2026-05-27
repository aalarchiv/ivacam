//! Sub-op geometry caches (eb8.8).
//!
//! [`crate::pipeline_cache`] keys the WHOLE-OP output by every field that
//! affects the emitted G-code — depth, feeds, plunge, leads, tabs, the
//! full machine config. That's correct for the output level, but it
//! invalidates aggressively: a depth tweak on a V-Carve op forces
//! re-running `medial_axis` even though the boundary geometry is
//! unchanged. On a 1000-segment contour that's 2–4 s of pure waste
//! every time the user nudges the depth spinner.
//!
//! This module caches the expensive geometry primitives one level below:
//! medial axis, pocket cascade rings. The keys depend only on input
//! SHAPE (and tool radius where applicable), not on depth / feeds /
//! leads / tabs. So depth-only edits are now free, and full regenerates
//! after a deep-cache run only pay the I/O cost of cloning the cached
//! output (~100 µs per medial axis).
//!
//! ## Hashing discipline
//!
//! Same conventions as [`crate::pipeline_cache`]: f64 → bits, sort maps
//! before iterating, hand-written impls because [`Point2`] carries
//! f64. Each cache folds [`crate::pipeline_cache::PIPELINE_VERSION`]
//! into the key so any algorithm change invalidates every entry.
//!
//! ## Capacity
//!
//! Default 64 per cache. A medial axis or cascade-rings result is
//! typically <1 MB; 64 keeps memory bounded while still covering the
//! common authoring loop where the user oscillates between a handful
//! of recently-edited ops.

use std::hash::{Hash, Hasher};
use std::num::NonZeroUsize;
use std::sync::{Mutex, MutexGuard, OnceLock};

use lru::LruCache;
use seahash::SeaHasher;

use crate::cam::vcarve::{medial_axis_cancellable, VPoint, VcRegion};
use crate::geometry::Point2;
use crate::pipeline::CancelToken;
use crate::pipeline_cache::PIPELINE_VERSION;

/// Algorithm discriminator byte. Folded into every key so two different
/// algorithms can't collide even if they happened to hash their inputs
/// identically.
const ALG_MEDIAL_AXIS: u8 = 1;
const ALG_POCKET_CASCADE_WITH_ISLANDS: u8 = 2;

/// LRU capacity per algorithm. 64 entries fits the typical authoring
/// loop (user oscillates between ~3–5 ops at a time, each with 1–3
/// distinct boundary shapes); larger would burn memory without paying
/// for itself.
const CAPACITY: usize = 64;

// ── hash helpers ─────────────────────────────────────────────────────

fn hash_f64<H: Hasher>(v: f64, h: &mut H) {
    h.write_u64(v.to_bits());
}

fn hash_point<H: Hasher>(p: Point2, h: &mut H) {
    hash_f64(p.x, h);
    hash_f64(p.y, h);
}

fn hash_ring<H: Hasher>(ring: &[Point2], h: &mut H) {
    h.write_usize(ring.len());
    for p in ring {
        hash_point(*p, h);
    }
}

fn hash_vc_region<H: Hasher>(region: &VcRegion, h: &mut H) {
    hash_ring(&region.outer, h);
    h.write_usize(region.holes.len());
    for hole in &region.holes {
        hash_ring(hole, h);
    }
}

// ── medial axis ──────────────────────────────────────────────────────

type MedialAxisOutput = Vec<Vec<VPoint>>;

static MEDIAL_AXIS_CACHE: OnceLock<Mutex<LruCache<u64, MedialAxisOutput>>> = OnceLock::new();

fn medial_axis_lock() -> MutexGuard<'static, LruCache<u64, MedialAxisOutput>> {
    MEDIAL_AXIS_CACHE
        .get_or_init(|| {
            Mutex::new(LruCache::new(
                NonZeroUsize::new(CAPACITY).expect("non-zero capacity"),
            ))
        })
        .lock()
        .expect("medial-axis cache mutex poisoned")
}

/// Cached wrapper around [`medial_axis_cancellable`]. Keys by the
/// region's outer ring + every hole; recomputes on a miss, returns the
/// cloned cached value on a hit.
///
/// Cancellation: if `cancel` fires during a miss, the partial result is
/// returned WITHOUT being cached — caching a cancellation would poison
/// the cache and burn the next caller. Hit path doesn't check cancel
/// (a clone is cheap and the caller still gets to react to the flag
/// itself).
pub fn medial_axis_cached(region: &VcRegion, cancel: Option<&CancelToken>) -> MedialAxisOutput {
    let mut h = SeaHasher::new();
    PIPELINE_VERSION.hash(&mut h);
    h.write_u8(ALG_MEDIAL_AXIS);
    hash_vc_region(region, &mut h);
    let key = h.finish();
    if let Some(v) = medial_axis_lock().get(&key).cloned() {
        return v;
    }
    let out = medial_axis_cancellable(region, cancel);
    if !cancel.is_some_and(CancelToken::is_cancelled) {
        medial_axis_lock().put(key, out.clone());
    }
    out
}

// ── pocket cascade with islands ──────────────────────────────────────

type PocketCascadeOutput = Vec<Vec<Point2>>;

static POCKET_CASCADE_CACHE: OnceLock<Mutex<LruCache<u64, PocketCascadeOutput>>> = OnceLock::new();

fn pocket_cascade_lock() -> MutexGuard<'static, LruCache<u64, PocketCascadeOutput>> {
    POCKET_CASCADE_CACHE
        .get_or_init(|| {
            Mutex::new(LruCache::new(
                NonZeroUsize::new(CAPACITY).expect("non-zero capacity"),
            ))
        })
        .lock()
        .expect("pocket-cascade cache mutex poisoned")
}

/// Cached wrapper around [`crate::cam::offsets::pocket_cascade_with_islands`].
/// Keys by the boundary, every island, and the step delta. Re-runs of
/// the same pocket op with different depth / feed / plunge inputs all
/// hit the cache.
#[must_use]
pub fn pocket_cascade_with_islands_cached(
    boundary: &[Point2],
    islands: &[Vec<Point2>],
    delta: f64,
) -> PocketCascadeOutput {
    let mut h = SeaHasher::new();
    PIPELINE_VERSION.hash(&mut h);
    h.write_u8(ALG_POCKET_CASCADE_WITH_ISLANDS);
    hash_ring(boundary, &mut h);
    h.write_usize(islands.len());
    for island in islands {
        hash_ring(island, &mut h);
    }
    hash_f64(delta, &mut h);
    let key = h.finish();
    if let Some(v) = pocket_cascade_lock().get(&key).cloned() {
        return v;
    }
    let out = crate::cam::offsets::pocket_cascade_with_islands(boundary, islands, delta);
    pocket_cascade_lock().put(key, out.clone());
    out
}

// ── test introspection ───────────────────────────────────────────────

#[cfg(test)]
pub(crate) fn medial_axis_cache_len() -> usize {
    medial_axis_lock().len()
}

#[cfg(test)]
pub(crate) fn pocket_cascade_cache_len() -> usize {
    pocket_cascade_lock().len()
}

#[cfg(test)]
pub(crate) fn clear_all_caches() {
    medial_axis_lock().clear();
    pocket_cascade_lock().clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(side: f64) -> Vec<Point2> {
        vec![
            Point2::new(0.0, 0.0),
            Point2::new(side, 0.0),
            Point2::new(side, side),
            Point2::new(0.0, side),
        ]
    }

    fn square_region(side: f64) -> VcRegion {
        VcRegion {
            outer: square(side),
            holes: Vec::new(),
        }
    }

    #[test]
    fn medial_axis_cached_same_region_hits_cache() {
        clear_all_caches();
        let r = square_region(20.0);
        let v1 = medial_axis_cached(&r, None);
        let len_after_miss = medial_axis_cache_len();
        let v2 = medial_axis_cached(&r, None);
        let len_after_hit = medial_axis_cache_len();
        assert_eq!(
            len_after_miss, 1,
            "first call should populate the cache (got {len_after_miss})"
        );
        assert_eq!(
            len_after_hit, 1,
            "second call should hit (no new entry; got {len_after_hit})"
        );
        assert_eq!(
            v1.len(),
            v2.len(),
            "same region must return same chain count"
        );
    }

    #[test]
    fn medial_axis_cached_different_region_misses() {
        clear_all_caches();
        let _ = medial_axis_cached(&square_region(20.0), None);
        let _ = medial_axis_cached(&square_region(30.0), None);
        assert_eq!(medial_axis_cache_len(), 2);
    }

    #[test]
    fn medial_axis_cached_holes_change_key() {
        clear_all_caches();
        let mut r = square_region(20.0);
        let _ = medial_axis_cached(&r, None);
        // Same outer, add a hole — different region, must miss.
        r.holes.push(vec![
            Point2::new(5.0, 5.0),
            Point2::new(10.0, 5.0),
            Point2::new(10.0, 10.0),
            Point2::new(5.0, 10.0),
        ]);
        let _ = medial_axis_cached(&r, None);
        assert_eq!(medial_axis_cache_len(), 2);
    }

    #[test]
    fn pocket_cascade_cached_same_inputs_hits_cache() {
        clear_all_caches();
        let boundary = square(20.0);
        let _ = pocket_cascade_with_islands_cached(&boundary, &[], 2.0);
        let len_after_miss = pocket_cascade_cache_len();
        let _ = pocket_cascade_with_islands_cached(&boundary, &[], 2.0);
        let len_after_hit = pocket_cascade_cache_len();
        assert_eq!(len_after_miss, 1);
        assert_eq!(len_after_hit, 1);
    }

    #[test]
    fn pocket_cascade_cached_different_delta_misses() {
        clear_all_caches();
        let boundary = square(20.0);
        let _ = pocket_cascade_with_islands_cached(&boundary, &[], 2.0);
        let _ = pocket_cascade_with_islands_cached(&boundary, &[], 2.5);
        assert_eq!(pocket_cascade_cache_len(), 2);
    }

    #[test]
    fn pocket_cascade_cached_islands_change_key() {
        clear_all_caches();
        let boundary = square(20.0);
        let island = vec![
            Point2::new(5.0, 5.0),
            Point2::new(10.0, 5.0),
            Point2::new(10.0, 10.0),
            Point2::new(5.0, 10.0),
        ];
        let _ = pocket_cascade_with_islands_cached(&boundary, &[], 2.0);
        let _ = pocket_cascade_with_islands_cached(&boundary, &[island], 2.0);
        assert_eq!(pocket_cascade_cache_len(), 2);
    }
}
