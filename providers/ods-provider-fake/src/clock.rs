//! A clock that only moves when told to.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

/// A shared, manually advanced clock. Clones share the same time.
#[derive(Debug, Clone)]
pub struct FakeClock(Arc<Mutex<SystemTime>>);

impl FakeClock {
    /// A clock starting at a fixed instant (2026-01-01T00:00:00Z), so tests are
    /// deterministic.
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(
            SystemTime::UNIX_EPOCH + Duration::from_secs(1_767_225_600),
        )))
    }

    /// The current time.
    pub fn now(&self) -> SystemTime {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Moves time forward.
    pub fn advance(&self, by: Duration) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) += by;
    }
}

impl Default for FakeClock {
    fn default() -> Self {
        Self::new()
    }
}
