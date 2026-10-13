//! Totality harness for the Dukascopy Rust-side wire decoders. This venue has no socket: its market
//! data is the vendor's LZMA-compressed `.bi5` tick archives (hourly over HTTP, daily from an
//! imported archive) and its execution is a Java sidecar speaking JSON lines on stdio, so the
//! "wire" here is (a) archive BYTES and archive PATHS, and (b) sidecar stdout LINES and the
//! commands written back.
//!
//! Every decoder is fed (a) arbitrary text / bytes and lossy-decoded byte noise, (b) LZMA-alone
//! streams with a plausible header (valid properties byte, small dictionary, small declared size, the
//! range coder's leading zero) followed by random bytes — the lane that reaches the decoder past its
//! first sanity check, (c) VALID compressed payloads of real 20-byte records that are then mutated
//! (a flipped byte, a truncation, appended garbage), and (d) structured JSON whose object keys are
//! the protocol's REAL field names with hostile leaves (`NaN`, `inf`, `1e999`, `i64::MIN`, empty
//! strings, wrong types).
//!
//! The property is TOTALITY: a hostile or truncated input may decode to nothing, to `None`, or to a
//! `Bi5Refusal`, but it must never panic the reader thread (sidecar lines) or the import worker
//! (archive bytes). Outputs are asserted only where a cheap invariant must always hold (a decoded
//! daily file is ascending and inside its day).
//!
//! Covered (all through the PUBLIC API): `proto::{parse_envelope, parse_command}`,
//! `recon_client::to_position_report`, `data::{decompress, decode_ticks, ticks_to_bars,
//! tick_to_quote, hour_url, point_divisor}`, `archive::{classify_bi5_path, is_bi5_layout_dir,
//! read_bi5_header, decode_daily_file, Bi5Day}`, `price_scale::{bi5_price_scale,
//! unscaled_instrument_refusal}`. The reader thread's netting fold (`ShadowBook`, `pub(crate)`) is
//! covered by the sibling unit test `crates/bridges/dukascopy/src/netting_props.rs`. NOT covered:
//! the JForex bridge itself (Java), and the `fetch_*` functions (network I/O).
//!
//! A minimized counterexample is a REAL bug: commit the `decoder_never_panics.proptest-regressions`
//! seed beside this file and fix the decoder.

use proptest::prelude::*;
use serde_json::{Map, Value};
use vike_dukascopy::{
    Bi5Day, Bi5Limits, Bi5PriceScale, VenuePosition, bi5_price_scale, classify_bi5_path,
    decode_daily_file, decode_ticks, decompress, hour_url, is_bi5_layout_dir, parse_command,
    parse_envelope, point_divisor, read_bi5_header, tick_to_quote, ticks_to_bars,
    unscaled_instrument_refusal,
};

const DAY_MS: i64 = 86_400_000;

/// Strings that matter to a numeric / id / enum read on the wire (including non-ASCII, for the
/// non-char-boundary class of slicing bugs).
const STR_POOL: &[&str] = &[
    "",
    " ",
    "0",
    "1",
    "-1",
    "-0",
    "0.0",
    "1.5",
    "42",
    "NaN",
    "nan",
    "inf",
    "-inf",
    "Infinity",
    "1e999",
    "-1e999",
    "1e-999",
    "9223372036854775807",
    "9223372036854775808",
    "-9223372036854775809",
    "18446744073709551616",
    "99999999999999999999999999999999",
    "1700000000000",
    "true",
    "null",
    "\u{0}",
    "é",
    "日本",
];

/// Sidecar-protocol vocabulary: kinds, commands, event tags, sides, symbols.
const WORDS: &[&str] = &[
    "ready",
    "fatal",
    "event",
    "position",
    "submit",
    "cancel",
    "shutdown",
    "market",
    "limit",
    "stop",
    "EURUSD",
    "USDJPY",
    "DEMO2cGyrc",
    "dukascopy",
    "Taker",
    "Maker",
    "Unknown",
    "BOTH",
    "Gtc",
    "Ioc",
    "Fok",
    "Gtd",
    "Day",
    "Cross",
    "Isolated",
    "Mark",
    "default",
];

/// Every `Event` tag the sidecar's `event` envelope may carry.
const EVENT_TYPES: &[&str] = &[
    "FillEvent",
    "OrderSubmitted",
    "OrderAccepted",
    "OrderRejected",
    "OrderDenied",
    "OrderTriggered",
    "OrderPartiallyFilled",
    "OrderFilled",
    "OrderCanceled",
    "OrderExpired",
    "OrderLiquidated",
    "OrderModified",
    "PositionOpened",
    "PositionChanged",
    "PositionClosed",
    "AccountState",
    "FundingEvent",
    "PositionLiquidated",
    "OrderCancelRejected",
    "OrderModifyRejected",
];

const SYMBOLS: &[&str] = &["EURUSD", "USDJPY", "GBPUSD", "XAUUSD", ""];
/// Every real field name the protocol decoders read, plus the envelope / command tags.
const PROTO_KEYS: &[&str] = &[
    "kind",
    "cmd",
    "account",
    "balance",
    "event",
    "order",
    "symbol",
    "size",
    "avg_px",
    "ts",
    "reason",
    "client_order_id",
    "venue_order_id",
    "type",
    "trade_id",
    "venue",
    "side",
    "qty",
    "last_qty",
    "last_px",
    "commission",
    "commission_asset",
    "liquidity_side",
    "mark_price",
    "position_side",
    "fill",
    "order_type",
    "price",
    "trigger_price",
    "reduce_only",
    "time_in_force",
    "gtd_expiry",
    "parent_order_id",
    "linked_order_ids",
    "order_list_id",
    "contingency_type",
    "weight",
    "stop",
    "trail",
    "extreme",
    "on_close",
    "combo_legs",
    "margin_mode",
    "trigger_by",
    "balances",
    "route_key",
];

// --- the generator kit ---------------------------------------------------------------------

fn arb_str() -> BoxedStrategy<String> {
    prop_oneof![
        4 => prop::sample::select(STR_POOL).prop_map(String::from),
        4 => prop::sample::select(WORDS).prop_map(String::from),
        1 => any::<i64>().prop_map(|n| n.to_string()),
        1 => any::<f64>().prop_map(|f| f.to_string()),
        2 => "[ -~]{0,12}",
        1 => any::<String>(),
    ]
    .boxed()
}

/// A JSON leaf: null, bool, i64 extremes / zero / negative, u64, f64, hostile strings, and the
/// empty array / object.
fn arb_leaf() -> BoxedStrategy<Value> {
    prop_oneof![
        1 => Just(Value::Null),
        1 => any::<bool>().prop_map(Value::Bool),
        2 => prop::sample::select(vec![0i64, 1, -1, i64::MAX, i64::MIN]).prop_map(|n| Value::Number(n.into())),
        1 => any::<i64>().prop_map(|n| Value::Number(n.into())),
        1 => any::<u64>().prop_map(|n| Value::Number(n.into())),
        1 => any::<f64>().prop_map(|f| serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number)),
        5 => arb_str().prop_map(Value::String),
        1 => Just(Value::Array(Vec::new())),
        1 => Just(Value::Object(Map::new())),
    ]
    .boxed()
}

fn arb_key(keys: &'static [&'static str]) -> BoxedStrategy<String> {
    prop_oneof![
        8 => prop::sample::select(keys).prop_map(String::from),
        1 => "[a-zA-Z_]{1,8}",
    ]
    .boxed()
}

/// Random JSON, depth <= 4, object keys drawn mostly from `keys`.
fn arb_json(keys: &'static [&'static str]) -> BoxedStrategy<Value> {
    arb_leaf()
        .prop_recursive(4, 64, 6, move |inner| {
            prop_oneof![
                prop::collection::vec(inner.clone(), 0..6).prop_map(Value::Array),
                prop::collection::vec((arb_key(keys), inner), 0..6)
                    .prop_map(|kvs| Value::Object(kvs.into_iter().collect())),
            ]
        })
        .boxed()
}

/// Mostly the well-typed `good` value, sometimes any leaf / random JSON (the wrong-type lane).
fn junk_or(good: BoxedStrategy<Value>) -> BoxedStrategy<Value> {
    prop_oneof![9 => good, 1 => arb_json(PROTO_KEYS)].boxed()
}

/// One of `set` as a JSON string (80%), else any leaf.
fn pick(set: &'static [&'static str]) -> BoxedStrategy<Value> {
    prop_oneof![
        8 => prop::sample::select(set).prop_map(|s| Value::String(s.to_string())),
        2 => arb_leaf(),
    ]
    .boxed()
}

/// Decimal text a numeric read would parse (mostly), else hostile text.
fn num_text() -> BoxedStrategy<String> {
    prop_oneof![
        4 => (-1_000_000i64..1_000_000).prop_map(|n| n.to_string()),
        4 => (-1.0e6f64..1.0e6).prop_map(|f| format!("{f:.6}")),
        1 => any::<f64>().prop_map(|f| f.to_string()),
        1 => arb_str(),
    ]
    .boxed()
}

fn json_f64(f: f64) -> Value {
    serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number)
}

/// A numeric wire field: the sidecar writes JSON NUMBERS (Gson), sometimes a string / any leaf.
fn numv() -> BoxedStrategy<Value> {
    prop_oneof![
        5 => (-1.0e6f64..1.0e6).prop_map(json_f64),
        2 => any::<i64>().prop_map(|n| Value::Number(n.into())),
        1 => any::<f64>().prop_map(json_f64),
        2 => num_text().prop_map(Value::String),
        1 => arb_leaf(),
    ]
    .boxed()
}

/// An integer wire field (epoch-ms stamps): usually a number in the epoch-ms range.
fn jint() -> BoxedStrategy<Value> {
    prop_oneof![
        6 => (0i64..4_102_444_800_000).prop_map(|n| Value::Number(n.into())),
        1 => any::<i64>().prop_map(|n| Value::Number(n.into())),
        1 => num_text().prop_map(Value::String),
        1 => arb_leaf(),
    ]
    .boxed()
}

/// A short id-shaped field: usually non-empty.
fn idv() -> BoxedStrategy<Value> {
    prop_oneof![5 => "[a-z0-9_]{1,8}".prop_map(Value::String), 1 => arb_leaf()].boxed()
}

/// An object over exactly the named fields; each is present with p = 0.85 and drawn from its own
/// (mostly well-typed) strategy.
fn obj_of(fields: Vec<(&'static str, BoxedStrategy<Value>)>) -> BoxedStrategy<Value> {
    fields
        .into_iter()
        .map(|(k, s)| prop::option::weighted(0.85, s).prop_map(move |v| (k, v)))
        .collect::<Vec<_>>()
        .prop_map(|kvs| {
            let mut m = Map::new();
            for (k, v) in kvs {
                if let Some(v) = v {
                    m.insert(k.to_string(), v);
                }
            }
            Value::Object(m)
        })
        .boxed()
}

fn array_of(row: BoxedStrategy<Value>, max: usize) -> BoxedStrategy<Value> {
    prop::collection::vec(row, 0..max).prop_map(Value::Array).boxed()
}

// --- protocol lines ------------------------------------------------------------------------

/// A `FillEvent`-shaped object (also the `fill` of the order wraps).
fn fill_obj() -> BoxedStrategy<Value> {
    obj_of(vec![
        ("trade_id", idv()),
        ("client_order_id", idv()),
        ("venue", pick(&["dukascopy"])),
        ("symbol", pick(SYMBOLS)),
        ("side", jint()),
        ("last_qty", numv()),
        ("last_px", numv()),
        ("commission", numv()),
        ("commission_asset", pick(&["USD", ""])),
        ("liquidity_side", pick(&["Taker", "Maker", "Unknown", ""])),
        ("ts", jint()),
        ("mark_price", numv()),
        ("position_side", pick(&["BOTH", "LONG", "SHORT", ""])),
    ])
}

/// An `Event`-tagged object: a tag from the real union, then fields from every variant's payload.
fn event_obj() -> BoxedStrategy<Value> {
    obj_of(vec![
        ("type", pick(EVENT_TYPES)),
        ("client_order_id", idv()),
        ("venue_order_id", idv()),
        ("ts", jint()),
        ("reason", arb_leaf()),
        ("fill", junk_or(fill_obj())),
        ("trade_id", idv()),
        ("venue", pick(&["dukascopy"])),
        ("symbol", pick(SYMBOLS)),
        ("side", jint()),
        ("last_qty", numv()),
        ("last_px", numv()),
        ("commission", numv()),
        ("commission_asset", pick(&["USD", ""])),
        ("liquidity_side", pick(&["Taker", "Maker", "Unknown", ""])),
        ("mark_price", numv()),
        ("position_side", pick(&["BOTH", ""])),
        ("balances", arb_json(PROTO_KEYS)),
    ])
}

/// An `OrderRequest`-shaped object (the `submit` command's `order`).
fn order_obj() -> BoxedStrategy<Value> {
    obj_of(vec![
        ("client_order_id", idv()),
        ("venue", pick(&["dukascopy"])),
        ("symbol", pick(SYMBOLS)),
        ("side", jint()),
        ("qty", numv()),
        ("order_type", pick(&["market", "limit", "stop", ""])),
        ("price", numv()),
        ("trigger_price", numv()),
        ("reduce_only", arb_leaf()),
        ("time_in_force", pick(&["Gtc", "Ioc", "Fok", "Gtd", "Day", ""])),
        ("gtd_expiry", jint()),
        ("ts", jint()),
        ("parent_order_id", arb_leaf()),
        ("linked_order_ids", junk_or(array_of(idv(), 3))),
        ("order_list_id", arb_leaf()),
        ("contingency_type", arb_leaf()),
        ("weight", numv()),
        ("stop", numv()),
        ("trail", numv()),
        ("extreme", numv()),
        ("on_close", arb_leaf()),
        (
            "combo_legs",
            junk_or(array_of(obj_of(vec![("symbol", pick(SYMBOLS)), ("ratio", jint())]), 3)),
        ),
        ("margin_mode", pick(&["Cross", "Isolated", ""])),
        ("trigger_by", pick(&["Mark", "Last", ""])),
        ("account", pick(&["default", "DEFAULT", ""])),
    ])
}

/// A sidecar stdout line as JSON: `ready` / `event` / `position` / `fatal`, each mostly well-typed.
fn envelope_value() -> BoxedStrategy<Value> {
    let kind = |k: &'static str| -> BoxedStrategy<Value> {
        prop_oneof![9 => Just(Value::String(k.to_string())), 1 => arb_leaf()].boxed()
    };
    prop_oneof![
        obj_of(vec![
            ("kind", kind("ready")),
            ("account", pick(&["DEMO2cGyrc", ""])),
            ("balance", numv())
        ]),
        obj_of(vec![("kind", kind("event")), ("event", junk_or(event_obj()))]),
        obj_of(vec![
            ("kind", kind("position")),
            ("symbol", pick(SYMBOLS)),
            ("size", numv()),
            ("avg_px", numv()),
            ("ts", jint()),
        ]),
        obj_of(vec![("kind", kind("fatal")), ("reason", arb_leaf())]),
        arb_json(PROTO_KEYS),
    ]
    .boxed()
}

/// A Rust -> sidecar command line as JSON: `submit` / `cancel` / `shutdown`.
fn command_value() -> BoxedStrategy<Value> {
    let cmd = |c: &'static str| -> BoxedStrategy<Value> {
        prop_oneof![9 => Just(Value::String(c.to_string())), 1 => arb_leaf()].boxed()
    };
    prop_oneof![
        obj_of(vec![("cmd", cmd("submit")), ("order", junk_or(order_obj()))]),
        obj_of(vec![("cmd", cmd("cancel")), ("client_order_id", idv())]),
        obj_of(vec![("cmd", cmd("shutdown"))]),
        arb_json(PROTO_KEYS),
    ]
    .boxed()
}

/// Both protocol decoders on one line; a decoded `position` line maps to a report.
fn drive_line(line: &str) {
    if let Some(vike_dukascopy::Envelope::Position { symbol, size, avg_px, ts }) =
        parse_envelope(line)
    {
        let _ = vike_dukascopy::recon_client::to_position_report(
            &symbol,
            &VenuePosition { size, avg_px, ts },
        );
    }
    let _ = parse_command(line);
}

// --- archive bytes -------------------------------------------------------------------------

/// An LZMA-alone stream with a plausible header — valid properties byte, a small dictionary, a small
/// (record-sized) or hostile declared size, the range coder's leading zero — then random bytes: the
/// lane that gets past the decoder's first sanity checks into its probability model.
fn lzma_noise() -> BoxedStrategy<Vec<u8>> {
    (
        prop_oneof![8 => 0u8..225, 1 => any::<u8>()],
        prop_oneof![Just(0u32), Just(4096u32), Just(1u32 << 16), Just(1u32 << 22), any::<u32>()],
        prop_oneof![
            4 => (0u64..2_048).prop_map(|n| n * 20),
            2 => 0u64..5_000,
            1 => Just(u64::MAX),
            1 => any::<u64>(),
        ],
        any::<bool>(),
        prop::collection::vec(any::<u8>(), 0..300),
    )
        .prop_map(|(props, dict, size, zero, body)| {
            let mut v = vec![props];
            v.extend_from_slice(&dict.to_le_bytes());
            v.extend_from_slice(&size.to_le_bytes());
            if zero {
                v.push(0);
            }
            v.extend_from_slice(&body);
            v
        })
        .boxed()
}

/// `raw` LZMA-compressed by `lzma-rs` itself, size UNKNOWN in the header (an end-marker stream).
fn lzma_unknown_size(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    lzma_rs::lzma_compress(&mut &raw[..], &mut out).expect("compress");
    out
}

/// `raw` LZMA-compressed by `lzma-rs`, its size DECLARED in the header.
fn lzma_declared_size(raw: &[u8]) -> Vec<u8> {
    let options = lzma_rs::compress::Options {
        unpacked_size: lzma_rs::compress::UnpackedSize::WriteToHeader(Some(raw.len() as u64)),
    };
    let mut out = Vec::new();
    lzma_rs::lzma_compress_with_options(&mut &raw[..], &mut out, &options).expect("compress");
    out
}

/// One 20-byte `>IIIff` record.
fn record(offset: u32, ask: u32, bid: u32, ask_vol: f32, bid_vol: f32) -> Vec<u8> {
    let mut r = Vec::with_capacity(20);
    r.extend_from_slice(&offset.to_be_bytes());
    r.extend_from_slice(&ask.to_be_bytes());
    r.extend_from_slice(&bid.to_be_bytes());
    r.extend_from_slice(&ask_vol.to_be_bytes());
    r.extend_from_slice(&bid_vol.to_be_bytes());
    r
}

/// A decoded payload of whole records: usually ascending offsets across the day (so a daily decode
/// can succeed), sometimes arbitrary `u32`s (out of range / non-monotonic), prices and volumes
/// arbitrary (`f32` NaN / inf included).
fn payload() -> BoxedStrategy<Vec<u8>> {
    let sorted = prop::collection::vec(0u32..86_400_000, 0..40).prop_map(|mut v| {
        v.sort_unstable();
        v
    });
    let offsets = prop_oneof![8 => sorted, 2 => prop::collection::vec(any::<u32>(), 0..40)];
    (offsets, any::<u32>(), any::<u32>(), any::<f32>(), any::<f32>())
        .prop_map(|(offsets, ask, bid, av, bv)| {
            offsets
                .iter()
                .enumerate()
                .flat_map(|(i, &o)| record(o, ask.wrapping_add(i as u32), bid, av, bv))
                .collect()
        })
        .boxed()
}

/// How a valid compressed blob is damaged.
#[derive(Clone, Debug)]
enum Damage {
    None,
    Flip { at: prop::sample::Index, mask: u8 },
    Truncate(prop::sample::Index),
    Append(Vec<u8>),
}

fn damage() -> BoxedStrategy<Damage> {
    prop_oneof![
        2 => Just(Damage::None),
        3 => (any::<prop::sample::Index>(), 1u8..=255).prop_map(|(at, mask)| Damage::Flip { at, mask }),
        2 => any::<prop::sample::Index>().prop_map(Damage::Truncate),
        1 => prop::collection::vec(any::<u8>(), 1..16).prop_map(Damage::Append),
    ]
    .boxed()
}

fn apply(damage: &Damage, mut blob: Vec<u8>) -> Vec<u8> {
    match damage {
        Damage::None => {}
        Damage::Flip { at, mask } => {
            if !blob.is_empty() {
                let i = at.index(blob.len());
                blob[i] ^= mask;
            }
        }
        Damage::Truncate(at) => {
            let n = at.index(blob.len() + 1);
            blob.truncate(n);
        }
        Damage::Append(extra) => blob.extend_from_slice(extra),
    }
    blob
}

/// A compressed blob: valid streams of a valid payload, damaged; plus plausible-header noise and
/// plain random bytes.
fn blob() -> BoxedStrategy<Vec<u8>> {
    prop_oneof![
        4 => (payload(), any::<bool>(), damage()).prop_map(|(raw, declared, damage)| {
            let packed = if declared { lzma_declared_size(&raw) } else { lzma_unknown_size(&raw) };
            apply(&damage, packed)
        }),
        3 => lzma_noise(),
        1 => prop::collection::vec(any::<u8>(), 0..300),
    ]
    .boxed()
}

fn limits() -> BoxedStrategy<Bi5Limits> {
    prop_oneof![
        1 => Just(Bi5Limits::DEFAULT),
        4 => (
            prop::sample::select(vec![0usize, 1, 13, 64, 1 << 12, 1 << 16]),
            prop::sample::select(vec![0usize, 19, 20, 100, 1 << 12, 1 << 20]),
        )
            .prop_map(|(max_compressed_bytes, max_decoded_bytes)| Bi5Limits {
                max_compressed_bytes,
                max_decoded_bytes,
            }),
    ]
    .boxed()
}

fn scale() -> &'static Bi5PriceScale {
    bi5_price_scale("EURUSD").expect("EURUSD is an admitted instrument")
}

/// A real archive day: a UTC midnight from 1970 to ~2050.
fn day() -> BoxedStrategy<Bi5Day> {
    (0i64..29_000)
        .prop_map(|n| {
            Bi5Day::from_start_ms(n * DAY_MS).expect("a UTC midnight on or after the epoch")
        })
        .boxed()
}

// --- archive paths -------------------------------------------------------------------------

/// Path segments seeded with the shapes the grammar tests: real and impossible dates, the wrong
/// width, case, a `Z` suffix, non-ASCII digits, empty.
const PATH_SEGS: &[&str] = &[
    "2024",
    "2026",
    "1970",
    "1969",
    "9999",
    "0000",
    "00",
    "01",
    "11",
    "12",
    "15",
    "29",
    "30",
    "31",
    "32",
    "15_ticks.bi5",
    "31_ticks.bi5",
    "00_ticks.bi5",
    "5_ticks.bi5",
    "15_TICKS.BI5",
    "00h_ticks.bi5",
    "23h_ticks.bi5",
    "24h_ticks.bi5",
    "EURUSD",
    "é",
    "日本",
    "",
    "٣٣",
    "20 24",
];

fn path_seg() -> BoxedStrategy<String> {
    prop_oneof![
        8 => prop::sample::select(PATH_SEGS).prop_map(String::from),
        1 => "[0-9]{0,6}",
        1 => "[ -~]{0,12}",
        1 => any::<String>(),
    ]
    .boxed()
}

fn now_ms() -> BoxedStrategy<i64> {
    prop_oneof![
        4 => 1_700_000_000_000i64..1_900_000_000_000,
        1 => any::<i64>(),
        1 => Just(0i64),
        1 => Just(i64::MAX),
        1 => Just(i64::MIN),
    ]
    .boxed()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    // --- the sidecar protocol --------------------------------------------------------------

    /// (a) arbitrary text never panics the line decoders.
    #[test]
    fn protocol_lines_are_total_over_arbitrary_text(text in any::<String>()) {
        drive_line(&text);
    }

    /// (a') ...nor lossy-decoded byte noise (controls, invalid UTF-8 replaced).
    #[test]
    fn protocol_lines_survive_byte_noise(bytes in prop::collection::vec(any::<u8>(), 0..256)) {
        drive_line(&String::from_utf8_lossy(&bytes));
    }

    /// (b) random JSON over the protocol's real field names.
    #[test]
    fn protocol_lines_are_total_over_arbitrary_json(v in arb_json(PROTO_KEYS)) {
        drive_line(&v.to_string());
    }

    /// (b') well-shaped sidecar lines and commands with hostile leaves, whole and cut off
    /// mid-line.
    #[test]
    fn protocol_lines_are_total_over_structured_lines(
        env in envelope_value(),
        cmd in command_value(),
        cut in 0usize..400,
    ) {
        for v in [&env, &cmd] {
            let line = v.to_string();
            drive_line(&line);
            drive_line(&line.chars().take(cut).collect::<String>());
        }
    }

    // --- the hourly `.bi5` decoders (data.rs) ---------------------------------------------

    /// (a) arbitrary bytes never panic LZMA decompression, and whatever it yields decodes to ticks
    /// that aggregate to ascending bars.
    #[test]
    fn hourly_decoders_are_total_over_arbitrary_bytes(
        bytes in prop::collection::vec(any::<u8>(), 0..400),
        hour_start in 0i64..4_102_444_800_000,
        divisor in prop_oneof![Just(1e5), Just(1e3), Just(0.0), any::<f64>()],
        step in 1i64..10_000_000_000,
    ) {
        if let Ok(raw) = decompress(&bytes) {
            let ticks = decode_ticks(&raw, hour_start, divisor);
            prop_assert_eq!(ticks.len(), raw.len() / 20);
            let bars = ticks_to_bars(&ticks, step);
            prop_assert!(bars.len() <= ticks.len());
            prop_assert!(bars.windows(2).all(|w| w[0].ts < w[1].ts), "bars ascend by bucket");
            for t in &ticks {
                let _ = tick_to_quote(t, "EURUSD");
            }
        }
        // the raw bytes are also a valid DECODED payload of some ticks
        let ticks = decode_ticks(&bytes, hour_start, divisor);
        let _ = ticks_to_bars(&ticks, step);
    }

    /// (b) plausible-header LZMA noise and damaged valid streams through the unbounded hourly
    /// decompressor.
    #[test]
    fn hourly_decompress_is_total_over_structured_streams(packed in blob()) {
        let _ = decompress(&packed);
    }

    /// The URL and divisor builders over arbitrary symbols and instants.
    #[test]
    fn hourly_url_and_divisor_are_total(symbol in any::<String>(), hour_start in any::<i64>()) {
        let url = hour_url(&symbol, hour_start);
        prop_assert!(url.ends_with("h_ticks.bi5"));
        let d = point_divisor(&symbol);
        prop_assert!(d == 1e3 || d == 1e5);
    }

    // --- the daily archive decoder (archive.rs) -------------------------------------------

    /// (a) arbitrary path segments never panic the layout grammar.
    #[test]
    fn archive_paths_are_total(
        rel in prop::collection::vec(path_seg(), 0..6),
        now in now_ms(),
    ) {
        let segs: Vec<&str> = rel.iter().map(String::as_str).collect();
        let _ = classify_bi5_path(&segs, now);
        let _ = is_bi5_layout_dir(&segs, now);
    }

    /// (b) grammar-shaped paths built from real calendar numbers reach the date / hour checks.
    #[test]
    fn archive_paths_are_total_over_calendar_shapes(
        year in 1960u32..2100,
        month0 in 0u32..14,
        dom in 0u32..34,
        hour in 0u32..26,
        now in now_ms(),
    ) {
        let (y, m, d, h) = (
            format!("{year:04}"),
            format!("{month0:02}"),
            format!("{dom:02}"),
            format!("{hour:02}h_ticks.bi5"),
        );
        let daily = format!("{d}_ticks.bi5");
        let _ = classify_bi5_path(&[&y, &m, &daily], now);
        let _ = classify_bi5_path(&[&y, &m, &d, &h], now);
        let _ = is_bi5_layout_dir(&[&y], now);
        let _ = is_bi5_layout_dir(&[&y, &m], now);
        let _ = is_bi5_layout_dir(&[&y, &m, &d], now);
    }

    /// The day key over arbitrary instants: a day is only ever a UTC midnight, and prints.
    #[test]
    fn archive_day_keys_are_total(start_ms in any::<i64>()) {
        if let Some(day) = Bi5Day::from_start_ms(start_ms) {
            prop_assert_eq!(day.start_ms() % DAY_MS, 0);
            prop_assert!(!day.to_string().is_empty());
        }
    }

    /// (a) arbitrary bytes through the header reader and the bounded daily decoder.
    #[test]
    fn archive_decode_is_total_over_arbitrary_bytes(
        bytes in prop::collection::vec(any::<u8>(), 0..300),
        caps in limits(),
        archive_day in day(),
    ) {
        let _ = read_bi5_header(&bytes, &caps);
        let _ = decode_daily_file(&bytes, archive_day, scale(), &caps);
    }

    /// (b) plausible-header noise, and (c) VALID compressed payloads damaged in flight, through the
    /// bounded daily decoder: a refusal or an `Ok` day — and an `Ok` day is ascending and inside the
    /// day its path named.
    #[test]
    fn archive_decode_is_total_over_structured_streams(
        packed in blob(),
        caps in limits(),
        archive_day in day(),
    ) {
        let _ = read_bi5_header(&packed, &caps);
        if let Ok(ticks) = decode_daily_file(&packed, archive_day, scale(), &caps) {
            let start = archive_day.start_ms();
            prop_assert!(ticks.windows(2).all(|w| w[0].ts <= w[1].ts), "ticks never decrease");
            prop_assert!(
                ticks.iter().all(|t| t.ts >= start && t.ts < start + DAY_MS),
                "every tick sits inside its day"
            );
        }
    }

    /// The scale table lookup and refusal text over arbitrary instrument names.
    #[test]
    fn price_scale_lookup_is_total(instrument in any::<String>()) {
        let _ = bi5_price_scale(&instrument);
        prop_assert!(unscaled_instrument_refusal(&instrument).contains(&instrument));
    }
}
