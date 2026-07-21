use std::sync::atomic::{AtomicU64, Ordering};

/// Monotonic identifier source owned by the component that owns the IDs.
#[derive(Debug)]
pub struct SimpleCounter(AtomicU64);

impl SimpleCounter {
    pub const fn new() -> Self {
        Self(AtomicU64::new(1))
    }

    #[inline]
    pub fn next(&self) -> u64 {
        self.0.fetch_add(1, Ordering::Relaxed)
    }
}

impl Default for SimpleCounter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_counter_starts_at_one_and_increments() {
        let counter = SimpleCounter::new();

        assert_eq!(counter.next(), 1);
        assert_eq!(counter.next(), 2);
        assert_eq!(counter.next(), 3);
    }

    #[test]
    fn independent_counters_do_not_share_sequences() {
        let first = SimpleCounter::new();
        let second = SimpleCounter::new();

        assert_eq!(first.next(), 1);
        assert_eq!(first.next(), 2);
        assert_eq!(second.next(), 1);
    }
}
