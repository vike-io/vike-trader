//! Helpers the unit tests of several modules share, compiled only under `cfg(test)`.

use crate::distributions::normal_quantile;
use crate::random::SplitMix64;

/// A standard normal draw, through the one inverse normal CDF: the GARCH and hidden Markov fit tests
/// simulate their returns with it, from this crate's own seeded generator (`rand` is no dependency of a
/// determinism-critical crate, even for its tests).
pub(crate) fn normal(rng: &mut SplitMix64) -> f64 {
    loop {
        let u = rng.next_unit();
        if u > 0.0 {
            return normal_quantile(u);
        }
    }
}
