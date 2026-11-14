use super::*;
use std::assert_matches;
use vike_bridge_core::venue_adapter::{ArmingNotRunningCause, missing_time_field};
use vike_bridge_core::venue_adapter_fixture::{
    AdapterFixture, ExecutionClientInert, assert_one_key_set_resolves_its_tier,
};
use vike_secrets::venue_setting::VenueSettings;
use vike_venues::venue_urls::VenueUrls;

/// The agent-wallet pair — an account's whole required key set (`SIGNER` is optional).
const PAIR: &[(&str, &str)] = &[("USER", "0xUser"), ("PRIVATE_KEY", "0xkey")];

/// A fixture holding `fields` and the release's aster rows, as a fresh box's start is handed them.
fn seeded(fields: &[(&str, &str)]) -> AdapterFixture {
    let mut fx = AdapterFixture::new(fields);
    fx.settings = VenueSettings::default().with_urls(VenueUrls::seed(VENUE));
    fx
}

fn alt() -> vike_model::accounts::account_keys::AccountLabel {
    vike_model::accounts::account_keys::AccountLabel::parse("ALT").expect("a legal label")
}

/// The REAL body aster's futures host answered, captured from the CI box on 2026-08-09 (moved from
/// `vike-tradehub-core`, whose `fixture()` helper read it).
fn fixture() -> serde_json::Value {
    serde_json::from_str(include_str!("../tests/fixtures/server_time/aster.json"))
        .expect("the captured fixture is valid JSON")
}

/// ⚠ THE MIGRATION'S ONE INTENDED BEHAVIOUR CHANGE (the venue-adapter contract spec's Finding 1).
/// For every tier a start can meet, the tier the start BINDS — which `vike-tradehub-core` records the
/// authenticated account against — is the tier `resolve` REPORTS, and the network the exec client
/// is spawned on. `vike-tradehub-core`'s legacy fold addressed `Demo` for every row below (its CEX conjunct
/// `ceiling_selects_mainnet(venue) && live_permitted`, which is `false` for aster): the `Live` row
/// is the one this test turned from red to green.
///
/// It drives [`start_with`] — [`VenueAdapterAster::start`]'s own body — with only the grid pre-fetch
/// and the exec spawn replaced by offline doubles, so it reads what the start BINDS: a tier written
/// into that body as a literal fails it.
#[test]
fn the_bound_tier_is_the_resolved_tier_and_a_live_key_set_binds_live() {
    let (tx, _rx) = vike_execution::event_channel(8);
    for (tier, want) in [
        (AccountTier::Live, AccountTier::Live),
        (AccountTier::Demo, AccountTier::Demo),
        (AccountTier::Paper, AccountTier::Demo),
    ] {
        let fx = seeded(PAIR);
        let mut spawned_on = None;
        let out = start_with(
            fx.request(tier, "BTCUSDT.P", &tx),
            |_, _, _| None,
            |spawn| {
                spawned_on = Some(spawn.urls);
                Box::new(ExecutionClientInert)
            },
        );
        let VenueAdapterExecutionOutcome::Running(exec) = out.exec else {
            panic!("every row here holds the account's pair, so the mount goes live");
        };
        assert_eq!(
            exec.bound_tier, want,
            "the tier the mount BINDS (and vike-tradehub-core records)"
        );
        assert_matches!(
            VenueAdapterAster.resolve(&fx.inputs(tier)),
            ArmingResolution::Armed { tier, .. } if tier == want,
            "the tier the arming screen REPORTS must be the same one"
        );
        assert_eq!(
            spawned_on,
            Some(AsterUrls::exec(&VenueUrls::seed(VENUE), want).unwrap()),
            "and it is the network the exec client is spawned on"
        );
    }
}

/// The start resolves its hosts ONCE, from the account's own rows at the BOUND tier, and hands the
/// SAME four to the grid pre-fetch and the exec spawn: a `demo` start never reaches a live host.
#[test]
fn the_start_hands_the_grid_and_the_spawn_the_bound_tiers_rows() {
    let (tx, _rx) = vike_execution::event_channel(8);
    let live = AsterUrls::exec(&VenueUrls::seed(VENUE), AccountTier::Live).unwrap();
    for tier in [AccountTier::Demo, AccountTier::Paper] {
        let fx = seeded(PAIR);
        let (mut grid_on, mut spawned_on) = (None, None);
        let _ = start_with(
            fx.request(tier, "BTCUSDT.P", &tx),
            |urls, _, _| {
                grid_on = Some(urls);
                None
            },
            |spawn| {
                spawned_on = Some(spawn.urls);
                Box::new(ExecutionClientInert)
            },
        );
        let (grid_on, spawned_on) = (grid_on.unwrap(), spawned_on.unwrap());
        assert_eq!(grid_on, spawned_on, "{tier}: one resolution feeds both");
        for host in [
            spawned_on.fapi_rest,
            spawned_on.sapi_rest,
            spawned_on.fapi_websocket,
            spawned_on.sapi_websocket,
        ] {
            assert!(
                ![live.fapi_rest, live.sapi_rest, live.fapi_websocket, live.sapi_websocket]
                    .contains(&host),
                "{tier}: {host} is a live address"
            );
        }
    }
}

/// An account holding its pair whose rows cannot answer is NOT RUNNING: paper, no reconcile client,
/// nothing fetched and nothing spawned — never a fallback to another network's address.
#[test]
fn a_start_whose_rows_cannot_answer_is_not_running_and_dials_nothing() {
    let (tx, _rx) = vike_execution::event_channel(8);
    for tier in [AccountTier::Live, AccountTier::Demo] {
        let fx = AdapterFixture::new(PAIR); // a bare `VenueSettings`: no rows at all
        let out = start_with(
            fx.request(tier, "BTCUSDT.P", &tx),
            |_, _, _| panic!("{tier}: no grid fetch without a host"),
            |_| panic!("{tier}: no spawn without a host"),
        );
        assert!(matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning), "{tier}");
        assert!(out.reconcile.is_none() && out.identity.is_none(), "{tier}");
    }
}

/// The clock read refuses, naming the row, when the account's `trade` row is missing — it never
/// reads another network's clock instead.
#[test]
fn the_clock_read_refuses_without_the_rows() {
    let fx = AdapterFixture::new(PAIR);
    let err = VenueAdapterAster
        .server_time_ms(&fx.inputs(AccountTier::Demo), Duration::from_millis(1))
        .unwrap_err();
    assert!(err.contains("demo trade rest product=usdm"), "{err}");
}

#[test]
fn the_declaration_is_the_rows_the_tradehub_core_carried() {
    let d = VenueAdapterAster.declaration();
    assert_eq!(VenueAdapterAster.venue(), "aster");
    assert!(d.addresses_accounts && d.process_exclusive.is_none() && !d.takes_reconcile_trigger);
    assert_eq!(d.grid_source, GridSource::PerSymbolFetch);
    assert_eq!(
        d.book_identity,
        BookIdentityRule::Named { name_fields: &["USER"], evm_key_fields: &[] }
    );
    assert_eq!(
        d.clock,
        ClockDeclaration::Wired {
            endpoint: "GET /fapi/v1/time (public)",
            auth: ClockAuthentication::Public,
            risk: ClockRisk::SignedTimestamp,
        }
    );
}

/// The arming probe over the account-has-one-tier rule (decision 0156 item 6): the account's pair
/// arms the account's own tier with nothing held below live; without a usable pair a `live`
/// account is paper for want of its LIVE key set and a `demo` one for want of any — an account has
/// no second (testnet) set beside its own to fall back to. A half-written pair is no pair.
#[test]
fn resolve_arms_the_accounts_own_tier() {
    for (tier, want) in
        [(AccountTier::Live, AccountTier::Live), (AccountTier::Demo, AccountTier::Demo)]
    {
        assert_eq!(
            VenueAdapterAster.resolve(&AdapterFixture::new(PAIR).inputs(tier)),
            ArmingResolution::Armed { tier: want, held_below_live: None },
            "{tier}"
        );
    }
    for fields in [&[][..], &[("USER", "0xUser")][..], &[("PRIVATE_KEY", "0xkey")][..]] {
        assert_eq!(
            VenueAdapterAster.resolve(&AdapterFixture::new(fields).inputs(AccountTier::Live)),
            ArmingResolution::NotRunning(ArmingNotRunningCause::LiveCredentialsAbsent),
            "live with {fields:?}"
        );
        assert_eq!(
            VenueAdapterAster.resolve(&AdapterFixture::new(fields).inputs(AccountTier::Demo)),
            ArmingResolution::NotRunning(ArmingNotRunningCause::NoCredentials),
            "demo with {fields:?}"
        );
    }
}

/// The shared one-key-set matrix at EVERY tier, `Paper` included: `live` alone is mainnet.
#[test]
fn every_tier_arms_the_network_it_names() {
    let absent = [AdapterFixture::new(&[]), AdapterFixture::new(&[("USER", "0xUser")])];
    assert_one_key_set_resolves_its_tier(&VenueAdapterAster, absent, &AdapterFixture::new(PAIR));
}

/// Review Focus 2, and the MEASURED HOLE this venue had: below a `live` tier nothing resolves
/// `Live` — the account's pair arms the testnet it was issued for, whatever it holds.
#[test]
fn a_tier_below_live_never_resolves_live() {
    for tier in [AccountTier::Demo, AccountTier::Paper] {
        assert!(!matches!(
            VenueAdapterAster.resolve(&AdapterFixture::new(PAIR).inputs(tier)),
            ArmingResolution::Armed { tier: AccountTier::Live, .. }
        ));
    }
}

/// The account's FIELDS are the only credential the start reads: a labelled account arms on its
/// own pair, and a deployment value spelled like a field word arms nothing — this venue is MAINNET
/// in practice, so a key read from anywhere but the account would sign for real money it does not
/// belong to.
#[test]
fn only_the_accounts_own_fields_arm() {
    let mut labelled = AdapterFixture::new(PAIR);
    labelled.account = alt();
    assert_matches!(
        VenueAdapterAster.resolve(&labelled.inputs(AccountTier::Live)),
        ArmingResolution::Armed { tier: AccountTier::Live, .. }
    );
    let deployment_only = AdapterFixture::new(&[]).with_vars(PAIR);
    assert_eq!(
        VenueAdapterAster.resolve(&deployment_only.inputs(AccountTier::Live)),
        ArmingResolution::NotRunning(ArmingNotRunningCause::LiveCredentialsAbsent),
        "a deployment's named values are never an account's credential"
    );
}

#[test]
fn with_no_credentials_the_start_is_paper_and_offline() {
    let fx = AdapterFixture::new(&[]);
    let (tx, _rx) = vike_execution::event_channel(8);
    let out = VenueAdapterAster.start(fx.request(AccountTier::Live, "BTCUSDT.P", &tx));
    assert!(matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning));
    assert!(out.reconcile.is_none() && out.identity.is_none());
}

// ---- the clock HOST, offline --------------------------------------------------------------------

/// The clock is read on the network the account's start would bind under its tier, and on testnet
/// when nothing resolves. The tier rows are the load-bearing ones: this venue's clock row is
/// `SignedTimestamp`, whose FAIL demotes it to paper, so a demo-tier account that measured
/// MAINNET's clock would be judged by a host it never signs for.
#[test]
fn the_clock_is_read_on_the_network_the_tier_lets_the_account_bind() {
    for (fields, tier, want) in [
        (PAIR, AccountTier::Demo, AccountTier::Demo),
        (PAIR, AccountTier::Paper, AccountTier::Demo),
        (PAIR, AccountTier::Live, AccountTier::Live),
        (&[][..], AccountTier::Demo, AccountTier::Demo),
        (&[][..], AccountTier::Live, AccountTier::Demo),
    ] {
        let fx = AdapterFixture::new(fields);
        assert_eq!(clock_env(&fx.inputs(tier)), want, "fields {fields:?} under tier = {tier}");
    }
}

// ---- the clock PARSE, against the real captured body (moved from vike-tradehub-core) ------------------

#[test]
fn the_parser_reads_the_real_captured_body() {
    assert_eq!(parse_server_time(&fixture()), Ok(1_786_242_370_439));
}

#[test]
fn a_body_without_the_stamp_names_the_missing_field() {
    assert_eq!(parse_server_time(&serde_json::json!({})), Err(missing_time_field("serverTime")));
}
