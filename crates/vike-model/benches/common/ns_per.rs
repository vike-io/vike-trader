//! The timing loop every bench of this crate shares (`#[path]`-included by each `[[bench]]`).

use std::time::Instant;

/// ns/op over `iters` iterations, after a warm-up of an eighth as many.
pub fn ns_per<F: FnMut()>(iters: u64, mut f: F) -> f64 {
    for _ in 0..(iters / 8).max(1) {
        f();
    }
    let t = Instant::now();
    for _ in 0..iters {
        f();
    }
    t.elapsed().as_nanos() as f64 / iters as f64
}
