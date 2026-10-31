//! The rollover interest a CLOSED trade realized, booked as funding (A26; FEES-AUDIT 2026-10-26 section 4.7).
//!
//! Until now the bridge booked none of it: a trade held across the 17:00 New York rollover earned or paid interest
//! the venue put on the balance, and the engine's `funding_paid` and cash never saw it.
//!
//! # What is booked, and where
//!
//! The shim's closing-fill envelope (`fcshim.cpp`'s `EventListener`, a ClosedTrades Insert) carries the closed
//! row's own `getRolloverInterest()` as the `rollover` key (the opening fill's is the literal 0.0). The exec drain
//! hands every envelope here after the order-event decode (`crate::exec`'s `fold_with_rollover`): a CLOSING fill
//! (`<trade id>-close-<closing order id>`, `crate::commission`) with a non-zero `rollover` becomes ONE
//! [`FundingEvent`] under the canonical symbol, in the account's currency, once per closing fill id
//! (`RolloverBooked`: a ForexConnect reconnect re-delivers table rows, `apply_funding` is not idempotent and a
//! funding event has no id). It is the realized rollover of the CLOSED amount, booked at the close; the running
//! figure on an open trade (`IO2GTradeRow::getRolloverInterest`) is not read, so the engine's cash does not move
//! by the interest during a hold and a reconcile past its tolerance re-seeds it. A trade whose close this process
//! did not see (a restart between the open and the close, a close made in the platform) books nothing: the first
//! reconcile adopted the balance that already holds it. Dividends (`getDividends`, CFD equities) are not read.
//!
//! # The currency
//!
//! "Expressed in the account currency" (the SDK reference), which the exec thread reads once from its own
//! session's login-time system properties (`BASE_CRNCY`, `stated_account_ccy`: a STATED currency only, never the
//! assumed USD of [`crate::home_conversion::account_ccy`]). Without it the event is booked unstated, which an
//! account-quoted engine takes as stated and a foreign-quoted one (the EUR demo on its dollar-quoted mount) leaves
//! out and counts.
//!
//! # The sign is documented and not measured
//!
//! The reference says the closed row's figure is "the cumulative amount of funds ADDED to the account balance for
//! holding a position opened overnight ... positive or negative". [`ROLLOVER_BALANCE_SIGN`] reads that as the
//! signed balance change, a named, pinned decision. It is the vendor's word and not a measurement: the only account
//! read (the EUR demo) has never held a trade overnight, so every row read `0.00`. The first booking of a run
//! says so at `warn!`, as does every power of ten after it, and the operator kit's t26 FXCM script reads the venue.

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;
use vike_model::events::{Event, FundingEvent, PositionSide};

use crate::event_mapper::from_fxcm_instrument;
use crate::home_conversion::BASE_CURRENCY_PROPERTY;

const VENUE: &str = "fxcm";

/// The mark between a closed trade's id and its closing order's id on a closing fill (`<trade id>-close-<order id>`).
const CLOSE_MARK: &str = "-close-";

/// How `rollover` (the closed row's `getRolloverInterest()`) becomes `FundingEvent::amount`: multiplied by this.
/// `1.0` = the figure IS the signed balance change (the SDK: "funds added to the account balance", positive or
/// negative). The vendor's word, not a measurement (module doc); `the_rollover_sign_is_pinned` holds the value.
pub const ROLLOVER_BALANCE_SIGN: f64 = 1.0;

/// How many closing-fill ids [`RolloverBooked`] remembers. A closing fill with a rollover is a trade held
/// overnight, so this is a leak guard; the oldest id leaves first.
const SEEN_CAP: usize = 4096;

/// Rollover figures booked while their sign was unmeasured, this run.
static UNMEASURED_BOOKED: AtomicU64 = AtomicU64::new(0);

/// Count one booking and say so on the 1st, 10th, 100th, ... one.
fn note_unmeasured_booking() {
    let n = UNMEASURED_BOOKED.fetch_add(1, Ordering::Relaxed) + 1;
    if n == 1 || (n >= 10 && 10u64.pow(n.ilog10()) == n) {
        tracing::warn!(
            target: "vike_fxcm",
            venue = VENUE,
            booked = n,
            "fxcm: booked a rollover interest on a sign the vendor documents (funds added to the balance) and \
             nobody measured: confirm it with the operator kit's t26 FXCM script against the balance"
        );
    }
}

/// The tests' read of the counter above.
#[cfg(test)]
pub(crate) fn unmeasured_bookings() -> u64 {
    UNMEASURED_BOOKED.load(Ordering::Relaxed)
}

/// The funding event a CLOSING fill envelope implies, stateless: `None` for any envelope that is not a fill, a fill
/// without the close mark in its trade id (an opening one), an unreadable or zero `rollover`, or no instrument.
/// Total over arbitrary JSON. `ccy` is the account's currency when known.
///
/// `pub` so the grammar gate (`crates/bridges/fxcm/tests/fxcm_shim_envelope_grammar.rs`) can ablate the `rollover`
/// key through the seam that reads it; production books through `RolloverBooked::events`.
#[must_use]
pub fn rollover_funding(v: &Value, ccy: Option<&str>) -> Option<FundingEvent> {
    if v.get("kind").and_then(Value::as_str) != Some("fill") {
        return None;
    }
    let trade_id = v.get("trade_id").and_then(Value::as_str)?;
    if !trade_id.contains(CLOSE_MARK) {
        return None;
    }
    let rollover =
        v.get("rollover").and_then(Value::as_f64).filter(|x| x.is_finite() && *x != 0.0)?;
    let instrument = v.get("instrument").and_then(Value::as_str).filter(|s| !s.is_empty())?;
    Some(FundingEvent {
        venue: VENUE.into(),
        symbol: from_fxcm_instrument(instrument).into(),
        position_side: PositionSide::Both,
        funding_rate: 0.0,
        amount: rollover * ROLLOVER_BALANCE_SIGN,
        mark_price: None,
        ts: v.get("ts").and_then(Value::as_i64).unwrap_or(0),
        route_key: None,
        ccy: ccy.map(Into::into),
    })
}

/// What the closed row's OWN figures say about [`ROLLOVER_BALANCE_SIGN`]: the shim sends the row's `gross_pl` and
/// `net_pl` beside its `rollover` and `commission`, and a row whose net is its gross plus the rollover (as the sign
/// reads it) less the commission has confirmed the sign on the venue's own arithmetic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignCheck {
    /// `net = gross + rollover - commission`: a positive rollover IS funds added.
    Confirmed,
    /// `net = gross - rollover - commission`: the sign is the other way round. The booking is WRONG.
    Contradicted,
    /// Both figures are there and neither identity holds (dividends, another charge, a rounding).
    Unexplained,
    /// Nothing to check: not a closing fill, no rollover, or an older shim that sends no `gross_pl` / `net_pl`.
    Nothing,
}

/// The money on the row is stated to the cent.
const CHECK_TOL: f64 = 0.0051;

/// [`SignCheck`] of one drained envelope (total over arbitrary JSON).
#[must_use]
pub fn rollover_sign_check(v: &Value) -> SignCheck {
    let Some(booked) = rollover_funding(v, None) else { return SignCheck::Nothing };
    let num = |k: &str| v.get(k).and_then(Value::as_f64).filter(|x| x.is_finite());
    let (Some(gross), Some(net)) = (num("gross_pl"), num("net_pl")) else {
        return SignCheck::Nothing;
    };
    let commission = num("commission").unwrap_or(0.0);
    let raw = booked.amount / ROLLOVER_BALANCE_SIGN; // the row's own figure, whatever sign the booking gives it
    // The sign the row's arithmetic implies for the raw figure: +1 = added to the net, -1 = taken from it.
    let implied = if (net - (gross + raw - commission)).abs() <= CHECK_TOL {
        Some(1.0)
    } else if (net - (gross - raw - commission)).abs() <= CHECK_TOL && raw.abs() > 2.0 * CHECK_TOL {
        Some(-1.0)
    } else {
        None
    };
    match implied {
        Some(sign) if sign == ROLLOVER_BALANCE_SIGN => SignCheck::Confirmed,
        Some(_) => SignCheck::Contradicted,
        None => SignCheck::Unexplained,
    }
}

/// The account currency a session's login-time system properties STATE (`BASE_CRNCY`, a three-letter code), upper
/// case; `None` when the body is not a JSON object, the property is absent or it is not a currency code. Never
/// the assumed USD: an unstated currency stays unstated here.
#[must_use]
pub(crate) fn stated_account_ccy(system_properties_json: &str) -> Option<String> {
    let value: Value = serde_json::from_str(system_properties_json).ok()?;
    let code = value.get(BASE_CURRENCY_PROPERTY)?.as_str()?.trim();
    (code.len() == 3 && code.chars().all(|c| c.is_ascii_alphabetic())).then(|| code.to_uppercase())
}

/// The exec thread's booking state: which closing fills it already booked, and the account's currency once read.
#[derive(Default)]
pub(crate) struct RolloverBooked {
    seen: HashSet<String>,
    order: VecDeque<String>,
    /// The account's STATED currency once the session answered ([`stated_account_ccy`]).
    pub(crate) ccy: Option<String>,
}

impl RolloverBooked {
    /// Whether `v` is a closing fill that has a rollover to book and has not been booked yet while the currency is
    /// still unknown: the drain reads the session's system properties only then.
    pub(crate) fn wants_ccy(&self, v: &Value) -> bool {
        self.ccy.is_none() && rollover_funding(v, None).is_some() && !self.is_seen(v)
    }

    fn is_seen(&self, v: &Value) -> bool {
        v.get("trade_id").and_then(Value::as_str).is_some_and(|id| self.seen.contains(id))
    }

    /// The events of one drained envelope: its closing fill's rollover as funding, at most once per trade id.
    pub(crate) fn events(&mut self, v: &Value) -> Vec<Event> {
        let Some(funding) = rollover_funding(v, self.ccy.as_deref()) else { return Vec::new() };
        let Some(id) = v.get("trade_id").and_then(Value::as_str) else { return Vec::new() };
        if self.seen.contains(id) {
            return Vec::new();
        }
        if self.order.len() >= SEEN_CAP
            && let Some(oldest) = self.order.pop_front()
        {
            self.seen.remove(&oldest);
        }
        self.seen.insert(id.to_string());
        self.order.push_back(id.to_string());
        match rollover_sign_check(v) {
            SignCheck::Confirmed => tracing::info!(
                target: "vike_fxcm",
                venue = VENUE,
                amount = funding.amount,
                "fxcm: the closed row's own net P/L confirms the rollover sign (net = gross + rollover - commission); \
                 booked a rollover interest"
            ),
            SignCheck::Contradicted => tracing::warn!(
                target: "vike_fxcm",
                venue = VENUE,
                amount = funding.amount,
                "fxcm: the closed row's own net P/L CONTRADICTS the rollover sign (net = gross - rollover - commission): \
                 the booked rollover interest has the WRONG sign; flip ROLLOVER_BALANCE_SIGN"
            ),
            SignCheck::Unexplained | SignCheck::Nothing => note_unmeasured_booking(),
        }
        vec![Event::Funding(funding)]
    }
}

#[path = "rollover_tests.rs"]
#[cfg(test)]
mod rollover_tests;
