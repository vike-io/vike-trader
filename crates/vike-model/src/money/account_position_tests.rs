use super::*;

/// The five copies' shape before the side was typed: the same fields, the side a `String`.
#[derive(Serialize)]
struct WordPosition<'a> {
    symbol: &'a str,
    position_side: &'a str,
    size: f64,
    average_price: f64,
}

/// The venue-naming copy's shape (the journal's row before the side was typed).
#[derive(Serialize)]
struct WordVenuePosition<'a> {
    venue: &'a str,
    symbol: &'a str,
    position_side: &'a str,
    size: f64,
    average_price: f64,
}

/// The journal's encoding (`vike_journal`'s `frame::encode_into`): named MessagePack, human-readable.
fn journal_bytes(value: &impl Serialize) -> Vec<u8> {
    let mut buffer = Vec::new();
    let mut serializer =
        rmp_serde::Serializer::new(&mut buffer).with_struct_map().with_human_readable();
    value.serialize(&mut serializer).expect("serializes");
    buffer
}

const SIDES: [(PositionSide, &str); 3] =
    [(PositionSide::Both, "BOTH"), (PositionSide::Long, "LONG"), (PositionSide::Short, "SHORT")];

/// Typing the side changes no byte: for every side, the JSON a vike.db row and the wire hold and the
/// MessagePack the journal holds are exactly the string-sided copy's, and they read back.
#[test]
fn a_typed_side_writes_the_bytes_the_word_did() {
    for (side, word) in SIDES {
        let typed = AccountPosition {
            symbol: "BTCUSDT".into(),
            position_side: side,
            size: -1.5,
            average_price: 99.5,
        };
        let worded = WordPosition {
            symbol: "BTCUSDT",
            position_side: word,
            size: -1.5,
            average_price: 99.5,
        };
        let json = serde_json::to_string(&typed).expect("serializes");
        assert_eq!(json, serde_json::to_string(&worded).expect("serializes"), "{word}");
        assert_eq!(
            json,
            format!(
                r#"{{"symbol":"BTCUSDT","position_side":"{word}","size":-1.5,"average_price":99.5}}"#
            )
        );
        assert_eq!(journal_bytes(&typed), journal_bytes(&worded), "{word}");
        assert_eq!(serde_json::from_str::<AccountPosition>(&json).expect("reads"), typed);

        let on_venue = typed.clone().on_venue("bybit".into());
        let worded_venue = WordVenuePosition {
            venue: "bybit",
            symbol: "BTCUSDT",
            position_side: word,
            size: -1.5,
            average_price: 99.5,
        };
        assert_eq!(
            serde_json::to_string(&on_venue).expect("serializes"),
            serde_json::to_string(&worded_venue).expect("serializes")
        );
        assert_eq!(journal_bytes(&on_venue), journal_bytes(&worded_venue), "{word}");
        assert_eq!(on_venue.without_venue(), typed);
    }
}

/// A side outside the three words is refused at the read, never folded into `BOTH`
/// (`PositionSide::from_word_or_both` is the lenient door, for a word that arrives as data).
#[test]
fn a_side_outside_the_three_words_is_refused() {
    for bad in ["both", "", "NET"] {
        let json =
            format!(r#"{{"symbol":"X","position_side":"{bad}","size":1.0,"average_price":1.0}}"#);
        assert!(serde_json::from_str::<AccountPosition>(&json).is_err(), "{bad:?}");
    }
}
