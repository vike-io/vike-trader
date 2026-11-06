//! What a CLOSING fill owes in commission, decided from what the OPENING fill already booked. Pure, no session, no FFI.
//!
//! ⚠ **WHAT FXCM SAYS** (the ForexConnect C++ reference, `IO2GTradeRow::getCommission` / `IO2GClosedTradeRow::getCommission`
//! and the "Trading Commissions" article; read from `doc/chm/ForexConnectAPICPP.chm` of the SDK, 2026-10-26):
//! the figure is "the amount of funds SUBTRACTED from the account balance", "expressed in the ACCOUNT currency", and
//! `0.0` when none has been charged. So a positive number is a charge, as `FillEvent::commission` is; the currency is the
//! account's (EUR on the demo account 9), never the pair's. A schedule pays at one of three stages
//! (`O2GCommissionStage`): all at the OPEN, all at the CLOSE, or half at each ("any deal").
//!
//! ⚠ **THE SIGN IS NOT TRUSTED** ([`charge`]): the vendor's word was never measured, so every figure books as its magnitude,
//! a cost. A venue that wrote a charge as a negative number would otherwise CREDIT the account every fee (the core nets a
//! negative commission in as a rebate); the price of this rule is that a true rebate on a trade row, which the vendor does not
//! describe, would book as a cost.
//!
//! ⚠ **MEASURED**: the demo account (EUR, id 9) charges NO commission. 20 market fills of at most 2000 units on 2026-10-26
//! (9 opens, 11 closes) read `0.00` on every Trades and every ClosedTrades row, and the balance moved by exactly
//! `GrossPL` (50000.00 -> 49999.08; the cost was the spread). So the sign, the stage and what a closed row's figure
//! contains are the vendor's words, NOT a measurement, until a commission-bearing account is read.
//!
//! **WHAT THE BRIDGE BOOKED BEFORE THIS.** Only the OPEN row's figure (the `Trades` insert); the closing fill carried
//! `0.0` because whether the closed row's figure already contains the open one was unknown. So an account paying at the
//! CLOSE was booked ZERO, and an "any deal" one HALF.
//!
//! **THE RULE.** The shim now sends the closed row's own figure on the closing fill's envelope, and [`OpenCommissions`]
//! books `closed row - the open fill's share of it`:
//!
//! * the open fill's commission is remembered per trade id, with its amount;
//! * the open share of a closed (part of a) trade is `open commission * closed amount / open amount`;
//! * what the close books is `max(0, closed figure - open share)`, rounded to the wire's six decimals.
//!
//! If the closed figure is the position's TOTAL (open + close), that is exactly the close side, at every stage. If it is
//! only the close side, the rule books it at the "close" and "open" stages and `0` at "any deal" (the open share eats it),
//! which is what was booked before: **it can understate, it can never charge a fee twice**. Both readings are one measurement
//! away on a commission-bearing account; `docs/ops/fxcm-forexconnect.md`'s "Commission" section lists it.
//!
//! **WHAT IT DOES NOT KNOW** (said at `warn!` with the figure, never booked from a guess): a closed trade whose open fill this
//! process did not see (a restart between the open and the close; the open fill's envelope came from the reconcile pass), and
//! the REMAINDER of a partly closed trade, which FXCM re-ids (the shim does not say the new id). Their close side books `0`.

use std::collections::{HashMap, VecDeque};

use serde_json::Value;

/// The mark `fcshim.cpp`'s `EventListener` puts between a closed trade's id and its closing order's id:
/// `<trade id>-close-<closing order id>` (the shim source test pins the literal).
const CLOSE_MARK: &str = "-close-";

/// How many open fills the session remembers. A row leaves only by this cap (the OLDEST goes); the figure it answers is
/// needed for as long as the trade stays open, and a trade older than this many later fills books its close side as unknown.
const MAX_OPENS: usize = 10_000;

/// The wire's precision is six decimals (`%.6f`): a difference under half of the last one is rounding, not money.
const WIRE_SCALE: f64 = 1e6;

/// The commission and amount the opening fill of each trade carried, bounded FIFO.
#[derive(Default)]
pub(crate) struct OpenCommissions {
    opens: HashMap<String, (f64, f64)>,
    inserted: VecDeque<String>,
}

impl OpenCommissions {
    /// Take one decoded shim envelope: an OPENING fill is remembered, a CLOSING fill's `commission` is replaced by what the
    /// close owes; anything else is left alone. Idempotent for a replay (a reconnect re-surfaces the same rows: the same open
    /// is remembered again, the same close gets the same answer, and the core drops the repeated trade id anyway).
    pub(crate) fn settle(&mut self, v: &mut Value) {
        if v.get("kind").and_then(Value::as_str) != Some("fill") {
            return;
        }
        let number =
            |k: &str| v.get(k).and_then(Value::as_f64).filter(|x| x.is_finite()).unwrap_or(0.0);
        let (amount, raw) = (number("amount"), number("commission"));
        let commission = charge(raw);
        let Some(trade) = v.get("trade_id").and_then(Value::as_str).map(str::to_owned) else {
            return;
        };
        if raw < 0.0 {
            tracing::warn!(
                target: "vike_fxcm",
                venue = "fxcm",
                trade_id = %trade,
                commission = raw,
                "fxcm: a fill carries a NEGATIVE commission; the vendor documents a charge as positive and no account that \
                 charges has been read, so the figure is booked as a charge of the same size, never as a rebate"
            );
        }
        match trade.split_once(CLOSE_MARK) {
            None => {
                self.remember(trade, amount, commission);
                if commission != raw {
                    v["commission"] = Value::from(commission);
                }
            }
            Some((opened, _)) => {
                let owed = self.close_side(opened, amount, commission);
                v["commission"] = Value::from(owed);
            }
        }
    }

    fn remember(&mut self, trade: String, amount: f64, commission: f64) {
        if self.opens.insert(trade.clone(), (amount, commission)).is_none() {
            self.inserted.push_back(trade);
        }
        while self.inserted.len() > MAX_OPENS {
            if let Some(oldest) = self.inserted.pop_front() {
                self.opens.remove(&oldest);
            }
        }
    }

    fn close_side(&self, opened: &str, closed_amount: f64, closed_figure: f64) -> f64 {
        if closed_figure <= 0.0 {
            return 0.0; // nothing was charged (the demo, and a shim that still sends 0.0): nothing to look up
        }
        let open = self.opens.get(opened).copied();
        let owed = owed_at_close(closed_figure, open, closed_amount);
        if open.is_none() {
            tracing::warn!(
                target: "vike_fxcm",
                venue = "fxcm",
                trade_id = %opened,
                closed_commission = closed_figure,
                "fxcm: a closed trade carries a commission and this process never saw its opening fill (a restart, the \
                 remainder of a part-closed trade, or a fill the reconcile pass booked), so its close side is NOT booked; \
                 the venue's balance has it"
            );
        }
        owed
    }
}

/// What a venue commission figure books as: a cost of its own size, never a rebate. The sign is the vendor's word and was
/// never measured (the demo charges nothing), so it is not trusted: if FXCM wrote a charge as a negative number, passing
/// it through would credit the account the fee, which is worse than any other mistake here. FXCM pays no rebate on a trade
/// row (a rebate is another figure, "added to the account balance"), so a magnitude cannot hide one. Applied to every
/// figure that enters the books: the exec drain's ([`OpenCommissions::settle`]) and the reconcile report's.
#[must_use]
pub(crate) fn charge(raw: f64) -> f64 {
    raw.abs()
}

/// The commission a close owes: `closed_figure` less the open fill's share of it (`open` is the opening fill's
/// `(amount, commission)`), never below zero, in the wire's precision. No opening fill known: `0.0`, never the whole figure
/// (the open side may already be booked, and a fee charged twice is worse than one left out and said).
#[must_use]
pub(crate) fn owed_at_close(
    closed_figure: f64,
    open: Option<(f64, f64)>,
    closed_amount: f64,
) -> f64 {
    let Some((open_amount, open_commission)) = open else { return 0.0 };
    if !closed_figure.is_finite() || closed_figure <= 0.0 || open_amount <= 0.0 {
        return 0.0;
    }
    let open_share = open_commission * (closed_amount / open_amount).clamp(0.0, 1.0);
    let owed = closed_figure - open_share;
    let owed = (owed * WIRE_SCALE).round() / WIRE_SCALE;
    if owed > 0.0 { owed } else { 0.0 }
}

#[path = "commission_tests.rs"]
#[cfg(test)]
mod commission_tests;
