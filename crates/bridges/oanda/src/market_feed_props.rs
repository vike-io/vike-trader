//! "Arbitrary input never panics" harness for the PRIVATE wire folds of this module: the
//! pricing-line fold [`fold_pricing_line`] (decode + the stateful [`StreamHealth`] it drives), the
//! candles fold [`fold_candles_response`], and the chunked-line pump [`pump_lines`] fed byte noise.
//! The public decoders under them are covered in `crates/bridges/oanda/tests/decoder_never_panics.rs`;
//! these three are not reachable from outside the crate, so they get a sibling unit file in the
//! `market_feed_tests.rs` style.
//!
//! The property is TOTALITY (no panic, no fabricated flood): a hostile line may fold to nothing, but
//! it must never kill the quote pump thread — a dead pump is a feed that silently goes quiet.
//!
//! REGRESSION: a PRICE whose `time` is a huge NEGATIVE epoch (`"-1e300"`) decodes to
//! `ts == i64::MIN`, and `StreamHealth::check_freshness` (`crates/vike-bridge-core/src/stream_health.rs`)
//! then computed `now_ms - ref_ts` unchecked - an overflow panic in a debug / overflow-checked build
//! (a silent wrap in release). The subtraction now saturates.

use super::*;
use proptest::prelude::*;
use serde_json::{Value, json};

/// A well-formed PRICE / HEARTBEAT line whose `time` is drawn from `time` (a regex strategy).
fn price_line(time: &'static str) -> impl Strategy<Value = String> {
    (
        time,
        "-?[0-9]{1,3}(\\.[0-9]{1,6})?",
        "-?[0-9]{1,3}(\\.[0-9]{1,6})?",
        "[0-9]{1,8}",
        prop_oneof![Just("PRICE"), Just("HEARTBEAT")],
    )
        .prop_map(|(t, bid, ask, liquidity, kind)| {
            format!(
                r#"{{"type":"{kind}","time":"{t}","instrument":"EUR_USD","bids":[{{"price":"{bid}","liquidity":{liquidity}}}],"asks":[{{"price":"{ask}","liquidity":{liquidity}}}]}}"#
            )
        })
}

/// Epoch seconds a real venue could send: up to 12 digits, optional nanosecond fraction.
const SANE_TIME: &str = "[0-9]{1,12}(\\.[0-9]{1,9})?";
/// Everything the float parser accepts, negative and astronomically large included.
const HOSTILE_TIME: &str = "-?[0-9]{1,320}(\\.[0-9]{1,9})?";

/// Raw bytes -> text the way a lossy socket read would produce it.
fn arb_noise() -> impl Strategy<Value = String> {
    prop::collection::vec(any::<u8>(), 0..512)
        .prop_map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

/// One line off the pricing stream: well-formed frames mixed with text, noise and truncations.
fn arb_line() -> impl Strategy<Value = String> {
    prop_oneof![
        4 => price_line(SANE_TIME),
        1 => any::<String>(),
        1 => arb_noise(),
        1 => price_line(SANE_TIME).prop_map(|l| l[..l.len() / 2].to_string()),
    ]
}

fn arb_leaf() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(Value::from),
        Just(Value::String("1e999".to_string())),
        Just(Value::String("NaN".to_string())),
        "-?[0-9]{1,30}(\\.[0-9]{1,12})?".prop_map(Value::String),
        any::<String>().prop_map(Value::String),
    ]
}

/// One `candles` element: `complete` mostly a real bool so both lanes run.
fn arb_candle() -> impl Strategy<Value = Value> {
    (
        prop_oneof![6 => any::<bool>().prop_map(Value::Bool), 1 => arb_leaf()],
        arb_leaf(),
        arb_leaf(),
        [arb_leaf(), arb_leaf(), arb_leaf(), arb_leaf()],
    )
        .prop_map(|(complete, time, volume, [o, h, l, c])| {
            json!({"complete": complete, "time": time, "volume": volume,
                   "mid": {"o": o, "h": h, "l": l, "c": c}})
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// (c) A run of lines folded into ONE `StreamHealth` at a moving clock never panics; a quote is
    /// relabelled and stamped by the fold, and one line discloses at most a recovery + a freshness
    /// verdict.
    #[test]
    fn fold_pricing_line_survives_a_line_sequence(
        lines in prop::collection::vec(arb_line(), 1..8),
        steps in prop::collection::vec(0i64..400_000, 8),
        start in 0i64..2_000_000_000_000,
        threshold in prop_oneof![Just(0i64), 1i64..600_000],
    ) {
        let mut health = StreamHealth::new(threshold);
        let mut now = start;
        for (line, step) in lines.iter().zip(steps) {
            now += step;
            let (quote, events) = fold_pricing_line(line, "eurusd", &mut health, now);
            prop_assert!(events.len() <= 2, "{} health events from one line", events.len());
            if let Some(q) = quote {
                prop_assert_eq!(q.symbol, "eurusd");
                prop_assert_eq!(q.local_ts, now);
            }
        }
    }

    /// The same fold across a transport gap opening and closing between lines.
    #[test]
    fn fold_pricing_line_survives_gaps_between_lines(
        lines in prop::collection::vec(arb_line(), 1..8),
        gaps in prop::collection::vec(any::<bool>(), 8),
        start in 0i64..2_000_000_000_000,
    ) {
        let mut health = StreamHealth::new(300_000);
        let mut now = start;
        for (line, gap) in lines.iter().zip(gaps) {
            now += 1_000;
            if gap {
                let _ = health.enter_gap(now);
            }
            let _ = fold_pricing_line(line, "eurusd", &mut health, now);
        }
    }

    /// REGRESSION, see the module doc: a hostile (negative / astronomical) `time` reaches
    /// `StreamHealth::check_freshness`.
    #[test]
    fn fold_pricing_line_survives_hostile_venue_times(
        lines in prop::collection::vec(price_line(HOSTILE_TIME), 1..6),
        start in 0i64..2_000_000_000_000,
    ) {
        let mut health = StreamHealth::new(300_000);
        for line in &lines {
            let _ = fold_pricing_line(line, "eurusd", &mut health, start);
        }
    }

    /// `fold_candles_response` is total over candle responses with hostile prices / times /
    /// completeness and an arbitrary watermark: it only ever emits bars NEWER than the watermark,
    /// never more than the response carries, and the advanced watermark is that of the last bar.
    #[test]
    fn fold_candles_response_survives_hostile_candles(
        candles in prop::collection::vec(arb_candle(), 0..6),
        last_closed_ts in any::<i64>(),
        free in prop_oneof![arb_leaf(), arb_candle()],
    ) {
        let n = candles.len();
        let resp = json!({ "candles": candles });
        let (fresh, _forming, new_last) = fold_candles_response(&resp, last_closed_ts);
        prop_assert!(fresh.len() <= n);
        prop_assert!(fresh.iter().all(|b| b.ts > last_closed_ts));
        prop_assert_eq!(new_last, fresh.last().map_or(last_closed_ts, |b| b.ts));
        let _ = fold_candles_response(&free, last_closed_ts);
    }

    /// The pump itself, fed byte noise and well-formed lines over a scripted run of dials, with the
    /// quote fold riding every line the way `quotes_main` wires it: never a panic, never more
    /// quotes than lines.
    #[test]
    fn pump_lines_survives_byte_noise_bodies(
        bodies in prop::collection::vec(
            prop_oneof![
                prop::collection::vec(any::<u8>(), 0..300),
                prop::collection::vec(arb_line(), 0..6).prop_map(|ls| ls.join("\n").into_bytes()),
            ],
            1..4,
        ),
    ) {
        let stop = AtomicBool::new(false);
        let mut dials = bodies.into_iter();
        let dial = || match dials.next() {
            Some(body) => Ok(std::io::Cursor::new(body)),
            None => {
                stop.store(true, Ordering::Relaxed);
                Err("script over".to_string())
            }
        };
        let mut health = StreamHealth::new(300_000);
        let (mut lines, mut quotes) = (0usize, 0usize);
        pump_lines(&stop, Duration::ZERO, dial, |ev| {
            if let PumpEvent::Line(l) = ev {
                lines += 1;
                let (q, _events) = fold_pricing_line(l, "eurusd", &mut health, 1_700_000_000_000);
                quotes += usize::from(q.is_some());
            }
        });
        prop_assert!(quotes <= lines, "{quotes} quotes from {lines} lines");
    }
}
