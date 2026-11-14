//! Live Deribit public MARKET data — the pure frame decoders (split-plane I9). The venue's live
//! market-data client did not exist at all until this module: `history.rs` is the historical REST
//! candle reader and the WS lanes in `execution/user_data.rs`/`reconcile/mod.rs` are execution-side. This is
//! the decode half of the new `vike_data::DataClient` feed ([`crate::feed`] is the pump
//! half); everything here is PURE over its inputs — fixture-tested without a socket, the same
//! split every other covered venue keeps (`vike_bybit::feed::tick` is the closest sibling).
//!
//! All four channels ride Deribit's ONE public JSON-RPC subscription grammar (the
//! [`crate::feed::options`] precedent): `public/subscribe` with a channel list, then
//! `{"method":"subscription","params":{"channel":…,"data":…}}` notifications. Keyless — every
//! channel here is public, on the SAME mainnet host as the crate's other public reads
//! (`crates/bridges/deribit/src/feed/options/mod.rs`'s `mainnet_websocket`; the authed exec half points at
//! testnet — the crate CLAUDE.md's two-networks split, and this module sits on the public side).
//!
//! ## The L2 book: `book.{instrument}.{interval}` — a DeltaSync chain over `change_id`
//! The first frame after subscribe is `type:"snapshot"` (full book, a `change_id` anchor); every
//! later frame is `type:"change"` carrying `prev_change_id` + `change_id`. Deribit's documented
//! sync rule: a change frame is trustworthy iff its `prev_change_id` equals the `change_id` of the
//! frame before it — anything else means the venue dropped a message and the consumer must
//! re-subscribe for a fresh snapshot. [`route_frame`] maps that onto the shared
//! [`vike_marketdata::L2Book`] law the conformance harness pins
//! (`crates/vike-bridge-core/tests/market_data_conformance.rs`, where deribit is a covered
//! DeltaSync row):
//!
//! * `type:"snapshot"` → [`L2Book::apply_snapshot`] at `change_id` → [`DataClientEvent::BookUpdated`];
//! * `type:"change"`, `change_id <= last_sequence` → already reflected (a replayed frame) →
//!   [`DataClientEvent::Ignored`], book untouched — checked FIRST, the binance `apply_depth_event`
//!   ordering, because a replay's `prev_change_id` no longer matches the anchor either and judging
//!   the chain first would misread a harmless duplicate as a gap;
//! * `type:"change"`, `prev_change_id != last_sequence` → the chain is broken (frames dropped) →
//!   [`DataClientEvent::Resync`], book untouched — the pump ends the session and reconnects, and the fresh
//!   subscribe delivers a fresh snapshot (resync == resubscribe on this venue, by construction);
//! * otherwise fold via [`L2Book::apply_delta`] at `change_id` → [`DataClientEvent::BookUpdated`].
//!
//! `change_id` values JUMP arbitrarily between frames (they are venue-global, not per-book
//! contiguous), which is why the chain check is `prev_change_id == last_sequence` and never a `+1` rule
//! — the binance `U`/`u` span shape, not bybit's strict increment.
//!
//! Book levels are `[action, price, amount]` triplets — JSON NUMBERS, not the decimal strings
//! binance/bybit/okx send. `action` ∈ `"new"`/`"change"`/`"delete"`; a `delete` carries amount `0`
//! and maps to the qty-0 remove [`L2Book`] already understands, so all three actions collapse to
//! `(price, amount)` with `delete` forced to `0.0`.
//!
//! ## Wire units the decoders carry VERBATIM (scaling is the consumer's job)
//! * `trades.{instrument}.{interval}` rows: `direction` is the TAKER side, so `"sell"` →
//!   `is_buyer_maker = true` (the buyer rested) — the same mapping as bybit's `S == "Sell"` and
//!   okx's `side == "sell"`. `amount` is the venue's CONTRACT unit — USD notional on the inverse
//!   futures/perpetuals, coin on the options/linear books — carried verbatim into
//!   [`TickTrade::size`], never rescaled here.
//! * `quote.{instrument}`: best bid/ask price + amount, one frame per top-of-book change (venue
//!   throttled) → [`TickQuote`], `ts` from the frame's `timestamp` (epoch-ms).
//! * `chart.trades.{instrument}.{resolution}`: one OHLCV row per push, `tick` = the bar-open
//!   epoch-ms, `volume` = BASE units (`cost`, the quote notional, is deliberately NOT read — the
//!   same column choice as `crates/bridges/deribit/src/history.rs`'s `parse_deribit_klines`, so a
//!   live bar and a backfilled bar agree). Deribit pushes NO closed-bar flag — closing is inferred
//!   by the feed's [`crate::feed::BarFolder`] when a later push opens a newer bucket.

use serde_json::Value;

// A price/size pair that can safely enter a book/tape fold is `is_valid_trade`'s: finite and
// positive (a downstream volume fold would spin on `+Inf` or corrupt on a zero/garbage size).
use vike_bridge_core::data_client::event::DataClientEvent;
use vike_bridge_core::is_valid_trade;
use vike_bridge_core::json::get_f64_opt;
use vike_marketdata::{Bar, FundingRate, L2Book, L2BookLevel, TickQuote, TickTrade};

// ── channel builders ─────────────────────────────────────────────────────────────────────────────

/// Notification cadence for the book/trades channels. 100 ms is the freshness sweet spot without a
/// raw-rate frame storm — the SAME deliberate pick (and rationale) as
/// [`crate::feed::options`]'s `TICKER_INTERVAL`.
pub const MARKET_DATA_INTERVAL: &str = "100ms";

/// `book.{instrument}.{interval}` — the incremental L2 book channel. Instrument ids are
/// case-SENSITIVE (uppercase), passed through verbatim.
pub fn book_channel(instrument: &str) -> String {
    format!("book.{instrument}.{MARKET_DATA_INTERVAL}")
}

/// `trades.{instrument}.{interval}` — the executed-prints channel.
pub fn trades_channel(instrument: &str) -> String {
    format!("trades.{instrument}.{MARKET_DATA_INTERVAL}")
}

/// `quote.{instrument}` — the best-bid/ask channel (venue-throttled, no interval suffix).
pub fn quote_channel(instrument: &str) -> String {
    format!("quote.{instrument}")
}

/// `chart.trades.{instrument}.{resolution}` — the live OHLCV channel; `resolution` is the SAME
/// venue code the REST reader maps (`crates/bridges/deribit/src/history.rs`'s `resolution_code`:
/// `"1"`, `"60"`, `"1D"`, …), so the live and historical lanes cannot disagree about buckets.
pub fn chart_channel(instrument: &str, resolution: &str) -> String {
    format!("chart.trades.{instrument}.{resolution}")
}

/// `ticker.{instrument}.agg2` — the aggregated public ticker, which states a perpetual's funding
/// (`current_funding`, `funding_8h`). The bars lane of a PERPETUAL subscribes it beside its chart
/// channel, for the funding rate alone.
pub fn ticker_channel(instrument: &str) -> String {
    format!("ticker.{instrument}.agg2")
}

/// How the paper side settles Deribit's funding: every whole hour.
pub const FUNDING_SETTLE_MS: i64 = 3_600_000;

/// The period `current_funding` is stated per: 8 hours, in ms.
const FUNDING_RATE_PERIOD_MS: f64 = 8.0 * 3_600_000.0;

/// The hour's funding so far, integrated from the ticker's `current_funding` (the model
/// [`parse_ticker_funding`] states). One per perpetual ticker subscription; like
/// `crate::feed::BarFolder` it lives in the feed's `on_text` closure, so it survives
/// reconnects. Each stated rate holds until the next one; a gap (a reconnect) is held at the last rate.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct FundingIntegral {
    /// Start of the hour being integrated, epoch-ms; `None` before the first observation.
    hour: Option<i64>,
    /// `current_funding` x ms accrued from `hour` to `last_timestamp`.
    accumulator: f64,
    last_timestamp: i64,
    last_rate: f64,
}

impl FundingIntegral {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold one stated 8-hour rate at `ts` and answer the funding of the hour it falls in: the integral
    /// so far plus `rate_8h` held to the end of the hour, per hour (`rate_8h / 8` when it holds all hour).
    /// The first observation stands for the hour before it too. A `ts` older than the last one states
    /// nothing (`None`).
    pub fn observe(&mut self, ts: i64, rate_8h: f64) -> Option<f64> {
        if self.hour.is_some() && ts < self.last_timestamp {
            return None;
        }
        let hour = ts.div_euclid(FUNDING_SETTLE_MS) * FUNDING_SETTLE_MS;
        match self.hour {
            Some(h) if h == hour => {
                self.accumulator += self.last_rate * (ts - self.last_timestamp) as f64
            }
            // a new hour: the rate in force crossed its start (the first one stands for its own past)
            previous => {
                let held = if previous.is_some() { self.last_rate } else { rate_8h };
                self.hour = Some(hour);
                self.accumulator = held * (ts - hour) as f64;
            }
        }
        self.last_timestamp = ts;
        self.last_rate = rate_8h;
        Some(
            (self.accumulator + rate_8h * (hour + FUNDING_SETTLE_MS - ts) as f64)
                / FUNDING_RATE_PERIOD_MS,
        )
    }
}

/// Parse a perpetual's `ticker.*` frame into the sink's funding observation for `symbol`, folding its
/// rate into `integral`, or `None` (another channel, a reply, a ticker with no `current_funding`: a
/// dated future or an option; a ticker older than the last one).
///
/// ⚠ **The model.** Deribit accrues funding CONTINUOUSLY (each instant at its current rate, into the
/// session's realized PnL); `current_funding` is that instantaneous rate, stated per 8 hours, and it
/// moves from second to second. MEASURED 2026-11-08 on mainnet (decision 0262): the published
/// `interest_1h` of the hour ending at `H` is the integral of that rate over `[H-1h, H)` / 8h, and a
/// once-an-hour sample of it missed `interest_1h` by about half its size each hour. So the observation's
/// settlement is the next whole hour after the ticker's own `timestamp` and its rate is the hour's
/// [`FundingIntegral`]: the integral so far plus the current rate held to the hour's end, which the last
/// ticker before the hour makes the hour's whole accrual. `funding_8h` (the trailing realized 8-hour
/// figure) is not used. The price is left to the mark.
pub fn parse_ticker_funding(
    frame: &Value,
    symbol: &str,
    integral: &mut FundingIntegral,
) -> Option<FundingRate> {
    let data = subscription_data(frame, "ticker.")?;
    let rate_8h = get_f64_opt(data, "current_funding").filter(|r| r.is_finite())?;
    let ts = data.get("timestamp").and_then(Value::as_i64)?;
    Some(FundingRate {
        timestamp: ts,
        symbol: symbol.to_string(),
        rate: integral.observe(ts, rate_8h)?,
        interval_ms: Some(FUNDING_SETTLE_MS as u64),
        next_timestamp: Some((ts.div_euclid(FUNDING_SETTLE_MS) + 1) * FUNDING_SETTLE_MS),
        predicted: None,
        price: None,
    })
}

/// Deribit `public/subscribe` frame for keyless public channels (replayed verbatim each session by
/// the shared driver). The `id` is a fixed sentinel — the pump confirms the handshake off the
/// reply/data frames ([`classify_rpc_reply`]), never by id-matching.
pub fn public_subscribe_frame(channels: &[String]) -> String {
    serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "public/subscribe",
        "params": {"channels": channels},
    })
    .to_string()
}

/// The `public/unsubscribe` twin of [`public_subscribe_frame`]: sent with it on an open socket, the
/// depth lane's timed re-seed (the venue answers the re-subscribe with a fresh `snapshot`). Its reply
/// names the channels it dropped, which [`classify_rpc_reply`] reads as an ack.
pub fn public_unsubscribe_frame(channels: &[String]) -> String {
    serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "public/unsubscribe",
        "params": {"channels": channels},
    })
    .to_string()
}

// ── JSON-RPC reply classification (the subscribe ack / keepalive-reply lane) ─────────────────────

/// What a NON-subscription (JSON-RPC reply) frame means to a pump session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedRpcReply {
    /// A `public/subscribe` reply whose `result` array names at least one channel — the venue
    /// accepted the subscription (disarms a subscribe-ack watchdog; today the lanes run on the
    /// idle watchdog and treat this as plain confirmation).
    Ack,
    /// An attributable venue error — a JSON-RPC `error` envelope, or a subscribe reply whose
    /// `result` array is EMPTY (the venue matched no channel: a misspelled instrument would
    /// otherwise idle-loop forever with no data and no error).
    Error(String),
    /// Any other reply (e.g. the `public/test` keepalive answer, whose `result` is an object) —
    /// inbound activity for the idle watchdog, nothing more.
    Other,
    /// Not a JSON-RPC reply frame at all (a `subscription` notification, junk).
    NotReply,
}

/// Classify a decoded frame as a JSON-RPC REPLY (`id` + `result`/`error`) — the shared first step
/// of every lane's `on_text`. Subscription notifications and non-objects are [`FeedRpcReply::NotReply`].
pub fn classify_rpc_reply(frame: &Value) -> FeedRpcReply {
    if !frame.is_object() || frame.get("method").is_some() {
        return FeedRpcReply::NotReply; // a notification (or not an envelope at all)
    }
    if let Some(err) = frame.get("error") {
        let code = err.get("code").and_then(Value::as_i64).unwrap_or(0);
        let msg = err.get("message").and_then(Value::as_str).unwrap_or("(no message)");
        return FeedRpcReply::Error(format!("Deribit error {code}: {msg}"));
    }
    match frame.get("result") {
        Some(Value::Array(chans)) if chans.is_empty() => {
            FeedRpcReply::Error("Deribit subscribe matched no channels (bad instrument?)".into())
        }
        Some(Value::Array(_)) => FeedRpcReply::Ack,
        Some(_) => FeedRpcReply::Other, // e.g. the public/test keepalive reply ({"version": …})
        None => FeedRpcReply::NotReply,
    }
}

// ── subscription-notification decoders ───────────────────────────────────────────────────────────

/// `params.data` of a `subscription` notification on a channel with the given prefix, or `None`
/// for any other frame.
fn subscription_data<'a>(frame: &'a Value, channel_prefix: &str) -> Option<&'a Value> {
    if frame.get("method").and_then(Value::as_str) != Some("subscription") {
        return None;
    }
    let params = frame.get("params")?;
    let channel = params.get("channel").and_then(Value::as_str)?;
    if !channel.starts_with(channel_prefix) {
        return None;
    }
    params.get("data")
}

/// One side of a `book.*` frame: `[["new"|"change"|"delete", price, amount], …]` (JSON numbers) →
/// `L2BookLevel`s, with `delete` forced to qty `0.0` (the [`L2Book`] remove convention). A malformed
/// entry is skipped, never fatal.
fn book_levels(side: Option<&Value>) -> Vec<L2BookLevel> {
    side.and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|level| {
                    let l = level.as_array()?;
                    let action = l.first()?.as_str()?;
                    let px = l.get(1)?.as_f64()?;
                    let qty = if action == "delete" { 0.0 } else { l.get(2)?.as_f64()? };
                    Some(L2BookLevel { price: px, qty })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A `quote.*` subscription frame → [`TickQuote`], or `None` for any other frame. The quotes
/// lane's decoder ([`route_frame`] reaches the same code for a frame arriving on a mixed socket).
pub fn parse_quote(frame: &Value) -> Option<TickQuote> {
    quote_from_data(subscription_data(frame, "quote.")?)
}

/// A `trades.*` subscription frame → its prints (empty for any other frame; a malformed row is
/// skipped in place, never fatal — the okx `parse_trades` tolerance).
pub fn parse_trades(frame: &Value) -> Vec<TickTrade> {
    subscription_data(frame, "trades.")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(trade_from_row).collect())
        .unwrap_or_default()
}

/// A `quote.*` data object → [`TickQuote`] (`symbol` from the wire `instrument_name`). `None`
/// unless BOTH sides are present and valid — Deribit omits a side when the book is one-sided
/// there, and a half quote must not fabricate a `0.0` for the other side.
fn quote_from_data(data: &Value) -> Option<TickQuote> {
    let symbol = data.get("instrument_name").and_then(Value::as_str)?;
    let bid = get_f64_opt(data, "best_bid_price")?;
    let ask = get_f64_opt(data, "best_ask_price")?;
    let bid_size = get_f64_opt(data, "best_bid_amount").unwrap_or(0.0);
    let ask_size = get_f64_opt(data, "best_ask_amount").unwrap_or(0.0);
    if !is_valid_trade(bid, bid_size) || !is_valid_trade(ask, ask_size) {
        return None;
    }
    Some(TickQuote {
        timestamp: data.get("timestamp").and_then(Value::as_i64).unwrap_or(0),
        // stamped by the pump at WS receive (0 = not stamped — the shared mapper convention)
        local_timestamp: 0,
        bid,
        ask,
        bid_size,
        ask_size,
        symbol: symbol.to_string(),
    })
}

/// One `trades.*` row → [`TickTrade`]. `direction` is the TAKER side (`"sell"` → the buyer was
/// the maker); `amount` is the venue contract unit, verbatim (module doc).
fn trade_from_row(row: &Value) -> Option<TickTrade> {
    let symbol = row.get("instrument_name").and_then(Value::as_str)?;
    let price = get_f64_opt(row, "price")?;
    let size = get_f64_opt(row, "amount")?;
    if !is_valid_trade(price, size) {
        return None;
    }
    let is_buyer_maker = match row.get("direction").and_then(Value::as_str)? {
        "sell" => true,
        "buy" => false,
        _ => return None,
    };
    Some(TickTrade {
        timestamp: row.get("timestamp").and_then(Value::as_i64).unwrap_or(0),
        local_timestamp: 0, // stamped by the pump at WS receive
        price,
        size,
        is_buyer_maker,
        symbol: symbol.to_string(),
    })
}

/// A `chart.trades.*` data object → [`Bar`] (`tick` = bar-open ms; `volume` BASE units — `cost`
/// deliberately unread, matching the REST reader's column choice). `None` if any OHLC field is
/// absent/non-numeric.
pub fn parse_chart_bar(frame: &Value) -> Option<Bar> {
    let data = subscription_data(frame, "chart.trades.")?;
    Some(vike_bridge_core::klines::kline_to_bar(
        data.get("tick").and_then(Value::as_i64)?,
        get_f64_opt(data, "open")?,
        get_f64_opt(data, "high")?,
        get_f64_opt(data, "low")?,
        get_f64_opt(data, "close")?,
        get_f64_opt(data, "volume").unwrap_or(0.0),
    ))
}

/// Parse a `book.*` SNAPSHOT frame's raw levels + `change_id`. `None` if the frame is not a book
/// snapshot (a change, a reply, another channel). The book pump uses this to infer the tick grid
/// and (re)build its [`L2Book`] before handing subsequent frames to [`route_frame`] — the
/// `vike_bybit::feed::tick::parse_orderbook_snapshot` role.
pub fn parse_book_snapshot(frame: &Value) -> Option<(u64, Vec<L2BookLevel>, Vec<L2BookLevel>)> {
    let data = subscription_data(frame, "book.")?;
    if data.get("type").and_then(Value::as_str) != Some("snapshot") {
        return None;
    }
    let seq = data.get("change_id").and_then(Value::as_u64)?;
    Some((seq, book_levels(data.get("bids")), book_levels(data.get("asks"))))
}

/// Route ONE Deribit WS text frame to a lane, folding `book.*` snapshot/change frames into `book`.
/// Pure over `book` — testable without a socket, and the seam the market-data conformance harness
/// drives (module doc: the chain-sync rules and their check ORDER).
///
/// It answers one [`DataClientEvent`]: a `quote.*` top of book; the prints of one `trades.*` push;
/// the book changed, with the notification's `params.data.timestamp` as `ts_ms` (`0` when absent) so
/// the depth lane's freshness watchdog needs no second parse; `Resync` when the `prev_change_id` chain
/// broke (the book NOT touched: the pump ends the session and the reconnect's fresh subscribe delivers
/// a fresh snapshot); or nothing (a reply frame, an unknown channel, a malformed or stale row).
pub fn route_frame(text: &str, _symbol: &str, book: &mut L2Book) -> DataClientEvent {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return DataClientEvent::Ignored;
    };
    if let Some(data) = subscription_data(&v, "book.") {
        let Some(change_id) = data.get("change_id").and_then(Value::as_u64) else {
            return DataClientEvent::Ignored;
        };
        let (bids, asks) = (book_levels(data.get("bids")), book_levels(data.get("asks")));
        let ts_ms = data.get("timestamp").and_then(Value::as_i64).unwrap_or(0);
        return match data.get("type").and_then(Value::as_str) {
            Some("snapshot") => {
                book.apply_snapshot(change_id, &bids, &asks);
                DataClientEvent::BookUpdated { timestamp_ms: ts_ms }
            }
            Some("change") => {
                // Stale FIRST (binance's apply_depth_event ordering — see the module doc), then
                // the chain check, then the fold.
                if change_id <= book.last_sequence {
                    DataClientEvent::Ignored // already reflected — book stays trustworthy
                } else if data.get("prev_change_id").and_then(Value::as_u64)
                    != Some(book.last_sequence)
                {
                    DataClientEvent::Resync // chain broken — frames dropped, do NOT fold
                } else if book.apply_delta(change_id, &bids, &asks) {
                    DataClientEvent::BookUpdated { timestamp_ms: ts_ms }
                } else {
                    DataClientEvent::Ignored
                }
            }
            _ => DataClientEvent::Ignored,
        };
    }
    let rows = parse_trades(&v);
    if !rows.is_empty() {
        return DataClientEvent::Trades(rows);
    }
    parse_quote(&v).map_or(DataClientEvent::Ignored, DataClientEvent::Quote)
}

#[path = "decode_tests.rs"]
#[cfg(test)]
mod decode_tests;
