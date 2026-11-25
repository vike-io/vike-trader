//! The ALPHA POOL (book W21; Isichenko, *Quantitative Portfolio Management*, 2021, ch. 3, "Forecast
//! combining"): many strategies' forecasts, each measured against what followed it, weighed against
//! each other and combined into one forecast.
//!
//! **The shapes.** A forecast PANEL is a [`Matrix`] of `periods × symbols`: entry `(t, s)` is one
//! strategy's forecast for symbol `s` made at period `t`, a non-finite entry meaning "no forecast" (a
//! strategy that said nothing there holds nothing there). The REALIZED panel has the same shape: entry
//! `(t, s)` is the return of `s` over the horizon that FOLLOWS period `t`, the one the forecast at `t`
//! was about. The caller aligns them (a study does it from a run's `vike_model::ForecastRow`s and the
//! bars); nothing here shifts a series, so nothing here can look ahead.
//!
//! - [`alpha_returns`] — each strategy's per-period return as an ALPHA, the forecast-weighted realized
//!   return `Σₛ f(t, s) · r(t, s)` (Isichenko 3.1): the series the pool's statistics are taken over.
//!   Their correlation (3.1, diversification) is `vike_math::linear_algebra::correlation_matrix` of
//!   that matrix, nothing more.
//! - [`mean_variance_weights`] — the weights of greatest Sharpe ratio of the combined alpha
//!   (3.3, mean-variance combining): `crate::mean_variance`'s tangency portfolio over the alphas'
//!   covariance and mean returns, unconstrained or long-only.
//! - [`hierarchical_weights`] — the same, two levels deep (3.5.3, hierarchical combining): the
//!   alphas clustered by correlation (`crate::hierarchical::clusters_by_silhouette`), weighed inside
//!   each cluster and then across them (`crate::nested_clustered`), so a cluster of near-duplicate
//!   strategies cannot swing one large inversion.
//! - [`combined_forecast`] — the pool's forecast, the weighted sum of the panels.
//!
//!   ⚠ **No partial pool.** A cell is combined only where EVERY member said something; one silent
//!   member (warming up, a stalled series) leaves the pool silent there too. A partial sum under
//!   tangency weights is not the pool: with weights (2.5, −1.5) a silent second member makes the sum
//!   2.5 times the first member's forecast, a silent first one turns the sign over, and renormalising
//!   by the present weights can divide by a sum near zero. Silence can never raise a position or flip
//!   it; the strategy's own rule for "no forecast" (hold, or flatten) applies.
//! - [`PoolWeights`] — the pool's weights as a versioned TEXT ([`POOL_TEXT_FORMAT`]): a study writes
//!   it as its run's `model.txt`, a LIVE strategy file is handed it through a `type = "model"` header
//!   key (book W32's delivery: `vike_strategy::file`'s format note), parses it with
//!   [`PoolWeights::parse`] and computes the combined forecast ITSELF with [`PoolWeights::combine`]
//!   from its own members' forecasts (the members are rules the file runs; no strategy reads another).
//! - [`information_decay`], [`decay_half_life`] and [`information_by_window`] — decay tracking (3.8):
//!   how a forecast's information coefficient (Pearson's, pooled over the panel) falls as the return
//!   it is held against lies further ahead, and how it moves from one stretch of time to the next.
//!
//! No arithmetic of a portfolio is written here: covariance, correlation and every solve are
//! `vike_math::linear_algebra`'s and the optimisations this crate's own modules'.

use vike_math::linear_algebra::{Matrix, column_means, correlation_matrix, covariance_matrix};

use crate::hierarchical::clusters_by_silhouette;
use crate::mean_variance::{long_only_maximum_sharpe, maximum_sharpe};
use crate::nested_clustered::nested_clustered_optimisation;

/// Each forecast panel's per-period ALPHA RETURN against `realized`: a `periods × strategies` matrix
/// whose entry `(t, k)` is `Σₛ fₖ(t, s) · r(t, s)` over the symbols where both are finite (a missing
/// forecast holds nothing, so it adds nothing).
///
/// `None` when there is no panel, a panel's shape differs from `realized`'s, or `realized` is empty.
pub fn alpha_returns(forecasts: &[Matrix], realized: &Matrix) -> Option<Matrix> {
    let (periods, symbols) = (realized.rows(), realized.columns());
    if forecasts.is_empty() || periods == 0 || symbols == 0 {
        return None;
    }
    if forecasts.iter().any(|f| f.rows() != periods || f.columns() != symbols) {
        return None;
    }
    let mut values = Vec::with_capacity(periods * forecasts.len());
    for t in 0..periods {
        for panel in forecasts {
            let alpha = panel.row(t).iter().zip(realized.row(t)).fold(0.0, |total, (f, r)| {
                if f.is_finite() && r.is_finite() { total + f * r } else { total }
            });
            values.push(alpha);
        }
    }
    Matrix::from_row_major(periods, forecasts.len(), values)
}

/// The combining weights of greatest Sharpe ratio over `alpha_returns` (`periods × strategies`, as
/// [`alpha_returns`] gives): `Σ⁻¹μ / 1ᵀΣ⁻¹μ` of the alphas' sample covariance and mean returns, or with
/// `long_only` the long-only maximum (no strategy's forecast is turned against itself). They sum to 1.
///
/// `None` with fewer than two periods, a non-finite entry, a covariance that is not positive definite
/// (two strategies that are one, or one that never varies), or no fully invested maximum
/// (`crate::mean_variance`'s `maximum_sharpe` and `long_only_maximum_sharpe`).
pub fn mean_variance_weights(alpha_returns: &Matrix, long_only: bool) -> Option<Vec<f64>> {
    let covariance = covariance_matrix(alpha_returns)?;
    let means = column_means(alpha_returns);
    if long_only {
        long_only_maximum_sharpe(&covariance, &means)
    } else {
        maximum_sharpe(&covariance, &means)
    }
}

/// The combining weights two levels deep: the strategies clustered by the correlation of their alphas
/// into at most `max_clusters` clusters (the silhouette's best cut), the tangency weights inside each
/// cluster and then across the clusters (Nested Clustered Optimisation with the mean alpha returns).
/// They sum to 1, and on alphas whose covariance is block-diagonal along the clusters they are
/// [`mean_variance_weights`]' own.
///
/// `None` where [`mean_variance_weights`] is, and with fewer than three strategies or a `max_clusters`
/// below 2, which admit no clustering (combine those with [`mean_variance_weights`]).
pub fn hierarchical_weights(alpha_returns: &Matrix, max_clusters: usize) -> Option<Vec<f64>> {
    let covariance = covariance_matrix(alpha_returns)?;
    let clusters = clusters_by_silhouette(&correlation_matrix(alpha_returns)?, max_clusters)?;
    let means = column_means(alpha_returns);
    nested_clustered_optimisation(&covariance, Some(&means), &clusters)
}

/// The POOL's forecast: `Σₖ wₖ fₖ(t, s)` over the panels, and NaN (no forecast) where ANY panel is
/// missing (the module doc's "No partial pool").
///
/// `None` when the panels and `weights` differ in number, there is none, a weight is not finite, or two
/// panels differ in shape.
pub fn combined_forecast(forecasts: &[Matrix], weights: &[f64]) -> Option<Matrix> {
    let first = forecasts.first()?;
    let (periods, symbols) = (first.rows(), first.columns());
    if forecasts.len() != weights.len() || weights.iter().any(|w| !w.is_finite()) {
        return None;
    }
    if forecasts.iter().any(|f| f.rows() != periods || f.columns() != symbols) {
        return None;
    }
    let values = (0..periods * symbols)
        .map(|i| weighted_sum(forecasts.iter().zip(weights).map(|(f, w)| (*w, f.values()[i]))))
        .collect();
    Matrix::from_row_major(periods, symbols, values)
}

/// THE combining rule of one cell, [`combined_forecast`]'s and [`PoolWeights::combine`]'s alike:
/// `Σ w · f` over the pairs, in pair order; NaN when any forecast is not finite (the module doc's
/// "No partial pool": a silent member silences the pool, never shrinks it to the members left).
fn weighted_sum(pairs: impl Iterator<Item = (f64, f64)>) -> f64 {
    let mut total = 0.0;
    for (w, f) in pairs {
        if !f.is_finite() {
            return f64::NAN;
        }
        total += w * f;
    }
    total
}

/// The first line of a pool's weights text: the format and its version. A text written under another
/// first line is refused by [`PoolWeights::parse`], never guessed.
pub const POOL_TEXT_FORMAT: &str = "vike-alpha-pool v1";

/// The pool's combining weights, one per MEMBER, in the pool's order: what a study measured and a
/// strategy file combines with. A member is a NAME (the study writes each saved run's id, or the name
/// its recipe gives it); a strategy file maps each name to the rule it runs for that member.
///
/// The TEXT ([`Self::to_text`], [`Self::parse`]): [`POOL_TEXT_FORMAT`] on the first line, then one line
/// per member, `<name>\t<weight>`, the weight written as Rust's shortest round-trip form of the `f64`,
/// so a text read back gives the same bits. Built and read only through [`Self::new`]'s rule: at least
/// one member, every name non-empty with no tab or line break and none twice, every weight finite.
#[derive(Debug, Clone, PartialEq)]
pub struct PoolWeights {
    members: Vec<(String, f64)>,
}

impl PoolWeights {
    /// The weights `members` names, in that order.
    ///
    /// # Errors
    /// The rule's words: no member, an empty name, a name holding a tab or a line break, a name given
    /// twice, or a weight that is not finite.
    pub fn new(members: Vec<(String, f64)>) -> Result<Self, String> {
        if members.is_empty() {
            return Err("a pool has at least one member".to_string());
        }
        for (i, (name, weight)) in members.iter().enumerate() {
            if name.is_empty() || name.contains(['\t', '\n', '\r']) {
                return Err(format!(
                    "member {i}'s name {name:?} is empty or holds a tab or a line break"
                ));
            }
            if members[..i].iter().any(|(earlier, _)| earlier == name) {
                return Err(format!("member {name:?} is named twice"));
            }
            if !weight.is_finite() {
                return Err(format!("member {name:?}'s weight {weight} is not a finite number"));
            }
        }
        Ok(PoolWeights { members })
    }

    /// The members and their weights, in the pool's order.
    pub fn members(&self) -> &[(String, f64)] {
        &self.members
    }

    /// The text a study writes and a strategy file is handed.
    pub fn to_text(&self) -> String {
        let mut text = format!("{POOL_TEXT_FORMAT}\n");
        for (name, weight) in &self.members {
            text.push_str(&format!("{name}\t{weight}\n"));
        }
        text
    }

    /// The weights `text` holds ([`Self::to_text`]'s form; a line break may be `\r\n`, a trailing empty
    /// line is no member).
    ///
    /// # Errors
    /// Another first line, a member line that is not `<name>\t<weight>`, a weight that does not read
    /// as a number, or [`Self::new`]'s rule.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut lines = text.lines();
        let first = lines.next().unwrap_or_default();
        if first != POOL_TEXT_FORMAT {
            return Err(format!(
                "not a pool's weights: the first line is {first:?}, not {POOL_TEXT_FORMAT:?}"
            ));
        }
        let mut members = Vec::new();
        for (n, line) in lines.enumerate().filter(|(_, line)| !line.is_empty()) {
            let (name, weight) = line
                .split_once('\t')
                .ok_or_else(|| format!("line {} is not `<name>\\t<weight>`: {line:?}", n + 2))?;
            let weight: f64 =
                weight.parse().map_err(|e| format!("line {}'s weight {weight:?}: {e}", n + 2))?;
            members.push((name.to_string(), weight));
        }
        PoolWeights::new(members)
    }

    /// The combined forecast from each member's own forecast now (`forecast_of(name)`, `None` or a
    /// non-finite value for a member with none): `Σ w · f` over every member, in the pool's order, the
    /// same rule (and the same bits) as [`combined_forecast`]'s cell; `None` when ANY member said
    /// nothing (the module doc's "No partial pool": a strategy sizes from the whole pool or not at all).
    pub fn combine(&self, mut forecast_of: impl FnMut(&str) -> Option<f64>) -> Option<f64> {
        let pairs =
            self.members.iter().map(|(name, w)| (*w, forecast_of(name).unwrap_or(f64::NAN)));
        let combined = weighted_sum(pairs);
        (!combined.is_nan()).then_some(combined)
    }
}

/// The Pearson correlation of the finite pairs `(f(t, s), r(t + lag, s))` for `t` in `periods`; NaN
/// with fewer than two pairs or a side that does not vary.
fn information_at(
    forecast: &Matrix,
    realized: &Matrix,
    lag: usize,
    periods: (usize, usize),
) -> f64 {
    let mut pairs = Vec::new();
    for t in periods.0..periods.1 {
        if t + lag >= realized.rows() {
            break;
        }
        for (f, r) in forecast.row(t).iter().zip(realized.row(t + lag)) {
            if f.is_finite() && r.is_finite() {
                pairs.extend([*f, *r]);
            }
        }
    }
    Matrix::from_row_major(pairs.len() / 2, 2, pairs)
        .and_then(|m| correlation_matrix(&m))
        .map_or(f64::NAN, |c| c.get(0, 1))
}

/// The forecast's INFORMATION COEFFICIENT at each lag `0..=max_lag`: entry `ℓ` is the Pearson
/// correlation, pooled over every period and symbol, of the forecast made at `t` with the realized
/// return of `t + ℓ` (`ℓ = 0` is the return the forecast was about). A forecast whose information
/// is real and short-lived has its largest coefficient at 0 and decays from there; one whose
/// coefficient at a later lag is as large was predicting something else. NaN at a lag with fewer than
/// two pairs or a side that does not vary.
///
/// `None` when the two panels differ in shape.
pub fn information_decay(forecast: &Matrix, realized: &Matrix, max_lag: usize) -> Option<Vec<f64>> {
    if forecast.rows() != realized.rows() || forecast.columns() != realized.columns() {
        return None;
    }
    let all = (0, forecast.rows());
    Some((0..=max_lag).map(|lag| information_at(forecast, realized, lag, all)).collect())
}

/// The first lag at which an [`information_decay`] curve has fallen to half its value at lag 0 or
/// below: the forecast's half-life in periods. `None` when the coefficient at lag 0 is not a positive
/// number (there is no information to decay) or the curve never halves within its lags.
pub fn decay_half_life(information_by_lag: &[f64]) -> Option<usize> {
    let first = *information_by_lag.first()?;
    if first.is_nan() || first <= 0.0 {
        return None;
    }
    information_by_lag.iter().skip(1).position(|ic| *ic <= first / 2.0).map(|at| at + 1)
}

/// The forecast's information coefficient at lag 0 over each consecutive stretch of `window`
/// periods (the last one shorter when the periods do not divide): how a strategy's edge moves through
/// time, the track of its decay after it was found. NaN for a stretch with fewer than two pairs or a
/// side that does not vary.
///
/// `None` when the two panels differ in shape or `window` is zero.
pub fn information_by_window(
    forecast: &Matrix,
    realized: &Matrix,
    window: usize,
) -> Option<Vec<f64>> {
    if window == 0 || forecast.rows() != realized.rows() || forecast.columns() != realized.columns()
    {
        return None;
    }
    let starts = (0..forecast.rows()).step_by(window);
    Some(
        starts
            .map(|start| {
                let end = (start + window).min(forecast.rows());
                information_at(forecast, realized, 0, (start, end))
            })
            .collect(),
    )
}

#[cfg(test)]
#[path = "alpha_pool_tests.rs"]
mod tests;
