use super::*;
use std::assert_matches;
use vike_bridge_core::venue_mount_fixture::MountFixture;

/// The agent-wallet pair — an account's whole required key set (`SIGNER` is optional).
const PAIR: &[(&str, &str)] = &[("USER", "0xUser"), ("PRIVATE_KEY", "0xkey")];

fn alt() -> vike_model::accounts::account_keys::AccountLabel {
    vike_model::accounts::account_keys::AccountLabel::parse("ALT").expect("a legal label")
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
/// For every tier a mount can meet, the tier the mount BINDS — which `vike-mount` records the
/// authenticated account against — is the tier `resolve` REPORTS, and the network the exec client
/// is spawned on. `vike-mount`'s legacy fold addressed `Demo` for every row below (its CEX conjunct
/// `ceiling_selects_mainnet(venue) && live_permitted`, which is `false` for aster): the `Live` row
/// is the one this test turned from red to green.
///
/// It drives [`mount_with`] — [`AsterVenueMount::mount`]'s own body — with only the grid pre-fetch
/// and the exec spawn replaced by offline doubles, so it reads what the mount BINDS: a tier written
/// into that body as a literal fails it.
#[test]
fn the_bound_tier_is_the_resolved_tier_and_a_live_key_set_binds_live() {
    let (tx, _rx) = vike_exec::event_channel(8);
    for (tier, want) in [
        (AccountTier::Live, AccountTier::Live),
        (AccountTier::Demo, AccountTier::Demo),
        (AccountTier::Paper, AccountTier::Demo),
    ] {
        let fx = MountFixture::new(PAIR);
        let mut spawned_on = None;
        let out = mount_with(
            fx.request(tier, "BTCUSDT.P", &tx),
            |_, _, _| None,
            |spawn| {
                spawned_on = Some(spawn.env);
                Box::new(Unspawned)
            },
        );
        let ExecOutcome::Live(exec) = out.exec else {
            panic!("every row here holds the account's pair, so the mount goes live");
        };
        assert_eq!(exec.bound_tier, want, "the tier the mount BINDS (and vike-mount records)");
        assert_matches!(
            AsterVenueMount.resolve(&fx.inputs(tier)),
            Resolution::Armed { tier, .. } if tier == want,
            "the tier the arming screen REPORTS must be the same one"
        );
        assert_eq!(spawned_on, Some(want), "and it is the network the exec client is spawned on");
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
        BookIdentity::Named { name_fields: &["USER"], evm_key_fields: &[] }
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
            AsterVenueMount.resolve(&MountFixture::new(PAIR).inputs(tier)),
            Resolution::Armed { tier: want, held_below_live: None },
            "{tier}"
        );
    }
    for fields in [&[][..], &[("USER", "0xUser")][..], &[("PRIVATE_KEY", "0xkey")][..]] {
        assert_eq!(
            AsterVenueMount.resolve(&MountFixture::new(fields).inputs(AccountTier::Live)),
            Resolution::Paper(PaperCause::LiveCredentialsAbsent),
            "live with {fields:?}"
        );
        assert_eq!(
            AsterVenueMount.resolve(&MountFixture::new(fields).inputs(AccountTier::Demo)),
            Resolution::Paper(PaperCause::NoCredentials),
            "demo with {fields:?}"
        );
    }
}

/// Review Focus 2, and the MEASURED HOLE this venue had: below a `live` tier nothing resolves
/// `Live` — the account's pair arms the testnet it was issued for, whatever it holds.
#[test]
fn a_tier_below_live_never_resolves_live() {
    for tier in [AccountTier::Demo, AccountTier::Paper] {
        assert!(!matches!(
            AsterVenueMount.resolve(&MountFixture::new(PAIR).inputs(tier)),
            Resolution::Armed { tier: AccountTier::Live, .. }
        ));
    }
}

/// The account's FIELDS are the only credential the mount reads: a labelled account arms on its
/// own pair, and a deployment value spelled like a field word arms nothing — this venue is MAINNET
/// in practice, so a key read from anywhere but the account would sign for real money it does not
/// belong to.
#[test]
fn only_the_accounts_own_fields_arm() {
    let mut labelled = MountFixture::new(PAIR);
    labelled.account = alt();
    assert_matches!(
        AsterVenueMount.resolve(&labelled.inputs(AccountTier::Live)),
        Resolution::Armed { tier: AccountTier::Live, .. }
    );
    let deployment_only = MountFixture::new(&[]).with_vars(PAIR);
    assert_eq!(
        AsterVenueMount.resolve(&deployment_only.inputs(AccountTier::Live)),
        Resolution::Paper(PaperCause::LiveCredentialsAbsent),
        "a deployment's named values are never an account's credential"
    );
}

#[test]
fn with_no_credentials_the_mount_is_paper_and_offline() {
    let fx = MountFixture::new(&[]);
    let (tx, _rx) = vike_exec::event_channel(8);
    let out = AsterVenueMount.mount(fx.request(AccountTier::Live, "BTCUSDT.P", &tx));
    assert!(matches!(out.exec, ExecOutcome::Paper));
    assert!(out.recon.is_none() && out.identity.is_none());
}

// ---- the clock HOST, offline --------------------------------------------------------------------

/// The clock is read on the network the account's mount would bind under its tier, and on testnet
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
        let fx = MountFixture::new(fields);
        assert_eq!(clock_env(&fx.inputs(tier)), want, "fields {fields:?} under tier = {tier}");
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
