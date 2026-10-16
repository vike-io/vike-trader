use super::*;
use std::assert_matches;
use vike_bridge_core::venue_adapter_fixture::{AdapterFixture, found_tier_events};
use vike_log::capture::{CapturedEvent, captured};
use vike_model::accounts::account_keys::AccountLabel;
use vike_venues::credential_keys::{API_KEY, API_SECRET};

/// A distinctive stand-in for a secret, so "no value was logged" is an assertion that can fail.
const SECRET: &str = "SECRET-VALUE-do-not-log";

/// A fixture whose started account `label` holds the complete pair — its `API_KEY` and
/// `API_SECRET` fields, by the field words `api_credentials` reads (decision 0156 item 6: no name
/// is composed, so none is spelled here). The secret is [`SECRET`].
fn keyed(label: &AccountLabel) -> AdapterFixture {
    let mut fx = AdapterFixture::new(&[(API_KEY, "k"), (API_SECRET, SECRET)]);
    fx.account = label.clone();
    fx
}

/// A fixture whose started account `label` holds the key and NOT the secret — a half-written pair.
fn half_keyed(label: &AccountLabel) -> AdapterFixture {
    let mut fx = AdapterFixture::new(&[(API_KEY, SECRET)]);
    fx.account = label.clone();
    fx
}

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}

/// The REAL body the testnet answered, captured from the CI box on 2026-08-09 (moved from
/// `vike-tradehub-core`, whose `fixture()` helper read it).
fn fixture() -> serde_json::Value {
    serde_json::from_str(include_str!("../tests/fixtures/server_time/deribit.json"))
        .expect("the captured fixture is valid JSON")
}

#[test]
fn the_declaration_is_the_rows_the_tradehub_core_carried() {
    let d = DeribitVenueAdapter.declaration();
    assert_eq!(DeribitVenueAdapter.venue(), "deribit");
    assert!(d.addresses_accounts && d.process_exclusive.is_none() && !d.takes_recon_trigger);
    assert_eq!(d.grid_source, DeclaredGridSource::PerSymbolFetch);
    assert_matches!(d.book_identity, BookIdentity::Undeterminable { .. });
    assert_eq!(
        d.clock,
        ClockDecl::Wired {
            endpoint: "GET /api/v2/public/get_time (public, testnet host)",
            auth: ClockAuth::Public,
            risk: ClockRisk::NoTimestamp,
        }
    );
}

/// The arming row of a DEMO-ONLY arm (decision 0156 item 6): the account's pair arms the DEMO tier
/// — held below live as `DemoOnlyArm` — when the account's tier is `demo` (or `paper`, read as
/// demo); an account holding nothing is `NoCredentials` at every tier.
#[test]
fn resolve_is_the_arms_own_gate() {
    for live in [AccountTier::Paper, AccountTier::Demo, AccountTier::Live] {
        assert_eq!(
            DeribitVenueAdapter.resolve(&AdapterFixture::new(&[]).inputs(live)),
            Resolution::Paper(PaperCause::NoCredentials),
            "tier={live}"
        );
    }
    for below in [AccountTier::Paper, AccountTier::Demo] {
        assert_eq!(
            DeribitVenueAdapter.resolve(&keyed(&AccountLabel::Default).inputs(below)),
            Resolution::Armed {
                tier: AccountTier::Demo,
                held_below_live: Some(HeldBelowLive::DemoOnlyArm)
            },
            "tier={below}"
        );
    }
}

/// Review Focus 2 at this venue: no tier makes this arm resolve `Live` — it has no live arm.
#[test]
fn a_ceiling_below_live_never_resolves_live() {
    for live in [AccountTier::Paper, AccountTier::Demo, AccountTier::Live] {
        assert!(!matches!(
            DeribitVenueAdapter.resolve(&keyed(&AccountLabel::Default).inputs(live)),
            Resolution::Armed { tier: AccountTier::Live, .. }
        ));
    }
}

/// A `live` account holding its pair arms NOTHING — this arm binds only the testnet — and says WHY,
/// with the cause that names the live tier rather than the one that says the store holds nothing.
#[test]
fn a_live_key_set_alone_arms_nothing() {
    assert_eq!(
        DeribitVenueAdapter.resolve(&keyed(&AccountLabel::Default).inputs(AccountTier::Live)),
        Resolution::Paper(PaperCause::LiveTierNotWired)
    );
}

/// Only a COMPLETE pair is a key set: half of it is absent by the generic loader's own rule, so
/// the original cause stands, at every tier.
#[test]
fn half_a_key_set_is_still_no_credentials() {
    for live in [AccountTier::Demo, AccountTier::Live] {
        assert_eq!(
            DeribitVenueAdapter.resolve(&half_keyed(&AccountLabel::Default).inputs(live)),
            Resolution::Paper(PaperCause::NoCredentials),
            "tier={live}"
        );
    }
}

/// The cause is the STARTED account's: a labelled `live` account holding its pair is
/// `LiveTierNotWired`, and one holding nothing is `NoCredentials` — the label picks no key.
#[test]
fn the_live_tier_cause_is_scoped_to_the_account_that_holds_the_keys() {
    assert_eq!(
        DeribitVenueAdapter.resolve(&keyed(&alt()).inputs(AccountTier::Live)),
        Resolution::Paper(PaperCause::LiveTierNotWired)
    );
    let mut alt_empty = AdapterFixture::new(&[]);
    alt_empty.account = alt();
    assert_eq!(
        DeribitVenueAdapter.resolve(&alt_empty.inputs(AccountTier::Live)),
        Resolution::Paper(PaperCause::NoCredentials),
        "ALT holds no keys of its own"
    );
}

/// The stay-paper half at the START: a `live` account holding its pair does NOT RUN, offline,
/// reconcile asked for and not built.
#[test]
fn a_live_key_set_alone_starts_paper_offline() {
    let fx = keyed(&AccountLabel::parse("OFFLINE").expect("a legal label"));
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut req = fx.request(AccountTier::Live, "BTC-PERPETUAL", &tx);
    req.recon_enabled = true;
    let out = DeribitVenueAdapter.start(req);
    assert!(matches!(out.exec, ExecOutcome::Paper));
    assert!(out.recon.is_none() && out.identity.is_none());
}

/// The LABEL picks no key: a labelled `demo` account arms on its own pair and does not run without
/// one, exactly as the default account does.
#[test]
fn a_labelled_account_reads_only_its_own_keys() {
    assert_matches!(
        DeribitVenueAdapter.resolve(&keyed(&alt()).inputs(AccountTier::Demo)),
        Resolution::Armed { .. }
    );
    let mut alt_empty = AdapterFixture::new(&[]);
    alt_empty.account = alt();
    assert_eq!(
        DeribitVenueAdapter.resolve(&alt_empty.inputs(AccountTier::Demo)),
        Resolution::Paper(PaperCause::NoCredentials)
    );
}

#[test]
fn with_no_credentials_the_start_is_paper_and_offline() {
    let fx = AdapterFixture::new(&[]);
    let (tx, _rx) = vike_exec::event_channel(8);
    for live in [AccountTier::Demo, AccountTier::Live] {
        let out = DeribitVenueAdapter.start(fx.request(live, "BTC-PERPETUAL", &tx));
        assert!(matches!(out.exec, ExecOutcome::Paper));
        assert!(out.recon.is_none() && out.identity.is_none());
    }
}

// ---- the clock PARSE, against the real captured body (moved from vike-tradehub-core) ---------------

#[test]
fn the_parser_reads_the_real_captured_body() {
    assert_eq!(parse_server_time(&fixture()), Ok(1_786_242_370_599));
}

/// THE UNIT TRAP: `usIn`/`usOut` are MICROSECONDS beside the ms `result`.
#[test]
fn the_parser_does_not_read_the_microsecond_neighbour() {
    let body = fixture();
    let us_in = body["usIn"].as_i64().expect("the capture carries usIn");
    let read = parse_server_time(&body).expect("the ms result parses");
    assert_ne!(read, us_in, "usIn is microseconds — a thousandfold error");
    assert_eq!(read, us_in / 1_000);
}

/// deribit's row measures the TESTNET host every exec spawn site binds, and its body says so. A
/// mainnet body — `testnet: false` — is refused, and an absent flag is not an implicit pass.
#[test]
fn a_deribit_body_from_the_wrong_host_is_refused() {
    let mut mainnet = fixture();
    mainnet["testnet"] = serde_json::Value::Bool(false);
    let e = parse_server_time(&mainnet).expect_err("wrong host");
    assert!(e.contains("wrong host"), "{e}");
    let mut absent = fixture();
    absent.as_object_mut().expect("object").remove("testnet");
    assert!(parse_server_time(&absent).is_err());
}

// ── what the arm says about a LIVE account it will not trade ────────────────────────────────────

/// `start_with` over offline doubles at the account's tier `live`: no grid, no reconcile client, a
/// recording exec client.
fn start_offline(fx: &AdapterFixture, live: AccountTier) -> (AdapterOutcome, Vec<CapturedEvent>) {
    let (tx, _rx) = vike_exec::event_channel(8);
    captured(|| {
        start_with(
            fx.request(live, "BTC-PERPETUAL", &tx),
            |_, _| None,
            |_, _| None,
            |_, _, _, _| Box::new(vike_exec::testing::RecordingClient::default()),
        )
    })
}

/// A `live` account holding its pair is named ONCE at `error!` — `venue`, `account` and
/// `found_tier=live`, never `tier` — and the account does not run and OFFLINE: the doubles panic if
/// reached, so no grid, reconcile client or exec client is built for it. No credential VALUE
/// reaches any line.
#[test]
fn a_live_account_holding_its_pair_is_named_once_and_starts_paper_offline() {
    let live_account = AccountLabel::parse("LIVEACCT").expect("a legal label");
    let fx = keyed(&live_account);
    let (tx, _rx) = vike_exec::event_channel(8);
    let (out, events) = captured(|| {
        start_with(
            fx.request(AccountTier::Live, "BTC-PERPETUAL", &tx),
            |_, _| panic!("a live account is not built for, so no grid fetch"),
            |_, _| panic!("a live account is not built for, so no reconcile client"),
            |_, _, _, _| panic!("a live account is not built for, so no exec client"),
        )
    });
    assert!(matches!(out.exec, ExecOutcome::Paper));
    assert!(out.recon.is_none() && out.identity.is_none());
    let said = found_tier_events(&events);
    assert_eq!(said.len(), 1, "exactly one line about the live account: {events:?}");
    let e = said[0];
    assert_eq!(e.level, tracing::Level::ERROR, "{e:?}");
    assert_eq!(
        (e.field("venue"), e.field("account")),
        (Some("deribit"), Some("LIVEACCT")),
        "{e:?}"
    );
    assert_eq!(e.field("found_tier"), Some("live"), "{e:?}");
    assert!(e.field("tier").is_none(), "a diagnostic carries `found_tier`, never `tier`: {e:?}");
    assert!(
        events.iter().all(|e| !format!("{e:?}").contains(SECRET)),
        "no credential VALUE may reach a line: {events:?}"
    );
}

/// A `demo` account holding its pair — the ordinary case — starts the testnet, bound at DEMO, and
/// says nothing about a live tier.
#[test]
fn a_demo_pair_alone_adds_no_line_about_the_live_tier() {
    let alone = AccountLabel::parse("ALONE").expect("a legal label");
    let (out, events) = start_offline(&keyed(&alone), AccountTier::Demo);
    let ExecOutcome::Live(exec) = out.exec else { panic!("the demo pair mounts, as it did") };
    assert_eq!(exec.bound_tier, AccountTier::Demo);
    assert!(found_tier_events(&events).is_empty(), "{events:?}");
    assert!(
        events.iter().any(|e| e.field("tier") == Some("demo")),
        "the mount announcement is said, at the tier it bound: {events:?}"
    );
    assert!(
        events.iter().all(|e| !format!("{e:?}").contains(SECRET)),
        "no credential VALUE may reach a line: {events:?}"
    );
}

/// A half-written pair (a key without its secret) is ABSENT by the generic loader's rule: the
/// start is paper and offline and says nothing about a live tier — the arming screen's cause
/// (`NoCredentials`) is the whole report.
#[test]
fn a_half_written_pair_is_silent_and_the_start_is_paper() {
    let half = AccountLabel::parse("HALF").expect("a legal label");
    let fx = half_keyed(&half);
    let (tx, _rx) = vike_exec::event_channel(8);
    let (out, events) = captured(|| {
        start_with(
            fx.request(AccountTier::Live, "BTC-PERPETUAL", &tx),
            |_, _| panic!("no complete pair, so no grid fetch"),
            |_, _| panic!("no complete pair, so no reconcile client"),
            |_, _, _, _| panic!("no complete pair, so no exec client"),
        )
    });
    assert!(matches!(out.exec, ExecOutcome::Paper));
    assert!(found_tier_events(&events).is_empty(), "{events:?}");
    assert!(!format!("{events:?}").contains(SECRET), "no credential VALUE: {events:?}");
    assert_eq!(
        DeribitVenueAdapter.resolve(&fx.inputs(AccountTier::Live)),
        Resolution::Paper(PaperCause::NoCredentials)
    );
}

/// A LABELLED account is started only when it armed, so a `live` one holding its pair never reaches
/// `start` — `vike-tradehub-core` asks the arm to speak for it instead, and the arm says exactly what
/// `start` says. Everything else (a half-written pair, an empty account, a `demo` account) is
/// silent.
#[test]
fn an_account_that_is_never_started_is_spoken_for_in_the_same_words() {
    let unmounted = AccountLabel::parse("UNMNT").expect("a legal label");
    let (_, events) = captured(|| {
        DeribitVenueAdapter
            .report_not_running_account(&keyed(&unmounted).inputs(AccountTier::Live));
    });
    let said = found_tier_events(&events);
    assert_eq!(said.len(), 1, "{events:?}");
    assert_eq!(said[0].level, tracing::Level::ERROR, "{events:?}");
    assert_eq!(said[0].field("account"), Some("UNMNT"), "{events:?}");

    for (fx, live) in [
        (half_keyed(&unmounted), AccountTier::Live),
        (AdapterFixture::new(&[]), AccountTier::Live),
        (keyed(&unmounted), AccountTier::Demo),
    ] {
        let (_, events) =
            captured(|| DeribitVenueAdapter.report_not_running_account(&fx.inputs(live)));
        assert!(events.is_empty(), "tier={live}: nothing to say: {events:?}");
    }
}
