//! Live Deribit public MARKET data — the pure frame decoders (split-plane I9). The venue's live
//! market-data client did not exist at all until this module: `data.rs` is the historical REST
//! candle reader and the WS lanes in `user_data.rs`/`recon_client.rs` are execution-side. This is
//! the decode half of the new `vike_data::DataClient` feed ([`crate::market_feed`] is the pump
//! half); everything here is PURE over its inputs — fixture-tested without a socket, the same
//! split every other covered venue keeps (`vike_bybit::market_data` is the closest sibling).
//!
//! All four channels ride Deribit's ONE public JSON-RPC subscription grammar (the
//! [`crate::options_feed`] precedent): `public/subscribe` with a channel list, then
//! `{"method":"subscription","params":{"channel":…,"data":…}}` notifications. Keyless — every
//! channel here is public, on the SAME mainnet host as the crate's other public reads
//! (`crates/bridges/deribit/src/options_feed.rs`'s `MAINNET_WS`; the authed exec half points at
//! testnet — the crate CLAUDE.md's two-networks split, and this module sits on the public side).
//!
//! ## The L2 book: `book.{instrument}.{interval}` — a DeltaSync chain over `change_id`
//! The first frame after subscribe is `type:"snapshot"` (full book, a `change_id` anchor); every
//! later frame is `type:"change"` carrying `prev_change_id` + `change_id`. Deribit's documented
//! sync rule: a change frame is trustworthy iff its `prev_change_id` equals the `change_id` of the
//! frame before it — anything else means the venue dropped a message and the consumer must
//! re-subscribe for a fresh snapshot. [`route_frame`] maps that onto the shared
//! [`vike_model::L2Book`] law the conformance harness pins
//! (`crates/vike-bridge-core/tests/market_data_conformance.rs`, where deribit is a covered
//! DeltaSync row):
//!
//! * `type:"snapshot"` → [`L2Book::apply_snapshot`] at `change_id` → [`MdEvent::BookUpdated`];
//! * `type:"change"`, `change_id <= last_seq` → already reflected (a replayed frame) →
//!   [`MdEvent::Ignored`], book untouched — checked FIRST, the binance `apply_depth_event`
//!   ordering, because a replay's `prev_change_id` no longer matches the anchor either and judging
//!   the chain first would misread a harmless duplicate as a gap;
//! * `type:"change"`, `prev_change_id != last_seq` → the chain is broken (frames dropped) →
//!   [`MdEvent::Resync`], book untouched — the pump ends the session and reconnects, and the fresh
//!   subscribe delivers a fresh snapshot (resync == resubscribe on this venue, by construction);
//! * otherwise fold via [`L2Book::apply_delta`] at `change_id` → [`MdEvent::BookUpdated`].
//!
//! `change_id` values JUMP arbitrarily between frames (they are venue-global, not per-book
//! contiguous), which is why the chain check is `prev_change_id == last_seq` and never a `+1` rule
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
//!   [`TradeTick::size`], never rescaled here.
//! * `quote.{instrument}`: best bid/ask price + amount, one frame per top-of-book change (venue
//!   throttled) → [`QuoteTick`], `ts` from the frame's `timestamp` (epoch-ms).
//! * `chart.trades.{instrument}.{resolution}`: one OHLCV row per push, `tick` = the bar-open
//!   epoch-ms, `volume` = BASE units (`cost`, the quote notional, is deliberately NOT read — the
//!   same column choice as `crates/bridges/deribit/src/data.rs`'s `parse_deribit_klines`, so a
//!   live bar and a backfilled bar agree). Deribit pushes NO closed-bar flag — closing is inferred
//!   by the feed's [`crate::market_feed::BarFolder`] when a later push opens a newer bucket.

use serde_json::Value;

use vike_bridge_core::json::get_f64_opt;
use vike_model::{Bar, BookLevel, L2Book, QuoteTick, TradeTick};

/// A price/size pair that can safely enter a book/tape fold: finite and positive (mirrors the
/// okx/binance `is_valid_trade` guard — a downstream volume fold would spin on `+Inf` or corrupt
/// on a zero/garbage size).
fn is_valid_trade(price: f64, size: f64) -> bool {
    price.is_finite() && size.is_finite() && price > 0.0 && size > 0.0
}

// ── channel builders ─────────────────────────────────────────────────────────────────────────────

/// Notification cadence for the book/trades channels. 100 ms is the freshness sweet spot without a
/// raw-rate frame storm — the SAME deliberate pick (and rationale) as
/// [`crate::options_feed`]'s `TICKER_INTERVAL`.
pub const MD_INTERVAL: &str = "100ms";

/// `book.{instrument}.{interval}` — the incremental L2 book channel. Instrument ids are
/// case-SENSITIVE (uppercase), passed through verbatim.
pub fn book_channel(instrument: &str) -> String {
    format!("book.{instrument}.{MD_INTERVAL}")
}

/// `trades.{instrument}.{interval}` — the executed-prints channel.
pub fn trades_channel(instrument: &str) -> String {
    format!("trades.{instrument}.{MD_INTERVAL}")
}

/// `quote.{instrument}` — the best-bid/ask channel (venue-throttled, no interval suffix).
pub fn quote_channel(instrument: &str) -> String {
    format!("quote.{instrument}")
}

/// `chart.trades.{instrument}.{resolution}` — the live OHLCV channel; `resolution` is the SAME
/// venue code the REST reader maps (`crates/bridges/deribit/src/data.rs`'s `resolution_code`:
/// `"1"`, `"60"`, `"1D"`, …), so the live and historical lanes cannot disagree about buckets.
pub fn chart_channel(instrument: &str, resolution: &str) -> String {
    format!("chart.trades.{instrument}.{resolution}")
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

// ── JSON-RPC reply classification (the subscribe ack / keepalive-reply lane) ─────────────────────

/// What a NON-subscription (JSON-RPC reply) frame means to a pump session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RpcReply {
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
/// of every lane's `on_text`. Subscription notifications and non-objects are [`RpcReply::NotReply`].
pub fn classify_rpc_reply(frame: &Value) -> RpcReply {
    if !frame.is_object() || frame.get("method").is_some() {
        return RpcReply::NotReply; // a notification (or not an envelope at all)
    }
    if let Some(err) = frame.get("error") {
        let code = err.get("code").and_then(Value::as_i64).unwrap_or(0);
        let msg = err.get("message").and_then(Value::as_str).unwrap_or("(no message)");
        return RpcReply::Error(format!("Deribit error {code}: {msg}"));
    }
    match frame.get("result") {
        Some(Value::Array(chans)) if chans.is_empty() => {
            RpcReply::Error("Deribit subscribe matched no channels (bad instrument?)".into())
        }
        Some(Value::Array(_)) => RpcReply::Ack,
        Some(_) => RpcReply::Other, // e.g. the public/test keepalive reply ({"version": …})
        None => RpcReply::NotReply,
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
/// `BookLevel`s, with `delete` forced to qty `0.0` (the [`L2Book`] remove convention). A malformed
/// entry is skipped, never fatal.
fn book_levels(side: Option<&Value>) -> Vec<BookLevel> {
    side.and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|lvl| {
                    let l = lvl.as_array()?;
                    let action = l.first()?.as_str()?;
                    let px = l.get(1)?.as_f64()?;
                    let qty = if action == "delete" { 0.0 } else { l.get(2)?.as_f64()? };
                    Some(BookLevel { price: px, qty })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A `quote.*` subscription frame → [`QuoteTick`], or `None` for any other frame. The quotes
/// lane's decoder ([`route_frame`] reaches the same code for a frame arriving on a mixed socket).
pub fn parse_quote(frame: &Value) -> Option<QuoteTick> {
    quote_from_data(subscription_data(frame, "quote.")?)
}

/// A `trades.*` subscription frame → its prints (empty for any other frame; a malformed row is
/// skipped in place, never fatal — the okx `parse_trades` tolerance).
pub fn parse_trades(frame: &Value) -> Vec<TradeTick> {
    subscription_data(frame, "trades.")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(trade_from_row).collect())
        .unwrap_or_default()
}

/// A `quote.*` data object → [`QuoteTick`] (`symbol` from the wire `instrument_name`). `None`
/// unless BOTH sides are present and valid — Deribit omits a side when the book is one-sided
/// there, and a half quote must not fabricate a `0.0` for the other side.
fn quote_from_data(data: &Value) -> Option<QuoteTick> {
    let symbol = data.get("instrument_name").and_then(Value::as_str)?;
    let bid = get_f64_opt(data, "best_bid_price")?;
    let ask = get_f64_opt(data, "best_ask_price")?;
    let bid_size = get_f64_opt(data, "best_bid_amount").unwrap_or(0.0);
    let ask_size = get_f64_opt(data, "best_ask_amount").unwrap_or(0.0);
    if !is_valid_trade(bid, bid_size) || !is_valid_trade(ask, ask_size) {
        return None;
    }
    Some(QuoteTick {
        ts: data.get("timestamp").and_then(Value::as_i64).unwrap_or(0),
        // stamped by the pump at WS receive (0 = not stamped — the shared mapper convention)
        local_ts: 0,
        bid,
        ask,
        bid_size,
        ask_size,
        symbol: symbol.to_string(),
    })
}

/// One `trades.*` row → [`TradeTick`]. `direction` is the TAKER side (`"sell"` → the buyer was
/// the maker); `amount` is the venue contract unit, verbatim (module doc).
fn trade_from_row(row: &Value) -> Option<TradeTick> {
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
    Some(TradeTick {
        ts: row.get("timestamp").and_then(Value::as_i64).unwrap_or(0),
        local_ts: 0, // stamped by the pump at WS receive
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
/// `vike_bybit::market_data::parse_orderbook_snapshot` role.
pub fn parse_book_snapshot(frame: &Value) -> Option<(u64, Vec<BookLevel>, Vec<BookLevel>)> {
    let data = subscription_data(frame, "book.")?;
    if data.get("type").and_then(Value::as_str) != Some("snapshot") {
        return None;
    }
    let seq = data.get("change_id").and_then(Value::as_u64)?;
    Some((seq, book_levels(data.get("bids")), book_levels(data.get("asks"))))
}

/// What one decoded Deribit market-data frame becomes.
#[derive(Debug, Clone, PartialEq)]
pub enum MdEvent {
    /// A `quote.*` top-of-book update.
    Quote(QuoteTick),
    /// The prints of one `trades.*` push (a frame may carry several rows).
    Trades(Vec<TradeTick>),
    /// The L2 book changed (snapshot applied or a change folded) — the pump publishes the book.
    BookUpdated,
    /// The `prev_change_id` chain broke — frames were dropped; the book is untrustworthy and was
    /// NOT touched. The pump must resync (end the session; the reconnect's fresh subscribe
    /// delivers a fresh snapshot).
    Resync,
    /// Anything else: a reply frame, an unknown channel, a malformed/stale row.
    Ignored,
}

/// Route ONE Deribit WS text frame to a lane, folding `book.*` snapshot/change frames into `book`.
/// Pure over `book` — testable without a socket, and the seam the market-data conformance harness
/// drives (module doc: the chain-sync rules and their check ORDER).
pub fn route_frame(text: &str, _symbol: &str, book: &mut L2Book) -> MdEvent {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return MdEvent::Ignored;
    };
    if let Some(data) = subscription_data(&v, "book.") {
        let Some(change_id) = data.get("change_id").and_then(Value::as_u64) else {
            return MdEvent::Ignored;
        };
        let (bids, asks) = (book_levels(data.get("bids")), book_levels(data.get("asks")));
        return match data.get("type").and_then(Value::as_str) {
            Some("snapshot") => {
                book.apply_snapshot(change_id, &bids, &asks);
                MdEvent::BookUpdated
            }
            Some("change") => {
                // Stale FIRST (binance's apply_depth_event ordering — see the module doc), then
                // the chain check, then the fold.
                if change_id <= book.last_seq {
                    MdEvent::Ignored // already reflected — book stays trustworthy
                } else if data.get("prev_change_id").and_then(Value::as_u64) != Some(book.last_seq)
                {
                    MdEvent::Resync // chain broken — frames dropped, do NOT fold
                } else if book.apply_delta(change_id, &bids, &asks) {
                    MdEvent::BookUpdated
                } else {
                    MdEvent::Ignored
                }
            }
            _ => MdEvent::Ignored,
        };
    }
    let rows = parse_trades(&v);
    if !rows.is_empty() {
        return MdEvent::Trades(rows);
    }
    parse_quote(&v).map_or(MdEvent::Ignored, MdEvent::Quote)
}

#[path = "market_data_tests.rs"]
#[cfg(test)]
mod market_data_tests;
