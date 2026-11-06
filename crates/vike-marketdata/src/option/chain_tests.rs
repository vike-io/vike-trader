use super::*;

fn chain() -> OptionChain {
    let mut call = OptionQuote::new(60_000.0, OptionKind::Call);
    call.bid = Some(0.05);
    call.iv = Some(0.62);
    call.in_the_money = Some(false);
    call.instrument_name = Some("BTC-27JUL26-60000-C".into());
    OptionChain {
        underlying: "BTC".into(),
        underlying_kind: UnderlyingKind::Crypto,
        underlying_price: Some(59_000.0),
        expiry: Expiry { date: "2026-07-27".into(), dte: 3, label: "27 Jul".into() },
        asof_ms: 1_700_000_000_000,
        source: "deribit".into(),
        rows: vec![StrikeRow {
            strike: 60_000.0,
            call: Some(call),
            put: Some(OptionQuote::new(60_000.0, OptionKind::Put)),
        }],
    }
}

#[test]
fn a_chain_survives_a_json_round_trip() {
    let chain = chain();
    let text = serde_json::to_string(&chain).unwrap();
    assert_eq!(serde_json::from_str::<OptionChain>(&text).unwrap(), chain);
}

#[test]
fn new_leaves_every_optional_field_absent() {
    let q = OptionQuote::new(100.0, OptionKind::Put);
    assert_eq!((q.strike, q.kind), (100.0, OptionKind::Put));
    assert!(q.bid.is_none() && q.ask.is_none() && q.mark.is_none() && q.iv.is_none());
    assert!(q.in_the_money.is_none() && q.instrument_name.is_none());
}

#[test]
fn a_quote_without_its_optional_fields_still_deserialises() {
    let q: OptionQuote = serde_json::from_str(r#"{"strike":5.0,"kind":"call"}"#).unwrap();
    assert_eq!(q, OptionQuote::new(5.0, OptionKind::Call));

    let row: StrikeRow = serde_json::from_str(r#"{"strike":5.0}"#).unwrap();
    assert_eq!((row.call, row.put), (None, None));
}

#[test]
fn the_underlying_kind_is_a_snake_case_word_on_the_wire() {
    assert_eq!(serde_json::to_string(&UnderlyingKind::Equity).unwrap(), r#""equity""#);
    assert_eq!(serde_json::to_string(&OptionKind::Put).unwrap(), r#""put""#);
}

#[test]
fn an_underlying_bundle_survives_a_json_round_trip() {
    let front = chain();
    let mut chains = std::collections::BTreeMap::new();
    chains.insert(front.expiry.date.clone(), front.clone());
    let bundle = UnderlyingChains {
        default_expiry: front.expiry.date.clone(),
        expiries: vec![front.expiry.clone()],
        chains,
    };
    let text = serde_json::to_string(&bundle).unwrap();
    assert_eq!(serde_json::from_str::<UnderlyingChains>(&text).unwrap(), bundle);
    assert_eq!(UnderlyingChains::default().chains.len(), 0, "Default is the empty bundle");
}
