//! Bybit funding via the REST transaction-log — the documented received-positive source.
//! Exact port of `exec/bybit/funding.py`.
//!
//! GET /v5/account/transaction-log?category=linear&type=SETTLEMENT: `funding` is the
//! SIGNED cashflow (+received / −paid, doc verbatim) fed DIRECTLY into
//! FundingEvent.amount. NOT the WS execution-topic 'Funding' row, whose execFee is the
//! EXACT NEGATIVE of this cashflow (the sign trap — solved by source selection, not
//! sign-flipping). No markPrice in the log (mark_price=None); feeRate IS the rate.
//! Dedups by row 'id' BEFORE decoding (apply_funding is NOT idempotent), marking seen
//! only once every page of the poll was read; the seen ids are forgotten by time (the window's).
//!
//! **Spawn-time floor (the restart law).** The id-dedup is in-memory only, so at every process
//! (re)start the seen-set is empty and — with no time bound — the venue's whole default
//! transaction-log window (~7 days of settlements) would re-fold through the non-idempotent
//! `Account::apply_funding` (`balance += amount`). Those historical settlements are ALREADY
//! embedded in the venue-reported balance the live account runs on: live engines start
//! `BalanceMode::Delta` and flip `Authoritative` once a venue balance lands, which the reconcile
//! pass does (`ReconcileClient::fetch_balance` seeds `Account::balance` absolutely — see
//! `vike_core::runtime::reconcile_reports`), so re-emitting a pre-start settlement double-counts
//! it. This lane exists ONLY for settlements that land WHILE started. Hence [`BybitFundingPoller`]
//! takes a `floor_ms` (the spawner passes its spawn instant) and NEVER emits a row with
//! `transactionTime < floor_ms` — the client-side ts filter is the guarantee; the `startTime`
//! request param merely shrinks the response. The in-memory id-dedup remains the second guard
//! within a process lifetime.
//!
//! **A window that MOVES, both ends sent.** The venue reads a lone `startTime` as `[startTime,
//! startTime + 24 h]` (its doc: "Only startTime is passed, return range between startTime and
//! startTime+24 hours"; both ends at most 7 days apart). Until 2026-11-08 the poller sent the spawn
//! floor alone, so a node up for more than a day asked for its first day forever and booked no
//! funding after it. Each poll now asks `[since, min(now, since + 7 d)]`, `since` starting at the
//! floor and then [`FUNDING_LOOKBACK_MS`] before the end of the last GOOD window (the one window of every polled
//! venue, `vike_bridge_core::exec_actor::funding_poll`'s `FundingWindow`: a failed poll loses nothing, a long
//! outage is read back a week at a time), follows
//! `nextPageCursor` past a full page (the venue's page is 20 rows by default, 50 at most), and never emits a row
//! stamped before `since` (an earlier window held it). A floorless probe (`floor_ms == 0`) sends the
//! request it always sent.

use serde_json::{Value, json};
#[cfg(doc)]
use vike_bridge_core::exec_actor::funding_poll::FUNDING_LOOKBACK_MS;
use vike_bridge_core::exec_actor::funding_poll::FundingWindow;
use vike_bridge_core::json::{get_f64, get_i64, json_number, json_text};
use vike_model::events::{Event, FundingEvent};

use crate::execution::rest::VenueRestBybit;
use crate::transport::client::FailedRequest;
use vike_bridge_core::transport::json::RestTransportJson;

pub const LOG_PATH: &str = "/v5/account/transaction-log";
/// Rows asked per page: the venue's maximum `limit`.
const PAGE_LIMIT: u32 = 50;
/// Pages read per poll at most (a venue repeating its cursor cannot hold the poll in a loop).
const MAX_PAGES: usize = 20;

/// Decode SETTLEMENT rows → FundingEvents. Skips non-SETTLEMENT rows and rows whose
/// `funding` is null, "", or the literal "0" (Python tuple-membership, ported exactly).
pub fn decode_bybit_funding_settlements(
    rows: &[Value],
    venue: &str,
    symbol: &str,
) -> Vec<FundingEvent> {
    let mut out = Vec::new();
    for r in rows {
        if r.get("type").map(json_text).unwrap_or_default() != "SETTLEMENT" {
            continue;
        }
        let funding = r.get("funding");
        let skip = match funding {
            None | Some(Value::Null) => true,
            Some(Value::String(s)) => s.is_empty() || s == "0",
            _ => false,
        };
        if skip {
            continue;
        }
        out.push(FundingEvent {
            venue: venue.to_string().into(),
            symbol: match r.get("symbol") {
                Some(Value::String(s)) => s.clone().into(),
                _ => symbol.to_string().into(),
            },
            position_side: vike_model::events::PositionSide::Both,
            funding_rate: get_f64(r, "feeRate"),
            amount: funding.map(number_v).unwrap_or(0.0), // received-positive, NO flip
            mark_price: None,
            timestamp: get_i64(r, "transactionTime"),
            // Stamped at the START — see `vike_model::events::FundingEvent::route_key`.
            route_key: None,
            currency: None,
        });
    }
    out
}

/// Value-level `float(x or 0)` for the already-extracted `funding` cell — the keyless twin of the
/// shared keyed `json::get_f64` above; both fold through `json_number`, so the coercion has
/// exactly one home. (The `id`/`type` reads use `json::json_text` instead, deliberately: the dedup
/// guard needs serde's Display of a non-string id.)
fn number_v(v: &Value) -> f64 {
    json_number(v).unwrap_or(0.0)
}

/// REST poller for linear-perp funding settlements. `poll()` returns the NEW events (`ts >=
/// floor_ms`, dedup by row id) for the caller to feed into the core ingest; a fetch failure is
/// `Err` in `FailedRequest`'s words (so the caller can rate-limit a warn) and never marks anything seen — the
/// next poll retries.
pub struct BybitFundingPoller<'a, T: RestTransportJson> {
    client: &'a VenueRestBybit<T>,
    symbol: String,
    /// The window and the row ids it emitted (`vike_bridge_core::exec_actor::funding_poll`'s law, keyed by the row's
    /// `id`): floored at `floor_ms` (the module doc's restart law; spawners pass their spawn instant, `0` = no floor
    /// for tests and probes), then [`FUNDING_LOOKBACK_MS`] before the end of the last good window.
    window: FundingWindow<String>,
}

impl<'a, T: RestTransportJson> BybitFundingPoller<'a, T> {
    pub fn new(client: &'a VenueRestBybit<T>, symbol: &str, floor_ms: i64) -> Self {
        BybitFundingPoller {
            client,
            symbol: symbol.to_string(),
            window: FundingWindow::new(floor_ms),
        }
    }

    /// [`Self::poll_at`] the wall clock.
    pub fn poll(&mut self) -> Result<Vec<Event>, String> {
        self.poll_at(vike_model::now_ms())
    }

    /// One poll of the window ending at `now_ms` (the module doc), its NEW events in the venue's order.
    pub fn poll_at(&mut self, now_ms: i64) -> Result<Vec<Event>, String> {
        // `startTime` is a response-size optimization ONLY — the ts filter below is the law
        // (sent only with a real floor, so a floorless probe stays byte-identical to before).
        let mut params =
            vec![("category", json!(self.client.category.wire())), ("type", json!("SETTLEMENT"))];
        // A coin-margined mount's funding is paid in its settle coin (decision 0258 books it as stated): the
        // log is asked for that coin alone, so another coin's settlement never lands in this engine's books.
        if self.client.category == crate::history::Category::Inverse
            && let Some(coin) = vike_catalog::inverse_settle_coin("bybit", &self.symbol)
        {
            params.push(("currency", json!(coin)));
        }
        let (since, end) = self.window.span(now_ms);
        if self.window.floor_ms() > 0 {
            params.push(("startTime", json!(since.to_string())));
            params.push(("endTime", json!(end.to_string())));
            params.push(("limit", json!(PAGE_LIMIT)));
        }
        let mut rows: Vec<Value> = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let mut page_params = params.clone();
            if let Some(c) = &cursor {
                // The venue percent-encodes its cursor and the signer encodes every value once.
                page_params.push(("cursor", json!(crate::reconcile::percent_decode(c))));
            }
            let result = self
                .client
                .call_public(LOG_PATH, &page_params)
                .map_err(|e| FailedRequest::new("GET", LOG_PATH, &page_params, e).to_string())?;
            let page = result.get("list").and_then(Value::as_array).cloned().unwrap_or_default();
            // Only a FULL page of a windowed poll goes on: the venue hands a cursor on its last page too
            // (the recon client's measurement), and a floorless probe sends the one request it always sent.
            let full = self.window.floor_ms() > 0 && page.len() >= PAGE_LIMIT as usize;
            rows.extend(page);
            match result.get("nextPageCursor").and_then(Value::as_str) {
                Some(next) if full && !next.is_empty() && cursor.as_deref() != Some(next) => {
                    cursor = Some(next.to_string());
                }
                _ => break,
            }
        }
        // Every page was read: a row stamped before the window (pre-floor, or an earlier window's) or already emitted is
        // not admitted, and a row with no id could not be told from itself on the next poll.
        let new_rows: Vec<Value> = rows
            .into_iter()
            .filter(|r| {
                let rid = r.get("id").map(json_text).unwrap_or_default();
                !rid.is_empty() && self.window.admit(rid, get_i64(r, "transactionTime"))
            })
            .collect();
        self.window.read_through(end);
        if new_rows.is_empty() {
            return Ok(Vec::new());
        }
        // A settlement row names the venue's symbol in the log's book: booked under its USER name (decision 0290: a
        // linear `BTCUSDT` is `BTCUSDT.P`), as the fills of the same position are.
        let book = Some(self.client.category);
        let events: Vec<Event> = decode_bybit_funding_settlements(&new_rows, "bybit", &self.symbol)
            .into_iter()
            .map(|mut funding| {
                funding.symbol =
                    crate::instrument::class::user_symbol_or_wire(book, &funding.symbol)
                        .as_str()
                        .into();
                Event::Funding(funding)
            })
            .collect();
        Ok(events)
    }
}
