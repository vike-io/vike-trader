//! Cost-gated pairs (statistical-arbitrage) strategy — a portable
//! `impl<B: OrderBroker> Strategy<B>`, so the SAME type runs in the backtest sweep harness and
//! live. No Python twin, so NOT parity-gated.
//!
//! ```toml
//! [strategy]
//! runs_on = "live"
//! why = """\
//!     TWO-LEG on one venue, and the mount declares both: `PairsZScore::symbols` \
//!     names `symbol_a` and `symbol_b`, the daemon declares each one that is not \
//!     the mount's own as a leg (`vike_tradehub_core::MountPlacement::legs`), the node \
//!     admits it into the venue engine's `extra_symbols`, its bars reach the \
//!     strategy stamped with their symbol, and an order on it is routed there \
//!     (`resolve_intent_symbol`) while an order on any other symbol is refused. \
//!     Market orders only, judged per order by the pre-trade gate on the leg's \
//!     own grid; no per-leg cap beyond the per-order and per-mount ones (the \
//!     owner's answer Q7 of 2026-11-10)"""
//! routes = "multi_leg"
//! multi_leg_why = """\
//!     TWO-LEG: the two keys name the two legs of one spread trade, so at most ONE of them \
//!     could ever equal the mount's own symbol and requiring both to would be incoherent. \
//!     `PairsZScore::symbols` declares both, so the mount declares the other as its leg"""
//!
//! [params.symbol_a]
//! type = "text"
//! route = "symbol"
//!
//! [params.symbol_b]
//! type = "text"
//! route = "symbol"
//!
//! [params.period]
//! type = "number"
//! default = 20
//!
//! [params.entry_z]
//! type = "number"
//! default = 2.0
//!
//! [params.exit_z]
//! type = "number"
//! default = 0.5
//!
//! [params.beta]
//! type = "number"
//! default = 1.0
//!
//! [params.notional]
//! type = "number"
//! default = 1000.0
//!
//! [params.taker_fee]
//! type = "number"
//! default = 0.0005
//!
//! [params.half_spread_bps]
//! type = "number"
//! default = 1.0
//!
//! [params.hold_intervals]
//! type = "number"
//! default = 3.0
//!
//! [params.funding_a]
//! type = "number"
//! default = 0.0
//!
//! [params.funding_b]
//! type = "number"
//! default = 0.0
//!
//! [params.max_half_life]
//! type = "number"
//! default = 0.0
//! ```
//!
//! Structure adapted from Hummingbot's `controllers/generic/stat_arb.py` (Apache-2.0), with its
//! two substantive defects fixed:
//!
//! 1. **Beta SIZES the legs.** Hummingbot fits `beta` by OLS, then splits the legs 50/50 by dollar
//!    value (`pos_hedge_ratio = 1.0`), so it is not beta-neutral — the most common real-world
//!    pairs bug. Here the fitted beta sizes the hedge leg ([`beta_neutral_qtys`]).
//! 2. **Cost-gated, symmetric barriers.** Hummingbot's `+1% / −5%` barriers are EV-neutral BY
//!    CONSTRUCTION on a driftless process (`5/6·1% − 1/6·5% = 0`). Here entry requires the expected
//!    convergence to EXCEED a full round trip INCLUDING CARRY, and exit is symmetric (reversion or
//!    sign flip).
//!
//! ## A measurement instrument, not a product
//!
//! Published evidence (ten Binance perpetuals at 2bp/crossing) decomposes a round trip as gross
//! `+0.15%`, cost `−0.12%`, funding `−0.18%` ⇒ net `−0.15%`. This module reproduces that verdict
//! on vike's OWN data, fees and overfitting gates: expect a negative. It is also the only consumer
//! proving the `spread_quote` cost helper (its private child, inline below) is wired
//! into a decision path.

use vike_model::params::{ParamValue, Params};

use vike_marketdata::Bar;
use vike_model::{ContractValue, OrderBroker, OrderPlacement, Strategy};

/// The strategy this file runs, built from the profile's `[strategy.params]`.
pub fn build<B: OrderBroker + 'static>(params: &Params) -> Box<dyn Strategy<B> + Send> {
    Box::new(PairsZScore::from_params(params))
}

/// What each declared key RESOLVED to, in the header's order (the mount's "effective params" line).
/// `period` is a COUNT read through `as_f64` and rounded, so `period = 2.6` is legal input resolving
/// to `3`, visible only here.
pub fn echo(params: &Params) -> Vec<(&'static str, String)> {
    fn number(v: f64) -> String {
        format!("{v}")
    }
    /// A REQUIRED symbol left empty. ⚠ Not cosmetic: with either leg unset no order routes (there
    /// is no single-symbol fallback for a two-leg trade), so an echo that printed an empty string
    /// would show a mount that cannot trade as if it were configured.
    fn request_symbol(v: &str) -> String {
        if v.is_empty() { "(unset — this leg cannot route)".to_string() } else { v.to_string() }
    }
    let p = PairsZScore::from_params(params);
    vec![
        ("symbol_a", request_symbol(&p.symbol_a)),
        ("symbol_b", request_symbol(&p.symbol_b)),
        ("period", p.period.to_string()),
        ("entry_z", number(p.entry_z)),
        ("exit_z", number(p.exit_z)),
        ("beta", number(p.beta)),
        ("notional", number(p.notional)),
        ("taker_fee", number(p.taker_fee)),
        ("half_spread_bps", number(p.half_spread_bps)),
        ("hold_intervals", number(p.hold_intervals)),
        ("funding_a", number(p.funding_a)),
        ("funding_b", number(p.funding_b)),
        ("max_half_life", number(p.max_half_life)),
    ]
}

use spread_quote::{SpreadLeg, spread_total_cost};

mod spread_quote {
    //! Executable spread pricing — what a multi-leg spread ACTUALLY costs to trade,
    //! as opposed to its mid-price appearance.
    //!
    //! A pair/spread signal computed on mid prices is systematically optimistic: a
    //! round trip crosses the bid-ask of EVERY leg, twice. This module is the
    //! arithmetic that makes that cost explicit, so a strategy can refuse a trade
    //! whose edge does not survive it. It is pure `f64` with no I/O and no internal
    //! crate deps, and is a private child of the pairs strategy (`pairs.rs`), its one
    //! consumer: it lived in vike-model until 2026-10-12, when nothing else called it
    //! (`vike-market-making`'s xEMM pricing keeps its own sanity check and never did).
    //!
    //! ## The sign rule
    //!
    //! For a spread `S = Σ ratioᵢ · priceᵢ`, BUYING the spread means buying every
    //! positive-ratio leg (lifting its ASK) and selling every negative-ratio leg
    //! (hitting its BID). Selling the spread mirrors it. A naive `bid_A − bid_B` /
    //! `ask_A − ask_B` construction takes the same side on every leg; that
    //! understates crossing cost and produces a CROSSED quote (bid > ask) whenever
    //! the short leg is wider than the long leg.
    //!
    //! Prior art: a reference engine's spread-quote aggregator gets this convention right
    //! while its synthetic-instrument type makes exactly that error. That engine is
    //! LGPL-3.0 — the convention is reimplemented here from the rule (a fact, not
    //! expression); no source text was copied.
    //!
    //! ## Why this exists
    //!
    //! Measured on crypto perpetuals, a pairs round trip decomposes as gross
    //! `+0.15%`, trading cost `−0.12%`, funding `−0.18%` ⇒ net `−0.15%`. The
    //! mean-reversion signal is real and it is small, so the cost terms decide the
    //! outcome. [`spread_roundtrip_cost`] is the fixed per-round-trip half of that;
    //! the time-proportional carry half is charged separately.
    //!
    //! ## Per BASE unit, whatever the contract
    //!
    //! Every amount here is per one BASE unit of the spread (a `ratio` is base units of a leg, a price
    //! is per base unit), so a leg's fee and carry are the default [`ContractValue`]'s `fee` and
    //! `funding` on its `ratio` at its mid: what a venue charges on that much notional whether it books
    //! the leg linear or inverse (an inverse contract's fee and funding are the rate on its dollars,
    //! which is `ratio · mid` here). A leg's order quantity in its OWN contract's units is
    //! `beta_neutral_qtys`' business, one layer up.

    use vike_model::ContractValue;

    /// One leg of a spread, with its own book and taker fee.
    #[derive(Debug, Clone, Copy, PartialEq)]
    pub(super) struct SpreadLeg {
        /// Signed leg ratio: `+` = held long in the spread, `−` = held short.
        pub(super) ratio: f64,
        /// Best bid for this leg.
        pub(super) bid: f64,
        /// Best ask for this leg.
        pub(super) ask: f64,
        /// Taker fee as a fraction of notional, charged PER CROSSING (e.g. `0.0005`).
        pub(super) taker_fee: f64,
    }

    /// `true` when every leg has a finite, uncrossed book and the set is non-empty.
    fn legs_are_sane(legs: &[SpreadLeg]) -> bool {
        !legs.is_empty()
            && legs.iter().all(|l| {
                l.ratio.is_finite()
                    && l.bid.is_finite()
                    && l.ask.is_finite()
                    && l.taker_fee.is_finite()
                    && l.ask >= l.bid
            })
    }

    /// The EXECUTABLE `(bid, ask)` of the spread: what you receive selling it and
    /// what you pay buying it, crossing each leg on the correct side.
    ///
    /// `None` when any leg's book is missing, non-finite or crossed — an unknowable
    /// price, never a free one. A caller must treat `None` as "do not trade", the
    /// same contract as [`vike_model::OrderBroker::quote_vwap`].
    pub(super) fn executable_spread(legs: &[SpreadLeg]) -> Option<(f64, f64)> {
        if !legs_are_sane(legs) {
            return None;
        }
        let mut bid = 0.0f64;
        let mut ask = 0.0f64;
        for l in legs {
            if l.ratio > 0.0 {
                ask += l.ratio * l.ask;
                bid += l.ratio * l.bid;
            } else {
                // A short leg is SOLD at the bid when opening and BOUGHT BACK at the
                // ask when closing, so its contribution flips sides. `ratio` is
                // negative here, so these terms subtract.
                ask += l.ratio * l.bid;
                bid += l.ratio * l.ask;
            }
        }
        Some((bid, ask))
    }

    /// Total cost of a full round trip in and out of the spread, in spread price
    /// units: the executable width (every leg's bid-ask, ratio-weighted) plus taker
    /// fees on TWO crossings per leg.
    ///
    /// This is the number an entry edge must EXCEED, not merely approach. It is the
    /// FIXED half of the cost; carry grows with holding time and is charged
    /// separately, so a caller holding a position must add both.
    pub(super) fn spread_roundtrip_cost(legs: &[SpreadLeg]) -> Option<f64> {
        let (bid, ask) = executable_spread(legs)?;
        let width = ask - bid;
        let mut fees = 0.0f64;
        for l in legs {
            let mid = (l.bid + l.ask) / 2.0;
            fees += 2.0 * ContractValue::default().fee(l.ratio.abs(), mid, l.taker_fee);
        }
        Some(width + fees)
    }

    /// The gate: does `edge` (the expected convergence, in spread price units)
    /// strictly exceed a full round trip? `false` whenever the cost is unknowable.
    ///
    /// Note this charges CROSSINGS ONLY. A strategy that holds a position across
    /// funding settlements must gate on crossings plus carry instead.
    ///
    /// Test-only since it left vike-model: `pairs.rs` gates on [`spread_total_cost`], so no
    /// production code calls this, and a production build would carry it as dead code.
    #[cfg(test)]
    pub(super) fn spread_edge_clears_cost(edge: f64, legs: &[SpreadLeg]) -> bool {
        match spread_roundtrip_cost(legs) {
            Some(c) => edge.is_finite() && edge > c,
            None => false,
        }
    }

    /// Time-proportional CARRY on an open spread: per-interval funding paid on each
    /// leg's signed notional, summed over `intervals_held`.
    ///
    /// A long leg PAYS positive funding and a short leg RECEIVES it, so a hedged
    /// pair's net carry is the DIFFERENTIAL, not either leg's rate. `funding_rates`
    /// is one per-interval rate per leg, index-aligned with `legs`.
    ///
    /// **This term is why "hold longer to amortize the crossing cost" backfires.**
    /// The crossing cost is fixed per round trip; carry grows linearly with the
    /// hold, so the long hold that dilutes one maximizes the other. Measured on
    /// crypto perpetuals, carry (`−0.18%`/trade) EXCEEDED trading cost
    /// (`−0.12%`/trade) — a cost floor that omits it is not a cost floor. It is also
    /// the term essentially every open-source pairs implementation leaves out.
    ///
    /// `None` when the rate slice does not match the legs, or any input is
    /// non-finite — unknowable, never free.
    pub(super) fn spread_carry_cost(
        legs: &[SpreadLeg],
        funding_rates: &[f64],
        intervals_held: f64,
    ) -> Option<f64> {
        if !legs_are_sane(legs)
            || funding_rates.len() != legs.len()
            || !intervals_held.is_finite()
            || intervals_held < 0.0
            || funding_rates.iter().any(|r| !r.is_finite())
        {
            return None;
        }
        let mut carry = 0.0f64;
        for (l, rate) in legs.iter().zip(funding_rates.iter()) {
            let mid = (l.bid + l.ask) / 2.0;
            // `ratio` carries the sign: a long leg pays, a short leg receives.
            carry += ContractValue::default().funding(l.ratio, mid, *rate) * intervals_held;
        }
        Some(carry)
    }

    /// The number an entry edge must actually beat: crossings PLUS carry.
    ///
    /// Prefer this over [`spread_roundtrip_cost`] anywhere a position is held across
    /// funding settlements. Gating on crossings alone reproduces the exact omission
    /// that makes naive pairs backtests look profitable.
    pub(super) fn spread_total_cost(
        legs: &[SpreadLeg],
        funding_rates: &[f64],
        intervals_held: f64,
    ) -> Option<f64> {
        Some(spread_roundtrip_cost(legs)? + spread_carry_cost(legs, funding_rates, intervals_held)?)
    }

    // Copied from `crates/vike-strategy/src/strategies/pairs/spread_quote_tests.rs`.
    #[cfg(test)]
    mod spread_quote_tests {
        //! The executable-spread arithmetic judged on hand-built legs: the sign rule, the round-trip and
        //! carry costs, and the gate an entry edge must clear.

        use super::*;

        fn leg(ratio: f64, bid: f64, ask: f64, fee: f64) -> SpreadLeg {
            SpreadLeg { ratio, bid, ask, taker_fee: fee }
        }

        /// Buying the spread LIFTS the ask on a long leg and HITS the bid on a short
        /// leg. A naive `bid_A - bid_B` / `ask_A - ask_B` understates the cost.
        #[test]
        fn negative_ratio_leg_uses_the_opposite_side() {
            let legs = [leg(1.0, 99.0, 101.0, 0.0), leg(-1.0, 49.0, 51.0, 0.0)];
            let (bid, ask) = executable_spread(&legs).unwrap();
            // buy spread: pay ask_A(101), receive bid_B(49) -> 52
            assert_eq!(ask.to_bits(), 52.0_f64.to_bits());
            // sell spread: receive bid_A(99), pay ask_B(51) -> 48
            assert_eq!(bid.to_bits(), 48.0_f64.to_bits());
        }

        /// The property a naive synthetic-instrument quote violates: the executable
        /// spread is NEVER crossed, even when the short leg is much wider.
        #[test]
        fn spread_is_never_crossed_even_with_a_wide_short_leg() {
            let legs = [leg(1.0, 99.9, 100.1, 0.0), leg(-1.0, 40.0, 60.0, 0.0)];
            let (bid, ask) = executable_spread(&legs).unwrap();
            assert!(ask >= bid, "crossed spread: bid {bid} > ask {ask}");
        }

        /// A round trip crosses FOUR half-spreads: both legs' full widths.
        #[test]
        fn roundtrip_cost_is_both_legs_full_width_when_fees_are_zero() {
            let legs = [leg(1.0, 99.0, 101.0, 0.0), leg(-1.0, 49.0, 51.0, 0.0)];
            let cost = spread_roundtrip_cost(&legs).unwrap();
            assert_eq!(cost.to_bits(), 4.0_f64.to_bits()); // 2.0 + 2.0
        }

        /// Hedge ratio scales the short leg's contribution to both quote and cost.
        #[test]
        fn hedge_ratio_scales_the_short_leg() {
            let legs = [leg(1.0, 99.0, 101.0, 0.0), leg(-2.0, 49.0, 51.0, 0.0)];
            let cost = spread_roundtrip_cost(&legs).unwrap();
            assert_eq!(cost.to_bits(), 6.0_f64.to_bits()); // 2.0 + 2*2.0
        }

        /// Fees are charged on TWO crossings per leg (in and out), on |ratio|*mid.
        #[test]
        fn fees_are_charged_on_two_crossings_per_leg() {
            let legs = [leg(1.0, 100.0, 100.0, 0.001), leg(-1.0, 50.0, 50.0, 0.001)];
            let cost = spread_roundtrip_cost(&legs).unwrap();
            // width 0; fees = 2*(1*100*0.001) + 2*(1*50*0.001) = 0.2 + 0.1
            assert!((cost - 0.3).abs() < 1e-12, "cost {cost}");
        }

        /// A degenerate or crossed input book is UNKNOWABLE, not free.
        #[test]
        fn bad_book_is_none_not_zero() {
            assert!(executable_spread(&[leg(1.0, 101.0, 99.0, 0.0)]).is_none(), "crossed leg");
            assert!(executable_spread(&[leg(1.0, f64::NAN, 99.0, 0.0)]).is_none(), "nan leg");
            assert!(executable_spread(&[]).is_none(), "no legs");
        }

        /// The gate: an edge smaller than the round trip must NOT clear.
        #[test]
        fn edge_must_exceed_the_full_roundtrip_cost() {
            let legs = [leg(1.0, 99.0, 101.0, 0.0), leg(-1.0, 49.0, 51.0, 0.0)];
            assert!(!spread_edge_clears_cost(3.9, &legs), "3.9 < 4.0 must not clear");
            assert!(spread_edge_clears_cost(4.1, &legs), "4.1 > 4.0 must clear");
        }

        // --- carry (Task 1b) ---

        /// Carry is charged per leg on signed exposure: a long leg PAYS positive
        /// funding, a short leg RECEIVES it. Net carry on a hedged pair is the
        /// DIFFERENTIAL, not either leg's rate.
        #[test]
        fn carry_is_the_signed_differential_across_legs() {
            let legs = [leg(1.0, 100.0, 100.0, 0.0), leg(-1.0, 100.0, 100.0, 0.0)];
            // identical rates on both legs net to zero carry
            let c = spread_carry_cost(&legs, &[0.0001, 0.0001], 3.0).unwrap();
            assert!(c.abs() < 1e-12, "equal rates must net to zero, got {c}");
            // long leg pays 1bp more than the short receives -> a real cost
            let c = spread_carry_cost(&legs, &[0.0002, 0.0001], 3.0).unwrap();
            assert!((c - 0.03).abs() < 1e-12, "carry {c}"); // 1e-4 * 100 * 3 intervals
        }

        /// Carry scales with holding time — the term that makes "hold longer to
        /// amortize the fees" backfire.
        #[test]
        fn carry_scales_with_intervals_held() {
            let legs = [leg(1.0, 100.0, 100.0, 0.0)];
            let short_hold = spread_carry_cost(&legs, &[0.0001], 1.0).unwrap();
            let long_hold = spread_carry_cost(&legs, &[0.0001], 30.0).unwrap();
            assert!((long_hold - 30.0 * short_hold).abs() < 1e-12);
        }

        /// Total cost = crossings + carry. A caller must never charge only one.
        #[test]
        fn total_cost_is_crossings_plus_carry() {
            let legs = [leg(1.0, 99.0, 101.0, 0.0), leg(-1.0, 49.0, 51.0, 0.0)];
            let rt = spread_roundtrip_cost(&legs).unwrap();
            let carry = spread_carry_cost(&legs, &[0.0002, 0.0001], 3.0).unwrap();
            let total = spread_total_cost(&legs, &[0.0002, 0.0001], 3.0).unwrap();
            assert!((total - (rt + carry)).abs() < 1e-12);
        }

        /// A rate per leg is required; a mismatched slice is unknowable, not free.
        #[test]
        fn mismatched_rate_slice_is_none() {
            let legs = [leg(1.0, 100.0, 100.0, 0.0)];
            assert!(spread_carry_cost(&legs, &[], 1.0).is_none());
            assert!(spread_carry_cost(&legs, &[0.0001, 0.0002], 1.0).is_none());
            assert!(spread_carry_cost(&legs, &[f64::NAN], 1.0).is_none());
            assert!(spread_carry_cost(&legs, &[0.0001], -1.0).is_none(), "negative hold");
        }

        /// Carry can exceed the crossing cost on a long hold — the regime that makes
        /// a crossings-only floor dangerously optimistic.
        #[test]
        fn carry_can_dominate_the_crossing_cost() {
            let legs = [leg(1.0, 99.9, 100.1, 0.0), leg(-1.0, 99.9, 100.1, 0.0)];
            let rt = spread_roundtrip_cost(&legs).unwrap(); // 0.4
            // 3bp/interval differential over 90 intervals (~30 days at 8h funding)
            let carry = spread_carry_cost(&legs, &[0.0003, 0.0], 90.0).unwrap();
            assert!(carry > rt, "carry {carry} should dominate crossings {rt}");
        }
    }
}

/// Order quantities for a beta-neutral pair: `notional` worth of leg A against `beta` BASE units of
/// B per base unit of A (the spread `A − beta·B` is in prices, so in base units), NOT a 50/50 dollar
/// split. Each leg's quantity is in its own contract's units (`value_a`, `value_b`,
/// [`ContractValue::units_worth`] and [`ContractValue::units_as`]): one ES contract of 50 is 50 index
/// units, and ten Deribit dollars are `10/p` coins. At the default unit on both legs it is
/// `notional / price_a` and `beta` times that, bit for bit. `None` for a non-positive or non-finite
/// beta, price or notional: refusing beats silently inverting a leg.
pub fn beta_neutral_qtys(
    notional: f64,
    price_a: f64,
    price_b: f64,
    beta: f64,
    value_a: ContractValue,
    value_b: ContractValue,
) -> Option<(f64, f64)> {
    let usable = notional.is_finite()
        && notional > 0.0
        && price_a.is_finite()
        && price_a > 0.0
        && price_b.is_finite()
        && price_b > 0.0
        && beta.is_finite()
        && beta > 0.0;
    if !usable {
        return None;
    }
    let quantity_a = value_a.units_worth(notional, price_a, 1.0)?;
    let base = ContractValue::default();
    let base_a = value_a.units_as(quantity_a, price_a, base);
    Some((quantity_a, base.units_as(beta * base_a, price_b, value_b)))
}

/// Population mean + standard deviation of the trailing `period` values, at the LAST element of
/// `values`; `None` when the window is short or any value is non-finite.
///
/// ⚠ Deliberately duplicates `vike_indicators::pairs::spread_zscore`'s kernel BIT-FOR-BIT, so the
/// full traversal is load-bearing: the same running sum from index 0 (a fresh trailing-window sum
/// differs at ~1e-12), then `E[s²] − E[s]²` clamped at zero.
/// `inline_zscore_matches_the_indicator_seam` asserts it bitwise, over the SAME slice (the
/// strategy's buffer is bounded).
pub fn rolling_mean_sd_last(values: &[f64], period: usize) -> Option<(f64, f64)> {
    if period == 0 || values.len() < period {
        return None;
    }
    let p = period as f64;
    let (mut run_sum, mut run_sum2) = (0.0f64, 0.0f64);
    let mut out: Option<(f64, f64)> = None;
    for (i, &v) in values.iter().enumerate() {
        if !v.is_finite() {
            return None;
        }
        run_sum += v;
        run_sum2 += v * v;
        if i >= period {
            let old = values[i - period];
            run_sum -= old;
            run_sum2 -= old * old;
        }
        if i + 1 >= period {
            let mean = run_sum / p;
            let var = run_sum2 / p - mean * mean;
            out = Some((mean, var.max(0.0).sqrt()));
        }
    }
    out
}

/// Enter only when `|z| > entry_z` AND the expected convergence `edge` exceeds the FULL cost —
/// crossings **plus carry**. A `None` cost (crossed or missing book) never enters: an
/// unpriceable trade is not a free one.
pub fn should_enter(z: f64, entry_z: f64, edge: f64, total_cost: Option<f64>) -> bool {
    match total_cost {
        Some(c) => z.is_finite() && z.abs() > entry_z && edge.is_finite() && edge > c,
        None => false,
    }
}

/// Exit on reversion INTO `exit_z`, or on a sign flip vs the z the position opened at. Symmetric
/// on purpose: an asymmetric `+1% / −5%` pair is EV-neutral on a driftless process yet LOOKS
/// excellent on a short sample (five winners per loser).
pub fn should_exit(z: f64, entry_z_at_open: f64, exit_z: f64) -> bool {
    if !z.is_finite() {
        return false;
    }
    z.abs() <= exit_z || (entry_z_at_open * z < 0.0)
}

/// Minimal close-only bars, for feeding the 2-series indicator seam.
fn bars_from(closes: &[f64]) -> Vec<Bar> {
    closes
        .iter()
        .map(|&c| Bar {
            timestamp: 0,
            open: c,
            high: c,
            low: c,
            close: c,
            volume: 0.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        })
        .collect()
}

/// The cost-gated z-score pairs strategy (module doc); params via [`PairsZScore::from_params`].
#[derive(Debug, Clone)]
pub struct PairsZScore {
    /// Dependent leg — the `A` in `A − beta·B`. PINNED, never swept: which leg is dependent
    /// materially changes the hedge ratio (0.74 one way vs 1.27 the other; `1/0.74 ≈ 1.35`).
    pub symbol_a: String,
    /// Hedge leg — the `B`.
    pub symbol_b: String,
    /// Rolling window for the spread's mean/sd.
    pub period: usize,
    /// Enter when `|z| > entry_z`.
    pub entry_z: f64,
    /// Exit when `|z| <= exit_z` (or z flips sign).
    pub exit_z: f64,
    /// Hedge ratio in `A − beta·B`. Sizes the hedge leg — see [`beta_neutral_qtys`].
    pub beta: f64,
    /// Notional of leg A per entry.
    pub notional: f64,
    /// Taker fee as a fraction of notional, PER CROSSING.
    pub taker_fee: f64,
    /// Assumed half-spread per leg, in bps, for the executable quote (bars carry no book). Zero
    /// asserts a frictionless book — the assumption this strategy exists to test.
    pub half_spread_bps: f64,
    /// Expected funding intervals held, for the carry half of the cost floor.
    pub hold_intervals: f64,
    /// Per-interval funding rate for each leg, used when a bar carries none.
    pub funding_a: f64,
    pub funding_b: f64,
    /// Opt-in regime gate: refuse entry when the spread's OU half-life exceeds this many bars
    /// (or is NaN: not mean-reverting). `<= 0.0` disables it.
    pub max_half_life: f64,

    closes_a: Vec<f64>,
    closes_b: Vec<f64>,
    pending_a: Option<f64>,
    pending_b: Option<f64>,
    last_funding_a: Option<f64>,
    last_funding_b: Option<f64>,
    entry_z_at_open: Option<f64>,
}

impl Default for PairsZScore {
    fn default() -> Self {
        PairsZScore {
            symbol_a: String::new(),
            symbol_b: String::new(),
            period: 20,
            entry_z: 2.0,
            exit_z: 0.5,
            beta: 1.0,
            notional: 1000.0,
            taker_fee: 0.0005,
            half_spread_bps: 1.0,
            hold_intervals: 3.0,
            funding_a: 0.0,
            funding_b: 0.0,
            max_half_life: 0.0,
            closes_a: Vec::new(),
            closes_b: Vec::new(),
            pending_a: None,
            pending_b: None,
            last_funding_a: None,
            last_funding_b: None,
            entry_z_at_open: None,
        }
    }
}

impl PairsZScore {
    /// Read the knobs (`BuyHold` reader convention). `symbol_a`/`symbol_b` are REQUIRED in
    /// practice: with either empty no order routes (no single-symbol fallback for two legs).
    pub fn from_params(params: &Params) -> Self {
        let d = PairsZScore::default();
        PairsZScore {
            symbol_a: params
                .get("symbol_a")
                .and_then(ParamValue::as_str)
                .map(str::to_string)
                .unwrap_or(d.symbol_a),
            symbol_b: params
                .get("symbol_b")
                .and_then(ParamValue::as_str)
                .map(str::to_string)
                .unwrap_or(d.symbol_b),
            period: params
                .get("period")
                .and_then(ParamValue::as_f64)
                .map(|v| v.round().max(2.0) as usize)
                .unwrap_or(d.period),
            entry_z: params.get("entry_z").and_then(ParamValue::as_f64).unwrap_or(d.entry_z),
            exit_z: params.get("exit_z").and_then(ParamValue::as_f64).unwrap_or(d.exit_z),
            beta: params.get("beta").and_then(ParamValue::as_f64).unwrap_or(d.beta),
            notional: params.get("notional").and_then(ParamValue::as_f64).unwrap_or(d.notional),
            taker_fee: params.get("taker_fee").and_then(ParamValue::as_f64).unwrap_or(d.taker_fee),
            half_spread_bps: params
                .get("half_spread_bps")
                .and_then(ParamValue::as_f64)
                .unwrap_or(d.half_spread_bps),
            hold_intervals: params
                .get("hold_intervals")
                .and_then(ParamValue::as_f64)
                .unwrap_or(d.hold_intervals),
            funding_a: params.get("funding_a").and_then(ParamValue::as_f64).unwrap_or(d.funding_a),
            funding_b: params.get("funding_b").and_then(ParamValue::as_f64).unwrap_or(d.funding_b),
            max_half_life: params
                .get("max_half_life")
                .and_then(ParamValue::as_f64)
                .unwrap_or(d.max_half_life),
            ..PairsZScore::default()
        }
    }

    /// The two executable legs at the current prices, `half_spread_bps` wide.
    fn legs(&self, price_a: f64, price_b: f64) -> [SpreadLeg; 2] {
        let h = self.half_spread_bps / 10_000.0;
        [
            SpreadLeg {
                ratio: 1.0,
                bid: price_a * (1.0 - h),
                ask: price_a * (1.0 + h),
                taker_fee: self.taker_fee,
            },
            SpreadLeg {
                ratio: -self.beta,
                bid: price_b * (1.0 - h),
                ask: price_b * (1.0 + h),
                taker_fee: self.taker_fee,
            },
        ]
    }

    /// The opt-in regime gate. A NaN half-life (NOT mean-reverting) stands down, not a pass.
    fn half_life_ok(&self) -> bool {
        use vike_indicators::PairIndicator;
        use vike_indicators::pairs::HalfLife;
        let indicator = HalfLife::with_params(&[self.period as f64, self.beta]);
        let cols = indicator.vectorize_pair(&bars_from(&self.closes_a), &bars_from(&self.closes_b));
        match cols.first().and_then(|l| l.last()) {
            Some(&hl) if hl.is_finite() && hl > 0.0 => hl <= self.max_half_life,
            _ => false,
        }
    }

    fn flatten<B: OrderBroker>(&mut self, broker: &mut B) {
        let symbols = [self.symbol_a.clone(), self.symbol_b.clone()];
        for symbol in symbols {
            let p = broker.position(&symbol);
            if p != 0.0 {
                let side = vike_model::closing_side(p);
                broker.place(&symbol, "", side, p.abs(), OrderPlacement::Market, false);
            }
        }
        self.entry_z_at_open = None;
    }

    /// One completed step (both legs advanced): recompute the spread stats and act.
    fn evaluate<B: OrderBroker>(&mut self, broker: &mut B, price_a: f64, price_b: f64) {
        let n = self.closes_a.len();
        if n < self.period {
            return;
        }
        let spreads: Vec<f64> =
            (0..n).map(|i| self.closes_a[i] - self.beta * self.closes_b[i]).collect();
        let Some((mean, sd)) = rolling_mean_sd_last(&spreads, self.period) else {
            return;
        };
        if sd <= 0.0 {
            return; // a degenerate spread has no z-score
        }
        let z = (spreads[n - 1] - mean) / sd;
        if !z.is_finite() {
            return;
        }

        if broker.position(&self.symbol_a) != 0.0 {
            let entry = self.entry_z_at_open.unwrap_or(z);
            if should_exit(z, entry, self.exit_z) {
                self.flatten(broker);
            }
            return;
        }

        if self.max_half_life > 0.0 && !self.half_life_ok() {
            return;
        }

        let legs = self.legs(price_a, price_b);
        let rates = [
            self.last_funding_a.unwrap_or(self.funding_a),
            self.last_funding_b.unwrap_or(self.funding_b),
        ];
        let cost = spread_total_cost(&legs, &rates, self.hold_intervals);
        // Expected convergence back to the EXIT band (not the full |z|·sd: you exit at exit_z).
        let edge = (z.abs() - self.exit_z) * sd;
        if !should_enter(z, self.entry_z, edge, cost) {
            return;
        }

        let value = |s: &str| broker.contract_value(s).unwrap_or_default();
        let (value_a, value_b) = (value(&self.symbol_a), value(&self.symbol_b));
        let Some((qa, qb)) =
            beta_neutral_qtys(self.notional, price_a, price_b, self.beta, value_a, value_b)
        else {
            return;
        };
        // z > 0 ⇒ the spread is rich ⇒ SELL it: short A, long B. z < 0 mirrors.
        let side_a = if z > 0.0 { -1 } else { 1 };
        broker.place(&self.symbol_a, "", side_a, qa, OrderPlacement::Market, false);
        broker.place(&self.symbol_b, "", -side_a, qb, OrderPlacement::Market, false);
        self.entry_z_at_open = Some(z);
    }
}

impl<B: OrderBroker> Strategy<B> for PairsZScore {
    fn warmup(&self) -> usize {
        self.period
    }

    /// Both legs, so a live mount declares whichever is not its own (an empty one names nothing).
    fn symbols(&self) -> Vec<String> {
        [&self.symbol_a, &self.symbol_b].into_iter().filter(|s| !s.is_empty()).cloned().collect()
    }

    fn on_bar(&mut self, broker: &mut B, bar: &Bar) {
        // `on_bar` fires ONCE PER SYMBOL PER STEP: buffer each leg and act only once BOTH have
        // advanced, or the spread is evaluated against a stale opposite leg.
        let Some(symbol) = bar.symbol.as_deref() else {
            return;
        };
        if symbol == self.symbol_a {
            self.pending_a = Some(bar.close);
            if let Some(f) = bar.funding {
                self.last_funding_a = Some(f);
            }
        } else if symbol == self.symbol_b {
            self.pending_b = Some(bar.close);
            if let Some(f) = bar.funding {
                self.last_funding_b = Some(f);
            }
        } else {
            return;
        }

        let (Some(a), Some(b)) = (self.pending_a, self.pending_b) else {
            return;
        };
        self.pending_a = None;
        self.pending_b = None;
        let prices_usable = a.is_finite() && a > 0.0 && b.is_finite() && b > 0.0;
        if !prices_usable {
            return;
        }
        self.closes_a.push(a);
        self.closes_b.push(b);
        // Bound the buffers: stats and half-life read at most `period` (+1 lagged difference).
        let cap = self.period.saturating_add(2);
        if self.closes_a.len() > cap {
            let drop = self.closes_a.len() - cap;
            self.closes_a.drain(0..drop);
            self.closes_b.drain(0..drop);
        }
        self.evaluate(broker, a, b);
    }
}

// Copied from `crates/vike-strategy/src/strategies/pairs_tests.rs`. Run by `crates/vike-strategy-lab`
// (`cargo test -p vike-strategy-lab`); a build of this file compiles no test.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::OrderBrokerRecording;

    /// [`beta_neutral_qtys`] on two legs of the default unit (a base unit each).
    fn quantities(notional: f64, price_a: f64, price_b: f64, beta: f64) -> Option<(f64, f64)> {
        let unit = ContractValue::default();
        beta_neutral_qtys(notional, price_a, price_b, beta, unit, unit)
    }

    /// Each leg is sized in its OWN contract's units. ES (50 index units a contract) at 5,000 against
    /// SPY-like units at 500 with beta 10: 1,000,000 of A is 4 contracts (it sent 200, a 50 million
    /// position) hedged by 2,000 units (the 200 index units times 10). A Deribit inverse BTC leg of 10
    /// dollars a contract at 50,000 against linear ETH at 2,500 with beta 20: 1,000 dollars is 100
    /// contracts (it sent 0.02) hedged by 0.4 ETH. The default unit is the old sizing, bit for bit.
    #[test]
    fn each_leg_is_sized_in_its_own_contracts_units() {
        let es = ContractValue::linear(50.0);
        let (qa, qb) =
            beta_neutral_qtys(1_000_000.0, 5_000.0, 500.0, 10.0, es, ContractValue::default())
                .unwrap();
        assert_eq!((qa, qb), (4.0, 2_000.0));
        let btc = ContractValue::inverse(10.0);
        let (qa, qb) =
            beta_neutral_qtys(1_000.0, 50_000.0, 2_500.0, 20.0, btc, ContractValue::default())
                .unwrap();
        assert_eq!(qa, 100.0);
        assert!((qb - 0.4).abs() < 1e-12, "{qb}");
        // ...and the reverse: a linear BTC leg hedged on the inverse ETH contract of 10 dollars
        let (qa, qb) =
            beta_neutral_qtys(1_000.0, 50_000.0, 2_500.0, 20.0, ContractValue::default(), btc)
                .unwrap();
        assert_eq!(qa, 0.02);
        assert!(
            (qb - 100.0).abs() < 1e-9,
            "0.4 ETH at 2,500 is 1,000 dollars, 100 contracts: {qb}"
        );
        for (n, pa, pb, beta) in [(1_000.0, 100.0, 50.0, 2.0), (777.7, 65_432.1, 3_210.9, 0.37)] {
            let (qa, qb) = quantities(n, pa, pb, beta).unwrap();
            assert_eq!(
                (qa.to_bits(), qb.to_bits()),
                ((n / pa).to_bits(), (beta * (n / pa)).to_bits())
            );
        }
    }

    /// The spread `A − β·B` means one unit of spread is 1 unit of A against β units
    /// of B. Sizing must use that β, not a 50/50 dollar split (the Hummingbot bug).
    #[test]
    fn hedge_leg_is_scaled_by_beta_not_split_evenly() {
        let (qa, qb) = quantities(1000.0, 100.0, 50.0, 2.0).unwrap();
        assert!((qa - 10.0).abs() < 1e-12, "qty_a {qa}");
        assert!((qb - 20.0).abs() < 1e-12, "qty_b {qb}"); // 2.0 * 10.0
    }

    /// beta = 1 is the degenerate equal-UNITS case, NOT equal-dollars. A 50/50
    /// dollar split would give quantity_b = 20 here; beta-neutral gives 10.
    #[test]
    fn beta_one_is_equal_units_not_equal_dollars() {
        let (qa, qb) = quantities(1000.0, 100.0, 50.0, 1.0).unwrap();
        assert!((qa - 10.0).abs() < 1e-12);
        assert!((qb - 10.0).abs() < 1e-12, "equal UNITS: {qb}");
    }

    /// A non-positive or non-finite beta is not a hedge — refuse rather than invert
    /// a leg behind the caller's back.
    #[test]
    fn non_positive_or_nonfinite_beta_is_refused() {
        assert!(quantities(1000.0, 100.0, 50.0, 0.0).is_none());
        assert!(quantities(1000.0, 100.0, 50.0, -1.5).is_none());
        assert!(quantities(1000.0, 100.0, 50.0, f64::NAN).is_none());
    }

    #[test]
    fn bad_prices_or_notional_are_refused() {
        assert!(quantities(1000.0, 0.0, 50.0, 1.0).is_none());
        assert!(quantities(1000.0, 100.0, 0.0, 1.0).is_none());
        assert!(quantities(0.0, 100.0, 50.0, 1.0).is_none());
        assert!(quantities(f64::INFINITY, 100.0, 50.0, 1.0).is_none());
    }

    /// Dollar-neutrality is NOT the same as beta-neutrality, and the difference
    /// grows with beta — pinned so a future "simplification" back to a 50/50 split
    /// fails loudly.
    #[test]
    fn beta_neutral_differs_from_dollar_neutral_when_beta_is_not_one() {
        let notional = 1000.0;
        let (qa, qb) = quantities(notional, 100.0, 50.0, 3.0).unwrap();
        let dollar_neutral_qb = notional / 50.0; // what a 50/50 split would size
        assert!((qa - 10.0).abs() < 1e-12);
        assert!((qb - 30.0).abs() < 1e-12);
        assert!(
            (qb - dollar_neutral_qb).abs() > 1e-9,
            "beta-neutral {qb} must differ from dollar-neutral {dollar_neutral_qb}"
        );
    }

    // --- entry / exit rules ---

    /// A band break alone is NOT enough: the expected convergence must also clear a
    /// full round trip. This is the gate that makes the harness honest.
    #[test]
    fn entry_requires_both_a_band_break_and_a_cleared_cost_floor() {
        // |z| = 3 breaks a 2.0 band, but an edge of 1.0 is below a 40.0 round trip.
        assert!(!should_enter(3.0, 2.0, 1.0, Some(40.0)));
        // Same band break, edge now exceeds the round trip.
        assert!(should_enter(3.0, 2.0, 41.0, Some(40.0)));
        // Ample edge but the band is not broken.
        assert!(!should_enter(1.0, 2.0, 41.0, Some(40.0)));
    }

    /// An unknowable cost is never a free one — a missing/crossed book must block
    /// entry rather than be treated as zero cost.
    #[test]
    fn entry_is_refused_when_the_cost_is_unknowable() {
        assert!(!should_enter(5.0, 2.0, 1e9, None));
    }

    #[test]
    fn entry_is_refused_on_non_finite_inputs() {
        assert!(!should_enter(f64::NAN, 2.0, 100.0, Some(1.0)));
        assert!(!should_enter(5.0, 2.0, f64::NAN, Some(1.0)));
    }

    /// Exit is symmetric: reversion INTO the band, or a sign flip vs the entry z.
    #[test]
    fn exit_on_reversion_or_sign_flip() {
        assert!(should_exit(0.4, 2.5, 0.5), "reverted inside the exit band");
        assert!(!should_exit(1.2, 2.5, 0.5), "still outside the exit band");
        assert!(should_exit(-0.9, 2.5, 0.5), "sign flip vs the entry z");
        assert!(!should_exit(f64::NAN, 2.5, 0.5), "NaN holds rather than churns");
    }

    // --- z-score definition parity with the indicator seam ---

    /// The strategy computes its z inline (it needs the sd for the edge estimate
    /// anyway), so this pins that the inline definition matches
    /// `vike_indicators::pairs::spread_zscore` to a relative 1e-9. Without this gate the z the
    /// strategy trades on could silently drift from the z the seam plots — a
    /// population-vs-sample variance change would be invisible otherwise.
    #[test]
    fn inline_zscore_matches_the_indicator_seam() {
        use vike_indicators::PairIndicator;
        use vike_indicators::pairs::SpreadZscore;

        let period = 20usize;
        let beta = 1.0f64;
        let a: Vec<f64> = (0..80).map(|i| 100.0 + (i as f64 * 0.13).sin() * 6.0).collect();
        let b: Vec<f64> = (0..80).map(|i| 50.0 + (i as f64 * 0.09).cos() * 3.0).collect();

        let indicator = SpreadZscore::with_params(&[period as f64, beta]);
        let seam = indicator.vectorize_pair(&bars_from(&a), &bars_from(&b));
        let seam_z = &seam[0];

        let spreads: Vec<f64> = a.iter().zip(b.iter()).map(|(x, y)| x - beta * y).collect();
        for end in period..=spreads.len() {
            let (mean, sd) = rolling_mean_sd_last(&spreads[..end], period).unwrap();
            let mine = (spreads[end - 1] - mean) / sd;
            let theirs = seam_z[end - 1];
            // Relative 1e-9, no longer bit-exact: the seam sums on shifted ring sums since the fourth
            // speed wave (`fast_r.rs`'s `ZscoreCore`), so its last bits differ from this inline
            // recurrence. A change of DEFINITION still fails here: population against sample variance
            // moves the z by percent (a factor of sqrt(20 / 19) at this period), not by 1e-9.
            assert!(
                (mine - theirs).abs() <= 1e-9 * theirs.abs().max(1.0),
                "idx {}: inline z {mine} != seam z {theirs}",
                end - 1
            );
        }
    }

    // --- strategy wiring ---

    fn tagged_bar(symbol_name: &str, close: f64) -> Bar {
        Bar {
            timestamp: 0,
            open: close,
            high: close,
            low: close,
            close,
            volume: 0.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: Some(symbol_name.into()),
        }
    }

    /// A two-leg strategy must not act on one leg's bar: until BOTH legs advance the
    /// spread would be evaluated against a STALE opposite leg. `on_bar` fires once
    /// per symbol per step, so this is the core wiring contract.
    #[test]
    fn a_step_requires_both_legs_to_advance() {
        let mut s =
            PairsZScore { symbol_a: "A".into(), symbol_b: "B".into(), ..Default::default() };
        let mut br = OrderBrokerRecording::default();
        for i in 0..5 {
            s.on_bar(&mut br, &tagged_bar("A", 100.0 + f64::from(i)));
        }
        assert_eq!(s.closes_a.len(), 0, "no step may complete on one leg alone");
        s.on_bar(&mut br, &tagged_bar("B", 50.0));
        assert_eq!(s.closes_a.len(), 1, "the step completes once B arrives");
        assert_eq!(s.closes_b.len(), 1);
    }

    /// Bars for an unrelated symbol must be ignored entirely.
    #[test]
    fn unknown_symbols_are_ignored() {
        let mut s =
            PairsZScore { symbol_a: "A".into(), symbol_b: "B".into(), ..Default::default() };
        let mut br = OrderBrokerRecording::default();
        s.on_bar(&mut br, &tagged_bar("ZZZ", 1.0));
        s.on_bar(&mut br, &tagged_bar("A", 100.0));
        assert_eq!(s.closes_a.len(), 0, "an unrelated symbol must not complete a step");
    }

    /// Drive a diverging spread through the strategy twice — once with a book so
    /// wide the round trip cannot be cleared, once with a thin book. THE COST GATE
    /// IS THE ONLY DIFFERENCE, so this proves it is genuinely wired into the
    /// decision path rather than merely unit-tested in isolation.
    #[test]
    fn the_cost_gate_actually_blocks_entry() {
        fn run(half_spread_bps: f64, taker_fee: f64) -> usize {
            let mut s = PairsZScore {
                symbol_a: "A".into(),
                symbol_b: "B".into(),
                period: 10,
                entry_z: 1.5,
                half_spread_bps,
                taker_fee,
                hold_intervals: 0.0,
                ..Default::default()
            };
            let mut br = OrderBrokerRecording::default();
            // 10 flat steps to fill the window, then a large one-sided divergence.
            for i in 0..10 {
                s.on_bar(&mut br, &tagged_bar("A", 100.0 + f64::from(i % 2)));
                s.on_bar(&mut br, &tagged_bar("B", 50.0));
            }
            for _ in 0..3 {
                s.on_bar(&mut br, &tagged_bar("A", 130.0));
                s.on_bar(&mut br, &tagged_bar("B", 50.0));
            }
            br.orders.len()
        }

        let thin = run(0.5, 0.0);
        let wide = run(5_000.0, 0.05); // a 50% half-spread + 5% fee: nothing can clear it
        assert!(thin > 0, "a thin book with a big divergence should trade (got {thin} orders)");
        assert_eq!(wide, 0, "a prohibitive book must block entry entirely (got {wide} orders)");
    }

    /// Both legs must be routed on entry, in OPPOSITE directions, with the hedge leg
    /// sized by beta — the beta-neutrality contract, checked end-to-end.
    #[test]
    fn entry_routes_two_opposite_legs_sized_by_beta() {
        let mut s = PairsZScore {
            symbol_a: "A".into(),
            symbol_b: "B".into(),
            period: 10,
            entry_z: 1.5,
            beta: 2.0,
            notional: 1000.0,
            half_spread_bps: 0.5,
            taker_fee: 0.0,
            hold_intervals: 0.0,
            ..Default::default()
        };
        let mut br = OrderBrokerRecording::default();
        for i in 0..10 {
            s.on_bar(&mut br, &tagged_bar("A", 100.0 + f64::from(i % 2)));
            s.on_bar(&mut br, &tagged_bar("B", 50.0));
        }
        for _ in 0..3 {
            s.on_bar(&mut br, &tagged_bar("A", 130.0));
            s.on_bar(&mut br, &tagged_bar("B", 50.0));
        }
        assert!(br.orders.len() >= 2, "expected a two-leg entry, got {:?}", br.orders);
        let (sym_a, side_a, quantity_a) = br.orders[0].clone();
        let (sym_b, side_b, quantity_b) = br.orders[1].clone();
        assert_eq!(sym_a, "A");
        assert_eq!(sym_b, "B");
        assert_eq!(side_a, -side_b, "legs must be routed in opposite directions");
        // quantity_a = notional/price_a = 1000/130; quantity_b = beta * quantity_a
        assert!(
            (quantity_b - 2.0 * quantity_a).abs() < 1e-9,
            "hedge leg must be beta-scaled: {quantity_a}/{quantity_b}"
        );
    }

    /// A rich spread (z > 0) is SOLD: short the dependent leg, long the hedge.
    #[test]
    fn a_rich_spread_is_sold() {
        let mut s = PairsZScore {
            symbol_a: "A".into(),
            symbol_b: "B".into(),
            period: 10,
            entry_z: 1.5,
            half_spread_bps: 0.5,
            taker_fee: 0.0,
            hold_intervals: 0.0,
            ..Default::default()
        };
        let mut br = OrderBrokerRecording::default();
        for i in 0..10 {
            s.on_bar(&mut br, &tagged_bar("A", 100.0 + f64::from(i % 2)));
            s.on_bar(&mut br, &tagged_bar("B", 50.0));
        }
        // A jumps: the spread A - B is RICH, so A should be SHORTED.
        for _ in 0..3 {
            s.on_bar(&mut br, &tagged_bar("A", 130.0));
            s.on_bar(&mut br, &tagged_bar("B", 50.0));
        }
        assert!(!br.orders.is_empty(), "expected an entry");
        assert_eq!(br.orders[0].1, -1, "a rich spread must SHORT the dependent leg");
    }

    /// Defaults must resolve from an empty params table — the contract
    /// `registry_lists_every_match_arm` enforces for every roster entry.
    #[test]
    fn from_params_resolves_with_an_empty_table() {
        let empty: Params = toml::from_str("").unwrap();
        let s = PairsZScore::from_params(&empty);
        assert_eq!(s.period, 20);
        assert!((s.entry_z - 2.0).abs() < 1e-12);
        assert!((s.beta - 1.0).abs() < 1e-12);
        assert!(s.symbol_a.is_empty(), "no symbol default — a two-leg trade needs both named");
    }

    /// Params actually override, and `period` is floored at 2 (a 1-bar window has no
    /// dispersion).
    #[test]
    fn from_params_reads_overrides() {
        let t: Params = toml::from_str(
            r#"symbol_a = "ETH"
    symbol_b = "BTC"
    period = 50
    entry_z = 2.5
    beta = 1.7
    "#,
        )
        .unwrap();
        let s = PairsZScore::from_params(&t);
        assert_eq!(s.symbol_a, "ETH");
        assert_eq!(s.symbol_b, "BTC");
        assert_eq!(s.period, 50);
        assert!((s.entry_z - 2.5).abs() < 1e-12);
        assert!((s.beta - 1.7).abs() < 1e-12);

        let floored: Params = toml::from_str("period = 1").unwrap();
        assert_eq!(PairsZScore::from_params(&floored).period, 2, "period floored at 2");
    }
}
