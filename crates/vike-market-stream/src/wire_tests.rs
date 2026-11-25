//! Unit tests of the stream wire's tables: the stream-word maps, the sub-op lane words and the
//! start-failure round trip. The wire's BYTES are pinned elsewhere (`tests/stream_tag_fixtures.rs`,
//! over the committed `fixtures/stream_wire/` goldens).

use vike_model::StreamLabel;

use super::*;

#[test]
fn the_wire_stream_word_is_total_and_its_own_inverse() {
    for word in ["1m", "15s", "1d", "", "weird word", "ticks", "quotes", "trades", "book", "depth"]
    {
        assert_eq!(StreamWordWire::from_word(word).word(), word, "the word {word:?} must survive");
    }
    for label in StreamLabel::ALL {
        let wire = StreamWordWire::from_word(label.word());
        assert_eq!(wire, StreamWordWire::Lane(label));
    }
    assert_eq!(StreamWordWire::from_word("ticks"), StreamWordWire::Link);
    assert_eq!(StreamWordWire::from_word("1m"), StreamWordWire::Bar("1m".into()));
}

#[test]
fn a_sub_op_names_the_stream_word_its_disclosures_carry() {
    let op = SubscriptionOperation::Bars { symbol: "BTCUSDT".into(), interval: "5m".into() };
    assert_eq!(op.symbol(), "BTCUSDT");
    assert_eq!(op.stream().word(), "5m");
    assert_eq!(
        SubscriptionOperation::Quotes { symbol: "X".into() }.stream().word(),
        StreamLabel::Quotes.word()
    );
    assert_eq!(
        SubscriptionOperation::Trades { symbol: "X".into() }.stream().word(),
        StreamLabel::Trades.word()
    );
    assert_eq!(
        SubscriptionOperation::Book { symbol: "X".into() }.stream().word(),
        StreamLabel::Book.word()
    );
    assert_eq!(
        SubscriptionOperation::Depth { symbol: "X".into() }.stream().word(),
        StreamLabel::Depth.word()
    );
}

#[test]
fn a_start_failure_crosses_the_wire_both_ways_with_its_variant_and_its_text() {
    use vike_data::DataClientError;

    let failures = [
        DataClientError::Subscribe("the OS refused a feed thread".into()),
        DataClientError::Unsupported(
            "venue's declared VenueCapabilities.live_data serves no live trade prints",
        ),
        DataClientError::Unsupported("a bridge's own reason: no live depth feed"),
        DataClientError::NotNow("gatewayhub: a join is still in progress".into()),
        DataClientError::Cap("gatewayhub: already hosting 16 tick pumps".into()),
    ];
    for original in failures {
        let wire = StreamSubscriptionErrorKind::from_live_error(&original);
        let json = serde_json::to_string(&wire).expect("encodes");
        let back: StreamSubscriptionErrorKind = serde_json::from_str(&json).expect("decodes");
        let again = back.into_live_error("gatewayhub");
        assert_eq!(again.to_string(), original.to_string(), "the text is unchanged");
        assert_eq!(std::mem::discriminant(&again), std::mem::discriminant(&original));
    }
}

#[test]
fn a_sub_op_names_its_tick_lane_and_a_bar_lane_has_none() {
    assert_eq!(
        SubscriptionOperation::Quotes { symbol: "X".into() }.lane(),
        Some(StreamLabel::Quotes)
    );
    assert_eq!(
        SubscriptionOperation::Trades { symbol: "X".into() }.lane(),
        Some(StreamLabel::Trades)
    );
    assert_eq!(SubscriptionOperation::Book { symbol: "X".into() }.lane(), Some(StreamLabel::Book));
    assert_eq!(
        SubscriptionOperation::Depth { symbol: "X".into() }.lane(),
        Some(StreamLabel::Depth)
    );
    assert_eq!(
        SubscriptionOperation::Bars { symbol: "X".into(), interval: "1m".into() }.lane(),
        None
    );
}

/// A `Subscribe` without a class is byte-for-byte the frame a peer without the feature reads and writes: the
/// field is absent, not `null`, and an older frame decodes to `None`.
#[test]
fn a_sub_without_a_class_is_the_frame_every_older_peer_speaks() {
    let op = SubscriptionOperation::Depth { symbol: "BTCUSDT".into() };
    let plain = StreamUp::Subscribe { sid: 5, op: op.clone(), class: None };
    let json = serde_json::to_string(&plain).expect("encodes");
    assert!(!json.contains("class"), "{json}");
    assert_eq!(json, r#"{"Subscribe":{"sid":5,"op":{"Depth":{"symbol":"BTCUSDT"}}}}"#);
    let back: StreamUp = serde_json::from_str(&json).expect("decodes");
    assert_eq!(back, plain);

    let raised = StreamUp::Subscribe { sid: 5, op, class: Some(DataClientSinkDelivery::Lossless) };
    let json = serde_json::to_string(&raised).expect("encodes");
    assert!(json.ends_with(r#""class":"Lossless"}}"#), "{json}");
    assert_eq!(serde_json::from_str::<StreamUp>(&json).expect("decodes"), raised);
}

/// **The pump's `Ticks` op names the link word and no tick lane**, its symbol is the symbol, and its
/// frame is the one the golden pins: the grid crosses as a JSON number that reads back bit for bit.
#[test]
fn the_ticks_op_discloses_under_the_link_word_and_has_no_lane_to_raise() {
    let op = SubscriptionOperation::Ticks { symbol: "BTCUSDT.P".into(), tick_size: 0.1 };
    assert_eq!(op.symbol(), "BTCUSDT.P");
    assert_eq!(op.stream(), StreamWordWire::Link);
    assert_eq!(op.stream().word(), "ticks");
    assert_eq!(op.lane(), None, "a pump's frames are the verbs' own classes: there is no floor");

    let sub = StreamUp::Subscribe { sid: 7, op: op.clone(), class: None };
    let json = serde_json::to_string(&sub).expect("encodes");
    assert_eq!(
        json,
        r#"{"Subscribe":{"sid":7,"op":{"Ticks":{"symbol":"BTCUSDT.P","tick_size":0.1}}}}"#
    );
    let back: StreamUp = serde_json::from_str(&json).expect("decodes");
    match back {
        StreamUp::Subscribe { op: SubscriptionOperation::Ticks { tick_size, .. }, .. } => {
            assert_eq!(tick_size.to_bits(), 0.1_f64.to_bits(), "the grid survives bit for bit");
        }
        other => panic!("{other:?}"),
    }
}

/// **One rule for a client-named grid**: finite, strictly positive and at most the ceiling. A
/// non-finite number would be `null` on this JSON wire and read back as a different frame.
#[test]
fn a_tick_size_is_finite_positive_and_under_the_ceiling() {
    for good in [TICK_SIZE_FLOOR, 1.0e-8, 0.001, 0.1, 1.0, 1000.0, TICK_SIZE_CEILING] {
        assert_eq!(validate_tick_size(good), Ok(good), "{good}");
    }
    for bad in [
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        0.0,
        -0.0,
        -0.5,
        TICK_SIZE_CEILING * 2.0,
        f64::MAX,
        5.0e-324,
        f64::MIN_POSITIVE,
        TICK_SIZE_FLOOR * 0.5,
    ] {
        let why = validate_tick_size(bad).expect_err("refused");
        assert!(why.contains("tick size"), "{bad}: {why}");
    }
    assert!(validate_tick_size(TICK_SIZE_CEILING * 1.000_001).is_err(), "just past the ceiling");
}

/// **One spelling rule for both hubs, under the limit the host names**, in the words a refused
/// subscription has always carried.
#[test]
fn a_spelling_is_refused_blank_overlong_or_with_a_control_character() {
    let quotes = |s: &str| SubscriptionOperation::Quotes { symbol: s.to_string() };
    let words = |op: SubscriptionOperation| op.refuse_spelling(8);
    assert_eq!(words(quotes("BTCUSDT")), None);
    assert_eq!(words(quotes("12345678")), None, "the limit itself is accepted");
    assert_eq!(words(quotes("  ")).as_deref(), Some("the symbol is blank"));
    assert_eq!(
        words(quotes("123456789")).as_deref(),
        Some("the symbol is 9 bytes, over the 8-byte limit")
    );
    assert_eq!(words(quotes("A\nB")).as_deref(), Some("the symbol carries a control character"));
    let bars = |interval: &str| SubscriptionOperation::Bars {
        symbol: "BTC".to_string(),
        interval: interval.into(),
    };
    assert_eq!(words(bars("1m")), None);
    assert_eq!(words(bars(" ")).as_deref(), Some("the interval is blank"));
    assert_eq!(
        quotes("123456789").refuse_spelling(9),
        None,
        "the limit is the host's, not this crate's"
    );
}
