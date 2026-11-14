use super::*;
use vike_marketdata::L2BookLevel;
use vike_model::StreamLabel;

/// THIS TEST IS THE ARGUMENT FOR THE MANUAL SERDE IMPL (see `MarketDataSessionId`'s `Serialize` doc): a
/// derived `u128` fails `serde_json::to_value` above `u64::MAX` and reaches `jq` as an `f64`.
#[test]
fn a_session_id_rides_as_hex_not_a_number() {
    let id = MarketDataSessionId(u128::MAX);
    let v = serde_json::to_value(id).expect("to_value must not choke — this is the whole point");
    assert!(v.is_string(), "the session id must ride as a STRING, got {v:?}");
    assert_eq!(v.as_str().unwrap(), "ffffffffffffffffffffffffffffffff");
    assert_eq!(v.as_str().unwrap().len(), 32, "always 32 hex chars, zero-padded");
    let back: MarketDataSessionId = serde_json::from_value(v).expect("round trip");
    assert_eq!(back, id);

    // ...and a SMALL id is padded rather than printed short, so an id is one width everywhere.
    assert_eq!(MarketDataSessionId(1).to_string(), "00000000000000000000000000000001");
    assert_eq!(
        serde_json::from_str::<MarketDataSessionId>("\"00000000000000000000000000000001\"")
            .unwrap(),
        MarketDataSessionId(1)
    );
    // A fresh id is not the zero one — the mint is wired to a generator, not to a default.
    assert_ne!(MarketDataSessionId::fresh(), MarketDataSessionId(0));
}

/// Depth is NOT part of a key, and two specs differing only in depth name one subscription.
#[test]
fn depth_is_not_part_of_a_subscription_key() {
    let a = MarketDataSpec {
        venue: "binance".into(),
        symbol: "BTCUSDT.P".into(),
        lane: StreamLabel::Depth,
        depth_levels: Some(200),
    };
    let b = MarketDataSpec { depth_levels: None, ..a.clone() };
    assert_eq!(a.key(), b.key());
    assert_eq!(a.resolved_depth(), MARKET_DATA_DEPTH_LEVELS_CEILING);
    assert_eq!(b.resolved_depth(), MARKET_DATA_DEPTH_LEVELS_DEFAULT);
    // ...and a request ABOVE the ceiling is clamped rather than refused.
    let deep = MarketDataSpec { depth_levels: Some(5_000), ..a.clone() };
    assert_eq!(deep.resolved_depth(), MARKET_DATA_DEPTH_LEVELS_CEILING);
    // A zero is a floor of one, not an empty ladder.
    let zero = MarketDataSpec { depth_levels: Some(0), ..a };
    assert_eq!(zero.resolved_depth(), 1);
}

/// A spec with no depth request carries NO `depth_levels` key on the wire (the `Welcome.nonce`
/// attribute pair), and one with a depth carries it.
#[test]
fn an_absent_depth_is_absent_from_the_bytes() {
    let bare = MarketDataSpec {
        venue: "polymarket".into(),
        symbol: "123".into(),
        lane: StreamLabel::Trades,
        depth_levels: None,
    };
    let s = serde_json::to_string(&bare).unwrap();
    assert!(!s.contains("depth_levels"), "an absent depth must not ride as null: {s}");
    let deep = MarketDataSpec { depth_levels: Some(25), ..bare.clone() };
    assert!(serde_json::to_string(&deep).unwrap().contains("depth_levels"));
    assert_eq!(serde_json::from_str::<MarketDataSpec>(&s).unwrap(), bare);
}

/// EXHAUSTIVE over [`MarketDataRefusal`]: every variant is classified, and both classes are non-empty —
/// a classifier that answered one way for everything would satisfy neither assertion.
#[test]
fn a_refusal_classifies_permanently_or_not() {
    let all = [
        MarketDataRefusal::UnknownVenue,
        MarketDataRefusal::VenueNotServed("binance, polymarket".into()),
        MarketDataRefusal::LaneUnsupported(
            "venue's declared VenueCapabilities.live_data serves no…".into(),
        ),
        MarketDataRefusal::SymbolRejected("a subscription symbol of 10000 bytes exceeds…".into()),
        MarketDataRefusal::KeyCapTotal { held: 64, cap: 64 },
        MarketDataRefusal::KeyCapVenue { venue: "binance".into(), held: 16, cap: 16 },
        MarketDataRefusal::SpecCapSession { held: 64, cap: 64 },
        MarketDataRefusal::NotHeld,
    ];
    let permanent = all.iter().filter(|r| r.is_permanent()).count();
    assert_eq!(permanent, 4, "the three CAPABILITY refusals plus the VALIDITY one are permanent");
    assert_eq!(all.len() - permanent, 4, "...and the three CAPS and `NotHeld` are not");
    for r in &all {
        // The compile-time completeness guard, mirroring `is_permanent`'s own match.
        match r {
            MarketDataRefusal::UnknownVenue
            | MarketDataRefusal::VenueNotServed(_)
            | MarketDataRefusal::LaneUnsupported(_)
            | MarketDataRefusal::SymbolRejected(_) => assert!(r.is_permanent()),
            MarketDataRefusal::KeyCapTotal { .. }
            | MarketDataRefusal::KeyCapVenue { .. }
            | MarketDataRefusal::SpecCapSession { .. }
            | MarketDataRefusal::NotHeld => assert!(!r.is_permanent()),
        }
        // ...and each one rides the codec.
        let bytes = serde_json::to_vec(r).unwrap();
        assert_eq!(&serde_json::from_slice::<MarketDataRefusal>(&bytes).unwrap(), r);
    }
}

/// [`validate_market_data_symbol`] at its BOUNDARIES, both directions — the shape a length rule goes
/// wrong in is admitting one byte too many or refusing one byte too few, and neither is visible
/// from a test that only tries an absurd value.
#[test]
fn the_symbol_validator_is_exact_at_both_edges() {
    // The FLOOR that must be admitted: a polymarket CLOB token id is a uint256 in decimal and
    // 2^256-1 is exactly 78 digits, so this is the longest symbol this wire can ever carry.
    assert!(validate_market_data_symbol(&"7".repeat(78)).is_ok());
    assert!(
        validate_market_data_symbol(&"A".repeat(MARKET_DATA_MAX_SYMBOL_BYTES)).is_ok(),
        "at the bound"
    );
    let over = validate_market_data_symbol(&"A".repeat(MARKET_DATA_MAX_SYMBOL_BYTES + 1))
        .expect_err("one byte over the bound");
    assert!(over.contains(&(MARKET_DATA_MAX_SYMBOL_BYTES + 1).to_string()), "the length: {over}");
    assert!(over.contains(&MARKET_DATA_MAX_SYMBOL_BYTES.to_string()), "...and the cap: {over}");

    // BLANK, in all three spellings an argument arrives in.
    for blank in ["", " ", "\t\n "] {
        let why = validate_market_data_symbol(blank).expect_err("blank {blank:?}");
        assert!(why.contains("BLANK"), "{why}");
    }

    // The CONTROL-BYTE rule, which is part of the bound rather than beside it, and its
    // NARROWNESS — the three characters most likely to be swept up with it stay legal, because
    // a venue this plane does not yet serve spells real instruments with them (an IBKR OSI
    // local symbol carries padding spaces).
    assert!(validate_market_data_symbol("BTC\u{1}USDT").is_err());
    assert!(validate_market_data_symbol("BTC\u{7f}USDT").is_err(), "DEL is an ASCII control byte");
    assert!(validate_market_data_symbol("BTC USDT").is_ok(), "a space is not a control byte");
    assert!(validate_market_data_symbol("BTC\"USDT").is_ok(), "the quote is escaped, not refused");
    assert!(validate_market_data_symbol("BTC\\USDT").is_ok(), "...nor the backslash");
    assert!(validate_market_data_symbol("BTC₿").is_ok(), "non-ASCII rides raw through serde_json");

    // ⚠ The bound is in BYTES, not characters, because bytes are what the frame budget is
    // spent in. A multi-byte symbol therefore admits FEWER characters, and that is correct.
    let three_byte = "₿".repeat(MARKET_DATA_MAX_SYMBOL_BYTES / 3);
    assert_eq!(three_byte.len(), MARKET_DATA_MAX_SYMBOL_BYTES, "the fixture must sit ON the bound");
    assert!(validate_market_data_symbol(&three_byte).is_ok());
    assert!(validate_market_data_symbol(&format!("{three_byte}a")).is_err(), "one BYTE over");
}

/// **The lane TAGS of this wire, byte for byte, for every label.** The lane is `StreamLabel` itself
/// (the retired `StreamLabel` mirror carried the same four tags), so its serde tag IS this wire's: a spec
/// and a status name the lane by the variant name, never by the feed word. Only the quote lane has a
/// golden file (`fixtures/datahub_wire/v1_market_data_quotes_*.jsonl`); this pins the other three.
#[test]
fn the_lane_tags_on_the_wire_are_the_variant_names() {
    for (lane, tag) in [
        (StreamLabel::Depth, "Depth"),
        (StreamLabel::Book, "Book"),
        (StreamLabel::Trades, "Trades"),
        (StreamLabel::Quotes, "Quotes"),
    ] {
        let spec =
            MarketDataSpec { venue: "v".into(), symbol: "s".into(), lane, depth_levels: None };
        let json = serde_json::to_string(&spec).unwrap();
        assert_eq!(json, format!(r#"{{"venue":"v","symbol":"s","lane":"{tag}"}}"#));
        assert_eq!(serde_json::from_str::<MarketDataSpec>(&json).unwrap(), spec);
    }
    assert_eq!(StreamLabel::ALL.len(), 4, "a new label is a new lane tag: pin it above");
}

/// The lane → `DataClientVerb` mapping, which is what makes §7.5's gate a re-read of the declared
/// capability matrix. The PARTITION it produces on the initial venue set is asserted here rather
/// than described: five CEX serve Depth and refuse Book, polymarket the reverse.
#[test]
fn the_lane_verbs_partition_the_initial_venue_set() {
    use vike_venues::DataClientVerb;
    assert_eq!(DataClientVerb::from(StreamLabel::Depth), DataClientVerb::Depth);
    assert_eq!(DataClientVerb::from(StreamLabel::Book), DataClientVerb::Book);
    assert_eq!(DataClientVerb::from(StreamLabel::Trades), DataClientVerb::Trades);

    for v in ["binance", "bybit", "okx", "aster", "hyperliquid"] {
        assert!(
            vike_data::require_live_verb(v, StreamLabel::Depth.into()).is_ok(),
            "{v} must serve the Depth lane"
        );
        assert!(
            vike_data::require_live_verb(v, StreamLabel::Book.into()).is_err(),
            "{v} declares no lossless book lane — the wire must refuse it"
        );
    }
    assert!(vike_data::require_live_verb("polymarket", StreamLabel::Book.into()).is_ok());
    assert!(vike_data::require_live_verb("polymarket", StreamLabel::Depth.into()).is_err());
}

/// A whole frame rides the codec — the round trip over the type the wire actually carries,
/// including the level ORDER contract this type's doc states.
#[test]
fn a_book_frame_round_trips_best_first() {
    let snap = BookSnapshot {
        venue: "binance".into(),
        symbol: "BTCUSDT.P".into(),
        tick_size: 0.1,
        bids: vec![L2BookLevel::new(100.2, 3.0), L2BookLevel::new(100.1, 5.0)],
        asks: vec![L2BookLevel::new(100.3, 2.0), L2BookLevel::new(100.4, 7.0)],
        venue_timestamp: 1_700_000_000_000,
        venue_sequence: 42,
        sequence: 7,
    };
    // Best-first: bids DESCEND, asks ASCEND. `L2Book::top_n`'s own order.
    assert!(snap.bids[0].price > snap.bids[1].price);
    assert!(snap.asks[0].price < snap.asks[1].price);
    let frame = MarketDataFrame::Depth(snap.clone());
    let bytes = serde_json::to_vec(&frame).unwrap();
    assert_eq!(serde_json::from_slice::<MarketDataFrame>(&bytes).unwrap(), frame);
    // ...and the two lanes are DISTINCT on the wire even though the payload is identical, which
    // is the whole disclosure property.
    assert_ne!(serde_json::to_vec(&MarketDataFrame::Book(snap)).unwrap(), bytes);
}
