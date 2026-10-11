//! "Arbitrary input never panics" harness for the PRIVATE wire decoders of this module: the
//! history time parse [`epoch_ms`], the strict page decode [`decode_page`], the paging walk
//! [`walk_pages`] driven by a scripted wire, and the operator-facing error scrub [`failure_text`].
//! The public decoders are covered in `crates/bridges/oanda/tests/decoder_never_panics.rs`; these
//! are reachable from outside the crate only through a live HTTP fetch, so they get a sibling unit
//! file in the `klines_tests.rs` style.
//!
//! The property is TOTALITY: a hostile page may be refused with a `SourceError` / `Err`, but it must
//! never panic the backfill worker, and the walk must always terminate. Beyond that only the
//! invariants the code states are asserted: a decoded page is strictly increasing, a walk returns
//! each candle once, oldest first, inside the window, and a scrubbed error text never contains the
//! bearer token.

use super::*;
use proptest::prelude::*;
use serde_json::{Map, Value, json};

/// The (code, step) rows a walk is driven with.
const GRAINS: &[Grain] = &[
    Grain { granularity: "S5", step_s: 5 },
    Grain { granularity: "M1", step_s: 60 },
    Grain { granularity: "H1", step_s: 3600 },
    Grain { granularity: "D", step_s: 86_400 },
];

const KEYS: &[&str] = &["candles", "complete", "time", "volume", "mid", "o", "h", "l", "c"];

fn arb_leaf() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        prop_oneof![Just(0i64), Just(-1), Just(i64::MAX), Just(i64::MIN), any::<i64>()]
            .prop_map(Value::from),
        any::<f64>()
            .prop_map(|f| serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number)),
        prop::sample::select(vec![
            "NaN",
            "inf",
            "-0",
            "1e999",
            "-1e999",
            "",
            ".",
            "-",
            "9223372036854775807",
            "9223372036854775808",
            "9223372036854775.807",
            "99999999999999999999999999999",
            "1478012400.000000000",
            "2016-11-01T15:00:00.000000000Z",
        ])
        .prop_map(|s| Value::String(s.to_string())),
        "[0-9]{1,24}(\\.[0-9]{0,24})?".prop_map(Value::String),
        any::<String>().prop_map(Value::String),
        Just(Value::Array(Vec::new())),
        Just(Value::Object(Map::new())),
    ]
}

/// Arbitrary JSON, depth <= 4, object keys drawn mostly from the real candles-response fields.
fn arb_json() -> impl Strategy<Value = Value> {
    let key = prop_oneof![
        8 => prop::sample::select(KEYS).prop_map(String::from),
        1 => "[a-zA-Z_]{1,8}",
    ];
    arb_leaf().prop_recursive(4, 64, 6, move |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..6).prop_map(Value::Array),
            prop::collection::vec((key.clone(), inner), 0..8)
                .prop_map(|kvs| Value::Object(kvs.into_iter().collect())),
        ]
    })
}

/// One candle's script: time step, fraction digits, completeness, four mid prices, and an
/// occasional hostile replacement for the time / the open.
type CandleSpec = (u64, String, bool, [String; 4], Option<Value>, Option<Value>);

fn arb_candle_spec() -> impl Strategy<Value = CandleSpec> {
    (
        1u64..10_000,
        "[0-9]{0,9}",
        any::<bool>(),
        [
            "[0-9]{1,2}(\\.[0-9]{1,5})?",
            "[0-9]{1,2}(\\.[0-9]{1,5})?",
            "[0-9]{1,2}(\\.[0-9]{1,5})?",
            "[0-9]{1,2}(\\.[0-9]{1,5})?",
        ],
        prop::option::weighted(0.1, arb_leaf()),
        prop::option::weighted(0.1, arb_leaf()),
    )
}

/// A candles response shaped like the venue's: times strictly increasing WITHIN the page, mostly
/// valid decimal prices, so the decode gets past the first refusal and the walk gets past page one.
fn arb_page() -> impl Strategy<Value = Value> {
    (0u64..2_000_000_000, prop::collection::vec(arb_candle_spec(), 0..5)).prop_map(
        |(base, specs)| {
            let mut t = base;
            let candles: Vec<Value> = specs
                .into_iter()
                .map(|(delta, frac, complete, [o, h, l, c], bad_time, bad_open)| {
                    t += delta;
                    let time = bad_time.unwrap_or_else(|| Value::String(format!("{t}.{frac:0<9}")));
                    let open = bad_open.unwrap_or(Value::String(o));
                    json!({"complete": complete, "time": time, "volume": 7,
                           "mid": {"o": open, "h": h, "l": l, "c": c}})
                })
                .collect();
            json!({ "candles": candles })
        },
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// `epoch_ms` is total over arbitrary text, and only ever accepts digits with one optional
    /// fractional part (the RFC 3339 spelling is refused).
    #[test]
    fn epoch_ms_survives_arbitrary_text(time in any::<String>()) {
        if epoch_ms(&time).is_some() {
            prop_assert!(time.bytes().all(|b| b.is_ascii_digit() || b == b'.'), "{time:?}");
        }
    }

    /// ...and over digit strings of every length (the `checked_mul` / `checked_add` edge).
    #[test]
    fn epoch_ms_survives_digit_strings(time in "[0-9]{0,24}(\\.[0-9]{0,24})?") {
        let _ = epoch_ms(&time);
    }

    /// `decode_page` is total over arbitrary JSON; an accepted page is strictly increasing.
    #[test]
    fn decode_page_survives_structured_json(v in arb_json()) {
        if let Ok(page) = decode_page(&v) {
            prop_assert!(page.bars.windows(2).all(|w| w[0].ts < w[1].ts));
            prop_assert!(page.bars.len() <= page.total);
        }
    }

    /// ...and over venue-shaped pages with the occasional hostile time / price.
    #[test]
    fn decode_page_survives_shaped_pages(v in arb_page()) {
        if let Ok(page) = decode_page(&v) {
            prop_assert!(page.bars.windows(2).all(|w| w[0].ts < w[1].ts));
            prop_assert_eq!(page.total, v["candles"].as_array().map_or(0, Vec::len));
        }
    }

    /// The paging walk over a scripted run of hostile pages (then an empty page): always
    /// terminates, and returns each candle once, oldest first, inside the window.
    #[test]
    fn walk_pages_survives_hostile_pages(
        pages in prop::collection::vec(prop_oneof![4 => arb_page(), 1 => arb_json()], 0..5),
        grain in prop::sample::select(GRAINS),
        start_ms in prop_oneof![0i64..2_000_000_000_000, any::<i64>()],
        span in prop_oneof![0i64..4_000_000_000_000, any::<i64>()],
        page_count in 0usize..5,
    ) {
        let end_ms = start_ms.saturating_add(span);
        let mut pages = pages.into_iter();
        let out = walk_pages(&grain, "EUR_USD", start_ms, end_ms, page_count, |_| {
            Ok(pages.next().unwrap_or_else(|| json!({"candles": []})))
        });
        if let Ok(bars) = out {
            prop_assert!(bars.windows(2).all(|w| w[0].ts < w[1].ts));
            prop_assert!(bars.iter().all(|b| b.ts >= start_ms && b.ts <= end_ms));
        }
    }

    /// The error scrub is total over arbitrary bodies (JSON with / without `errorMessage`, noise),
    /// is bounded, and never lets the bearer token through.
    #[test]
    fn failure_text_survives_arbitrary_bodies_and_masks_the_token(
        token in "[A-Za-z0-9-]{8,40}",
        before in any::<String>(),
        after in any::<String>(),
        wrap in any::<bool>(),
    ) {
        let secret = Secret::new(token.clone()).expect("a non-blank token");
        let raw = format!("{before}{token}{after}");
        let body = if wrap { json!({ "errorMessage": raw }).to_string() } else { raw };
        let text = failure_text(&secret, &body);
        prop_assert!(text.chars().count() <= 200);
        prop_assert!(!text.contains(&token), "the token survived the scrub: {text:?}");
    }
}
