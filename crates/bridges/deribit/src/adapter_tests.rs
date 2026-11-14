use super::*;
use std::assert_matches;
use vike_bridge_core::venue_adapter::{ArmingHeldBelowLive, ArmingNotRunningCause};
use vike_bridge_core::venue_adapter_fixture::{AdapterFixture, found_tier_events};
use vike_log::capture::{CapturedEvent, captured};
use vike_model::SymbolProperties;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::venue_setting::VenueSettings;
use vike_venues::credential_keys::{API_KEY, API_SECRET};
use vike_venues::venue_urls::VenueUrls;

/// What the testnet row of `BTC-PERPETUAL` yields: its lot is 10 dollars and its `amount` IS dollars.
fn inverse_perp_facts() -> DeribitProperties {
    DeribitProperties {
        grid: SymbolProperties {
            tick_size: 0.5,
            step_size: 10.0,
            min_qty: 10.0,
            contract_size: 10.0,
            ..Default::default()
        },
        value: Ok(vike_model::ContractValue::inverse(1.0)),
    }
}

/// A distinctive stand-in for a secret, so "no value was logged" is an assertion that can fail.
const SECRET: &str = "SECRET-VALUE-do-not-log";

/// A fixture whose started account `label` holds the complete pair — its `API_KEY` and
/// `API_SECRET` fields, by the field words `api_credentials` reads (decision 0156 item 6: no name
/// is composed, so none is spelled here). The secret is [`SECRET`].
fn keyed(label: &AccountLabel) -> AdapterFixture {
    let mut fx = AdapterFixture::new(&[(API_KEY, "k"), (API_SECRET, SECRET)]);
    fx.account = label.clone();
    fx.settings = VenueSettings::default().with_urls(VenueUrls::seed("deribit"));
    fx
}

/// [`keyed`] over `rows` instead of the release's seed.
fn keyed_over(label: &AccountLabel, rows: VenueUrls) -> AdapterFixture {
    let mut fx = keyed(label);
    fx.settings = VenueSettings::default().with_urls(rows);
    fx
}

/// The seed's testnet REST and socket addresses are what the exec half dialled before they moved into
/// rows, path included, byte for byte.
#[test]
fn the_seed_gives_the_testnet_addresses_the_execution_half_always_dialled() {
    let urls = DeribitUrls::of(&VenueUrls::seed("deribit")).expect("the seed holds both rows");
    assert_eq!(urls.ws, "wss://test.deribit.com/ws/api/v2");
    assert_eq!(urls.rest, "https://test.deribit.com");
}

/// A `demo` account whose rows hold no testnet `trade` row does NOT RUN, says which row, and dials
/// nothing: no grid fetch, no reconcile client, no exec client.
#[test]
fn a_demo_account_with_no_testnet_row_is_not_running_and_names_the_row() {
    let fx = keyed_over(
        &AccountLabel::parse("NOROWS").expect("a legal label"),
        VenueUrls::seed_typed("deribit", vike_venues::venue_urls::UrlType::Live),
    );
    let (tx, _rx) = vike_execution::event_channel(8);
    let (out, events) = captured(|| {
        start_with(
            fx.request(AccountTier::Demo, "BTC-PERPETUAL", &tx),
            |_, _| panic!("no row, so no grid fetch"),
            |_, _, _| panic!("no row, so no reconcile client"),
            |_, _, _, _, _| panic!("no row, so no exec client"),
        )
    });
    assert!(matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning));
    assert!(
        events.iter().any(|e| e.level == tracing::Level::ERROR
            && format!("{e:?}").contains("no address row for demo trade ws")),
        "the refusal names the row: {events:?}"
    );
}

/// A demo row that names the LIVE host is refused by the guard: a demo account never dials live.
#[test]
fn a_demo_row_naming_the_live_host_is_refused() {
    let rows = VenueUrls::seed("deribit");
    let live_websocket = rows
        .url(
            vike_venues::venue_urls::UrlType::Live,
            vike_venues::venue_urls::UrlChannel::Trade,
            vike_venues::venue_urls::UrlProtocol::Websocket,
            "",
        )
        .expect("the live ws row")
        .to_string();
    let swapped: Vec<_> = vike_venues::venue_urls::VENUE_URL_SEED
        .iter()
        .filter(|s| s.venue == "deribit")
        .map(|s| {
            let url = if s.ty == vike_venues::venue_urls::UrlType::Demo
                && s.protocol == vike_venues::venue_urls::UrlProtocol::Websocket
            {
                live_websocket.as_str()
            } else {
                s.url
            };
            (s.venue, s.ty, s.channel, s.protocol, s.product, url)
        })
        .collect();
    let refusal = DeribitUrls::of(&VenueUrls::from_rows("deribit", swapped))
        .expect_err("a demo row equal to the live one is refused");
    assert!(refusal.to_string().contains("LIVE address"), "{refusal}");
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
    let d = VenueAdapterDeribit.declaration();
    assert_eq!(VenueAdapterDeribit.venue(), "deribit");
    assert!(d.addresses_accounts && d.process_exclusive.is_none() && !d.takes_reconcile_trigger);
    assert_eq!(d.grid_source, GridSource::PerSymbolFetch);
    assert_matches!(d.book_identity, BookIdentityRule::Undeterminable { .. });
    assert_eq!(
        d.clock,
        ClockDeclaration::Wired {
            endpoint: "GET /api/v2/public/get_time (public, testnet host)",
            auth: ClockAuthentication::Public,
            risk: ClockRisk::NoTimestamp,
        }
    );
}

/// The arming row of a DEMO-ONLY arm (decision 0156 item 6): the account's pair arms the DEMO tier
/// — held below live as `ArmingDemoOnly` — when the account's tier is `demo` (or `paper`, read as
/// demo); an account holding nothing is `NoCredentials` at every tier.
#[test]
fn resolve_is_the_arms_own_gate() {
    for live in [AccountTier::Paper, AccountTier::Demo, AccountTier::Live] {
        assert_eq!(
            VenueAdapterDeribit.resolve(&AdapterFixture::new(&[]).inputs(live)),
            ArmingResolution::NotRunning(ArmingNotRunningCause::NoCredentials),
            "tier={live}"
        );
    }
    for below in [AccountTier::Paper, AccountTier::Demo] {
        assert_eq!(
            VenueAdapterDeribit.resolve(&keyed(&AccountLabel::Default).inputs(below)),
            ArmingResolution::Armed {
                tier: AccountTier::Demo,
                held_below_live: Some(ArmingHeldBelowLive::DemoOnlyArm)
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
            VenueAdapterDeribit.resolve(&keyed(&AccountLabel::Default).inputs(live)),
            ArmingResolution::Armed { tier: AccountTier::Live, .. }
        ));
    }
}

/// A `live` account holding its pair arms NOTHING — this arm binds only the testnet — and says WHY,
/// with the cause that names the live tier rather than the one that says the store holds nothing.
#[test]
fn a_live_key_set_alone_arms_nothing() {
    assert_eq!(
        VenueAdapterDeribit.resolve(&keyed(&AccountLabel::Default).inputs(AccountTier::Live)),
        ArmingResolution::NotRunning(ArmingNotRunningCause::LiveTierNotWired)
    );
}

/// Only a COMPLETE pair is a key set: half of it is absent by the generic loader's own rule, so
/// the original cause stands, at every tier.
#[test]
fn half_a_key_set_is_still_no_credentials() {
    for live in [AccountTier::Demo, AccountTier::Live] {
        assert_eq!(
            VenueAdapterDeribit.resolve(&half_keyed(&AccountLabel::Default).inputs(live)),
            ArmingResolution::NotRunning(ArmingNotRunningCause::NoCredentials),
            "tier={live}"
        );
    }
}

/// The cause is the STARTED account's: a labelled `live` account holding its pair is
/// `LiveTierNotWired`, and one holding nothing is `NoCredentials` — the label picks no key.
#[test]
fn the_live_tier_cause_is_scoped_to_the_account_that_holds_the_keys() {
    assert_eq!(
        VenueAdapterDeribit.resolve(&keyed(&alt()).inputs(AccountTier::Live)),
        ArmingResolution::NotRunning(ArmingNotRunningCause::LiveTierNotWired)
    );
    let mut alt_empty = AdapterFixture::new(&[]);
    alt_empty.account = alt();
    assert_eq!(
        VenueAdapterDeribit.resolve(&alt_empty.inputs(AccountTier::Live)),
        ArmingResolution::NotRunning(ArmingNotRunningCause::NoCredentials),
        "ALT holds no keys of its own"
    );
}

/// The stay-paper half at the START: a `live` account holding its pair does NOT RUN, offline,
/// reconcile asked for and not built.
#[test]
fn a_live_key_set_alone_starts_paper_offline() {
    let fx = keyed(&AccountLabel::parse("OFFLINE").expect("a legal label"));
    let (tx, _rx) = vike_execution::event_channel(8);
    let mut req = fx.request(AccountTier::Live, "BTC-PERPETUAL", &tx);
    req.reconcile_enabled = true;
    let out = VenueAdapterDeribit.start(req);
    assert!(matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning));
    assert!(out.reconcile.is_none() && out.identity.is_none());
}

/// The LABEL picks no key: a labelled `demo` account arms on its own pair and does not run without
/// one, exactly as the default account does.
#[test]
fn a_labelled_account_reads_only_its_own_keys() {
    assert_matches!(
        VenueAdapterDeribit.resolve(&keyed(&alt()).inputs(AccountTier::Demo)),
        ArmingResolution::Armed { .. }
    );
    let mut alt_empty = AdapterFixture::new(&[]);
    alt_empty.account = alt();
    assert_eq!(
        VenueAdapterDeribit.resolve(&alt_empty.inputs(AccountTier::Demo)),
        ArmingResolution::NotRunning(ArmingNotRunningCause::NoCredentials)
    );
}

#[test]
fn with_no_credentials_the_start_is_paper_and_offline() {
    let fx = AdapterFixture::new(&[]);
    let (tx, _rx) = vike_execution::event_channel(8);
    for live in [AccountTier::Demo, AccountTier::Live] {
        let out = VenueAdapterDeribit.start(fx.request(live, "BTC-PERPETUAL", &tx));
        assert!(matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning));
        assert!(out.reconcile.is_none() && out.identity.is_none());
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
    let (tx, _rx) = vike_execution::event_channel(8);
    captured(|| {
        start_with(
            fx.request(live, "BTC-PERPETUAL", &tx),
            |_, _| Some(inverse_perp_facts()),
            |_, _, _| None,
            |_, _, _, _, _| Box::new(vike_execution::testing::RecordingClient::default()),
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
    let (tx, _rx) = vike_execution::event_channel(8);
    let (out, events) = captured(|| {
        start_with(
            fx.request(AccountTier::Live, "BTC-PERPETUAL", &tx),
            |_, _| panic!("a live account is not built for, so no grid fetch"),
            |_, _, _| panic!("a live account is not built for, so no reconcile client"),
            |_, _, _, _, _| panic!("a live account is not built for, so no exec client"),
        )
    });
    assert!(matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning));
    assert!(out.reconcile.is_none() && out.identity.is_none());
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
    let VenueAdapterExecutionOutcome::Running(exec) = out.exec else {
        panic!("the demo pair mounts, as it did")
    };
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
    let (tx, _rx) = vike_execution::event_channel(8);
    let (out, events) = captured(|| {
        start_with(
            fx.request(AccountTier::Live, "BTC-PERPETUAL", &tx),
            |_, _| panic!("no complete pair, so no grid fetch"),
            |_, _, _| panic!("no complete pair, so no reconcile client"),
            |_, _, _, _, _| panic!("no complete pair, so no exec client"),
        )
    });
    assert!(matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning));
    assert!(found_tier_events(&events).is_empty(), "{events:?}");
    assert!(!format!("{events:?}").contains(SECRET), "no credential VALUE: {events:?}");
    assert_eq!(
        VenueAdapterDeribit.resolve(&fx.inputs(AccountTier::Live)),
        ArmingResolution::NotRunning(ArmingNotRunningCause::NoCredentials)
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
        VenueAdapterDeribit
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
            captured(|| VenueAdapterDeribit.report_not_running_account(&fx.inputs(live)));
        assert!(events.is_empty(), "tier={live}: nothing to say: {events:?}");
    }
}

/// Decision 0170: the mounted client carries the row's contract value (inverse, unit 1.0 for
/// `BTC-PERPETUAL`) and the grid with the lot still on it, and `contract_size` is no money.
#[test]
fn a_demo_mount_carries_the_rows_contract_value_and_keeps_the_lot_on_the_grid() {
    let alone = AccountLabel::parse("VALUE").expect("a legal label");
    let (out, _events) = start_offline(&keyed(&alone), AccountTier::Demo);
    let VenueAdapterExecutionOutcome::Running(exec) = out.exec else {
        panic!("the demo pair mounts")
    };
    assert_eq!(exec.contract_value, Some(vike_model::ContractValue::inverse(1.0)));
    assert_eq!(exec.grid.map(|g| g.contract_size), Some(10.0));
}

/// An instrument row nobody could fetch leaves the kind unknown, and an unknown kind is never
/// `Linear` on the venue that lists coin-margined contracts: the account does not run.
#[test]
fn a_row_that_could_not_be_fetched_is_not_running_and_says_why() {
    let fx = keyed(&AccountLabel::parse("NOROW").expect("a legal label"));
    let (tx, _rx) = vike_execution::event_channel(8);
    let (out, events) = captured(|| {
        start_with(
            fx.request(AccountTier::Demo, "BTC-PERPETUAL", &tx),
            |_, _| None,
            |_, _, _| panic!("a refused start builds no reconcile client"),
            |_, _, _, _, _| panic!("a refused start builds no exec client"),
        )
    });
    assert!(matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning));
    assert!(
        events.iter().any(|e| e.level == tracing::Level::ERROR),
        "the refusal is said at error!: {events:?}"
    );
}

/// An operator's read of an account with no pair is refused in words, at either tier and for every
/// kind, and never as `not offered`: the bridge offers it. Nothing here dials (this test has no venue).
#[test]
fn a_read_of_an_account_with_no_pair_is_refused_before_any_network() {
    let fx = half_keyed(&AccountLabel::Default);
    for tier in [AccountTier::Demo, AccountTier::Live] {
        for kind in [VenueReadKind::Balance, VenueReadKind::OpenOrders, VenueReadKind::Positions] {
            let why = VenueAdapterDeribit.venue_read(&fx.inputs(tier), kind).unwrap_err();
            assert!(why.contains("holds no deribit"), "{why}");
            assert!(!why.contains(SECRET), "a credential never reaches a sentence: {why}");
        }
    }
}

/// A `live` account's pair is not a testnet pair, and this bridge has no other host: it is told so with
/// nothing sent, instead of the venue's "invalid credentials" from the wrong network.
#[test]
fn a_read_of_a_live_account_asks_nothing_and_says_why() {
    let fx = keyed(&AccountLabel::Default);
    for kind in [VenueReadKind::Balance, VenueReadKind::OpenOrders, VenueReadKind::Positions] {
        let why = VenueAdapterDeribit.venue_read(&fx.inputs(AccountTier::Live), kind).unwrap_err();
        assert!(why.contains("only deribit's testnet"), "{why}");
        assert!(why.contains("nothing was asked"), "{why}");
        assert!(!why.contains(SECRET), "a credential never reaches a sentence: {why}");
    }
}

/// **A demo account MOUNTED on a coin-settled option does not run, and says why** (decision 0233, item 4,
/// at the mount): the facts are the mount's own read of the REAL `BTC-5OCT26-74000-C` row
/// (`crate::execution::parse_instrument_facts`), whose premium is quoted in BTC, so no grid or exec client is
/// built and the refusal's words reach the log. It ran before, valued as dollars.
#[test]
fn a_coin_settled_option_mount_is_not_running_and_says_why() {
    let all: serde_json::Value =
        serde_json::from_str(include_str!("../tests/fixtures/deribit_get_instruments.json"))
            .expect("the fixture is valid JSON");
    let row = all["option_btc"]["result"]
        .as_array()
        .and_then(|rows| rows.iter().find(|r| r["instrument_name"] == "BTC-5OCT26-74000-C"))
        .expect("the recorded coin-settled option row")
        .clone();
    let facts = crate::execution::parse_instrument_facts(&serde_json::json!({ "result": row }))
        .expect("the row has a grid");
    let fx = keyed(&AccountLabel::parse("COINOPT").expect("a legal label"));
    let (tx, _rx) = vike_execution::event_channel(8);
    let (out, events) = captured(|| {
        start_with(
            fx.request(AccountTier::Demo, "BTC-5OCT26-74000-C", &tx),
            |_, _| Some(facts),
            |_, _, _| panic!("a refused start builds no reconcile client"),
            |_, _, _, _, _| panic!("a refused start builds no exec client"),
        )
    });
    assert!(matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning));
    assert!(
        events.iter().any(|e| format!("{e:?}").contains("is a coin-settled option")),
        "the refusal's own words reach the log: {events:?}"
    );
}

/// A row that cannot be valued (no margining word, or quanto) is not guessed either.
#[test]
fn a_row_with_no_contract_value_is_not_running_and_says_why() {
    let fx = keyed(&AccountLabel::parse("NOVAL").expect("a legal label"));
    let (tx, _rx) = vike_execution::event_channel(8);
    let (out, events) = captured(|| {
        start_with(
            fx.request(AccountTier::Demo, "BTC-PERPETUAL", &tx),
            |_, _| {
                Some(DeribitProperties {
                    grid: SymbolProperties::default(),
                    value: Err("the BTC-PERPETUAL row states no instrument_type".to_string()),
                })
            },
            |_, _, _| panic!("a refused start builds no reconcile client"),
            |_, _, _, _, _| panic!("a refused start builds no exec client"),
        )
    });
    assert!(matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning));
    assert!(
        events.iter().any(|e| format!("{e:?}").contains("states no instrument_type")),
        "the row's own reason reaches the log: {events:?}"
    );
}
