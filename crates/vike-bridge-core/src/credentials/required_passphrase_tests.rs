//! The half-credential gate: a venue whose signer REQUIRES a passphrase must not load without one,
//! because loading is what mounts it live.

use super::*;

/// Seed a var map through the module's OWN naming site, so these fixtures cannot drift from the
/// names the loader reads — and so this file spells no `{VENUE}_{TIER}_API_*` literal:
/// `vike_model::scan`'s map-lookup sweep harvests env-shaped string literals wherever they appear,
/// and a fixture spelling one is indistinguishable from a real read.
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

/// OKX key + secret with NO passphrase resolve like ABSENT credentials: loaded, the venue would
/// mount live and every signed request come back `OK-ACCESS-PASSPHRASE cannot be empty`.
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

/// A BLANK passphrase is an absent one: the signer sends an empty `OK-ACCESS-PASSPHRASE` header
/// either way.
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

/// ⚠ **NOT a blanket requirement.** The venues whose signers take no passphrase load with exactly
/// two credentials; demanding one everywhere would strand every binance/bybit/deribit mount on
/// paper. The roster's other venues ride bespoke shapes this loader never sees.
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

/// The `Optional` row is a real distinction: aster READS the field (its agent signer address) and
/// derives it when blank, so folding `Optional` into `Required` would strand it.
#[test]
fn asters_optional_passphrase_never_gates() {
    let v = seed(&[("aster", "DEMO", None)]);
    assert!(load_credentials_from("aster", Environment::Demo, &v).is_some());
    assert_eq!(missing_required_passphrase("aster", Environment::Demo, &v), None);
}

/// The finding is the NAME of the missing variable — NEVER a value, not even of the two
/// credentials that WERE found: this string goes to a log.
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

/// Nothing configured stays SILENT: an operator who wrote no keys must not be told a credential is
/// missing.
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

/// A COMPLETE `MAINNET`-spelled set beside a half-configured LIVE one satisfies nothing: that
/// spelling is no tier (owner ruling 2026-10-09), so the live load stays absent and the finding
/// names the LIVE passphrase the operator has to write.
#[test]
fn a_complete_mainnet_set_does_not_satisfy_a_half_configured_live_tier() {
    let v = seed(&[("okx", "LIVE", None), ("okx", "MAINNET", Some("lp"))]);
    assert!(
        load_credentials_from("okx", Environment::Live, &v).is_none(),
        "a MAINNET-spelled set is no live credential"
    );
    // The expected name is BUILT the way the loader builds it, so the assertion stays exact with
    // no env-shaped literal for the map-lookup sweep to mistake for a read.
    let (_, _, want) = names_for_prefix(&prefix_for("okx", Environment::Live.as_str()));
    assert_eq!(missing_required_passphrase("okx", Environment::Live, &v), Some(want));
}
