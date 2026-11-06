//! `route_frame` reads a frame through a typed scan first and falls back to the `Value` decoder
//! (`crates/bridges/bybit/src/market_scan.rs`). The contract is that the answer is the SAME on
//! every input, so this file keeps the OLD `route_frame` (its trade arm reads every row, as the
//! decoder now does), with its own copy of the trade decoder, as the ORACLE and requires, for every
//! frame, the same `MdEvent` AND the same book
//! afterwards (every level and `last_seq`, bit for bit, so a `NaN` quantity compares equal to
//! itself).
//!
//! Three lanes: (a) a proptest over frames built TEXTUALLY (a `serde_json::Value` would sort the
//! keys and hide key order, repeated keys and escapes) with every field optionally absent or of the
//! wrong type, repeated, key-escaped, in another order, and envelopes that are truncated, doubled,
//! trailed by text, nested past the recursion limit or carry a lone surrogate / `1e999` in a field
//! nobody reads (the two inputs on which `IgnoredAny` validates less than `Value`); (b) arbitrary
//! printable text and byte noise; (c) every deterministic `frame_mutations` of a canonical frame of
//! each kind.
//!
//! A counterexample here is a REAL divergence: fix the scan, never the oracle.

use proptest::prelude::*;
use serde_json::Value;
use vike_bridge_core::capture::frame_mutations;
use vike_bridge_core::depth::parse_levels;
use vike_bridge_core::typed_frame_kit::{
    ANY_WEIRD, DEEP_120, LEVELS_WEIRD, TS_WEIRD, arb_slot, key_text, levels_text, seeded, shuffle,
    wire_text,
};
use vike_bybit::market_data::{MdEvent, route_frame};
use vike_marketdata::{DeltaDecision, L2Book, SeqPolicy, TradeTick};

const SYMBOL: &str = "BTCUSDT";

// ---------------------------------------------------------------------------------------------
// The oracle: `route_frame` as it was before the typed scan (copied, not imported)
// ---------------------------------------------------------------------------------------------

fn f(v: Option<&Value>) -> Option<f64> {
    v.and_then(|x| x.as_str()).and_then(|s| s.parse::<f64>().ok())
}

fn old_decode_trade(d: &Value, symbol: &str) -> Option<TradeTick> {
    Some(TradeTick {
        ts: d.get("T").and_then(Value::as_i64).unwrap_or(0),
        local_ts: 0,
        price: f(d.get("p"))?,
        size: f(d.get("v"))?,
        is_buyer_maker: d.get("S").and_then(Value::as_str) == Some("Sell"),
        symbol: symbol.to_string(),
    })
}

fn old_route_frame(text: &str, symbol: &str, book: &mut L2Book) -> MdEvent {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return MdEvent::Ignored;
    };
    let Some(topic) = v.get("topic").and_then(Value::as_str) else {
        return MdEvent::Ignored;
    };
    if topic.starts_with("orderbook.") {
        let msg_type = v.get("type").and_then(Value::as_str).unwrap_or("");
        let Some(data) = v.get("data") else {
            return MdEvent::Ignored;
        };
        let u = data.get("u").and_then(Value::as_u64).unwrap_or(0);
        let (b, a) = (parse_levels(data.get("b")), parse_levels(data.get("a")));
        let ts_ms = v.get("ts").and_then(Value::as_i64).unwrap_or(0);
        match msg_type {
            "snapshot" => {
                book.apply_snapshot(u, &b, &a);
                MdEvent::BookUpdated { ts_ms }
            }
            "delta" => match book.delta_decision(u, SeqPolicy::Strict) {
                DeltaDecision::Apply => {
                    book.apply_delta(u, &b, &a);
                    MdEvent::BookUpdated { ts_ms }
                }
                DeltaDecision::Stale => MdEvent::Ignored,
                DeltaDecision::Gap => MdEvent::Resync,
            },
            _ => MdEvent::Ignored,
        }
    } else if topic.starts_with("publicTrade") {
        // EVERY row the oracle can read, in frame order (the one deliberate change to this copy:
        // the old decoder read only the first row)
        let trades: Vec<TradeTick> = match v.get("data").and_then(Value::as_array) {
            Some(rows) => rows.iter().filter_map(|d| old_decode_trade(d, symbol)).collect(),
            None => Vec::new(),
        };
        if trades.is_empty() { MdEvent::Ignored } else { MdEvent::Trades(trades) }
    } else {
        MdEvent::Ignored
    }
}

// ---------------------------------------------------------------------------------------------
// The comparison
// ---------------------------------------------------------------------------------------------

/// The new decode and the oracle, from the same book, on the same text: same event, same book.
fn agree(text: &str, last_seq: u64) -> Result<(), TestCaseError> {
    vike_bridge_core::typed_frame_kit::agree(
        text,
        last_seq,
        |t, b| old_route_frame(t, SYMBOL, b),
        |t, b| route_frame(t, SYMBOL, b),
    )
}

// ---------------------------------------------------------------------------------------------
// (a) textual frames
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Ty {
    Dec,
    Id,
    Ts,
    Side,
    Levels,
    Any,
}

const BOOK: [(&str, Ty); 5] =
    [("u", Ty::Id), ("b", Ty::Levels), ("a", Ty::Levels), ("s", Ty::Any), ("seq", Ty::Id)];
const TRADE: [(&str, Ty); 6] = [
    ("T", Ty::Ts),
    ("p", Ty::Dec),
    ("v", Ty::Dec),
    ("S", Ty::Side),
    ("s", Ty::Any),
    ("i", Ty::Any),
];

const DEC_WEIRD: &[&str] = &[
    r#""NaN""#,
    r#""inf""#,
    r#""-1.5""#,
    r#""1e999""#,
    r#""""#,
    r#""abc""#,
    r#""1e3""#,
    r#""60.5""#,
    "1.5",
    "null",
    "true",
    "[]",
    "{}",
    r#""\ud800""#,
    r#"" 1""#,
    r#""0""#,
    r#""-0""#,
    r#""+5""#,
];
const ID_WEIRD: &[&str] = &[
    "0",
    "1",
    "2",
    "3",
    "7",
    "18446744073709551615",
    "18446744073709551616",
    "-1",
    "1.5",
    "1e2",
    r#""5""#,
    "null",
    "true",
    "[]",
    "{}",
    "-0",
    "9223372036854775808",
    "00",
];
const SIDE_WEIRD: &[&str] =
    &[r#""Buy""#, r#""sell""#, r#""Sell""#, r#""""#, "1", "null", "true", "[]", "{}"];
const TYPE_WEIRD: &[&str] =
    &[r#""snapshot""#, r#""delta""#, r#""""#, r#""foo""#, "1", "null", "[]", r#""delta""#];
const TOPIC_BOOK: &[&str] = &["orderbook.50.BTCUSDT", "orderbook.1.BTCUSDT", "orderbook."];
const TOPIC_TRADE: &[&str] = &["publicTrade.BTCUSDT", "publicTrade"];
const TOPIC_OTHER: &[&str] = &["tickers.BTCUSDT", "orderbook", "kline.1.BTCUSDT", ""];

#[derive(Debug, Clone)]
struct Spec {
    /// 0 orderbook delta, 1 orderbook snapshot, 2 publicTrade, 3 another topic
    kind: u8,
    topic_variant: u8,
    /// per slot: `None` = key absent, `Some(0)` = canonical value, `Some(n)` = the n-th weird one
    slots: Vec<Option<u8>>,
    type_variant: Option<u8>,
    ts_variant: Option<u8>,
    n_trades: u8,
    second_trade_variant: u8,
    order_seed: u64,
    dup: Option<(u8, u8)>,
    esc_key: Option<u8>,
    env: u8,
    last_seq: u64,
    seq_mode: u8,
    n_levels: u8,
    c: u8,
}

fn arb_spec() -> impl Strategy<Value = Spec> {
    let shape = (
        0u8..4,
        prop_oneof![8 => Just(0u8), 1 => 1u8..4],
        prop::collection::vec(arb_slot(), 6),
        prop::option::weighted(0.9, prop_oneof![6 => Just(0u8), 3 => 1u8..10]),
        prop::option::weighted(0.8, prop_oneof![6 => Just(0u8), 3 => 1u8..20]),
        0u8..4,
        prop_oneof![3 => Just(0u8), 2 => 1u8..30],
    );
    let rest = (
        any::<u64>(),
        prop::option::weighted(0.15, (0u8..6, 0u8..40)),
        prop::option::weighted(0.1, 0u8..6),
        prop_oneof![10 => Just(0u8), 1 => 1u8..24],
        prop_oneof![3 => Just(0u64), 3 => 1u64..8, 1 => Just(u64::MAX), 1 => 4_000_000_000u64..4_000_000_010],
        0u8..5,
        0u8..14,
        any::<u8>(),
    );
    (shape, rest).prop_map(
        |(
            (kind, topic_variant, slots, type_variant, ts_variant, n_trades, second_trade_variant),
            (order_seed, dup, esc_key, env, last_seq, seq_mode, n_levels, c),
        )| Spec {
            kind,
            topic_variant,
            slots,
            type_variant,
            ts_variant,
            n_trades,
            second_trade_variant,
            order_seed,
            dup,
            esc_key,
            env,
            last_seq,
            seq_mode,
            n_levels,
            c,
        },
    )
}

fn value_text(ty: Ty, key: &str, variant: u8, spec: &Spec) -> String {
    if variant == 0 {
        return match ty {
            Ty::Dec => format!(r#""{}.{}""#, 90 + spec.c % 20, spec.c % 10),
            // Strict policy: +1 applies, +0 is a duplicate, anything else is a gap
            Ty::Id if key == "u" => {
                let u = match spec.seq_mode {
                    0 | 1 => spec.last_seq.saturating_add(1),
                    2 => spec.last_seq,
                    3 => spec.last_seq.saturating_add(2),
                    _ => 1,
                };
                u.to_string()
            }
            Ty::Id => (4_000_000 + u64::from(spec.c)).to_string(),
            Ty::Ts => (1_700_000_000_000_i64 + i64::from(spec.c)).to_string(),
            Ty::Side => {
                (if spec.c.is_multiple_of(2) { r#""Sell""# } else { r#""Buy""# }).to_string()
            }
            Ty::Levels => levels_text(spec.n_levels, spec.c, key == "b"),
            Ty::Any => r#""BTCUSDT""#.to_string(),
        };
    }
    let weird = match ty {
        Ty::Dec => DEC_WEIRD,
        Ty::Id => ID_WEIRD,
        Ty::Ts => TS_WEIRD,
        Ty::Side => SIDE_WEIRD,
        Ty::Levels => LEVELS_WEIRD,
        Ty::Any => ANY_WEIRD,
    };
    weird[(variant as usize - 1) % weird.len()].to_string()
}

/// A JSON object from the slots of `layout`, in a shuffled order, with the spec's repeated and
/// escaped keys.
fn object_text(spec: &Spec, layout: &[(&str, Ty)], variants: &[Option<u8>], seed: u64) -> String {
    let mut pairs: Vec<String> = Vec::new();
    for (idx, (key, ty)) in layout.iter().enumerate() {
        if let Some(Some(variant)) = variants.get(idx) {
            let escaped = spec.esc_key == Some(idx as u8);
            pairs.push(format!(
                "{}:{}",
                key_text(key, escaped),
                value_text(*ty, key, *variant, spec)
            ));
        }
    }
    if let Some((slot, variant)) = spec.dup {
        let (key, ty) = layout[usize::from(slot) % layout.len()];
        pairs.push(format!("{}:{}", key_text(key, false), value_text(ty, key, variant, spec)));
    }
    shuffle(&mut pairs, seed);
    format!("{{{}}}", pairs.join(","))
}

fn render(spec: &Spec) -> String {
    let topic = match spec.kind {
        0 | 1 => TOPIC_BOOK[usize::from(spec.topic_variant) % TOPIC_BOOK.len()],
        2 => TOPIC_TRADE[usize::from(spec.topic_variant) % TOPIC_TRADE.len()],
        _ => TOPIC_OTHER[usize::from(spec.topic_variant) % TOPIC_OTHER.len()],
    };
    let data = if spec.kind == 2 {
        let first = object_text(spec, &TRADE, &spec.slots, spec.order_seed);
        let mut items = vec![first];
        for n in 1..spec.n_trades {
            let variants: Vec<Option<u8>> = (0..TRADE.len())
                .map(
                    |i| if i == usize::from(n) { Some(spec.second_trade_variant) } else { Some(0) },
                )
                .collect();
            items.push(object_text(spec, &TRADE, &variants, spec.order_seed ^ u64::from(n)));
        }
        if spec.n_trades == 0 && spec.c.is_multiple_of(2) {
            items.clear();
        }
        format!("[{}]", items.join(","))
    } else {
        object_text(spec, &BOOK, &spec.slots, spec.order_seed)
    };
    let type_json = match spec.type_variant {
        None => String::new(),
        Some(v) => {
            let default = if spec.kind == 1 { r#""snapshot""# } else { r#""delta""# };
            let t = if v == 0 { default } else { TYPE_WEIRD[(v as usize - 1) % TYPE_WEIRD.len()] };
            format!(r#""type":{t},"#)
        }
    };
    let ts_json = match spec.ts_variant {
        None => String::new(),
        Some(v) => format!(r#""ts":{},"#, value_text(Ty::Ts, "ts", v, spec)),
    };
    let topic_json = format!("\"{topic}\"");
    let deep = |n: usize| format!("{}1{}", "[".repeat(n), "]".repeat(n));
    let head = format!(r#""topic":{topic_json},{type_json}{ts_json}"#);
    match spec.env {
        0 => format!(r#"{{{head}"data":{data},"cts":1}}"#),
        1 => format!(r#"{{"data":{data},{head}"cts":1}}"#),
        2 => format!(r#"{{{head}"data":{data},"x":1}}"#),
        3 => format!(r#"{{"x":[1,{{"y":null}}],{head}"data":{data}}}"#),
        4 => format!(r#"{{{head}"data":{data}}} trailing"#),
        5 => format!(r#"{{{head}"cts":1}}"#),
        6 => format!(r#"{{"data":{data}}}"#),
        7 => format!(r#"{{{head}"topic":"kline.1.X","data":{data}}}"#),
        8 => format!("[{topic_json},{data}]"),
        9 => format!(r#"{{"topic":{topic_json},{type_json}{ts_json}"data":{data}}}"#),
        10 => {
            format!("  {{\n \"topic\" : {topic_json} ,\n\t{type_json}\"data\" :\r\n {data}\n}}  ")
        }
        11 => format!(r#"{{{head}"data":null}}"#),
        12 => format!(r#"{{{head}"data":[1,2]}}"#),
        13 => format!(r#"{{{head}"data":{data},"data":{data}}}"#),
        14 => format!("\u{feff}{{{head}\"data\":{data}}}"),
        15 => "{}".to_string(),
        16 => "null".to_string(),
        17 => format!(r#"{{{head}"data":{data}}}{{{head}"data":{data}}}"#),
        18 => format!(r#"{{{head}"data":{data},"z":{}}}"#, deep(DEEP_120)),
        19 => format!(r#"{{{head}"data":{data},"z":{}}}"#, deep(DEEP_120 + 20)),
        20 => format!(r#"{{{head}"data":{data},"z":"\ud800"}}"#),
        21 => format!(r#"{{{head}"data":{data},"z":1e999}}"#),
        22 => format!(r#"{{{head}"data":{data},"z":"😀"}}"#),
        23 => format!(r#"{{{head}"type":"delta","data":{data}}}"#),
        _ => format!(r#"{{{head}"data":{data}}}"#),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// The typed scan + fallback answer exactly as the `Value` decoder did, on frames that are
    /// canonical, nearly canonical and hostile in every way the generator knows.
    #[test]
    fn route_frame_matches_the_value_decoder(spec in arb_spec()) {
        agree(&render(&spec), spec.last_seq)?;
    }

    /// (b) arbitrary printable text.
    #[test]
    fn route_frame_matches_the_value_decoder_on_text(text in "[ -~]{0,200}", last_seq in 0u64..10) {
        agree(&text, last_seq)?;
    }

    /// (b') byte noise (controls, newlines, lossy-decoded invalid UTF-8).
    #[test]
    fn route_frame_matches_the_value_decoder_on_byte_noise(
        bytes in prop::collection::vec(any::<u8>(), 0..200),
        last_seq in 0u64..10,
    ) {
        agree(&String::from_utf8_lossy(&bytes), last_seq)?;
    }
}

// ---------------------------------------------------------------------------------------------
// (c) deterministic corpus
// ---------------------------------------------------------------------------------------------

/// One real-shaped frame of each kind the pump consumes (Bybit V5 docs' field sets).
fn canonical_frames() -> Vec<Value> {
    let book = |kind: &str, u: u64| {
        serde_json::json!({
            "topic": "orderbook.50.BTCUSDT", "type": kind, "ts": 1_687_940_967_466_i64,
            "data": {"s": "BTCUSDT", "b": [["100.00", "1.5"], ["99.50", "0"], ["98.00", "4"]],
                     "a": [["100.50", "2"], ["101.00", "0"]], "u": u, "seq": 66_544_703_342_u64},
            "cts": 1_687_940_967_464_i64
        })
    };
    vec![
        book("snapshot", 10),
        book("delta", 6),
        serde_json::json!({
            "topic": "publicTrade.BTCUSDT", "type": "snapshot", "ts": 1_672_304_486_868_i64,
            "data": [
                {"T": 1_672_304_486_865_i64, "s": "BTCUSDT", "S": "Buy", "v": "0.001",
                 "p": "16578.50", "L": "PlusTick", "i": "20f43950", "BT": false},
                {"T": 1_672_304_486_866_i64, "s": "BTCUSDT", "S": "Sell", "v": "0.002",
                 "p": "16578.60", "i": "20f43951", "BT": false}
            ]
        }),
        serde_json::json!({"topic": "tickers.BTCUSDT", "data": {"symbol": "BTCUSDT"}}),
    ]
}

/// Every deterministic near-miss of a canonical frame of each kind, from a book anchored before,
/// at and past the diff.
#[test]
fn every_mutation_of_every_canonical_frame_agrees() {
    let mut texts: Vec<String> = Vec::new();
    for frame in canonical_frames() {
        texts.push(wire_text(&frame, "topic"));
        texts.extend(frame_mutations(&frame).iter().map(|m| wire_text(m, "topic")));
    }
    assert!(texts.len() > 100, "only {} frames — the corpus is broken", texts.len());

    let mut seen = std::collections::BTreeSet::new();
    for text in &texts {
        for last_seq in [0, 5, 6, 9, 10, 1000] {
            agree(text, last_seq).unwrap_or_else(|e| panic!("{e}"));
            let mut book = seeded(last_seq);
            let tag = match old_route_frame(text, SYMBOL, &mut book) {
                MdEvent::Trades(_) => "trade",
                MdEvent::BookUpdated { .. } => "book",
                MdEvent::Resync => "resync",
                MdEvent::Ignored => "ignored",
            };
            seen.insert(tag);
        }
    }
    // The corpus must reach every arm, or "they agree" says nothing about the arms it missed.
    assert_eq!(
        seen.into_iter().collect::<Vec<_>>(),
        ["book", "ignored", "resync", "trade"],
        "the mutation corpus no longer reaches every MdEvent arm"
    );
}

/// The specific inputs on which a derived/`IgnoredAny` reader would have diverged from `Value`,
/// pinned by name so a regression names its cause. Each is a frame the oracle drops.
#[test]
fn the_inputs_ignored_any_would_accept_are_still_dropped() {
    let deep = format!(r#","z":{}1{}}}"#, "[".repeat(129), "]".repeat(129));
    let tails = [
        r#","z":"\ud800"}"#.to_string(), // lone surrogate in a field nobody reads
        r#","z":1e999}"#.to_string(),    // out-of-range number in a field nobody reads
        deep,                            // nested one past serde_json's recursion limit (128)
    ];
    for tail in tails {
        let text = format!(
            r#"{{"topic":"publicTrade.BTCUSDT","data":[{{"p":"60000.1","v":"0.5","S":"Buy"{tail}]}}"#
        );
        let mut book = seeded(0);
        assert_eq!(
            old_route_frame(&text, SYMBOL, &mut book),
            MdEvent::Ignored,
            "premise: the oracle drops this frame"
        );
        assert_eq!(route_frame(&text, SYMBOL, &mut seeded(0)), MdEvent::Ignored);
    }
}
