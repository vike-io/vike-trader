//! `FeeSchedule` — the per-venue transaction-fee model (integration steal-list #5).
//!
//! Two seams, kept apart on purpose (see `docs/superpowers/specs/2026-07-17-fee-model-design.md`):
//! - **data**: [`FeeSchedule`] describes a venue's fee *shape and rate* (crypto maker/taker bps,
//!   equities per-share+floor, or free), and `vike_venues::fees::fee_schedule_for` is the per-venue
//!   registry of real published tier-0 defaults, a table over this shape that lives in `vike-venues`
//!   (decision 0162).
//! - **logic**: [`FeeSchedule::commission`] is the pure fee-application function (LEAN's
//!   `GetOrderFee` analog): `(is_maker, qty, px) -> commission` in quote currency, deterministic
//!   (no clock/env reads), so it is backtest-safe.
//!
//! The `slippage` cost stays SEPARATE (it lives on the paper/backtest engine, not here); this
//! module models commission only. Perp funding is a different path
//! (`FundingEvent`/`Account.funding_paid`) and is untouched.
//!
//! Prediction-market probability curve ([`FeeSchedule::ProbabilityScaled`], pm-economics lane):
//! `fee = qty × rate × p × (1−p)` with `p` the fill price clamped into the `[0,1]` probability
//! domain — the fee vanishes at the certainty bounds (p→0 or p→1) and peaks at p=0.5, mirroring
//! how prediction-market venues scale fees by outcome uncertainty (the mechanism behind
//! other engines' Polymarket fee models, reimplemented independently). Maker fills may
//! additionally earn a rebate: `maker_rebate_share` of the equivalent TAKER fee is subtracted, so
//! with `maker_rate == 0.0` and a positive share the maker commission is NEGATIVE (a rebate — the
//! sign convention below). **Registry decision (default-compatible, byte-identical):**
//! `vike_venues::fees::fee_schedule_for` KEEPS returning [`FeeSchedule::Free`] for `"polymarket"` — the paper fill
//! path (`vike_paper` via `vike_tradehub_core::make_engine`, #414/#416; it was `vike_backtest::paper`
//! until the paper exchange left that crate, and this named that path until 2026-09-28) and the
//! snapshot cost display consume that registry directly, so flipping the default would change
//! existing users' numbers. The fee curve is an OPT-IN: `vike_venues::fees::fee_schedule_for_with_pm_curve` returns
//! `vike_venues::fees::POLYMARKET_V2_FEE_CURVE` — the verified **2026 Polymarket V2 regime** (V2 launched
//! 2026-04-28; sports taker `0.05`, maker rebate `15 %` of collected taker fees as a NEGATIVE maker
//! commission) — for `"polymarket"`, and delegates to `fee_schedule_for` for every other venue.
//! The separate all-zero `POLYMARKET_PROB_CURVE` is retained as the numerically-identical-to-
//! `Free` SHAPE template (its zero rates are relied on by the backtest's zero-rate regression
//! guard). See `POLYMARKET_V2_FEE_CURVE`'s doc for the per-market rate variation, per-market
//! rebate pooling, and USDC-at-match caveats that a per-venue, per-fill schedule cannot express —
//! each a `vike-backtest` follow-up, not fixable in `vike-model` alone.
//!
//! **Maker/taker classification caveat (matters most for a rebate-bearing schedule).** Every
//! commission call site decides `is_maker` from the ORDER KIND, not from crossing aggressiveness:
//! `vike_paper::ExecutionClientPaper`'s `emit_fill`, the backtest engine's `dispatch_fill` and its
//! cash-gated lane all ask [`crate::OrderResting::books_as_maker`], which classifies
//! `OrderKind::Limit` as maker; a paper COMBO has no working order and asks whether it carries a
//! `net_limit` (`crates/vike-paper/src/client/combo.rs`); a TAGGED quote is a limit by construction
//! and the engine's tagged lanes pass `true`. A MARKETABLE limit (priced through the book, filled
//! immediately at the next bar's open) is therefore booked as a maker fill. Under the fee shapes
//! that predate this module that only understated the cost by `taker − maker`; under a
//! [`FeeSchedule::ProbabilityScaled`] with a positive `maker_rebate_share` it flips the SIGN — an
//! aggressive limit EARNS a rebate instead of paying the taker fee, so a strategy that uses limits
//! as marketable orders books systematically negative costs. Reclassifying by marketability would
//! change the fee of every existing limit fill (a parity-gated path), so it stays as-is and is
//! documented here: model aggressive orders as `OrderKind::Market` when the fee sign matters.
//!
//! Deribit options (fee model follow-up 2): Deribit's real options fee is
//! `min(0.03% × underlying, 12.5% × premium)`, which [`FeeSchedule::commission_with_underlying`]
//! computes. [`FeeSchedule::commission`] on the [`FeeSchedule::PercentOfUnderlying`] shape has no
//! underlying, so it charges the 0.03% on the premium, which understates an option's fee by the
//! underlying-to-premium ratio (41 times on a measured fill). No option is charged that way: the
//! backtest and the Studio charge an option on its underlying's series and refuse a run without
//! one, and the paper book refuses the order unless an underlying source prices it. Live Deribit
//! fills report the venue's real commission.

use crate::money::contract_value::ContractValue;

/// A venue's fee shape + rate. `Copy` (all fields scalar) so it is cheap to pass by value.
///
/// Sign convention (matching the venues' own execution reports): a positive
/// commission is a cost; a negative commission is a rebate. Two ways to reach a rebate: a live
/// maker-rebate rate fetched from a venue, or a STATICALLY constructed
/// [`FeeSchedule::ProbabilityScaled`] with `maker_rebate_share > 0` (its maker side then charges
/// `maker_fee − share × taker_fee`, negative whenever the rebate outweighs the maker rate). Every
/// entry in the `fee_schedule_for` registry is non-negative on both sides.
///
/// ⚠ **The serde shape is a WIRE format**: the gatewayhub lease carries it as it is (the `FeeRates` reconcile
/// reply, `fixtures/gatewayhub_wire/v1_trade_down.jsonl`), externally tagged with the field names below. A
/// variant or a field renamed here is a protocol break, and a NEW variant is one an older reader cannot decode:
/// `crates/vike-gatewayhub-client/tests/wire_tag_fixtures.rs`'s `MODEL_FEES_SOURCE` row refuses a variant no golden
/// frame carries.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum FeeSchedule {
    /// Crypto maker/taker as **basis points of quote-notional** (`qty * px`). The required variant.
    PercentMakerTaker { maker_bps: f64, taker_bps: f64 },
    /// Equities-shaped: a per-share fee, a minimum floor, and a maximum-percent-of-notional cap
    /// (the Interactive Brokers fixed schedule shape). `is_maker` does not apply.
    PerShareWithFloor { per_share: f64, min: f64, max_percent: f64 },
    /// Deribit-options-shaped: `bps` of the **underlying** notional, capped at `premium_cap_percent` of
    /// the **premium** notional (Deribit's real rule: `min(0.03% × underlying, 12.5% × premium)`,
    /// maker == taker). The accurate figure needs BOTH the underlying price AND the premium — use
    /// [`FeeSchedule::commission_with_underlying`]. The plain [`FeeSchedule::commission`], which
    /// has no underlying, gives `min(bps × premium, premium_cap_percent × premium)`: the bps of the
    /// notional for a symbol that is no option, and an understatement for an option, which is
    /// therefore never charged through it (the module note says where each fill site goes instead).
    PercentOfUnderlying { bps: f64, premium_cap_percent: f64 },
    /// Prediction-market-shaped (Polymarket-style): `fee = qty × rate × p × (1−p)` where `p` is the
    /// fill price clamped into the `[0,1]` probability domain. `taker_rate`/`maker_rate` are plain
    /// FRACTIONS of the p(1−p)-scaled share count (NOT bps — a probability price has no bps-of-
    /// notional convention). Maker fills subtract a rebate of `maker_rebate_share` × the equivalent
    /// taker fee, so `maker_rate == 0.0` with a positive share yields a NEGATIVE maker commission
    /// (a rebate, per the sign convention above). See the module doc for the registry decision.
    ProbabilityScaled { taker_rate: f64, maker_rate: f64, maker_rebate_share: f64 },
    /// Interactive-Brokers-FX-shaped: `bps` of the notional (maker == taker), and never less than
    /// `min` per ORDER. `min` is a currency amount, so like [`FeeSchedule::PerShareWithFloor`]'s it
    /// is not scaled by the contract unit, and it is read in whatever money the caller books the
    /// fee in (the registry's one row states dollars). The newest shape on the wire: a reader older
    /// than it fails to decode it, and no venue's `fetch_fee_rates` answers this shape today.
    BpsWithFloor { bps: f64, min: f64 },
    /// No per-order commission — FX/CFD venues charge via the spread; polymarket CLOB V2 dropped
    /// `feeRateBps`; US-equities cash accounts are commission-free.
    Free,
}

impl FeeSchedule {
    /// The `GetOrderFee` analog: the commission (quote currency) for a fill of `qty` @ `px`, given
    /// whether it was a maker (resting-limit) or taker (crossing) fill, on a contract whose one unit
    /// is one unit of the quote ([`Self::commission_on`] at the default value). Pure — no clock/env reads.
    #[inline]
    pub fn commission(&self, is_maker: bool, qty: f64, px: f64) -> f64 {
        self.commission_on(is_maker, qty, px, ContractValue::default())
    }

    /// [`Self::commission`] for a contract of any `value` (decision 0170): the RATE terms (the
    /// percent of notional, the notional cap, the premium percent) are charged on what the order is
    /// worth, `qty·px·u` for a linear unit and its dollars `|qty|·u` for an inverse one
    /// ([`ContractValue::order_notional`]), and the CURRENCY terms are not scaled by the unit: a
    /// per-share fee is per unit of `qty` (one ES contract pays one `per_share`), a minimum is per
    /// ORDER, and the probability curve is per share. Every linear rate term keeps the factor order
    /// `qty·px·rate·u` the simulator's flat `fee` has, so it is bit for bit what scaling the unit-1
    /// answer by `u` gave.
    ///
    /// A minimum (and a notional cap) is per ORDER and this answers `qty` as a whole order: an order
    /// filled in pieces is charged through `vike_fills::broker_sim::commission`, which carries the
    /// order's [`OrderFees`] and charges each later piece what it adds to the whole order's answer.
    ///
    /// The money is `ContractValue`'s: a percent term is [`ContractValue::fee`], and the cap and the
    /// premium terms read its fee basis in their own factor order.
    #[inline]
    pub fn commission_on(&self, is_maker: bool, qty: f64, px: f64, value: ContractValue) -> f64 {
        // What a rate term is charged on, as `nq · np · m`.
        let (nq, np, m) = value.fee_basis(qty, px);
        match self {
            FeeSchedule::PercentMakerTaker { maker_bps, taker_bps } => {
                let bps = if is_maker { *maker_bps } else { *taker_bps };
                value.fee(qty, px, bps / 10_000.0)
            }
            FeeSchedule::PerShareWithFloor { per_share, min, max_percent: max_pct } => {
                let raw = (per_share * qty).max(*min);
                let cap = max_pct * nq * np * m;
                // The cap is a ceiling only when it is a real (positive) notional bound.
                if cap > 0.0 { raw.min(cap) } else { raw }
            }
            FeeSchedule::BpsWithFloor { bps, min } => value.fee(qty, px, bps / 10_000.0).max(*min),
            // No underlying available at the plain-commission site (px is the traded PREMIUM), so
            // this is the premium-cap-bounded approximation. `commission_with_underlying` is the
            // accurate path when an underlying/index price is available (see that method + the
            // variant doc). `is_maker` does not apply (Deribit options maker == taker).
            FeeSchedule::PercentOfUnderlying { bps, premium_cap_percent: premium_cap_pct } => {
                let premium_notional = nq * np;
                (premium_notional * (bps / 10_000.0)).min(premium_notional * premium_cap_pct) * m
            }
            // The p(1−p) probability curve: zero at the certainty bounds, peak at p = 0.5. `px` is
            // clamped into [0,1] — a paper/backtest price a hair outside the domain (slippage
            // adjustment on a 0/1-priced fill) must not manufacture a negative curve. The maker
            // rebate is `maker_rebate_share` of the EQUIVALENT TAKER fee (not of the maker fee),
            // so with `maker_rate == 0` the maker commission is the negative rebate outright.
            FeeSchedule::ProbabilityScaled { taker_rate, maker_rate, maker_rebate_share } => {
                let p = px.clamp(0.0, 1.0);
                let curve = p * (1.0 - p);
                let taker_fee = qty * taker_rate * curve;
                if is_maker {
                    qty * maker_rate * curve - maker_rebate_share * taker_fee
                } else {
                    taker_fee
                }
            }
            FeeSchedule::Free => 0.0,
        }
    }

    /// The ACCURATE Deribit-options fee when both the traded `premium` and the `underlying`/index
    /// price are known: `min(bps × underlying × qty, premium_cap_percent × premium × qty)` — the real
    /// `min(0.03%-of-underlying, 12.5%-of-premium)` rule. The 12.5%-of-premium cap genuinely binds
    /// for cheap deep-OTM options (where `0.125 × premium < 0.0003 × underlying`), protecting the
    /// buyer. For every NON-`PercentOfUnderlying` shape the `underlying` is irrelevant and this
    /// delegates to [`Self::commission`] on the `premium` (side-agnostic taker). Pure — no I/O.
    ///
    /// Its callers: the simulator's fill (`vike_simulator::StrategyEngineConfig::fee_underlyings`, which the
    /// backtest harness and the Studio fill from the run's underlying series) and the paper book's
    /// (`vike_paper::ExecutionClientPaper::with_underlying_source`, which no production code supplies
    /// yet, so the paper book refuses an option order instead). Live Deribit fills report the real
    /// commission.
    #[inline]
    pub fn commission_with_underlying(&self, qty: f64, premium: f64, underlying: f64) -> f64 {
        match self {
            FeeSchedule::PercentOfUnderlying { bps, premium_cap_percent: premium_cap_pct } => {
                let underlying_term = qty * underlying * (bps / 10_000.0);
                let premium_cap = qty * premium * premium_cap_pct;
                underlying_term.min(premium_cap)
            }
            _ => self.commission(false, qty, premium),
        }
    }

    /// Flat maker/taker fractions (bps ÷ 10_000) for the percent shape — the raw rate inputs the
    /// paper/backtest primitives take. `(0.0, 0.0)` for the non-percent shapes — including
    /// [`FeeSchedule::ProbabilityScaled`], whose effective rate is price-dependent (p(1−p)-scaled)
    /// and has no flat equivalent.
    #[inline]
    pub fn maker_taker_rates(&self) -> (f64, f64) {
        match self {
            FeeSchedule::PercentMakerTaker { maker_bps, taker_bps } => {
                (maker_bps / 10_000.0, taker_bps / 10_000.0)
            }
            // Deribit options maker == taker; report the bps-of-notional fraction for both (the
            // premium cap is not expressible as a flat rate). The same for the floored rate: the
            // floor is not expressible either, so the pair understates a small order.
            FeeSchedule::PercentOfUnderlying { bps, .. }
            | FeeSchedule::BpsWithFloor { bps, .. } => (bps / 10_000.0, bps / 10_000.0),
            _ => (0.0, 0.0),
        }
    }

    /// Build a [`FeeSchedule::PercentMakerTaker`] from raw **fractions** (e.g. `0.001` = 10 bps),
    /// the form venue fee endpoints report. Multiplies to bps ONCE (no rate→bps→rate round-trip).
    #[inline]
    pub fn from_fractions(maker: f64, taker: f64) -> Self {
        FeeSchedule::PercentMakerTaker { maker_bps: maker * 10_000.0, taker_bps: taker * 10_000.0 }
    }

    /// Alias of [`Self::from_fractions`] for the Binance `/api/v3/account` `commissionRates`
    /// (already fractions) — named at the call site for clarity.
    #[inline]
    pub fn from_binance_rates(maker: f64, taker: f64) -> Self {
        Self::from_fractions(maker, taker)
    }
}

/// What one order has filled and been charged so far: what a fill site carries so a per-ORDER minimum
/// ([`FeeSchedule::PerShareWithFloor`], [`FeeSchedule::BpsWithFloor`]) is charged once per order, not
/// once per fill (`vike_fills::broker_sim::commission`). The default is an order with no fill yet.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct OrderFees {
    /// The quantity filled so far.
    pub qty: f64,
    /// Those fills' `qty × average price` ([`crate::ContractValue::add_quantity_price`]: `Σ qty × price` on a
    /// linear contract, the harmonic average on an inverse one), so `quantity_price / qty` is the average
    /// fill price.
    pub quantity_price: f64,
    /// The commission charged on them, in the money [`FeeSchedule::commission_on`] answers in.
    pub charged: f64,
}

/// The CROSS-EXCHANGE round-trip fee an xEMM pays for one full cycle: rest passively on the maker
/// venue, get filled, cross the taker venue to hedge. Returns the total as a FRACTION OF PRICE —
/// exactly the `total_fee` argument `vike_market_making::xemm::pricing::xemm_maker_quotes` backs its quotes
/// off by, on top of the required edge.
///
/// `maker_a + taker_b` by default: `maker` is the schedule of the venue the quote RESTS on,
/// `taker` of the venue the hedge CROSSES.
///
/// ## `None` is a REFUSAL, and callers must not default it to `0.0`
///
/// [`FeeSchedule::maker_taker_rates`] flattens three shapes to `(0.0, 0.0)` because they have no
/// flat-fraction equivalent at all: [`FeeSchedule::Free`] (the spread-charging /
/// commission-free venues), [`FeeSchedule::PerShareWithFloor`] (ibkr stocks and futures — per SHARE
/// or contract, with a floor and a notional cap, so the fraction depends on price and size) and
/// [`FeeSchedule::ProbabilityScaled`] (the polymarket p(1−p) curve, price-dependent by
/// construction). [`FeeSchedule::BpsWithFloor`] (ibkr FX) is refused too: its per-order minimum
/// makes the fraction depend on the size. A silent `0.0` there would make the maker quote at the reference touch plus
/// `min_profitability` alone — pricing a fee-bearing round trip as free, and systematically
/// under-charging every quote it rests. So the shape is matched, NOT the value: a genuine 0-bps
/// [`FeeSchedule::PercentMakerTaker`] is accepted and yields `Some(0.0)`.
///
/// [`FeeSchedule::PercentOfUnderlying`] (deribit) is also REFUSED: it reports bps-of-NOTIONAL for
/// both sides while the real charge is `min(bps × underlying, 12.5% × premium)`, so a flat fraction
/// UNDERSTATES it — and understating the fee is the dangerous direction (it narrows the maker
/// spread below break-even). A deribit-legged xEMM needs a fee model this scalar cannot express.
///
/// ## `assume_taker_on_maker_leg` — why it defaults to FALSE
///
/// A resting quote that gets CROSSED INTO is a genuine maker fill; the taker rate applies only to a
/// quote that was MARKETABLE AT SUBMIT. `vike_market_making::xemm::pricing::passive_clamp` structurally
/// prevents that (it holds each side strictly inside the maker venue's own touch), which is the
/// substitute for the post-only no roster venue supports. Defaulting to `taker + taker` would
/// roughly double the fee term on a crypto pair and back the maker so far off the reference touch
/// that it never fills — disabling the strategy in the name of caution. A caller that cannot rely
/// on the clamp (no maker-venue book, an aggressive style) passes `true`.
///
/// ⚠ Maker/taker is classified by ORDER KIND, not by crossing aggressiveness, at both fill sites
/// (`vike_paper`'s `is_maker = kind == Limit`, and `vike_backtest`'s engine) — so a marketable
/// limit books as a MAKER fill in paper/backtest and pays TAKER live. Read a rehearsal's PnL with
/// that systematic optimism in mind.
///
/// PURE, naive fold (no `mul_add`), no I/O.
pub fn xemm_round_trip_fee(
    maker: FeeSchedule,
    taker: FeeSchedule,
    assume_taker_on_maker_leg: bool,
) -> Option<f64> {
    let maker_rate = flat_rate(maker, assume_taker_on_maker_leg)?;
    let taker_rate = flat_rate(taker, true)?;
    Some(maker_rate + taker_rate)
}

/// The SINGLE-VENUE round-trip fee a market maker pays for one full cycle: rest a bid on this
/// venue, get filled, rest an ask on the same venue, get filled. **Both legs are MAKER fills**, so
/// the total is `maker + maker`, returned as a FRACTION OF PRICE — the one-venue sibling of
/// [`xemm_round_trip_fee`] (whose second leg CROSSES a different venue and therefore pays taker).
///
/// ## What a consumer does with it: the per-side break-even half-spread is HALF of this
///
/// A two-sided maker resting at `mid ± δ` captures `2δ` on a completed round trip and pays
/// `m·P_buy + m·P_sell ≈ this` fee against `P ≈ mid`. So break-even is `2δ ≥ fee·P`, i.e.
/// **`δ ≥ ½ · fee · P` per side** — one leg's rate times the price, even though the bar being
/// cleared is the whole round trip. That is why this returns the ROUND TRIP and the `½` lives at
/// the call site (`vike_market_making::avellaneda::break_even_half_spread`): a half-spread is half a spread,
/// and stating it any other way invites the factor-of-two error this whole seam exists to catch.
///
/// ## What it does NOT cover
///
/// A cycle whose EXIT is a taker (a flatten, a stop, an inventory unwind) pays `maker + taker`,
/// which is strictly larger on every `fee_schedule_for` row where the two differ. This number is
/// therefore a LOWER bound on the cost of getting flat, and a maker floored at it is protected
/// against the passive-round-trip loss only. Feed [`xemm_round_trip_fee`]`(s, s, false)` for the
/// maker-in/taker-out bar.
///
/// ## `None` is a REFUSAL, and callers must not default it to `0.0`
///
/// Identical shape rule to [`xemm_round_trip_fee`], and for the identical reason: only
/// [`FeeSchedule::PercentMakerTaker`] has a flat fraction-of-price at all. [`FeeSchedule::Free`]
/// (the FX/CFD venues that charge through the SPREAD),
/// [`FeeSchedule::PerShareWithFloor`] and [`FeeSchedule::BpsWithFloor`] (ibkr's floored rows) and
/// [`FeeSchedule::ProbabilityScaled`] are refused by SHAPE —
/// a `Free` FX venue's maker is not free, it is charged in a currency this scalar cannot name, and
/// answering `0.0` there is exactly the "unknown fee silently became zero" failure. A genuine
/// 0-bps [`FeeSchedule::PercentMakerTaker`] is accepted and yields `Some(0.0)`, because that IS a
/// measured zero. [`FeeSchedule::PercentOfUnderlying`] (deribit) is refused because a flat fraction
/// UNDERSTATES it, and understating is the dangerous direction — it would floor the maker BELOW
/// break-even, which is the bug rather than the fix.
///
/// PURE, naive fold (no `mul_add`), no I/O.
pub fn maker_round_trip_fee(schedule: FeeSchedule) -> Option<f64> {
    let maker = flat_rate(schedule, false)?;
    Some(maker + maker)
}

/// One leg's flat fraction-of-price, or `None` when the SHAPE cannot express one — see
/// [`xemm_round_trip_fee`] for why the refusal is by shape and not by value.
fn flat_rate(schedule: FeeSchedule, taker_side: bool) -> Option<f64> {
    match schedule {
        FeeSchedule::PercentMakerTaker { maker_bps, taker_bps } => {
            Some(if taker_side { taker_bps } else { maker_bps } / 10_000.0)
        }
        FeeSchedule::PercentOfUnderlying { .. }
        | FeeSchedule::PerShareWithFloor { .. }
        | FeeSchedule::BpsWithFloor { .. }
        | FeeSchedule::ProbabilityScaled { .. }
        | FeeSchedule::Free => None,
    }
}

#[path = "fees_tests.rs"]
#[cfg(test)]
mod fees_tests;
