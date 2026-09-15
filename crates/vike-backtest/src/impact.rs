//! impact — the opt-in **market-impact slippage model**, and the rule for how much of it each
//! fill lane is missing.
//!
//! The frozen cost model applies one FLAT adverse move per fill
//! ([`crate::broker_sim::adverse_fill_price`]): a 5bp `slippage` costs the same whether the
//! order is 1 share or 10% of the day's volume. That is wrong in the direction that flatters a
//! backtest — size is exactly what a real book charges for.
//!
//! ## ONE model, three lanes, two answers
//!
//! This module shipped bar-mode-only, on the claim that "where L2 depth is RECORDED, vike already
//! does better than any model". That claim is HALF true, and the half it gets wrong is the reason
//! this module now reaches the replay lanes. What the L2 walk supplies is the TEMPORARY
//! concession — the ladder a taker eats for demanding liquidity NOW — measured off real displayed
//! depth, which no model can beat. What it supplies is NOTHING of the PERMANENT footprint,
//! because a replayed book is a RECORDING: it was written by a market that never saw our order,
//! so the next snapshot, every subsequent mark and our eventual exit are all priced as though we
//! had not traded. That error is one-directional (always in the strategy's favour) and it scales
//! with size — the same shape as the error [`crate::fill_model::L2BookFillModel`] itself was
//! built to remove, one term further out.
//!
//! So the lane does not choose a MODEL, it chooses which TERMS it is still missing
//! ([`ImpactTerms`], selected at the wire-in site by [`crate::SimBroker::slippage_for`]):
//!
//! | lane | what its price law already charges for size | terms charged here |
//! |---|---|---|
//! | bar (`FillModelKind::Bar`) | nothing — one OHLC price plus a flat haircut | [`ImpactTerms::Both`] |
//! | tick (`FillModelKind::Tick`) | nothing — buy@ask / sell@bid fills ANY size at the top of book | [`ImpactTerms::Both`] |
//! | L2 (`FillModelKind::L2Book`, book present) | the temporary term, from the real ladder | [`ImpactTerms::PermanentOnly`] |
//! | L2 with no book (the documented degrade to the tick tier) | nothing | [`ImpactTerms::Both`] |
//!
//! There is deliberately no second model and no second coefficient set: the paper identifies the
//! two terms SEPARATELY, so asking this one model for its permanent half is reading it, not
//! forking it. Two cost models that can disagree is the failure the root `CLAUDE.md` has a whole
//! convention against.
//!
//! ## The maker exemption is a property of the LANE, not of `OrderKind`
//!
//! Impact is the cost of DEMANDING liquidity, so a fill that supplied it is exempt. Reading that
//! off the engine's `is_maker` flag alone was correct on the one lane this module used to reach
//! and is FALSE on both of the lanes it now reaches, because that flag is not a statement about
//! aggressiveness at all — `crates/vike-backtest/src/engine.rs`'s `dispatch_fill` sets it from
//! the order KIND and says so in its own comment ("a MARKETABLE limit books as a maker fill"),
//! which it does because the FEE side needs it that way. What a `Limit` fill is actually priced
//! at differs per lane:
//!
//! | lane | what a `Limit` fill is priced at | supplied liquidity? |
//! |---|---|---|
//! | bar (`FillModelKind::Bar`) | `vike_model::order_fill_price`'s `Limit` arm: `price.min(bar.open)` for a buy — at-or-better than the limit, reached because the market came DOWN to a resting order | YES → exempt |
//! | tick (`FillModelKind::Tick`) | `crate::fill_model::TickFillModel` fills a buy limit ONLY when `ask <= price`, AT THE ASK, for ANY size: it crossed the spread and took the touch | NO → charged |
//! | L2 (`FillModelKind::L2Book`, book present) | `crate::fill_model::book_taker_price` capped at the limit — a WALK down real resting levels for the order's own size | NO → charged |
//!
//! So [`crate::SimBroker::slippage_for`] exempts a maker on the BAR lane only
//! ([`crate::SimBroker::charges_impact`]), and the two replay lanes charge every fill. That is not
//! a widening for its own sake: leaving the `OrderKind` exemption in place on those lanes made the
//! whole charge AVOIDABLE — spell a taker as a marketable limit one tick through the touch and it
//! fills at the same price for the same size and pays nothing, which is the free lunch this module
//! exists to remove, wearing a different order kind.
//!
//! ⚠ **The accepted cost of charging a crossing limit: the fill can land past its own limit
//! price.** A buy limit at 103 that filled at the ask of 102 has `1/102` of headroom — just under
//! 1% — and an estimate larger than that reports a fill above 103, a price no venue would have
//! given. The honest reading of that case is that the order would not have filled THAT SIZE at
//! that price at all, which is a fill-SIZE consequence no price law here can express. Of the three
//! available answers this is the least bad, and the other two are worse in stateable ways: NOT
//! charging is the avoidance hole above (optimistic, in the operator's favour); CAPPING at the
//! limit makes the cost SATURATE exactly where it is largest — the fill that most moved the book
//! is the one whose estimate most exceeds the limit and is truncated — so the charge stops being
//! monotone in size at precisely the sizes it exists to penalise, flattering big makers while
//! looking like it penalises them. Charging it uncapped errs PESSIMISTIC, which is the safe
//! direction for a backtest, and the extreme tail is already counted
//! ([`crate::SimBroker::slippage_saturations`]).
//!
//! ⚠ **What is still missing on the maker side is not a price effect at all**, and it is
//! deliberately not built here. Our resting order is not in the replayed book, so the level it
//! joins is shallower on the tape than it would really have been, and a recorded sweep that
//! `crates/vike-backtest/src/engine/queued.rs`'s `queue_gate` treats as a strict cross (fill in
//! full) would in reality have stopped INSIDE our order. That is a fill-SIZE law and it belongs to
//! [`crate::queue_model`], beside the rest of the queue arithmetic — putting it here would be the
//! second-model failure this module just argued against.
//!
//! OPT-IN: activated by `EngineParams::impact = Some(model)`. `None` (the default) leaves the
//! fill path byte-identical — the wire-in site ([`crate::SimBroker::slippage_for`]) returns the
//! flat `slippage` field unchanged, with no arithmetic performed on it, so every parity and
//! golden fixture (r1/r3/r4/r7_gate/engine_kernel_parity/properties_fills) is untouched.
//!
//! ## The model
//!
//! [`AlmgrenChriss`] ports the published cost function of Almgren, Thum, Hauptmann & Li (2005),
//! *"Direct Estimation of Equity Market Impact"* (Risk 18(7)) — the empirical calibration fit to
//! ~700k Citigroup US equity orders, which is why its coefficients are quoted rather than tuned.
//! Two additive terms, both expressed as a FRACTION of price:
//!
//! ```text
//!   nu        = qty / (exec_time * avg_volume)      -- participation RATE
//!   permanent = gamma * sigma * (nu * exec_time)^alpha    == gamma * sigma * (qty/avg_volume)^alpha
//!   temporary = eta   * sigma * nu^beta
//!   impact    = permanent + temporary
//! ```
//!
//! with the published exponents `alpha = 0.891`, `beta = 0.600` and coefficients
//! `gamma = 0.314`, `eta = 0.142`. Permanent impact is the shift the order leaves in the market
//! after it is done; temporary impact is the concession paid for demanding liquidity NOW, and
//! decays once trading stops.
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
//! `(shares_outstanding / avg_volume)^delta` factor on the temporary term with `delta = 0.267`
//! (see [`AC_DELTA`]). It needs a per-issuer shares-outstanding series this workspace has no
//! source for, and it does not exist at all for the crypto/perp symbols that are the primary
//! bar-mode lane. Its absence makes the temporary term the paper's value at the calibration
//! sample's median liquidity.
//!
//! ## Units — read this before trusting a number
//!
//! The published coefficients are calibrated on DAILY units: `sigma` is a daily return
//! volatility and `avg_volume` a daily share volume, so `exec_time` is measured in DAYS.
//! [`window_stats`] derives both from the bar series being backtested, which means the
//! calibration is only literally right on DAILY bars. On finer bars the shape (monotone in
//! size, concave in participation) still holds but the absolute level is a per-bar quantity —
//! treat it as a tunable cost, not a published estimate, and scale [`AlmgrenChriss::exec_time`]
//! accordingly.
//!
//! ⚠ **On the tick lane the period is one TRADE PRINT, and that is a far smaller unit than a
//! bar** — [`TickWindow`] measures `sigma` over consecutive print-to-print returns and
//! `avg_volume` as the mean printed size. Both move with the measurement period in opposite
//! directions (`sigma` falls as the period shortens, `qty/avg_volume` rises), so a context
//! measured per PRINT rather than per DAY inflates the published estimate by a large multiple —
//! at the paper's own exponents, of order `N^(alpha - 1/2)` for `N` prints in a day, i.e. tens.
//! The estimate can then exceed 1.0 and SATURATE
//! ([`crate::SimBroker::slippage_saturations`] is the counter that says so).
//!
//! ⚠ **`exec_time` is NOT the knob that answers this, and a previous revision of this paragraph
//! said it was.** `exec_time` scales the TEMPORARY term ONLY: it cancels out of the permanent
//! term exactly (`nu * exec_time == qty / avg_volume`), which is the paper's own
//! horizon-independence and is pinned three times in this file
//! ([`AlmgrenChriss::exec_time`]'s own doc, `exec_time_scales_only_the_temporary_term`,
//! `permanent_impact_does_not_move_with_the_horizon`). So on the L2 lane — which charges
//! [`ImpactTerms::PermanentOnly`] and nothing else — `exec_time` is PROVABLY INERT, and on the
//! tick lane it moves only the smaller of the two addends at a high participation rate. A
//! documented knob that cannot move the number is worse than an undocumented gap.
//!
//! **The knob that does move it, on every lane, is the coefficient pair.** `gamma` and `eta` are
//! public fields of [`AlmgrenChriss`] and are settable from a profile
//! (`crates/vike-backtest/src/harness/profile.rs`'s `ImpactCfg`), defaulting to the published
//! [`AC_GAMMA`] / [`AC_ETA`]. `gamma` is exactly the coefficient the L2 lane's permanent-only
//! charge is proportional to, so scaling it is the L2 operator's only lever and it is a linear
//! one. Nothing here rescales the operator's number on their behalf — an auto-derived horizon or
//! a silently reinterpreted coefficient would be a number nobody could audit, and the published
//! calibration must stay recognisable as the published calibration.
//!
//! Pure arithmetic: no I/O, no clock, no state. `sigma`/`avg_volume` come from the caller.
//!
//! ⚠ Both `^` in the formula above are `libm::pow`, never `f64::powf`. Base AND exponent are
//! arbitrary runtime f64s here, which is the case
//! `docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md` says the
//! platform's libm decides the last bit of — and this number is multiplied into a FILL PRICE.
//! [`AlmgrenChriss::impact_frac`] carries the full argument.

use std::collections::VecDeque;

use vike_model::{Bar, py_sum};

/// Permanent-impact coefficient, Almgren et al. (2005).
pub const AC_GAMMA: f64 = 0.314;
/// Permanent-impact exponent on the participation rate, Almgren et al. (2005).
pub const AC_ALPHA: f64 = 0.891;
/// Temporary-impact coefficient, Almgren et al. (2005).
pub const AC_ETA: f64 = 0.142;
/// Temporary-impact exponent on the participation rate, Almgren et al. (2005).
pub const AC_BETA: f64 = 0.600;
/// Exponent of the paper's fundamental-data liquidity adjustment
/// `(shares_outstanding / avg_volume)^delta` on the temporary term.
///
/// Recorded for provenance and NOT applied — see the module docs for why the adjustment is
/// omitted. Nothing in this crate reads it.
pub const AC_DELTA: f64 = 0.267;

/// The market context one fill is priced against. `qty` is an ABSOLUTE order size in the same
/// unit as `avg_volume` (shares, contracts, coins); `sigma` is a fractional return volatility
/// over the same period `avg_volume` is measured on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImpactInputs {
    /// order size, absolute (never signed — direction is the engine's `side_sign`)
    pub qty: f64,
    /// average traded volume per period, same unit as `qty`
    pub avg_volume: f64,
    /// return volatility over one period, as a fraction (0.02 == 2%)
    pub sigma: f64,
}

/// Which HALVES of the one impact model a fill lane is still missing, and therefore charges.
///
/// This is a property of the LANE's price law, never of the model: see the module docs' table.
/// It exists so that extending impact past the bar lane adds no second model and no second
/// coefficient set — the Almgren et al. (2005) cost function identifies its permanent and
/// temporary terms separately, so a lane that has already paid one of them asks for the other.
///
/// ⚠ There is no `TemporaryOnly`. It would name the configuration "a lane that already accounts
/// for the footprint an order leaves behind but not for the concession it pays now", and no fill
/// law in this crate is that: every one of them prices a single moment. Adding the variant would
/// invite a caller to pick it because the name is available.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImpactTerms {
    /// Permanent + temporary — the whole cost function. The lane's own price law is
    /// size-INDIFFERENT (a bar OHLC price, or an L1 quote that fills any size at the top of
    /// book), so nothing of the estimate has been paid yet.
    Both,
    /// The permanent term ALONE. The lane walked a real resting book for the order's own size
    /// ([`crate::fill_model::L2BookFillModel`]), which IS the temporary concession measured off
    /// displayed depth rather than estimated — charging the temporary term again would
    /// double-count it. What the walk cannot supply is the footprint: the replayed book is a
    /// recording of a market that never saw the order, so it never moves in response to it.
    PermanentOnly,
}

/// The market-impact estimator seam: order size + market context -> an ADDITIONAL adverse price
/// move, as a fraction of price, ADDED to the engine's flat `slippage`.
///
/// The return is always non-negative and finite: it is a cost, and the engine applies the sign
/// (buys fill up, sells down). A model with insufficient or degenerate inputs must return `0.0`
/// rather than a NaN/inf that would silently poison the equity curve.
///
/// NO UPPER BOUND IS REQUIRED HERE, and that is a decision rather than an omission. A fraction of
/// 1.0 means "the move ate the whole price", and past it the multiplicative haircut in
/// [`crate::broker_sim::adverse_fill_price`] stops being adverse and INVERTS the sign — but an
/// implementation cannot see the two things that determine whether that happens: the flat
/// `slippage` this estimate is ADDED to ([`crate::SimBroker::slippage_for`] returns the sum, so a
/// per-model cap of 1.0 would still let `0.5 + 0.9` cross), and the side the engine will point the
/// move in. Capping here would therefore be a cap that does not enforce the invariant while
/// looking as though it does. The bound lives where both operands are in hand — see
/// `broker_sim`'s module docs — and this trait keeps the honest contract: report the cost the
/// model believes in, even when it is absurd, and let the fill site saturate and COUNT it
/// ([`crate::SimBroker::slippage_saturations`]).
pub trait ImpactModel: std::fmt::Debug + Send + Sync {
    /// Additional adverse move as a fraction of price, for the `terms` the calling lane has not
    /// already priced. MUST be finite and `>= 0.0`, and MUST be non-decreasing in `qty` (bigger
    /// orders never cost less) FOR EACH `terms` value independently. May exceed 1.0 — see the
    /// trait docs for why that is bounded at the fill site rather than here.
    ///
    /// This — not [`Self::impact_frac`] — is the REQUIRED method, deliberately. A defaulted
    /// terms-aware method would let an implementor inherit `Both` silently on the L2 lane and
    /// double-charge the walk with nothing anywhere saying so; making it required means a model
    /// that has not thought about the split fails to COMPILE, which is the loud failure.
    fn impact_frac_for(&self, input: &ImpactInputs, terms: ImpactTerms) -> f64;

    /// The whole cost function — [`Self::impact_frac_for`] with [`ImpactTerms::Both`]. The
    /// spelling for a caller that is pricing a lane whose own law charges nothing for size, and
    /// the one every pre-lane-split call site already used.
    fn impact_frac(&self, input: &ImpactInputs) -> f64 {
        self.impact_frac_for(input, ImpactTerms::Both)
    }
}

/// The Almgren–Chriss (2005) empirical impact model — see the module docs for the formula, the
/// published constants, and the omitted liquidity adjustment.
///
/// [`Default`] is the published calibration with a one-period execution horizon.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AlmgrenChriss {
    /// permanent-impact coefficient (published [`AC_GAMMA`])
    pub gamma: f64,
    /// permanent-impact exponent (published [`AC_ALPHA`])
    pub alpha: f64,
    /// temporary-impact coefficient (published [`AC_ETA`])
    pub eta: f64,
    /// temporary-impact exponent (published [`AC_BETA`])
    pub beta: f64,
    /// execution horizon, in the same period as `sigma`/`avg_volume` (days for the published
    /// calibration). Larger = the order is worked more slowly = lower participation = less
    /// TEMPORARY impact. The permanent term does not move with it: the footprint an order leaves
    /// depends on the total volume fraction it took, not on how long it took to take it.
    ///
    /// ⚠ Read that last sentence as the operational limit it is, not as a footnote. Because this
    /// scales the temporary term ALONE, it is **provably inert on the L2 lane**, which charges
    /// [`ImpactTerms::PermanentOnly`] and nothing else — an L2 operator who reaches for this knob
    /// is turning a dial wired to nothing. `gamma` above is the lever there. Pinned by
    /// `exec_time_scales_only_the_temporary_term` and
    /// `permanent_impact_does_not_move_with_the_horizon` in this file's tests.
    pub exec_time: f64,
}

impl Default for AlmgrenChriss {
    fn default() -> Self {
        AlmgrenChriss {
            gamma: AC_GAMMA,
            alpha: AC_ALPHA,
            eta: AC_ETA,
            beta: AC_BETA,
            exec_time: 1.0,
        }
    }
}

impl AlmgrenChriss {
    /// The published calibration, worked over `exec_time` periods.
    pub fn with_exec_time(exec_time: f64) -> Self {
        AlmgrenChriss { exec_time, ..Default::default() }
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
        AlmgrenChriss { gamma, eta, exec_time, ..Default::default() }
    }

    /// Participation rate `nu = qty / (exec_time * avg_volume)`, or `None` when the inputs cannot
    /// support an estimate (non-positive volume/horizon, non-positive or non-finite qty).
    fn participation(&self, input: &ImpactInputs) -> Option<f64> {
        let ok = input.qty > 0.0
            && input.avg_volume > 0.0
            && self.exec_time > 0.0
            && input.qty.is_finite()
            && input.avg_volume.is_finite()
            && self.exec_time.is_finite();
        if !ok {
            return None;
        }
        let nu = input.qty / self.exec_time / input.avg_volume;
        nu.is_finite().then_some(nu)
    }
}

impl ImpactModel for AlmgrenChriss {
    fn impact_frac_for(&self, input: &ImpactInputs, terms: ImpactTerms) -> f64 {
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
        // is ADDED to the engine's flat slippage and multiplied into a FILL PRICE
        // (`crate::SimBroker::slippage_for` -> `crate::broker_sim::adverse_fill_price`), so it
        // reaches the equity curve of every bar-mode backtest that opts this model in. Two boxes
        // replaying one tape must not report different PnL.
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

/// The rolling market context [`ImpactInputs`] needs, measured off a bar series.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MarketStats {
    /// sample standard deviation of per-bar simple returns
    pub sigma: f64,
    /// mean per-bar volume
    pub avg_volume: f64,
}

/// Measure [`MarketStats`] over the LAST `window` bars of `bars` (fewer if the series is
/// shorter). Returns `None` when the window cannot support an estimate — under two returns
/// (i.e. under three bars), a non-positive close in the window (no meaningful return), or a
/// non-positive mean volume.
///
/// `sigma` is the SAMPLE standard deviation (`n-1`) of simple returns `c[i]/c[i-1] - 1`;
/// `avg_volume` the arithmetic mean of the window's volumes. Both sums go through
/// [`py_sum`] so the result is independent of how the window was sliced — a backtest must be
/// reproducible bar-for-bar.
pub fn window_stats(bars: &[Bar], window: usize) -> Option<MarketStats> {
    if window == 0 {
        return None;
    }
    let start = bars.len().saturating_sub(window);
    let w = &bars[start..];
    measure_series(w.iter().map(|b| b.close), w.iter().map(|b| b.volume), w.len())
}

/// The ONE market-context measurement, shared by the bar window ([`window_stats`]) and the tick
/// window ([`TickWindow::stats`]) so the two lanes cannot drift about what `sigma` and
/// `avg_volume` MEAN. `prices` and `sizes` are one observation each, oldest first, `n` long.
///
/// Generic over the iterators rather than taking two slices on purpose: the bar caller reads its
/// observations straight out of a `&[Bar]` and the tick caller out of a `VecDeque`, and a slice
/// signature would make one of them collect a Vec per priced fill for no gain.
///
/// Returns `None` when the window cannot support an estimate — under two returns (i.e. under
/// three observations), a non-positive or non-finite price anywhere in it, or a non-positive
/// mean size.
fn measure_series<P, S>(prices: P, sizes: S, n: usize) -> Option<MarketStats>
where
    P: Iterator<Item = f64> + Clone,
    S: Iterator<Item = f64>,
{
    if n < 3 {
        return None;
    }
    if prices.clone().any(|p| p <= 0.0 || !p.is_finite()) {
        return None;
    }
    let rets: Vec<f64> = prices.clone().zip(prices.skip(1)).map(|(a, b)| b / a - 1.0).collect();
    // `rets.len() == n - 1 >= 2` here (n >= 3 was checked above), so the sample denominator is
    // never zero. The sample
    // mean+variance is the crate's shared `metrics::mean_variance` (same py_sum fold discipline,
    // `ddof = 1.0`) rather than a second hand-rolled copy — see `benchmark::variance`, which
    // already delegates the same way.
    // ⚠ CORRECTED 2026-08-28 (a pre-existing drift, noticed while converting this file's own
    // transcendentals): this comment used to read "`mean_variance` squares with `(x-mean).powf(2.0)`;
    // that is bit-identical to the former `(r-mean)*(r-mean)` here". The spelling moved when
    // vike-analytics was converted ahead of this crate — `crates/vike-analytics/src/metrics.rs`'s
    // `mean_variance` now squares with `libm::pow(x - mean, 2.0)`, per the same decision record this
    // file's `impact_frac` cites. The bit-identity claim SURVIVES the move and is now true by
    // construction rather than by luck: `libm`'s `pow` takes a `y is 2` branch that returns `x * x`
    // outright, so the squaring IS the multiply, on every platform. (The old sentence's version of
    // it rested on the PLATFORM's `powf` agreeing with a multiply, which is the assumption that
    // record exists to stop anyone making.)
    let (_mean, var) = crate::metrics::mean_variance(rets.iter().copied(), 1.0);
    let sigma = var.sqrt();
    let avg_volume = py_sum(sizes) / n as f64;
    if !sigma.is_finite() || !avg_volume.is_finite() || avg_volume <= 0.0 {
        return None;
    }
    Some(MarketStats { sigma, avg_volume })
}

/// Upper bound on what [`TickWindow::with_capacity`] pre-reserves; a larger `cap` still holds
/// `cap` prints, it just grows into them.
const RESERVE_CEILING: usize = 4_096;

/// The TICK lane's market context: a bounded rolling window of the tape's own TRADE PRINTS,
/// measured by the same `measure_series` the bar window uses.
///
/// **Trade prints only, and that is the honest choice rather than a convenience.** A quote tick
/// carries no traded volume at all (`vike_model::quote_tick_to_bar` projects `volume = 0.0`), so
/// feeding quotes here would drive `avg_volume` toward zero and the participation rate toward
/// infinity — an impact estimate that grows because the tape is quote-heavy, which is the
/// opposite of what a deep quoting market means. A tape with no trade prints therefore measures
/// nothing and [`Self::stats`] returns `None`, which the wire-in site treats exactly as it treats
/// a too-short bar window: charge the flat slippage alone rather than guess.
///
/// BOUNDED BY CONSTRUCTION. The window holds at most `cap` observations (the engine passes
/// `EngineParams::impact_window`) and evicts the oldest, so a 100M-tick replay costs `cap`
/// observations per symbol rather than growing with the tape — the same discipline
/// `EngineParams::equity_sampling` exists to enforce on the equity curve.
///
/// ⚠ `cap` counts PRINTS here, not bars. [`crate::DEFAULT_IMPACT_WINDOW`] is 21, chosen as one
/// trading month of DAILY bars; 21 prints is a fraction of a second on a liquid tape, so a tick
/// run should set `impact_window` deliberately. Nothing scales it automatically — a lane that
/// silently reinterpreted the operator's number would be worse than one that spends it literally.
#[derive(Debug, Clone)]
pub struct TickWindow {
    cap: usize,
    /// `(price, size)` per print, oldest first
    obs: VecDeque<(f64, f64)>,
}

impl TickWindow {
    /// A window holding at most `cap` prints. `cap == 0` is a window that never records
    /// anything and always measures `None` — the same inert answer `window_stats` gives a
    /// zero-length bar window, rather than a panic or an unbounded buffer.
    pub fn with_capacity(cap: usize) -> Self {
        // Reserve up to a sane ceiling rather than `cap` outright: `impact_window` is operator
        // input and the deque grows on demand anyway, so an absurd number must not turn into an
        // absurd allocation PER SYMBOL at the top of a replay.
        TickWindow { cap, obs: VecDeque::with_capacity(cap.min(RESERVE_CEILING)) }
    }

    /// Record one trade print. Non-finite or non-positive prices/sizes are recorded as given —
    /// `measure_series` is the one place that decides a window is unmeasurable, so a
    /// degenerate print is never silently dropped into a window that then looks healthy.
    pub fn push(&mut self, price: f64, size: f64) {
        if self.cap == 0 {
            return;
        }
        // `while`, not `if`: the invariant this must restore is `len < cap` before the push, and
        // an equality check would silently stop enforcing it if the window were ever handed a
        // smaller capacity than it already holds.
        while self.obs.len() >= self.cap {
            self.obs.pop_front();
        }
        self.obs.push_back((price, size));
    }

    /// Measure this window, or `None` when it cannot support an estimate (see
    /// `measure_series`).
    pub fn stats(&self) -> Option<MarketStats> {
        measure_series(self.obs.iter().map(|o| o.0), self.obs.iter().map(|o| o.1), self.obs.len())
    }

    /// Prints currently held (at most the capacity it was built with).
    pub fn len(&self) -> usize {
        self.obs.len()
    }

    /// No print recorded yet — the run's warmup, during which [`Self::stats`] measures `None`.
    pub fn is_empty(&self) -> bool {
        self.obs.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(ts: i64, close: f64, volume: f64) -> Bar {
        Bar {
            ts,
            open: close,
            high: close,
            low: close,
            close,
            volume,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        }
    }

    /// Hand-computed against the published constants. nu = 1000 / (1.0 * 100_000) = 0.01, and at
    /// the unit horizon nu == X/V, so both terms read off the same number.
    ///   permanent = 0.314 * 0.02 * 0.01^0.891
    ///   temporary = 0.142 * 0.02 * 0.01^0.600
    ///
    /// The expectations in this module spell `libm::pow`, matching `impact_frac`. They assert at
    /// `1e-15` ABSOLUTE on quantities of order `1e-3`, i.e. ~1e-12 relative, so a last-bit
    /// disagreement between `libm` and a platform `powf` would never have reddened them — this is
    /// hygiene, not a fix. It buys two things: the comparison becomes exact rather than merely
    /// tolerant, and no reader can copy a `.powf` out of a test in this file into production.
    #[test]
    fn almgren_chriss_matches_a_hand_computed_value() {
        let m = AlmgrenChriss::default();
        let input = ImpactInputs { qty: 1000.0, avg_volume: 100_000.0, sigma: 0.02 };
        let nu: f64 = 0.01;
        let expect =
            0.314 * 0.02 * libm::pow(nu * 1.0, 0.891) + 0.142 * 0.02 * libm::pow(nu, 0.600);
        assert!((m.impact_frac(&input) - expect).abs() < 1e-15, "got {}", m.impact_frac(&input));
        // Sanity on the magnitude: 1% of daily volume at 2% daily vol costs a few bp.
        assert!(expect > 1e-4 && expect < 1e-2, "{expect}");
    }

    /// A second hand-computed point at a NON-UNIT horizon, written in the PAPER's own variables
    /// (`X/V` for permanent, `nu = X/(V*T)` for temporary) rather than in the code's algebra — so
    /// it catches a permanent term that has picked up a spurious `T` factor. This is the exact
    /// deviation an earlier revision carried: `gamma*sigma*T*nu^alpha`, which equals the paper
    /// only at `T = 1` and inflates by `T^(1-alpha)` elsewhere.
    #[test]
    fn exec_time_scales_only_the_temporary_term() {
        let t: f64 = 2.0;
        let m = AlmgrenChriss::with_exec_time(t);
        let (qty, vol, sigma) = (4000.0f64, 100_000.0f64, 0.03f64);
        let input = ImpactInputs { qty, avg_volume: vol, sigma };
        let frac_of_volume = qty / vol; // X/V = 0.04 — the permanent term's argument
        let nu = qty / t / vol; // 0.02 — the participation RATE
        let expect =
            0.314 * sigma * libm::pow(frac_of_volume, 0.891) + 0.142 * sigma * libm::pow(nu, 0.600);
        assert!((m.impact_frac(&input) - expect).abs() < 1e-15);
    }

    /// The horizon-independence of the permanent term, stated directly: hold `qty`/`avg_volume`
    /// fixed, vary `exec_time`, and the ONLY thing that may move is the temporary term. Pinned by
    /// reconstructing the permanent term once and subtracting it at each horizon.
    #[test]
    fn permanent_impact_does_not_move_with_the_horizon() {
        let (qty, vol, sigma) = (10_000.0f64, 200_000.0f64, 0.025f64);
        let permanent = AC_GAMMA * sigma * libm::pow(qty / vol, AC_ALPHA);
        let mut prev_temporary = f64::INFINITY;
        for t in [0.25f64, 1.0, 4.0, 10.0] {
            let m = AlmgrenChriss::with_exec_time(t);
            let total = m.impact_frac(&ImpactInputs { qty, avg_volume: vol, sigma });
            let temporary = total - permanent;
            let expect_temporary = AC_ETA * sigma * libm::pow(qty / t / vol, AC_BETA);
            assert!(
                (temporary - expect_temporary).abs() < 1e-15,
                "permanent term drifted at exec_time={t}: residual {temporary} != {expect_temporary}"
            );
            assert!(temporary < prev_temporary, "a slower horizon must cost less temporarily");
            prev_temporary = temporary;
        }
    }

    #[test]
    fn impact_is_strictly_monotone_in_size() {
        let m = AlmgrenChriss::default();
        let f = |qty| m.impact_frac(&ImpactInputs { qty, avg_volume: 50_000.0, sigma: 0.02 });
        let mut prev = f(1.0);
        for qty in [10.0, 100.0, 500.0, 1_000.0, 5_000.0, 25_000.0, 100_000.0] {
            let cur = f(qty);
            assert!(cur > prev, "not increasing at qty={qty}: {cur} <= {prev}");
            prev = cur;
        }
    }

    /// Concavity: doubling size less than doubles the cost (both exponents are < 1). This is the
    /// economically load-bearing shape — a linear model would make large orders uninvestable.
    #[test]
    fn impact_is_concave_in_size() {
        let m = AlmgrenChriss::default();
        let f = |qty| m.impact_frac(&ImpactInputs { qty, avg_volume: 50_000.0, sigma: 0.02 });
        for qty in [100.0, 1_000.0, 10_000.0] {
            assert!(f(2.0 * qty) < 2.0 * f(qty), "not concave at {qty}");
        }
    }

    #[test]
    fn impact_scales_linearly_in_sigma_and_falls_with_liquidity() {
        let m = AlmgrenChriss::default();
        let a = m.impact_frac(&ImpactInputs { qty: 500.0, avg_volume: 50_000.0, sigma: 0.01 });
        let b = m.impact_frac(&ImpactInputs { qty: 500.0, avg_volume: 50_000.0, sigma: 0.02 });
        assert!((b - 2.0 * a).abs() < 1e-15, "sigma must enter linearly");
        let deep = m.impact_frac(&ImpactInputs { qty: 500.0, avg_volume: 500_000.0, sigma: 0.01 });
        assert!(deep < a, "a deeper book must cost less");
    }

    #[test]
    fn degenerate_inputs_charge_nothing_rather_than_nan() {
        let m = AlmgrenChriss::default();
        for input in [
            ImpactInputs { qty: 0.0, avg_volume: 1000.0, sigma: 0.02 },
            ImpactInputs { qty: -5.0, avg_volume: 1000.0, sigma: 0.02 },
            ImpactInputs { qty: 100.0, avg_volume: 0.0, sigma: 0.02 },
            ImpactInputs { qty: 100.0, avg_volume: 1000.0, sigma: 0.0 },
            ImpactInputs { qty: 100.0, avg_volume: 1000.0, sigma: f64::NAN },
            ImpactInputs { qty: f64::INFINITY, avg_volume: 1000.0, sigma: 0.02 },
        ] {
            assert_eq!(m.impact_frac(&input), 0.0, "{input:?}");
        }
        assert_eq!(
            AlmgrenChriss::with_exec_time(0.0).impact_frac(&ImpactInputs {
                qty: 100.0,
                avg_volume: 1000.0,
                sigma: 0.02
            }),
            0.0
        );
    }

    #[test]
    fn window_stats_measures_sigma_and_mean_volume() {
        // closes 100, 110, 121 -> returns 0.1, 0.1 -> sigma 0 (constant growth), avg vol 200
        let bars: Vec<Bar> = vec![bar(0, 100.0, 100.0), bar(1, 110.0, 200.0), bar(2, 121.0, 300.0)];
        let s = window_stats(&bars, 10).unwrap();
        assert!(s.sigma.abs() < 1e-15, "constant returns => zero dispersion, got {}", s.sigma);
        assert!((s.avg_volume - 200.0).abs() < 1e-12);
    }

    #[test]
    fn window_stats_honors_the_window_and_needs_three_bars() {
        let bars: Vec<Bar> =
            (0..10).map(|i| bar(i, 100.0 + i as f64, 10.0 * (i + 1) as f64)).collect();
        // last 3 bars: volumes 80, 90, 100 -> mean 90
        let s = window_stats(&bars, 3).unwrap();
        assert!((s.avg_volume - 90.0).abs() < 1e-12);
        assert!(s.sigma > 0.0);
        // whole series mean volume 55
        let all = window_stats(&bars, 100).unwrap();
        assert!((all.avg_volume - 55.0).abs() < 1e-12);
        assert!(window_stats(&bars[..2], 10).is_none(), "two bars = one return, not enough");
        assert!(window_stats(&bars, 0).is_none());
    }

    #[test]
    fn window_stats_rejects_a_degenerate_window() {
        let bad: Vec<Bar> = vec![bar(0, 100.0, 10.0), bar(1, 0.0, 10.0), bar(2, 100.0, 10.0)];
        assert!(window_stats(&bad, 10).is_none(), "a zero close has no meaningful return");
        let novol: Vec<Bar> = vec![bar(0, 100.0, 0.0), bar(1, 101.0, 0.0), bar(2, 102.0, 0.0)];
        assert!(window_stats(&novol, 10).is_none(), "zero volume cannot price impact");
    }

    // --- the lane split: which TERMS a fill law has not already paid ---------------------------

    /// `PermanentOnly` is EXACTLY the permanent term — reconstructed from the published constants
    /// rather than by subtracting, so a bug that moved a factor from one term to the other cannot
    /// hide behind an identity that holds by construction.
    #[test]
    fn permanent_only_is_exactly_the_permanent_term() {
        for t in [0.5f64, 1.0, 7.0] {
            let m = AlmgrenChriss::with_exec_time(t);
            let input = ImpactInputs { qty: 3_000.0, avg_volume: 80_000.0, sigma: 0.018 };
            let want = AC_GAMMA * input.sigma * libm::pow(input.qty / input.avg_volume, AC_ALPHA);
            let got = m.impact_frac_for(&input, ImpactTerms::PermanentOnly);
            assert!((got - want).abs() < 1e-15, "exec_time={t}: {got} vs {want}");
        }
    }

    /// The double-count claim, as arithmetic: a lane that already walked a real book pays strictly
    /// LESS here than one that priced its fill off a single quote — and the gap it does not pay is
    /// precisely the temporary term, the thing the walk supplied.
    #[test]
    fn a_walked_book_is_charged_strictly_less_than_a_top_of_book_quote() {
        let m = AlmgrenChriss::default();
        let input = ImpactInputs { qty: 2_500.0, avg_volume: 60_000.0, sigma: 0.02 };
        let both = m.impact_frac_for(&input, ImpactTerms::Both);
        let permanent = m.impact_frac_for(&input, ImpactTerms::PermanentOnly);
        assert!(permanent > 0.0, "the footprint a replayed book cannot show is still a real cost");
        assert!(permanent < both, "{permanent} is not strictly less than {both}");
        let nu = input.qty / input.avg_volume;
        let temporary = AC_ETA * input.sigma * libm::pow(nu, AC_BETA);
        assert!((both - permanent - temporary).abs() < 1e-15, "the gap is not the temporary term");
    }

    /// `gamma` is the L2 lane's ONLY lever, and this is that claim as arithmetic rather than as
    /// prose. Two pins, and the second is the one that matters: a `PermanentOnly` charge scales
    /// LINEARLY with `gamma`, and it does not move AT ALL with `exec_time` — so an operator told
    /// to answer a unit mismatch with the horizon knob would be turning a dial wired to nothing.
    ///
    /// This is the positive twin of `permanent_impact_does_not_move_with_the_horizon` above:
    /// that one proves the inertness inside the whole model, this one proves it survives the
    /// `PermanentOnly` selection the L2 lane actually asks for, AND that something else works.
    #[test]
    fn gamma_moves_a_permanent_only_charge_and_the_horizon_does_not() {
        let input = ImpactInputs { qty: 3_000.0, avg_volume: 80_000.0, sigma: 0.018 };
        let published =
            AlmgrenChriss::default().impact_frac_for(&input, ImpactTerms::PermanentOnly);
        assert!(published > 0.0, "the fixture must produce a real charge");

        // The horizon knob: inert on this selection, at every horizon. Compared at `1e-15`
        // ABSOLUTE on a quantity of order `1e-4` (~1e-11 relative) rather than bit-for-bit,
        // because the code reconstructs `X/V` as `(qty / exec_time / avg_volume) * exec_time`, and
        // that round trip is exact in the paper's algebra but not in binary. The tolerance is
        // still orders tighter than the thing it watches for: an `exec_time` that had leaked into
        // the permanent term would move this by a FACTOR, not by a last bit.
        for t in [0.1f64, 1.0, 1_000.0] {
            let got = AlmgrenChriss::with_exec_time(t)
                .impact_frac_for(&input, ImpactTerms::PermanentOnly);
            assert!(
                (got - published).abs() < 1e-15,
                "exec_time={t} moved a PermanentOnly charge ({got} vs {published}) — the L2 \
                 lane's knob is not the horizon"
            );
        }
        // the coefficient knob: strictly linear, and it reaches this selection
        for scale in [0.02f64, 0.5, 3.0] {
            let m = AlmgrenChriss::with_coefficients(AC_GAMMA * scale, AC_ETA, 1.0);
            let got = m.impact_frac_for(&input, ImpactTerms::PermanentOnly);
            assert!(
                (got - published * scale).abs() < 1e-15,
                "gamma must scale a PermanentOnly charge linearly: {got} != {published} * {scale}"
            );
        }
    }

    /// ...and `eta` reaches the OTHER half, leaving the permanent term where it was — so the two
    /// coefficients are independent levers rather than one knob spelt twice.
    #[test]
    fn eta_moves_only_the_temporary_half() {
        let input = ImpactInputs { qty: 3_000.0, avg_volume: 80_000.0, sigma: 0.018 };
        let base = AlmgrenChriss::default();
        let permanent = base.impact_frac_for(&input, ImpactTerms::PermanentOnly);
        let m = AlmgrenChriss::with_coefficients(AC_GAMMA, AC_ETA * 4.0, 1.0);
        assert_eq!(
            m.impact_frac_for(&input, ImpactTerms::PermanentOnly).to_bits(),
            permanent.to_bits(),
            "eta leaked into the permanent term"
        );
        let want_temporary = 4.0 * (base.impact_frac_for(&input, ImpactTerms::Both) - permanent);
        let got_temporary = m.impact_frac_for(&input, ImpactTerms::Both) - permanent;
        assert!(
            (got_temporary - want_temporary).abs() < 1e-15,
            "eta must scale the temporary term linearly: {got_temporary} != {want_temporary}"
        );
    }

    /// The trait's monotonicity contract binds EACH `terms` value on its own — the L2 lane must
    /// not be the one place where a bigger order can cost less.
    #[test]
    fn each_terms_selection_is_monotone_and_concave_in_size() {
        let m = AlmgrenChriss::default();
        for terms in [ImpactTerms::Both, ImpactTerms::PermanentOnly] {
            let f = |qty| {
                m.impact_frac_for(&ImpactInputs { qty, avg_volume: 50_000.0, sigma: 0.02 }, terms)
            };
            let mut prev = f(1.0);
            for qty in [10.0, 100.0, 1_000.0, 25_000.0] {
                let cur = f(qty);
                assert!(cur > prev, "{terms:?} not increasing at qty={qty}: {cur} <= {prev}");
                assert!(f(2.0 * qty) < 2.0 * cur, "{terms:?} not concave at {qty}");
                prev = cur;
            }
        }
    }

    /// The provided method is the `Both` spelling, bit for bit — every pre-split call site keeps
    /// its number.
    #[test]
    fn impact_frac_is_the_both_spelling_bit_for_bit() {
        let m = AlmgrenChriss::default();
        for qty in [1.0f64, 750.0, 90_000.0] {
            let input = ImpactInputs { qty, avg_volume: 40_000.0, sigma: 0.03 };
            assert_eq!(
                m.impact_frac(&input).to_bits(),
                m.impact_frac_for(&input, ImpactTerms::Both).to_bits()
            );
        }
    }

    /// Degenerate inputs charge nothing under EITHER selection — the `PermanentOnly` arm shares
    /// the guards rather than reaching them by a second route.
    #[test]
    fn degenerate_inputs_charge_nothing_under_either_terms_selection() {
        let m = AlmgrenChriss::default();
        for input in [
            ImpactInputs { qty: 0.0, avg_volume: 1000.0, sigma: 0.02 },
            ImpactInputs { qty: 100.0, avg_volume: 0.0, sigma: 0.02 },
            ImpactInputs { qty: 100.0, avg_volume: 1000.0, sigma: f64::NAN },
        ] {
            assert_eq!(m.impact_frac_for(&input, ImpactTerms::PermanentOnly), 0.0, "{input:?}");
        }
    }

    // --- the tick-lane window -------------------------------------------------------------------

    /// The two windows are ONE measurement: a `TickWindow` fed the same (price, size) pairs a bar
    /// series carries measures bit-identically. This is what stops the tick lane from growing a
    /// second, subtly different definition of `sigma`.
    #[test]
    fn the_tick_window_measures_exactly_what_the_bar_window_measures() {
        let bars: Vec<Bar> = vec![
            bar(0, 100.0, 12.0),
            bar(1, 101.5, 9.0),
            bar(2, 100.75, 31.0),
            bar(3, 103.0, 4.0),
            bar(4, 102.25, 17.0),
        ];
        let mut w = TickWindow::with_capacity(bars.len());
        for b in &bars {
            w.push(b.close, b.volume);
        }
        let from_bars = window_stats(&bars, bars.len()).expect("measurable");
        let from_ticks = w.stats().expect("measurable");
        assert_eq!(from_ticks.sigma.to_bits(), from_bars.sigma.to_bits());
        assert_eq!(from_ticks.avg_volume.to_bits(), from_bars.avg_volume.to_bits());
    }

    /// The window is BOUNDED: it holds `cap` prints and evicts the oldest, so a long replay costs
    /// a constant. Proven by measurement, not by reading `len` — the retained window must be the
    /// one the last `cap` prints describe.
    #[test]
    fn the_tick_window_is_bounded_and_keeps_the_newest_prints() {
        let mut w = TickWindow::with_capacity(3);
        for (px, sz) in [(10.0, 1.0), (11.0, 1.0), (12.0, 5.0), (13.0, 9.0), (14.0, 10.0)] {
            w.push(px, sz);
        }
        assert_eq!(w.len(), 3, "the window grew past its capacity");
        let tail: Vec<Bar> = vec![bar(0, 12.0, 5.0), bar(1, 13.0, 9.0), bar(2, 14.0, 10.0)];
        let want = window_stats(&tail, 3).expect("measurable");
        let got = w.stats().expect("measurable");
        assert_eq!(got.avg_volume.to_bits(), want.avg_volume.to_bits(), "not the newest three");
        assert_eq!(got.sigma.to_bits(), want.sigma.to_bits());
    }

    /// Warmup and degenerate tapes measure NOTHING rather than guessing — the tick twin of
    /// `window_stats_honors_the_window_and_needs_three_bars`.
    #[test]
    fn the_tick_window_refuses_to_measure_a_window_it_cannot_support() {
        let mut w = TickWindow::with_capacity(8);
        assert!(w.is_empty() && w.stats().is_none(), "an empty window measures nothing");
        w.push(10.0, 1.0);
        w.push(11.0, 1.0);
        assert!(w.stats().is_none(), "two prints are one return, not enough");
        w.push(12.0, 1.0);
        assert!(w.stats().is_some(), "three prints are measurable");
        // a zero-size tape cannot price participation at all
        let mut novol = TickWindow::with_capacity(8);
        for px in [10.0, 11.0, 12.0] {
            novol.push(px, 0.0);
        }
        assert!(novol.stats().is_none(), "zero traded size cannot price impact");
        // ...nor can a non-positive print
        let mut bad = TickWindow::with_capacity(8);
        for px in [10.0, 0.0, 12.0] {
            bad.push(px, 1.0);
        }
        assert!(bad.stats().is_none(), "a zero price has no meaningful return");
        // a zero-capacity window records nothing at all rather than growing without bound
        let mut nocap = TickWindow::with_capacity(0);
        for px in [10.0, 11.0, 12.0] {
            nocap.push(px, 1.0);
        }
        assert!(nocap.is_empty() && nocap.stats().is_none());
    }

    /// The trait object shape the engine stores carries the same `Send + Sync` bound as the
    /// `properties` `Arc` beside it. NOT because anything moves it across a thread today —
    /// `crates/vike-backtest/src/harness/optimize.rs`'s `StoreEvaluator` maps `run_backtest` sequentially, and `SimBroker` holds
    /// `Vec<Rc<Vec<Bar>>>` so it is not `Send` at all — but so that this field is never the thing
    /// that blocks a future parallel sweep. Relaxing the bound later is a breaking change;
    /// keeping it costs nothing.
    #[test]
    fn model_is_a_send_sync_trait_object() {
        fn assert_send_sync<T: Send + Sync + ?Sized>() {}
        assert_send_sync::<dyn ImpactModel>();
        let _boxed: std::sync::Arc<dyn ImpactModel> = std::sync::Arc::new(AlmgrenChriss::default());
    }
}
