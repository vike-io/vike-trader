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
use vike_model::{Bar, L2Book, Level, QuoteTick, TradeTick};

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
/// `Level`s, with `delete` forced to qty `0.0` (the [`L2Book`] remove convention). A malformed
/// entry is skipped, never fatal.
fn book_levels(side: Option<&Value>) -> Vec<Level> {
    side.and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|lvl| {
                    let l = lvl.as_array()?;
                    let action = l.first()?.as_str()?;
                    let px = l.get(1)?.as_f64()?;
                    let qty = if action == "delete" { 0.0 } else { l.get(2)?.as_f64()? };
                    Some((px, qty))
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
/// [`vike_bybit::market_data::parse_orderbook_snapshot`] role.
pub fn parse_book_snapshot(frame: &Value) -> Option<(u64, Vec<Level>, Vec<Level>)> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A wire-faithful `book.*` SNAPSHOT frame (documented shape: `[action, price, amount]`
    /// triplets as JSON NUMBERS, `change_id` the anchor).
    fn snapshot_frame(change_id: u64, bids: &[(f64, f64)], asks: &[(f64, f64)]) -> String {
        json!({
            "jsonrpc": "2.0", "method": "subscription",
            "params": {"channel": "book.BTC-PERPETUAL.100ms", "data": {
                "type": "snapshot", "timestamp": 1_700_000_000_000i64,
                "instrument_name": "BTC-PERPETUAL", "change_id": change_id,
                "bids": bids.iter().map(|&(p, q)| json!(["new", p, q])).collect::<Vec<_>>(),
                "asks": asks.iter().map(|&(p, q)| json!(["new", p, q])).collect::<Vec<_>>(),
            }}
        })
        .to_string()
    }

    /// A wire-faithful `book.*` CHANGE frame (`prev_change_id` chains to the previous frame).
    fn change_frame(prev: u64, change_id: u64, bids: &[serde_json::Value]) -> String {
        json!({
            "jsonrpc": "2.0", "method": "subscription",
            "params": {"channel": "book.BTC-PERPETUAL.100ms", "data": {
                "type": "change", "timestamp": 1_700_000_000_100i64,
                "instrument_name": "BTC-PERPETUAL",
                "prev_change_id": prev, "change_id": change_id,
                "bids": bids, "asks": [],
            }}
        })
        .to_string()
    }

    #[test]
    fn channel_builders_are_verbatim() {
        assert_eq!(book_channel("BTC-PERPETUAL"), "book.BTC-PERPETUAL.100ms");
        assert_eq!(trades_channel("BTC-PERPETUAL"), "trades.BTC-PERPETUAL.100ms");
        assert_eq!(quote_channel("ETH-PERPETUAL"), "quote.ETH-PERPETUAL");
        assert_eq!(chart_channel("BTC-PERPETUAL", "1"), "chart.trades.BTC-PERPETUAL.1");
    }

    #[test]
    fn subscribe_frame_is_a_public_subscribe_rpc() {
        let sub = public_subscribe_frame(&["book.BTC-PERPETUAL.100ms".to_string()]);
        let v: Value = serde_json::from_str(&sub).unwrap();
        assert_eq!(v["method"], "public/subscribe");
        assert_eq!(v["params"]["channels"][0], "book.BTC-PERPETUAL.100ms");
    }

    // ── book: the DeltaSync chain ───────────────────────────────────────────────────────────────

    #[test]
    fn snapshot_seeds_the_book_at_change_id() {
        let mut book = L2Book::new(0.5);
        let ev = route_frame(
            &snapshot_frame(297_000, &[(60_000.0, 5.0), (59_999.5, 2.0)], &[(60_000.5, 4.0)]),
            "BTC-PERPETUAL",
            &mut book,
        );
        assert_eq!(ev, MdEvent::BookUpdated);
        assert_eq!(book.last_seq, 297_000);
        assert_eq!(book.best_bid(), Some((60_000.0, 5.0)));
        assert_eq!(book.best_ask(), Some((60_000.5, 4.0)));
    }

    #[test]
    fn a_chained_change_folds_and_advances_the_anchor() {
        let mut book = L2Book::new(0.5);
        route_frame(&snapshot_frame(100, &[(60_000.0, 5.0)], &[(60_000.5, 4.0)]), "s", &mut book);
        // change_id values JUMP (venue-global) — only prev_change_id must chain.
        let ev = route_frame(
            &change_frame(100, 137, &[json!(["change", 60_000.0, 8.0])]),
            "s",
            &mut book,
        );
        assert_eq!(ev, MdEvent::BookUpdated);
        assert_eq!(book.last_seq, 137);
        assert_eq!(book.best_bid(), Some((60_000.0, 8.0)));
    }

    #[test]
    fn a_delete_action_removes_the_level() {
        let mut book = L2Book::new(0.5);
        route_frame(
            &snapshot_frame(100, &[(60_000.0, 5.0), (59_999.5, 2.0)], &[(60_000.5, 4.0)]),
            "s",
            &mut book,
        );
        // The documented delete shape: amount 0 on the wire; the decoder forces 0.0 regardless.
        let ev = route_frame(
            &change_frame(100, 101, &[json!(["delete", 60_000.0, 0.0])]),
            "s",
            &mut book,
        );
        assert_eq!(ev, MdEvent::BookUpdated);
        assert_eq!(book.best_bid(), Some((59_999.5, 2.0)), "the deleted top level is gone");
    }

    #[test]
    fn a_replayed_change_is_ignored_and_the_book_untouched() {
        let mut book = L2Book::new(0.5);
        route_frame(&snapshot_frame(100, &[(60_000.0, 5.0)], &[(60_000.5, 4.0)]), "s", &mut book);
        route_frame(&change_frame(100, 137, &[json!(["change", 60_000.0, 8.0])]), "s", &mut book);
        // The SAME frame again: change_id already reflected → Ignored (NOT Resync, even though its
        // prev_change_id no longer matches the advanced anchor — the check-order rule).
        let ev = route_frame(
            &change_frame(100, 137, &[json!(["change", 60_000.0, 999.0])]),
            "s",
            &mut book,
        );
        assert_eq!(ev, MdEvent::Ignored);
        assert_eq!(book.last_seq, 137, "anchor unmoved");
        assert_eq!(book.best_bid(), Some((60_000.0, 8.0)), "book unmoved");
    }

    #[test]
    fn a_broken_chain_answers_resync_and_does_not_fold() {
        let mut book = L2Book::new(0.5);
        route_frame(&snapshot_frame(100, &[(60_000.0, 5.0)], &[(60_000.5, 4.0)]), "s", &mut book);
        // prev_change_id 104 != anchor 100 — the venue dropped a message between them.
        let ev = route_frame(
            &change_frame(104, 105, &[json!(["change", 60_000.0, 999.0])]),
            "s",
            &mut book,
        );
        assert_eq!(ev, MdEvent::Resync);
        assert_eq!(book.last_seq, 100, "anchor unmoved across the gap");
        assert_eq!(book.best_bid(), Some((60_000.0, 5.0)), "the gapped delta was NOT folded");
    }

    #[test]
    fn a_post_gap_snapshot_reseeds_cleanly() {
        let mut book = L2Book::new(0.5);
        route_frame(&snapshot_frame(100, &[(60_000.0, 5.0)], &[(60_000.5, 4.0)]), "s", &mut book);
        route_frame(&change_frame(104, 105, &[]), "s", &mut book); // gap
        let ev = route_frame(
            &snapshot_frame(300, &[(60_000.5, 3.0)], &[(60_001.0, 4.0)]),
            "s",
            &mut book,
        );
        assert_eq!(ev, MdEvent::BookUpdated);
        assert_eq!(book.last_seq, 300);
        assert_eq!(book.best_bid(), Some((60_000.5, 3.0)));
    }

    #[test]
    fn parse_book_snapshot_answers_only_snapshots() {
        let frame = snapshot_frame(297_000, &[(60_000.0, 5.0)], &[(60_000.5, 4.0)]);
        let (seq, bids, asks) =
            parse_book_snapshot(&serde_json::from_str(&frame).unwrap()).unwrap();
        assert_eq!(seq, 297_000);
        assert_eq!(bids, vec![(60_000.0, 5.0)]);
        assert_eq!(asks, vec![(60_000.5, 4.0)]);
        let change = change_frame(297_000, 297_001, &[]);
        assert!(parse_book_snapshot(&serde_json::from_str(&change).unwrap()).is_none());
    }

    // ── trades ──────────────────────────────────────────────────────────────────────────────────

    /// Documented `trades.*` shape: an ARRAY of rows; `direction` is the TAKER side; `amount` the
    /// venue contract unit (USD on the inverse perp).
    const TRADES_FRAME: &str = r#"{"jsonrpc":"2.0","method":"subscription","params":{"channel":"trades.BTC-PERPETUAL.100ms","data":[{"trade_seq":30289432,"trade_id":"48079254","timestamp":1590484512188,"tick_direction":2,"price":8950.0,"mark_price":8948.9,"instrument_name":"BTC-PERPETUAL","index_price":8955.88,"direction":"sell","amount":10.0},{"trade_seq":30289433,"trade_id":"48079255","timestamp":1590484512188,"tick_direction":2,"price":8949.5,"mark_price":8948.9,"instrument_name":"BTC-PERPETUAL","index_price":8955.88,"direction":"buy","amount":20.0}]}}"#;

    #[test]
    fn trades_frame_decodes_every_row_with_the_taker_side_mapping() {
        let mut book = L2Book::new(0.5);
        let MdEvent::Trades(rows) = route_frame(TRADES_FRAME, "BTC-PERPETUAL", &mut book) else {
            panic!("expected Trades");
        };
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].price, 8950.0);
        assert_eq!(rows[0].size, 10.0, "amount verbatim — venue contract units");
        assert!(rows[0].is_buyer_maker, "taker sold → buyer was the maker");
        assert!(!rows[1].is_buyer_maker, "taker bought");
        assert_eq!(rows[0].ts, 1_590_484_512_188);
        assert_eq!(rows[0].symbol, "BTC-PERPETUAL");
        assert_eq!(rows[0].local_ts, 0, "receive stamp is the pump's job");
    }

    #[test]
    fn a_malformed_trade_row_is_skipped_not_fatal() {
        let frame = json!({
            "jsonrpc": "2.0", "method": "subscription",
            "params": {"channel": "trades.BTC-PERPETUAL.100ms", "data": [
                {"instrument_name": "BTC-PERPETUAL", "price": 8950.0, "amount": 10.0,
                 "direction": "sell", "timestamp": 1i64},
                {"instrument_name": "BTC-PERPETUAL", "price": 0.0, "amount": 10.0,
                 "direction": "sell", "timestamp": 2i64},
                {"instrument_name": "BTC-PERPETUAL", "price": 8950.0, "amount": 10.0,
                 "direction": "??", "timestamp": 3i64}
            ]}
        })
        .to_string();
        let mut book = L2Book::new(0.5);
        let MdEvent::Trades(rows) = route_frame(&frame, "s", &mut book) else {
            panic!("expected Trades");
        };
        assert_eq!(rows.len(), 1, "zero-price and unknown-direction rows dropped in place");
    }

    // ── quotes ──────────────────────────────────────────────────────────────────────────────────

    /// Documented `quote.*` shape: a SINGLE object, both sides + amounts, `timestamp` epoch-ms.
    const QUOTE_FRAME: &str = r#"{"jsonrpc":"2.0","method":"subscription","params":{"channel":"quote.BTC-PERPETUAL","data":{"timestamp":1550658624149,"instrument_name":"BTC-PERPETUAL","best_bid_price":3914.97,"best_bid_amount":40.0,"best_ask_price":3915.5,"best_ask_amount":50.0}}}"#;

    #[test]
    fn quote_frame_decodes_to_a_two_sided_quote() {
        let mut book = L2Book::new(0.5);
        let MdEvent::Quote(q) = route_frame(QUOTE_FRAME, "BTC-PERPETUAL", &mut book) else {
            panic!("expected Quote");
        };
        assert_eq!((q.bid, q.bid_size, q.ask, q.ask_size), (3914.97, 40.0, 3915.5, 50.0));
        assert_eq!(q.ts, 1_550_658_624_149);
        assert_eq!(q.symbol, "BTC-PERPETUAL");
        assert_eq!(q.local_ts, 0);
    }

    #[test]
    fn a_one_sided_quote_is_ignored_never_fabricated() {
        let frame = json!({
            "jsonrpc": "2.0", "method": "subscription",
            "params": {"channel": "quote.BTC-PERPETUAL", "data": {
                "timestamp": 1i64, "instrument_name": "BTC-PERPETUAL",
                "best_bid_price": 3914.97, "best_bid_amount": 40.0
            }}
        })
        .to_string();
        let mut book = L2Book::new(0.5);
        assert_eq!(route_frame(&frame, "s", &mut book), MdEvent::Ignored);
    }

    // ── chart bars ──────────────────────────────────────────────────────────────────────────────

    /// Documented `chart.trades.*` shape: `tick` = bar-open ms, `volume` BASE units, `cost` quote
    /// notional (unread — the REST reader's column choice, pinned here too).
    const CHART_FRAME: &str = r#"{"jsonrpc":"2.0","method":"subscription","params":{"channel":"chart.trades.BTC-PERPETUAL.1","data":{"volume":0.05219351,"tick":1573645080000,"open":8869.79,"low":8788.25,"high":8870.31,"cost":463.0,"close":8791.25}}}"#;

    #[test]
    fn chart_frame_decodes_volume_base_not_cost() {
        let bar = parse_chart_bar(&serde_json::from_str(CHART_FRAME).unwrap()).unwrap();
        assert_eq!(bar.ts, 1_573_645_080_000);
        assert_eq!((bar.open, bar.high, bar.low, bar.close), (8869.79, 8870.31, 8788.25, 8791.25));
        assert_eq!(bar.volume, 0.05219351, "volume = BASE units; cost (463.0) must NOT be read");
    }

    #[test]
    fn chart_parse_rejects_other_channels_and_replies() {
        assert!(parse_chart_bar(&serde_json::from_str::<Value>(QUOTE_FRAME).unwrap()).is_none());
        let reply = json!({"jsonrpc": "2.0", "id": 1, "result": ["chart.trades.BTC-PERPETUAL.1"]});
        assert!(parse_chart_bar(&reply).is_none());
    }

    // ── the JSON-RPC reply lane ─────────────────────────────────────────────────────────────────

    #[test]
    fn rpc_replies_classify_ack_error_empty_and_keepalive() {
        let ack = json!({"jsonrpc": "2.0", "id": 1, "result": ["book.BTC-PERPETUAL.100ms"]});
        assert_eq!(classify_rpc_reply(&ack), RpcReply::Ack);
        let empty = json!({"jsonrpc": "2.0", "id": 1, "result": []});
        assert!(
            matches!(classify_rpc_reply(&empty), RpcReply::Error(_)),
            "an empty subscribe result is a dead subscription, not a success"
        );
        let err = json!({"jsonrpc": "2.0", "id": 1,
            "error": {"code": -32602, "message": "Invalid params"}});
        let RpcReply::Error(msg) = classify_rpc_reply(&err) else { panic!("expected Error") };
        assert!(msg.contains("-32602") && msg.contains("Invalid params"));
        // the public/test keepalive reply: a result OBJECT — activity, not an ack.
        let ping = json!({"jsonrpc": "2.0", "id": 9929, "result": {"version": "1.2.26"}});
        assert_eq!(classify_rpc_reply(&ping), RpcReply::Other);
        // a subscription notification is not a reply.
        let notif: Value = serde_json::from_str(QUOTE_FRAME).unwrap();
        assert_eq!(classify_rpc_reply(&notif), RpcReply::NotReply);
    }

    #[test]
    fn junk_and_unknown_channels_are_ignored() {
        let mut book = L2Book::new(0.5);
        assert_eq!(route_frame("not json", "s", &mut book), MdEvent::Ignored);
        assert_eq!(
            route_frame(
                r#"{"jsonrpc":"2.0","method":"subscription","params":{"channel":"deribit_price_index.btc_usd","data":{"price":60000.0}}}"#,
                "s",
                &mut book
            ),
            MdEvent::Ignored
        );
        // a reply frame through the book router: Ignored (the reply lane classifies it).
        assert_eq!(
            route_frame(r#"{"jsonrpc":"2.0","id":1,"result":["x"]}"#, "s", &mut book),
            MdEvent::Ignored
        );
    }
}
