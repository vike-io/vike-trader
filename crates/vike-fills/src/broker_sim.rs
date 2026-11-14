//! broker_sim — the canonical cost model: ONE definition of a fill's price and of its commission.
//! Convention: `side_sign` is +1 buy / -1 sell. Slippage is adverse: buys fill up, sells down.
//! [`commission`] reads a contract's KIND (decision 0170) through its `ContractValue`, whose fee
//! arithmetic is `vike_model::ContractValue::fee` (it was `fee` here): the linear arm the scalar
//! product bit for bit, the inverse arm the venues' coin law (the dollars at the price).
//!
//! Funding is not here any more: the one funding law that was `funding_charge` here is
//! `vike_model::ContractValue::funding`, bit for bit (owner, 2026-11-09: contract money has one
//! home), which the simulator, the vector kernel and the paper engine's host call.
//!
//! ## An adverse move may not invert a price
//!
//! [`adverse_fill_price`] is multiplicative — `raw * (1 + side_sign * slippage)` — so an adverse
//! fraction at or past 1.0 on the losing side does not saturate, it INVERTS: a sell of a positive
//! quote comes back NEGATIVE (raw 100, `side_sign = -1`, slippage 1.5 -> -50).
//! `vike_model::round_to` keeps the sign, and nothing downstream re-checks it: the wrong-signed
//! price flows into the notional floor, the fee, the cash flow, the average price and the equity.
//!
//! That regime is REACHABLE: `vike_simulator::OrderBrokerSimulator::slippage_for` returns
//! `slippage + ImpactModel::impact_fraction(..)`, an addend bounded below (a cost is never negative)
//! but not above; `vike_simulator::ImpactModelAlmgrenChriss` crosses 1.0 at roughly 18x the window's mean bar
//! volume once the measured per-bar sigma reaches 20%, and a thin resampled series produces both
//! routinely. `crates/vike-paper/src/client/fee_schedule_tests.rs`'s
//! `schedule_path_clamps_a_slippage_pushed_price_into_the_probability_domain` states the assumption
//! the engine is written against (the sell side "cannot cross zero for any `slippage < 1`");
//! until [`MIN_ADVERSE_FACTOR`] nothing made it true.
//!
//! The floor lives HERE, not at any site that produces a slippage number, because this is the
//! only function holding both operands of the inversion and every producer funnels through it:
//! the flat `slippage` field, `slippage_for`'s impact sum, `StrategyEngine`'s cash pre-check (which
//! must price exactly what `apply_fill` will charge), the vector kernel and the paper exchange. A
//! floor upstream would fix one lane and leave four; one inside a `vike_simulator::ImpactModel` cannot
//! work at all, since a model sees neither the flat `slippage` it is added to nor the side.

use vike_model::{ContractValue, FeeSchedule, OrderFees};

/// The smallest factor [`adverse_fill_price`] will ever multiply a quoted price by: the adverse
/// move saturates once it has consumed 99% of the price.
///
/// A floor on the FACTOR, not the price, and STRICTLY POSITIVE (the load-bearing property): the
/// fill keeps its quote's sign, so a genuinely negative quote (futures, EOD backfills;
/// `vike_model::gross_notional` refuses to `.abs()` it away) stays negative. The magnitude is
/// policy: far above any economically meaningful move, so it binds only on an estimate that
/// already stopped meaning anything, yet far enough from zero that the saturated fill is not a
/// dust price `OrderBrokerSimulator::apply_fill`'s min-notional floor would turn into a dropped order.
///
/// It SATURATES rather than repairs: a run that reaches the floor has a cost model whose output
/// stopped being a cost, and its fill (1% of the quote) should look ruinous, because it is.
/// `vike_simulator::OrderBrokerSimulator::slippage_saturations` counts those fills so such a run says so.
pub const MIN_ADVERSE_FACTOR: f64 = 0.01;

/// The multiplicative factor an adverse move of `slippage` applies on `side_sign`, BEFORE the
/// [`MIN_ADVERSE_FACTOR`] floor. Private and shared so [`adverse_fill_price`] and
/// [`adverse_move_saturates`] can never disagree about what "the factor" is.
#[inline]
fn adverse_factor(side_sign: i32, slippage: f64) -> f64 {
    1.0 + side_sign as f64 * slippage
}

/// The fill price after adverse slippage: buys (+1) up, sells (-1) down — with the move floored at
/// [`MIN_ADVERSE_FACTOR`] times the quote, so it can never carry the price through zero.
///
/// BYTE-IDENTICAL wherever the floor does not bind: binding the factor to a local and multiplying
/// by it is the same two f64 operations in the same order (Rust never contracts `a * b + c` into
/// an FMA), and the branch is taken only when `1 + side_sign * slippage` is at or below zero —
/// exactly the inputs that used to produce a zero or sign-flipped fill.
///
/// NaN is deliberately NOT caught: `factor <= 0.0` is false for NaN, so a NaN slippage still
/// yields a NaN price. It is a different defect from an inversion, and swallowing it here would
/// re-introduce the silent behaviour the floor exists to remove.
#[inline]
pub fn adverse_fill_price(raw_price: f64, side_sign: i32, slippage: f64) -> f64 {
    let factor = adverse_factor(side_sign, slippage);
    let factor = if factor <= 0.0 { MIN_ADVERSE_FACTOR } else { factor };
    raw_price * factor
}

/// Did this `(side_sign, slippage)` pair drive [`adverse_fill_price`] onto its floor — has the
/// adverse move reached or passed the whole quoted price? Lets a caller COUNT a saturation without
/// re-spelling the factor (`vike_simulator::OrderBrokerSimulator::slippage_saturations`). `false` for a NaN
/// slippage, matching the price's NaN pass-through: a NaN did not saturate, it is unmeasurable.
#[inline]
pub fn adverse_move_saturates(side_sign: i32, slippage: f64) -> bool {
    adverse_factor(side_sign, slippage) <= 0.0
}

/// The commission of ONE fill, in the contract's QUOTE currency: the one decision the simulator's
/// `OrderBrokerSimulator::fill_terms` and the paper exchange's `ExecutionClientPaper::commission_for` share, so
/// a backtest and a paper account charge one number. The exact `schedule` when there is one
/// (`FeeSchedule::commission_on`, which knows a rate term from a per-unit or per-order one), else the
/// flat maker or taker rate through `ContractValue::fee`, on what the order is worth by its KIND
/// (decision 0170):
///
/// - Linear: `size` at `price`, the rate scaled by the unit `value.per_unit` (the multiplier).
/// - Inverse: the rate on the order's DOLLARS, `|size|·u` priced at 1, price-free like its notional
///   (`ContractValue::order_notional`). Every venue charges `rate·|q|·u/p` in the settle coin (the
///   inverse research, Part 1), which is this amount at the fill price: the paper book states it in
///   USD and `vike_execution::Account` divides it by the fill price, and the simulator books
///   `ContractValue::in_settle(fee, price)`, the same division.
///
/// `order` is what this fill's ORDER filled and was charged before it, and is advanced by this fill.
/// A schedule with a per-order minimum charges a later fill `max(0, whole − charged)`, `whole` being
/// the schedule on everything the order has filled (at its average price), so an order filled in
/// pieces pays the minimum once. An order's first fill, and every schedule without a minimum, is the
/// one-fill answer, bit for bit.
#[inline]
pub fn commission(
    schedule: Option<FeeSchedule>,
    maker_rate: f64,
    taker_rate: f64,
    is_maker: bool,
    size: f64,
    price: f64,
    value: ContractValue,
    order: &mut OrderFees,
) -> f64 {
    let charge = match schedule {
        Some(
            s
            @ (FeeSchedule::PerShareWithFloor { min, .. } | FeeSchedule::BpsWithFloor { min, .. }),
        ) if min > 0.0 && order.qty > 0.0 => {
            let qty = order.qty + size;
            let quantity_price =
                value.add_quantity_price(order.qty, order.quantity_price, size, price);
            (s.commission_on(is_maker, qty, quantity_price / qty, value) - order.charged).max(0.0)
        }
        Some(s) => s.commission_on(is_maker, size, price, value),
        None => value.fee(size, price, if is_maker { maker_rate } else { taker_rate }),
    };
    order.quantity_price = value.add_quantity_price(order.qty, order.quantity_price, size, price);
    order.qty += size;
    order.charged += charge;
    charge
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pre-floor expression, spelled out once: parity assertions compare against the
    /// arithmetic that shipped, not a paraphrase.
    fn frozen(raw_price: f64, side_sign: i32, slippage: f64) -> f64 {
        raw_price * (1.0 + side_sign as f64 * slippage)
    }

    /// [`commission`] of an order's only fill.
    fn one_fill(
        schedule: Option<FeeSchedule>,
        maker_rate: f64,
        taker_rate: f64,
        is_maker: bool,
        size: f64,
        price: f64,
        value: ContractValue,
    ) -> f64 {
        let fresh = &mut OrderFees::default();
        commission(schedule, maker_rate, taker_rate, is_maker, size, price, value, fresh)
    }

    /// The fees of one order filled in `pieces` of `(size, price)`, charged in turn.
    fn pieces(schedule: FeeSchedule, pieces: &[(f64, f64)]) -> Vec<f64> {
        let mut order = OrderFees::default();
        let one = ContractValue::linear(1.0);
        pieces
            .iter()
            .map(|&(size, px)| {
                commission(Some(schedule), 0.0, 0.0, false, size, px, one, &mut order)
            })
            .collect()
    }

    /// IBKR's stock minimum is per ORDER: 100 shares at 0.005 a share is 0.50, under the 1.00 minimum,
    /// so the order pays 1.00 whether it fills at once or as 33, 33 and 34 (the base charged each piece
    /// the minimum, 3.00). The 1 % notional cap is far off here.
    #[test]
    fn an_order_filled_in_pieces_pays_the_stock_minimum_once() {
        let ibkr = FeeSchedule::PerShareWithFloor { per_share: 0.005, min: 1.0, max_percent: 0.01 };
        let fees = pieces(ibkr, &[(33.0, 100.0), (33.0, 100.0), (34.0, 100.0)]);
        assert_eq!(fees, [1.0, 0.0, 0.0], "the first piece pays the minimum, the rest nothing");
        assert_eq!(fees.iter().sum::<f64>(), ibkr.commission(false, 100.0, 100.0));
        // past the minimum each piece pays what it adds: 300 shares are 1.50, the third piece 0.50
        let big = pieces(ibkr, &[(100.0, 100.0), (100.0, 100.0), (100.0, 100.0)]);
        assert_eq!(big, [1.0, 0.0, 0.5]);
    }

    /// IBKR's FX minimum likewise: 0.2 bp of 30,000 at 1.10 is 0.66, under the 2.00 minimum, so three
    /// pieces of 10,000 pay 2.00 together (the base, 6.00). Three of 50,000 at 1.00 are worth 3.00
    /// whole: the first pays the minimum, the second nothing, the third the 1.00 it adds.
    #[test]
    fn an_order_filled_in_pieces_pays_the_fx_minimum_once() {
        let fx = FeeSchedule::BpsWithFloor { bps: 0.2, min: 2.0 };
        let fees = pieces(fx, &[(10_000.0, 1.10), (10_000.0, 1.10), (10_000.0, 1.10)]);
        assert_eq!(fees, [2.0, 0.0, 0.0]);
        let fees = pieces(fx, &[(50_000.0, 1.0), (50_000.0, 1.0), (50_000.0, 1.0)]);
        assert_eq!(fees[0], 2.0);
        assert!(fees[1].abs() < 1e-12 && (fees[2] - 1.0).abs() < 1e-12, "{fees:?}");
        assert!((fees.iter().sum::<f64>() - fx.commission(false, 150_000.0, 1.0)).abs() < 1e-12);
    }

    /// The notional cap is per ORDER too: 2 contracts at 3,000 then 2 at 30,000, at 0.85 a contract
    /// with a 2.00 minimum and a 1e-4 cap. The first piece alone is capped at 0.60; the whole order
    /// (66,000) is not, and pays 4 × 0.85 = 3.40, where charging each piece alone paid 0.60 + 2.00.
    #[test]
    fn the_notional_cap_is_the_whole_orders() {
        let capped =
            FeeSchedule::PerShareWithFloor { per_share: 0.85, min: 2.0, max_percent: 1e-4 };
        let fees = pieces(capped, &[(2.0, 3_000.0), (2.0, 30_000.0)]);
        assert_eq!(fees[0], capped.commission(false, 2.0, 3_000.0), "the first piece alone");
        assert!((fees[0] - 0.6).abs() < 1e-12, "{fees:?}");
        assert!((fees.iter().sum::<f64>() - 3.4).abs() < 1e-12, "{fees:?}");
    }

    /// A first fill is the one-fill answer bit for bit, and a schedule without a minimum keeps the
    /// per-fill answer on every later fill too.
    #[test]
    fn a_first_fill_and_a_schedule_without_a_minimum_are_unchanged() {
        let five = ContractValue::linear(5.0);
        let pct = FeeSchedule::PercentMakerTaker { maker_bps: 2.0, taker_bps: 7.0 };
        let no_min =
            FeeSchedule::PerShareWithFloor { per_share: 0.005, min: 0.0, max_percent: 0.0 };
        let floor = FeeSchedule::BpsWithFloor { bps: 0.2, min: 2.0 };
        for sched in [None, Some(pct), Some(no_min), Some(floor)] {
            let mut order = OrderFees::default();
            let first = commission(sched, 0.001, 0.002, false, 3.0, 7.0, five, &mut order);
            assert_eq!(
                first.to_bits(),
                one_fill(sched, 0.001, 0.002, false, 3.0, 7.0, five).to_bits()
            );
            assert_eq!(order, OrderFees { qty: 3.0, quantity_price: 21.0, charged: first });
            if sched != Some(floor) {
                let later = commission(sched, 0.001, 0.002, false, 4.0, 9.0, five, &mut order);
                let alone = one_fill(sched, 0.001, 0.002, false, 4.0, 9.0, five);
                assert_eq!(later.to_bits(), alone.to_bits(), "{sched:?}");
            }
        }
    }

    /// The order's running average is the contract's (`ContractValue::add_quantity_price`): 1,000 USD of a
    /// coin-margined contract at 50,000 and 1,000 at 60,000 averaged 54,545.45, not 55,000.
    #[test]
    fn an_inverse_orders_running_average_is_harmonic() {
        let floor = Some(FeeSchedule::BpsWithFloor { bps: 0.2, min: 2.0 });
        let inverse = ContractValue::inverse(1.0);
        let mut order = OrderFees::default();
        for px in [50_000.0, 60_000.0] {
            commission(floor, 0.0, 0.0, false, 1_000.0, px, inverse, &mut order);
        }
        let avg = order.quantity_price / order.qty;
        assert!((avg - 54_545.454_545).abs() < 1e-6, "{avg}");
    }

    /// The case the min-notional floor of `crates/vike-simulator/src/engine/sim_broker/fills.rs`'s
    /// `fill_terms` used to trip on: sell 2 @ raw 100 with a total slippage of 1.5 priced at -50, which
    /// made that floor's SIGNED product unconditionally trip. It now saturates instead.
    #[test]
    fn a_sell_past_full_slippage_no_longer_inverts_the_price() {
        let px = adverse_fill_price(100.0, -1, 1.5);
        assert!(px > 0.0, "a positive quote must not sell for a negative price, got {px}");
        assert_eq!(px.to_bits(), (100.0 * MIN_ADVERSE_FACTOR).to_bits());
        assert!(adverse_move_saturates(-1, 1.5));
        // The SIGNED notional `OrderBrokerSimulator::apply_fill`'s below-min floor computes is positive again.
        assert!(2.0 * px > 0.0);
    }

    /// Exactly 100% adverse is the boundary and it SATURATES rather than filling at zero: a
    /// zero-priced fill is its own poison (zero notional, free position, meaningless basis).
    #[test]
    fn the_boundary_at_a_full_hundred_percent_saturates_rather_than_filling_at_zero() {
        assert_eq!(frozen(100.0, -1, 1.0), 0.0, "the frozen path filled AT zero here");
        assert!(adverse_move_saturates(-1, 1.0));
        assert_eq!(adverse_fill_price(100.0, -1, 1.0).to_bits(), 1.0f64.to_bits());
    }

    /// The mirror case: a BUY with a large NEGATIVE slippage (price improvement, the only way a buy
    /// can cross zero) is floored too — the guard is on the FACTOR, not the sell side.
    #[test]
    fn a_buy_with_a_past_full_price_improvement_is_floored_too() {
        assert!(frozen(100.0, 1, -1.5) < 0.0, "the frozen path inverted here");
        assert!(adverse_move_saturates(1, -1.5));
        assert_eq!(adverse_fill_price(100.0, 1, -1.5).to_bits(), 1.0f64.to_bits());
    }

    /// A genuinely NEGATIVE quote (futures, EOD backfills) keeps its sign in BOTH the normal and
    /// the saturated case: the floor is on the factor.
    #[test]
    fn a_negative_quoted_price_is_not_repaired() {
        for &side in &[1, -1] {
            for &slip in &[0.0, 0.0005, 0.01, 0.5, 0.99] {
                let got = adverse_fill_price(-50.0, side, slip);
                assert_eq!(got.to_bits(), frozen(-50.0, side, slip).to_bits());
                assert!(got < 0.0, "sign changed at side={side} slippage={slip}: {got}");
                assert!(!adverse_move_saturates(side, slip));
            }
        }
        // Even ON the floor the quote's own sign survives — the fill is not forced positive.
        let saturated = adverse_fill_price(-50.0, -1, 2.0);
        assert!(saturated < 0.0, "a negative quote must stay negative when floored: {saturated}");
        assert_eq!(saturated.to_bits(), (-50.0 * MIN_ADVERSE_FACTOR).to_bits());
    }

    /// BYTE-PARITY: every input whose factor stayed positive prices bit-for-bit as it did before
    /// the floor existed. Swept over a deterministic xorshift grid rather than a handful of points,
    /// because "no re-association" is the claim being made and one ulp is the failure mode.
    #[test]
    fn every_non_inverting_input_is_bit_for_bit_unchanged() {
        fn next(seed: &mut u64) -> f64 {
            *seed ^= *seed << 13;
            *seed ^= *seed >> 7;
            *seed ^= *seed << 17;
            (*seed >> 11) as f64 / (1u64 << 53) as f64
        }
        let seed = &mut 0x243F_6A88_85A3_08D3u64;
        for _ in 0..200_000 {
            let raw = next(seed) * 200_000.0 - 100_000.0; // both signs, wide magnitude
            let slip = next(seed) * 0.999_999; // any move short of the whole price
            for side in [-1i32, 1] {
                assert!(!adverse_move_saturates(side, slip));
                assert_eq!(
                    adverse_fill_price(raw, side, slip).to_bits(),
                    frozen(raw, side, slip).to_bits(),
                    "moved at raw={raw} side={side} slippage={slip}"
                );
            }
        }
        // The pinned scalars the r1 golden fixture is built from live in this same regime — every
        // `slippage` it carries is under 0.003 — so the fixture is untouched by construction.
        for &slip in &[0.0, 0.0005, 0.001, 0.002] {
            for side in [-1i32, 1] {
                assert_eq!(
                    adverse_fill_price(46_842.6, side, slip).to_bits(),
                    frozen(46_842.6, side, slip).to_bits()
                );
            }
        }
    }

    /// A NaN slippage stays a NaN price. It is a different defect and the floor must not hide it.
    #[test]
    fn a_nan_slippage_is_not_swallowed_by_the_floor() {
        assert!(adverse_fill_price(100.0, -1, f64::NAN).is_nan());
        assert!(
            !adverse_move_saturates(-1, f64::NAN),
            "a NaN did not saturate, it is unmeasurable"
        );
    }

    /// No schedule is the flat rate of the fill's side, bit for bit; a schedule wins over the rates,
    /// and its per-order minimum is one per ORDER whatever the unit.
    #[test]
    fn a_commission_is_the_schedule_else_the_rate_of_its_side() {
        let five = ContractValue::linear(5.0);
        let flat = |is_maker| one_fill(None, 0.001, 0.002, is_maker, 3.0, 7.0, five);
        assert_eq!(flat(true).to_bits(), (3.0 * 7.0 * 0.001 * 5.0f64).to_bits());
        assert_eq!(flat(false).to_bits(), (3.0 * 7.0 * 0.002 * 5.0f64).to_bits());

        let floor = FeeSchedule::PerShareWithFloor { per_share: 0.005, min: 1.0, max_percent: 0.0 };
        assert_eq!(one_fill(Some(floor), 0.001, 0.002, false, 3.0, 7.0, five), 1.0);
    }

    /// An IBKR future (ES, multiplier 50) at 5,000, 2 contracts, 0.85 a contract with a 2.00 minimum:
    /// 2 × 0.85 = 1.70 is under the minimum, so the order pays 2.00. Scaling the unit-1 answer by the
    /// multiplier charged 100 (and 85 had the per-contract term won). The rate terms still scale: a
    /// notional cap is a fraction of the 500,000 the order is worth.
    #[test]
    fn a_per_contract_fee_and_its_minimum_are_not_scaled_by_the_multiplier() {
        let es = ContractValue::linear(50.0);
        let ibkr = FeeSchedule::PerShareWithFloor { per_share: 0.85, min: 2.0, max_percent: 0.0 };
        assert_eq!(one_fill(Some(ibkr), 0.0, 0.0, false, 2.0, 5_000.0, es), 2.0);
        assert_eq!(one_fill(Some(ibkr), 0.0, 0.0, false, 10.0, 5_000.0, es), 8.5, "10 × 0.85");
        let capped =
            FeeSchedule::PerShareWithFloor { per_share: 0.85, min: 2.0, max_percent: 1e-6 };
        // the cap, 1e-6 of 2 × 5,000 × 50 = 0.5, is a notional bound and binds under the minimum
        assert_eq!(
            one_fill(Some(capped), 0.0, 0.0, false, 2.0, 5_000.0, es).to_bits(),
            (1e-6 * 2.0 * 5_000.0 * 50.0f64).to_bits()
        );
        let pct = FeeSchedule::PercentMakerTaker { maker_bps: 1.0, taker_bps: 1.0 };
        assert_eq!(
            one_fill(Some(pct), 0.0, 0.0, false, 2.0, 5_000.0, es).to_bits(),
            (pct.commission(false, 2.0, 5_000.0) * 50.0).to_bits(),
            "a rate term is the unit-1 rate times the unit, bit for bit"
        );
    }

    /// A linear value is the scalar fee and funding, bit for bit, schedule or not: the expression the
    /// commission had when it took a bare multiplier, for every RATE-shaped term (a percent, a binding
    /// notional cap, a premium percent). The per-share term and the minimum are not scaled
    /// (`a_per_contract_fee_and_its_minimum_are_not_scaled_by_the_multiplier`).
    #[test]
    fn the_linear_arms_are_the_scalar_fee_and_funding_bit_for_bit() {
        // the cap, 1e-4 × 3 × 7 × m, binds under the 1.0 minimum at every unit below
        let capped =
            FeeSchedule::PerShareWithFloor { per_share: 0.005, min: 1.0, max_percent: 1e-4 };
        let pct = FeeSchedule::PercentMakerTaker { maker_bps: 2.0, taker_bps: 7.0 };
        let premium = FeeSchedule::PercentOfUnderlying { bps: 3.0, premium_cap_percent: 0.125 };
        for sched in [None, Some(capped), Some(pct), Some(premium)] {
            for m in [1.0, 10.0, 0.0001] {
                for maker in [true, false] {
                    let got =
                        one_fill(sched, 0.001, 0.002, maker, 3.0, 7.0, ContractValue::linear(m));
                    let was = match sched {
                        Some(s) => s.commission(maker, 3.0, 7.0) * m,
                        None => 3.0 * 7.0 * (if maker { 0.001 } else { 0.002 }) * m,
                    };
                    assert_eq!(got.to_bits(), was.to_bits(), "{sched:?} at {m}");
                }
                let f = ContractValue::linear(m).funding(-2.5, 65_700.0, 0.0001);
                assert_eq!(f.to_bits(), (-2.5 * 65_700.0 * 0.0001 * m).to_bits());
            }
        }
    }

    /// The venues' own fee examples (the inverse research, Part 1), through the inverse arm and the
    /// coin conversion a ledger applies (the dollars over the fill price).
    #[test]
    fn an_inverse_fee_is_the_rate_on_its_dollars_in_coin_at_the_fill_price() {
        let coin = |u: f64, qty: f64, px: f64, rate: f64| {
            let v = ContractValue::inverse(u);
            v.in_settle(one_fill(None, rate, rate, false, qty, px, v), px)
        };
        // bybit: (1,000 / 5,000) × 0.055% = 0.00011 BTC
        assert!((coin(1.0, 1_000.0, 5_000.0, 0.000_55) - 0.000_11).abs() < 1e-15);
        // okx: 100 USD × 100 contracts ÷ 10,000 × 0.05% = 0.0005 BTC
        assert!((coin(100.0, 100.0, 10_000.0, 0.000_5) - 0.000_5).abs() < 1e-15);
        // binance COIN-M: 10 × 100 / 10,104 = 0.09897 BTC, × 0.05% = 0.000049485 BTC (rounded)
        assert!((coin(100.0, 10.0, 10_104.0, 0.000_5) - 0.000_049_485).abs() < 1e-9);
        // deribit, the research's worked example: 1,000 USD at 60,000, taker 3.5 bp
        assert!((coin(1.0, 1_000.0, 60_000.0, 0.000_35) - 0.000_005_833_333_333).abs() < 1e-15);
        // in dollars it is price-free, as the paper book states it: 0.35 at any price, either side
        let v = ContractValue::inverse(1.0);
        let at = |rate_m: f64, rate_t: f64, maker: bool, qty: f64, px: f64| {
            one_fill(None, rate_m, rate_t, maker, qty, px, v)
        };
        let fee = at(0.0, 0.000_35, false, 1_000.0, 60_000.0);
        assert!((fee - 0.35).abs() < 1e-15, "{fee}");
        assert_eq!(at(0.0, 0.000_35, false, -1_000.0, 1.0).to_bits(), fee.to_bits());
        assert_eq!(at(0.000_35, 0.0, true, 1_000.0, 1.0).to_bits(), fee.to_bits(), "maker rate");
    }

    /// An infinite adverse move on the losing side is an inversion like any other (`-inf` from a
    /// positive quote); on the winning side it is not, and passes through.
    #[test]
    fn an_infinite_adverse_move_is_floored_only_where_it_inverts() {
        assert!(adverse_move_saturates(-1, f64::INFINITY));
        assert_eq!(adverse_fill_price(100.0, -1, f64::INFINITY).to_bits(), 1.0f64.to_bits());
        assert!(!adverse_move_saturates(1, f64::INFINITY));
        assert_eq!(adverse_fill_price(100.0, 1, f64::INFINITY), f64::INFINITY);
    }

    /// The predicate and the price function read the SAME factor, so the predicate exactly decides
    /// which of the two arms the price took — they cannot drift apart.
    #[test]
    fn the_predicate_decides_exactly_which_arm_the_price_took() {
        for &raw in &[100.0f64, -100.0, 0.25] {
            for side in [-1i32, 1] {
                for &slip in &[-3.0f64, -1.5, -1.0, -0.5, 0.0, 0.5, 0.999, 1.0, 1.5, 3.0] {
                    let got = adverse_fill_price(raw, side, slip);
                    if adverse_move_saturates(side, slip) {
                        assert_eq!(
                            got.to_bits(),
                            (raw * MIN_ADVERSE_FACTOR).to_bits(),
                            "saturated but not floored: raw={raw} side={side} slippage={slip}"
                        );
                    } else {
                        assert_eq!(
                            got.to_bits(),
                            frozen(raw, side, slip).to_bits(),
                            "not saturated but moved: raw={raw} side={side} slippage={slip}"
                        );
                    }
                }
            }
        }
    }
}
