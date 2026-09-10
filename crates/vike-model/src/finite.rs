//! Finiteness of venue-sourced numbers — the inbound twin of the RiskGate's outbound NaN discipline.
//!
//! ## Why this exists
//!
//! Every venue REST/WS payload carries numbers as decimal STRINGS (`"0.00100000"`), decoded in ONE
//! place: `vike_bridge_core::json::json_num`'s `Value::String(s) => s.parse::<f64>().ok()` arm.
//! **Rust's `f64::from_str` accepts `"NaN"`, `"inf"`, `"-inf"`, `"infinity"` and their sign/case
//! variants** — it is a total parser over the IEEE-754 *value* space, not over the decimal literals
//! a venue is supposed to send. So a compromised (or merely broken) venue socket can put a
//! non-finite f64 into a `FillEvent` through the ordinary, fully-typed mapper path. Nothing about
//! that is a parse error, and nothing downstream re-checks.
//!
//! One such value is UNRECOVERABLE, not merely wrong. `vike_exec::Account::fold` runs
//! [`crate::compute_fill`], whose `new_avg_px` becomes NaN and is STORED on the position; every
//! later fill on that key recomputes from the poisoned basis, `realized_pnl` accumulates NaN, and
//! `equity_all` — a sparse full recompute with no cache to invalidate — returns NaN for the rest of
//! the session. `balance` goes the same way through a NaN `commission`. There is no fold that
//! removes a NaN once one is in the ledger, and reconcile cannot repair it either: `recon::diff`
//! compares local numbers to venue numbers, and every comparison against NaN is false.
//!
//! ## The rule
//!
//! **A venue-sourced number that is not finite is not data.** The hazard is already understood on
//! the way OUT — `vike_exec::risk::RiskGate::check_inner`'s price collar tests `!p.is_finite()`
//! explicitly, because `NaN > band` is false and a bare comparison would wave the order through.
//! This module is the same discipline applied on the way IN, at the fold boundary.
//!
//! [`FiniteNumbers`] is deliberately a PURE predicate over the domain types and holds no policy:
//! what a rejected event should DO (drop / count / halt) belongs to the folding engine, which owns
//! the counters and the log. `vike_exec::ExecutionEngine::on_event` is that policy site today.
//!
//! ## What each impl covers, and what it deliberately does not
//!
//! Only the f64 fields that REACH ACCOUNT STATE are checked. A field the fold never reads cannot
//! poison anything, and discarding a money event because a decorative field was malformed would
//! turn a cosmetic venue bug into a lost fill — strictly worse than the disease.
//!
//! - [`FillEvent`] — `last_qty` / `last_px` / `commission`. **`mark_price` is deliberately
//!   EXCLUDED**: it feeds the mark slot, not the position fold, and it is guarded at
//!   `vike_exec::Account::set_mark_from` — THE single writer of that slot, which therefore covers
//!   the market-data and reconcile mark writers in the same stroke. A fill must not be discarded
//!   because the mark rode along malformed.
//! - [`FundingEvent`] — `amount` only. `funding_rate` is carried, never folded (`apply_funding`
//!   touches `balance`/`funding_paid` from `amount` alone).
//! - [`AccountState`] — every `balances` qty, because ALL THREE selection paths in
//!   `apply_account_state` can reach any of them (the quote-asset match, the single-balance case,
//!   and the `py_sum` last resort — where one NaN poisons the whole sum).
//! - [`PositionLiquidated`] — `qty` / `liq_price` / `fee`. ⚠ `qty` looks self-defending
//!   (`f64::min` returns its non-NaN operand, so a NaN qty clamps to the held size) but `liq_price`
//!   flows straight into [`crate::compute_fill`] and `fee` straight into `balance`.
//!
//! `ts` fields are `i64` and `side` is `i32`, so neither is reachable by this class of bug.

use crate::events::{AccountState, FillEvent, FundingEvent, PositionLiquidated};

/// A venue-sourced event whose state-reaching f64 fields are all finite (not NaN, not ±∞).
///
/// See the module doc for why this is checked at all and for the per-type field set. Implementors
/// answer ONLY the question in the name — the decision to drop, count or halt is the caller's.
pub trait FiniteNumbers {
    /// `true` when every f64 this event folds into account state is finite.
    ///
    /// Cost: one `is_finite` per checked field — on x86-64 an `andps`/`ucomisd` pair per field with
    /// no branch misprediction pressure and no memory traffic beyond the already-loaded event. The
    /// fill lane checks three fields.
    fn numbers_finite(&self) -> bool;
}

impl FiniteNumbers for FillEvent {
    fn numbers_finite(&self) -> bool {
        // `mark_price` is EXCLUDED on purpose — see the module doc ("What each impl covers").
        self.last_qty.is_finite() && self.last_px.is_finite() && self.commission.is_finite()
    }
}

impl FiniteNumbers for FundingEvent {
    fn numbers_finite(&self) -> bool {
        // `funding_rate` is carried but never folded; `apply_funding` reads `amount` alone.
        self.amount.is_finite()
    }
}

impl FiniteNumbers for AccountState {
    fn numbers_finite(&self) -> bool {
        // EVERY qty: the `py_sum` last-resort path folds all of them, and one NaN poisons the sum.
        self.balances.iter().all(|(_asset, qty)| qty.is_finite())
    }
}

impl FiniteNumbers for PositionLiquidated {
    fn numbers_finite(&self) -> bool {
        self.qty.is_finite() && self.liq_price.is_finite() && self.fee.is_finite()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::PositionSide;
    use ustr::ustr;

    /// The premise the whole module rests on: `f64::from_str` — which `json_num` calls on every
    /// string-encoded venue number — ACCEPTS the non-finite spellings. If this ever stops being
    /// true the guards below become dead code, and this test is where that is noticed.
    #[test]
    fn rust_float_parsing_accepts_the_non_finite_spellings() {
        for s in ["NaN", "nan", "NAN", "inf", "-inf", "Inf", "infinity", "-Infinity", "+inf"] {
            let parsed = s.parse::<f64>();
            assert!(parsed.is_ok(), "{s:?} must parse (that is the hazard), got {parsed:?}");
            assert!(
                !parsed.unwrap().is_finite(),
                "{s:?} must parse to a NON-finite f64 — the value this module exists to reject"
            );
        }
        // ...and the control: an ordinary venue decimal string is finite and must never be rejected.
        for s in ["0", "0.00100000", "-12345.6789", "1e-9"] {
            assert!(s.parse::<f64>().unwrap().is_finite(), "{s:?} is ordinary venue data");
        }
    }

    fn fill() -> FillEvent {
        FillEvent {
            trade_id: "t1".into(),
            client_order_id: "c1".to_string(),
            venue: ustr("sim"),
            symbol: ustr("BTCUSDT"),
            side: 1,
            last_qty: 1.0,
            last_px: 100.0,
            commission: 0.1,
            commission_asset: ustr("USDT"),
            liquidity_side: Default::default(),
            ts: 0,
            mark_price: None,
            position_side: PositionSide::Both,
        }
    }

    #[test]
    fn an_ordinary_fill_is_finite() {
        assert!(fill().numbers_finite());
    }

    #[test]
    fn every_folded_fill_field_is_checked() {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(!FillEvent { last_qty: bad, ..fill() }.numbers_finite(), "last_qty {bad}");
            assert!(!FillEvent { last_px: bad, ..fill() }.numbers_finite(), "last_px {bad}");
            assert!(!FillEvent { commission: bad, ..fill() }.numbers_finite(), "commission {bad}");
        }
    }

    /// The documented EXCLUSION, pinned so it cannot be "fixed" by accident: a malformed decorative
    /// mark must not discard a money event. `Account::set_mark_from` rejects it instead.
    #[test]
    fn a_non_finite_mark_price_does_not_reject_the_fill() {
        assert!(FillEvent { mark_price: Some(f64::NAN), ..fill() }.numbers_finite());
        assert!(FillEvent { mark_price: Some(f64::INFINITY), ..fill() }.numbers_finite());
    }

    #[test]
    fn funding_checks_the_folded_amount_and_not_the_carried_rate() {
        let ev = FundingEvent {
            venue: ustr("sim"),
            symbol: ustr("BTCUSDT"),
            position_side: PositionSide::Both,
            funding_rate: 0.0001,
            amount: -1.25,
            mark_price: None,
            ts: 0,
            route_key: None,
        };
        assert!(ev.numbers_finite());
        assert!(!FundingEvent { amount: f64::NAN, ..ev.clone() }.numbers_finite());
        // `funding_rate` is never folded, so it does not reject the cashflow.
        assert!(FundingEvent { funding_rate: f64::NAN, ..ev }.numbers_finite());
    }

    #[test]
    fn account_state_checks_every_balance_because_py_sum_folds_them_all() {
        let ev = AccountState {
            venue: ustr("sim"),
            balances: vec![("USDT".to_string(), 100.0), ("BTC".to_string(), 0.5)],
            ts: 0,
            route_key: None,
        };
        assert!(ev.numbers_finite());
        // the SECOND entry — the one a "check the first balance" shortcut would miss
        let poisoned = AccountState {
            balances: vec![("USDT".to_string(), 100.0), ("BTC".to_string(), f64::NAN)],
            ..ev.clone()
        };
        assert!(!poisoned.numbers_finite());
        // an empty frame is vacuously finite (`apply_account_state` early-returns on it anyway)
        assert!(AccountState { balances: vec![], ..ev }.numbers_finite());
    }

    #[test]
    fn liquidation_checks_qty_price_and_fee() {
        let ev = PositionLiquidated {
            venue: ustr("sim"),
            symbol: ustr("BTCUSDT"),
            position_side: PositionSide::Both,
            qty: 1.0,
            liq_price: 90.0,
            fee: 0.2,
            ts: 0,
            trade_id: "l1".into(),
            route_key: None,
        };
        assert!(ev.numbers_finite());
        assert!(!PositionLiquidated { qty: f64::NAN, ..ev.clone() }.numbers_finite());
        assert!(!PositionLiquidated { liq_price: f64::NAN, ..ev.clone() }.numbers_finite());
        assert!(!PositionLiquidated { fee: f64::INFINITY, ..ev }.numbers_finite());
    }
}
