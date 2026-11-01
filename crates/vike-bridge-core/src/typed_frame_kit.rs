//! The venue-neutral half of the typed-frame ORACLE harnesses (`crates/bridges/{binance,bybit}/tests/typed_frame_props.rs`),
//! written once here behind `test-support` (with `proptest`).
//!
//! Each harness keeps its own OLD `route_frame` (the oracle, copied verbatim so it cannot drift with
//! production), its field layouts, its `Ty`, its value grammar and its envelopes; this module owns
//! only the shapes both had copied: the seeded book, the bit-exact book comparison, the agreement
//! check over the two decoders, the textual frame pieces and the deterministic shuffle.
//!
//! ⚠ **The strategy TREE is the contract** (as in [`crate::json_arb`]): [`arb_slot`] is the copies' tree
//! weight for weight, so a committed regression seed still replays.

use std::fmt::Debug;

use proptest::prelude::*;
use serde_json::Value;
use vike_marketdata::{BookLevel, L2Book};

/// One book side as `(price bits, qty bits)`, so a `NaN` quantity compares equal to itself.
pub type LevelBits = Vec<(u64, u64)>;

/// The deep-envelope arms' nesting: `DEEP_120` and `DEEP_120 + 20` sit either side of serde_json's
/// recursion limit (128).
pub const DEEP_120: usize = 120;

/// Timestamp values a field may carry: extremes, overflow, floats, a string, a non-number.
pub const TS_WEIRD: &[&str] = &[
    "0",
    "-1",
    "-9223372036854775808",
    "9223372036854775807",
    "9223372036854775808",
    "1.5",
    "1e3",
    r#""17""#,
    "null",
    "true",
    "[1]",
];

/// Values for a field nobody reads: other symbols, containers, a lone surrogate, broken JSON.
pub const ANY_WEIRD: &[&str] = &[
    r#""ETHUSDT""#,
    "1",
    "null",
    "[]",
    "{}",
    r#"{"a":[1,2,{"b":null}]}"#,
    r#""\ud800""#,
    r#""café""#,
    "1e999",
    "-",
    r#""unterminated"#,
    "tru",
    "[1,2",
];

/// `[price, qty]` level arrays: short, long, non-string, non-numeric, removals, truncated.
pub const LEVELS_WEIRD: &[&str] = &[
    "[]",
    r#"[["1"]]"#,
    r#"[["1","2","3"]]"#,
    r#"[[1,2]]"#,
    r#"[["x","1"]]"#,
    r#""abc""#,
    "null",
    "[null]",
    r#"[["1","2"],["NaN","3"]]"#,
    r#"[["60000.1","0"]]"#,
    r#"[["1","2"],"x"]"#,
    "[[]]",
    "{}",
    r#"[["1","2"]]"#,
    r#"[["1","2"],["3","4"],["5","6"],["7","8"]]"#,
    r#"[["1","2"]"#,
    r#"[["inf","1"],["1","-inf"]]"#,
    r#"[["100.00","0"],["99.50","0"],["99.00","0"]]"#,
];

/// Every level of the book, bit for bit, plus the sequence anchor.
pub fn book_state(b: &L2Book) -> (u64, u64, LevelBits, LevelBits) {
    let bits = |side: Vec<BookLevel>| -> LevelBits {
        side.iter().map(|l| (l.price.to_bits(), l.qty.to_bits())).collect()
    };
    let (bids, asks) = b.top_n(usize::MAX);
    (b.last_seq, b.tick_size.to_bits(), bits(bids), bits(asks))
}

/// A two-sided book anchored at `last_seq`, so a diff has something to overwrite or remove.
pub fn seeded(last_seq: u64) -> L2Book {
    let mut b = L2Book::new(0.01);
    b.apply_snapshot(
        last_seq,
        &[BookLevel::new(100.0, 1.0), BookLevel::new(99.5, 2.0), BookLevel::new(99.0, 3.0)],
        &[BookLevel::new(100.5, 1.0), BookLevel::new(101.0, 2.0), BookLevel::new(101.5, 3.0)],
    );
    b
}

/// The new decode and the oracle, from the same book, on the same text: same event, same book.
pub fn agree<E: Debug>(
    text: &str,
    last_seq: u64,
    old: impl FnOnce(&str, &mut L2Book) -> E,
    new: impl FnOnce(&str, &mut L2Book) -> E,
) -> Result<(), TestCaseError> {
    let (mut old_book, mut new_book) = (seeded(last_seq), seeded(last_seq));
    let old = old(text, &mut old_book);
    let new = new(text, &mut new_book);
    prop_assert_eq!(format!("{old:?}"), format!("{new:?}"), "event differs on {:?}", text);
    prop_assert_eq!(book_state(&old_book), book_state(&new_book), "book differs on {:?}", text);
    Ok(())
}

/// A slot of a textual frame: absent, canonical, or the n-th weird value.
pub fn arb_slot() -> impl Strategy<Value = Option<u8>> {
    prop_oneof![
        1 => Just(None),
        6 => Just(Some(0u8)),
        3 => (1u8..40).prop_map(Some),
    ]
}

/// One splitmix64 step: the deterministic key-order shuffle's generator.
pub fn mix(s: &mut u64) -> u64 {
    *s = s.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *s;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Fisher-Yates over [`mix`], so the same seed always gives the same key order.
pub fn shuffle<T>(v: &mut [T], seed: u64) {
    let mut s = seed;
    for i in (1..v.len()).rev() {
        let j = (mix(&mut s) % (i as u64 + 1)) as usize;
        v.swap(i, j);
    }
}

/// `n_levels` `["px","qty"]` rows walking away from the seeded touch; a zero qty removes a level.
pub fn levels_text(n_levels: u8, c: u8, side_bid: bool) -> String {
    let rows: Vec<String> = (0..n_levels)
        .map(|i| {
            let step = f64::from(i) * 0.5;
            let px = if side_bid { 100.0 - step } else { 100.5 + step };
            let qty = (u32::from(i) * 7 + u32::from(c)) % 5; // 0 removes a level
            format!(r#"["{px:.2}","{qty}"]"#)
        })
        .collect();
    format!("[{}]", rows.join(","))
}

/// `key` as JSON text, optionally with its first character as a `\u` escape (the same key decoded).
pub fn key_text(key: &str, escaped: bool) -> String {
    if escaped {
        // the first character as a \u escape: the same key once decoded
        let mut chars = key.chars();
        let head = chars.next().map_or(String::new(), |c| format!("\\u{:04x}", c as u32));
        format!("\"{head}{}\"", chars.as_str())
    } else {
        format!("\"{key}\"")
    }
}

/// Compact JSON with the venue's `lead` key first at the top level; a `Value` would sort the keys,
/// which is a different (declined) frame.
pub fn wire_text(v: &Value, lead: &str) -> String {
    let Value::Object(m) = v else { return v.to_string() };
    let mut parts = Vec::new();
    if let Some(x) = m.get(lead) {
        parts.push(format!("\"{lead}\":{x}"));
    }
    for (k, x) in m {
        if k != lead {
            parts.push(format!("{}:{x}", serde_json::to_string(k).expect("a key serializes")));
        }
    }
    format!("{{{}}}", parts.join(","))
}
