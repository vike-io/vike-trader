//! Deribit perpetual FUNDING, read from the account's transaction log: this venue's one producer of a
//! [`FundingEvent`] (decision 0258, item 6, as amended).
//!
//! **The source: `private/get_transaction_log`, each row's `interest_pl`.** Deribit accrues a perpetual's
//! funding continuously and REALIZES it on the account's rows of that instrument: on every trade or other
//! position change (`interest_pl` is the funding PnL "for the interval between this entry and the previous
//! trade or position change", the venue's doc) and on the daily 08:00 UTC `settlement` row. So the
//! instrument's `interest_pl` values add up to the funding the account paid or received, each slice ONCE,
//! and every row carries the log's unique integer `id`. The sources not taken:
//! `private/get_settlement_history_by_currency` and `_by_instrument` state `funding` on the settlement
//! entry alone (the slices a trade realized are missing) and name no entry id; the position's
//! `realized_funding` is a running total, not an event; the private stream has no funding channel.
//!
//! **In the settle coin, received positive.** `interest_pl` is PnL in the row's `currency` (BTC for
//! `BTC-PERPETUAL`): [`FundingEvent::amount`] takes it as stated (no flip), `currency` names that currency, and a
//! coin ledger books it as is (`vike_execution`'s `Account::apply_funding`). A row states no rate
//! (`funding_rate` 0.0, as okx's bills) and the event carries no mark (a coin ledger reads none).
//!
//! **Once, against the adopted wallet.** The recon client of a perpetual or future mount, inverse or linear,
//! reports `balance + session_rpl` (`crate::reconcile::parse_account_summary_coin_cash`, its
//! `reports_session_cash`), and the funding a row realizes is inside `session_rpl` from that row on (the venue's
//! doc: a position's `realized_funding` is "included in session realized profit or loss"). Booking the event
//! moves the ledger by the same amount, so the next reconcile pass expects the wallet's figure again: the event
//! is how the funding reaches own PnL and the drawdown latch, never a second copy of what the wallet holds
//! (`crates/bridges/deribit/tests/offline/funding.rs` proves both halves).
//!
//! **The restart law (bybit's and okx's).** The poller is floored at its spawn instant: a row stamped
//! before it is already inside the wallet the first reconcile pass adopts, so folding it would count it
//! twice. The window is `vike_bridge_core::exec_actor::funding_poll`'s `FundingWindow` (every polled venue's: it
//! starts at the floor, then five minutes before the end of the last GOOD one, at most a week wide), and the poll
//! follows `continuation` to its start, so a failed poll loses nothing. A row stamped before the window's start is
//! NEVER emitted (an earlier window held it): the client-side filter is the law, `start_timestamp` only shrinks the
//! answer, as bybit's `startTime` does. Inside the window the row `id` de-duplicates.
//!
//! **Scope: a PERPETUAL mount, inverse or linear (USDC), its own rows** ([`funding_currency`]); a dated future pays
//! no funding. A linear perpetual is read exactly as an inverse one, each row's `interest_pl` when it realizes,
//! because its recon balance is the same `balance + session_rpl`: MEASURED 2026-11-08 on the demo account's USDC log,
//! a `BTC_USDC-PERPETUAL` close that realized `interest_pl` -1e-08 moved `balance` by its fee alone, and the next
//! 08:00 `settlement` row moved it by the session's closing cashflows plus that funding (`-0.00038001`), the figure
//! `session_rpl` held until then and resets from, so the reported cash neither lags the books nor jumps at 08:00.
//! shortcut: a perpetual ADMITTED beside a dated-future mount is not read (its rows are not the mounted
//! instrument's); share the recon client's admitted list, as okx's poller does, when one is.

use serde_json::{Value, json};
use vike_bridge_core::exec_actor::funding_poll::FundingWindow;
use vike_bridge_core::json::{get_f64_opt, get_i64, json_int};
use vike_model::events::{Event, FundingEvent, PositionSide};

/// The JSON-RPC method of the account's transaction log.
pub const PATH_TRANSACTION_LOG: &str = "private/get_transaction_log";
/// Rows asked per page: the venue's maximum `count`.
const PAGE: usize = 1000;

/// The settle coin of a PERPETUAL `public/get_instrument` row (inverse or linear), the currency its log is read
/// in; `None` for every other instrument (the module doc's scope).
pub fn funding_currency(row: &Value) -> Option<String> {
    match crate::catalog::contract_of(row)? {
        vike_model::Contract::Perp(p) => Some(p.settle),
        _ => None,
    }
}

/// One log row as `symbol`'s funding: `Some((id, event))` for a row of that instrument whose
/// `interest_pl` is a non-zero number and which carries its `id` (a row with none could not be told from
/// itself on the next poll, so its funding is left to the wallet). The event names the MOUNTED spelling,
/// the symbol the engine accepts.
fn funding_of(row: &Value, symbol: &str) -> Option<(i64, FundingEvent)> {
    let name = row.get("instrument_name").and_then(Value::as_str)?;
    let amount = get_f64_opt(row, "interest_pl").filter(|f| *f != 0.0 && f.is_finite())?;
    if !name.eq_ignore_ascii_case(symbol) {
        return None;
    }
    let id = row.get("id").and_then(json_int)?;
    Some((
        id,
        FundingEvent {
            venue: "deribit".into(),
            symbol: symbol.into(),
            position_side: PositionSide::Both,
            funding_rate: 0.0,
            amount, // received-positive, NO flip
            mark_price: None,
            timestamp: get_i64(row, "timestamp"),
            // Stamped at the START — see `vike_model::events::FundingEvent::route_key`.
            route_key: None,
            currency: row.get("currency").and_then(Value::as_str).map(Into::into),
        },
    ))
}

/// The funding rows of `symbol` in one page of transaction-log `logs`, as events, in the page's order.
pub fn decode_deribit_funding(logs: &[Value], symbol: &str) -> Vec<FundingEvent> {
    logs.iter().filter_map(|r| funding_of(r, symbol)).map(|(_, event)| event).collect()
}

/// The transaction-log poller of one perpetual mount. `read` is one idempotent JSON-RPC read of
/// [`PATH_TRANSACTION_LOG`] with the given params, answering its `result` (the exec side hands it the
/// healed read of a socket of its own). `poll()` answers the NEW funding events, oldest first; a failed
/// read is `Err` and moves nothing, so the next poll reads the same window again.
pub struct DeribitFundingPoller<R> {
    read: R,
    symbol: String,
    currency: String,
    /// The window over the log and the row ids emitted, floored at the spawn instant (`0` = none).
    window: FundingWindow<i64>,
}

impl<R: FnMut(&Value) -> Result<Value, String>> DeribitFundingPoller<R> {
    pub fn new(read: R, symbol: &str, currency: &str, floor_ms: i64) -> Self {
        DeribitFundingPoller {
            read,
            symbol: symbol.to_string(),
            currency: currency.to_string(),
            window: FundingWindow::new(floor_ms),
        }
    }

    pub fn poll(&mut self, now_ms: i64) -> Result<Vec<Event>, String> {
        let (start, end) = self.window.span(now_ms);
        let mut params = json!({
            "currency": self.currency,
            "start_timestamp": start,
            "end_timestamp": end,
            "count": PAGE,
        });
        let mut rows = Vec::new();
        let mut last: Option<i64> = None;
        loop {
            let page = (self.read)(&params)?;
            let logs = page.get("logs").and_then(Value::as_array).ok_or_else(|| {
                format!("{PATH_TRANSACTION_LOG}: the answer carries no `logs` list")
            })?;
            rows.extend(logs.iter().filter_map(|r| funding_of(r, &self.symbol)));
            // The next OLDER page while the venue names one that moves back (never a loop on a repeated token).
            match page.get("continuation").and_then(json_int) {
                Some(c) if !logs.is_empty() && last.is_none_or(|l| c < l) => {
                    last = Some(c);
                    params["continuation"] = json!(c);
                }
                _ => break,
            }
        }
        rows.sort_by_key(|(id, _)| *id);
        // every read succeeded: a row is admitted once, though two pages list it
        rows.retain(|(id, event)| self.window.admit(*id, event.timestamp));
        self.window.read_through(end);
        Ok(rows.into_iter().map(|(_, event)| Event::Funding(event)).collect())
    }
}
