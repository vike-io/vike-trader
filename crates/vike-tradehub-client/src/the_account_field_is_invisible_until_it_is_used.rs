use super::*;

/// **THE forward-compatibility property, measured rather than asserted.**
///
/// A client that names no account must put a frame on the wire that is BYTE-IDENTICAL to the
/// one it sent before the field existed. Everything else rests on this: the existing round-trip
/// fixtures, an old node's decode, and the claim in `FEATURE_ACCOUNT_ROUTING`'s doc that
/// absence is indistinguishable from a dropped field.
///
/// The assertion is on the SERIALIZED TEXT, not on a round-trip — a round-trip would pass just
/// as happily with `"account":null` in the JSON, which is exactly the byte an old node would
/// choke on.
#[test]
fn a_command_naming_no_account_serialises_without_the_key() {
    let cmds = [
        WireCommand::MassCancel { venue: None, symbol: None, account: None },
        WireCommand::Flatten { venue: "binance".into(), symbol: "BTCUSDT".into(), account: None },
        WireCommand::MarketExit { venue: Some("okx".into()), account: None },
    ];
    for c in &cmds {
        let js = serde_json::to_string(c).expect("a command serialises");
        assert!(
            !js.contains("account"),
            "a command naming no account must not mention the key at all: {js}"
        );
    }
}

/// ...and the order request half, which is where a Submit carries it.
#[test]
fn an_order_naming_no_account_serialises_without_the_key() {
    let req = WireOrderRequest {
        client_order_id: "c-1".into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(1.0),
        trigger_price: None,
        reduce_only: false,
        account: None,
    };
    let js = serde_json::to_string(&req).expect("an order serialises");
    assert!(!js.contains("account"), "{js}");
}

/// The complement, and it is what makes the test above mean something: when a client DOES name
/// an account the key is on the wire, with the label as written.
#[test]
fn a_named_account_reaches_the_wire_verbatim() {
    let c = WireCommand::Flatten {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        account: Some("ALT".into()),
    };
    let js = serde_json::to_string(&c).expect("a command serialises");
    assert!(js.contains("\"account\":\"ALT\""), "{js}");
}

/// ⚠ **An OLD node's frame still decodes** — the other direction of the same property. A frame
/// with no `account` key is what every client sends today, and `#[serde(default)]` is what
/// keeps it decodable once the field exists.
#[test]
fn a_pre_field_frame_still_decodes_and_names_no_account() {
    let old = r#"{"Flatten":{"venue":"binance","symbol":"BTCUSDT"}}"#;
    let c: WireCommand = serde_json::from_str(old).expect("a pre-field frame must decode");
    match c {
        WireCommand::Flatten { account, .. } => {
            assert!(account.is_none(), "a dropped field reads as NAMED NOTHING")
        }
        other => panic!("decoded as the wrong variant: {other:?}"),
    }
}
