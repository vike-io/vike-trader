use std::assert_matches;
use std::sync::{Mutex, MutexGuard, PoisonError};

use tracing::Level;

use super::*;
use vike_bridge_core::venue_adapter_fixture::AdapterFixture;
use vike_log::capture::captured;
use vike_model::accounts::account_keys::AccountLabel;
use vike_venues::credential_keys::{API_KEY, API_PASSPHRASE, API_SECRET};

/// A fixture whose started account holds the complete trio — the passphrase is required
/// (`vike_secrets::venue_passphrase`'s `Required` row). The account's fields, by the field words
/// `api_credentials` reads (decision 0156 item 6: no name is composed, so none is spelled here).
fn trio() -> AdapterFixture {
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

/// Moved verbatim from `crates/vike-tradehub-core/src/recon.rs`'s `recon_client_tests`.
fn demo_creds() -> Credentials {
    Credentials {
        api_key: "test-key".to_string().into(),
        api_secret: "test-secret".to_string().into(),
        passphrase: Some("test-pass".to_string().into()),
    }
}

/// Every test that calls `OkxVenueAdapter::start` takes a turn, because the two log lines
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
    let (tx, _rx) = vike_exec::event_channel(8);
    let (out, events) =
        captured(|| OkxVenueAdapter.start(fx.request(ceiling_live, "BTC-USDT-SWAP", &tx)));
    assert!(matches!(out.exec, ExecOutcome::Paper), "every mount captured here is paper");
    let target = module_path!().rsplit_once("::").map_or("", |(parent, _)| parent);
    events
        .into_iter()
        .map(|e| Captured {
            venue: e.field("venue").map(str::to_string),
            level: e.level,
            target: e.target,
            message: e.message,
        })
        .filter(|e| e.target == target)
        .collect()
}

/// The rows `vike-tradehub-core`'s tables carried for okx, pinned as a matrix.
#[test]
fn the_declaration_is_the_rows_the_tradehub_core_carried() {
    let d = OkxVenueAdapter.declaration();
    assert_eq!(OkxVenueAdapter.venue(), "okx");
    assert!(d.addresses_accounts && d.process_exclusive.is_none());
    assert!(d.takes_recon_trigger, "the resync supervisor pokes the reconcile driver");
    assert_eq!(d.grid_source, DeclaredGridSource::PerSymbolFetch);
    assert_matches!(d.book_identity, BookIdentity::Undeterminable { .. });
    assert_eq!(
        d.clock,
        ClockDecl::Wired {
            endpoint: "GET /api/v5/public/time (public)",
            auth: ClockAuth::Public,
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
        OkxVenueAdapter.resolve(&empty.inputs(AccountTier::Demo)),
        Resolution::Paper(PaperCause::NoCredentials)
    );
    assert_eq!(
        OkxVenueAdapter.resolve(&empty.inputs(AccountTier::Live)),
        Resolution::Paper(PaperCause::LiveCredentialsAbsent)
    );
    assert_eq!(
        OkxVenueAdapter.resolve(&trio().inputs(AccountTier::Demo)),
        Resolution::Armed { tier: AccountTier::Demo, held_below_live: None }
    );
    assert_eq!(
        OkxVenueAdapter.resolve(&trio().inputs(AccountTier::Live)),
        Resolution::Armed { tier: AccountTier::Live, held_below_live: None }
    );
}

/// Review Focus 2 at this venue: a complete trio under a tier below `live` resolves the DEMO tier
/// and never `Live` — `Paper` reads as `Demo`.
#[test]
fn a_ceiling_below_live_never_resolves_live() {
    for below in [AccountTier::Demo, AccountTier::Paper] {
        assert_eq!(
            OkxVenueAdapter.resolve(&trio().inputs(below)),
            Resolution::Armed { tier: AccountTier::Demo, held_below_live: None },
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
    let (tx, _rx) = vike_exec::event_channel(8);
    let half = half_trio();
    for (ceiling_live, cause) in [
        (AccountTier::Demo, PaperCause::NoCredentials),
        (AccountTier::Live, PaperCause::LiveCredentialsAbsent),
    ] {
        assert_eq!(OkxVenueAdapter.resolve(&half.inputs(ceiling_live)), Resolution::Paper(cause));
        let out = OkxVenueAdapter.start(half.request(ceiling_live, "BTC-USDT-SWAP", &tx));
        assert!(matches!(out.exec, ExecOutcome::Paper));
        assert!(out.recon.is_none() && out.identity.is_none());
        assert!(OkxVenueAdapter.credential_probe(&half.inputs(ceiling_live)).is_none());
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
    assert!(!at_demo[0].message.contains("secrets.env"), "{at_demo:?}");

    let at_live = paper_start_events(&half_trio(), AccountTier::Live);
    assert_eq!(at_live.len(), 2, "{at_live:?}");
    assert!(is_report_naming(&at_live[0], API_PASSPHRASE), "{at_live:?}");
    assert_eq!(at_live[1].level, Level::WARN, "{at_live:?}");
    assert_eq!(at_live[1].venue.as_deref(), Some(VENUE), "{at_live:?}");
    assert_eq!(at_live[1].message, format!("{VENUE}: {NO_LIVE_CREDENTIALS}"));

    for ceiling_live in [AccountTier::Demo, AccountTier::Live] {
        let events = paper_start_events(&AdapterFixture::new(&[]), ceiling_live);
        assert!(events.iter().all(|e| e.level != Level::ERROR), "{events:?}");
        assert_eq!(OkxVenueAdapter::missing_passphrase(&trio().inputs(ceiling_live)), None);
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
        assert!(
            !report.message.contains("secrets.env"),
            "a migrated box does not read that file: {report:?}"
        );
        assert_eq!(
            OkxVenueAdapter::missing_passphrase(&as_account(trio(), alt()).inputs(ceiling_live)),
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
        OkxVenueAdapter
            .resolve(&as_account(AdapterFixture::new(&[]), alt()).inputs(AccountTier::Demo)),
        Resolution::Paper(PaperCause::NoCredentials),
        "ALT without keys of its own is paper"
    );
    assert_matches!(
        OkxVenueAdapter.resolve(&as_account(trio(), alt()).inputs(AccountTier::Demo)),
        Resolution::Armed { .. }
    );
}

/// No complete trio: paper, nothing to reconcile, nothing recorded, OFFLINE — and no startup
/// credential probe either.
#[test]
fn without_a_key_set_for_the_ceilings_tier_the_start_is_paper_and_offline() {
    let _turn = start_turn();
    let (tx, _rx) = vike_exec::event_channel(8);
    let empty = AdapterFixture::new(&[]);
    for ceiling_live in [AccountTier::Demo, AccountTier::Live] {
        let out = OkxVenueAdapter.start(empty.request(ceiling_live, "BTC-USDT-SWAP", &tx));
        assert!(matches!(out.exec, ExecOutcome::Paper));
        assert!(out.recon.is_none() && out.identity.is_none());
        assert!(OkxVenueAdapter.credential_probe(&empty.inputs(ceiling_live)).is_none());
    }
}

/// THE TIER at the one leg that SIGNS — the property `vike-tradehub-core`'s `startup_tests.rs` pinned for
/// this venue under this name until `probe_mainnet` left with this port: the probe binds the
/// account's own tier, so a `demo` account never signs against the real-money network. Pure — the
/// reconcile client is a signer plus a transport, scoped with `FALLBACK_CTVAL`.
#[test]
fn a_demo_ceiling_never_signs_against_the_real_money_tier() {
    assert!(matches!(
        OkxVenueAdapter.credential_probe(&trio().inputs(AccountTier::Demo)),
        Some(CredentialProbe::RecordsIdentity { bound_tier: AccountTier::Demo, .. })
    ));
    assert!(matches!(
        OkxVenueAdapter.credential_probe(&trio().inputs(AccountTier::Live)),
        Some(CredentialProbe::RecordsIdentity { bound_tier: AccountTier::Live, .. })
    ));
}

/// Moved from `crates/vike-tradehub-core/src/recon.rs`'s `recon_client_tests` (its okx row and the okx
/// line of its armed-mainnet row): the reconcile handle builds network-free at both tiers, with the
/// fallback `ctVal` the probe uses.
#[test]
fn credentialed_okx_yields_a_recon_client_at_both_tiers() {
    for mainnet in [false, true] {
        assert!(
            crate::recon_client::recon_client(
                &demo_creds(),
                "BTC-USDT-SWAP",
                FALLBACK_CTVAL,
                mainnet
            )
            .is_some(),
            "mainnet={mainnet}"
        );
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
