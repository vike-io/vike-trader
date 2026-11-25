//! How `data realtime` renders — one live frame as a JSON row or a table line.

use vike_datahub_client::market::MarketDataFrame;
use vike_datahub_client::{BookSnapshot, MarketDataBye};
use vike_marketdata::TickTrade;
use vike_marketdata::aggressor::{TickAggressor, classify};
use vike_model::StreamStatus;
use vike_model::time::epoch_ms_to_utc_timestamp;

use super::Render;

/// A [`Render`] as the operator spells it — used by the one refusal that has to name the value it
/// was handed back.
pub(super) fn render_word(render: Render) -> &'static str {
    match render {
        Render::Table => "table",
        Render::Jsonl => "jsonl",
        Render::Json => "json",
    }
}

// ─── rendering one frame ─────────────────────────────────────────────────────────────────────────

/// One frame as a JSON object — `watch`'s machine form, one per line.
///
/// Every row carries `type`, including the heartbeat, so a consumer filters on a field rather than
/// on a shape. ⚠ The heartbeat IS rendered: a stream that emitted only data would be one a consumer
/// cannot tell from a stalled one, which is the exact thing the heartbeat exists to answer.
pub(super) fn jsonl_row(frame: &MarketDataFrame) -> String {
    let value = match frame {
        MarketDataFrame::Depth(snap) => book_json("depth", snap),
        MarketDataFrame::Book(snap) => book_json("book", snap),
        MarketDataFrame::Quote(q) => serde_json::json!({
            "type": "quote",
            "venue": q.venue,
            "symbol": q.symbol,
            "bid": q.bid,
            "ask": q.ask,
            "bid_size": q.bid_size,
            "ask_size": q.ask_size,
            "sequence": q.sequence,
            "venue_timestamp": q.venue_timestamp,
            "age_ms": q.age_ms,
            // The label's own word, absent when the stream never said one.
            "delay": q.delay.map(vike_model::StreamDelay::word),
        }),
        MarketDataFrame::Trades { venue, symbol, ticks, sequence: seq } => {
            // ⚠ **The ticks are re-stamped from the ENVELOPE and never serialized raw.**
            // `MarketDataFrame::Trades`' own doc: every tick in the batch carries an EMPTY `symbol`, and
            // this variant's `symbol` field is authoritative for all of them. A row built by
            // serializing `TickTrade` would publish `"symbol": ""` on every print — the mis-key that
            // doc warns about, wearing a JSON field an operator would group by.
            let prints: Vec<serde_json::Value> = ticks
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "timestamp": t.timestamp,
                        "local_timestamp": t.local_timestamp,
                        "price": t.price,
                        "size": t.size,
                        // The model's OWN field name, carried raw rather than as a `side` word: a
                        // consumer re-derives the aggressor (see `aggressor`) instead of trusting
                        // this side's reading of a flag.
                        "is_buyer_maker": t.is_buyer_maker,
                    })
                })
                .collect();
            serde_json::json!({
                "type": "trades",
                "venue": venue,
                "symbol": symbol,
                "sequence": seq,
                "count": prints.len(),
                "prints": prints,
            })
        }
        MarketDataFrame::Status { venue, symbol, lane, status } => {
            let mut row = serde_json::json!({
                "type": "status",
                "venue": venue,
                "symbol": symbol,
                "lane": lane.word(),
            });
            // ⚠ FLATTENED rather than serialized through `StreamStatus`' own serde, and
            // deliberately: that enum rides the wire externally tagged (`{"Live":{…}}`), which is the
            // datahub wire's spelling, not this verb's output contract, and is hostile to `jq`. The state word is a stable
            // machine token — `crate::cmd::data::catalog`'s `ServerView::state` sets the precedent —
            // and every number the verdict rests on rides beside it.
            let (state, at, newest, now) = match status {
                StreamStatus::GapStart { at_timestamp_ms: at_ts_ms } => {
                    ("gap_start", Some(*at_ts_ms), None, None)
                }
                StreamStatus::Live { gap_started_timestamp_ms: gap_started_ts_ms } => {
                    ("live", *gap_started_ts_ms, None, None)
                }
                StreamStatus::Stale { newest_data_timestamp_ms: newest_data_ts_ms, now_ms } => {
                    ("stale", None, Some(*newest_data_ts_ms), Some(*now_ms))
                }
            };
            row["state"] = serde_json::json!(state);
            row["episode_ts"] = serde_json::json!(at);
            row["newest_data_ts"] = serde_json::json!(newest);
            row["judged_at_ts"] = serde_json::json!(now);
            row
        }
        MarketDataFrame::TapeGap {
            venue,
            symbol,
            dropped,
            from_sequence: from_seq,
            to_sequence: to_seq,
        } => {
            serde_json::json!({
                "type": "tape_gap",
                "venue": venue,
                "symbol": symbol,
                // ⚠ AUTHORITATIVE for how much was lost; the range says FROM WHERE. Several holes merge
                // into one marker, so `dropped` can exceed what the range appears to cover —
                // `MarketDataFrame::TapeGap`'s own doc carries the rule and this field order follows it.
                "dropped": dropped,
                "from_sequence": from_seq,
                "to_sequence": to_seq,
            })
        }
        MarketDataFrame::Heartbeat => serde_json::json!({ "type": "heartbeat" }),
        MarketDataFrame::Bye(why) => {
            let mut row = serde_json::json!({ "type": "bye", "reason": bye_token(*why) });
            if let MarketDataBye::TooSlow { lapses } = why {
                row["lapses"] = serde_json::json!(lapses);
            }
            row
        }
    };
    serde_json::to_string(&value)
        .expect("a tree of strings, numbers and bools; serialization is total")
}

/// A book/depth frame as a row.
///
/// The levels ride as the MODEL's own shape — `vike_marketdata::L2BookLevel` serializes as a two-element
/// `[price, qty]` array through its own `#[serde(into)]`, which is what the journal has always
/// written — rather than as a shape invented here.
///
/// ⚠ **Best-first is a CONTRACT, not an accident**: `BookSnapshot`'s doc states bids DESCEND and asks
/// ASCEND, and nothing here re-sorts. A renderer that "tidied" the order would paint every DOM
/// ladder upside down, which no round-trip test would catch.
fn book_json(kind: &str, snap: &BookSnapshot) -> serde_json::Value {
    serde_json::json!({
        "type": kind,
        "venue": snap.venue,
        "symbol": snap.symbol,
        "tick_size": snap.tick_size,
        "bids": snap.bids,
        "asks": snap.asks,
        // The WIRE sequence — strictly +1 per frame for this key, so a jump means this connection
        // did not receive frames the server produced.
        "sequence": snap.sequence,
        // ⚠ DIAGNOSTIC ONLY, both of them: the publisher conflates on a cadence, so consecutive
        // frames legitimately skip venue sequence numbers, and the stamp is a different clock on
        // each lane (the venue's on `depth`, the hub's receipt on `book`).
        "venue_sequence": snap.venue_sequence,
        "venue_timestamp": snap.venue_timestamp,
    })
}

/// One frame as a line for a person.
///
/// ⚠ A book frame is SUMMARISED here (top of book plus the level counts) while [`jsonl_row`] carries
/// every level. That is the split the two renderings are for: 200 levels a side is the DATA and it
/// is unreadable as a terminal line, so the machine form keeps it and the human form keeps what a
/// person watching a ladder actually reads.
pub(super) fn table_line(frame: &MarketDataFrame) -> String {
    match frame {
        MarketDataFrame::Depth(snap) => book_line("depth", snap),
        MarketDataFrame::Book(snap) => book_line("book", snap),
        MarketDataFrame::Quote(q) => format!(
            "quote {} {} seq={} bid {} | ask {} ({} ms old{})",
            q.venue,
            q.symbol,
            q.sequence,
            q.bid,
            q.ask,
            q.age_ms,
            q.delay.map_or(String::new(), |d| format!(", {}", d.word()))
        ),
        MarketDataFrame::Trades { venue, symbol, ticks, sequence: seq } => {
            let prints: Vec<String> = ticks
                .iter()
                // The tick's own `symbol` is EMPTY on this wire — see `jsonl_row` — so nothing here
                // reads it; the envelope above is the authority.
                .map(|t| {
                    format!(
                        "{} {} x {} @{}",
                        aggressor(t),
                        t.price,
                        t.size,
                        epoch_ms_to_utc_timestamp(t.timestamp)
                    )
                })
                .collect();
            format!(
                "trades {venue} {symbol} seq={seq} ({} print(s)) {}",
                prints.len(),
                prints.join(" | ")
            )
        }
        MarketDataFrame::Status { venue, symbol, lane, status } => {
            format!("status {venue} {symbol} {}: {}", lane.word(), status_sentence(status))
        }
        MarketDataFrame::TapeGap {
            venue,
            symbol,
            dropped,
            from_sequence: from_seq,
            to_sequence: to_seq,
        } => format!(
            "⚠ TAPE GAP {venue} {symbol}: {dropped} print(s) LOST between seq {from_seq} and \
             {to_seq}"
        ),
        MarketDataFrame::Heartbeat => {
            "heartbeat — the link is alive and the key is quiet".to_string()
        }
        MarketDataFrame::Bye(why) => format!("bye — {}", bye_sentence(*why)),
    }
}

/// A book/depth frame as a line: the top of book, then how deep the frame actually was.
fn book_line(kind: &str, snap: &BookSnapshot) -> String {
    let side = |levels: &[vike_marketdata::L2BookLevel]| match levels.first() {
        Some(l) => format!("{} x {}", l.price, l.qty),
        None => "-".to_string(),
    };
    format!(
        "{kind} {} {} seq={} bid {} | ask {} ({}x{} levels)",
        snap.venue,
        snap.symbol,
        snap.sequence,
        side(&snap.bids),
        side(&snap.asks),
        snap.bids.len(),
        snap.asks.len()
    )
}

/// Which side CROSSED the spread, as the word a table line prints.
///
/// `TickTrade::is_buyer_maker` says the BUYER was resting, so the taker was the SELLER. The side is
/// decided by [`classify`] — the workspace's one reading of that flag, the same one every orderflow
/// builder routes by — and this only spells it, because inverting it is a silent error: a
/// footprint reading `buy` for every sell prints a chart that is precisely wrong and never empty.
/// [`jsonl_row`] deliberately carries the raw flag instead, so a machine reader re-derives this
/// rather than trusting it.
pub(super) fn aggressor(t: &TickTrade) -> &'static str {
    match classify(t) {
        TickAggressor::Buy => "buy",
        TickAggressor::Sell => "sell",
    }
}

/// A stream-status disclosure in an operator's words. Exhaustive, no `_` arm: a new
/// [`StreamStatus`] variant must be given a sentence rather than inheriting one.
pub(super) fn status_sentence(status: &StreamStatus) -> String {
    match status {
        StreamStatus::GapStart { at_timestamp_ms: at_ts_ms } => format!(
            "the feed can no longer be trusted from {} — a transport fault, a server close or an \
             idle watchdog",
            epoch_ms_to_utc_timestamp(*at_ts_ms)
        ),
        StreamStatus::Live { gap_started_timestamp_ms: Some(from) } => format!(
            "live again, closing the gap that opened at {}",
            epoch_ms_to_utc_timestamp(*from)
        ),
        StreamStatus::Live { gap_started_timestamp_ms: None } => "live".to_string(),
        StreamStatus::Stale { newest_data_timestamp_ms: newest_data_ts_ms, now_ms } => format!(
            "STALE — the transport is alive and the newest data is from {}, judged at {}. This is \
             the silently-failed re-subscribe a socket watchdog cannot see",
            epoch_ms_to_utc_timestamp(*newest_data_ts_ms),
            epoch_ms_to_utc_timestamp(*now_ms)
        ),
    }
}

/// A goodbye as a stable machine token. Exhaustive, no `_` arm.
pub(super) fn bye_token(why: MarketDataBye) -> &'static str {
    match why {
        MarketDataBye::TooSlow { .. } => "too_slow",
        MarketDataBye::ServerStopping => "server_stopping",
        MarketDataBye::SessionIdle => "session_idle",
        MarketDataBye::ControlLaneOverflow => "control_lane_overflow",
    }
}

/// A goodbye in an operator's words. ⚠ `too_slow` and `session_idle` are OPPOSITE diagnoses — asking
/// for more than you could read, versus asking for nothing — and [`MarketDataBye`]'s own doc records what
/// sending the wrong one cost, so each keeps its own sentence.
pub(super) fn bye_sentence(why: MarketDataBye) -> String {
    match why {
        MarketDataBye::TooSlow { lapses } => format!(
            "this reader could not keep up ({lapses} lapses). The server stops serving a client it \
             would otherwise be lying to more slowly — read faster, narrow the lane, or take \
             `--format jsonl` into a file rather than a terminal"
        ),
        MarketDataBye::ServerStopping => "the datahub is shutting down".to_string(),
        MarketDataBye::SessionIdle => {
            "the session held no subscriptions for long enough that the socket bought nothing"
                .to_string()
        }
        MarketDataBye::ControlLaneOverflow => {
            "the reserved CONTROL lane overflowed — this reader could not absorb even the status \
             frames its own keys produced, which is a slow consumer rather than an idle one"
                .to_string()
        }
    }
}
