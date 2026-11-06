//! The timing loop the `harness = false` micro-benchmarks share (`main()`s that print a report, no
//! criterion): vike-binance's `route_frame`, vike-chart's `render`, vike-backtest's `engines` and
//! `tick_replay`. Behind `test-support`, which a bench reaches through its crate's dev edge, so no
//! shipped build compiles it. It lives here for the reason `poll` does: this crate is the
//! test-infrastructure leaf, and the crates below it are clock-pinned or published.

use std::time::Instant;

/// Run `f` (`warmup` + `reps`) times, returning only the timed (post-warmup) elapsed-ns samples.
/// `black_box`ed so the optimizer can't prove `f`'s result is unused and elide the work.
pub fn time_it<T>(reps: usize, warmup: usize, mut f: impl FnMut() -> T) -> Vec<u128> {
    let mut times = Vec::with_capacity(reps);
    for k in 0..(warmup + reps) {
        let t0 = Instant::now();
        let out = f();
        let dt = t0.elapsed().as_nanos();
        std::hint::black_box(out);
        if k >= warmup {
            times.push(dt);
        }
    }
    times
}

/// The upper median of the samples (the element at `len / 2` once sorted). Panics on no samples.
pub fn median(mut t: Vec<u128>) -> u128 {
    t.sort_unstable();
    t[t.len() / 2]
}
