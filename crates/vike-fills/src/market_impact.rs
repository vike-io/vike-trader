//! market_impact — the opt-in **market-impact slippage model**, its market context and what a
//! slippage table (`vike_model::ProfileEngineSlippage`) resolves to: ONE copy for every fill site that charges the cost (the
//! simulator, `vike_simulator::OrderBrokerSimulator::slippage_for`, and the paper book,
//! `vike_paper::ExecutionClientPaper`) and for the research fit that calibrates it from the user's
//! own fills ([`calibrate_impact`]). It lives in this crate beside the fill price
//! (`broker_sim::adverse_fill_price`, which moves the fill by the slippage this fraction is added
//! to) and `broker_sim::commission`, so what a fill costs is one crate below both the simulator and
//! the paper book; a study names it through its file template. It was `vike-analytics`' until
//! 2026-11-18 (owner: "excellent - do"), which kept the metrics and report cluster in every paper
//! mount's closure for this one model.
//!
//! ## The model
//!
//! [`ImpactModelAlmgrenChriss`] ports the published cost function of Almgren, Thum, Hauptmann & Li
//! (2005), *"Direct Estimation of Equity Market Impact"* (Risk 18(7)) — the empirical calibration
//! fit to ~700k Citigroup US equity orders, which is why its coefficients are quoted rather than
//! tuned. Two additive terms, both expressed as a FRACTION of price:
//!
//! ```text
//!   nu        = qty / (exec_time * average_volume)      -- participation RATE
//!   permanent = gamma * sigma * (nu * exec_time)^alpha    == gamma * sigma * (qty/avg_volume)^alpha
//!   temporary = eta   * sigma * nu^beta
//!   impact    = permanent + temporary
//! ```
//!
//! with the published exponents [`AC_ALPHA`] `= 0.891`, [`AC_BETA`] `= 0.600` (the SHAPE, which no
//! profile restates) and coefficients [`AC_GAMMA`] `= 0.314`, [`AC_ETA`] `= 0.142` (the LEVEL, which
//! a strategy's stated slippage table may restate and [`calibrate_impact`] fits). Permanent impact
//! is the shift the order leaves in the market after it is done; temporary impact is the concession
//! paid for demanding liquidity NOW, and decays once trading stops.
//!
//! Note where `exec_time` does and does NOT appear, because it is the term a transcription slip
//! reaches for. In the paper the permanent term is a function of the TOTAL fraction of volume
//! traded, `X/V`, and is **horizon-independent** — working an order more slowly does not change
//! the footprint it leaves behind. Only the temporary term sees the horizon, through the
//! participation rate `nu = X/(V*T)`: worked slower, you demand less liquidity per unit time and
//! pay less. Writing the permanent term as `(nu * exec_time)^alpha` keeps that identity exact
//! while expressing both terms in the one `nu` the code computes.
//!
//! DELIBERATELY OMITTED — the paper's *fundamental-data liquidity adjustment*, a
//! `(shares_outstanding / average_volume)^delta` factor on the temporary term with `delta = 0.267`
//! (see `AC_DELTA`). It needs a per-issuer shares-outstanding series this workspace has no
//! source for, and it does not exist at all for the crypto/perp symbols that are the primary
//! bar-mode lane. Its absence makes the temporary term the paper's value at the calibration
//! sample's median liquidity.
//!
//! Which HALF of the model a fill site charges is the site's lane, never the model's
//! ([`ImpactTerms`]; the simulator's `crates/vike-simulator/src/impact.rs` module doc carries the
//! lane table and the maker exemption).
//!
//! ## Units — read this before trusting a number
//!
//! The published coefficients are calibrated on DAILY units: `sigma` is a daily return
//! volatility and `average_volume` a daily share volume, so `exec_time` is measured in DAYS.
//! [`window_stats`] derives both from the bar series being priced, which means the calibration is
//! only literally right on DAILY bars. On finer bars the shape (monotone in size, concave in
//! participation) still holds but the absolute level is a per-bar quantity — treat it as a tunable
//! cost, not a published estimate, and scale [`ImpactModelAlmgrenChriss::exec_time`] accordingly.
//! Or FIT the level to your own fills: [`calibrate_impact`] returns the `base`, `gamma` and `eta` a
//! strategy's stated slippage table states.
//!
//! ⚠ `exec_time` scales the TEMPORARY term ONLY: it cancels out of the permanent term exactly
//! (`nu * exec_time == qty / average_volume`), so on a lane that charges
//! [`ImpactTerms::PermanentOnly`] it is provably inert. **The knob that moves every lane is the
//! coefficient pair** `gamma` and `eta`, settable from a profile and defaulting to the published
//! [`AC_GAMMA`] / [`AC_ETA`]. Nothing here rescales the operator's number on their behalf.
//!
//! ## The market context
//!
//! A calibration has to read three things EXACTLY as the engine reads them, or the coefficients it
//! fits price a different model: the exponents, and `sigma` and `average_volume` over the bars that
//! CLOSED BEFORE the fill ([`window_stats_before`], [`window_stats`] and [`measure_series`], the one
//! measurement the engine's bar and tick lanes and the paper book share).
//!
//! ## The table
//!
//! A run's slippage is ONE table (`vike_model::ProfileEngineSlippage`, the shape and its check, which a
//! settings row is judged by too): in a backtest the strategy's `slippage` param, else what the strategy
//! states (the owner's ruling of 2026-11-18); [`slippage_terms`] resolves it to
//! the three numbers a fill site charges ([`ProfileEngineSlippageTerms`]): no table is no slippage,
//! byte for byte. The backtest and the paper book resolve it here, so the two cannot disagree about
//! what a table means.
//!
//! Pure arithmetic: no I/O, no clock, no state. `sigma`/`average_volume` come from the caller.
//!
//! ⚠ Every power is `libm::pow` (decision 0032): base and exponent are runtime values, and the
//! model's result is multiplied into a FILL PRICE ([`ImpactModel::impact_fraction_for`] on
//! [`ImpactModelAlmgrenChriss`] carries the full argument).

use std::sync::Arc;

use vike_marketdata::Bar;
use vike_math::summation::compensated_sum;
use vike_model::ProfileEngineSlippage;

/// Permanent-impact exponent on the traded fraction of volume, Almgren et al. (2005).
pub const AC_ALPHA: f64 = 0.891;
/// Temporary-impact exponent on the participation rate, Almgren et al. (2005).
pub const AC_BETA: f64 = 0.600;

/// The rolling market context the impact model reads, measured off a bar series.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImpactModelMarketStatistics {
    /// sample standard deviation of per-bar simple returns
    pub sigma: f64,
    /// mean per-bar volume
    pub average_volume: f64,
}

/// Measure [`ImpactModelMarketStatistics`] over the LAST `window` bars of `bars` (fewer if the series is
/// shorter). Returns `None` when the window cannot support an estimate — under two returns
/// (i.e. under three bars), a non-positive close in the window (no meaningful return), or a
/// non-positive mean volume.
///
/// `sigma` is the SAMPLE standard deviation (`n-1`) of simple returns `c[i]/c[i-1] - 1`;
/// `average_volume` the arithmetic mean of the window's volumes. Both sums go through
/// [`compensated_sum`] so the result is independent of how the window was sliced — a backtest must be
/// reproducible bar-for-bar.
pub fn window_stats(bars: &[Bar], window: usize) -> Option<ImpactModelMarketStatistics> {
    if window == 0 {
        return None;
    }
    let start = bars.len().saturating_sub(window);
    let w = &bars[start..];
    measure_series(w.iter().map(|b| b.close), w.iter().map(|b| b.volume), w.len())
}

/// [`window_stats`] over the last `window` bars that CLOSED BEFORE `ts`: the bar `ts` falls INSIDE
/// is left out, because its own close and volume are not knowable when the order prices and its
/// volume would in part be this very order. `bars` ascend by open time.
///
/// The window ends at `partition_point(|b| b.timestamp <= ts) - 1` (the index of the bar containing
/// `ts`), NOT at `partition_point(|b| b.timestamp < ts)`: the two agree when `ts` is a bar's own open
/// (the engine's coarse bar lanes), and only the former holds for a `ts` strictly inside a bar (the
/// engine's granular sub-bar lane, and every real fill a calibration reads).
pub fn window_stats_before(
    bars: &[Bar],
    ts: i64,
    window: usize,
) -> Option<ImpactModelMarketStatistics> {
    let hi = bars.partition_point(|b| b.timestamp <= ts).saturating_sub(1);
    window_stats(&bars[..hi], window)
}

/// The ONE market-context measurement, shared by the bar window ([`window_stats`]) and the engine's
/// tick window (`crates/vike-simulator/src/impact.rs`'s `ImpactModelTickWindow`) so the two lanes cannot
/// drift about what `sigma` and `average_volume` MEAN. `prices` and `sizes` are one observation
/// each, oldest first, `n` long.
///
/// Generic over the iterators rather than taking two slices on purpose: the bar caller reads its
/// observations straight out of a `&[Bar]` and the tick caller out of a `VecDeque`, and a slice
/// signature would make one of them collect a Vec per priced fill for no gain.
///
/// Returns `None` when the window cannot support an estimate — under two returns (i.e. under
/// three observations), a non-positive or non-finite price anywhere in it, or a non-positive
/// mean size.
pub fn measure_series<P, S>(prices: P, sizes: S, n: usize) -> Option<ImpactModelMarketStatistics>
where
    P: Iterator<Item = f64> + Clone,
    S: ExactSizeIterator<Item = f64>,
{
    if n < 3 {
        return None;
    }
    if prices.clone().any(|p| p <= 0.0 || !p.is_finite()) {
        return None;
    }
    let rets: Vec<f64> = prices.clone().zip(prices.skip(1)).map(|(a, b)| b / a - 1.0).collect();
    // `rets.len() == n - 1 >= 2` here, so the sample denominator is never zero. `mean_variance`
    // squares with `libm::pow(x - mean, 2.0)`, whose `y is 2` branch returns `x * x`: the square
    // IS the multiply on every platform.
    let (_mean, var) = vike_math::statistics::mean_variance(rets.iter().copied(), 1.0);
    let sigma = var.sqrt();
    let average_volume = vike_math::statistics::mean(sizes);
    if !sigma.is_finite() || !average_volume.is_finite() || average_volume <= 0.0 {
        return None;
    }
    Some(ImpactModelMarketStatistics { sigma, average_volume })
}

/// The model's two terms for one fill at UNIT coefficients (`gamma = eta = 1`), as
/// `(permanent, temporary)`: the regressors [`calibrate_impact`] fits the coefficients on. The
/// algebra is the engine's own (`nu = qty / exec_time / average_volume`, the permanent term over
/// `nu * exec_time`), so a fitted coefficient multiplies the number the engine multiplies.
///
/// `None` when the inputs cannot support an estimate, where the engine charges nothing: a
/// non-positive or non-finite `quantity`, `execution_time`, `sigma` or `average_volume`.
pub fn unit_impact_terms(
    quantity: f64,
    context: &ImpactModelMarketStatistics,
    execution_time: f64,
) -> Option<(f64, f64)> {
    let positive = |x: f64| x.is_finite() && x > 0.0;
    if ![quantity, execution_time, context.sigma, context.average_volume].into_iter().all(positive)
    {
        return None;
    }
    let nu = quantity / execution_time / context.average_volume;
    if !nu.is_finite() {
        return None;
    }
    let permanent = context.sigma * libm::pow(nu * execution_time, AC_ALPHA);
    let temporary = context.sigma * libm::pow(nu, AC_BETA);
    (permanent.is_finite() && temporary.is_finite()).then_some((permanent, temporary))
}

/// One of the user's own TAKER fills, with what a calibration reads around it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImpactFill {
    /// `+1` buy, `-1` sell.
    pub side: i32,
    /// The filled size, absolute, in the unit the bars' volume counts.
    pub quantity: f64,
    /// The fill's price.
    pub price: f64,
    /// The mid just BEFORE the fill: the arrival price.
    pub arrival_mid: f64,
    /// The mid a horizon AFTER the fill: what stayed of the move.
    pub after_mid: f64,
    /// The market context at the fill ([`window_stats_before`]).
    pub context: ImpactModelMarketStatistics,
}

/// The fitted level of the engine's `market_impact` slippage: the three numbers a stated slippage
/// table carries.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImpactCalibration {
    /// The constant fraction every fill pays beside the impact (the half spread, chiefly): `base`.
    pub base: f64,
    /// The permanent-impact coefficient: `gamma`.
    pub gamma: f64,
    /// The temporary-impact coefficient: `eta`.
    pub eta: f64,
    /// How many fills the fit used.
    pub fills: usize,
}

/// Fit `base`, `gamma` and `eta` of the engine's model from `fills`, at `execution_time` (the profile's
/// `exec_time`).
///
/// Per fill, with `(p, t)` = [`unit_impact_terms`] and moves signed by the side (a cost is positive):
/// - the permanent move `m = side * (after_mid - arrival_mid) / arrival_mid`;
/// - the realized cost `c = side * (price - arrival_mid) / arrival_mid`.
///
/// `gamma` is the least-squares slope of `m` on `p` THROUGH THE ORIGIN (a fill of nothing leaves no
/// footprint). `eta` and `base` are the slope and intercept of `c - gamma * p` on `t`, because the
/// engine charges `base + permanent + temporary` on the fill itself (Almgren et al. fit
/// `c = m / 2 + temporary` instead, for a price path traded over time; the engine prices one fill,
/// so this fit follows the engine's shape, which is the one the coefficients will run in).
///
/// A fill whose prices are not all positive and finite, whose side is not `±1`, or whose
/// [`unit_impact_terms`] is `None` is skipped. `None` when fewer than two fills remain or the
/// regressors do not vary (`gamma` or `eta` would not be identified). The numbers are what the data
/// says, signs included: a profile refuses a `gamma` or `eta` not above zero and a negative `base`,
/// and judging that is the caller's.
pub fn calibrate_impact(fills: &[ImpactFill], execution_time: f64) -> Option<ImpactCalibration> {
    let positive = |x: f64| x.is_finite() && x > 0.0;
    // (permanent regressor, temporary regressor, permanent move, realized cost)
    let rows: Vec<(f64, f64, f64, f64)> = fills
        .iter()
        .filter(|f| f.side == 1 || f.side == -1)
        .filter(|f| positive(f.price) && positive(f.arrival_mid) && positive(f.after_mid))
        .filter_map(|f| {
            let (p, t) = unit_impact_terms(f.quantity, &f.context, execution_time)?;
            let side = f64::from(f.side);
            let moved = side * (f.after_mid - f.arrival_mid) / f.arrival_mid;
            let cost = side * (f.price - f.arrival_mid) / f.arrival_mid;
            Some((p, t, moved, cost))
        })
        .collect();
    if rows.len() < 2 {
        return None;
    }
    let squares = compensated_sum(rows.iter().map(|r| r.0 * r.0));
    if !positive(squares) {
        return None;
    }
    let gamma = compensated_sum(rows.iter().map(|r| r.0 * r.2)) / squares;
    let temporary: Vec<f64> = rows.iter().map(|r| r.1).collect();
    let rest: Vec<f64> = rows.iter().map(|r| r.3 - gamma * r.0).collect();
    // The sample (`n-1`) variance and covariance from `vike_math` itself: `rows.len() >= 2` here,
    // so the under-two `0.0` guard of the analytics wrappers called until 2026-11-18 never fired.
    let (_, spread) = vike_math::statistics::mean_variance(temporary.iter().copied(), 1.0);
    if !positive(spread) {
        return None;
    }
    let eta = vike_math::statistics::covariance(&temporary, &rest, 1.0) / spread;
    let base = vike_math::statistics::mean(rest.iter().copied())
        - eta * vike_math::statistics::mean(temporary.iter().copied());
    let fitted = [base, gamma, eta];
    fitted.iter().all(|x| x.is_finite()).then_some(ImpactCalibration {
        base,
        gamma,
        eta,
        fills: rows.len(),
    })
}

/// Permanent-impact coefficient, Almgren et al. (2005).
pub const AC_GAMMA: f64 = 0.314;
/// Temporary-impact coefficient, Almgren et al. (2005).
pub const AC_ETA: f64 = 0.142;
/// Exponent of the paper's fundamental-data liquidity adjustment
/// `(shares_outstanding / average_volume)^delta` on the temporary term.
///
/// Recorded for provenance and NOT applied — see the module docs for why the adjustment is
/// omitted. Nothing in this crate reads it.
///
/// ⚠ So it is also the one item of the model no other crate can name: `pub(crate)`, it stays as the
/// paper's recorded value, which is why `dead_code` is allowed on it rather than the constant being
/// deleted: provenance for the term no fill site applies.
#[expect(dead_code)]
pub(crate) const AC_DELTA: f64 = 0.267;

/// The default rolling window, in BARS, the impact model's `sigma` and volume are measured over
/// when a slippage table names none: one trading month of DAILY bars (`21`). A tick replay
/// counts PRINTS with the same number (`vike_simulator::ImpactModelTickWindow`), which is a fraction
/// of a second on a liquid tape, so a tick run should set `window` deliberately.
pub const DEFAULT_IMPACT_WINDOW: usize = 21;

/// The market context one fill is priced against. `qty` is an ABSOLUTE order size in the same
/// unit as `average_volume` (shares, contracts, coins); `sigma` is a fractional return volatility
/// over the same period `average_volume` is measured on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImpactInputs {
    /// order size, absolute (never signed — direction is the engine's `side_sign`)
    pub qty: f64,
    /// average traded volume per period, same unit as `qty`
    pub average_volume: f64,
    /// return volatility over one period, as a fraction (0.02 == 2%)
    pub sigma: f64,
}

/// Which HALVES of the one impact model a fill lane is still missing, and therefore charges.
///
/// This is a property of the LANE's price law, never of the model: see the simulator's
/// `crates/vike-simulator/src/impact.rs` module doc for the table. It exists so that extending
/// impact past the bar lane adds no second model and no second coefficient set — the Almgren et
/// al. (2005) cost function identifies its permanent and temporary terms separately, so a lane
/// that has already paid one of them asks for the other.
///
/// ⚠ There is no `TemporaryOnly`. It would name the configuration "a lane that already accounts
/// for the footprint an order leaves behind but not for the concession it pays now", and no fill
/// law in this workspace is that: every one of them prices a single moment. Adding the variant
/// would invite a caller to pick it because the name is available.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImpactTerms {
    /// Permanent + temporary — the whole cost function. The lane's own price law is
    /// size-INDIFFERENT (a bar OHLC price, or an L1 quote that fills any size at the top of
    /// book), so nothing of the estimate has been paid yet.
    Both,
    /// The permanent term ALONE. The lane walked a real resting book for the order's own size
    /// (`vike_fills::fill_model::FillModelL2Book`), which IS the temporary concession measured off
    /// displayed depth rather than estimated — charging the temporary term again would
    /// double-count it. What the walk cannot supply is the footprint: the replayed book is a
    /// recording of a market that never saw the order, so it never moves in response to it.
    PermanentOnly,
}

/// The market-impact estimator seam: order size + market context -> an ADDITIONAL adverse price
/// move, as a fraction of price, ADDED to the fill site's flat `slippage`.
///
/// The return is always non-negative and finite: it is a cost, and the fill site applies the sign
/// (buys fill up, sells down). A model with insufficient or degenerate inputs must return `0.0`
/// rather than a NaN/inf that would silently poison the equity curve.
///
/// NO UPPER BOUND IS REQUIRED HERE, and that is a decision rather than an omission. A fraction of
/// 1.0 means "the move ate the whole price", and past it the multiplicative haircut in
/// `vike_fills::broker_sim::adverse_fill_price` stops being adverse and INVERTS the sign — but an
/// implementation cannot see the two things that determine whether that happens: the flat
/// `slippage` this estimate is ADDED to (the fill site returns the sum, so a per-model cap of 1.0
/// would still let `0.5 + 0.9` cross), and the side the fill site will point the move in. Capping
/// here would therefore be a cap that does not enforce the invariant while looking as though it
/// does. The bound lives where both operands are in hand — `vike_fills::broker_sim`'s module docs —
/// and this trait keeps the honest contract: report the cost the model believes in, even when it
/// is absurd, and let the fill site saturate (and, in the simulator, COUNT it:
/// `vike_simulator::OrderBrokerSimulator::slippage_saturations`).
pub trait ImpactModel: std::fmt::Debug + Send + Sync {
    /// Additional adverse move as a fraction of price, for the `terms` the calling lane has not
    /// already priced. MUST be finite and `>= 0.0`, and MUST be non-decreasing in `qty` (bigger
    /// orders never cost less) FOR EACH `terms` value independently. May exceed 1.0 — see the
    /// trait docs for why that is bounded at the fill site rather than here.
    ///
    /// This — not [`Self::impact_fraction`] — is the REQUIRED method, deliberately. A defaulted
    /// terms-aware method would let an implementor inherit `Both` silently on the L2 lane and
    /// double-charge the walk with nothing anywhere saying so; making it required means a model
    /// that has not thought about the split fails to COMPILE, which is the loud failure.
    fn impact_fraction_for(&self, input: &ImpactInputs, terms: ImpactTerms) -> f64;

    /// The whole cost function — [`Self::impact_fraction_for`] with [`ImpactTerms::Both`]. The
    /// spelling for a caller that is pricing a lane whose own law charges nothing for size, and
    /// the one every pre-lane-split call site already used.
    fn impact_fraction(&self, input: &ImpactInputs) -> f64 {
        self.impact_fraction_for(input, ImpactTerms::Both)
    }
}

/// The Almgren–Chriss (2005) empirical impact model — see the module docs for the formula, the
/// published constants, and the omitted liquidity adjustment.
///
/// [`Default`] is the published calibration with a one-period execution horizon.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImpactModelAlmgrenChriss {
    /// permanent-impact coefficient (published [`AC_GAMMA`])
    pub gamma: f64,
    /// permanent-impact exponent (published [`AC_ALPHA`])
    pub alpha: f64,
    /// temporary-impact coefficient (published [`AC_ETA`])
    pub eta: f64,
    /// temporary-impact exponent (published [`AC_BETA`])
    pub beta: f64,
    /// execution horizon, in the same period as `sigma`/`average_volume` (days for the published
    /// calibration). Larger = the order is worked more slowly = lower participation = less
    /// TEMPORARY impact. The permanent term does not move with it: the footprint an order leaves
    /// depends on the total volume fraction it took, not on how long it took to take it.
    ///
    /// ⚠ Read that last sentence as the operational limit it is, not as a footnote. Because this
    /// scales the temporary term ALONE, it is **provably inert on the L2 lane**, which charges
    /// [`ImpactTerms::PermanentOnly`] and nothing else — an L2 operator who reaches for this knob
    /// is turning a dial wired to nothing. `gamma` above is the lever there. Pinned by
    /// `execution_time_scales_only_the_temporary_term` and
    /// `permanent_impact_does_not_move_with_the_horizon` in this module's tests.
    pub exec_time: f64,
}

impl Default for ImpactModelAlmgrenChriss {
    fn default() -> Self {
        ImpactModelAlmgrenChriss {
            gamma: AC_GAMMA,
            alpha: AC_ALPHA,
            eta: AC_ETA,
            beta: AC_BETA,
            exec_time: 1.0,
        }
    }
}

impl ImpactModelAlmgrenChriss {
    /// The published calibration, worked over `exec_time` periods.
    pub fn with_execution_time(exec_time: f64) -> Self {
        ImpactModelAlmgrenChriss { exec_time, ..Default::default() }
    }

    /// The published EXPONENTS with the two COEFFICIENTS rescaled — the lever that answers the
    /// unit mismatch the module docs warn about, and the ONLY one that reaches an
    /// [`ImpactTerms::PermanentOnly`] charge (`exec_time` cancels out of that term by
    /// construction, so on the L2 lane it moves nothing at all).
    ///
    /// The exponents deliberately stay at [`AC_ALPHA`] / [`AC_BETA`] and are not exposed
    /// alongside: `gamma`/`eta` set the LEVEL of a cost, which an operator can defensibly restate
    /// for a market and a measurement period the 2005 US-equity sample did not cover, while
    /// `alpha`/`beta` are the SHAPE — monotone and concave in participation — and refitting them
    /// is a different model wearing this one's name. The struct's fields stay public for a caller
    /// who genuinely means that; a profile cannot reach them.
    pub fn with_coefficients(gamma: f64, eta: f64, exec_time: f64) -> Self {
        ImpactModelAlmgrenChriss { gamma, eta, exec_time, ..Default::default() }
    }

    /// Participation rate `nu = qty / (exec_time * average_volume)`, or `None` when the inputs cannot
    /// support an estimate (non-positive volume/horizon, non-positive or non-finite qty).
    fn participation(&self, input: &ImpactInputs) -> Option<f64> {
        let ok = input.qty > 0.0
            && input.average_volume > 0.0
            && self.exec_time > 0.0
            && input.qty.is_finite()
            && input.average_volume.is_finite()
            && self.exec_time.is_finite();
        if !ok {
            return None;
        }
        let nu = input.qty / self.exec_time / input.average_volume;
        nu.is_finite().then_some(nu)
    }
}

impl ImpactModel for ImpactModelAlmgrenChriss {
    fn impact_fraction_for(&self, input: &ImpactInputs, terms: ImpactTerms) -> f64 {
        // A non-positive or non-finite sigma means "we could not measure volatility" — charge
        // nothing rather than invent a cost.
        if input.sigma <= 0.0 || !input.sigma.is_finite() {
            return 0.0;
        }
        let Some(nu) = self.participation(input) else {
            return 0.0;
        };
        // Permanent impact is horizon-INDEPENDENT in the paper: it is a function of the total
        // volume fraction X/V, not of the rate. `nu * exec_time` reconstructs X/V exactly (nu was
        // divided by exec_time to build it), so this is `gamma * sigma * (X/V)^alpha` — the
        // published functional form at EVERY horizon, not just at exec_time = 1.
        //
        // ⚠ `libm::pow`, NOT `f64::powf`. IEEE 754 requires `+ - * /` and `sqrt` to be correctly
        // rounded and requires nothing at all of `pow`, so the method spelling calls the PLATFORM's
        // libm — MSVC's CRT on the Windows dev box, glibc on the the CI box Linux boxes — and the two
        // disagree in the last bit. This is the WORST CASE of that rule rather than a marginal
        // instance: `docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md`
        // says to classify a power by its BASE, because a literal `10` or `2` is exactly
        // representable and lands on the same value however the call is lowered — and here BOTH
        // arguments are arbitrary runtime f64s. The base is the measured participation rate; the
        // exponents are the paper's `alpha = 0.891` / `beta = 0.600`, irrational-looking empirical
        // fits with no exact binary structure to save them. And the result is not a diagnostic: it
        // is ADDED to the fill site's flat slippage and multiplied into a FILL PRICE
        // (`vike_simulator::OrderBrokerSimulator::slippage_for` and the paper book ->
        // `vike_fills::broker_sim::adverse_fill_price`), so it reaches the equity curve of every
        // run that opts this model in. Two boxes replaying one tape must not report different PnL.
        let permanent = self.gamma * input.sigma * libm::pow(nu * self.exec_time, self.alpha);
        // The TEMPORARY term is the concession for demanding liquidity NOW — exactly what a walk
        // of a real resting book already charges. A lane that walked one asks for
        // `PermanentOnly` and this addend is not computed at all, so nothing about the fill it
        // prices depends on a `pow` whose result is then multiplied by zero.
        let temporary = match terms {
            ImpactTerms::Both => self.eta * input.sigma * libm::pow(nu, self.beta),
            ImpactTerms::PermanentOnly => 0.0,
        };
        let total = permanent + temporary;
        // Total guard: a caller-supplied coefficient set could overflow on an extreme nu.
        if total.is_finite() && total > 0.0 { total } else { 0.0 }
    }
}

/// What a slippage table resolves to: the three numbers a fill site charges. The
/// simulator's `vike_simulator::StrategyEngineConfig` fields `slippage`, `slippage_amount` and
/// `impact` + `impact_window`, and the paper book's `vike_paper::ExecutionClientPaper::with_slippage_terms`.
#[derive(Debug, Clone)]
pub struct ProfileEngineSlippageTerms {
    /// A fraction of the price every fill moves against the trader (`percent`'s `value`,
    /// `market_impact`'s `base`).
    pub fraction: f64,
    /// A price amount every fill no limit bounds moves against the trader (`ticks * tick`).
    pub amount: f64,
    /// The impact model and its window, in bars (`market_impact`).
    pub impact: Option<(Arc<dyn ImpactModel>, usize)>,
}

/// The `market_impact` knobs of `table`, each its default where the table states none:
/// `(exec_time, window, gamma, eta)`.
pub fn impact_knobs(table: &ProfileEngineSlippage) -> (f64, usize, f64, f64) {
    (
        table.exec_time.unwrap_or(1.0),
        table.window.unwrap_or(DEFAULT_IMPACT_WINDOW),
        table.gamma.unwrap_or(AC_GAMMA),
        table.eta.unwrap_or(AC_ETA),
    )
}

/// `config` resolved to the three numbers a fill site charges, after its check
/// (`vike_model::ProfileEngineSlippage::check`, whose words are the error). Absent (or `none`) is no
/// slippage at all: `0.0`, `0.0`, no model.
///
/// # Errors
///
/// The table's check, in its words.
pub fn slippage_terms(
    config: Option<&ProfileEngineSlippage>,
) -> Result<ProfileEngineSlippageTerms, String> {
    let none = ProfileEngineSlippageTerms { fraction: 0.0, amount: 0.0, impact: None };
    let Some(c) = config else { return Ok(none) };
    c.check()?;
    Ok(match c.model.as_str() {
        "percent" => ProfileEngineSlippageTerms { fraction: c.value.unwrap_or(0.0), ..none },
        // The lab's `slip_ticks * tick`, in that order.
        "ticks" => ProfileEngineSlippageTerms {
            amount: c.ticks.unwrap_or(0.0) * c.tick.unwrap_or(0.0),
            ..none
        },
        "market_impact" => {
            let (exec_time, window, gamma, eta) = impact_knobs(c);
            let model = ImpactModelAlmgrenChriss::with_coefficients(gamma, eta, exec_time);
            ProfileEngineSlippageTerms {
                fraction: c.base.unwrap_or(0.0),
                impact: Some((Arc::new(model), window)),
                ..none
            }
        }
        _ => none,
    })
}

#[path = "market_impact_tests.rs"]
#[cfg(test)]
mod market_impact_tests;
