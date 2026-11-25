//! The sink side of the kit: the twelve `DataClientSink` verbs as DATA (`vike_data::SinkCall`), a way to
//! emit one, and the line two runs are compared by.
//!
//! A twin test (the in-process path against the hop) wants the SAME events pushed into two sinks and the
//! two recordings compared. `SinkCall` is the workspace's one shared vocabulary for a recorded sink call
//! (`vike_data::RecordingSink` captures a stream of them), so a script is a `Vec<SinkCall>`, [`emit`]
//! plays one into any sink, and [`exact_line`] renders it for a comparison that cannot lose a bit.
//!
//! # Why `Debug`, not `==` and not `Display`
//!
//! `SinkCall`'s `Display` is a lossy summary (a bar prints its close and nothing else) and `f64`'s `==`
//! cannot tell `-0.0` from `0.0`. `{:?}` of an `f64` is the SHORTEST text that parses back to the same
//! bits, so two equal [`exact_line`]s are bit-equal payloads, signed zeros and subnormals included.
//!
//! # The canonical script
//!
//! [`canonical_script`] exercises every one of the twelve verbs for each symbol it is given, with
//! floats whose bit patterns are not short decimals. Each CONFLATING verb (`forming_bar`, `mark_tick`,
//! `bar_close_tick`, `l2_snapshot`) appears once per key: a second emission of the same slot may
//! legitimately be superseded before it reaches the wire, which is a property of the conflating lane,
//! not of the lane this script is meant to prove.

use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_data::{DataClientSink, RecordingSink, SinkCall};
use vike_marketdata::{
    Bar, L2Book, L2BookLevel, L2BookUpdate, L2BookUpdateKind, TickQuote, TickTrade,
};
use vike_model::{StreamDelay, StreamLabel, StreamStatus};

/// Play one recorded call into `sink`: the verb it names, with its payload (a clone of `call`, see
/// [`play`]).
pub fn emit(sink: &dyn DataClientSink, call: &SinkCall) {
    play(sink, call.clone());
}

/// [`emit`] that MOVES the call's payload into the sink, what a pump pays (the gatewayhub's
/// `tests/hop_latency.rs` times it, and a clone is not part of the hop). Exhaustive over `SinkCall`, so a
/// fourteenth verb fails to compile here until the kit can emit it.
#[inline]
pub fn play(sink: &dyn DataClientSink, call: SinkCall) {
    match call {
        SinkCall::SeedBars { venue, symbol, interval, bars } => {
            sink.seed_bars(&venue, &symbol, &interval, bars);
        }
        SinkCall::CloseBar { venue, symbol, interval, bar } => {
            sink.close_bar(&venue, &symbol, &interval, bar);
        }
        SinkCall::FormingBar { venue, symbol, interval, bar } => {
            sink.forming_bar(&venue, &symbol, &interval, bar);
        }
        SinkCall::MarkTick { venue, symbol, price: px, timestamp: ts } => {
            sink.mark_tick(&venue, &symbol, px, ts)
        }
        SinkCall::BarCloseTick { venue, symbol, price: px, timestamp: ts } => {
            sink.bar_close_tick(&venue, &symbol, px, ts);
        }
        SinkCall::L2Snapshot { venue, symbol, tick_size, bids, asks, timestamp: ts } => {
            sink.l2_snapshot(&venue, &symbol, tick_size, bids, asks, ts);
        }
        SinkCall::Quote { venue, symbol, quote } => sink.quote(&venue, &symbol, quote),
        SinkCall::Trade { venue, symbol, trade } => sink.trade(&venue, &symbol, trade),
        SinkCall::Book { venue, symbol, book } => sink.book(&venue, &symbol, Arc::new(book)),
        SinkCall::BookUpdate { venue, symbol, update } => {
            sink.book_update(&venue, &symbol, update);
        }
        SinkCall::StreamStatus { venue, symbol, stream, status } => {
            sink.stream_status(&venue, &symbol, &stream, status);
        }
        SinkCall::DataDelay { venue, symbol, stream, delay } => {
            sink.data_delay(&venue, &symbol, &stream, delay);
        }
        SinkCall::Funding { venue, symbol, funding } => sink.funding(&venue, &symbol, funding),
    }
}

/// Play `calls` into `sink`, in order.
pub fn emit_all(sink: &dyn DataClientSink, calls: &[SinkCall]) {
    for call in calls {
        emit(sink, call);
    }
}

/// The symbol a recorded call is for (the key a consumer files it under).
pub fn symbol_of(call: &SinkCall) -> &str {
    match call {
        SinkCall::SeedBars { symbol, .. }
        | SinkCall::CloseBar { symbol, .. }
        | SinkCall::FormingBar { symbol, .. }
        | SinkCall::MarkTick { symbol, .. }
        | SinkCall::BarCloseTick { symbol, .. }
        | SinkCall::L2Snapshot { symbol, .. }
        | SinkCall::Quote { symbol, .. }
        | SinkCall::Trade { symbol, .. }
        | SinkCall::Book { symbol, .. }
        | SinkCall::BookUpdate { symbol, .. }
        | SinkCall::StreamStatus { symbol, .. }
        | SinkCall::DataDelay { symbol, .. }
        | SinkCall::Funding { symbol, .. } => symbol,
    }
}

/// The verb a recorded call is, as the sink method's own name.
pub fn verb_of(call: &SinkCall) -> &'static str {
    match call {
        SinkCall::SeedBars { .. } => "seed_bars",
        SinkCall::CloseBar { .. } => "close_bar",
        SinkCall::FormingBar { .. } => "forming_bar",
        SinkCall::MarkTick { .. } => "mark_tick",
        SinkCall::BarCloseTick { .. } => "bar_close_tick",
        SinkCall::L2Snapshot { .. } => "l2_snapshot",
        SinkCall::Quote { .. } => "quote",
        SinkCall::Trade { .. } => "trade",
        SinkCall::Book { .. } => "book",
        SinkCall::BookUpdate { .. } => "book_update",
        SinkCall::StreamStatus { .. } => "stream_status",
        SinkCall::DataDelay { .. } => "data_delay",
        SinkCall::Funding { .. } => "funding",
    }
}

/// A recorded call as the line two recordings are compared by: its `Debug`. See the module doc for why
/// equal lines mean bit-equal payloads.
pub fn exact_line(call: &SinkCall) -> String {
    format!("{call:?}")
}

/// [`exact_line`] of every call of `calls` that is for `symbol`, in order.
pub fn exact_lines_of(calls: &[SinkCall], symbol: &str) -> Vec<String> {
    calls.iter().filter(|c| symbol_of(c) == symbol).map(exact_line).collect()
}

/// Block until `sink` has recorded at least `want` calls. Panics, saying what did arrive, after
/// `within`: a hop that lost a frame must fail the test, not hang it.
pub fn wait_for_calls(sink: &RecordingSink, want: usize, within: Duration) {
    let deadline = Instant::now() + within;
    loop {
        let have = sink.recorded();
        if have.len() >= want {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out after {within:?}: {} of {want} calls arrived; saw {:?}",
            have.len(),
            have.iter().map(|c| format!("{}:{}", verb_of(c), symbol_of(c))).collect::<Vec<_>>()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A bar whose every float has a non-trivial bit pattern (a signed zero, a subnormal-adjacent volume, a
/// third) so a lossy hop shows.
pub fn bar(ts: i64, x: f64) -> Bar {
    Bar {
        timestamp: ts,
        open: x,
        high: x + 1.5,
        low: -0.0,
        close: x / 3.0,
        volume: f64::MIN_POSITIVE,
        funding: Some(1e-4),
        bid: Some(x - 0.25),
        ask: None,
        symbol: None,
    }
}

/// An L1 quote at price `x`.
pub fn quote(symbol: &str, ts: i64, x: f64) -> TickQuote {
    TickQuote {
        timestamp: ts,
        local_timestamp: ts + 1,
        bid: x,
        ask: x + 0.5,
        bid_size: 1.25,
        ask_size: 2.5,
        symbol: symbol.into(),
    }
}

/// A trade print at price `x`.
pub fn trade(symbol: &str, ts: i64, x: f64) -> TickTrade {
    TickTrade {
        timestamp: ts,
        local_timestamp: ts + 1,
        price: x,
        size: 0.3,
        is_buyer_maker: ts % 2 == 0,
        symbol: symbol.into(),
    }
}

/// A raw book delta with sequence `seq` around price `x`.
pub fn delta(symbol: &str, seq: u64, x: f64) -> L2BookUpdate {
    L2BookUpdate {
        timestamp: seq as i64,
        local_timestamp: seq as i64 + 1,
        sequence: seq,
        kind: L2BookUpdateKind::Delta,
        tick_size: 0.5,
        bids: vec![L2BookLevel::new(x, 0.0)],
        asks: vec![L2BookLevel::new(x + 0.5, 1.5)],
        symbol: symbol.into(),
    }
}

/// Every one of the twelve `DataClientSink` verbs for each of `symbols`, the lossless ones several times,
/// each conflating verb once per key (module doc). Order: a seed per symbol, three rounds of the
/// lossless tick and bar verbs across the symbols, then each symbol's conflating verbs, folded book,
/// two status transitions and a delay label.
pub fn canonical_script(venue: &str, symbols: &[&str]) -> Vec<SinkCall> {
    let mut out = Vec::new();
    for (n, symbol_name) in symbols.iter().enumerate() {
        let salt = n as f64 * 1e-7;
        out.push(SinkCall::SeedBars {
            venue: venue.into(),
            symbol: (*symbol_name).into(),
            interval: "1m".into(),
            bars: vec![bar(0, 0.1 + 0.2 + salt), bar(60_000, 1.0 / 3.0 + salt)],
        });
    }
    for i in 0..3_i64 {
        for (n, symbol_name) in symbols.iter().enumerate() {
            let x = 0.1 * (i as f64 + 1.0) + n as f64 * 1e-7;
            out.push(SinkCall::Quote {
                venue: venue.into(),
                symbol: (*symbol_name).into(),
                quote: quote(symbol_name, i, x),
            });
            out.push(SinkCall::Trade {
                venue: venue.into(),
                symbol: (*symbol_name).into(),
                trade: trade(symbol_name, i, x),
            });
            out.push(SinkCall::CloseBar {
                venue: venue.into(),
                symbol: (*symbol_name).into(),
                interval: "1m".into(),
                bar: bar(60_000 * (i + 1), x),
            });
            out.push(SinkCall::BookUpdate {
                venue: venue.into(),
                symbol: (*symbol_name).into(),
                update: delta(symbol_name, i as u64 + 1, x),
            });
        }
    }
    for (n, symbol_name) in symbols.iter().enumerate() {
        let x = 100.0 + n as f64 * 1e-7;
        let (venue, symbol) = (venue.to_string(), (*symbol_name).to_string());
        out.push(SinkCall::FormingBar {
            venue: venue.clone(),
            symbol: symbol.clone(),
            interval: "1m".into(),
            bar: bar(240_000, x),
        });
        out.push(SinkCall::MarkTick {
            venue: venue.clone(),
            symbol: symbol.clone(),
            price: x + 0.5,
            timestamp: 7,
        });
        out.push(SinkCall::BarCloseTick {
            venue: venue.clone(),
            symbol: symbol.clone(),
            price: f64::MIN_POSITIVE + x,
            timestamp: 8,
        });
        out.push(SinkCall::L2Snapshot {
            venue: venue.clone(),
            symbol: symbol.clone(),
            tick_size: 0.5,
            bids: vec![L2BookLevel::new(x, 1.0), L2BookLevel::new(x - 0.5, 2.0)],
            asks: vec![L2BookLevel::new(x + 0.5, 1.5)],
            timestamp: 9,
        });
        let mut book = L2Book::new(0.5);
        book.apply_snapshot(5, &[L2BookLevel::new(x, 1.0)], &[L2BookLevel::new(x + 0.5, 1.0)]);
        out.push(SinkCall::Book { venue: venue.clone(), symbol: symbol.clone(), book });
        out.push(SinkCall::StreamStatus {
            venue: venue.clone(),
            symbol: symbol.clone(),
            stream: "1m".into(),
            status: StreamStatus::GapStart { at_timestamp_ms: 14 },
        });
        out.push(SinkCall::StreamStatus {
            venue: venue.clone(),
            symbol: symbol.clone(),
            stream: "1m".into(),
            status: StreamStatus::Live { gap_started_timestamp_ms: Some(14) },
        });
        out.push(SinkCall::DataDelay {
            venue,
            symbol,
            stream: StreamLabel::Quotes.word().into(),
            delay: StreamDelay::Delayed { lag_ms: Some(900_000) },
        });
    }
    out
}
