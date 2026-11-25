//! Market-microstructure measures over bars, each written from its published definition: Kyle's
//! lambda (`kyle_lambda`, the price change per unit of signed volume), Amihud's illiquidity
//! (`amihud_illiquidity`, the absolute return per unit of traded value) and Roll's spread
//! (`roll_spread`, the bid-ask spread implied by the negative serial covariance of price changes).
//! Cores and types live here; the rows are in `registry/volume.rs` (`kyle_lambda`,
//! `amihud_illiquidity`) and `registry/statistics.rs` (`roll_spread`).
//!
//! Sources:
//!
//! - Albert S. Kyle, "Continuous Auctions and Insider Trading", Econometrica 53(6), 1985,
//!   1315-1335: `lambda`, the price impact of net order flow.
//! - Marcos López de Prado, "Advances in Financial Machine Learning", Wiley, 2018, chapter 19
//!   ("Microstructural Features"): Kyle's lambda estimated by the regression
//!   `dp[t] = lambda (b[t] V[t]) + e[t]` of the price change on the signed volume.
//! - Yakov Amihud, "Illiquidity and stock returns: cross-section and time-series effects", Journal
//!   of Financial Markets 5(1), 2002, 31-56: `ILLIQ = (1/D) sum |R[d]| / VOLD[d]`, the mean over
//!   the `D` days that traded.
//! - Richard Roll, "A Simple Implicit Measure of the Effective Bid-Ask Spread in an Efficient
//!   Market", Journal of Finance 39(4), 1984, 1127-1139: `spread = 2 sqrt(-cov(dp[t], dp[t-1]))`.
//!
//! No arithmetic here is new; each measure is assembled from cores the crate already runs, so no
//! rolling sum, regression or buy/sell rule exists twice:
//!
//! - the rolling regression and the rolling covariance are `crate::fast_r::CoWindow`, the window of
//!   pairs behind `beta`, `correl` and `half_life`, and Kyle's lambda is `beta`'s own slope
//!   (`vike_math::regression::slope_of_moments`), fed the price change and the signed volume instead of two returns;
//! - the signed volume is `net_volume`'s (`crate::fast_k::NetVolumeCore`);
//! - Amihud's return is `crate::fast_k::rocp`, and its mean over the bars that traded is the ratio
//!   of two `crate::roll::SmaCore` means.
//!
//! On bars, not trades: a price change is close to close, the traded value is `close * volume`, and
//! the signed volume is the bar's volume signed by the direction of its close, `0` when the close
//! did not move. López de Prado's tick rule, for single TRADES, carries the last move's sign through
//! an unchanged price; a bar whose close did not move carries no direction here, which is the rule
//! `net_volume` already applies. A trade's own aggressor flag is `vike_marketdata::aggressor`'s, and
//! a bar carries none. No transcendental is taken: a square root is exact on every platform.

use super::pbars;
use crate::fast_e::scalar_indicator;
use crate::fast_k::{NetVolumeCore, rocp};
use crate::fast_r::{CoWindow, Moments};
use crate::roll::SmaCore;

// ---- kyle's lambda -------------------------------------------------------------

/// Kyle's lambda over the last `n` bars: the slope of the close-to-close change on the signed
/// volume.
#[derive(Clone)]
pub(crate) struct KyleLambdaCore {
    window: CoWindow,
    signed: NetVolumeCore,
    n: usize,
    previous: f64,
    started: bool,
}

impl KyleLambdaCore {
    pub(crate) fn new(n: usize) -> Self {
        Self {
            window: CoWindow::new(n),
            signed: NetVolumeCore::new(),
            n,
            previous: f64::NAN,
            started: false,
        }
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::new(self.n);
    }

    #[inline]
    pub(crate) fn push(&mut self, close: f64, volume: f64) -> f64 {
        let signed = self.signed.push(close, volume);
        let previous = std::mem::replace(&mut self.previous, close);
        if !std::mem::replace(&mut self.started, true) {
            // The first bar has no change.
            return f64::NAN;
        }
        self.window.push(close - previous, signed);
        self.window
            .resolved()
            .map_or(f64::NAN, |m| vike_math::regression::slope_of_moments(m.cov, m.vv))
    }
}

scalar_indicator! {
    /// Kyle's lambda (`kyle_lambda`): how far the price moves per unit of signed volume, the price
    /// impact of order flow. Parameter `period` (the number of bar-to-bar changes the regression
    /// spans, default 20); one output line, `NaN` for the first `period` bars.
    ///
    /// Formula (Kyle 1985; López de Prado, "Advances in Financial Machine Learning", chapter 19, in
    /// this module's doc). Over the last `period` bars, the least-squares slope of the price change
    /// on the signed volume, `lambda = cov(dp, s) / var(s)`, with `dp = close - close[1]` and `s`
    /// the bar's volume signed by the direction of its close (`net_volume`: `+volume` when the close
    /// rose, `-volume` when it fell, `0` when it did not move). The regression has an intercept (the
    /// centred form `beta` uses); the book writes it without one, and over a window whose signed
    /// volume averages zero the two agree. The unit is price per unit of volume.
    ///
    /// `NaN` where the window's signed volume is constant (no regressor), and while a `NaN` close,
    /// or the `NaN` volume of a bar whose close moved, is inside the window; exact again once it
    /// has left.
    KyleLambda, "kyle_lambda", KyleLambdaCore, input = cv,
    [period = 20.0],
    build = KyleLambdaCore::new(pbars(period)),
    lookback = pbars(period),
    path_dependent = false
}

// ---- amihud illiquidity ----------------------------------------------------------

/// Amihud's illiquidity over the last `n` bars: the mean of `|return| / (close * volume)` over the
/// bars that traded, as the mean of the ratio (a bar that traded nothing counting `0`) over the
/// share of the window that traded.
#[derive(Clone)]
pub(crate) struct AmihudCore {
    ratio: SmaCore,
    traded: SmaCore,
    n: usize,
    previous: f64,
    started: bool,
}

impl AmihudCore {
    pub(crate) fn new(n: usize) -> Self {
        Self {
            ratio: SmaCore::new(n),
            traded: SmaCore::new(n),
            n,
            previous: f64::NAN,
            started: false,
        }
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::new(self.n);
    }

    #[inline]
    pub(crate) fn push(&mut self, close: f64, volume: f64) -> f64 {
        let previous = std::mem::replace(&mut self.previous, close);
        if !std::mem::replace(&mut self.started, true) {
            // The first bar has no return.
            return f64::NAN;
        }
        // A `NaN` volume is not "no trade": it reaches the ratio and the mean is `NaN` while it is inside.
        let traded = volume != 0.0;
        let ratio = if traded { rocp(close, previous).abs() / (close * volume) } else { 0.0 };
        let share = self.traded.push(f64::from(traded));
        self.ratio.push(ratio) / share
    }
}

scalar_indicator! {
    /// Amihud's illiquidity (`amihud_illiquidity`): the absolute return per unit of traded value,
    /// how far one unit of money moves the price. Parameter `period` (the number of bar returns
    /// averaged, default 20); one output line, `NaN` for the first `period` bars.
    ///
    /// Formula (Amihud 2002, in this module's doc), on bars: `ILLIQ = (1/D) sum |r| / (close *
    /// volume)` over the last `period` bars, where `r = (close - close[1]) / close[1]` (`0` after a
    /// zero close, as `rocp` reads it) and `D` counts the bars in the window that traded. A bar with
    /// zero volume has no ratio and is left out, as the paper averages over the days with trading;
    /// a window in which nothing traded is `NaN`. The value is the raw ratio: the paper's `10^6`
    /// scale, or any other, is the caller's.
    ///
    /// ⚠ **`close * volume` is the traded value only of a LINEAR contract at 1.0 a unit**: `volume` must be
    /// the bar's volume in BASE units. An inverse contract's volume counts quote dollars (contracts of 100
    /// USD), so its traded value is not `close * volume` and its ratio reads about `close` times too small
    /// beside a linear one's, which mis-ranks the two; a linear multiplier other than 1 is off by that factor.
    /// Feed such a series the bar's traded notional over its close as its volume (an inverse contract:
    /// `volume * face / close`; a linear multiplier `m`: `volume * m`).
    ///
    /// `NaN` while a `NaN` close or volume is inside the window; exact again once it has left.
    AmihudIlliquidity, "amihud_illiquidity", AmihudCore, input = cv,
    [period = 20.0],
    build = AmihudCore::new(pbars(period)),
    lookback = pbars(period),
    path_dependent = false
}

// ---- roll spread ---------------------------------------------------------------

/// Roll's spread over the last `n` pairs of consecutive close-to-close changes.
#[derive(Clone)]
pub(crate) struct RollSpreadCore {
    window: CoWindow,
    n: usize,
    previous: f64,
    previous_change: f64,
    /// Bars seen, up to 2: a pair of changes needs three closes.
    seen: u8,
}

impl RollSpreadCore {
    pub(crate) fn new(n: usize) -> Self {
        Self { window: CoWindow::new(n), n, previous: f64::NAN, previous_change: f64::NAN, seen: 0 }
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::new(self.n);
    }

    #[inline]
    pub(crate) fn push(&mut self, close: f64) -> f64 {
        let change = close - std::mem::replace(&mut self.previous, close);
        if self.seen < 2 {
            // No change on the first bar, no earlier change to pair with on the second.
            self.seen += 1;
            self.previous_change = change;
            return f64::NAN;
        }
        let lagged = std::mem::replace(&mut self.previous_change, change);
        self.window.push(change, lagged);
        let n = self.n;
        self.window.resolved().map_or(f64::NAN, |m| roll_spread_of(m, n))
    }
}

/// `2 sqrt(-cov)` from the window's moments, with the SAMPLE covariance (`m.cov` is
/// `n sum (u - mean u)(v - mean v)`, so it is divided by `n` and by `n - 1`), and `0` where that
/// covariance is not negative.
#[inline]
fn roll_spread_of(m: Moments, n: usize) -> f64 {
    let n = n as f64;
    let covariance = m.cov / n / (n - 1.0);
    if covariance.is_nan() {
        f64::NAN
    } else if covariance < 0.0 {
        2.0 * (-covariance).sqrt()
    } else {
        0.0
    }
}

scalar_indicator! {
    /// Roll's spread (`roll_spread`): the effective bid-ask spread implied by the bounce of the
    /// close between bid and ask. Parameter `period` (the number of pairs of consecutive changes,
    /// default 20); one output line, `NaN` for the first `period + 1` bars.
    ///
    /// Formula (Roll 1984, in this module's doc). With `dp = close - close[1]`, over the last
    /// `period` pairs `(dp, dp[1])` the sample serial covariance `cov = sum (dp - mean)(dp[1] -
    /// mean[1]) / (period - 1)`, each side with its own mean, and `spread = 2 sqrt(-cov)`. A
    /// covariance that is not negative, which Roll's model cannot produce, reads `0`. The unit is the
    /// price's.
    ///
    /// `NaN` while a `NaN` close is inside the window; exact again once it has left.
    RollSpread, "roll_spread", RollSpreadCore, input = value,
    [period = 20.0],
    build = RollSpreadCore::new(pbars(period)),
    lookback = pbars(period) + 1,
    path_dependent = false
}
