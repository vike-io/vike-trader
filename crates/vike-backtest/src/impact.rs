//! impact — opt-in **market-impact slippage models** for BAR-mode backtests.
//!
//! The frozen cost model applies one FLAT adverse move per fill
//! ([`crate::broker_sim::adverse_fill_price`]): a 5bp `slippage` costs the same whether the
//! order is 1 share or 10% of the day's volume. That is wrong in the direction that flatters a
//! backtest — size is exactly what a real book charges for.
//!
//! Where L2 depth is RECORDED, vike already does better than any model: `run_ticks` walks the
//! real replayed book and the queue model ([`crate::queue_model`]) gates resting makers. This
//! module is for the lanes where **no book exists** — bar-mode crypto bench-hist, Alpaca
//! equities — and estimates the missing size-dependent cost from bar OHLCV alone.
//!
//! TAKER-ONLY: the estimate is charged on aggressive fills only. A resting limit that was hit
//! SUPPLIED the liquidity this cost pays for, so [`crate::SimBroker::slippage_for`] returns the
//! flat `slippage` alone for `is_maker` fills — charging otherwise would execute a maker THROUGH
//! its own limit price. The gate lives at the wire-in site, not in the models, so every
//! [`ImpactModel`] inherits it.
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
//! Pure arithmetic: no I/O, no clock, no state. `sigma`/`avg_volume` come from the caller.
//!
//! ⚠ Both `^` in the formula above are `libm::pow`, never `f64::powf`. Base AND exponent are
//! arbitrary runtime f64s here, which is the case
//! `docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md` says the
//! platform's libm decides the last bit of — and this number is multiplied into a FILL PRICE.
//! [`AlmgrenChriss::impact_frac`] carries the full argument.

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
    /// Additional adverse move as a fraction of price. MUST be finite and `>= 0.0`, and MUST be
    /// non-decreasing in `qty` (bigger orders never cost less). May exceed 1.0 — see the trait
    /// docs for why that is bounded at the fill site rather than here.
    fn impact_frac(&self, input: &ImpactInputs) -> f64;
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
    fn impact_frac(&self, input: &ImpactInputs) -> f64 {
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
        let temporary = self.eta * input.sigma * libm::pow(nu, self.beta);
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
    if w.len() < 3 {
        return None;
    }
    if w.iter().any(|b| b.close <= 0.0 || !b.close.is_finite()) {
        return None;
    }
    let rets: Vec<f64> = w.windows(2).map(|p| p[1].close / p[0].close - 1.0).collect();
    // n >= 2 here (w.len() >= 3), so the sample denominator is never zero. The sample
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
    let avg_volume = py_sum(w.iter().map(|b| b.volume)) / w.len() as f64;
    if !sigma.is_finite() || !avg_volume.is_finite() || avg_volume <= 0.0 {
        return None;
    }
    Some(MarketStats { sigma, avg_volume })
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

    /// The trait object shape the engine stores carries the same `Send + Sync` bound as the
    /// `properties` `Arc` beside it. NOT because anything moves it across a thread today —
    /// `run_rows` (`harness/sweep.rs`) maps `run_backtest` sequentially, and `SimBroker` holds
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
