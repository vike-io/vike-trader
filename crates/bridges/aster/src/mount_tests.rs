use super::*;
use std::assert_matches;
use vike_bridge_core::venue_mount_fixture::MountFixture;
use vike_model::accounts::account_keys::account_key;

/// The TESTNET agent-wallet pair — the DEMO tier's whole required key set (`SIGNER` is optional).
const TESTNET: &[(&str, &str)] =
    &[("ASTER_TESTNET_USER", "0xUser"), ("ASTER_TESTNET_PRIVATE_KEY", "0xkey")];
/// The LIVE (mainnet) agent-wallet pair.
const LIVE: &[(&str, &str)] = &[("ASTER_LIVE_USER", "0xUser"), ("ASTER_LIVE_PRIVATE_KEY", "0xkey")];

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}

/// A fixture holding `pairs` under `label`'s key names, asking for `label`.
fn keyed(pairs: &[(&str, &str)], label: &AccountLabel) -> MountFixture {
    let mut fx = MountFixture::new(&[]);
    for (k, v) in pairs {
        fx.vars.insert(account_key(k, label), (*v).to_string());
    }
    fx.account = label.clone();
    fx
}

/// The REAL body aster's futures host answered, captured from the CI box on 2026-08-09 (moved from
/// `vike-mount`, whose `fixture()` helper read it).
fn fixture() -> serde_json::Value {
    serde_json::from_str(include_str!("../tests/fixtures/server_time/aster.json"))
        .expect("the captured fixture is valid JSON")
}

/// An exec client that is never driven. The bound-tier pin reads the mount's OUTCOME, and the real
/// `AsterExecutionClient` would dial the venue from its own thread the moment it spawned.
struct Unspawned;

impl ExecutionClient for Unspawned {
    fn submit(&mut self, _request: &vike_model::OrderRequest) {}
    fn cancel(&mut self, _client_order_id: &str) {}
}

/// ⚠ THE MIGRATION'S ONE INTENDED BEHAVIOUR CHANGE (the venue mount contract spec's Finding 1).
/// For every (key set, tier) a mount can meet, the tier the mount BINDS — which `vike-mount`
/// records the authenticated account against — is the tier `resolve` REPORTS, and the network the
/// exec client is spawned on. `vike-mount`'s legacy fold addressed `Demo` for every row below (its
/// CEX conjunct `ceiling_selects_mainnet(venue) && live_permitted`, which is `false` for aster):
/// the two `Live` rows are the ones this test turns from red to green.
///
/// It drives [`mount_with`] — [`AsterVenueMount::mount`]'s own body — with only the grid pre-fetch
/// and the exec spawn replaced by offline doubles, so it reads what the mount BINDS: a tier written
/// into that body as a literal fails it.
#[test]
fn the_bound_tier_is_the_resolved_tier_and_a_live_key_set_binds_live() {
    let both: Vec<(&str, &str)> = LIVE.iter().chain(TESTNET).copied().collect();
    let (tx, _rx) = vike_exec::event_channel(8);
    for (pairs, live, want) in [
        (&both[..], true, Tier::Live),
        (LIVE, true, Tier::Live),
        (&both[..], false, Tier::Demo),
        (TESTNET, true, Tier::Demo),
    ] {
        let fx = keyed(pairs, &AccountLabel::Default);
        let mut spawned_on = None;
        let out = mount_with(
            fx.request(live, "BTCUSDT.P", &tx),
            |_, _, _| None,
            |spawn| {
                spawned_on = Some(spawn.env);
                Box::new(Unspawned)
            },
        );
        let ExecOutcome::Live(exec) = out.exec else {
            panic!("every row here resolves a key set, so the mount goes live");
        };
        assert_eq!(exec.bound_tier, want, "the tier the mount BINDS (and vike-mount records)");
        assert_matches!(
            AsterVenueMount.resolve(&fx.inputs(live)),
            Resolution::Armed { tier, .. } if tier == want,
            "the tier the arming screen REPORTS must be the same one"
        );
        assert_eq!(
            spawned_on,
            Some(if want == Tier::Live { Environment::Live } else { Environment::Demo }),
            "and it is the network the exec client is spawned on"
        );
    }
}

#[test]
fn the_declaration_is_the_rows_the_mount_crate_carried() {
    let d = AsterVenueMount.declaration();
    assert_eq!(AsterVenueMount.venue(), "aster");
    assert!(d.addresses_accounts && d.process_exclusive.is_none() && !d.takes_recon_trigger);
    assert_eq!(d.grid_source, DeclaredGridSource::PerSymbolFetch);
    assert_eq!(
        d.book_identity,
        BookIdentity::Named {
            prefix: "ASTER",
            demo_tiers: &["TESTNET"],
            live_tiers: &["LIVE"],
            name_suffixes: &["USER"],
            evm_key_suffixes: &[],
        }
    );
    assert_eq!(
        d.clock,
        ClockDecl::Wired {
            endpoint: "GET /fapi/v1/time (public)",
            auth: ClockAuth::Public,
            risk: ClockRisk::SignedTimestamp,
        }
    );
}

/// The arming-probe row as it was: Live FIRST for a `live`-tier account; a testnet key set arms the
/// demo tier and says what holds it below live; nothing resolvable is paper.
#[test]
fn resolve_is_the_arms_own_chain() {
    assert_eq!(
        AsterVenueMount.resolve(&keyed(LIVE, &AccountLabel::Default).inputs(true)),
        Resolution::Armed { tier: Tier::Live, held_below_live: None }
    );
    let testnet = Resolution::Armed {
        tier: Tier::Demo,
        held_below_live: Some(HeldBelowLive::LiveCredentialsAbsent),
    };
    for live in [false, true] {
        assert_eq!(
            AsterVenueMount.resolve(&keyed(TESTNET, &AccountLabel::Default).inputs(live)),
            testnet
        );
        assert_eq!(
            AsterVenueMount.resolve(&MountFixture::new(&[]).inputs(live)),
            Resolution::Paper(PaperCause::NoCredentials)
        );
    }
}

/// Review Focus 2, and the MEASURED HOLE this venue had: below a `live` tier the Live attempt is
/// DELETED from the chain — a LIVE-only store resolves nothing, a store holding both resolves the
/// testnet pair — so nothing resolves `Live`.
#[test]
fn a_ceiling_below_live_never_resolves_live() {
    assert_eq!(
        AsterVenueMount.resolve(&keyed(LIVE, &AccountLabel::Default).inputs(false)),
        Resolution::Paper(PaperCause::NoCredentials)
    );
    let both: Vec<(&str, &str)> = LIVE.iter().chain(TESTNET).copied().collect();
    assert!(!matches!(
        AsterVenueMount.resolve(&keyed(&both, &AccountLabel::Default).inputs(false)),
        Resolution::Armed { tier: Tier::Live, .. }
    ));
}

/// The account 2×2: this venue is MAINNET in practice, so a labelled account reading the default
/// account's agent wallet would be a second client signing for the first account's real money.
#[test]
fn a_labelled_account_reads_only_its_own_keys() {
    let mut alt_asks_default_keys = keyed(TESTNET, &AccountLabel::Default);
    alt_asks_default_keys.account = alt();
    assert_eq!(
        AsterVenueMount.resolve(&alt_asks_default_keys.inputs(true)),
        Resolution::Paper(PaperCause::NoCredentials),
        "ALT must not arm off the default account's agent wallet"
    );
    assert_matches!(
        AsterVenueMount.resolve(&keyed(TESTNET, &alt()).inputs(true)),
        Resolution::Armed { .. }
    );
    let mut default_asks_alt_keys = keyed(TESTNET, &alt());
    default_asks_alt_keys.account = AccountLabel::Default;
    assert_eq!(
        AsterVenueMount.resolve(&default_asks_alt_keys.inputs(true)),
        Resolution::Paper(PaperCause::NoCredentials)
    );
}

#[test]
fn with_no_credentials_the_mount_is_paper_and_offline() {
    let fx = MountFixture::new(&[]);
    let (tx, _rx) = vike_exec::event_channel(8);
    let out = AsterVenueMount.mount(fx.request(true, "BTCUSDT.P", &tx));
    assert!(matches!(out.exec, ExecOutcome::Paper));
    assert!(out.recon.is_none() && out.identity.is_none());
}

// ---- the clock HOST, offline --------------------------------------------------------------------

/// The clock is read on the network the DEFAULT account's mount would bind under this tier, and
/// on testnet when nothing resolves. The tier rows are the load-bearing ones: this venue's clock
/// row is `SignedTimestamp`, whose FAIL demotes it to paper, so a demo-tier box holding both key
/// sets that measured MAINNET's clock would be judged by a host it never signs for.
#[test]
fn the_clock_is_read_on_the_network_the_ceiling_lets_the_default_account_bind() {
    let both: Vec<(&str, &str)> = LIVE.iter().chain(TESTNET).copied().collect();
    for (pairs, live, want) in [
        (LIVE, false, Environment::Demo),
        (&both[..], false, Environment::Demo),
        (&both[..], true, Environment::Live),
        (&[][..], false, Environment::Demo),
        (&[][..], true, Environment::Demo),
    ] {
        let fx = keyed(pairs, &AccountLabel::Default);
        assert_eq!(
            clock_env(&fx.inputs(live)),
            want,
            "keys {pairs:?} under live_permitted = {live}"
        );
    }
}

// ---- the clock PARSE, against the real captured body (moved from vike-mount) ------------------

#[test]
fn the_parser_reads_the_real_captured_body() {
    assert_eq!(parse_server_time(&fixture()), Ok(1_786_242_370_439));
}

#[test]
fn a_body_without_the_stamp_names_the_missing_field() {
    assert_eq!(parse_server_time(&serde_json::json!({})), Err(missing_time_field("serverTime")));
}
