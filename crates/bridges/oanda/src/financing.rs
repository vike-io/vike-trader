//! The cash OANDA moves that is not a fill's price: overnight financing and the guaranteed-stop fee.
//!
//! Until now the bridge booked none of it (FEES-AUDIT 2026-10-26 section 4.7): a position held across the
//! 17:00 New York rollover moved the venue's balance by `DAILY_FINANCING` and the engine's by nothing, so
//! `funding_paid` read 0 and the cash drifted until a reconcile past `BalanceTol` re-seeded it.
//!
//! # What is booked, and where
//!
//! * **`DAILY_FINANCING`** (a transaction of its own, one per rollover): one [`FundingEvent`] per entry of
//!   `positionFinancings` (`instrument`, `financing`), under the CANONICAL symbol ([`canonical_symbol`], the
//!   six-letter spelling every mount and admission uses), so the engine that holds the pair folds it and one
//!   that does not drops it (a position the web platform opened is the venue's, not this ledger's).
//! * **An `ORDER_FILL`'s own `financing`** (the financing settled when the order filled): one event for the
//!   fill's instrument. Distinct cash from the `DAILY_FINANCING` rows: each transaction states the account
//!   balance after it, and the venue debits each once.
//! * **`guaranteedExecutionFee`** is NOT a funding event: it rides on the same fill as `commission` and is
//!   added to the fill's commission ([`fill_charges`], read by the stream, the order-POST mapper and the
//!   reconcile parser alike), so it books once under the fill's trade id with the fill's currency rule.
//! * **`plHomeConversionCost`** (an `ORDER_FILL` that closes a position in a pair quoted in another currency
//!   than the account's) rides on the same fill's commission too, and for the same reason. OANDA converts the
//!   quote-currency result of a close with `gainQuoteHomeConversionFactor` (a gain) or
//!   `lossQuoteHomeConversionFactor` (a loss), which sit exactly one percent either side of the mid on the
//!   practice account, and states the cost of that conversion in the account's currency on the fill itself:
//!   the venue's `pl` and the balance it moves are the unmarked conversion PLUS this figure. The engine books
//!   a close at the mid (decision 0183), so without it the engine's cash ran about one percent of every such
//!   PnL ahead of the venue's balance, a gap that grows with every close (measured: 0.67 USD after four
//!   round trips of 20000 units). The figure is the venue's own, never inferred from the factors, and it is
//!   booked by MAGNITUDE: every captured close was a loss and stated it negative, the field is in no public
//!   reference, and the sign for a gain is unmeasured (the operator kit's n09 script reads it). A fill that
//!   states none (a pair quoted in the account's currency states zero, an older body none) books no
//!   conversion cost, which is the decision-0183 booking unchanged.
//!
//! # The currency
//!
//! Every figure is in the account's home currency. [`FundingEvent::ccy`] states it when the lane could read
//! it (`GET /summary`, `account.currency`, asked lazily at the first event and again until it answers):
//! without it an engine that holds a foreign-quoted symbol (a EUR account on the dollar-quoted mount) leaves
//! the event out and counts it, which would make this whole module a no-op on exactly that account.
//!
//! # Only the stream lane books it, and only once
//!
//! The transactions stream is account-wide, so it carries every `DAILY_FINANCING` and every fill, whoever
//! placed it. [`FinancingLane`] is owned by the stream thread (the live line AND the sinceid backfill after a
//! reconnect pass through it) and remembers the transaction ids it booked ([`SEEN_CAP`]): `apply_funding`
//! is not idempotent and a funding event has no id, so the reconnect overlap would otherwise book a
//! rollover twice. A rollover that happened while the node was DOWN is never replayed (the stream carries
//! only transactions from connect time, and the first reconcile adopts the venue's balance that already
//! holds it), the same restart law as `crates/bridges/okx/src/funding.rs`.
//!
//! # The sign is not measured
//!
//! OANDA's reference says `financing` is "the amount of financing paid/collected" and the fill's
//! `commission` is "always a positive quantity ... however it reduces the balance". It never states which
//! way `financing` points, and the only account measured (practice, no position ever held across a
//! rollover) has no `DAILY_FINANCING` row. [`FINANCING_BALANCE_SIGN`] is therefore a DECISION, named and
//! pinned: `financing` is the signed balance change (a collected amount positive, a paid one negative,
//! `FundingEvent::amount`'s own convention). While nothing measured it, the first booking of a run says so
//! at `warn!` and the 10th, 100th, ... say how many unmeasured figures were booked. The read-only confirmation against the real venue is the operator kit's
//! `t26-oanda-financing-sign.sh` (outside this repository): it compares `accountBalance` before and after
//! every `DAILY_FINANCING` row, and every fill that states a `financing`, with that row's `financing`.

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;
use vike_model::events::{Event, FundingEvent, PositionSide};

use crate::config::OandaConfig;
use crate::rest::OandaRest;

const VENUE: &str = "oanda";

/// How `financing` (a `DAILY_FINANCING` row, a position entry, an `ORDER_FILL`) becomes
/// `FundingEvent::amount`: multiplied by this. `1.0` = the figure IS the signed balance change.
/// UNMEASURED (module doc); flipping it flips every booked rollover, so a change is a decision with a
/// measurement behind it, and `the_financing_sign_is_pinned` holds the value.
pub(crate) const FINANCING_BALANCE_SIGN: f64 = 1.0;

/// How `guaranteedExecutionFee` becomes part of a fill's `commission` (`> 0` charge): multiplied by this.
/// `1.0` = a positive figure is a cost, as the venue states the fill's `commission`. UNMEASURED: no
/// practice account has a guaranteed stop.
pub(crate) const GUARANTEED_FEE_COST_SIGN: f64 = 1.0;

/// How many booked transaction ids the lane remembers. A rollover is one transaction a day and a fill with
/// financing a rarer one, so this is a leak guard; the oldest id leaves first.
const SEEN_CAP: usize = 4096;

/// Financing figures booked while their sign was unmeasured, this run.
static UNMEASURED_BOOKED: AtomicU64 = AtomicU64::new(0);

/// How many figures this run booked on the unmeasured sign (module doc; the warning carries the count,
/// this is the tests' read of it).
#[cfg(test)]
pub(crate) fn unmeasured_bookings() -> u64 {
    UNMEASURED_BOOKED.load(Ordering::Relaxed)
}

/// Count one unmeasured booking and say so on the 1st, 10th, 100th, ... one.
fn note_unmeasured_booking(what: &str) {
    let n = UNMEASURED_BOOKED.fetch_add(1, Ordering::Relaxed) + 1;
    if is_log_point(n) {
        tracing::warn!(
            venue = VENUE,
            what,
            booked = n,
            "oanda: booked a {what} on an UNMEASURED sign (`financing` is read as the signed balance change, \
             negative = paid): confirm it with the kit's t26 script against the venue's accountBalance"
        );
    }
}

/// `n` is 1 or a power of ten: the occurrences worth a line.
fn is_log_point(n: u64) -> bool {
    n == 1 || (n >= 10 && 10u64.pow(n.ilog10()) == n)
}

/// A field as a finite number: OANDA sends every scalar as a string, a fixture may send a JSON number.
fn num(v: &Value, key: &str) -> Option<f64> {
    let field = v.get(key)?;
    field
        .as_str()
        .and_then(|s| s.trim().parse::<f64>().ok())
        .or_else(|| field.as_f64())
        .filter(|x| x.is_finite())
}

/// The transaction's `time` (epoch seconds, a string) in epoch milliseconds, `0` when absent.
fn ts_ms(v: &Value) -> i64 {
    v.get("time")
        .and_then(Value::as_str)
        .and_then(|t| t.parse::<f64>().ok())
        .map_or(0, |secs| (secs * 1000.0) as i64)
}

/// The canonical vike symbol of a venue instrument: `EUR_USD` -> `EURUSD` (two three-letter halves, the
/// spelling every mount, admission and reconcile row uses); any other name (`XAU_USD`, `SPX500_USD`) keeps
/// the venue's spelling, which is what the mount of such an instrument would carry.
fn canonical_symbol(instrument: &str) -> String {
    match instrument.split_once('_') {
        Some((base, quote)) if base.len() == 3 && quote.len() == 3 => format!("{base}{quote}"),
        _ => instrument.to_string(),
    }
}

/// What the fill's own transaction charges besides the price, for `FillEvent::commission` (`> 0` charge):
/// `commission`, plus `guaranteedExecutionFee`, plus the home-conversion cost of the fill's `pl`
/// (module doc). All absent or unreadable = `0.0`.
pub(crate) fn fill_charges(tx: &Value) -> f64 {
    let commission = num(tx, "commission").unwrap_or(0.0);
    let commission = match num(tx, "guaranteedExecutionFee").filter(|fee| *fee != 0.0) {
        Some(fee) => {
            note_unmeasured_booking("guaranteedExecutionFee");
            commission + fee * GUARANTEED_FEE_COST_SIGN
        }
        None => commission,
    };
    // The MAGNITUDE, whatever sign the venue states: a conversion mark-up is a cost either way. Measured
    // negative (`pl` = the unmarked conversion + this) on every one of 15 closes, all of them losses; the
    // field is in no public reference and a close at a gain has never been captured, so the sign for a gain
    // is not read from it (a positive figure there would otherwise CREDIT the cost).
    match num(tx, "plHomeConversionCost").filter(|cost| *cost != 0.0) {
        Some(cost) => commission + cost.abs(),
        None => commission,
    }
}

/// One funding event, `amount` already signed for the ledger.
fn funding(instrument: &str, amount: f64, ts: i64, ccy: Option<&str>) -> FundingEvent {
    FundingEvent {
        venue: VENUE.into(),
        symbol: canonical_symbol(instrument).into(),
        position_side: PositionSide::Both,
        funding_rate: 0.0,
        amount,
        mark_price: None,
        ts,
        route_key: None,
        ccy: ccy.map(Into::into),
    }
}

/// The funding events one transaction implies, stateless: a `DAILY_FINANCING` row's non-zero position
/// entries, an `ORDER_FILL`'s non-zero `financing`; anything else, nothing. `ccy` is the account's home
/// currency when known. A `DAILY_FINANCING` that states a non-zero total but no routable position entry is
/// returned as `Err(total)` so the caller can count it: an amount with no instrument belongs to no engine.
fn events_of(tx: &Value, ccy: Option<&str>) -> Result<Vec<FundingEvent>, f64> {
    let ts = ts_ms(tx);
    match tx.get("type").and_then(Value::as_str) {
        Some("DAILY_FINANCING") => {
            let entries = tx.get("positionFinancings").and_then(Value::as_array);
            let out: Vec<FundingEvent> = entries
                .into_iter()
                .flatten()
                .filter_map(|p| {
                    let instrument = p.get("instrument").and_then(Value::as_str)?;
                    let amount = num(p, "financing").filter(|a| *a != 0.0)?;
                    Some(funding(instrument, amount * FINANCING_BALANCE_SIGN, ts, ccy))
                })
                .collect();
            match num(tx, "financing").filter(|total| *total != 0.0) {
                Some(total) if out.is_empty() => Err(total),
                _ => Ok(out),
            }
        }
        Some("ORDER_FILL") => {
            let instrument = tx.get("instrument").and_then(Value::as_str).unwrap_or_default();
            Ok(num(tx, "financing")
                .filter(|a| *a != 0.0 && !instrument.is_empty())
                .map(|a| funding(instrument, a * FINANCING_BALANCE_SIGN, ts, ccy))
                .into_iter()
                .collect())
        }
        _ => Ok(Vec::new()),
    }
}

/// [`events_of`] as engine events, stateless: no dedup, no currency read, no unmeasured-sign accounting.
/// `pub` (re-exported `#[doc(hidden)]`) only so an integration test can fold the mapper's output through a
/// real engine; the production caller is [`FinancingLane::events`].
pub fn financing_events(tx: &Value, ccy: Option<&str>) -> Vec<Event> {
    events_of(tx, ccy).unwrap_or_default().into_iter().map(Event::Funding).collect()
}

/// The account's home currency: one bounded `GET /summary` (`account.currency`). `None` when the venue does
/// not answer or states none (said at `warn!`; the account id never reaches the line); the lane asks again
/// at its next event.
pub(crate) fn read_home_currency(rest: &OandaRest, config: &OandaConfig) -> Option<String> {
    let path = format!("/v3/accounts/{}/summary", config.account_id);
    match rest.get_lookup(&config.rest_base, &path, "") {
        Ok(summary) => {
            let currency = summary
                .pointer("/account/currency")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|c| !c.is_empty())
                .map(str::to_string);
            if currency.is_none() {
                tracing::warn!(
                    venue = VENUE,
                    "oanda: the account summary states no currency; a financing amount is booked \
                     without one (an engine holding a foreign-quoted symbol will leave it out)"
                );
            }
            currency
        }
        Err(e) => {
            tracing::warn!(
                venue = VENUE, error = %e,
                "oanda: could not read the account's currency for a financing amount; it is booked \
                 without one (an engine holding a foreign-quoted symbol will leave it out)"
            );
            None
        }
    }
}

/// The stream thread's booking state: which transactions it already booked and the account's currency.
#[derive(Default)]
pub(crate) struct FinancingLane {
    seen: HashSet<String>,
    order: VecDeque<String>,
    /// The account's home currency once `GET /summary` answered.
    home: Option<String>,
}

impl FinancingLane {
    /// The events of one transaction line (live or backfilled), each transaction booked at most once.
    /// `read_home` asks the venue for the account's currency and runs only when there is something to book
    /// and the currency is still unknown.
    pub(crate) fn events(
        &mut self,
        tx: &Value,
        read_home: &mut dyn FnMut() -> Option<String>,
    ) -> Vec<Event> {
        let Some(id) = tx.get("id").and_then(Value::as_str).filter(|id| !id.is_empty()) else {
            return Vec::new();
        };
        // Cheap pre-check before any venue ask: only these two kinds can carry cash, and only a non-zero
        // figure is worth the currency read.
        let kind = tx.get("type").and_then(Value::as_str);
        if !matches!(kind, Some("DAILY_FINANCING" | "ORDER_FILL")) || self.seen.contains(id) {
            return Vec::new();
        }
        if self.home.is_none() && events_of(tx, None).is_ok_and(|e| !e.is_empty()) {
            self.home = read_home();
        }
        let events = match events_of(tx, self.home.as_deref()) {
            Ok(events) => events,
            Err(total) => {
                tracing::warn!(
                    venue = VENUE,
                    id,
                    total,
                    "oanda: a DAILY_FINANCING states a total but no position entry to route it by; \
                     the amount is NOT booked (the reconcile pass re-seeds the balance)"
                );
                self.remember(id);
                return Vec::new();
            }
        };
        if events.is_empty() {
            return Vec::new();
        }
        self.remember(id);
        for _ in &events {
            note_unmeasured_booking("financing amount");
        }
        events.into_iter().map(Event::Funding).collect()
    }

    fn remember(&mut self, id: &str) {
        if self.order.len() >= SEEN_CAP
            && let Some(oldest) = self.order.pop_front()
        {
            self.seen.remove(&oldest);
        }
        self.seen.insert(id.to_string());
        self.order.push_back(id.to_string());
    }
}

#[path = "financing_tests.rs"]
#[cfg(test)]
mod financing_tests;
