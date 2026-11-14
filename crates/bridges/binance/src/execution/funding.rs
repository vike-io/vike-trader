//! Binance perpetual FUNDING, USDⓈ-M and COIN-M, read from the account's income history: this venue's one producer
//! of a [`FundingEvent`] ([`income_poller`], on a client of the mount's own book). Binance's alone: aster, whose
//! stream is Binance-verbatim, keeps the stream's funding.
//!
//! **The source: `GET /fapi/v1/income?incomeType=FUNDING_FEE`, each row's `income`.** A row names its `symbol`
//! (`BTCUSDT`), its `asset`, its `time` and a `tranId`, which the venue keeps unique per income type and account
//! (the doc), so the poll de-duplicates by it. The event names the row's own instrument as the perp series label
//! (`crate::family::perp_series_symbol`: `BTCUSDT.P`), so the engine's symbol filter books a payment only on an
//! engine that owns it: a cross-margin payment of a position the engine never opened (a position opened by hand on
//! the exchange) is dropped there, where the stream's frame, which names no symbol in cross margin, was booked on
//! the mounted instrument. Received positive as the venue states it (no flip), in the row's `asset` (`currency`). Ids
//! above 2^53 (`1668889277297297490`, recorded) are read as integers, never through a float.
//!
//! **Why not the stream.** The user-data stream's `ACCOUNT_UPDATE` with `m == "FUNDING_FEE"` was the producer
//! until 2026-11-08 (it stays aster's, `crate::family::execution::event_mapper::perp`). It has no id and no replay, so a payment that
//! lands while the socket reconnects was never booked, and in cross margin its wallet row names no symbol. The
//! binance pump now drops that frame whole (`crate::execution::event_mapper::perp::map_binance_perp_stream`): its funding is this
//! poll's, and its wallet figure, if folded, would hold the payment before the poll books it, counting it twice
//! until the next reconcile pass.
//!
//! **The restart law and the window (bybit's and deribit's).** The poller is floored at its spawn instant: a row
//! stamped before it is in the wallet the first reconcile pass adopts. Each poll reads `[since, min(now, since +
//! 7 d)]` with both ends sent, `since` the floor and then [`LOOKBACK_MS`] before the end of the last GOOD window, so
//! a failed poll loses nothing and a row the venue lists a moment late is still read; a row stamped before `since`
//! is never emitted (an earlier window held it). The venue lists a window OLDEST first (MEASURED 2026-11-08 on the
//! demo account's 16 funding rows), so a full page of [`PAGE`] rows is read on from its newest row.

use std::collections::HashMap;

use serde_json::Value;
use vike_bridge_core::json::get_f64_opt;
use vike_bridge_core::signer::Signer;
use vike_bridge_core::transport::RestTransport;
use vike_model::events::{Event, FundingEvent, PositionSide};

use crate::execution::rest::perp::VenueRestBinancePerp;

/// The income-history endpoint of the USDⓈ-M account.
pub const PATH_INCOME: &str = "/fapi/v1/income";
/// The income type of a funding payment.
const FUNDING_FEE: &str = "FUNDING_FEE";
/// Rows asked per page: the venue's maximum `limit`.
const PAGE: usize = 1000;
/// Pages read per poll at most.
const MAX_PAGES: usize = 20;
/// How far before the end of the last good window the next one starts.
pub const LOOKBACK_MS: i64 = 5 * 60_000;
/// The widest window one poll asks: the venue's own default week (its doc states no maximum).
const MAX_WINDOW_MS: i64 = 7 * 86_400_000;

/// One income row as a funding payment: `Some((tranId, event))` for a `FUNDING_FEE` row that names its symbol, a
/// non-zero `income` and an integer `tranId` (a row with none could not be told from itself on the next poll, so
/// its funding is left to the wallet).
fn funding_of(row: &Value) -> Option<(i64, FundingEvent)> {
    if row.get("incomeType").and_then(Value::as_str) != Some(FUNDING_FEE) {
        return None;
    }
    let symbol = row.get("symbol").and_then(Value::as_str).filter(|s| !s.is_empty())?;
    let amount = get_f64_opt(row, "income").filter(|f| *f != 0.0 && f.is_finite())?;
    let tran = row.get("tranId")?;
    let id = tran.as_i64().or_else(|| tran.as_str().and_then(|s| s.parse().ok()))?;
    Some((
        id,
        FundingEvent {
            venue: crate::VENUE.into(),
            symbol: crate::family::perp_series_symbol(symbol).into(),
            position_side: PositionSide::Both,
            funding_rate: 0.0,
            amount, // received-positive, NO flip
            mark_price: None,
            timestamp: row.get("time").and_then(Value::as_i64).unwrap_or(0),
            // Stamped at the START — see `vike_model::events::FundingEvent::route_key`.
            route_key: None,
            currency: row
                .get("asset")
                .and_then(Value::as_str)
                .filter(|a| !a.is_empty())
                .map(Into::into),
        },
    ))
}

/// The funding payments in one page of income `rows`, as events, in the page's order.
pub fn decode_binance_funding(rows: &[Value]) -> Vec<FundingEvent> {
    rows.iter().filter_map(funding_of).map(|(_, event)| event).collect()
}

/// **The funding poller of one perp mount, on `rest`**: a client of the mount's OWN book, so a USDⓈ-M mount reads
/// `/fapi/v1/income` and a COIN-M one, whose client sits on the `dapi` host of its tier
/// (`crate::execution`'s `BinanceUrls::futures_rest`), reads `/dapi/v1/income` (`crate::execution::rest::coin_m::RestTransportDapiPaths` routes the path). One
/// poller, one window law and one pager for both books; the rows of a COIN-M book state their coin as `asset`.
#[expect(clippy::type_complexity, reason = "the read closure's own signature, named once here")]
pub fn income_poller<'a, S: Signer, T: RestTransport>(
    rest: &'a VenueRestBinancePerp<S, T>,
    floor_ms: i64,
) -> BinanceFundingPoller<impl FnMut(&[(&str, String)]) -> Result<Value, String> + 'a> {
    BinanceFundingPoller::new(
        move |params: &[(&str, String)]| {
            rest.transport
                .signed(&rest.base_url, PATH_INCOME, "GET", params, &rest.signer)
                .map_err(|e| e.msg)
        },
        floor_ms,
    )
}

/// The income-history poller of one perp mount. `read` is one signed GET of [`PATH_INCOME`] with the given
/// params, answering the venue's JSON array (the exec side hands it a REST client of its own). `poll()` answers the
/// NEW funding events, oldest first; a failed read is `Err` and moves nothing.
pub struct BinanceFundingPoller<R> {
    read: R,
    /// The emission floor (the spawn instant).
    floor_ms: i64,
    /// Where the next window starts: the floor, then [`LOOKBACK_MS`] before the end of the last good window.
    since_ms: i64,
    /// `tranId` -> its stamp, for the rows emitted that a later window can still list.
    seen: HashMap<i64, i64>,
}

impl<R: FnMut(&[(&str, String)]) -> Result<Value, String>> BinanceFundingPoller<R> {
    pub fn new(read: R, floor_ms: i64) -> Self {
        BinanceFundingPoller { read, floor_ms, since_ms: floor_ms, seen: HashMap::new() }
    }

    pub fn poll(&mut self, now_ms: i64) -> Result<Vec<Event>, String> {
        let start = self.since_ms;
        let end = now_ms.min(start + MAX_WINDOW_MS).max(start);
        let mut from = start;
        let mut rows = Vec::new();
        for _ in 0..MAX_PAGES {
            let params = [
                ("incomeType", FUNDING_FEE.to_string()),
                ("startTime", from.to_string()),
                ("endTime", end.to_string()),
                ("limit", PAGE.to_string()),
            ];
            let page = (self.read)(&params)?;
            let page = page
                .as_array()
                .ok_or_else(|| format!("{PATH_INCOME}: the answer is not a list: {page}"))?;
            rows.extend(page.iter().filter_map(funding_of));
            // shortcut: a full page whose rows all share one instant (1000 perps funded at once) ends the poll
            // with the rest unread; no account of this platform holds that many.
            let newest = page.iter().filter_map(|r| r.get("time").and_then(Value::as_i64)).max();
            match newest {
                Some(t) if page.len() >= PAGE && t > from => from = t,
                _ => break,
            }
        }
        rows.retain(|(id, event)| event.timestamp >= start && !self.seen.contains_key(id));
        rows.sort_by_key(|(id, event)| (event.timestamp, *id));
        rows.dedup_by_key(|(id, _)| *id); // a row two pages listed is one row
        for (id, event) in &rows {
            self.seen.insert(*id, event.timestamp);
        }
        self.since_ms = (end - LOOKBACK_MS).max(self.floor_ms);
        let since = self.since_ms;
        self.seen.retain(|_, ts| *ts >= since);
        Ok(rows.into_iter().map(|(_, event)| Event::Funding(event)).collect())
    }
}
