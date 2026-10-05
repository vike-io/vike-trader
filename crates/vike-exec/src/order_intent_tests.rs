use super::*;
use vike_model::OrderRequest;

#[test]
fn order_intent_serde_roundtrips_each_variant() {
    let req = || {
        Box::new(OrderRequest {
            client_order_id: "c".into(),
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            order_type: "market".into(),
            ..Default::default()
        })
    };
    let variants = vec![
        OrderIntent::Submit(req()),
        OrderIntent::Cancel("c".into()),
        OrderIntent::Modify { client_order_id: "c".into(), new_qty: Some(2.0), new_price: None },
        OrderIntent::Confirm("c".into()),
        OrderIntent::MassCancel { venue: None, symbol: None, account: None },
        OrderIntent::Flatten { venue: "sim".into(), symbol: "BTCUSDT".into(), account: None },
        // The three reducing verbs NAMING an account — a label, and the DEFAULT account,
        // whose representation is the one `AccountLabel`'s own `Serialize` refuses.
        OrderIntent::MassCancel {
            venue: Some("sim".into()),
            symbol: None,
            account: Some(vike_model::accounts::account_keys::AccountLabel::Named("ALT".into())),
        },
        OrderIntent::Flatten {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            account: Some(vike_model::accounts::account_keys::AccountLabel::Default),
        },
        OrderIntent::MarketExit { venue: None, account: None },
        OrderIntent::MarketExit {
            venue: Some("sim".into()),
            account: Some(vike_model::accounts::account_keys::AccountLabel::Default),
        },
        OrderIntent::ArmConditional(ConditionalIntent {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: -1,
            qty: 1.0,
            price: Some(90.0),
            trail: None,
            trigger_by: None,
        }),
        OrderIntent::DisarmConditional { arm_id: "cafef00da0".into() },
        OrderIntent::SubmitBatch(vec![*req()]),
        OrderIntent::CancelBatch(vec!["c".into()]),
        OrderIntent::Bracket(Box::new(vike_model::BracketSpec {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 2.0,
            entry_price: Some(100.0),
            stop_loss: 95.0,
            take_profit: 110.0,
        })),
        OrderIntent::Combo(Box::new(vike_model::ComboSpec {
            venue: "deribit".into(),
            side: 1,
            qty: 2.0,
            legs: vec![
                vike_model::ComboLeg { symbol: "BTC-27MAR26-100000-C".into(), ratio: 1 },
                vike_model::ComboLeg { symbol: "BTC-27MAR26-120000-C".into(), ratio: -1 },
            ],
            // a CREDIT combo: the signed net limit is NEGATIVE and must survive the trip
            net_limit: Some(-0.0125),
            time_in_force: vike_model::TimeInForce::Gtc,
        })),
    ];
    for v in variants {
        let js = serde_json::to_string(&v).unwrap();
        let back: OrderIntent = serde_json::from_str(&js).unwrap();
        assert_eq!(format!("{v:?}"), format!("{back:?}"));
    }
}

/// ⚠ **An account-LESS reducing intent serialises to EXACTLY the bytes it had before the field
/// existed, and those bytes still read back.** This intent is journaled inside every
/// `Ingest::Command` write-ahead record, so the two directions are two different promises: a
/// journal a NEW binary writes must not move (the bit-parity fixtures and every golden suite
/// hash it), and one an OLD binary wrote must still replay. The literals below are the
/// pre-field spelling, written out rather than derived, because deriving them from today's
/// type would compare the serializer with itself.
#[test]
fn an_account_less_reducing_intent_is_byte_identical_to_the_pre_field_journal() {
    let pinned = [
        (
            OrderIntent::MassCancel { venue: None, symbol: None, account: None },
            r#"{"MassCancel":{"venue":null,"symbol":null}}"#,
        ),
        (
            OrderIntent::Flatten { venue: "sim".into(), symbol: "BTCUSDT".into(), account: None },
            r#"{"Flatten":{"venue":"sim","symbol":"BTCUSDT"}}"#,
        ),
        (
            OrderIntent::MarketExit { venue: Some("sim".into()), account: None },
            r#"{"MarketExit":{"venue":"sim"}}"#,
        ),
    ];
    for (intent, bytes) in pinned {
        assert_eq!(serde_json::to_string(&intent).unwrap(), bytes, "a new journal moved");
        let old: OrderIntent = serde_json::from_str(bytes).unwrap();
        assert_eq!(format!("{old:?}"), format!("{intent:?}"), "an old journal misreads");
    }
}

/// …and a NAMED account is carried in the representation `vike_model::OrderRequest::account`
/// uses, `DEFAULT` included — the value `AccountLabel`'s own `Serialize` REFUSES. Serialising
/// that refusal is what used to panic the fold thread's journal writer for a submit
/// (`wire_account_option`'s module doc carries the incident), so it is asserted as the literal
/// string rather than trusted to the round-trip above.
#[test]
fn a_reducing_intent_naming_the_default_account_journals_as_the_wire_spelling() {
    let intent = OrderIntent::MarketExit {
        venue: Some("binance".into()),
        account: Some(vike_model::accounts::account_keys::AccountLabel::Default),
    };
    let js = serde_json::to_string(&intent).expect("DEFAULT must be representable");
    assert_eq!(js, r#"{"MarketExit":{"venue":"binance","account":"DEFAULT"}}"#);
    let back: OrderIntent = serde_json::from_str(&js).unwrap();
    assert_eq!(format!("{back:?}"), format!("{intent:?}"));
}

#[test]
fn command_order_wraps_intent() {
    let c = Command::Order(OrderIntent::Cancel("c".into()));
    let js = serde_json::to_string(&c).unwrap();
    assert!(js.contains("Order"), "externally-tagged: {js}");
    let back: Command = serde_json::from_str(&js).unwrap();
    assert_eq!(format!("{c:?}"), format!("{back:?}"));
}
