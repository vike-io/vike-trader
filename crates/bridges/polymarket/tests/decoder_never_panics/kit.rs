//! The keyless kit of the Polymarket decoder harness: wire keys, strategies, the pump driver.

use std::sync::atomic::AtomicBool;

use proptest::prelude::*;
use serde_json::{Value, json};
use vike_bridge_core::scripted::{ScriptStep, ScriptedStream};
use vike_data::RecordingSink;
use vike_polymarket::{PumpMode, PumpTiming, RankBy, TokenSlot, UniversePolicy, run_shard_session};

pub(super) const TOKEN: &str = "111";
pub(super) const OTHER_TOKEN: &str = "222";
/// Per-slot §B freshness threshold for the scripted sessions (the Book/Quotes production value).
const FRESHNESS_MS: i64 = 300_000;

// ---- field names the decoders actually read (the keys the generators draw from) -------------------

pub(super) const MARKET_KEYS: &[&str] = &[
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
pub(super) const GAMMA_KEYS: &[&str] = &[
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
    "orderMinSize",
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
pub(super) const CLOB_KEYS: &[&str] = &[
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
pub(super) const EGRESS_KEYS: &[&str] = &["ip", "country", "city", "region", "blocked"];

// ---- leaf and JSON generators ------------------------------------------------------------------

/// i64 with the values that break arithmetic on a wire timestamp or count.
pub(super) fn arb_i64() -> impl Strategy<Value = i64> {
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
pub(super) fn arb_f64() -> impl Strategy<Value = f64> {
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
pub(super) fn arb_numeric_string() -> impl Strategy<Value = String> {
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
pub(super) fn arb_leaf() -> BoxedStrategy<Value> {
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
pub(super) fn arb_json(names: &'static [&'static str]) -> BoxedStrategy<Value> {
    arb_json_depth(names, 4)
}

/// The first half of `s` by CHARACTERS (a byte slice could split a code point in the harness
/// itself).
fn truncate_half(s: String) -> String {
    let keep = s.chars().count() / 2;
    s.chars().take(keep).collect()
}

/// A raw text frame: arbitrary unicode, lossy-decoded byte noise, valid JSON, and truncated JSON.
pub(super) fn arb_text(names: &'static [&'static str]) -> BoxedStrategy<String> {
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
pub(super) fn arb_wire_num() -> BoxedStrategy<Value> {
    prop_oneof![
        3 => arb_numeric_string().prop_map(Value::String),
        3 => (0u32..1000).prop_map(|n| Value::String(format!("0.{n:03}"))),
        1 => arb_leaf(),
    ]
    .boxed()
}

/// A frame timestamp: epoch-ms as a string (the live form), as a number, or garbage.
pub(super) fn arb_ts() -> BoxedStrategy<Value> {
    prop_oneof![
        3 => arb_i64().prop_map(|n| Value::String(n.to_string())),
        2 => arb_i64().prop_map(|n| Value::Number(n.into())),
        1 => arb_leaf(),
    ]
    .boxed()
}

pub(super) fn arb_side() -> BoxedStrategy<Value> {
    prop_oneof![
        3 => Just(json!("BUY")),
        3 => Just(json!("SELL")),
        1 => arb_leaf(),
    ]
    .boxed()
}

/// An `asset_id`: mostly one of the two subscribed tokens, so the router resolves a slot.
pub(super) fn arb_asset() -> BoxedStrategy<Value> {
    prop_oneof![
        8 => Just(json!(TOKEN)),
        2 => Just(json!(OTHER_TOKEN)),
        1 => arb_leaf(),
    ]
    .boxed()
}

pub(super) fn arb_mode() -> impl Strategy<Value = PumpMode> {
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
pub(super) fn arb_market_frame() -> BoxedStrategy<Value> {
    prop_oneof![
        6 => arb_shaped_frame(),
        2 => prop::collection::vec(arb_shaped_frame(), 0..4).prop_map(Value::Array),
        3 => arb_json(MARKET_KEYS),
    ]
    .boxed()
}

/// One inbound TEXT frame for the pump: a market frame, raw noise, or the CLOB `PONG` keepalive.
pub(super) fn arb_frame_text() -> BoxedStrategy<String> {
    prop_oneof![
        6 => arb_market_frame().prop_map(|v| v.to_string()),
        2 => arb_text(MARKET_KEYS),
        1 => Just("PONG".to_string()),
    ]
    .boxed()
}

/// Drive ONE scripted session of the real batched pump over two token slots. The script ending
/// surfaces as a stream-closed `Err`, which is the normal exit and is not asserted on.
pub(super) fn drive_session(mode: PumpMode, steps: Vec<ScriptStep>) -> RecordingSink {
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
pub(super) fn arb_activity_frame() -> BoxedStrategy<Value> {
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
pub(super) fn arb_rtds_frame() -> BoxedStrategy<Value> {
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

pub(super) fn arb_rtds_text() -> BoxedStrategy<String> {
    prop_oneof![
        6 => arb_rtds_frame().prop_map(|v| v.to_string()),
        2 => arb_text(RTDS_KEYS),
        1 => Just(String::new()),
        1 => Just("PONG".to_string()),
    ]
    .boxed()
}

// ---- Gamma and CLOB REST bodies -------------------------------------------------------------------

/// A JSON-string-encoded array (how Gamma ships `outcomes`/`clobTokenIds`/`outcomePrices`).
pub(super) fn arb_json_array_string() -> BoxedStrategy<Value> {
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
pub(super) fn arb_gamma_page() -> BoxedStrategy<Value> {
    prop_oneof![
        5 => prop::collection::vec(arb_gamma_market(), 0..4).prop_map(Value::Array),
        1 => arb_json(GAMMA_KEYS),
    ]
    .boxed()
}

/// An `/events` page: events carrying a `markets` array.
pub(super) fn arb_gamma_events() -> BoxedStrategy<Value> {
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
pub(super) fn arb_clob_page() -> BoxedStrategy<Value> {
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

pub(super) fn arb_policy() -> impl Strategy<Value = UniversePolicy> {
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
