//! "Arbitrary input never panics" harness for the PRIVATE wire folds of this module: the
//! pricing-line fold [`fold_pricing_line`] (decode + the stateful [`StreamHealth`] it drives), the
//! candles fold [`fold_candles_response`], and the line reader the shared pump reads the pricing stream through
//! (`LineStream`) fed byte noise.
//! The public decoders under them are covered in `crates/bridges/oanda/tests/decoder_never_panics.rs`;
//! the two folds are not reachable from outside the crate, so they get a sibling unit file in the
//! `feed/feed_tests.rs` style, and the reader runs here as the lane wires it.
//!
//! The property is TOTALITY (no panic, no fabricated flood): a hostile line may fold to nothing, but
//! it must never kill the quote pump thread — a dead pump is a feed that silently goes quiet.
//!
//! REGRESSION: a PRICE whose `time` was a huge NEGATIVE epoch (`"-1e300"`) decoded, under the float read this
//! crate had, to `ts == i64::MIN`, and `StreamHealth::check_freshness` (`crates/vike-bridge-core/src/stream_health.rs`)
//! then computed `now_ms - ref_timestamp` unchecked - an overflow panic in a debug / overflow-checked build
//! (a silent wrap in release). The subtraction now saturates, and the exact read
//! (`vike_utility::calendar::parse_epoch_seconds_ms`) refuses such a time.

use crate::feed::bar::fold_candles_response;
use crate::feed::tick::fold_pricing_line;
use proptest::prelude::*;
use serde_json::{Value, json};
use vike_bridge_core::json_arb::arb_noise;
use vike_bridge_core::stream_health::StreamHealth;
use vike_bridge_core::websocket_session::market::WebsocketSessionMarketStream;
use vike_bridge_core::websocket_session::user::line_stream::{LineFraming, LineStream};

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
/// Negative and astronomically large epoch text, which the exact read refuses or overflows on.
const HOSTILE_TIME: &str = "-?[0-9]{1,320}(\\.[0-9]{1,9})?";

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
                prop_assert_eq!(q.local_timestamp, now);
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
        last_closed_timestamp in any::<i64>(),
        free in prop_oneof![arb_leaf(), arb_candle()],
    ) {
        let n = candles.len();
        let response = json!({ "candles": candles });
        let (fresh, _forming, new_last) = fold_candles_response(&response, last_closed_timestamp);
        prop_assert!(fresh.len() <= n);
        prop_assert!(fresh.iter().all(|b| b.timestamp > last_closed_timestamp));
        prop_assert_eq!(new_last, fresh.last().map_or(last_closed_timestamp, |b| b.timestamp));
        let _ = fold_candles_response(&free, last_closed_timestamp);
    }

    /// The line reader the pump reads the pricing stream through, fed byte noise and well-formed lines over a run of
    /// bodies, with the quote fold riding every frame the way `quotes_main` wires it: never a panic, every body ends
    /// in a close (never a frame from nothing), never more quotes than frames.
    #[test]
    fn the_line_reader_survives_byte_noise_bodies(
        bodies in prop::collection::vec(
            prop_oneof![
                prop::collection::vec(any::<u8>(), 0..300),
                prop::collection::vec(arb_line(), 0..6).prop_map(|ls| ls.join("\n").into_bytes()),
            ],
            1..4,
        ),
    ) {
        let mut health = StreamHealth::new(300_000);
        let (mut frames, mut quotes) = (0usize, 0usize);
        for body in bodies {
            let most = body.len() + 1;
            let body = std::io::Cursor::new(body);
            let mut lines = LineStream::new(body, LineFraming::NewlineDelimitedJson);
            let mut reads = 0;
            while let Ok(frame) = lines.read_frame() {
                frames += 1;
                reads += 1;
                prop_assert!(reads <= most, "{reads} frames from a body of {most} bytes");
                let (q, _) = fold_pricing_line(&frame, "eurusd", &mut health, 1_700_000_000_000);
                quotes += usize::from(q.is_some());
            }
        }
        prop_assert!(quotes <= frames, "{quotes} quotes from {frames} frames");
    }
}
