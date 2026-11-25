//! `route_frame` reads a frame through a typed scan first and falls back to the `Value` decoder
//! (`crates/bridges/binance/src/family/feed/decode.rs`). The contract is that the answer is the SAME
//! on every input, so this file keeps the OLD `route_frame` — verbatim, with its own copies of the
//! two decoders and of `apply_depth_event`, so nothing in it can drift with the production code —
//! as the ORACLE and requires, for every frame, the same `DataClientEvent` AND the same book afterwards
//! (every level and `last_sequence`, bit for bit, so a `NaN` quantity compares equal to itself).
//!
//! Three lanes: (a) a proptest over frames built TEXTUALLY (a `serde_json::Value` would sort the
//! keys and hide key order, repeated keys and escapes) with every field optionally absent or of the
//! wrong type, repeated, key-escaped, in another order, and envelopes that are truncated, doubled,
//! trailed by text, nested past the recursion limit or carry a lone surrogate / `1e999` in a field
//! nobody reads (the two inputs on which `IgnoredAny` validates less than `Value`); (b) arbitrary
//! printable text and byte noise; (c) every deterministic `frame_mutations` of a canonical frame of
//! each kind plus of every committed captured fixture.
//!
//! A counterexample here is a REAL divergence: fix the scan, never the oracle.

use std::path::PathBuf;

use proptest::prelude::*;
use serde_json::Value;
use vike_binance::feed::tick::{DepthOutcome, route_frame};
use vike_bridge_core::capture::{frame_mutations, load_captured};
use vike_bridge_core::data_client::event::DataClientEvent;
use vike_bridge_core::depth::parse_levels;
use vike_bridge_core::typed_frame_kit::{
    ANY_WEIRD, DEEP_120, LEVELS_WEIRD, TIMESTAMP_WEIRD, arb_slot, key_text, levels_text, seeded,
    shuffle, wire_text,
};
use vike_marketdata::{L2Book, TickQuote, TickTrade};

const SYMBOL: &str = "BTCUSDT";

// ---------------------------------------------------------------------------------------------
// The oracle: `route_frame` as it was before the typed scan (copied, not imported)
// ---------------------------------------------------------------------------------------------

fn f(v: Option<&Value>) -> Option<f64> {
    v.and_then(|x| x.as_str()).and_then(|s| s.parse::<f64>().ok())
}

/// `ts` follows the decoder's one deliberate change since the copy (2026-11-09): the frame's `E`, else its `T`,
/// else 0, so a futures quote carries the venue's time into the core's price board.
fn old_decode_book_ticker(data: &Value, symbol: &str) -> Option<TickQuote> {
    Some(TickQuote {
        timestamp: data
            .get("E")
            .and_then(Value::as_i64)
            .or_else(|| data.get("T").and_then(Value::as_i64))
            .unwrap_or(0),
        local_timestamp: 0,
        bid: f(data.get("b"))?,
        ask: f(data.get("a"))?,
        bid_size: f(data.get("B")).unwrap_or(0.0),
        ask_size: f(data.get("A")).unwrap_or(0.0),
        symbol: symbol.into(),
    })
}

/// Follows the decoder's deliberate change since the copy (2026-11-11): a print the workspace's one trade
/// guard refuses (`vike_bridge_core::is_valid_trade`: a zero, negative, NaN or infinite price or size) is
/// no print, on the scan and on the `Value` path alike.
fn old_decode_trade(data: &Value, symbol: &str) -> Option<TickTrade> {
    let (price, size) = (f(data.get("p"))?, f(data.get("q"))?);
    if !vike_bridge_core::is_valid_trade(price, size) {
        return None;
    }
    Some(TickTrade {
        timestamp: data.get("T").and_then(Value::as_i64).unwrap_or(0),
        local_timestamp: 0,
        price,
        size,
        is_buyer_maker: data.get("m").and_then(Value::as_bool).unwrap_or(false),
        symbol: symbol.into(),
    })
}

fn old_apply_depth_event(book: &mut L2Book, data: &Value) -> DepthOutcome {
    let (Some(first_u), Some(final_u)) =
        (data.get("U").and_then(Value::as_u64), data.get("u").and_then(Value::as_u64))
    else {
        return DepthOutcome::Ignored;
    };
    if final_u <= book.last_sequence {
        return DepthOutcome::Stale;
    }
    let contiguous = match data.get("pu").and_then(Value::as_u64) {
        Some(previous_final_u) => {
            previous_final_u == book.last_sequence || first_u <= book.last_sequence
        }
        None => first_u <= book.last_sequence + 1,
    };
    if !contiguous {
        return DepthOutcome::Gap;
    }
    let bids = parse_levels(data.get("b"));
    let asks = parse_levels(data.get("a"));
    book.apply_delta(final_u, &bids, &asks);
    DepthOutcome::Applied
}

fn old_route_frame(text: &str, symbol: &str, book: &mut L2Book) -> DataClientEvent {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return DataClientEvent::Ignored;
    };
    let (Some(stream), Some(data)) = (v.get("stream").and_then(Value::as_str), v.get("data"))
    else {
        return DataClientEvent::Ignored;
    };
    if stream.ends_with("@bookTicker") {
        old_decode_book_ticker(data, symbol)
            .map_or(DataClientEvent::Ignored, DataClientEvent::Quote)
    } else if stream.ends_with("@trade") {
        old_decode_trade(data, symbol).map_or(DataClientEvent::Ignored, DataClientEvent::Trade)
    } else if stream.contains("@depth") {
        match old_apply_depth_event(book, data) {
            DepthOutcome::Applied => DataClientEvent::BookUpdated { timestamp_ms: 0 },
            DepthOutcome::Gap => DataClientEvent::Resync,
            _ => DataClientEvent::Ignored,
        }
    } else {
        DataClientEvent::Ignored
    }
}

// ---------------------------------------------------------------------------------------------
// The comparison
// ---------------------------------------------------------------------------------------------

/// The new decode and the oracle, from the same book, on the same text: same event, same book.
fn agree(text: &str, last_sequence: u64) -> Result<(), TestCaseError> {
    vike_bridge_core::typed_frame_kit::agree(
        text,
        last_sequence,
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
    Bool,
    Levels,
    Any,
}

const TICKER: [(&str, Ty); 9] = [
    ("b", Ty::Dec),
    ("a", Ty::Dec),
    ("B", Ty::Dec),
    ("A", Ty::Dec),
    ("u", Ty::Id),
    ("s", Ty::Any),
    ("e", Ty::Any),
    ("E", Ty::Ts),
    ("T", Ty::Ts),
];
const TRADE: [(&str, Ty); 8] = [
    ("p", Ty::Dec),
    ("q", Ty::Dec),
    ("T", Ty::Ts),
    ("m", Ty::Bool),
    ("t", Ty::Id),
    ("s", Ty::Any),
    ("e", Ty::Any),
    ("E", Ty::Ts),
];
const DEPTH: [(&str, Ty); 8] = [
    ("U", Ty::Id),
    ("u", Ty::Id),
    ("pu", Ty::Id),
    ("b", Ty::Levels),
    ("a", Ty::Levels),
    ("s", Ty::Any),
    ("e", Ty::Any),
    ("E", Ty::Ts),
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
    r#""1_0""#,
    r#""0x10""#,
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
    "1E1",
    "00",
];
const BOOL_WEIRD: &[&str] = &["true", "false", "null", "0", "1", r#""true""#, "[]", "{}"];

#[derive(Debug, Clone)]
struct Spec {
    kind: u8,
    stream_variant: u8,
    /// per slot: `None` = key absent, `Some(0)` = canonical value, `Some(n)` = the n-th weird one
    slots: Vec<Option<u8>>,
    order_seed: u64,
    dup: Option<(u8, u8)>,
    esc_key: Option<u8>,
    env: u8,
    last_sequence: u64,
    shift: u8,
    span: u8,
    pu_mode: u8,
    n_levels: u8,
    c: u8,
}

fn arb_spec() -> impl Strategy<Value = Spec> {
    let shape = (
        0u8..6,
        prop_oneof![8 => Just(0u8), 1 => 1u8..6],
        prop::collection::vec(arb_slot(), 9),
        any::<u64>(),
        prop::option::weighted(0.15, (0u8..8, 0u8..40)),
        prop::option::weighted(0.1, 0u8..8),
    );
    let env = (
        prop_oneof![10 => Just(0u8), 1 => 1u8..24],
        prop_oneof![3 => Just(0u64), 3 => 1u64..8, 1 => Just(u64::MAX), 1 => 4_000_000_000u64..4_000_000_010],
        0u8..5,
        0u8..4,
        0u8..3,
        0u8..14,
        any::<u8>(),
    );
    (shape, env).prop_map(
        |(
            (kind, stream_variant, slots, order_seed, dup, esc_key),
            (env, last_sequence_value, shift, span, pu_mode, n_levels, c),
        )| Spec {
            kind,
            stream_variant,
            slots,
            order_seed,
            dup,
            esc_key,
            env,
            last_sequence: last_sequence_value,
            shift,
            span,
            pu_mode,
            n_levels,
            c,
        },
    )
}

fn value_text(ty: Ty, key: &str, variant: u8, spec: &Spec) -> String {
    let first = spec.last_sequence.saturating_add(u64::from(spec.shift));
    let final_u = first.saturating_add(u64::from(spec.span));
    if variant == 0 {
        return match ty {
            Ty::Dec => format!(r#""{}.{}""#, 90 + spec.c % 20, spec.c % 10),
            Ty::Id => match key {
                "U" => first.to_string(),
                "pu" => match spec.pu_mode {
                    0 => spec.last_sequence.to_string(),
                    1 => spec.last_sequence.saturating_sub(1).to_string(),
                    _ => spec.last_sequence.saturating_add(3).to_string(),
                },
                _ => final_u.to_string(),
            },
            Ty::Ts => (1_700_000_000_000_i64 + i64::from(spec.c)).to_string(),
            Ty::Bool => spec.c.is_multiple_of(2).to_string(),
            Ty::Levels => levels_text(spec.n_levels, spec.c, key == "b"),
            Ty::Any => r#""BTCUSDT""#.to_string(),
        };
    }
    let weird = match ty {
        Ty::Dec => DEC_WEIRD,
        Ty::Id => ID_WEIRD,
        Ty::Ts => TIMESTAMP_WEIRD,
        Ty::Bool => BOOL_WEIRD,
        Ty::Levels => LEVELS_WEIRD,
        Ty::Any => ANY_WEIRD,
    };
    weird[(variant as usize - 1) % weird.len()].to_string()
}

fn render(spec: &Spec) -> String {
    let (stream, slots): (&str, &[(&str, Ty)]) = match spec.kind {
        0 => ("btcusdt@bookTicker", &TICKER),
        1 => ("btcusdt@trade", &TRADE),
        2 | 3 => ("btcusdt@depth@100ms", &DEPTH),
        4 => ("btcusdt@kline_1m", &TICKER),
        _ => ("btcusdt@trade@depth", &DEPTH),
    };
    let mut pairs: Vec<String> = Vec::new();
    for (index, (key, ty)) in slots.iter().enumerate() {
        // kind 2 = futures grammar (pu sent), kind 3 = spot grammar (pu never sent)
        if spec.kind == 3 && *key == "pu" {
            continue;
        }
        if let Some(Some(variant)) = spec.slots.get(index) {
            let escaped = spec.esc_key == Some(index as u8);
            pairs.push(format!(
                "{}:{}",
                key_text(key, escaped),
                value_text(*ty, key, *variant, spec)
            ));
        }
    }
    if let Some((slot, variant)) = spec.dup {
        let (key, ty) = slots[usize::from(slot) % slots.len()];
        pairs.push(format!("{}:{}", key_text(key, false), value_text(ty, key, variant, spec)));
    }
    shuffle(&mut pairs, spec.order_seed);
    let data = format!("{{{}}}", pairs.join(","));
    let stream_json = match spec.stream_variant {
        0 => format!("\"{stream}\""),
        1 => format!("\"{}\"", stream.replacen('t', "\\u0074", 1)),
        2 => "1".to_string(),
        3 => "null".to_string(),
        4 => r#""""#.to_string(),
        _ => format!("\"{stream}\\n\""),
    };
    let deep = |n: usize| format!("{}1{}", "[".repeat(n), "]".repeat(n));
    match spec.env {
        0 => format!(r#"{{"stream":{stream_json},"data":{data}}}"#),
        1 => format!(r#"{{"data":{data},"stream":{stream_json}}}"#),
        2 => format!(r#"{{"stream":{stream_json},"data":{data},"x":1}}"#),
        3 => format!(r#"{{"x":[1,{{"y":null}}],"stream":{stream_json},"data":{data}}}"#),
        4 => format!(r#"{{"stream":{stream_json},"data":{data}}} trailing"#),
        5 => format!(r#"{{"stream":{stream_json}}}"#),
        6 => format!(r#"{{"data":{data}}}"#),
        7 => format!(r#"{{"stream":{stream_json},"stream":"x@kline","data":{data}}}"#),
        8 => format!("[{stream_json},{data}]"),
        9 => format!(r#"{{"stream":{stream_json},"data":{data}}}"#),
        10 => format!("  {{\n \"stream\" : {stream_json} ,\n\t\"data\" :\r\n {data}\n}}  "),
        11 => format!(r#"{{"stream":{stream_json},"data":null}}"#),
        12 => format!(r#"{{"stream":{stream_json},"data":[1,2]}}"#),
        13 => format!(r#"{{"stream":{stream_json},"data":{data},"data":{data}}}"#),
        14 => format!("\u{feff}{{\"stream\":{stream_json},\"data\":{data}}}"),
        15 => "{}".to_string(),
        16 => "null".to_string(),
        17 => {
            format!(
                r#"{{"stream":{stream_json},"data":{data}}}{{"stream":{stream_json},"data":{data}}}"#
            )
        }
        18 => format!(r#"{{"stream":{stream_json},"data":{data},"z":{}}}"#, deep(DEEP_120)),
        19 => format!(r#"{{"stream":{stream_json},"data":{data},"z":{}}}"#, deep(DEEP_120 + 20)),
        20 => format!(r#"{{"stream":{stream_json},"data":{data},"z":"\ud800"}}"#),
        21 => format!(r#"{{"stream":{stream_json},"data":{data},"z":1e999}}"#),
        22 => format!(r#"{{"stream":{stream_json},"data":{data},"z":"😀"}}"#),
        _ => format!(r#"{{"stream":{stream_json},"data":{data}}}"#),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// The typed scan + fallback answer exactly as the `Value` decoder did, on frames that are
    /// canonical, nearly canonical and hostile in every way the generator knows.
    #[test]
    fn route_frame_matches_the_value_decoder(spec in arb_spec()) {
        agree(&render(&spec), spec.last_sequence)?;
    }

    /// (b) arbitrary printable text.
    #[test]
    fn route_frame_matches_the_value_decoder_on_text(text in "[ -~]{0,200}", last_sequence in 0u64..10) {
        agree(&text, last_sequence)?;
    }

    /// (b') byte noise (controls, newlines, lossy-decoded invalid UTF-8).
    #[test]
    fn route_frame_matches_the_value_decoder_on_byte_noise(
        bytes in prop::collection::vec(any::<u8>(), 0..200),
        last_sequence in 0u64..10,
    ) {
        agree(&String::from_utf8_lossy(&bytes), last_sequence)?;
    }
}

// ---------------------------------------------------------------------------------------------
// (c) deterministic corpus
// ---------------------------------------------------------------------------------------------

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/captured")
}

/// One real-shaped frame of each kind the pump consumes (the synthesized field sets the crate's
/// `route_frame` bench uses: `e`/`E`/`s` context fields included), futures and spot depth.
fn canonical_frames() -> Vec<Value> {
    let depth = |extra: Value| {
        let mut data = serde_json::json!({
            "e": "depthUpdate", "E": 1_700_000_000_000_i64, "s": "BTCUSDT",
            "U": 6, "u": 9,
            "b": [["100.00", "1.5"], ["99.50", "0"], ["98.00", "4"]],
            "a": [["100.50", "2"], ["101.00", "0"]]
        });
        if let (Some(d), Value::Object(x)) = (data.as_object_mut(), extra) {
            d.extend(x);
        }
        serde_json::json!({"stream": "btcusdt@depth@100ms", "data": data})
    };
    vec![
        serde_json::json!({"stream": "btcusdt@bookTicker", "data": {
            "u": 400_000_001_u64, "s": "BTCUSDT", "b": "60000.10", "B": "1.50000",
            "a": "60000.20", "A": "2.25000"}}),
        // The futures shape: `e`, `E` and `T` beside the book fields.
        serde_json::json!({"stream": "btcusdt@bookTicker", "data": {
            "e": "bookTicker", "u": 400_900_217_u64, "E": 1_568_014_460_893_i64,
            "T": 1_568_014_460_891_i64, "s": "BTCUSDT", "b": "60000.10", "B": "1.50000",
            "a": "60000.20", "A": "2.25000"}}),
        serde_json::json!({"stream": "btcusdt@trade", "data": {
            "e": "trade", "E": 1_700_000_000_000_i64, "s": "BTCUSDT", "t": 3_000_000_001_u64,
            "p": "60000.10", "q": "0.00100", "T": 1_700_000_000_001_i64, "m": true, "M": true}}),
        depth(serde_json::json!({})),
        depth(serde_json::json!({"pu": 5})),
    ]
}

/// Every deterministic near-miss of a canonical frame of each kind, and of every committed
/// captured fixture, from a book anchored before, at and past the diff.
#[test]
fn every_mutation_of_every_canonical_and_captured_frame_agrees() {
    let mut texts: Vec<String> = Vec::new();
    for frame in canonical_frames() {
        texts.push(wire_text(&frame, "stream"));
        texts.extend(frame_mutations(&frame).iter().map(|m| wire_text(m, "stream")));
    }
    for kind in ["ws_accepted", "ws_canceled", "ws_fill", "ws_account_state"] {
        let fx = load_captured(&fixtures_dir(), kind)
            .unwrap_or_else(|| panic!("committed fixture {kind}.json is missing"));
        for frame in &fx.frames {
            texts.push(frame.to_string());
            texts.extend(frame_mutations(frame).iter().map(Value::to_string));
        }
    }
    assert!(texts.len() > 300, "only {} frames — the corpus is broken", texts.len());

    let mut seen = std::collections::BTreeSet::new();
    for text in &texts {
        for last_sequence in [0, 5, 6, 9, 1000] {
            agree(text, last_sequence).unwrap_or_else(|e| panic!("{e}"));
            let mut book = seeded(last_sequence);
            let tag = match old_route_frame(text, SYMBOL, &mut book) {
                DataClientEvent::Quote(_) => "quote",
                DataClientEvent::Trade(_) => "trade",
                DataClientEvent::BookUpdated { .. } => "book",
                DataClientEvent::Resync => "resync",
                DataClientEvent::Ignored => "ignored",
                other => unreachable!("binance's route answers no {other:?}"),
            };
            seen.insert(tag);
        }
    }
    // The corpus must reach every arm, or "they agree" says nothing about the arms it missed.
    assert_eq!(
        seen.into_iter().collect::<Vec<_>>(),
        ["book", "ignored", "quote", "resync", "trade"],
        "the mutation corpus no longer reaches every DataClientEvent arm"
    );
}

/// The specific inputs on which a derived/`IgnoredAny` reader would have diverged from `Value`,
/// pinned by name so a regression names its cause. Each is a frame the oracle drops.
#[test]
fn the_inputs_ignored_any_would_accept_are_still_dropped() {
    let deep = format!(r#","s":{}1{}}}"#, "[".repeat(129), "]".repeat(129));
    let tails = [
        r#","s":"\ud800"}"#.to_string(), // lone surrogate in a field nobody reads
        r#","s":1e999}"#.to_string(),    // out-of-range number in a field nobody reads
        deep,                            // nested one past serde_json's recursion limit (128)
    ];
    for tail in tails {
        let text = format!(
            r#"{{"stream":"btcusdt@trade","data":{{"p":"60000.1","q":"0.5","T":1,"m":false{tail}}}"#
        );
        let mut book = seeded(0);
        assert_eq!(
            old_route_frame(&text, SYMBOL, &mut book),
            DataClientEvent::Ignored,
            "premise: the oracle drops this frame"
        );
        assert_eq!(route_frame(&text, SYMBOL, &mut seeded(0)), DataClientEvent::Ignored);
    }
}
