use super::*;

use vike_model::{Contract, Instrument, StockSplit};

fn rows(venue: &str, n: usize) -> Vec<Instrument> {
    (0..n)
        .map(|i| {
            Instrument::new(venue, format!("SYM{i}"), "BTC", "USDT", Contract::CryptoSpot)
                .with_description(format!("row {i}, tick {}", 0.1 + 0.2 * i as f64))
        })
        .collect()
}

/// A body crosses whole, an unchanged venue and a changed one alike, and every cut of it is refused, never read
/// short.
#[test]
fn a_body_round_trips_and_a_cut_body_is_refused() {
    let split = StockSplit::new("binance", "SYM0", "2020-08-31", 4.0).expect("a split");
    let venues = vec![
        KeptRowsOfVenue {
            venue: "binance".into(),
            stamp: "ab".repeat(32),
            changed: Some(EncodedKept {
                rows: kept_msgpack(&rows("binance", 3)).expect("rows"),
                splits: kept_msgpack(&[split]).expect("splits"),
            }),
        },
        KeptRowsOfVenue { venue: "okx".into(), stamp: "cd".repeat(32), changed: None },
    ];
    let body = encode_kept_rows(&venues);
    assert!(body.starts_with(&KEPT_ROWS_MAGIC));
    assert_eq!(decode_kept_rows(&body).expect("decodes"), venues);
    for cut in KEPT_ROWS_MAGIC.len() + 1..body.len() {
        assert!(decode_kept_rows(&body[..cut]).is_err(), "a body cut at {cut} is refused");
    }
    assert!(decode_kept_rows(b"{\"Error\":\"x\"}").is_err(), "JSON is no kept-rows body");
    let mut longer = body.clone();
    longer.push(0);
    assert!(decode_kept_rows(&longer).is_err(), "a byte past the last venue is refused");
    assert_eq!(decode_kept_rows(&encode_kept_rows(&[])).expect("no venue"), Vec::new());
}

/// Joined arrays are the array of every element in order, at each header width (fixarray, `array 16`, `array 32`),
/// and one part comes back byte for byte.
#[test]
fn joined_arrays_are_the_array_of_every_element() {
    for (a, b) in [(1, 2), (3, 20), (40_000, 30_000), (0, 0)] {
        let (left, right) = (rows("binance", a), rows("okx", b));
        let joined = join_msgpack_arrays(&[
            &kept_msgpack(&left).expect("left"),
            &kept_msgpack(&right).expect("right"),
        ])
        .expect("joined");
        let whole: Vec<Instrument> = left.iter().chain(&right).cloned().collect();
        assert_eq!(joined, kept_msgpack(&whole).expect("whole"), "{a} + {b}");
        let one = kept_msgpack(&left).expect("one");
        assert_eq!(join_msgpack_arrays(&[&one]).expect("one part"), one, "{a}");
    }
    assert!(join_msgpack_arrays(&[b"\x81"]).is_err(), "a map is no array");
}
