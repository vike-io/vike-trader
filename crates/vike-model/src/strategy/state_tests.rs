use serde::{Deserialize, Serialize};

use super::StrategyState;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Saved {
    version: u32,
    kappa: f64,
    trades: Vec<(i64, f64, f64)>,
    name: String,
    armed: Option<bool>,
}

fn saved() -> Saved {
    Saved {
        version: 3,
        kappa: 0.1 + 0.2,
        trades: vec![(1_700_000_000_000, 64_321.5, 0.25), (1_700_000_000_250, f64::NAN, -0.0)],
        name: "spread".to_string(),
        armed: None,
    }
}

/// A state reads back into its own type with every float's bits, a NaN and a negative zero included.
#[test]
fn a_state_reads_back_bit_for_bit() {
    let state = StrategyState::of(&saved()).expect("encodes");
    let back: Saved = state.read().expect("reads");
    let bits =
        |s: &Saved| s.trades.iter().map(|t| (t.1.to_bits(), t.2.to_bits())).collect::<Vec<_>>();
    assert_eq!(bits(&back), bits(&saved()));
    assert_eq!(
        (back.version, back.kappa.to_bits(), back.name.as_str()),
        (3, saved().kappa.to_bits(), "spread")
    );
    assert_eq!(StrategyState::from_bytes(state.bytes().to_vec()), state);
    StrategyState::check(state.bytes()).expect("one msgpack value");
}

/// An OLD JSON state (a sidecar written before) reads into the strategy's type as the document did, an
/// integer written for a float included; a fresh save of it is the msgpack one.
#[test]
fn an_old_json_state_reads_as_its_document_did() {
    let document = serde_json::json!({
        "version": 3, "kappa": 1, "trades": [[1_700_000_000_000_i64, 64_321.5, 0.25]],
        "name": "spread", "armed": true,
    });
    let state = StrategyState::from_json_value(&document);
    let read: Saved = state.read().expect("the old document reads");
    let parsed: Saved = serde_json::from_value(document.clone()).expect("as JSON");
    assert_eq!(read, parsed);
    assert_eq!(state.to_json_value().expect("back to JSON"), document);
}

/// MEASURED, not asserted: a spread-maker-shaped state the size of the CI box's (227 KB as JSON, its trade window
/// most of it), each way. JSON is the path before `ABI_VERSION` 15 (`to_value` then text, a syntax scan as it
/// arrived, a parse then `from_value` at a load); msgpack is [`StrategyState::of`], [`StrategyState::check`]
/// and [`StrategyState::read`]. Best of seven runs. Run it with
/// `cargo test -p vike-model --release --lib -- --ignored --nocapture measure_a_maker_sized_state`.
#[test]
#[ignore = "a measurement: run it in release with --ignored --nocapture"]
fn measure_a_maker_sized_state() {
    #[derive(Serialize, Deserialize)]
    struct Side {
        placed: bool,
        own: Option<(f64, f64)>,
        quoted_timestamp: i64,
    }
    #[derive(Serialize, Deserialize)]
    struct Accumulators {
        sigma2: Option<f64>,
        last_mid: Option<(f64, i64)>,
        last_quote_mid: Option<f64>,
        trades: Vec<(f64, f64, i64)>,
        sum_w: f64,
        sum_w_delta: f64,
    }
    #[derive(Serialize, Deserialize)]
    struct Maker {
        v: u32,
        bid_suppressed_until: i64,
        ask_suppressed_until: i64,
        as_state: Option<Accumulators>,
        bid: Side,
        ask: Side,
    }
    let trade = |i: i64| {
        let i_f = i as f64;
        (
            (i_f * 0.618_033_988_7).fract() * 1e-3,
            0.001 + (i_f * 0.37).fract() * 0.05,
            1_763_500_000_000 + i * 137,
        )
    };
    let mut trades: Vec<(f64, f64, i64)> = Vec::new();
    let maker = |trades: Vec<(f64, f64, i64)>| Maker {
        v: 1,
        bid_suppressed_until: 0,
        ask_suppressed_until: 1_763_500_100_000,
        as_state: Some(Accumulators {
            sigma2: Some(1.234_567_890_123e-7),
            last_mid: Some((91_234.55, 1_763_500_099_000)),
            last_quote_mid: Some(91_234.6),
            trades,
            sum_w: 12.345_678_901_234,
            sum_w_delta: 0.000_123_456_789,
        }),
        bid: Side {
            placed: true,
            own: Some((91_230.1, 0.002)),
            quoted_timestamp: 1_763_500_098_000,
        },
        ask: Side {
            placed: true,
            own: Some((91_239.9, 0.002)),
            quoted_timestamp: 1_763_500_098_000,
        },
    };
    while serde_json::to_string(&maker(trades.clone())).expect("encodes").len() < 227 * 1024 {
        trades.extend((trades.len() as i64..trades.len() as i64 + 256).map(trade));
    }
    let state = maker(trades);
    let best = |f: &mut dyn FnMut()| {
        (0..7)
            .map(|_| {
                let start = std::time::Instant::now();
                f();
                start.elapsed()
            })
            .min()
            .expect("seven runs")
    };
    let text =
        serde_json::to_string(&serde_json::to_value(&state).expect("a document")).expect("text");
    let packed = StrategyState::of(&state).expect("encodes");
    let json_encode = best(&mut || {
        std::hint::black_box(
            serde_json::to_string(&serde_json::to_value(&state).expect("a document"))
                .expect("text"),
        );
    });
    let json_check = best(&mut || {
        std::hint::black_box(serde_json::from_str::<serde::de::IgnoredAny>(&text).expect("JSON"));
    });
    let json_load = best(&mut || {
        let document: serde_json::Value = serde_json::from_str(&text).expect("JSON");
        std::hint::black_box(serde_json::from_value::<Maker>(document).expect("reads"));
    });
    let msgpack_encode = best(&mut || {
        std::hint::black_box(StrategyState::of(&state).expect("encodes"));
    });
    let msgpack_check = best(&mut || StrategyState::check(packed.bytes()).expect("msgpack"));
    let msgpack_load = best(&mut || {
        std::hint::black_box(packed.read::<Maker>().expect("reads"));
    });
    let trades = state.as_state.as_ref().map_or(0, |a| a.trades.len());
    println!("maker-sized state, {trades} trades in its window (best of 7):");
    println!("  size   JSON {} B, msgpack {} B", text.len(), packed.bytes().len());
    println!("  encode JSON {json_encode:?}, msgpack {msgpack_encode:?}");
    println!("  check  JSON {json_check:?}, msgpack {msgpack_check:?}");
    println!("  load   JSON {json_load:?}, msgpack {msgpack_load:?}");
    let back: Maker = packed.read().expect("reads");
    assert_eq!(
        rmp_serde::to_vec_named(&back).expect("encodes"),
        packed.bytes(),
        "the bytes round trip"
    );
}

/// The scan a reader runs as a state arrives: one msgpack value, nothing after it.
#[test]
fn the_check_refuses_what_is_not_one_msgpack_value() {
    let state = StrategyState::of(&saved()).expect("encodes");
    let mut trailing = state.bytes().to_vec();
    trailing.push(0xc0);
    for (bytes, words) in [
        (&b"{\"json\": 1}"[..], "not msgpack"),
        (&state.bytes()[..state.bytes().len() - 1], "not msgpack"),
        (&trailing[..], "1 bytes after its value"),
        (&[0xc1][..], "not msgpack"),
    ] {
        let refused = StrategyState::check(bytes).expect_err("refused");
        assert!(refused.contains(words), "{refused}");
    }
}

/// A state nested deeper than `StrategyState::MAX_DEPTH` is refused in words by `check` and by `read`
/// alike, and refusing it costs no deep recursion: on a 2 MiB thread stack (the tradehub reader thread's
/// default), in the DEBUG build `cargo test` makes, where msgpack's own limit (1024 levels) overflows.
#[test]
fn a_state_nested_past_the_cap_is_refused_by_check_and_read_alike() {
    let nested = |depth: usize| {
        let mut bytes = vec![0x91; depth]; // an array of one, `depth` times...
        bytes.push(0xc0); // ...around a nil
        bytes
    };
    let at_cap = nested(StrategyState::MAX_DEPTH);
    StrategyState::check(&at_cap).expect("a state at the cap is checked");
    StrategyState::from_bytes(at_cap).read::<serde_json::Value>().expect("and reads");
    for depth in [StrategyState::MAX_DEPTH + 1, 1023, 100_000] {
        let deep = nested(depth);
        let (checked, read) = std::thread::Builder::new()
            .stack_size(2 << 20)
            .spawn(move || {
                let read = StrategyState::from_bytes(deep.clone()).read::<serde_json::Value>();
                (StrategyState::check(&deep), read.map(|_| ()))
            })
            .expect("a thread")
            .join()
            .expect("refused without overflowing the stack");
        for refused in [checked.expect_err("check refuses"), read.expect_err("read refuses")] {
            assert!(refused.contains("nests deeper than 128 levels"), "{depth}: {refused}");
        }
    }
}

/// A sequence's length is the wire's CLAIM (a 5-byte msgpack array header can claim 4 G elements): the
/// state allocates at most a capped amount ahead of the bytes that actually arrive.
#[test]
fn a_claimed_length_is_not_trusted_for_the_allocation() {
    struct Claims(std::vec::IntoIter<u8>);
    impl Iterator for Claims {
        type Item = u8;
        fn next(&mut self) -> Option<u8> {
            self.0.next()
        }
        // An exact claim, and a lie: trusted, it overflows the allocation.
        fn size_hint(&self) -> (usize, Option<usize>) {
            (usize::MAX, Some(usize::MAX))
        }
    }
    let lying = serde::de::value::SeqDeserializer::<_, serde::de::value::Error>::new(Claims(
        vec![1, 2, 3].into_iter(),
    ));
    assert_eq!(StrategyState::deserialize(lying).expect("three bytes").bytes(), [1, 2, 3]);
    let claim = rmp_serde::from_slice::<StrategyState>(&[0xdd, 0xff, 0xff, 0xff, 0xff]);
    assert!(claim.is_err(), "a 4 G claim with nothing behind it is refused: {claim:?}");
}
