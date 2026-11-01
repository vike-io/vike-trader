//! Test doubles the unit tests and the integration binaries of this crate share (`test-support` only:
//! this crate's own `tests/` reach it through its self-referential dev-dependency).

use std::cell::Cell;

/// A `should_stop` probe that answers `false` to its first `k` asks and `true` from then on: "fires
/// after chunk `k`", since an ingest asks once at the top of every chunk. It counts its asks, and it
/// latches the way the datahub's does: once it has said stop, it keeps saying it.
pub struct FiresAfter {
    k: usize,
    /// How many times the ingest asked.
    pub asked: Cell<usize>,
}

impl FiresAfter {
    /// A probe that fires after chunk `k`.
    pub fn new(k: usize) -> Self {
        FiresAfter { k, asked: Cell::new(0) }
    }

    /// One ask: `true` once more than `k` asks were made.
    pub fn ask(&self) -> bool {
        self.asked.set(self.asked.get() + 1);
        self.asked.get() > self.k
    }
}
