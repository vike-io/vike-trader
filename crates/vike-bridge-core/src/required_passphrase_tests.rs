use super::*;

/// Seed a var map through the module's OWN naming site, so these fixtures can never drift
/// from the names the loader reads. (It also keeps `{VENUE}_{TIER}_API_*` literals out of this
/// file: `vike_ops::scan`'s map-lookup sweep harvests env-shaped string literals wherever they
/// appear, and a fixture spelling one is indistinguishable from a real read.)
fn seed(pairs: &[(&str, &str, Option<&str>)]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for (venue, tier, pass) in pairs {
        let (k, s, p) = names_for_prefix(&prefix_for(venue, tier));
        out.insert(k, "k".to_string());
        out.insert(s, "s".to_string());
        if let Some(pass) = pass {
            out.insert(p, (*pass).to_string());
        }
    }
    out
}

/// THE DEFECT, pinned: key + secret and NO passphrase must resolve like ABSENT credentials on
/// OKX. Before this gate the same map returned `Some`, mounted live, and every signed request
/// came back `OK-ACCESS-PASSPHRASE cannot be empty`.
#[test]
fn okx_key_and_secret_without_a_passphrase_do_not_load() {
    let half = seed(&[("okx", "DEMO", None)]);
    assert!(
        load_credentials_from("okx", Environment::Demo, &half).is_none(),
        "okx key+secret with no passphrase must resolve like absent credentials (stay PAPER)"
    );
    // …and the SAME map plus the passphrase loads, so the gate is the passphrase and nothing
    // else (a gate that refused both ways would be indistinguishable from a broken loader).
    let full = seed(&[("okx", "DEMO", Some("p"))]);
    let c = load_credentials_from("okx", Environment::Demo, &full)
        .expect("all three credentials present ⇒ loads");
    assert_eq!(c.api_key, "k");
    assert_eq!(c.passphrase.as_deref(), Some("p"));
}

/// A BLANK passphrase is the same as an absent one. The wire cannot tell the difference — the
/// signer sends an empty `OK-ACCESS-PASSPHRASE` header either way.
#[test]
fn a_blank_okx_passphrase_is_an_absent_one() {
    for blank in ["", "   ", "\t"] {
        let v = seed(&[("okx", "DEMO", Some(blank))]);
        assert!(
            load_credentials_from("okx", Environment::Demo, &v).is_none(),
            "a blank passphrase ({blank:?}) must not count as configured"
        );
    }
}

/// ⚠ **NOT a blanket requirement.** The venues whose signers take no passphrase must load with
/// exactly two credentials, byte-identically to before the gate existed — demanding one
/// everywhere would strand every working binance/bybit/deribit mount on paper. The roster's
/// remaining venues ride bespoke shapes this loader never sees, so this covers every venue for
/// which the generic path is the live gate.
#[test]
fn passphrase_free_venues_still_load_with_key_and_secret_alone() {
    for venue in ["binance", "bybit", "deribit"] {
        let v = seed(&[(venue, "DEMO", None)]);
        let c = load_credentials_from(venue, Environment::Demo, &v)
            .unwrap_or_else(|| panic!("{venue} must load with key+secret alone"));
        assert!(c.passphrase.is_none());
        assert_eq!(missing_required_passphrase(venue, Environment::Demo, &v), None);
    }
}

/// The `Optional` row is a real distinction, not a spelling: aster READS the field (as its
/// agent signer address) and derives it when blank, so folding `Optional` into `Required`
/// would strand it. Its own loader owns its var shape; this asserts the generic path's row.
#[test]
fn asters_optional_passphrase_never_gates() {
    let v = seed(&[("aster", "DEMO", None)]);
    assert!(load_credentials_from("aster", Environment::Demo, &v).is_some());
    assert_eq!(missing_required_passphrase("aster", Environment::Demo, &v), None);
}

/// The finding is the NAME of the missing variable — and NEVER a value. The two credentials
/// that WERE found must not appear in it either: this string goes to a log.
#[test]
fn the_finding_names_the_variable_and_echoes_no_value() {
    let mut v = seed(&[("okx", "DEMO", None)]);
    for val in v.values_mut() {
        *val = "secret-material-Zx81".to_string();
    }
    let name = missing_required_passphrase("okx", Environment::Demo, &v)
        .expect("a half-configured okx store reports its missing credential");
    assert_eq!(name, "OKX_DEMO_API_PASSPHRASE");
    assert!(!name.contains("Zx81"), "no credential VALUE may reach the finding: {name}");
}

/// Nothing configured stays SILENT — the ordinary unconfigured state is not a finding, and an
/// operator who wrote no keys must not be told a credential is missing.
#[test]
fn an_unconfigured_venue_reports_nothing() {
    assert_eq!(missing_required_passphrase("okx", Environment::Demo, &HashMap::new()), None);
    // key alone, then secret alone — still nothing to report
    let full = seed(&[("okx", "DEMO", Some("p"))]);
    for name in full.keys() {
        let one: HashMap<String, String> = HashMap::from([(name.clone(), "v".to_string())]);
        assert_eq!(
            missing_required_passphrase("okx", Environment::Demo, &one),
            None,
            "one credential alone is UNCONFIGURED, not half-configured ({name})"
        );
    }
}

/// The legacy `MAINNET` tier is a real fallback, not a formality: a `Live` load whose LIVE set
/// is half-configured but whose MAINNET set is COMPLETE still loads — and reports nothing,
/// because nothing is wrong.
#[test]
fn a_complete_legacy_tier_satisfies_a_half_configured_live_tier() {
    let v = seed(&[("okx", "LIVE", None), ("okx", "MAINNET", Some("lp"))]);
    assert!(
        load_credentials_from("okx", Environment::Live, &v).is_some(),
        "the complete legacy tier still loads"
    );
    assert_eq!(missing_required_passphrase("okx", Environment::Live, &v), None);
    // …but with the legacy tier ALSO missing its passphrase, nothing loads and the finding
    // names the tier `load_credentials_from` tried FIRST.
    let v = seed(&[("okx", "LIVE", None), ("okx", "MAINNET", None)]);
    assert!(load_credentials_from("okx", Environment::Live, &v).is_none());
    // The expected name is BUILT the way the loader builds it — `names_for_prefix` over the
    // primary `{VENUE}_{TIER}` prefix — so the assertion is still exact (it pins the LIVE tier
    // over the MAINNET one) with no env-shaped literal for `vike_ops::scan`'s map-lookup sweep
    // to mistake for a real read.
    let (_, _, want) = names_for_prefix(&prefix_for("okx", Environment::Live.as_str()));
    assert_eq!(missing_required_passphrase("okx", Environment::Live, &v), Some(want));
}
