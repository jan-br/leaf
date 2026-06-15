use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

use leaf::prelude::*;

use crate::order::Order;

/// A `@Repository` recording placed order. Lock-light: a saved-count and a next-id
/// counter (no `Mutex`) stand in for a datastore.
#[derive(Debug)]
pub struct OrderRepository {
    next_id: AtomicI64,
    saved: AtomicUsize,
}
register_component!(OrderRepository);

impl OrderRepository {
    fn new() -> Self {
        OrderRepository { next_id: AtomicI64::new(1), saved: AtomicUsize::new(0) }
    }

    /// Allocate a fresh order id.
    pub fn next_id(&self) -> i64 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }

    /// Record a placed order.
    pub fn save(&self, _order: &Order) {
        self.saved.fetch_add(1, Ordering::SeqCst);
    }

    #[cfg(test)]
    pub fn saved_count(&self) -> usize {
        self.saved.load(Ordering::SeqCst)
    }
}
