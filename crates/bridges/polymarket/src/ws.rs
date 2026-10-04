//! Polymarket CLOB **market** channel protocol (public, unauthenticated): the subscribe frame, a
//! pure event decoder, and per-`token_id` book maintenance. Transport-free by design — like the
//! user-channel protocol half (`user_ws`), the pure decode/maintain is what the adapter owns; a
//! driver (app `marketfeed` or a follow-up runner) supplies the socket. The market channel carries
//! book snapshots + incremental `price_change` deltas + last-trade prints.

use std::collections::HashMap;
use vike_model::BookLevel;

use super::data::{PolyBook, parse_book};

/// A single ladder change from a `price_change` event.
#[derive(Debug, Clone, PartialEq)]
pub struct LevelChange {
    pub price: f64,
    pub size: f64,
    /// true = bid side (Polymarket `side: "BUY"`), false = ask (`"SELL"`).
    pub is_bid: bool,
}

/// A decoded market-channel event. Every variant carries `ts: Option<i64>` — the frame's wire
/// `timestamp` (ms since epoch) when present, `None` when the frame omits it (see [`frame_ts`]).
/// For `Book`, `ts` rides the wrapper alongside the [`PolyBook`] rather than being forced into
/// `PolyBook` itself (that type is also used by the plain REST `/book` fetch, which has no
/// per-event ts).
#[derive(Debug, Clone, PartialEq)]
pub enum MarketUpdate {
    /// full book snapshot (replaces state for its token).
    Book { book: PolyBook, ts: Option<i64> },
    /// incremental ladder deltas for one token.
    PriceChange { asset_id: String, changes: Vec<LevelChange>, ts: Option<i64> },
    /// last traded price print (no book change). `size` defaults to 0.0 when the frame omits
    /// it; `taker_is_buy` is `Some(true)`/`Some(false)` for an explicit `"BUY"`/`"SELL"` `side`
    /// field, `None` when the frame carries no side at all.
    LastTrade {
        asset_id: String,
        price: f64,
        size: f64,
        taker_is_buy: Option<bool>,
        ts: Option<i64>,
    },
}

/// The subscribe frame for the market channel: `{"assets_ids":[...],"type":"market"}`.
pub fn subscribe_message(assets: &[String]) -> String {
    serde_json::json!({ "assets_ids": assets, "type": "market" }).to_string()
}

/// Extract the frame-level wire timestamp (ms since epoch) when present. Polymarket market-channel
/// frames carry a top-level `timestamp` field as a string of epoch-ms — **confirmed against a live
/// capture (2026-07-08)** for all three market-channel frame types (`book`, `price_change`,
/// `last_trade_price`). The bare-number JSON form is also accepted defensively (no live frame has
/// ever shown it, but accepting it costs nothing and matches the user-channel decoder's tolerance).
/// `None` when the frame omits the field entirely; the caller then falls back to local receive time
/// (`market_feed`'s module doc documents that fallback).
fn frame_ts(ev: &serde_json::Value) -> Option<i64> {
    ev.get("timestamp")
        .and_then(|t| t.as_str())
        .and_then(|s| s.parse::<i64>().ok())
        .or_else(|| ev.get("timestamp").and_then(|t| t.as_i64()))
}

/// Parse one entry of a `price_changes` array into `(asset_id, LevelChange)`. Returns `None` when
/// a required field is missing/malformed (the entry is dropped, not the whole frame).
fn parse_price_change_entry(c: &serde_json::Value) -> Option<(String, LevelChange)> {
    let asset_id = c.get("asset_id").and_then(|a| a.as_str())?.to_string();
    let price = c.get("price").and_then(|x| x.as_str())?.parse().ok()?;
    let size = c.get("size").and_then(|x| x.as_str())?.parse().ok()?;
    let is_bid = c.get("side").and_then(|s| s.as_str()) == Some("BUY");
    Some((asset_id, LevelChange { price, size, is_bid }))
}

/// Decode one market-channel event object into zero or more [`MarketUpdate`]s. `book` and
/// `last_trade_price` are single-asset (one event → one update); the real `price_change` frame is
/// **multi-asset** — its `price_changes` array carries one entry per distinct `asset_id`, so a
/// single frame decodes to one `MarketUpdate::PriceChange` PER asset (grouped, preserving each
/// asset's first-seen order), each stamped with the frame's shared top-level `ts`. Unknown event
/// types decode to nothing.
fn decode_one(ev: &serde_json::Value) -> Vec<MarketUpdate> {
    let asset_id = || ev.get("asset_id").and_then(|a| a.as_str()).unwrap_or_default().to_string();
    let ts = frame_ts(ev);
    let Some(event_type) = ev.get("event_type").and_then(|e| e.as_str()) else {
        return Vec::new();
    };
    match event_type {
        // Follow-up (not this fix): the real `book` frame also embeds `tick_size` and
        // `last_trade_price` — a future change could read `tick_size` from here instead of the
        // separate `fetch_token_tick_size` REST call, keeping the REST lookup as a fallback for
        // when a delta arrives before the first snapshot.
        "book" => vec![MarketUpdate::Book { book: parse_book(ev), ts }],
        "price_change" => {
            // Group by asset_id, preserving first-seen order (small per-frame N — linear scan is
            // simplest and avoids pulling in IndexMap for a handful of entries).
            let mut by_asset: Vec<(String, Vec<LevelChange>)> = Vec::new();
            if let Some(arr) = ev.get("price_changes").and_then(|c| c.as_array()) {
                for c in arr {
                    let Some((asset, change)) = parse_price_change_entry(c) else { continue };
                    match by_asset.iter_mut().find(|(a, _)| *a == asset) {
                        Some((_, changes)) => changes.push(change),
                        None => by_asset.push((asset, vec![change])),
                    }
                }
            }
            by_asset
                .into_iter()
                .map(|(asset_id, changes)| MarketUpdate::PriceChange { asset_id, changes, ts })
                .collect()
        }
        "last_trade_price" => {
            let Some(price) = ev.get("price").and_then(|p| p.as_str()).and_then(|s| s.parse().ok())
            else {
                return Vec::new();
            };
            let size =
                ev.get("size").and_then(|s| s.as_str()).and_then(|s| s.parse().ok()).unwrap_or(0.0);
            let taker_is_buy = match ev.get("side").and_then(|s| s.as_str()) {
                Some("BUY") => Some(true),
                Some("SELL") => Some(false),
                _ => None,
            };
            vec![MarketUpdate::LastTrade { asset_id: asset_id(), price, size, taker_is_buy, ts }]
        }
        _ => Vec::new(),
    }
}

/// Decode a market-channel frame. The channel may deliver a single event object OR an array of
/// them; unknown event types are dropped. A single `price_change` event yields one update per
/// distinct asset (see [`decode_one`]), so this is a `flat_map`, not a 1:1 `filter_map`.
pub fn decode_market(frame: &serde_json::Value) -> Vec<MarketUpdate> {
    match frame {
        serde_json::Value::Array(arr) => arr.iter().flat_map(decode_one).collect(),
        obj => decode_one(obj),
    }
}

/// Apply an update to a per-`token_id` book map (snapshot replaces; price_change patches a level,
/// size 0 removes it; last-trade is a no-op).
pub fn apply_update(books: &mut HashMap<String, PolyBook>, up: &MarketUpdate) {
    match up {
        MarketUpdate::Book { book, .. } => {
            books.insert(book.asset_id.clone(), book.clone());
        }
        MarketUpdate::PriceChange { asset_id, changes, .. } => {
            let book = books.entry(asset_id.clone()).or_insert_with(|| PolyBook {
                asset_id: asset_id.clone(),
                bids: Vec::new(),
                asks: Vec::new(),
            });
            for ch in changes {
                let side = if ch.is_bid { &mut book.bids } else { &mut book.asks };
                side.retain(|l| (l.price - ch.price).abs() > f64::EPSILON);
                if ch.size > 0.0 {
                    side.push(BookLevel { price: ch.price, qty: ch.size });
                }
            }
        }
        MarketUpdate::LastTrade { .. } => {}
    }
}

#[path = "ws_tests.rs"]
#[cfg(test)]
mod ws_tests;
