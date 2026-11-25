//! **The poisoned-mutex lock** (decision 0141, amended 2026-11-18): [`lock`], the ONE spelling of
//! "take this lock whether or not a thread that panicked holding it poisoned it", which about thirty
//! private copies in sixteen crates used to carry (the venue bridges, vike-bridge-core, the hubs and
//! their clients, vike-data, vike-market-stream, vike-backtesthub and the desktop's app core).
//!
//! Recovering the guard is right where the guarded value is one a half-finished writer cannot leave
//! torn (a map, a counter, a slot, a flag), which is every caller's case: a panic on one thread must
//! not become a second panic on the next thread that reads the value. A value a panic CAN leave
//! inconsistent keeps `.lock()` and its own answer to the poison.

use std::sync::{Mutex, MutexGuard, PoisonError};

/// `m`, locked, whether or not a panicking holder poisoned it: the poison is dropped from the answer
/// (`PoisonError::into_inner`), never turned into a panic here.
pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "mutex_tests.rs"]
mod tests;
