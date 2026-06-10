//! Layer-name interning helper.
//!
//! [`Segment::layer`] is an [`Arc<str>`] so the clone path is alloc-free.
//! But every `Segment::line(start, end, "0", 7)` call in an importer goes
//! through `Arc::from(&str)` — a fresh allocation per segment. A
//! 5 000-segment DXF on 4 layers performs 5 000 allocations to express
//! 4 distinct names.
//!
//! [`LayerIntern`] caches the [`Arc<str>`] per layer name. The first
//! call for `"0"` allocates; every later call returns a clone of the
//! cached `Arc` (atomic refcount bump, no allocation).
//!
//! Importer integration: own one [`LayerIntern`] in the per-import
//! context (e.g. `dxf_in::ImportCtx`), intern the resolved layer string
//! once per entity, and pass the resulting [`Arc<str>`] (by reference)
//! to every `Segment::*` constructor for that entity.

use std::collections::HashMap;
use std::sync::Arc;

/// Per-import cache of `name → Arc<str>`. Lives for the duration of one
/// importer run; the resulting [`Arc<str>`]s outlive the [`LayerIntern`]
/// itself because they're embedded in the emitted [`crate::geometry::Segment`]s.
#[derive(Debug, Default)]
pub struct LayerIntern {
    map: HashMap<String, Arc<str>>,
}

impl LayerIntern {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Return the cached [`Arc<str>`] for `name`, allocating one on
    /// first sight. Subsequent calls with the same `name` (regardless
    /// of source &str provenance) return a clone of the same `Arc`.
    pub fn intern(&mut self, name: &str) -> Arc<str> {
        // entry().or_insert_with() needs an owned String key. Two-step
        // dance to avoid copying when the key is already present: probe
        // with `get`, fall through to insert only on miss.
        if let Some(existing) = self.map.get(name) {
            return Arc::clone(existing);
        }
        let arc: Arc<str> = Arc::from(name);
        self.map.insert(name.to_string(), Arc::clone(&arc));
        arc
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.map.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intern_same_name_returns_same_arc() {
        let mut intern = LayerIntern::new();
        let a = intern.intern("0");
        let b = intern.intern("0");
        assert!(Arc::ptr_eq(&a, &b), "same name must return the same Arc");
        assert_eq!(intern.len(), 1);
    }

    #[test]
    fn intern_different_names_distinct() {
        let mut intern = LayerIntern::new();
        let _ = intern.intern("0");
        let _ = intern.intern("PROFILE");
        let _ = intern.intern("TEXT");
        assert_eq!(intern.len(), 3);
    }

    #[test]
    fn intern_str_provenance_independent() {
        // Two different &str sources (one heap, one stack literal) must
        // share the Arc as long as they're equal.
        let mut intern = LayerIntern::new();
        let owned = String::from("PROFILE");
        let a = intern.intern(&owned);
        let b = intern.intern("PROFILE");
        assert!(Arc::ptr_eq(&a, &b));
    }
}
