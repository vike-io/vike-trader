//! Totality harness for the Polymarket WIRE decoders: whatever bytes a socket, an HTTP body or an
//! RPC log hands them, they may decode to nothing (or return `Err`), but they must NEVER panic the
//! feed/user-data/reconcile thread, and they must never fabricate an event flood. The property is
//! TOTALITY, not correctness — outputs are not compared, only cheap always-true bounds are.
//!
//! What is driven, all through PUBLIC items only (no visibility was widened for this file):
//!
//! * market channel: `decode_market` / `parse_book` / `apply_update` (the book-maintenance fold),
//!   and the whole per-frame pump (`on_frame` + `handle_update` + `after_book_change` + the §B
//!   freshness hooks) through `run_shard_session` over a `ScriptedStream` — the text lane, with
//!   two token slots so the per-`asset_id` router is exercised;
//! * RTDS: `decode_ref_prices`, `decode_activity_trades`, both scripted sessions, and the
//!   `ToxicityAggregator` fold the activity tape is built to feed;
//! * Gamma / CLOB REST bodies: `parse_gamma_markets`, `parse_gamma_events`, `parse_markets`, the
//!   taker-hold / tick-size / neg-risk probes, `RewardsConfig`, plus the stateful consumers a parsed
//!   page flows into (`UniverseManager::refresh`, `NegRiskSet::group`, `MarketCatalog`);
//! * egress / geoblock probe bodies;
//! * (`polymarket` feature) the user channel (`decode_user_with_tracker` over ONE registry and ONE
//!   `FillTracker`, with accept/remove/register interleaved), the history replay
//!   (`map_polymarket_history`), the reconcile parsers (`recon_client::parse_*`), `/positions`,
//!   order-scoring, the rate-limit headers folded into `RateBudget`, and the Polygon log decoders
//!   (`settlement::chain::decode_*`, `join_settlement`).
//!
//! Generators put the decoders' REAL field names into the object keys and real event/status words
//! into the leaves, so they reach the branches instead of bouncing off the first `.get()`; leaves
//! carry the hostile numbers (i64 extremes, `NaN`/`inf`/`1e999`, empty and numeric strings, wrong
//! types).
//!
//! ⚠ A minimized counterexample is a REAL bug. Several tests below are the standing reproducers of
//! decoder panics found by reading the code (each says so in its doc and names the site); they fail
//! until the decoder is fixed. Commit the generated `.proptest-regressions` seed beside this file.
//!
//! Features: `--features feeds` runs everything except the `exec_plane_decoders` module;
//! `--features polymarket` (which implies `feeds`) runs it all.
#![cfg(feature = "feeds")]

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use proptest::prelude::*;
use serde_json::{Value, json};
use vike_bridge_core::scripted::{ScriptStep, ScriptedStream};
use vike_data::RecordingSink;
use vike_polymarket::{
    ActivityTrade, ActivityTradeSink, GammaMarket, MarketCatalog, NegRiskSet, PolyBook, PumpMode,
    PumpTiming, RankBy, RewardsConfig, RtdsConfig, TickRegime, TokenSlot, ToxicityAggregator,
    UniverseManager, UniversePolicy, WalletClass, apply_update, decode_activity_trades,
    decode_json_string_array, decode_json_string_f64_array, decode_market, decode_ref_prices,
    geoblock_verdict, is_neg_risk, is_tick_size_reject, markets_to_instruments,
    neg_risk_question_id, next_cursor, normalize_ts_ms, parse_book, parse_condition_id,
    parse_egress, parse_gamma_events, parse_gamma_markets, parse_geoblock, parse_itode,
    parse_markets, parse_neg_risk, parse_seconds_delay, parse_tick_size, resolve_taker_hold_ms,
    run_rtds_activity_session, run_rtds_session, run_shard_session, select_universe,
};

const TOKEN: &str = "111";
const OTHER_TOKEN: &str = "222";
/// Per-slot §B freshness threshold for the scripted sessions (the Book/Quotes production value).
const FRESHNESS_MS: i64 = 300_000;

// ---- field names the decoders actually read (the keys the generators draw from) -------------------

const MARKET_KEYS: &[&str] = &[
    "event_type",
    "asset_id",
    "market",
    "timestamp",
    "hash",
    "bids",
    "asks",
    "price",
    "size",
    "side",
    "price_changes",
    "tick_size",
    "last_trade_price",
    "best_bid",
    "best_ask",
];
const RTDS_KEYS: &[&str] = &[
    "topic",
    "type",
    "timestamp",
    "ts",
    "payload",
    "data",
    "symbol",
    "value",
    "price",
    "full_accuracy_value",
    "connection_id",
    "received_at",
];
const ACTIVITY_KEYS: &[&str] = &[
    "proxyWallet",
    "side",
    "size",
    "price",
    "asset",
    "conditionId",
    "outcome",
    "transactionHash",
    "timestamp",
    "ts",
    "payload",
    "data",
    "topic",
    "type",
];
const GAMMA_KEYS: &[&str] = &[
    "id",
    "question",
    "conditionId",
    "slug",
    "endDate",
    "volumeNum",
    "liquidityNum",
    "active",
    "closed",
    "negRisk",
    "orderPriceMinTickSize",
    "outcomes",
    "clobTokenIds",
    "outcomePrices",
    "questionID",
    "negRiskMarketID",
    "negRiskRequestID",
    "groupItemTitle",
    "groupItemThreshold",
    "events",
    "markets",
    "title",
    "rewardsMinSize",
    "rewardsMaxSpread",
];
const CLOB_KEYS: &[&str] = &[
    "data",
    "next_cursor",
    "condition_id",
    "conditionId",
    "tokens",
    "token_id",
    "outcome",
    "neg_risk",
    "minimum_tick_size",
    "tick_size",
    "rewards",
    "rates",
    "rewards_daily_rate",
    "min_size",
    "max_spread",
    "itode",
    "seconds_delay",
    "secondsDelay",
    "sd",
    "mi",
    "ma",
    "e",
    "moas",
];
const EGRESS_KEYS: &[&str] = &["ip", "country", "city", "region", "blocked"];

// ---- leaf and JSON generators ------------------------------------------------------------------

/// i64 with the values that break arithmetic on a wire timestamp or count.
fn arb_i64() -> impl Strategy<Value = i64> {
    prop_oneof![
        Just(0i64),
        Just(1i64),
        Just(-1i64),
        Just(i64::MIN),
        Just(i64::MAX),
        Just(1_700_000_000_000i64),
        Just(1_700_000_000i64),
        any::<i64>(),
    ]
}

/// f64 with every IEEE special.
fn arb_f64() -> impl Strategy<Value = f64> {
    prop_oneof![
        Just(0.0f64),
        Just(-0.0f64),
        Just(f64::NAN),
        Just(f64::INFINITY),
        Just(f64::NEG_INFINITY),
        Just(f64::MIN_POSITIVE),
        Just(f64::MAX),
        Just(f64::MIN),
        any::<f64>(),
    ]
}

/// Polymarket quotes prices and sizes as decimal STRINGS, so the hostile numbers live here.
fn arb_numeric_string() -> impl Strategy<Value = String> {
    prop::sample::select(vec![
        "0",
        "-0",
        "0.5",
        "0.01",
        "1",
        "100",
        "1e3",
        "NaN",
        "inf",
        "-inf",
        "Infinity",
        "1e999",
        "-1e999",
        "",
        " ",
        "  7 ",
        "9223372036854775807",
        "-9223372036854775808",
        "18446744073709551616",
        "99999999999999999999999999999999999999",
        "0x10",
        "1_000",
        ".",
        "-",
        "+1",
    ])
    .prop_map(|s| s.to_string())
}

/// The words the decoders dispatch on (event types, sides, statuses, topics, order types).
fn arb_domain_word() -> impl Strategy<Value = String> {
    prop::sample::select(vec![
        "book",
        "price_change",
        "last_trade_price",
        "trade",
        "order",
        "BUY",
        "SELL",
        "buy",
        "MATCHED",
        "MINED",
        "CONFIRMED",
        "RETRYING",
        "FAILED",
        "UPDATE",
        "CANCELLATION",
        "PLACEMENT",
        "update",
        "subscribe",
        "trades",
        "crypto_prices",
        "activity",
        "true",
        "false",
        "PONG",
    ])
    .prop_map(|s| s.to_string())
}

/// Any JSON leaf: null, bool, i64 extremes, floats, numeric strings, text, domain words, and the
/// empty containers.
fn arb_leaf() -> BoxedStrategy<Value> {
    prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        arb_i64().prop_map(|n| Value::Number(n.into())),
        any::<f64>()
            .prop_map(|f| serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number)),
        arb_numeric_string().prop_map(Value::String),
        "[ -~]{0,12}".prop_map(Value::String),
        any::<String>().prop_map(Value::String),
        arb_domain_word().prop_map(Value::String),
        prop::sample::select(vec![json!([]), json!({})]),
    ]
    .boxed()
}

/// An object key: usually one of the decoder's real field names, sometimes noise.
fn arb_key(names: &'static [&'static str]) -> BoxedStrategy<String> {
    prop_oneof![
        9 => prop::sample::select(names.to_vec()).prop_map(|s| s.to_string()),
        1 => "[a-zA-Z_]{1,8}",
    ]
    .boxed()
}

/// Arbitrary JSON up to `depth`, objects keyed from `names`.
fn arb_json_depth(names: &'static [&'static str], depth: u32) -> BoxedStrategy<Value> {
    arb_leaf()
        .prop_recursive(depth, 64, 6, move |inner| {
            prop_oneof![
                prop::collection::vec(inner.clone(), 0..6).prop_map(Value::Array),
                prop::collection::vec((arb_key(names), inner), 0..6)
                    .prop_map(|kvs| Value::Object(kvs.into_iter().collect())),
            ]
        })
        .boxed()
}

/// Arbitrary JSON, depth <= 4.
fn arb_json(names: &'static [&'static str]) -> BoxedStrategy<Value> {
    arb_json_depth(names, 4)
}

/// The first half of `s` by CHARACTERS (a byte slice could split a code point in the harness
/// itself).
fn truncate_half(s: String) -> String {
    let keep = s.chars().count() / 2;
    s.chars().take(keep).collect()
}

/// A raw text frame: arbitrary unicode, lossy-decoded byte noise, valid JSON, and truncated JSON.
fn arb_text(names: &'static [&'static str]) -> BoxedStrategy<String> {
    prop_oneof![
        any::<String>(),
        prop::collection::vec(any::<u8>(), 0..256)
            .prop_map(|b| String::from_utf8_lossy(&b).into_owned()),
        arb_json(names).prop_map(|v| v.to_string()),
        arb_json(names).prop_map(|v| truncate_half(v.to_string())),
    ]
    .boxed()
}

/// A decimal-string number as the wire spells it, a hostile one, or any leaf.
fn arb_wire_num() -> BoxedStrategy<Value> {
    prop_oneof![
        3 => arb_numeric_string().prop_map(Value::String),
        3 => (0u32..1000).prop_map(|n| Value::String(format!("0.{n:03}"))),
        1 => arb_leaf(),
    ]
    .boxed()
}

/// A frame timestamp: epoch-ms as a string (the live form), as a number, or garbage.
fn arb_ts() -> BoxedStrategy<Value> {
    prop_oneof![
        3 => arb_i64().prop_map(|n| Value::String(n.to_string())),
        2 => arb_i64().prop_map(|n| Value::Number(n.into())),
        1 => arb_leaf(),
    ]
    .boxed()
}

fn arb_side() -> BoxedStrategy<Value> {
    prop_oneof![
        3 => Just(json!("BUY")),
        3 => Just(json!("SELL")),
        1 => arb_leaf(),
    ]
    .boxed()
}

/// An `asset_id`: mostly one of the two subscribed tokens, so the router resolves a slot.
fn arb_asset() -> BoxedStrategy<Value> {
    prop_oneof![
        8 => Just(json!(TOKEN)),
        2 => Just(json!(OTHER_TOKEN)),
        1 => arb_leaf(),
    ]
    .boxed()
}

fn arb_mode() -> impl Strategy<Value = PumpMode> {
    prop_oneof![Just(PumpMode::Book), Just(PumpMode::Quotes), Just(PumpMode::Trades)]
}

// ---- market channel -----------------------------------------------------------------------------

fn arb_level() -> impl Strategy<Value = Value> {
    (arb_wire_num(), arb_wire_num()).prop_map(|(p, s)| json!({ "price": p, "size": s }))
}

/// A frame shaped like the three live market-channel event types, with hostile field values.
fn arb_shaped_frame() -> BoxedStrategy<Value> {
    prop_oneof![
        (
            arb_asset(),
            prop::collection::vec(arb_level(), 0..5),
            prop::collection::vec(arb_level(), 0..5),
            prop::option::of(arb_ts()),
        )
            .prop_map(|(asset, bids, asks, ts)| {
                let mut o = json!({
                    "event_type": "book", "asset_id": asset, "bids": bids, "asks": asks
                });
                if let Some(ts) = ts {
                    o["timestamp"] = ts;
                }
                o
            }),
        (
            prop::collection::vec((arb_asset(), arb_wire_num(), arb_wire_num(), arb_side()), 0..5),
            prop::option::of(arb_ts()),
        )
            .prop_map(|(entries, ts)| {
                let rows: Vec<Value> = entries
                    .into_iter()
                    .map(|(a, p, s, side)| {
                        json!({ "asset_id": a, "price": p, "size": s, "side": side })
                    })
                    .collect();
                let mut o = json!({ "event_type": "price_change", "price_changes": rows });
                if let Some(ts) = ts {
                    o["timestamp"] = ts;
                }
                o
            }),
        (arb_asset(), arb_wire_num(), arb_wire_num(), arb_side(), prop::option::of(arb_ts()),)
            .prop_map(|(asset, price, size, side, ts)| {
                let mut o = json!({
                    "event_type": "last_trade_price", "asset_id": asset,
                    "price": price, "size": size, "side": side
                });
                if let Some(ts) = ts {
                    o["timestamp"] = ts;
                }
                o
            }),
    ]
    .boxed()
}

/// A market-channel frame: one shaped event, an array of them, or arbitrary keyed JSON.
fn arb_market_frame() -> BoxedStrategy<Value> {
    prop_oneof![
        6 => arb_shaped_frame(),
        2 => prop::collection::vec(arb_shaped_frame(), 0..4).prop_map(Value::Array),
        3 => arb_json(MARKET_KEYS),
    ]
    .boxed()
}

/// One inbound TEXT frame for the pump: a market frame, raw noise, or the CLOB `PONG` keepalive.
fn arb_frame_text() -> BoxedStrategy<String> {
    prop_oneof![
        6 => arb_market_frame().prop_map(|v| v.to_string()),
        2 => arb_text(MARKET_KEYS),
        1 => Just("PONG".to_string()),
    ]
    .boxed()
}

/// Drive ONE scripted session of the real batched pump over two token slots. The script ending
/// surfaces as a stream-closed `Err`, which is the normal exit and is not asserted on.
fn drive_session(mode: PumpMode, steps: Vec<ScriptStep>) -> RecordingSink {
    let mut stream = ScriptedStream::from_steps(steps);
    let sink = RecordingSink::default();
    let stop = AtomicBool::new(false);
    let timing = PumpTiming::new("prop");
    let mut slots = vec![
        TokenSlot::new(TOKEN, 0.01, FRESHNESS_MS),
        TokenSlot::new(OTHER_TOKEN, 0.001, FRESHNESS_MS),
    ];
    let _ = run_shard_session(&mut stream, &sink, mode, &mut slots, &stop, &timing, || {});
    sink
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// (a)+(b) `decode_market` and `parse_book` are total over arbitrary JSON, shaped or not.
    #[test]
    fn decode_market_and_parse_book_are_total_over_json(
        shaped in arb_market_frame(),
        generic in arb_json(MARKET_KEYS),
    ) {
        for frame in [&shaped, &generic] {
            let _ = decode_market(frame);
            let book = parse_book(frame);
            let _ = (book.best_bid(), book.best_ask(), book.mid());
        }
    }

    /// (a) ...and over arbitrary text: whatever parses is decoded, whatever does not is skipped.
    #[test]
    fn decode_market_is_total_over_arbitrary_text(text in arb_text(MARKET_KEYS)) {
        if let Ok(v) = serde_json::from_str::<Value>(&text) {
            let _ = decode_market(&v);
        }
    }

    /// (c) A short random frame sequence folds into ONE per-token book map without a panic.
    #[test]
    fn market_update_sequences_fold_into_books(
        frames in prop::collection::vec(arb_market_frame(), 1..8),
    ) {
        let mut books: HashMap<String, PolyBook> = HashMap::new();
        for frame in &frames {
            for up in decode_market(frame) {
                apply_update(&mut books, &up);
            }
        }
        for book in books.values() {
            let _ = (book.best_bid(), book.best_ask(), book.mid());
        }
    }

    /// (c) The whole per-frame pump (decode, route per `asset_id`, `L2Book` fold, quote/trade
    /// emission, §B bookkeeping) over a random frame script, in every pump mode. No event flood: a
    /// frame addresses at most two seated tokens, so it emits a bounded number of sink calls.
    #[test]
    fn market_sessions_over_arbitrary_frames_never_panic(
        mode in arb_mode(),
        frames in prop::collection::vec(arb_frame_text(), 1..8),
    ) {
        let n = frames.len();
        let sink = drive_session(mode, frames.into_iter().map(ScriptStep::Text).collect());
        let calls = sink.recorded().len();
        prop_assert!(calls <= 128 * n, "event flood: {calls} sink calls from {n} frames");
    }

    /// (c) The same, with read-timeout ticks interleaved: a tick is when the pump judges DATA
    /// freshness against the wire timestamps it has folded.
    ///
    /// ⚠ KNOWN FAILING until `vike_bridge_core::stream_health::StreamHealth::check_freshness` stops
    /// computing `now_ms - ref_ts` in plain `i64`: a frame stamped near `i64::MIN` (e.g.
    /// `"timestamp":"-9223372036854775808"`, which `ws::frame_ts` parses happily) becomes the
    /// slot's `newest_ts`, and the next tick overflows (debug panic). A shared home — its own PR.
    /// The deterministic reproducer is `i64_min_frame_timestamp_then_a_read_tick_does_not_overflow`.
    #[test]
    fn market_sessions_with_read_timeouts_never_panic(
        mode in arb_mode(),
        script in prop::collection::vec(
            prop_oneof![3 => arb_frame_text().prop_map(Some), 1 => Just(None::<String>)],
            1..8,
        ),
    ) {
        let steps: Vec<ScriptStep> = script
            .into_iter()
            .map(|s| match s {
                Some(text) => ScriptStep::Text(text),
                None => ScriptStep::Timeout,
            })
            .collect();
        let _ = drive_session(mode, steps);
    }
}

/// Deterministic reproducer for the `check_freshness` overflow described on
/// `market_sessions_with_read_timeouts_never_panic`. KNOWN FAILING until that arithmetic is
/// saturating or checked.
#[test]
fn i64_min_frame_timestamp_then_a_read_tick_does_not_overflow() {
    let frame = json!({
        "event_type": "book", "asset_id": TOKEN,
        "bids": [{ "price": "0.5", "size": "1" }], "asks": [{ "price": "0.6", "size": "1" }],
        "timestamp": "-9223372036854775808"
    })
    .to_string();
    let _ = drive_session(PumpMode::Book, vec![ScriptStep::Text(frame), ScriptStep::Timeout]);
}

// ---- RTDS ---------------------------------------------------------------------------------------

fn arb_symbol() -> BoxedStrategy<Value> {
    prop_oneof![
        3 => Just(json!("btcusdt")),
        1 => Just(json!("")),
        1 => arb_leaf(),
    ]
    .boxed()
}

fn arb_ref_entry() -> BoxedStrategy<Value> {
    (
        prop::option::of(arb_symbol()),
        arb_wire_num(),
        prop::option::of(arb_wire_num()),
        prop::option::of(arb_ts()),
    )
        .prop_map(|(sym, value, full, ts)| {
            let mut o = json!({ "value": value });
            if let Some(s) = sym {
                o["symbol"] = s;
            }
            if let Some(f) = full {
                o["full_accuracy_value"] = f;
            }
            if let Some(t) = ts {
                o["timestamp"] = t;
            }
            o
        })
        .boxed()
}

/// A `crypto_prices` envelope: the body under `data` or `payload`, as one entry, an array, or the
/// snapshot's nested `{symbol, data:[..]}`.
fn arb_ref_frame() -> BoxedStrategy<Value> {
    let body = prop_oneof![
        arb_ref_entry(),
        prop::collection::vec(arb_ref_entry(), 0..4).prop_map(Value::Array),
        (prop::option::of(arb_symbol()), prop::collection::vec(arb_ref_entry(), 0..4)).prop_map(
            |(sym, rows)| {
                let mut o = json!({ "data": rows });
                if let Some(s) = sym {
                    o["symbol"] = s;
                }
                o
            }
        ),
    ];
    (
        prop::sample::select(vec!["data", "payload"]),
        body,
        prop::option::of(arb_symbol()),
        prop::option::of(arb_ts()),
        prop::sample::select(vec!["crypto_prices", "activity", "", "equity_prices"]),
    )
        .prop_map(|(key, body, sym, ts, topic)| {
            let mut o = json!({ "topic": topic, "type": "update" });
            o[key] = body;
            if let Some(s) = sym {
                o["symbol"] = s;
            }
            if let Some(t) = ts {
                o["timestamp"] = t;
            }
            o
        })
        .boxed()
}

fn arb_wallet() -> BoxedStrategy<Value> {
    prop_oneof![
        3 => prop::sample::select(vec!["0xa", "0xbb", "0xccc"]).prop_map(|s| json!(s)),
        1 => arb_leaf(),
    ]
    .boxed()
}

fn arb_activity_entry() -> BoxedStrategy<Value> {
    (
        prop::option::of(arb_wallet()),
        prop::option::of(arb_side()),
        arb_wire_num(),
        arb_wire_num(),
        prop::option::of(arb_asset()),
        prop::option::of(arb_ts()),
    )
        .prop_map(|(wallet, side, size, price, asset, ts)| {
            let mut o = json!({ "size": size, "price": price });
            if let Some(w) = wallet {
                o["proxyWallet"] = w;
            }
            if let Some(s) = side {
                o["side"] = s;
            }
            if let Some(a) = asset {
                o["asset"] = a;
            }
            if let Some(t) = ts {
                o["timestamp"] = t;
            }
            o
        })
        .boxed()
}

/// An `activity`/`trades` envelope: one entry, an array of them, or a nested `data` array.
fn arb_activity_frame() -> BoxedStrategy<Value> {
    let body = prop_oneof![
        arb_activity_entry(),
        prop::collection::vec(arb_activity_entry(), 0..4).prop_map(Value::Array),
        prop::collection::vec(arb_activity_entry(), 0..4).prop_map(|rows| json!({ "data": rows })),
    ];
    (prop::sample::select(vec!["data", "payload"]), body, prop::option::of(arb_ts()))
        .prop_map(|(key, body, ts)| {
            let mut o = json!({ "topic": "activity", "type": "trades" });
            o[key] = body;
            if let Some(t) = ts {
                o["timestamp"] = t;
            }
            o
        })
        .boxed()
}

/// An RTDS frame of either lane, or a top-level array of envelopes, or arbitrary keyed JSON.
fn arb_rtds_frame() -> BoxedStrategy<Value> {
    prop_oneof![
        4 => arb_ref_frame(),
        4 => arb_activity_frame(),
        2 => prop::collection::vec(prop_oneof![arb_ref_frame(), arb_activity_frame()], 0..4)
            .prop_map(Value::Array),
        2 => arb_json(RTDS_KEYS),
        2 => arb_json(ACTIVITY_KEYS),
    ]
    .boxed()
}

fn arb_rtds_text() -> BoxedStrategy<String> {
    prop_oneof![
        6 => arb_rtds_frame().prop_map(|v| v.to_string()),
        2 => arb_text(RTDS_KEYS),
        1 => Just(String::new()),
        1 => Just("PONG".to_string()),
    ]
    .boxed()
}

/// Counts delivered activity rows; the delivery seam of the activity session.
#[derive(Default)]
struct CountingSink(AtomicUsize);

impl ActivityTradeSink for CountingSink {
    fn on_activity_trade(&self, _trade: &ActivityTrade) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// (a)+(b) Both RTDS decoders are total over arbitrary and shaped JSON, for any topic gate and
    /// default symbol; `normalize_ts_ms` is total over every i64.
    #[test]
    fn rtds_decoders_are_total_over_json(
        frame in arb_rtds_frame(),
        topic in prop::sample::select(vec!["crypto_prices", "activity", "", "equity_prices"]),
        default_symbol in "[ -~]{0,8}",
        ts in arb_i64(),
    ) {
        let _ = decode_ref_prices(&frame, topic, &default_symbol);
        let _ = decode_activity_trades(&frame);
        let _ = normalize_ts_ms(ts);
    }

    /// (a)+(c) Both scripted RTDS sessions (`on_frame` / `on_activity_frame`) over random text.
    #[test]
    fn rtds_sessions_over_arbitrary_text_never_panic(
        frames in prop::collection::vec(arb_rtds_text(), 1..8),
    ) {
        let stop = AtomicBool::new(false);

        let cfg = RtdsConfig::crypto_prices("btcusdt");
        let sink = RecordingSink::default();
        let mut stream = ScriptedStream::from_texts(frames.clone());
        let _ = run_rtds_session(&mut stream, &sink, &cfg, &stop);

        let acfg = RtdsConfig::activity_trades();
        let counter = CountingSink::default();
        let mut stream = ScriptedStream::from_texts(frames);
        let _ = run_rtds_activity_session(&mut stream, &counter, &acfg, &stop);
    }

    /// (c) The activity tape folds into the flow-toxicity aggregator it exists to feed: decoded
    /// rows (wire timestamps and sizes included) over a random window, read at a random `now`.
    #[test]
    fn activity_trades_fold_into_the_toxicity_aggregator(
        frames in prop::collection::vec(arb_activity_frame(), 1..8),
        window in arb_i64(),
        now in arb_i64(),
    ) {
        let mut agg = ToxicityAggregator::new(window);
        for frame in &frames {
            for t in decode_activity_trades(frame) {
                let class = match t.proxy_wallet.len() % 4 {
                    0 => WalletClass::Sharp,
                    1 => WalletClass::Whale,
                    2 => WalletClass::Retail,
                    _ => WalletClass::Unknown,
                };
                agg.observe(t.side, class, t.size, t.ts);
            }
            let _ = agg.current(now);
        }
    }
}

// ---- Gamma and CLOB REST bodies -------------------------------------------------------------------

/// A JSON-string-encoded array (how Gamma ships `outcomes`/`clobTokenIds`/`outcomePrices`).
fn arb_json_array_string() -> BoxedStrategy<Value> {
    prop_oneof![
        3 => prop::collection::vec(arb_leaf(), 0..4)
            .prop_map(|v| Value::String(Value::Array(v).to_string())),
        1 => arb_leaf(),
    ]
    .boxed()
}

/// `endDate`: well-formed, structurally date-like with an enormous year, or anything.
fn arb_end_date() -> BoxedStrategy<Value> {
    prop_oneof![
        2 => Just(json!("2026-12-31T23:59:59Z")),
        3 => ("[0-9]{1,19}", 0u8..14, 0u8..33, 0u8..25, 0u8..61, 0u8..61).prop_map(
            |(y, mo, d, h, mi, s)| Value::String(format!("{y}-{mo}-{d}T{h}:{mi}:{s}Z"))
        ),
        1 => arb_leaf(),
    ]
    .boxed()
}

fn arb_neg_risk_market_id() -> impl Strategy<Value = Value> {
    prop::sample::select(vec![
        "0x1111111111111111111111111111111111111111111111111111111111111100",
        "0x2222222222222222222222222222222222222222222222222222222222222200",
        "",
        "short",
    ])
    .prop_map(|s| json!(s))
}

/// A Gamma market object: the two required strings usually present, hostile everything else.
fn arb_gamma_market() -> impl Strategy<Value = Value> {
    let core = (
        prop_oneof![4 => Just(json!("Will it?")), 1 => arb_leaf()],
        prop_oneof![4 => Just(json!("0xcond")), 1 => arb_leaf()],
        arb_end_date(),
        arb_json_array_string(),
        arb_json_array_string(),
        arb_json_array_string(),
    );
    let neg = (
        prop::option::of(arb_neg_risk_market_id()),
        prop::option::of(prop_oneof![arb_wire_num(), arb_i64().prop_map(|n| json!(n))]),
        prop::option::of(arb_leaf()),
    );
    let extras = prop::collection::vec((arb_key(GAMMA_KEYS), arb_json_depth(GAMMA_KEYS, 2)), 0..6);
    (core, neg, extras).prop_map(
        |(
            (question, condition, end, outcomes, tokens, prices),
            (nrm, threshold, active),
            extras,
        )| {
            let mut o = json!({
                "question": question, "conditionId": condition, "endDate": end,
                "outcomes": outcomes, "clobTokenIds": tokens, "outcomePrices": prices
            });
            if let Some(v) = nrm {
                o["negRiskMarketID"] = v;
            }
            if let Some(v) = threshold {
                o["groupItemThreshold"] = v;
            }
            if let Some(v) = active {
                o["active"] = v;
            }
            for (k, v) in extras {
                o[k.as_str()] = v;
            }
            o
        },
    )
}

/// A `/markets` page: an array of Gamma markets, or arbitrary keyed JSON.
fn arb_gamma_page() -> BoxedStrategy<Value> {
    prop_oneof![
        5 => prop::collection::vec(arb_gamma_market(), 0..4).prop_map(Value::Array),
        1 => arb_json(GAMMA_KEYS),
    ]
    .boxed()
}

/// An `/events` page: events carrying a `markets` array.
fn arb_gamma_events() -> BoxedStrategy<Value> {
    prop_oneof![
        5 => prop::collection::vec(
            (arb_leaf(), arb_leaf(), arb_leaf(), arb_neg_risk_market_id(), arb_gamma_page()),
            0..3,
        )
        .prop_map(|events| Value::Array(
            events
                .into_iter()
                .map(|(id, slug, title, nrm, markets)| json!({
                    "id": id, "slug": slug, "title": title,
                    "negRiskMarketID": nrm, "markets": markets
                }))
                .collect()
        )),
        1 => arb_json(GAMMA_KEYS),
    ]
    .boxed()
}

fn arb_clob_market() -> impl Strategy<Value = Value> {
    (
        prop_oneof![4 => Just(json!("0xcond")), 1 => arb_leaf()],
        prop::collection::vec(
            (prop_oneof![3 => Just(json!("111")), 1 => arb_leaf()], arb_leaf()),
            0..4,
        ),
        arb_leaf(),
        arb_wire_num(),
        arb_wire_num(),
        (prop::collection::vec(arb_wire_num(), 0..3), arb_wire_num(), arb_wire_num()),
    )
        .prop_map(|(cond, tokens, neg, minimum, tick, (rates, min_size, max_spread))| {
            let tokens: Vec<Value> = tokens
                .into_iter()
                .map(|(id, outcome)| json!({ "token_id": id, "outcome": outcome }))
                .collect();
            let rates: Vec<Value> =
                rates.into_iter().map(|r| json!({ "rewards_daily_rate": r })).collect();
            json!({
                "condition_id": cond, "tokens": tokens, "neg_risk": neg,
                "minimum_tick_size": minimum, "tick_size": tick,
                "rewards": { "rates": rates, "min_size": min_size, "max_spread": max_spread }
            })
        })
}

/// A CLOB `/markets` page: `{data:[..], next_cursor}`, or arbitrary keyed JSON.
fn arb_clob_page() -> BoxedStrategy<Value> {
    prop_oneof![
        5 => (
            prop::collection::vec(arb_clob_market(), 0..4),
            prop_oneof![Just(json!("LTE=")), Just(json!("MA==")), Just(json!("")), arb_leaf()],
        )
            .prop_map(|(data, cursor)| json!({ "data": data, "next_cursor": cursor })),
        1 => arb_json(CLOB_KEYS),
    ]
    .boxed()
}

fn arb_policy() -> impl Strategy<Value = UniversePolicy> {
    (
        prop_oneof![Just(0usize), Just(1usize), 0usize..30, Just(usize::MAX)],
        arb_f64(),
        arb_f64(),
        any::<bool>(),
        prop_oneof![Just(RankBy::Liquidity), Just(RankBy::Volume)],
    )
        .prop_map(|(max_markets, min_liquidity, min_volume, require_open, rank_by)| {
            UniversePolicy { max_markets, min_liquidity, min_volume, require_open, rank_by }
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// (a)+(b) The Gamma parsers and the accessors on what they return are total over shaped and
    /// arbitrary pages (`resolution_ts_ms` has its own test below: it is the known-failing site).
    #[test]
    fn gamma_parsers_are_total_over_json(
        page in arb_gamma_page(),
        events in arb_gamma_events(),
        generic in arb_json(GAMMA_KEYS),
        stringy in arb_json_array_string(),
    ) {
        for v in [&page, &events, &generic] {
            for m in parse_gamma_markets(v).iter().chain(parse_gamma_events(v).iter()) {
                let _ = (m.yes_price(), m.yes_token_id(), m.no_token_id(), m.is_neg_risk_member());
            }
        }
        let _ = decode_json_string_array(&stringy);
        let _ = decode_json_string_f64_array(&stringy);
        let _ = decode_json_string_array(&generic);
        let _ = decode_json_string_f64_array(&generic);
    }

    /// (a) `neg_risk_question_id` slices a string by byte offset; the guard in front of it must
    /// keep every hostile input (multi-byte, wrong length, non-hex) away from the slice.
    #[test]
    fn neg_risk_question_id_is_total(
        id in prop_oneof![
            any::<String>(),
            "(0x)?[0-9a-fA-F]{60,66}",
            "[0-9a-f]{0,30}é[0-9a-f]{0,40}",
        ],
        index in prop_oneof![Just(0u32), Just(255u32), Just(256u32), any::<u32>()],
    ) {
        let _ = neg_risk_question_id(&id, index);
    }

    /// (c) A sequence of Gamma pages folds into ONE `UniverseManager` (subscribe/unsubscribe diff
    /// committed each round), and each parsed page flows through the neg-risk grouping and the
    /// searchable catalog.
    #[test]
    fn gamma_page_sequences_fold_into_universe_and_catalog(
        pages in prop::collection::vec(arb_gamma_page(), 1..8),
        policy in arb_policy(),
        query in "[ -~]{0,6}",
    ) {
        let mut mgr = UniverseManager::new(policy.clone());
        for page in &pages {
            let markets: Vec<GammaMarket> = parse_gamma_markets(page);
            let _ = select_universe(&markets, &policy);
            let (_selected, diff) = mgr.refresh(&markets);
            let _ = diff.is_empty();
            let _ = mgr.subscribed().count();

            for set in NegRiskSet::group(&markets) {
                let _ = (
                    set.completeness(),
                    set.yes_price_sum(),
                    set.yes_price_sum_with_ceiling(1.0),
                    set.arb_edge(0.01),
                    set.question_id_mismatches().len(),
                    set.all_token_ids().len(),
                    set.member(0).is_some(),
                );
            }
            let _ = markets_to_instruments(&markets);
            let catalog = MarketCatalog::from_markets(markets);
            let _ = catalog.search(&query).len();
            let _ = catalog.neg_risk_sets().len();
            let _ = catalog.neg_risk_set_for_condition("0xcond");
            let _ = catalog.neg_risk_set("0x1111111111111111111111111111111111111111111111111111111111111100");
        }
    }

    /// (a)+(b) The CLOB REST parsers (`/markets` page, `/tick-size`, `/neg-risk`, the taker-hold
    /// probes, rewards) are total over shaped and arbitrary JSON.
    #[test]
    fn clob_rest_parsers_are_total_over_json(
        page in arb_clob_page(),
        generic in arb_json(CLOB_KEYS),
        token in "[ -~]{0,8}",
    ) {
        for v in [&page, &generic] {
            let markets = parse_markets(v);
            let _ = next_cursor(v);
            let _ = is_neg_risk(&token, &markets);
            let _ = is_neg_risk(TOKEN, &markets);
            let _ = parse_tick_size(v);
            let _ = parse_neg_risk(v);
            let _ = parse_condition_id(v);
            let _ = resolve_taker_hold_ms(parse_itode(v), parse_seconds_delay(v));
            let rewards = [
                RewardsConfig::from_gamma_market(v),
                RewardsConfig::from_clob_rewards(v),
                RewardsConfig::from_compact(v),
            ];
            for r in rewards {
                let _ = r.earns_rewards();
            }
        }
    }

    /// (a) Egress / geoblock probe bodies (arbitrary text and keyed JSON), and their `Display`.
    #[test]
    fn probe_body_parsers_are_total(text in arb_text(EGRESS_KEYS), ws_lane in any::<bool>()) {
        if let Ok(e) = parse_egress(&text, ws_lane) {
            let _ = e.to_string();
        }
        if let Ok(g) = parse_geoblock(&text) {
            let _ = g.to_string();
        }
        let _ = geoblock_verdict(parse_geoblock(&text));
    }

    /// (a) The tick regime's pure pieces over arbitrary reasons, ticks and prices.
    #[test]
    fn tick_regime_is_total(
        reason in any::<String>(),
        ticks in prop::collection::vec(arb_f64(), 0..6),
        price in arb_f64(),
    ) {
        let _ = is_tick_size_reject(&reason);
        let regime = TickRegime::new();
        for tick in ticks {
            regime.set(TOKEN, tick);
            let _ = (regime.tick_size(TOKEN), regime.properties(TOKEN));
            let _ = regime.round_price(TOKEN, price);
        }
        let _ = regime.round_price(OTHER_TOKEN, price);
    }
}

/// `GammaMarket::resolution_ts_ms` parses the wire `endDate` and then multiplies in plain `i64`
/// (`days_from_civil`, then `((days*24+h)*60+mi)*60+se)*1000`). ⚠ KNOWN FAILING: a year of about
/// 300 million or more overflows the final multiplication (a year past ~2.5e16 overflows earlier,
/// in `days_from_civil`'s `era * 146_097`), which panics in a debug build. The year is the
/// untrusted part: it is `i64`-parsed from the response with no range check. A fix is a checked
/// or saturating chain, or a year bound, returning `None`.
#[test]
fn gamma_end_date_with_a_huge_year_does_not_overflow() {
    let m = GammaMarket { end_date: "999999999-01-01T00:00:00Z".to_string(), ..Default::default() };
    let _ = m.resolution_ts_ms();
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// The general form of the test above: any string, and any date-shaped string with a year of
    /// up to 19 digits, through `resolution_ts_ms`. ⚠ KNOWN FAILING for the same reason.
    #[test]
    fn gamma_resolution_ts_ms_is_total_over_end_dates(
        end_date in prop_oneof![
            any::<String>(),
            "[0-9T:Z .+-]{0,32}",
            ("\\+?[0-9]{1,19}", 0u8..14, 0u8..33, 0u8..25, 0u8..61, 0u8..61).prop_map(
                |(y, mo, d, h, mi, s)| format!("{y}-{mo}-{d}T{h}:{mi}:{s}Z")
            ),
        ],
    ) {
        let m = GammaMarket { end_date, ..Default::default() };
        let _ = m.resolution_ts_ms();
    }
}

// ---- the exec plane (needs `--features polymarket`) -----------------------------------------------

#[cfg(feature = "polymarket")]
mod exec_plane_decoders {
    use super::*;

    use vike_polymarket::exec_plane::rate_budget::parse_headers;
    use vike_polymarket::exec_plane::recon_client::{
        normalize_order_status, parse_balance, parse_fill_reports, parse_order_reports,
        parse_position_reports, settlement_fill_report,
    };
    use vike_polymarket::exec_plane::settlement::chain::{
        ChainResolution, RedeemVenue, Redemption, TOPIC_CONDITION_RESOLUTION,
        TOPIC_CTF_PAYOUT_REDEMPTION, TOPIC_CTF_POSITION_MERGE, TOPIC_CTF_POSITION_SPLIT,
        TOPIC_NEG_RISK_PAYOUT_REDEMPTION, TOPIC_NEG_RISK_POSITIONS_CONVERTED,
        TOPIC_ORDER_FILLED_V1, TOPIC_ORDER_FILLED_V2, TOPIC_TRANSFER_BATCH, TOPIC_TRANSFER_SINGLE,
        TOPIC_USDC_TRANSFER, TokenTransfer, data_words, decode_condition_resolution,
        decode_ctf_position_merge, decode_ctf_position_split, decode_ctf_redemption,
        decode_neg_risk_redemption, decode_order_fill_v1, decode_order_fill_v2,
        decode_positions_converted, decode_redemption, decode_token_transfer, decode_uint_result,
        decode_usdc_transfer, join_settlement, u256_word_to_decimal, word_address, word_u128,
    };
    use vike_polymarket::{
        FillTracker, PolymarketRegistry, RateBudget, Tier, decode_user, decode_user_with_tracker,
        map_polymarket_history, order_scoring_query, orders_scoring_query, parse_order_scoring,
        parse_orders_scoring, parse_positions,
    };

    const USER_KEYS: &[&str] = &[
        "event_type",
        "status",
        "id",
        "asset_id",
        "taker_order_id",
        "maker_orders",
        "order_id",
        "matched_amount",
        "size",
        "price",
        "timestamp",
        "type",
        "original_size",
        "size_matched",
        "side",
    ];
    const RECON_KEYS: &[&str] = &[
        "data",
        "id",
        "asset_id",
        "asset",
        "side",
        "order_type",
        "original_size",
        "size_matched",
        "status",
        "created_at",
        "taker_order_id",
        "maker_orders",
        "order_id",
        "matched_amount",
        "price",
        "size",
        "match_time",
        "timestamp",
        "last_update",
        "avgPrice",
        "balance",
        "conditionId",
        "redeemable",
        "negativeRisk",
        "outcomeIndex",
        "title",
        "curPrice",
    ];
    const LOG_KEYS: &[&str] =
        &["topics", "data", "transactionHash", "blockNumber", "blockTimestamp"];
    const SMALL_KEYS: &[&str] = &["scoring", "0xabc", "0xdef"];
    /// (coid, CLOB id) pairs the sequences accept/remove/register; the last one has an EMPTY CLOB
    /// id (the registry accepts it and the decoders must cope).
    const SLOTS: [(&str, &str); 4] =
        [("coid-T", "0xT"), ("coid-M", "0xM"), ("coid-U", "0xU"), ("coid-E", "")];

    // ---- user channel, history, reconcile -------------------------------------------------------

    fn arb_clob_id() -> BoxedStrategy<Value> {
        prop_oneof![
            6 => prop::sample::select(vec!["0xT", "0xM", "0xU", "0xFOREIGN"]).prop_map(|s| json!(s)),
            1 => Just(json!("")),
            1 => arb_leaf(),
        ]
        .boxed()
    }

    fn arb_user_trade() -> impl Strategy<Value = Value> {
        (
            prop_oneof![
                6 => prop::sample::select(vec!["MATCHED", "MINED", "CONFIRMED", "RETRYING", "FAILED"])
                    .prop_map(|s| json!(s)),
                1 => arb_leaf(),
            ],
            prop_oneof![
                3 => prop::sample::select(vec!["t1", "t2", "t3"]).prop_map(|s| json!(s)),
                1 => Just(json!("")),
                1 => arb_leaf(),
            ],
            arb_asset(),
            arb_clob_id(),
            prop::collection::vec((arb_clob_id(), arb_wire_num(), arb_wire_num()), 0..4),
            (arb_wire_num(), arb_wire_num(), prop::option::of(arb_ts())),
        )
            .prop_map(|(status, id, asset, taker, makers, (size, price, ts))| {
                let makers: Vec<Value> = makers
                    .into_iter()
                    .map(|(o, a, p)| json!({ "order_id": o, "matched_amount": a, "price": p }))
                    .collect();
                let mut v = json!({
                    "event_type": "trade", "status": status, "id": id, "asset_id": asset,
                    "taker_order_id": taker, "maker_orders": makers, "size": size, "price": price
                });
                if let Some(ts) = ts {
                    v["timestamp"] = ts;
                }
                v
            })
    }

    fn arb_user_order() -> impl Strategy<Value = Value> {
        (
            prop_oneof![
                6 => prop::sample::select(vec!["PLACEMENT", "UPDATE", "CANCELLATION"])
                    .prop_map(|s| json!(s)),
                1 => arb_leaf(),
            ],
            arb_clob_id(),
            arb_asset(),
            arb_wire_num(),
            arb_wire_num(),
        )
            .prop_map(|(ty, id, asset, original, matched)| {
                json!({
                    "event_type": "order", "type": ty, "id": id, "asset_id": asset,
                    "original_size": original, "size_matched": matched
                })
            })
    }

    fn arb_user_event() -> BoxedStrategy<Value> {
        prop_oneof![
            4 => arb_user_trade(),
            3 => arb_user_order(),
            1 => arb_json(USER_KEYS),
        ]
        .boxed()
    }

    /// A user-channel frame: one event, or an array of them.
    fn arb_user_frame() -> BoxedStrategy<Value> {
        prop_oneof![
            5 => arb_user_event(),
            2 => prop::collection::vec(arb_user_event(), 0..4).prop_map(Value::Array),
        ]
        .boxed()
    }

    #[derive(Debug, Clone)]
    enum UserOp {
        Accept { slot: usize, side: i32 },
        Remove { slot: usize },
        Register { slot: usize, qty: f64 },
        Frame(Value),
        History { trades: Value, orders: Value },
    }

    fn arb_user_op() -> impl Strategy<Value = UserOp> {
        prop_oneof![
            2 => (0usize..4, prop_oneof![Just(1i32), Just(-1i32), Just(0i32), any::<i32>()])
                .prop_map(|(slot, side)| UserOp::Accept { slot, side }),
            1 => (0usize..4).prop_map(|slot| UserOp::Remove { slot }),
            2 => (0usize..4, arb_f64()).prop_map(|(slot, qty)| UserOp::Register { slot, qty }),
            6 => arb_user_frame().prop_map(UserOp::Frame),
            1 => (
                prop::collection::vec(arb_user_trade(), 0..3),
                prop::collection::vec(arb_user_order(), 0..3),
            )
                .prop_map(|(trades, orders)| UserOp::History {
                    trades: Value::Array(trades),
                    orders: Value::Array(orders),
                }),
        ]
    }

    fn arb_recon_order() -> impl Strategy<Value = Value> {
        (
            arb_clob_id(),
            arb_asset(),
            arb_side(),
            arb_leaf(),
            arb_wire_num(),
            arb_wire_num(),
            arb_leaf(),
            arb_wire_num(),
        )
            .prop_map(|(id, asset, side, kind, original, matched, status, created)| {
                json!({
                    "id": id, "asset_id": asset, "side": side, "order_type": kind,
                    "original_size": original, "size_matched": matched, "status": status,
                    "created_at": created
                })
            })
    }

    fn arb_recon_position() -> impl Strategy<Value = Value> {
        (arb_asset(), arb_wire_num(), arb_wire_num(), arb_leaf(), arb_leaf(), arb_leaf()).prop_map(
            |(asset, size, avg, cond, redeemable, outcome_index)| {
                json!({
                    "asset": asset, "size": size, "avgPrice": avg, "conditionId": cond,
                    "redeemable": redeemable, "outcomeIndex": outcome_index,
                    "negativeRisk": false, "title": "t", "curPrice": 0.5
                })
            },
        )
    }

    /// Rows as a bare array or wrapped under `data`, the two list shapes the CLOB uses.
    fn arb_rows(row: impl Strategy<Value = Value> + 'static) -> impl Strategy<Value = Value> {
        (prop::collection::vec(row, 0..4), any::<bool>()).prop_map(|(rows, wrapped)| {
            if wrapped { json!({ "data": rows }) } else { Value::Array(rows) }
        })
    }

    fn seeded_registry() -> PolymarketRegistry {
        let reg = PolymarketRegistry::new();
        let _ = reg.on_accept("coid-T", "0xT", 1);
        let _ = reg.on_accept("coid-M", "0xM", -1);
        reg
    }

    // ---- Polygon logs -----------------------------------------------------------------------------

    fn hex_word(v: u128) -> String {
        format!("{v:064x}")
    }

    /// One 32-byte ABI word as the RPC spells it: ordinary values, the awkward ones (array offsets,
    /// `u64::MAX` and `u128::MAX` lengths, high-half-set values), random hex, and 64-BYTE strings
    /// carrying a multi-byte character at a position chosen to straddle the decoders' byte slices
    /// (`[..32]` in `word_u128`, `[24..]` in `word_address`).
    fn arb_word() -> BoxedStrategy<String> {
        prop_oneof![
            4 => prop::sample::select(vec![
                0u128,
                1,
                2,
                32,
                64,
                96,
                128,
                1_000_000,
                u128::from(u64::MAX),
                u128::MAX,
                1u128 << 64,
                1u128 << 127,
            ])
            .prop_map(hex_word),
            2 => "[0-9a-f]{64}",
            1 => "[0-9a-fA-F]{0,70}",
            1 => prop_oneof![Just(31usize), Just(23usize), Just(0usize), 0usize..=62]
                .prop_map(|p| format!("{}é{}", "0".repeat(p), "0".repeat(62 - p))),
            1 => prop_oneof![
                Just(30usize), Just(31usize), Just(22usize), Just(23usize), 0usize..=61
            ]
            .prop_map(|p| format!("{}€{}", "0".repeat(p), "0".repeat(61 - p))),
        ]
        .boxed()
    }

    /// A log topic: a 32-byte word with `0x`, a bare word, or noise.
    fn arb_topic_word() -> BoxedStrategy<String> {
        prop_oneof![
            4 => "0x[0-9a-f]{64}",
            2 => arb_word().prop_map(|w| format!("0x{w}")),
            1 => any::<String>(),
        ]
        .boxed()
    }

    fn arb_quantity() -> BoxedStrategy<Value> {
        prop_oneof![
            2 => Just(json!("0x10")),
            2 => Just(json!("0xffffffffffffffff")),
            2 => Just(json!("0x7fffffffffffffff")),
            1 => Just(json!("0x0")),
            1 => arb_leaf(),
        ]
        .boxed()
    }

    /// A log shaped like `eth_getLogs` output carrying one of the eleven event topics the decoders
    /// dispatch on.
    fn arb_log() -> impl Strategy<Value = Value> {
        (
            prop::sample::select(vec![
                TOPIC_CONDITION_RESOLUTION,
                TOPIC_CTF_PAYOUT_REDEMPTION,
                TOPIC_NEG_RISK_PAYOUT_REDEMPTION,
                TOPIC_TRANSFER_SINGLE,
                TOPIC_TRANSFER_BATCH,
                TOPIC_ORDER_FILLED_V2,
                TOPIC_ORDER_FILLED_V1,
                TOPIC_CTF_POSITION_SPLIT,
                TOPIC_CTF_POSITION_MERGE,
                TOPIC_NEG_RISK_POSITIONS_CONVERTED,
                TOPIC_USDC_TRANSFER,
            ]),
            prop::collection::vec(arb_topic_word(), 0..4),
            prop::collection::vec(arb_word(), 0..8),
            prop_oneof![Just(String::new()), "[0-9a-f]{1,40}"],
            (arb_quantity(), arb_quantity()),
        )
            .prop_map(|(t0, topics, words, tail, (block, block_ts))| {
                let mut all = vec![json!(t0)];
                all.extend(topics.into_iter().map(Value::String));
                json!({
                    "topics": all,
                    "data": format!("0x{}{tail}", words.concat()),
                    "transactionHash": "0xabc",
                    "blockNumber": block,
                    "blockTimestamp": block_ts
                })
            })
    }

    fn arb_u128() -> impl Strategy<Value = u128> {
        prop_oneof![
            Just(0u128),
            Just(1u128),
            Just(1_000_000u128),
            Just(u128::from(u64::MAX)),
            Just(u128::MAX),
            any::<u128>(),
        ]
    }

    fn arb_header_name() -> BoxedStrategy<String> {
        prop_oneof![
            4 => prop::sample::select(vec![
                "Poly-RateLimit-Warning",
                "poly-ratelimit-order-remaining",
                "Poly-RateLimit-Cancel-Remaining",
                "POLY-RATELIMIT-RESET",
            ])
            .prop_map(|s| s.to_string()),
            1 => any::<String>(),
        ]
        .boxed()
    }

    fn arb_header_value() -> impl Strategy<Value = String> {
        prop_oneof![arb_numeric_string(), any::<String>(), Just("false".to_string())]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// (c) A random op sequence against ONE registry and ONE dust-snap tracker: orders are
        /// accepted, removed (demoted to the settling grace) and registered while arbitrary
        /// user-channel frames and history replays are decoded through them. No event flood: one
        /// frame addresses at most a handful of orders, two events per matched order.
        #[test]
        fn user_channel_sequences_fold_through_one_registry(
            ops in prop::collection::vec(arb_user_op(), 1..8),
            threshold in prop_oneof![
                Just(0.0f64), Just(0.05f64), Just(f64::NAN), Just(-1.0f64), Just(f64::INFINITY)
            ],
        ) {
            let reg = PolymarketRegistry::new();
            let tracker = FillTracker::with_config(4, threshold);
            for op in &ops {
                match op {
                    UserOp::Accept { slot, side } => {
                        let (coid, clob) = SLOTS[*slot];
                        let _ = reg.on_accept(coid, clob, *side);
                    }
                    UserOp::Remove { slot } => reg.remove(SLOTS[*slot].0),
                    UserOp::Register { slot, qty } => tracker.register(SLOTS[*slot].0, *qty),
                    UserOp::Frame(frame) => {
                        let tracked = decode_user_with_tracker(frame, &reg, Some(&tracker));
                        prop_assert!(tracked.len() <= 512, "event flood: {} events", tracked.len());
                        let plain = decode_user(frame, &reg);
                        prop_assert!(plain.len() <= 512, "event flood: {} events", plain.len());
                    }
                    UserOp::History { trades, orders } => {
                        let evs = map_polymarket_history(trades, orders, &reg);
                        prop_assert!(evs.len() <= 1024, "event flood: {} events", evs.len());
                    }
                }
            }
        }

        /// (a) The user channel over raw text: whatever parses is decoded.
        #[test]
        fn user_channel_is_total_over_arbitrary_text(text in arb_text(USER_KEYS)) {
            let reg = seeded_registry();
            if let Ok(v) = serde_json::from_str::<Value>(&text) {
                let _ = decode_user(&v, &reg);
                let _ = map_polymarket_history(&v, &v, &reg);
            }
        }

        /// (a)+(b) The reconcile parsers and `/positions` over shaped bodies (bare or `data`
        /// wrapped) and arbitrary JSON.
        #[test]
        fn recon_parsers_are_total_over_json(
            orders in arb_rows(arb_recon_order()),
            trades in arb_rows(arb_user_trade()),
            positions in arb_rows(arb_recon_position()),
            balance in prop_oneof![
                arb_wire_num().prop_map(|b| json!({ "balance": b })),
                arb_json(RECON_KEYS),
            ],
            generic in arb_json(RECON_KEYS),
            status in any::<String>(),
            matched in arb_f64(),
            original in arb_f64(),
        ) {
            let reg = seeded_registry();
            for v in [&orders, &trades, &positions, &balance, &generic] {
                let _ = parse_order_reports(v, |id: &str| reg.lookup_clob(id));
                let _ = parse_fill_reports(v, |id: &str| reg.lookup_clob(id));
                let _ = parse_position_reports(v);
                let _ = parse_balance(v);
                let _ = parse_positions(v);
            }
            let _ = normalize_order_status(&status, matched, original);
        }

        /// (a) The order-scoring parsers and the query builders.
        #[test]
        fn scoring_parsers_are_total(
            v in arb_json(SMALL_KEYS),
            ids in prop::collection::vec(any::<String>(), 0..4),
        ) {
            let _ = parse_order_scoring(&v);
            let _ = parse_orders_scoring(&v);
            let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
            let _ = orders_scoring_query(&refs);
            let _ = order_scoring_query(refs.first().copied().unwrap_or_default());
        }

        /// (c) Rate-limit headers (hostile names and values) fold into ONE `RateBudget` across a
        /// run of requests, with submits and cancels interleaved. `now` is the local monotonic
        /// clock, so it stays small; only the venue-supplied numbers are hostile.
        #[test]
        fn rate_limit_headers_fold_into_the_budget(
            batches in prop::collection::vec(
                prop::collection::vec((arb_header_name(), arb_header_value()), 0..5),
                1..8,
            ),
            tier in prop_oneof![Just(Tier::Standard), Just(Tier::Copper), Just(Tier::Silver)],
            steps in prop::collection::vec(0i64..5_000, 1..8),
        ) {
            let mut budget = RateBudget::new(tier, 0);
            let mut now = 0i64;
            for (batch, step) in batches.iter().zip(steps.iter().cycle()) {
                now += *step;
                let signal = parse_headers(batch.iter().map(|(k, v)| (k.as_str(), v.as_str())));
                let _ = signal.is_actionable();
                budget.reconcile(&signal, now);
                let _ = budget.submit_decision(now);
                budget.on_submit(now);
                let _ = budget.routine_cancel_decision(now);
                budget.on_cancel(now);
                let _ = budget.cancel_strategy(3, now);
                budget.on_cancel_all(2, now);
                let _ = (budget.cancel_available(now), budget.order_available(now));
            }
        }

        /// (a)+(b) Every Polygon log decoder over shaped logs (real topic0s, ABI-word data, hostile
        /// words and quantities) and arbitrary keyed JSON, and the pure hex helpers over text.
        ///
        /// ⚠ KNOWN FAILING until the chain decoders are hardened (see the four reproducers
        /// below): `word_u128`/`word_address` slice a String by byte offset, `dyn_array` allocates
        /// `Vec::with_capacity` from a wire-supplied length, and the `blockTimestamp * 1000` is a
        /// plain `i64` multiply.
        #[test]
        fn chain_log_decoders_are_total(
            log in prop_oneof![5 => arb_log(), 1 => arb_json(LOG_KEYS)],
            text in prop_oneof![arb_word(), any::<String>()],
        ) {
            let _ = decode_ctf_redemption(&log);
            let _ = decode_neg_risk_redemption(&log);
            let _ = decode_redemption(&log);
            let _ = decode_condition_resolution(&log);
            let _ = decode_token_transfer(&log);
            let _ = decode_order_fill_v1(&log);
            let _ = decode_order_fill_v2(&log);
            let _ = decode_ctf_position_split(&log);
            let _ = decode_ctf_position_merge(&log);
            let _ = decode_positions_converted(&log);
            let _ = decode_usdc_transfer(&log);
            let _ = decode_uint_result(&text);
            let _ = data_words(&text);
            let _ = word_u128(&text);
            let _ = word_address(&text);
            let _ = u256_word_to_decimal(&text);
        }

        /// (c) The settlement join (a redemption, the funder's token outflows, optionally the
        /// chain's payout vector) over hostile amounts, and the reconcile fill it feeds.
        #[test]
        fn join_settlement_is_total(
            payout in arb_f64(),
            venue_ctf in any::<bool>(),
            transfers in prop::collection::vec(
                (
                    prop::collection::vec("[0-9]{1,78}", 0..4),
                    prop::collection::vec(arb_u128(), 0..4),
                ),
                0..4,
            ),
            resolution in prop::option::of((arb_u128(), prop::collection::vec(arb_u128(), 0..4))),
        ) {
            let redemption = Redemption {
                venue: if venue_ctf { RedeemVenue::Ctf } else { RedeemVenue::NegRisk },
                redeemer: String::new(),
                condition_id: "0xcond".to_string(),
                payout_usdc: payout,
                slot_values: Vec::new(),
                tx_hash: String::new(),
                block: 0,
                ts_ms: 0,
            };
            let transfers: Vec<TokenTransfer> = transfers
                .into_iter()
                .map(|(ids, values)| TokenTransfer {
                    operator: String::new(),
                    from: String::new(),
                    to: String::new(),
                    ids,
                    values,
                    tx_hash: String::new(),
                    block: 0,
                })
                .collect();
            let resolution = resolution.map(|(denominator, numerators)| ChainResolution {
                condition_id: "0xcond".to_string(),
                denominator,
                numerators,
            });
            for s in join_settlement(&redemption, &transfers, resolution.as_ref()) {
                let _ = settlement_fill_report(&s);
            }
        }
    }

    /// ⚠ KNOWN FAILING reproducer: `chain::word_u128` checks `w.len() != 64` in BYTES and then
    /// slices `&w[..32]`. A 64-byte word with a 2-byte character at bytes 31..33 passes the length
    /// check and panics on the non-char-boundary slice. Reachable from every log decoder (topics
    /// and data words are copied from the RPC response). Fix: validate hex digits first, or use
    /// `w.get(..32)`.
    #[test]
    fn word_u128_with_a_multibyte_char_across_the_split_does_not_panic() {
        let word = format!("{}é{}", "0".repeat(31), "0".repeat(31));
        assert_eq!(word.len(), 64);
        let _ = word_u128(&word);
    }

    /// ⚠ KNOWN FAILING reproducer: `chain::word_address` slices `&w[24..]` after the same byte
    /// length check; a 2-byte character at bytes 23..25 panics it.
    #[test]
    fn word_address_with_a_multibyte_char_across_the_split_does_not_panic() {
        let word = format!("{}é{}", "0".repeat(23), "0".repeat(39));
        assert_eq!(word.len(), 64);
        let _ = word_address(&word);
    }

    /// A syntactically valid CTF `PayoutRedemption` log whose data is `[conditionId, 0x60, payout,
    /// ..tail]`, for the reproducers below.
    fn redemption_log(tail: &[String], block_timestamp: &str) -> Value {
        let topic = format!("0x{}", "0".repeat(64));
        let mut words = vec![hex_word(0xabc), hex_word(0x60), hex_word(0)];
        words.extend(tail.iter().cloned());
        json!({
            "topics": [TOPIC_CTF_PAYOUT_REDEMPTION, topic],
            "data": format!("0x{}", words.concat()),
            "transactionHash": "0xabc",
            "blockNumber": "0x1",
            "blockTimestamp": block_timestamp
        })
    }

    /// ⚠ KNOWN FAILING reproducer: `chain::dyn_array` (and `dyn_array_ids`) read the array length
    /// from the wire (`word_u128(..) as usize`) and call `Vec::with_capacity(len)` before checking
    /// that many words exist. A length of `u64::MAX` is a "capacity overflow" panic; a few billion
    /// is a multi-gigabyte allocation (an abort). Fix: cap the capacity at `words.len()`.
    #[test]
    fn a_huge_dynamic_array_length_does_not_allocate_or_panic() {
        let log = redemption_log(&[hex_word(u128::from(u64::MAX))], "0x1");
        let _ = decode_ctf_redemption(&log);
    }

    /// ⚠ KNOWN FAILING reproducer: every log decoder computes `log_u64(.., "blockTimestamp") as
    /// i64 * 1000` in plain `i64`; a timestamp above ~9.2e15 overflows (debug panic).
    #[test]
    fn a_huge_block_timestamp_does_not_overflow() {
        let log = redemption_log(&[hex_word(0)], "0x7fffffffffffffff");
        let _ = decode_ctf_redemption(&log);
    }
}
