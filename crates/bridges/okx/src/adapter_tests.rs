use std::assert_matches;
use std::sync::{Mutex, MutexGuard, PoisonError};

use tracing::Level;

use super::*;
use vike_bridge_core::venue_adapter::ArmingNotRunningCause;
use vike_bridge_core::venue_adapter_fixture::{
    AdapterFixture, assert_one_key_set_resolves_its_tier,
};
use vike_log::capture::captured;
use vike_model::accounts::account_keys::AccountLabel;
use vike_venues::credential_keys::{API_KEY, API_PASSPHRASE, API_SECRET};

/// A fixture whose started account holds the complete trio — the passphrase is required
/// (`vike_secrets::venue_passphrase`'s `Required` row). The account's fields, by the field words
/// `api_credentials` reads (decision 0156 item 6: no name is composed, so none is spelled here). Its
/// addresses are the release's seed rows, what a fresh box's table holds.
fn trio() -> AdapterFixture {
    let mut fx = trio_without_addresses();
    fx.settings = vike_secrets::venue_setting::VenueSettings::default()
        .with_urls(vike_venues::venue_urls::VenueUrls::seed("okx"));
    fx
}

/// [`trio`] on a box whose `venue_url` table holds no okx row.
fn trio_without_addresses() -> AdapterFixture {
    AdapterFixture::new(&[(API_KEY, "k"), (API_SECRET, "s"), (API_PASSPHRASE, "p")])
}

/// A fixture whose started account holds key and secret but NO passphrase — the half trio.
fn half_trio() -> AdapterFixture {
    AdapterFixture::new(&[(API_KEY, "k"), (API_SECRET, "s")])
}

/// `fx`, started as account `label`.
fn as_account(mut fx: AdapterFixture, label: AccountLabel) -> AdapterFixture {
    fx.account = label;
    fx
}

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}

/// The REAL body okx answered, captured from the CI box on 2026-08-09 (copied from `vike-tradehub-core`, whose
/// clock tests read the same capture).
fn clock_body() -> serde_json::Value {
    serde_json::from_str(include_str!("../tests/fixtures/server_time/okx.json"))
        .expect("the captured fixture is valid JSON")
}

/// Every test that calls `VenueAdapterOkx::start` takes a turn, because the two log lines
/// `the_half_credential_report_names_the_passphrase_of_the_ceilings_tier` captures live there.
/// `tracing` caches an `Interest` per callsite, process-wide, and while a capture is the only live
/// dispatcher a thread with no subscriber that reaches a line FIRST fixes it at `never` for every
/// thread. ⚠ The turn is no longer what makes those captures hold — `vike_log::capture::captured`
/// does, against a sibling running the lines uncaptured at ANY time (its module doc carries the
/// mechanism and the anchor). What the turn still closes is that capture's one declared residual:
/// a first registration racing the process's first capture. (A runner with a process per test,
/// like nextest, never shares the cache; `cargo test` runs the tests as threads of one process.)
static START_LOCK: Mutex<()> = Mutex::new(());

fn start_turn() -> MutexGuard<'static, ()> {
    // A panicking test poisons the lock; what it guards is `tracing`'s cache, which a panic does
    // not corrupt.
    START_LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One `tracing` event, as the assertions below read it — projected from
/// `vike_log::capture::CapturedEvent`, the workspace's one scoped capture.
#[derive(Debug)]
struct Captured {
    level: Level,
    target: String,
    venue: Option<String>,
    message: String,
}

/// `fx`'s start under `ceiling_live` — which must be an offline PAPER start — and every event
/// `mount.rs` emitted during it (the parent module's target: this module's path, one segment up).
fn paper_start_events(fx: &AdapterFixture, ceiling_live: AccountTier) -> Vec<Captured> {
    let (tx, _rx) = vike_execution::event_channel(8);
    let (out, events) =
        captured(|| VenueAdapterOkx.start(fx.request(ceiling_live, "BTCUSDT.P", &tx)));
    assert!(
        matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning),
        "every mount captured here is paper"
    );
    let target = module_path!().rsplit_once("::").map_or("", |(parent, _)| parent);
    events
        .into_iter()
        .map(|e| Captured {
            venue: e.field("venue").map(str::to_string),
            level: e.level,
            target: e.target,
            message: e.message,
        })
        // the shared arming lines (`vike_bridge_core::venue_adapter`'s report fns) are written under their own module
        .filter(|e| e.target == target || e.target == "vike_bridge_core::venue_adapter")
        .collect()
}

/// The rows `vike-tradehub-core`'s tables carried for okx, pinned as a matrix.
#[test]
fn the_declaration_is_the_rows_the_tradehub_core_carried() {
    let d = VenueAdapterOkx.declaration();
    assert_eq!(VenueAdapterOkx.venue(), "okx");
    assert!(d.addresses_accounts && d.process_exclusive.is_none());
    assert_eq!(d.grid_source, GridSource::PerSymbolFetch);
    assert_matches!(d.book_identity, BookIdentityRule::Undeterminable { .. });
    assert_eq!(
        d.clock,
        ClockDeclaration::Wired {
            endpoint: "GET /api/v5/public/time (public)",
            auth: ClockAuthentication::Public,
            risk: ClockRisk::SignedTimestamp,
        }
    );
}

/// THE D1 MATRIX (docs/decisions/0095, decision 0156 item 6): the account's tier is the network,
/// and its trio is the one it holds. A complete trio arms the account's own tier; no trio is
/// paper, and under a `live` tier the cause is `LiveCredentialsAbsent`.
#[test]
fn resolve_reads_exactly_the_tier_the_ceiling_names() {
    let empty = AdapterFixture::new(&[]);
    assert_eq!(
        VenueAdapterOkx.resolve(&empty.inputs(AccountTier::Demo)),
        ArmingResolution::NotRunning(ArmingNotRunningCause::NoCredentials)
    );
    assert_eq!(
        VenueAdapterOkx.resolve(&empty.inputs(AccountTier::Live)),
        ArmingResolution::NotRunning(ArmingNotRunningCause::LiveCredentialsAbsent)
    );
    assert_eq!(
        VenueAdapterOkx.resolve(&trio().inputs(AccountTier::Demo)),
        ArmingResolution::Armed { tier: AccountTier::Demo, held_below_live: None }
    );
    assert_eq!(
        VenueAdapterOkx.resolve(&trio().inputs(AccountTier::Live)),
        ArmingResolution::Armed { tier: AccountTier::Live, held_below_live: None }
    );
}

/// The shared one-key-set matrix at EVERY tier, `Paper` included, and the startup probe's tier
/// with it: `live` alone is mainnet.
#[test]
fn every_tier_arms_the_network_it_names() {
    let absent = [AdapterFixture::new(&[]), half_trio()];
    assert_one_key_set_resolves_its_tier(&VenueAdapterOkx, absent, &trio());
}

/// Review Focus 2 at this venue: a complete trio under a tier below `live` resolves the DEMO tier
/// and never `Live` — `Paper` reads as `Demo`.
#[test]
fn a_ceiling_below_live_never_resolves_live() {
    for below in [AccountTier::Demo, AccountTier::Paper] {
        assert_eq!(
            VenueAdapterOkx.resolve(&trio().inputs(below)),
            ArmingResolution::Armed { tier: AccountTier::Demo, held_below_live: None },
            "tier={below}"
        );
    }
}

/// THE HALF-CREDENTIAL CASE (the pure half of
/// `crates/vike-gatewayhub/tests/trade_roster/mount_roster/preconnect.rs`'s
/// `okx_key_and_secret_without_a_passphrase_do_not_start_live`, which drives the real
/// `make_engine` over the same shape): key and secret without the passphrase okx's signer requires
/// are NO key set — the arming row of an absent trio at each tier, paper, and offline. The
/// contract has no cause of its own for it; the `error!` naming the missing field is `start`'s.
#[test]
fn a_key_and_secret_without_a_passphrase_is_no_key_set() {
    let _turn = start_turn();
    let (tx, _rx) = vike_execution::event_channel(8);
    let half = half_trio();
    for (ceiling_live, cause) in [
        (AccountTier::Demo, ArmingNotRunningCause::NoCredentials),
        (AccountTier::Live, ArmingNotRunningCause::LiveCredentialsAbsent),
    ] {
        assert_eq!(
            VenueAdapterOkx.resolve(&half.inputs(ceiling_live)),
            ArmingResolution::NotRunning(cause)
        );
        let out = VenueAdapterOkx.start(half.request(ceiling_live, "BTCUSDT.P", &tx));
        assert!(matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning));
        assert!(out.reconcile.is_none() && out.identity.is_none());
        assert!(VenueAdapterOkx.credential_probe(&half.inputs(ceiling_live)).is_none());
    }
}

/// THE HALF-CREDENTIAL REPORT, captured. Once okx's row flips this is the only site that can emit
/// it — okx is `vike_secrets::venue_passphrase`'s one `Required` row — and a test that stops at
/// the paper outcome cannot see it go. Key and secret without the passphrase name the missing
/// FIELD (`API_PASSPHRASE`) at `error!`, at either tier; under a `live` tier the no-LIVE-trio
/// warning follows it, in the legacy prefix's order. An account holding nothing reports nothing
/// (the ordinary unconfigured state), and a complete trio has no finding to report.
#[test]
fn the_half_credential_report_names_the_passphrase_of_the_ceilings_tier() {
    let _turn = start_turn();
    let is_report_naming = |event: &Captured, name: &str| {
        event.level == Level::ERROR
            && event.venue.as_deref() == Some(VENUE)
            && event.message.starts_with(&format!("{name} is unset or blank, but {VENUE} REQUIRES"))
    };

    let at_demo = paper_start_events(&half_trio(), AccountTier::Demo);
    assert_eq!(at_demo.len(), 1, "{at_demo:?}");
    assert!(is_report_naming(&at_demo[0], API_PASSPHRASE), "{at_demo:?}");
    assert!(at_demo[0].message.contains(&format!("`vike-cli secrets set {API_PASSPHRASE}`")));
    // The whole sentence: the store verb, never the `secrets.env` file a migrated box doesn't read.
    let report = format!(
        "{API_PASSPHRASE} is unset or blank, but {VENUE} REQUIRES an API passphrase — its key and \
         secret alone cannot sign a single request. These credentials are UNUSABLE, so {VENUE} is \
         NOT RUNNING (absent credentials are the live gate). Write it with `vike-cli secrets set \
         {API_PASSPHRASE}` to mount it live."
    );
    assert_eq!(at_demo[0].message, report, "{at_demo:?}");

    let at_live = paper_start_events(&half_trio(), AccountTier::Live);
    assert_eq!(at_live.len(), 2, "{at_live:?}");
    assert!(is_report_naming(&at_live[0], API_PASSPHRASE), "{at_live:?}");
    assert_eq!(at_live[1].level, Level::WARN, "{at_live:?}");
    assert_eq!(at_live[1].venue.as_deref(), Some(VENUE), "{at_live:?}");
    assert_eq!(
        at_live[1].message,
        format!("{VENUE}: {}", vike_bridge_core::venue_adapter::LIVE_CREDENTIALS_ABSENT)
    );

    for ceiling_live in [AccountTier::Demo, AccountTier::Live] {
        let events = paper_start_events(&AdapterFixture::new(&[]), ceiling_live);
        assert!(events.iter().all(|e| e.level != Level::ERROR), "{events:?}");
        assert_eq!(VenueAdapterOkx::missing_passphrase(&trio().inputs(ceiling_live)), None);
    }
}

/// THE REPORT IS ABOUT THE ACCOUNT BEING STARTED. A labelled account with key and secret but no
/// passphrase of its own is reported too — naming the FIELD the operator has to write on that
/// account — and the line names the store verb that writes it rather than a file a migrated box
/// does not read. A labelled account whose own trio is complete has nothing to report.
#[test]
fn a_labelled_accounts_half_credential_report_names_its_own_passphrase_key() {
    let _turn = start_turn();
    for ceiling_live in [AccountTier::Demo, AccountTier::Live] {
        let events = paper_start_events(&as_account(half_trio(), alt()), ceiling_live);
        let reports: Vec<&Captured> = events.iter().filter(|e| e.level == Level::ERROR).collect();
        assert_eq!(reports.len(), 1, "{events:?}");
        let report = reports[0];
        assert_eq!(report.venue.as_deref(), Some(VENUE), "{report:?}");
        let opening = format!("{API_PASSPHRASE} is unset or blank, but {VENUE} REQUIRES");
        assert!(report.message.starts_with(&opening), "{report:?}");
        assert!(
            report.message.contains(&format!("`vike-cli secrets set {API_PASSPHRASE}`")),
            "the line names the store verb that writes the key: {report:?}"
        );
        let whole = format!(
            "{opening} an API passphrase — its key and secret alone cannot sign a single request. \
             These credentials are UNUSABLE, so {VENUE} is NOT RUNNING (absent credentials are \
             the live gate). Write it with `vike-cli secrets set {API_PASSPHRASE}` to mount it \
             live."
        );
        assert_eq!(
            report.message, whole,
            "the store verb, never the `secrets.env` file a migrated box does not read"
        );
        assert_eq!(
            VenueAdapterOkx::missing_passphrase(&as_account(trio(), alt()).inputs(ceiling_live)),
            None,
            "ALT's complete trio has no finding"
        );
    }
}

/// The account 2×2: the LABEL picks no key — the start reads the credentials it is handed, the
/// started account's own, which `vike-tradehub-core` resolved from the account row — so a labelled account
/// arms on its trio and does not run without one, exactly as the default account does.
#[test]
fn a_labelled_account_reads_only_its_own_keys() {
    assert_eq!(
        VenueAdapterOkx
            .resolve(&as_account(AdapterFixture::new(&[]), alt()).inputs(AccountTier::Demo)),
        ArmingResolution::NotRunning(ArmingNotRunningCause::NoCredentials),
        "ALT without keys of its own is paper"
    );
    assert_matches!(
        VenueAdapterOkx.resolve(&as_account(trio(), alt()).inputs(AccountTier::Demo)),
        ArmingResolution::Armed { .. }
    );
}

/// No complete trio: paper, nothing to reconcile, nothing recorded, OFFLINE — and no startup
/// credential probe either.
#[test]
fn without_a_key_set_for_the_ceilings_tier_the_start_is_paper_and_offline() {
    let _turn = start_turn();
    let (tx, _rx) = vike_execution::event_channel(8);
    let empty = AdapterFixture::new(&[]);
    for ceiling_live in [AccountTier::Demo, AccountTier::Live] {
        let out = VenueAdapterOkx.start(empty.request(ceiling_live, "BTCUSDT.P", &tx));
        assert!(matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning));
        assert!(out.reconcile.is_none() && out.identity.is_none());
        assert!(VenueAdapterOkx.credential_probe(&empty.inputs(ceiling_live)).is_none());
    }
}

/// THE TIER at the one leg that SIGNS — the property `vike-tradehub-core`'s `startup_tests.rs` pinned for
/// this venue under this name until `probe_mainnet` left with this port: the probe binds the
/// account's own tier, so a `demo` account never signs against the real-money network. Pure — the
/// reconcile client is a signer plus a transport, built with no contract size (never a guessed one).
#[test]
fn a_demo_ceiling_never_signs_against_the_real_money_tier() {
    assert!(matches!(
        VenueAdapterOkx.credential_probe(&trio().inputs(AccountTier::Demo)),
        Some(CredentialProbe::RecordsIdentity { bound_tier: AccountTier::Demo, .. })
    ));
    assert!(matches!(
        VenueAdapterOkx.credential_probe(&trio().inputs(AccountTier::Live)),
        Some(CredentialProbe::RecordsIdentity { bound_tier: AccountTier::Live, .. })
    ));
}

/// Moved from `crates/vike-tradehub-core/src/recon.rs`'s `recon_client_tests` (its okx row and the okx
/// line of its armed-mainnet row): the reconcile handle builds network-free at both tiers, with no
/// `ctVal`, as the probe builds it.
#[test]
fn credentialed_okx_yields_a_reconcile_client_at_both_tiers() {
    for tier in [AccountTier::Demo, AccountTier::Live] {
        let net = OkxNet::resolve(&vike_venues::venue_urls::VenueUrls::seed("okx"), tier)
            .expect("the seed's rows");
        assert!(
            crate::reconcile::recon_client(
                &Credentials::for_test("test-key", "test-secret", Some("test-pass")),
                "BTC-USDT-SWAP",
                None,
                &net
            )
            .is_some(),
            "tier={tier}"
        );
    }
}

/// **No address row, no start**: a complete trio on a box whose `venue_url` table holds no okx row
/// starts NOTHING — paper, no reconcile client, offline (the refusal comes before the grid fetch) — and
/// says why at `error!`, naming the row it lacks. Its startup probe FAILS in the same words rather than
/// going silent, and so do the clock leg and the two per-symbol verbs.
#[test]
fn a_complete_trio_with_no_address_row_does_not_run_and_says_which_row() {
    let _turn = start_turn();
    let (tx, _rx) = vike_execution::event_channel(8);
    let fx = trio_without_addresses();
    for tier in [AccountTier::Demo, AccountTier::Live] {
        let (out, events) = captured(|| VenueAdapterOkx.start(fx.request(tier, "BTCUSDT.P", &tx)));
        assert!(matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning), "tier={tier}");
        assert!(out.reconcile.is_none() && out.identity.is_none(), "tier={tier}");
        let said: Vec<&str> =
            events.iter().filter(|e| e.level == Level::ERROR).map(|e| e.message.as_str()).collect();
        assert!(
            said.iter().any(|m| m.contains("no address row") && m.contains("NOT RUNNING")),
            "tier={tier}: {said:?}"
        );
        let probe = VenueAdapterOkx.credential_probe(&fx.inputs(tier));
        let Some(CredentialProbe::ReadOnly(probe)) = probe else {
            panic!("tier={tier}: a refused account's probe must fail, not vanish");
        };
        assert!(probe().unwrap_err().text.contains("no address row"), "tier={tier}");
        let clock = VenueAdapterOkx.server_time_ms(&fx.inputs(tier), Duration::from_millis(1));
        assert!(clock.unwrap_err().contains("no address row"), "tier={tier}");
        let leverage =
            VenueAdapterOkx.set_symbol_leverage(&fx.inputs(tier), "ETHUSDT.P", 2.0).unwrap_err();
        assert!(leverage.to_string().contains("no address row"), "tier={tier}: {leverage}");
        let modes = VenueAdapterOkx.heal_modes(&fx.inputs(tier), "ETHUSDT.P").unwrap_err();
        assert!(modes.contains("no address row"), "tier={tier}: {modes}");
    }
}

// ---- the clock PARSE, against the real captured body (moved from vike-tradehub-core) ---------------

#[test]
fn the_parser_reads_the_real_captured_body() {
    assert_eq!(parse_server_time(&clock_body()), Ok(1_786_242_370_157));
}

/// THE UNIT TRAP: the value is a STRING inside an ARRAY, so the naive `as_i64()` reads nothing.
#[test]
fn the_stamp_is_a_string_inside_an_array() {
    let body = clock_body();
    assert!(body["data"][0]["ts"].as_i64().is_none(), "precondition: it is a string");
    assert_eq!(parse_server_time(&body), Ok(1_786_242_370_157));
}

/// A body that answered without the stamp is an ERROR naming the field — including an EMPTY `data`
/// array (the shape that would index out of bounds) and a NUMBER `ts` (the shape a future API
/// change might send), which is refused rather than silently mis-read.
#[test]
fn a_body_without_the_stamp_names_the_missing_field() {
    let missing = Err(missing_time_field("data[0].ts"));
    assert_eq!(parse_server_time(&serde_json::json!({})), missing);
    assert_eq!(parse_server_time(&serde_json::json!({"code":"0","data":[],"msg":""})), missing);
    assert_eq!(
        parse_server_time(&serde_json::json!({"data":[{"ts":1_786_242_370_157i64}]})),
        missing
    );
}

// ---- the coin-margined mount (decision 0258's okx amendment) ----------------------------------------

/// One row of the demo's measured class fixture, by the key it is filed under.
fn demo_row(key: &str) -> crate::execution::rest::OkxInstrument {
    let all: serde_json::Value = serde_json::from_str(include_str!(
        "../tests/fixtures/classes/demo_instruments_2026-10-30.json"
    ))
    .expect("fixture is JSON");
    let row = all[key].clone();
    let id = row["instId"].as_str().expect("instId").to_string();
    crate::execution::rest::parse_okx_perp_instruments(&serde_json::json!({ "data": [row] }))[&id]
        .clone()
}

/// **A coin-margined mount reports `Inverse(100)` on its CONTRACTS grid and builds its reconcile client
/// with the factor 1.0**: the engine's quantity is the contract count. A linear mount is what it always
/// was (the grid in BASE, its `ctVal`, no value); a linear name with no row reports no grid and no factor
/// (never a guessed one: its reconcile client reads the size itself); an inverse name with no row is refused.
#[test]
fn an_inverse_mount_states_its_value_on_its_contracts_grid() {
    let inverse = demo_row("inverse_swap");
    let (grid, factor, value) = mount_parts("BTCUSD.P", Some(inverse.clone())).expect("mounts");
    assert_eq!(grid, Some(inverse.properties), "the venue's contracts grid, unscaled");
    assert_eq!(factor, Some(1.0));
    assert_eq!(value, Some(vike_model::ContractValue::inverse(100.0)));

    let linear = demo_row("future");
    let (grid, factor, value) =
        mount_parts("QTUMUSDT_261225", Some(linear.clone())).expect("linear");
    assert_eq!(grid, Some(vike_catalog::okx_grid_in_base(&linear.properties, linear.ct_val)));
    assert_eq!((factor, value), (Some(linear.ct_val), None));

    assert_eq!(mount_parts("BTCUSDT.P", None), Ok((None, None, None)), "never a guessed ctVal");
    assert_eq!(
        mount_parts("BTCUSD.P", None),
        Err(OkxMountRefusal::InverseRowUnread { symbol: "BTCUSD.P".to_string() })
    );
}

/// [`trio`] whose demo `trade rest` row (the instrument read's host) is `rest`: every other row is the seed's.
fn trio_reading_rows_from(rest: &str) -> AdapterFixture {
    use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlType, VENUE_URL_SEED, VenueUrls};
    let rows = VENUE_URL_SEED.iter().filter(|s| s.venue == VENUE).map(|s| {
        let is_rest = (s.ty, s.channel, s.protocol, s.product)
            == (UrlType::Demo, UrlChannel::Trade, UrlProtocol::Rest, "");
        (s.venue, s.ty, s.channel, s.protocol, s.product, if is_rest { rest } else { s.url })
    });
    let mut fx = trio_without_addresses();
    fx.settings = vike_secrets::venue_setting::VenueSettings::default()
        .with_urls(VenueUrls::from_rows(VENUE, rows));
    fx
}

/// A loopback venue that answers the ONE request it is sent with a 200 and `body`, then closes: its address.
fn answering_once(body: &'static str) -> String {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    std::thread::spawn(move || {
        let (mut conn, _) = listener.accept().expect("accept");
        // read the request before answering: a close with it unread resets the connection
        let _request = conn.read(&mut [0u8; 4096]);
        let _ = conn.write_all(
            format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        );
    });
    base
}

/// **A coin-margined start whose row read fails is told apart by the read's kind** (owner, 2026-11-16), through
/// the real start on loopback addresses: a venue that cannot be reached (a refused connection) is `Unreachable`,
/// asked again until it starts; a venue that answers with no row for the instrument is `NotRunning`, which waits
/// for the operator. Neither builds a reconcile client or spawns anything.
#[test]
fn a_coin_margined_start_unreachable_is_asked_again_and_one_refused_parks() {
    let _turn = start_turn();
    let (tx, _rx) = vike_execution::event_channel(8);
    let no_row = answering_once(r#"{"code":"0","msg":"","data":[]}"#);
    crate::instrument::kept_list_for_tests();
    for (rest, asked_again) in [("http://127.0.0.1:1", true), (no_row.as_str(), false)] {
        let fx = trio_reading_rows_from(rest);
        let out = VenueAdapterOkx.start(fx.request(AccountTier::Demo, "BTCUSD.P", &tx));
        let unreachable = matches!(out.exec, VenueAdapterExecutionOutcome::Unreachable);
        let parked = matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning);
        assert_eq!((unreachable, parked), (asked_again, !asked_again), "{rest}");
        assert!(out.reconcile.is_none() && out.identity.is_none(), "{rest}");
    }
}

/// The operator's open-orders read is the ADAPTER's (base: the trait's default answered `not offered by okx` to every
/// kind): with no complete key trio it is told so, at either tier, and a trio on a box with no address row is told
/// the row, both with nothing asked; the balance and the positions keep the shared `not offered` words even with a
/// trio. Offline: a read with a trio and the rows would reach the venue, so this test asks none.
#[test]
fn the_adapter_answers_the_open_orders_read_and_offers_no_other() {
    use vike_model::VenueReadKind::{Balance, OpenOrders, Positions};
    for tier in [AccountTier::Demo, AccountTier::Live] {
        let why = VenueAdapterOkx.venue_read(&half_trio().inputs(tier), OpenOrders).unwrap_err();
        assert_eq!(why, OkxAccountRefusal::NoKeySetNothingAsked.carried(), "{tier}");
    }
    let why = VenueAdapterOkx
        .venue_read(&trio_without_addresses().inputs(AccountTier::Demo), OpenOrders)
        .unwrap_err();
    assert!(why.text().contains("no address row"), "the missing row is said: {why}");
    for kind in [Balance, Positions] {
        let why = VenueAdapterOkx.venue_read(&trio().inputs(AccountTier::Demo), kind);
        assert_eq!(why.unwrap_err(), vike_model::not_offered_words(VENUE, kind).into());
    }
}

/// The DEMO account's `GET /api/v5/account/config` answer, captured 2026-11-22 by
/// `tests/okx_smokes/okx_key_permissions_capture.rs`: a trade-only key (`perm` `read_only,trade`) on a Multi-currency
/// margin account (`acctLv` 3) with auto-borrow off (`autoLoan` false).
fn captured_key_answer() -> serde_json::Value {
    let file: serde_json::Value =
        serde_json::from_str(include_str!("../tests/fixtures/captured/account_config_demo.json"))
            .expect("the captured fixture is valid JSON");
    file["frames"][0].clone()
}

/// **A live start reads its key's permissions FIRST, on the mainnet network** (B3.20). The captured trade-only key
/// passes the gate; the same answer with the token `withdraw` added by hand (a withdraw-capable key cannot be
/// captured on demo) is REFUSED, and `flags.allow_withdraw_keys` passes it; a demo start reads nothing. Offline: the
/// mounted name is one the kept list does not hold, so a start that passes the gate stops at that refusal, the next
/// step, before anything is fetched — and which of the two refusals spoke tells the gate's verdict.
#[test]
fn a_live_start_reads_its_key_permissions_first_and_a_withdraw_key_does_not_run() {
    let _turn = start_turn();
    crate::instrument::kept_list_for_tests();
    let (tx, _rx) = vike_execution::event_channel(8);
    let mut fx = trio();
    // `(refused by the gate, stopped at the kept list)` for a start whose key answers `answer`.
    let refusals = |fx: &AdapterFixture, tier: AccountTier, answer: &serde_json::Value| {
        let (out, events) = captured(|| {
            VenueAdapterOkx::start_with(fx.request(tier, "NOPEUSDT.P", &tx), |_, net, _| {
                assert!(net.mainnet, "only a live start reads its key");
                Ok(answer.clone())
            })
        });
        assert!(matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning));
        assert!(out.reconcile.is_none() && out.identity.is_none());
        let said = |words: &str| {
            events.iter().any(|e| e.level == Level::ERROR && e.message.contains(words))
        };
        (said("REFUSING to arm okx LIVE"), said("until the kept list holds it"))
    };
    let trade_only = captured_key_answer();
    let mut withdraw = captured_key_answer();
    withdraw["data"][0]["perm"] = "read_only,trade,withdraw".into();
    assert_eq!(
        refusals(&fx, AccountTier::Live, &trade_only),
        (false, true),
        "a trade-only key arms"
    );
    assert_eq!(
        refusals(&fx, AccountTier::Live, &withdraw),
        (true, false),
        "a withdraw key is refused"
    );
    assert_eq!(
        refusals(&fx, AccountTier::Demo, &withdraw),
        (false, true),
        "a demo start reads nothing"
    );
    fx.flags.allow_withdraw_keys = true;
    assert_eq!(refusals(&fx, AccountTier::Live, &withdraw), (false, true), "the flag arms the key");
}

/// **A live start refuses an account on which a spot order can BORROW** (decision 0232, amended 2026-11-23), judged on
/// the SAME `account/config` answer the withdraw gate reads: the captured DEMO account (Multi-currency margin,
/// `autoLoan` off) arms; the same answer with `autoLoan` on is REFUSED in words naming the setting, and
/// `flags.allow_withdraw_keys` does not arm it; a demo start reads nothing and is never refused; a failed read arms,
/// warned (the withdraw gate's posture). The kept-list stop tells a start that passed, as in the test above.
#[test]
fn a_live_start_refuses_an_account_on_which_a_spot_order_can_borrow() {
    let _turn = start_turn();
    crate::instrument::kept_list_for_tests();
    let (tx, _rx) = vike_execution::event_channel(8);
    let mut fx = trio();
    // `(the refusal's words, stopped at the kept list, the unread-configuration warning)` for a start whose one
    // `account/config` read answers `read`.
    let start =
        |fx: &AdapterFixture, tier: AccountTier, read: Result<serde_json::Value, String>| {
            let (out, events) = captured(|| {
                VenueAdapterOkx::start_with(fx.request(tier, "NOPEUSDT.P", &tx), |_, net, _| {
                    assert!(net.mainnet, "only a live start reads the account");
                    read
                })
            });
            assert!(matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning));
            let said = |level: Level, words: &str| {
                events.iter().find(|e| e.level == level && e.message.contains(words))
            };
            (
                said(Level::ERROR, "REFUSING to arm okx LIVE").map(|e| e.message.clone()),
                said(Level::ERROR, "until the kept list holds it").is_some(),
                said(Level::WARN, "whether a spot order can borrow is Unknown").is_some(),
            )
        };
    let mut auto_loan = captured_key_answer();
    auto_loan["data"][0]["autoLoan"] = true.into();
    assert_eq!(
        start(&fx, AccountTier::Live, Ok(captured_key_answer())),
        (None, true, false),
        "the captured account, auto-borrow off, arms"
    );
    let (refused, stopped, _) = start(&fx, AccountTier::Live, Ok(auto_loan.clone()));
    let refused = refused.expect("auto-borrow on is refused");
    for words in ["auto-borrow ON (`autoLoan` true)", "Turn auto-borrow off", "does NOT RUN"] {
        assert!(refused.contains(words), "{words:?} in {refused}");
    }
    assert!(!stopped, "a refused start goes no further");
    assert_eq!(
        start(&fx, AccountTier::Demo, Ok(auto_loan.clone())),
        (None, true, false),
        "a demo start reads nothing and is never refused"
    );
    assert_eq!(
        start(&fx, AccountTier::Live, Err("timed out".to_string())),
        (None, true, true),
        "a failed read arms, warned"
    );
    fx.flags.allow_withdraw_keys = true;
    assert!(
        start(&fx, AccountTier::Live, Ok(auto_loan)).0.is_some(),
        "no flag arms a borrowing account"
    );
}
