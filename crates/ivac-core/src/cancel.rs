//! Cooperative-cancellation primitive.
//!
//! Shared by the pipeline orchestrator and the long-running CAM
//! primitives it drives. It lives in a leaf module (no dependency on
//! `pipeline`) so the pure-math `cam` layer can consult it without
//! depending *upward* on the orchestrator. Re-exported from `pipeline`
//! for the existing `crate::pipeline::CancelToken` / transport call sites.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Cooperative-cancellation handle. Cloned cheaply; the inner flag is
/// shared. Long inner loops in CAM primitives consult `is_cancelled`
/// at a coarse-enough granularity to bail within ≤200 ms p95.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    #[must_use]
    pub fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}
