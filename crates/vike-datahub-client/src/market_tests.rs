use super::*;
use vike_model::BookLevel;

/// EXHAUSTIVE over [`vike_data::StreamStatus`], and the no-`_` match inside is the load-bearing
/// half: a round-trip over a hand-listed set proves nothing about a variant nobody added to the
/// list, so a NEW `StreamStatus` variant must fail to COMPILE here rather than silently never
/// crossing the wire.
#[test]
fn every_stream_status_round_trips() {
    let all = [
        vike_data::StreamStatus::GapStart { at_ts_ms: 1_700_000_000_000 },
        vike_data::StreamStatus::Live { gap_started_ts_ms: None },
        vike_data::StreamStatus::Live { gap_started_ts_ms: Some(1_700_000_000_000) },
        vike_data::StreamStatus::Stale {
            newest_data_ts_ms: 1_700_000_000_000,
            now_ms: 1_700_000_060_000,
        },
    ];
    for s in all {
        // The compile-time completeness guard. NO `_` arm.
        match s {
            vike_data::StreamStatus::GapStart { .. } => (),
            vike_data::StreamStatus::Live { .. } => (),
            vike_data::StreamStatus::Stale { .. } => (),
        }
        let wire = WireStreamStatus::from(s);
        assert_eq!(vike_data::StreamStatus::from(wire), s, "{s:?} did not survive the mirror");
        // ...and through the actual codec, since the mirror only earns its keep if it rides.
        let bytes = serde_json::to_vec(&wire).expect("serialize");
        let back: WireStreamStatus = serde_json::from_slice(&bytes).expect("deserialize");
        assert_eq!(back, wire);
    }
}

/// THIS TEST IS THE ARGUMENT FOR THE MANUAL SERDE IMPL (see `MdSessionId`'s `Serialize` doc): a
/// derived `u128` fails `serde_json::to_value` above `u64::MAX` and reaches `jq` as an `f64`.
#[test]
fn a_session_id_rides_as_hex_not_a_number() {
    let id = MdSessionId(u128::MAX);
    let v = serde_json::to_value(id).expect("to_value must not choke — this is the whole point");
    assert!(v.is_string(), "the session id must ride as a STRING, got {v:?}");
    assert_eq!(v.as_str().unwrap(), "ffffffffffffffffffffffffffffffff");
    assert_eq!(v.as_str().unwrap().len(), 32, "always 32 hex chars, zero-padded");
    let back: MdSessionId = serde_json::from_value(v).expect("round trip");
    assert_eq!(back, id);

    // ...and a SMALL id is padded rather than printed short, so an id is one width everywhere.
    assert_eq!(MdSessionId(1).to_string(), "00000000000000000000000000000001");
    assert_eq!(
        serde_json::from_str::<MdSessionId>("\"00000000000000000000000000000001\"").unwrap(),
        MdSessionId(1)
    );
    // A fresh id is not the zero one — the mint is wired to a generator, not to a default.
    assert_ne!(MdSessionId::fresh(), MdSessionId(0));
}

/// Depth is NOT part of a key, and two specs differing only in depth name one subscription.
#[test]
fn depth_is_not_part_of_a_subscription_key() {
    let a = MdSpec {
        venue: "binance".into(),
        symbol: "BTCUSDT.P".into(),
        lane: MdLane::Depth,
        depth_levels: Some(200),
    };
    let b = MdSpec { depth_levels: None, ..a.clone() };
    assert_eq!(a.key(), b.key());
    assert_eq!(a.resolved_depth(), MD_DEPTH_LEVELS_CEILING);
    assert_eq!(b.resolved_depth(), MD_DEPTH_LEVELS_DEFAULT);
    // ...and a request ABOVE the ceiling is clamped rather than refused.
    let deep = MdSpec { depth_levels: Some(5_000), ..a.clone() };
    assert_eq!(deep.resolved_depth(), MD_DEPTH_LEVELS_CEILING);
    // A zero is a floor of one, not an empty ladder.
    let zero = MdSpec { depth_levels: Some(0), ..a };
    assert_eq!(zero.resolved_depth(), 1);
}

/// A spec with no depth request carries NO `depth_levels` key on the wire (the `Welcome.nonce`
/// attribute pair), and one with a depth carries it.
#[test]
fn an_absent_depth_is_absent_from_the_bytes() {
    let bare = MdSpec {
        venue: "polymarket".into(),
        symbol: "123".into(),
        lane: MdLane::Trades,
        depth_levels: None,
    };
    let s = serde_json::to_string(&bare).unwrap();
    assert!(!s.contains("depth_levels"), "an absent depth must not ride as null: {s}");
    let deep = MdSpec { depth_levels: Some(25), ..bare.clone() };
    assert!(serde_json::to_string(&deep).unwrap().contains("depth_levels"));
    assert_eq!(serde_json::from_str::<MdSpec>(&s).unwrap(), bare);
}

/// EXHAUSTIVE over [`MdRefusal`]: every variant is classified, and both classes are non-empty —
/// a classifier that answered one way for everything would satisfy neither assertion.
#[test]
fn a_refusal_classifies_permanently_or_not() {
    let all = [
        MdRefusal::UnknownVenue,
        MdRefusal::VenueNotServed("binance, polymarket".into()),
        MdRefusal::LaneUnsupported("venue's declared VenueCaps.live_data serves no…".into()),
        MdRefusal::SymbolRejected("a subscription symbol of 10000 bytes exceeds…".into()),
        MdRefusal::KeyCapTotal { held: 64, cap: 64 },
        MdRefusal::KeyCapVenue { venue: "binance".into(), held: 16, cap: 16 },
        MdRefusal::SpecCapSession { held: 64, cap: 64 },
    ];
    let permanent = all.iter().filter(|r| r.is_permanent()).count();
    assert_eq!(permanent, 4, "the three CAPABILITY refusals plus the VALIDITY one are permanent");
    assert_eq!(all.len() - permanent, 3, "...and the three CAPS are not");
    for r in &all {
        // The compile-time completeness guard, mirroring `is_permanent`'s own match.
        match r {
            MdRefusal::UnknownVenue
            | MdRefusal::VenueNotServed(_)
            | MdRefusal::LaneUnsupported(_)
            | MdRefusal::SymbolRejected(_) => assert!(r.is_permanent()),
            MdRefusal::KeyCapTotal { .. }
            | MdRefusal::KeyCapVenue { .. }
            | MdRefusal::SpecCapSession { .. } => assert!(!r.is_permanent()),
        }
        // ...and each one rides the codec.
        let bytes = serde_json::to_vec(r).unwrap();
        assert_eq!(&serde_json::from_slice::<MdRefusal>(&bytes).unwrap(), r);
    }
}

/// [`validate_md_symbol`] at its BOUNDARIES, both directions — the shape a length rule goes
/// wrong in is admitting one byte too many or refusing one byte too few, and neither is visible
/// from a test that only tries an absurd value.
#[test]
fn the_symbol_validator_is_exact_at_both_edges() {
    // The FLOOR that must be admitted: a polymarket CLOB token id is a uint256 in decimal and
    // 2^256-1 is exactly 78 digits, so this is the longest symbol this wire can ever carry.
    assert!(validate_md_symbol(&"7".repeat(78)).is_ok());
    assert!(validate_md_symbol(&"A".repeat(MD_MAX_SYMBOL_BYTES)).is_ok(), "at the bound");
    let over = validate_md_symbol(&"A".repeat(MD_MAX_SYMBOL_BYTES + 1))
        .expect_err("one byte over the bound");
    assert!(over.contains(&(MD_MAX_SYMBOL_BYTES + 1).to_string()), "the length: {over}");
    assert!(over.contains(&MD_MAX_SYMBOL_BYTES.to_string()), "...and the cap: {over}");

    // BLANK, in all three spellings an argument arrives in.
    for blank in ["", " ", "\t\n "] {
        let why = validate_md_symbol(blank).expect_err("blank {blank:?}");
        assert!(why.contains("BLANK"), "{why}");
    }

    // The CONTROL-BYTE rule, which is part of the bound rather than beside it, and its
    // NARROWNESS — the three characters most likely to be swept up with it stay legal, because
    // a venue this plane does not yet serve spells real instruments with them (an IBKR OSI
    // local symbol carries padding spaces).
    assert!(validate_md_symbol("BTC\u{1}USDT").is_err());
    assert!(validate_md_symbol("BTC\u{7f}USDT").is_err(), "DEL is an ASCII control byte");
    assert!(validate_md_symbol("BTC USDT").is_ok(), "a space is not a control byte");
    assert!(validate_md_symbol("BTC\"USDT").is_ok(), "the quote is escaped, not refused");
    assert!(validate_md_symbol("BTC\\USDT").is_ok(), "...nor the backslash");
    assert!(validate_md_symbol("BTC₿").is_ok(), "non-ASCII rides raw through serde_json");

    // ⚠ The bound is in BYTES, not characters, because bytes are what the frame budget is
    // spent in. A multi-byte symbol therefore admits FEWER characters, and that is correct.
    let three_byte = "₿".repeat(MD_MAX_SYMBOL_BYTES / 3);
    assert_eq!(three_byte.len(), MD_MAX_SYMBOL_BYTES, "the fixture must sit ON the bound");
    assert!(validate_md_symbol(&three_byte).is_ok());
    assert!(validate_md_symbol(&format!("{three_byte}a")).is_err(), "one BYTE over");
}

/// The lane ↔ feed-label mapping is a round trip, and it names the labels the two INDEPENDENT
/// producers actually pass (`crates/bridges/binance/src/family/market_feed.rs`'s literal
/// `"depth"`, polymarket's `PumpMode::as_str`). A silent mismatch is a whole lane of gap
/// disclosure that never reaches a client.
#[test]
fn the_feed_stream_labels_round_trip_and_are_the_producers_own() {
    for lane in [MdLane::Depth, MdLane::Book, MdLane::Trades] {
        assert_eq!(MdLane::from_feed_stream_label(lane.feed_stream_label()), Some(lane));
    }
    assert_eq!(MdLane::Depth.feed_stream_label(), "depth");
    assert_eq!(MdLane::Book.feed_stream_label(), "book");
    assert_eq!(MdLane::Trades.feed_stream_label(), "trades");
    // A label this wire serves no lane for is `None`, never a wrong lane: a bar interval and the
    // quotes lane both arrive here and must be dropped rather than misfiled.
    assert_eq!(MdLane::from_feed_stream_label("1m"), None);
    assert_eq!(MdLane::from_feed_stream_label("quotes"), None);
}

/// The lane → `LiveVerb` mapping, which is what makes §7.5's gate a re-read of the declared
/// capability matrix. The PARTITION it produces on the initial venue set is asserted here rather
/// than described: five CEX serve Depth and refuse Book, polymarket the reverse.
#[test]
fn the_lane_verbs_partition_the_initial_venue_set() {
    assert_eq!(MdLane::Depth.live_verb(), vike_model::LiveVerb::Depth);
    assert_eq!(MdLane::Book.live_verb(), vike_model::LiveVerb::Book);
    assert_eq!(MdLane::Trades.live_verb(), vike_model::LiveVerb::Trades);

    for v in ["binance", "bybit", "okx", "aster", "hyperliquid"] {
        assert!(
            vike_data::require_live_verb(v, MdLane::Depth.live_verb()).is_ok(),
            "{v} must serve the Depth lane"
        );
        assert!(
            vike_data::require_live_verb(v, MdLane::Book.live_verb()).is_err(),
            "{v} declares no lossless book lane — the wire must refuse it"
        );
    }
    assert!(vike_data::require_live_verb("polymarket", MdLane::Book.live_verb()).is_ok());
    assert!(vike_data::require_live_verb("polymarket", MdLane::Depth.live_verb()).is_err());
}

/// A whole frame rides the codec — the round trip over the type the wire actually carries,
/// including the level ORDER contract this type's doc states.
#[test]
fn a_book_frame_round_trips_best_first() {
    let snap = BookSnapshot {
        venue: "binance".into(),
        symbol: "BTCUSDT.P".into(),
        tick_size: 0.1,
        bids: vec![BookLevel::new(100.2, 3.0), BookLevel::new(100.1, 5.0)],
        asks: vec![BookLevel::new(100.3, 2.0), BookLevel::new(100.4, 7.0)],
        venue_ts: 1_700_000_000_000,
        venue_seq: 42,
        seq: 7,
    };
    // Best-first: bids DESCEND, asks ASCEND. `L2Book::top_n`'s own order.
    assert!(snap.bids[0].price > snap.bids[1].price);
    assert!(snap.asks[0].price < snap.asks[1].price);
    let frame = MdFrame::Depth(snap.clone());
    let bytes = serde_json::to_vec(&frame).unwrap();
    assert_eq!(serde_json::from_slice::<MdFrame>(&bytes).unwrap(), frame);
    // ...and the two lanes are DISTINCT on the wire even though the payload is identical, which
    // is the whole disclosure property.
    assert_ne!(serde_json::to_vec(&MdFrame::Book(snap)).unwrap(), bytes);
}
