use super::*;
use vike_bridge_core::venue_mount_fixture::MountFixture;
use vike_model::account_keys::{AccountLabel, account_key};

use crate::config::alpaca_tier;

/// The minimal SANDBOX key set `load_alpaca_config_for_account` requires.
const KEYS: &[(&str, &str)] = &[
    ("ALPACA_SANDBOX_CLIENT_ID", "cid"),
    ("ALPACA_SANDBOX_CLIENT_SECRET", "csecret"),
    ("ALPACA_SANDBOX_ACCOUNT_ID", "acct-1"),
];

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}

/// A fixture holding `KEYS` under `label`'s key names, asking for `label`.
fn keyed(label: &AccountLabel) -> MountFixture {
    let mut fx = MountFixture::new(&[]);
    for (k, v) in KEYS {
        fx.vars.insert(account_key(k, label), (*v).to_string());
    }
    fx.account = label.clone();
    fx
}

/// A complete LIVE-tier key set for the default account, its names composed the way the loader
/// composes them (`alpaca_tier`) — so no bare LIVE-tier credential literal enters the source,
/// where `crates/vike-ops/tests/settings_registry.rs` would demand a registry row nothing reads.
fn live_tier_keys() -> Vec<(String, String)> {
    let prefix = format!("ALPACA_{}", alpaca_tier(Environment::Live));
    ["CLIENT_ID", "CLIENT_SECRET", "ACCOUNT_ID"]
        .iter()
        .map(|suffix| (format!("{prefix}_{suffix}"), "x".to_string()))
        .collect()
}

/// The rows `vike-mount`'s tables carried for alpaca, pinned as the capability-map playbook pins
/// a matrix: a change to any of them is a deliberate edit here too.
#[test]
fn the_declaration_is_the_rows_the_mount_crate_carried() {
    let d = AlpacaVenueMount.declaration();
    assert_eq!(AlpacaVenueMount.venue(), "alpaca");
    assert!(d.addresses_accounts);
    assert!(d.process_exclusive.is_none());
    assert!(!d.takes_recon_trigger);
    assert_eq!(d.grid_source, DeclaredGridSource::PerSymbolFetch);
    assert_eq!(
        d.book_identity,
        BookIdentity::Named {
            prefix: "ALPACA",
            demo_tiers: &["SANDBOX"],
            live_tiers: &["LIVE"],
            name_suffixes: &["ACCOUNT_ID"],
            evm_key_suffixes: &[],
        }
    );
    // The reason is the text an operator reads in the preflight's clock row, so it is pinned
    // whole rather than by shape.
    assert_eq!(
        d.clock,
        ClockDecl::NotWired {
            reason: "its /v1/clock needs the OAuth2 client-credentials Bearer, i.e. a second token \
                     exchange before any measurement, and alpaca stamps no timestamp on a request",
            unmeasured_risk: None,
        }
    );
}

/// The arming-probe row as it was: a complete SANDBOX key set arms a demo-only arm; anything
/// less stays paper.
#[test]
fn resolve_is_the_arms_own_gate() {
    let armed =
        Resolution::Armed { tier: Tier::Demo, held_below_live: Some(HeldBelowLive::DemoOnlyArm) };
    for live in [false, true] {
        assert_eq!(
            AlpacaVenueMount.resolve(&MountFixture::new(&[]).inputs(live)),
            Resolution::Paper(PaperCause::NoCredentials)
        );
        assert_eq!(AlpacaVenueMount.resolve(&keyed(&AccountLabel::Default).inputs(live)), armed);
        assert_eq!(
            AlpacaVenueMount.resolve(&MountFixture::new(&KEYS[..2]).inputs(live)),
            Resolution::Paper(PaperCause::NoCredentials),
            "half a key set is no key set"
        );
    }
}

/// Review Focus 2 at this venue: no ceiling makes this arm resolve `Live` — it has no live arm.
/// Asserted at BOTH values of `live_permitted`, with a complete LIVE-tier key set present: beside
/// the SANDBOX set it leaves the sandbox arming exactly as it is (demo, held by `DemoOnlyArm`), and
/// on its own it arms nothing at all.
#[test]
fn a_ceiling_below_live_never_resolves_live() {
    let mut both_tiers = keyed(&AccountLabel::Default);
    let mut live_only = MountFixture::new(&[]);
    for (k, v) in live_tier_keys() {
        both_tiers.vars.insert(k.clone(), v.clone());
        live_only.vars.insert(k, v);
    }
    for live in [false, true] {
        assert_eq!(
            AlpacaVenueMount.resolve(&both_tiers.inputs(live)),
            Resolution::Armed {
                tier: Tier::Demo,
                held_below_live: Some(HeldBelowLive::DemoOnlyArm)
            },
            "LIVE-tier keys beside the SANDBOX set must leave the sandbox arming untouched"
        );
        assert_eq!(
            AlpacaVenueMount.resolve(&live_only.inputs(live)),
            Resolution::Paper(PaperCause::NoCredentials),
            "the arm reads the SANDBOX tier alone, so LIVE-tier keys by themselves arm nothing"
        );
    }
}

/// The account 2×2: a labelled account reads its OWN keys and never the default account's.
#[test]
fn a_labelled_account_reads_only_its_own_keys() {
    let mut alt_asks_default_keys = keyed(&AccountLabel::Default);
    alt_asks_default_keys.account = alt();
    assert_eq!(
        AlpacaVenueMount.resolve(&alt_asks_default_keys.inputs(true)),
        Resolution::Paper(PaperCause::NoCredentials),
        "ALT must not arm off the default account's keys"
    );
    assert!(matches!(
        AlpacaVenueMount.resolve(&keyed(&alt()).inputs(true)),
        Resolution::Armed { .. }
    ));
    let mut default_asks_alt_keys = keyed(&alt());
    default_asks_alt_keys.account = AccountLabel::Default;
    assert_eq!(
        AlpacaVenueMount.resolve(&default_asks_alt_keys.inputs(true)),
        Resolution::Paper(PaperCause::NoCredentials)
    );
}

/// Absent credentials: paper, nothing to reconcile, no probe row — and nothing was built.
#[test]
fn with_no_credentials_the_mount_is_paper_and_offline() {
    let fx = MountFixture::new(&[]);
    let (tx, _rx) = vike_exec::event_channel(8);
    let out = AlpacaVenueMount.mount(fx.request(true, "AAPL", &tx));
    assert!(matches!(out.exec, ExecOutcome::Paper));
    assert!(out.recon.is_none() && out.identity.is_none());
    assert!(AlpacaVenueMount.credential_probe(&fx.inputs(true)).is_none());
}

/// With the default account's keys the probe row exists, built without the network (the OAuth2
/// mint happens on the first request).
#[test]
fn credentialed_alpaca_offers_a_read_only_probe() {
    let fx = keyed(&AccountLabel::Default);
    assert!(matches!(
        AlpacaVenueMount.credential_probe(&fx.inputs(true)),
        Some(CredentialProbe::ReadOnly(_))
    ));
}
