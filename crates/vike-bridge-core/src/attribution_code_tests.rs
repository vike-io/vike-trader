use super::*;

fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

#[test]
fn reads_broker_and_builder_code_keys() {
    let v = vars(&[("OKX_BROKER_CODE", "5328c82e5542BCDE"), ("POLYMARKET_BUILDER_CODE", "0xabc")]);
    assert_eq!(attribution_code_from(&v, "okx").as_deref(), Some("5328c82e5542BCDE"));
    assert_eq!(attribution_code_from(&v, "polymarket").as_deref(), Some("0xabc"));
}

#[test]
fn absent_or_invalid_or_unmechanized_is_none() {
    // absent
    assert_eq!(attribution_code_from(&vars(&[]), "okx"), None);
    // present but too long for OKX's 16-char tag → rejected (treated as unset)
    assert_eq!(
        attribution_code_from(&vars(&[("OKX_BROKER_CODE", "way_too_long_broker_tag")]), "okx"),
        None
    );
    // venue with no order-level mechanism → None even if a stray key exists.
    //
    // ⚠ The stray key is COMPOSED, not spelled. Deribit is `AttributionMechanic::None`, so
    // `attribution_code_from` returns before it builds a key and that name is provably never
    // looked up — which is exactly why `vike_model::credential_keys::attribution_keys` leaves
    // it out of the grid. A bare literal here would hand it a `vike_ops::settings::SETTINGS`
    // row anyway (the registry's raw literal sweep has no call syntax to anchor on and cannot
    // tell a fixture from a read), and `vike-cli config show` would then report an unread key
    // as a real setting with a real source — positive confirmation of something false, the
    // defect CLAUDE.md's settings section calls worse than an unimplemented feature. Same
    // idiom, same reason, as `required_passphrase_tests`' `names_for_prefix(&prefix_for(…))`.
    let stray = attribution_key("deribit", BROKER_CODE_SUFFIX);
    assert_eq!(attribution_code_from(&vars(&[(stray.as_str(), "x")]), "deribit"), None);
}
