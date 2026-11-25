//! Cooperative deadlines for SQLite and posting iteration.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct SearchControl {
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
}

impl SearchControl {
    pub fn new(timeout: Duration) -> Self {
        Self {
            deadline: Instant::now() + timeout,
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn expired(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed) || Instant::now() >= self.deadline
    }

    /// Interrupt long scans and sorts inside SQLite, including work before the first row.
    pub fn interrupt_sqlite(&self, connection: &rusqlite::Connection) {
        let control = self.clone();
        connection.progress_handler(1024, Some(move || control.expired()));
    }
}
