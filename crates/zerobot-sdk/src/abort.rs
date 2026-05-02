use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Handle to abort a running query.
///
/// Create via `AbortHandle::new()`, pass the handle to `query`/`query_stream`,
/// and call `abort()` from another task to interrupt.
#[derive(Debug, Clone)]
pub struct AbortHandle {
    aborted: Arc<AtomicBool>,
}

impl AbortHandle {
    pub fn new() -> Self {
        Self {
            aborted: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Signal the running query to stop.
    pub fn abort(&self) {
        self.aborted.store(true, Ordering::SeqCst);
    }

    /// Check if abort has been signaled.
    pub fn is_aborted(&self) -> bool {
        self.aborted.load(Ordering::SeqCst)
    }
}

impl Default for AbortHandle {
    fn default() -> Self {
        Self::new()
    }
}
