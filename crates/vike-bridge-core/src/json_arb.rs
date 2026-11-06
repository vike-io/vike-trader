//! The JSON generator kit of the bridges' decoder TOTALITY harnesses (`tests/decoder_never_panics*`),
//! written once here behind `test-support` (with `proptest`, which only that feature turns on).
//!
//! Each bridge keeps its OWN vocabulary — the strings, words, wire keys and dispatch tokens its
//! decoders meet — and hands it in; this module owns only the venue-neutral strategy SHAPES the
//! harnesses had copied. Two kit shapes exist, because the harnesses grew two:
//!
//! - [`PoolKit`] (deribit, dukascopy, hyperliquid, okx): leaves drawn from a hostile string pool plus
//!   the venue's words; objects as [`obj_of`] (each named field present with p = 0.85).
//! - [`TokenKit`] (aster, binance, bybit): leaves drawn from the venue's dispatch tokens and hostile
//!   numerics; objects as [`object`] over [`Field`]s.
//!
//! ⚠ **The strategy TREE is the contract.** A committed `*.proptest-regressions` seed replays the
//! RNG, not the value: every weight, range and arm order below is the one the venue copies had, so a
//! seed recorded before the move reproduces the same counterexample after it. Change a shape only as
//! a deliberate harness change, never in passing.

use proptest::prelude::*;
use serde_json::{Map, Value, json};

/// A finite `f64` as a JSON number; a non-finite one as `null` (JSON has no NaN/inf).
pub fn json_f64(f: f64) -> Value {
    serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number)
}

/// Mostly one of `keys` (`weight` times in `weight + 1`), sometimes a random identifier. Each kit
/// keeps the weight its copies had: [`PoolKit`] and [`keyed_json`] 8, [`TokenKit`] 5, the
/// polymarket and vike-ibkr kits 9.
pub fn weighted_key(weight: u32, keys: &'static [&'static str]) -> BoxedStrategy<String> {
    prop_oneof![
        weight => prop::sample::select(keys).prop_map(String::from),
        1 => "[a-zA-Z_]{1,8}",
    ]
    .boxed()
}

/// Random JSON over `leaf`, depth <= `depth`: arrays and objects of up to five children, object
/// keys drawn from `key`.
pub fn json_tree(
    leaf: BoxedStrategy<Value>,
    key: impl Fn() -> BoxedStrategy<String> + 'static,
    depth: u32,
) -> BoxedStrategy<Value> {
    leaf.prop_recursive(depth, 64, 6, move |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..6).prop_map(Value::Array),
            prop::collection::vec((key(), inner), 0..6)
                .prop_map(|kvs| Value::Object(kvs.into_iter().collect())),
        ]
    })
    .boxed()
}

/// The printable leaf of the binance and bybit `mapper_props` harnesses: null, bool, any i64, any
/// f64, short printable text.
pub fn printable_leaf() -> BoxedStrategy<Value> {
    prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(|n| Value::Number(n.into())),
        any::<f64>().prop_map(json_f64),
        "[ -~]{0,16}".prop_map(Value::String),
    ]
    .boxed()
}

/// The leaf of the polymarket and vike-ibkr kits over the venue's own hostile numeric strings
/// (`nums`) and dispatch words (`words`): null, bool, [`arb_i64`], any f64, those, printable and
/// unicode text, and the empty containers.
pub fn worded_leaf(
    nums: BoxedStrategy<String>,
    words: BoxedStrategy<String>,
) -> BoxedStrategy<Value> {
    prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        arb_i64().prop_map(|n| Value::Number(n.into())),
        any::<f64>().prop_map(json_f64),
        nums.prop_map(Value::String),
        "[ -~]{0,12}".prop_map(Value::String),
        any::<String>().prop_map(Value::String),
        words.prop_map(Value::String),
        prop::sample::select(vec![json!([]), json!({})]),
    ]
    .boxed()
}

/// Raw bytes -> text the way a lossy socket or buffer read would produce it.
pub fn arb_noise() -> impl Strategy<Value = String> {
    prop::collection::vec(any::<u8>(), 0..512)
        .prop_map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

/// `v` with `key` set to `child` (a non-object `v` comes back unchanged).
pub fn set(mut v: Value, key: &str, child: Value) -> Value {
    if let Value::Object(m) = &mut v {
        m.insert(key.to_string(), child);
    }
    v
}

/// A u64 sequence number as JSON: below `small` (so the sync rules hit equal / adjacent values),
/// the extremes, or anything.
pub fn seq(small: u64) -> BoxedStrategy<Value> {
    prop_oneof![0u64..small, Just(u64::MAX), Just(u64::MAX - 1), any::<u64>()]
        .prop_map(|n| json!(n))
        .boxed()
}

/// An object over exactly the named fields; each is present with p = 0.85 and drawn from its own
/// (mostly well-typed) strategy.
pub fn obj_of(fields: Vec<(&'static str, BoxedStrategy<Value>)>) -> BoxedStrategy<Value> {
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

/// Up to `max - 1` rows of `row`.
pub fn array_of(row: BoxedStrategy<Value>, max: usize) -> BoxedStrategy<Value> {
    prop::collection::vec(row, 0..max).prop_map(Value::Array).boxed()
}

/// The first `n` chars of `text` (a frame cut off mid-flight, never mid-char).
pub fn truncated(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

/// The first half of `s`, by chars (a body cut off mid-flight).
pub fn truncate_half(s: String) -> String {
    let keep = s.chars().count() / 2;
    s.chars().take(keep).collect()
}

/// An i64: zero, one, minus one, the extremes, an epoch-ms and an epoch-s stamp, or any.
pub fn arb_i64() -> impl Strategy<Value = i64> {
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

/// An f64: both zeros, NaN, both infinities, the smallest positive, the extremes, or any.
pub fn arb_f64() -> impl Strategy<Value = f64> {
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

/// Text a decoder may be handed: any unicode, lossy-decoded byte noise, a `json` value's text, or
/// that text cut in half.
pub fn json_text(json: BoxedStrategy<Value>) -> BoxedStrategy<String> {
    prop_oneof![
        any::<String>(),
        prop::collection::vec(any::<u8>(), 0..256)
            .prop_map(|b| String::from_utf8_lossy(&b).into_owned()),
        json.clone().prop_map(|v| v.to_string()),
        json.prop_map(|v| truncate_half(v.to_string())),
    ]
    .boxed()
}

/// Random JSON over the venue's own `leaf`, depth <= 4, object keys drawn mostly from `keys` (the
/// shape the alpaca, fxcm, ig and oanda harnesses share; their leaves differ, so each passes its own).
pub fn keyed_json(
    leaf: impl Strategy<Value = Value> + 'static,
    keys: &'static [&'static str],
) -> impl Strategy<Value = Value> {
    leaf.prop_recursive(4, 64, 6, move |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..6).prop_map(Value::Array),
            prop::collection::vec((weighted_key(8, keys), inner), 0..8)
                .prop_map(|kvs| Value::Object(kvs.into_iter().collect())),
        ]
    })
}

/// An object over exactly `keys`, each present with p = 0.85 and holding one of `leaf`.
pub fn shaped(
    leaf: impl Strategy<Value = Value>,
    keys: &'static [&'static str],
) -> impl Strategy<Value = Value> {
    prop::collection::vec(prop::option::weighted(0.85, leaf), keys.len()..=keys.len()).prop_map(
        move |vals| {
            Value::Object(
                keys.iter()
                    .zip(vals)
                    .filter_map(|(k, v)| v.map(|v| ((*k).to_string(), v)))
                    .collect(),
            )
        },
    )
}

/// A [`PoolKit`] venue's vocabulary: `strs`, the strings that matter to a numeric / id / enum read
/// on the wire (hostile numerics, non-ASCII), and `words`, the venue's own channels, states, sides.
#[derive(Clone, Copy)]
pub struct PoolKit {
    pub strs: &'static [&'static str],
    pub words: &'static [&'static str],
}

impl PoolKit {
    /// A string: pool, word, any integer / float text, printable noise, any unicode.
    pub fn arb_str(self) -> BoxedStrategy<String> {
        prop_oneof![
            4 => prop::sample::select(self.strs).prop_map(String::from),
            4 => prop::sample::select(self.words).prop_map(String::from),
            1 => any::<i64>().prop_map(|n| n.to_string()),
            1 => any::<f64>().prop_map(|f| f.to_string()),
            2 => "[ -~]{0,12}",
            1 => any::<String>(),
        ]
        .boxed()
    }

    /// A JSON leaf: null, bool, i64 extremes / zero / negative, u64, f64, hostile strings, and the
    /// empty array / object.
    pub fn arb_leaf(self) -> BoxedStrategy<Value> {
        prop_oneof![
            1 => Just(Value::Null),
            1 => any::<bool>().prop_map(Value::Bool),
            2 => prop::sample::select(vec![0i64, 1, -1, i64::MAX, i64::MIN]).prop_map(|n| Value::Number(n.into())),
            1 => any::<i64>().prop_map(|n| Value::Number(n.into())),
            1 => any::<u64>().prop_map(|n| Value::Number(n.into())),
            1 => any::<f64>().prop_map(json_f64),
            5 => self.arb_str().prop_map(Value::String),
            1 => Just(Value::Array(Vec::new())),
            1 => Just(Value::Object(Map::new())),
        ]
        .boxed()
    }

    /// Random JSON, depth <= 4, object keys drawn mostly from `keys`.
    pub fn arb_json(self, keys: &'static [&'static str]) -> BoxedStrategy<Value> {
        json_tree(self.arb_leaf(), move || weighted_key(8, keys), 4)
    }

    /// One of `set` as a JSON string (80%), else any leaf.
    pub fn pick(self, set: &'static [&'static str]) -> BoxedStrategy<Value> {
        prop_oneof![
            8 => prop::sample::select(set).prop_map(|s| Value::String(s.to_string())),
            2 => self.arb_leaf(),
        ]
        .boxed()
    }

    /// Decimal text a numeric read would parse (mostly), else hostile text.
    pub fn num_text(self) -> BoxedStrategy<String> {
        prop_oneof![
            4 => (-1_000_000i64..1_000_000).prop_map(|n| n.to_string()),
            4 => (-1.0e6f64..1.0e6).prop_map(|f| format!("{f:.6}")),
            1 => any::<f64>().prop_map(|f| f.to_string()),
            1 => self.arb_str(),
        ]
        .boxed()
    }

    /// A numeric wire field spoken as a decimal STRING (okx, hyperliquid): usually that string,
    /// sometimes a JSON number or any leaf.
    pub fn numv(self) -> BoxedStrategy<Value> {
        prop_oneof![
            7 => self.num_text().prop_map(Value::String),
            1 => any::<i64>().prop_map(|n| Value::Number(n.into())),
            1 => any::<f64>().prop_map(json_f64),
            2 => self.arb_leaf(),
        ]
        .boxed()
    }

    /// A numeric wire field spoken as a JSON NUMBER (deribit, dukascopy): usually a finite float,
    /// sometimes an integer, any float, decimal text or any leaf.
    pub fn jnum(self) -> BoxedStrategy<Value> {
        prop_oneof![
            5 => (-1.0e6f64..1.0e6).prop_map(json_f64),
            2 => any::<i64>().prop_map(|n| Value::Number(n.into())),
            1 => any::<f64>().prop_map(json_f64),
            2 => self.num_text().prop_map(Value::String),
            1 => self.arb_leaf(),
        ]
        .boxed()
    }

    /// An integer field (an epoch-ms timestamp, an id): usually a non-negative realistic value.
    pub fn jint(self) -> BoxedStrategy<Value> {
        prop_oneof![
            6 => (0i64..4_102_444_800_000).prop_map(|n| Value::Number(n.into())),
            1 => any::<i64>().prop_map(|n| Value::Number(n.into())),
            1 => self.num_text().prop_map(Value::String),
            1 => self.arb_leaf(),
        ]
        .boxed()
    }

    /// A sequence-number field: small chains (so a `prev` id can match), else anything.
    pub fn seqv(self) -> BoxedStrategy<Value> {
        prop_oneof![
            6 => (0u64..8).prop_map(|n| Value::Number(n.into())),
            2 => any::<u64>().prop_map(|n| Value::Number(n.into())),
            2 => self.arb_leaf(),
        ]
        .boxed()
    }
}

/// One optional `"key": value` pair of a [`TokenKit`] object: usually present with a value of the
/// right shape, sometimes with a hostile leaf, sometimes absent.
pub type Field = BoxedStrategy<Option<(String, Value)>>;

/// One of `list` as a JSON string.
pub fn tok(list: &'static [&'static str]) -> BoxedStrategy<Value> {
    prop::sample::select(list.to_vec()).prop_map(|s| Value::String(s.to_string())).boxed()
}

/// Numeric text: an integer, a signed decimal, or an exponent form.
pub fn numeric_string() -> BoxedStrategy<String> {
    prop_oneof!["[0-9]{1,6}", "-?[0-9]{1,8}\\.[0-9]{1,8}", "[0-9]{1,3}e-?[0-9]{1,3}"].boxed()
}

/// An i64 JSON number: realistic epoch-ms, zero, negative, extremes.
pub fn int() -> BoxedStrategy<Value> {
    prop_oneof![
        Just(0i64),
        Just(-1i64),
        Just(i64::MIN),
        Just(i64::MAX),
        1_600_000_000_000i64..1_800_000_000_000i64,
        any::<i64>(),
    ]
    .prop_map(|n| json!(n))
    .boxed()
}

/// An object of the present [`Field`]s.
pub fn object(fields: Vec<Field>) -> BoxedStrategy<Value> {
    fields.prop_map(|fs| Value::Object(fs.into_iter().flatten().collect())).boxed()
}

/// A book tick size: the plausible, the degenerate (zero, negative, NaN, inf) and any float.
pub fn tick_size() -> BoxedStrategy<f64> {
    prop_oneof![
        Just(0.01),
        Just(1.0),
        Just(0.0),
        Just(-1.0),
        Just(f64::NAN),
        Just(f64::INFINITY),
        Just(1e-300),
        Just(1e300),
        any::<f64>(),
    ]
    .boxed()
}

/// A [`TokenKit`] venue's vocabulary: `keys`, the real field names its decoders read; `tokens`, the
/// string values they `match` on; `special`, hostile numeric / textual leaves.
#[derive(Clone, Copy)]
pub struct TokenKit {
    pub keys: &'static [&'static str],
    pub tokens: &'static [&'static str],
    pub special: &'static [&'static str],
}

impl TokenKit {
    /// A numeric string value, mostly well-formed, sometimes one of `special`.
    pub fn num_str(self) -> BoxedStrategy<Value> {
        prop_oneof![
            8 => numeric_string().prop_map(Value::String),
            1 => tok(self.special),
        ]
        .boxed()
    }

    /// Any leaf the venue could (mis)send.
    pub fn arb_leaf(self) -> BoxedStrategy<Value> {
        prop_oneof![
            2 => Just(Value::Null),
            2 => any::<bool>().prop_map(Value::Bool),
            3 => int(),
            1 => Just(Value::Number(u64::MAX.into())),
            2 => any::<f64>().prop_map(json_f64),
            4 => numeric_string().prop_map(Value::String),
            3 => tok(self.special),
            4 => tok(self.tokens),
            2 => "\\PC{0,12}".prop_map(Value::String),
            1 => Just(json!([])),
            1 => Just(json!({})),
        ]
        .boxed()
    }

    /// Arbitrary JSON, depth <= 4, keys from the real vocabulary.
    pub fn arb_json(self) -> BoxedStrategy<Value> {
        json_tree(self.arb_leaf(), move || weighted_key(5, self.keys), 4)
    }

    /// Arbitrary text: unicode, lossy-decoded byte noise, or valid JSON text.
    pub fn arb_text(self) -> BoxedStrategy<String> {
        prop_oneof![
            any::<String>(),
            prop::collection::vec(any::<u8>(), 0..256)
                .prop_map(|b| String::from_utf8_lossy(&b).into_owned()),
            self.arb_json().prop_map(|v| v.to_string()),
        ]
        .boxed()
    }

    /// `key` with a value of `valid`'s shape (80%) or a hostile leaf, absent 10% of the time.
    pub fn field(self, key: &'static str, valid: BoxedStrategy<Value>) -> Field {
        let value = prop_oneof![8 => valid, 2 => self.arb_leaf()];
        prop_oneof![
            9 => value.prop_map(move |v| Some((key.to_string(), v))),
            1 => Just(None),
        ]
        .boxed()
    }

    /// `[price, qty]` book levels, mostly well-formed.
    pub fn book_side(self) -> BoxedStrategy<Value> {
        let level = prop_oneof![
            8 => (self.num_str(), self.num_str()).prop_map(|(p, q)| json!([p, q])),
            1 => self.arb_json(),
        ];
        prop_oneof![
            9 => prop::collection::vec(level, 0..8).prop_map(Value::Array),
            1 => self.arb_leaf(),
        ]
        .boxed()
    }

    /// A REST body: text, JSON, or (mostly) a top-level array of `rows`, or one row.
    pub fn arb_body(self, rows: BoxedStrategy<Value>) -> BoxedStrategy<String> {
        prop_oneof![
            1 => self.arb_text(),
            2 => self.arb_json().prop_map(|v| v.to_string()),
            6 => prop::collection::vec(rows.clone(), 0..6).prop_map(|r| Value::Array(r).to_string()),
            1 => rows.prop_map(|r| r.to_string()),
        ]
        .boxed()
    }
}
