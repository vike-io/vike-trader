use std::sync::{Mutex, MutexGuard, PoisonError};

use tracing::Level;

use super::*;
use vike_bridge_core::venue_mount_fixture::MountFixture;
use vike_log::capture::captured;
use vike_model::accounts::account_keys::{AccountLabel, account_key};
use vike_model::credential_keys::{
    API_KEY_SUFFIX, API_PASSPHRASE_SUFFIX, API_SECRET_SUFFIX, key_owner, starter_keys,
};

/// The DEMO trio `load_credentials_for_account` reads for this venue — the passphrase is required
/// (`vike_bridge_core::venue_passphrase`'s `Required` row). Spelled out: these are the names an
/// operator writes.
fn demo() -> Vec<(String, String)> {
    [("OKX_DEMO_API_KEY", "dk"), ("OKX_DEMO_API_SECRET", "ds"), ("OKX_DEMO_API_PASSPHRASE", "dp")]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// The LIVE trio (key, secret, passphrase, in that order), looked up in the credential grid's own
/// table (`vike_model::credential_keys`' `starter_keys` and `key_owner`). Not spelled out, by the
/// venue ports' convention that a bridge's `src/` holds no LIVE-tier credential literal — a
/// convention rather than a gate here: `every_read_variable_is_declared` asks only that a spelled
/// name be declared somewhere, and the `vike-bridge-core` grid rows declare `OKX_LIVE_API_*`. Not
/// composed either: `crates/vike-ops/tests/settings/settings_registry/walk_and_grid.rs`'s `generated_key_sites` reads any
/// call to one of the grid's builders — test modules included, since it folds a `#[path]` test
/// module into the file that declares it — as this crate reading the WHOLE grid, and would demand
/// a registry row under `bridges/okx` for every name in it.
fn live() -> Vec<(String, String)> {
    let tier = Some(Environment::Live.as_str());
    [(API_KEY_SUFFIX, "lk"), (API_SECRET_SUFFIX, "ls"), (API_PASSPHRASE_SUFFIX, "lp")]
        .into_iter()
        .map(|(suffix, v)| {
            let name = starter_keys(VENUE)
                .into_iter()
                .find(|k| k.ends_with(suffix) && key_owner(k) == Some((VENUE, tier)))
                .expect("the credential grid names this venue's LIVE-tier key");
            (name, v.to_string())
        })
        .collect()
}

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}

/// A fixture holding every pair of `pairs` under `label`'s key names, asking for `label`.
fn keyed(label: &AccountLabel, pairs: &[(String, String)]) -> MountFixture {
    let mut fx = MountFixture::new(&[]);
    for (k, v) in pairs {
        fx.vars.insert(account_key(k, label), v.clone());
    }
    fx.account = label.clone();
    fx
}

/// The REAL body okx answered, captured from the CI box on 2026-08-09 (copied from `vike-mount`, whose
/// clock tests read the same capture).
fn clock_body() -> serde_json::Value {
    serde_json::from_str(include_str!("../tests/fixtures/server_time/okx.json"))
        .expect("the captured fixture is valid JSON")
}

/// Moved verbatim from `crates/vike-mount/src/recon.rs`'s `recon_client_tests`.
fn demo_creds() -> Credentials {
    Credentials {
        api_key: "test-key".to_string(),
        api_secret: "test-secret".to_string(),
        passphrase: Some("test-pass".to_string()),
    }
}

/// Every test that calls `OkxVenueMount::mount` takes a turn, because the two log lines
/// `the_half_credential_report_names_the_passphrase_of_the_ceilings_tier` captures live there.
/// `tracing` caches an `Interest` per callsite, process-wide, and while a capture is the only live
/// dispatcher a thread with no subscriber that reaches a line FIRST fixes it at `never` for every
/// thread. ⚠ The turn is no longer what makes those captures hold — `vike_log::capture::captured`
/// does, against a sibling running the lines uncaptured at ANY time (its module doc carries the
/// mechanism and the anchor). What the turn still closes is that capture's one declared residual:
/// a first registration racing the process's first capture. (A runner with a process per test,
/// like nextest, never shares the cache; `cargo test` runs the tests as threads of one process.)
static MOUNT: Mutex<()> = Mutex::new(());

fn mount_turn() -> MutexGuard<'static, ()> {
    // A panicking test poisons the lock; what it guards is `tracing`'s cache, which a panic does
    // not corrupt.
    MOUNT.lock().unwrap_or_else(PoisonError::into_inner)
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

/// `fx`'s mount under `ceiling_live` — which must be an offline PAPER mount — and every event
/// `mount.rs` emitted during it (the parent module's target: this module's path, one segment up).
fn paper_mount_events(fx: &MountFixture, ceiling_live: bool) -> Vec<Captured> {
    let (tx, _rx) = vike_exec::event_channel(8);
    let (out, events) =
        captured(|| OkxVenueMount.mount(fx.request(ceiling_live, "BTC-USDT-SWAP", &tx)));
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

/// The rows `vike-mount`'s tables carried for okx, pinned as a matrix.
#[test]
fn the_declaration_is_the_rows_the_mount_crate_carried() {
    let d = OkxVenueMount.declaration();
    assert_eq!(OkxVenueMount.venue(), "okx");
    assert!(d.addresses_accounts && d.process_exclusive.is_none());
    assert!(d.takes_recon_trigger, "the resync supervisor pokes the reconcile driver");
    assert_eq!(d.grid_source, DeclaredGridSource::PerSymbolFetch);
    assert!(matches!(d.book_identity, BookIdentity::Undeterminable { .. }));
    assert_eq!(
        d.clock,
        ClockDecl::Wired {
            endpoint: "GET /api/v5/public/time (public)",
            auth: ClockAuth::Public,
            risk: ClockRisk::SignedTimestamp,
        }
    );
}

/// THE D1 MATRIX (docs/decisions/0095): the ceiling alone picks the key tier, and a `live` ceiling
/// with no LIVE trio is `LiveCredentialsAbsent` whatever the DEMO trio holds.
#[test]
fn resolve_reads_exactly_the_tier_the_ceiling_names() {
    let no_credentials = Resolution::Paper(PaperCause::NoCredentials);
    let no_live_trio = Resolution::Paper(PaperCause::LiveCredentialsAbsent);
    let empty = MountFixture::new(&[]);
    let demo_only = keyed(&AccountLabel::Default, &demo());
    let live_only = keyed(&AccountLabel::Default, &live());
    assert_eq!(OkxVenueMount.resolve(&empty.inputs(false)), no_credentials);
    assert_eq!(OkxVenueMount.resolve(&empty.inputs(true)), no_live_trio);
    assert_eq!(
        OkxVenueMount.resolve(&demo_only.inputs(false)),
        Resolution::Armed { tier: Tier::Demo, held_below_live: None }
    );
    assert_eq!(
        OkxVenueMount.resolve(&demo_only.inputs(true)),
        no_live_trio,
        "a live ceiling never falls back to the demo trio"
    );
    assert_eq!(
        OkxVenueMount.resolve(&live_only.inputs(true)),
        Resolution::Armed { tier: Tier::Live, held_below_live: None }
    );
    assert_eq!(
        OkxVenueMount.resolve(&live_only.inputs(false)),
        no_credentials,
        "a lower ceiling never reads the live trio"
    );
}

/// Review Focus 2 at this venue: with BOTH trios present, a ceiling below `live` still resolves
/// the DEMO tier and never `Live`.
#[test]
fn a_ceiling_below_live_never_resolves_live() {
    let both = keyed(&AccountLabel::Default, &[demo(), live()].concat());
    assert!(!matches!(
        OkxVenueMount.resolve(&both.inputs(false)),
        Resolution::Armed { tier: Tier::Live, .. }
    ));
}

/// THE HALF-CREDENTIAL CASE (the pure half of
/// `crates/vike-tradehub/tests/mount_roster/preconnect.rs`'s
/// `okx_key_and_secret_without_a_passphrase_do_not_mount_live`, which drives the real
/// `make_engine` over the same shape): key and secret without the passphrase okx's signer requires
/// are NO key set — the arming row of an absent trio at each ceiling, paper, and offline. The
/// contract has no cause of its own for it; the `error!` naming the missing variable is `mount`'s.
#[test]
fn a_key_and_secret_without_a_passphrase_is_no_key_set() {
    let _turn = mount_turn();
    let (tx, _rx) = vike_exec::event_channel(8);
    let half_demo = keyed(&AccountLabel::Default, &demo()[..2]);
    let half_live = keyed(&AccountLabel::Default, &live()[..2]);
    for (fx, ceiling_live, cause) in [
        (&half_demo, false, PaperCause::NoCredentials),
        (&half_live, true, PaperCause::LiveCredentialsAbsent),
    ] {
        assert_eq!(OkxVenueMount.resolve(&fx.inputs(ceiling_live)), Resolution::Paper(cause));
        let out = OkxVenueMount.mount(fx.request(ceiling_live, "BTC-USDT-SWAP", &tx));
        assert!(matches!(out.exec, ExecOutcome::Paper));
        assert!(out.recon.is_none() && out.identity.is_none());
        assert!(OkxVenueMount.credential_probe(&fx.inputs(ceiling_live)).is_none());
    }
}

/// THE HALF-CREDENTIAL REPORT, captured. Once okx's row flips this is the only site that can emit
/// it — okx is `vike_bridge_core::venue_passphrase`'s one `Required` row — and a test that stops at
/// the paper outcome cannot see it go. Key and secret without the passphrase name the passphrase
/// variable of the CEILING's tier at `error!`; under a `live` ceiling the no-LIVE-trio warning
/// follows it, in the legacy prefix's order. A complete trio names nothing: the report reads the
/// MOUNTING account's names, so a complete default trio is driven here for a labelled account with
/// no keys of its own (an absent account, which reports nothing), which keeps that mount paper and
/// offline. The labelled account's own half trio is
/// `a_labelled_accounts_half_credential_report_names_its_own_passphrase_key`'s.
#[test]
fn the_half_credential_report_names_the_passphrase_of_the_ceilings_tier() {
    let _turn = mount_turn();
    let (demo, live) = (demo(), live());
    let is_report_naming = |event: &Captured, name: &str| {
        event.level == Level::ERROR
            && event.venue.as_deref() == Some(VENUE)
            && event.message.starts_with(&format!("{name} is unset or blank, but {VENUE} REQUIRES"))
    };

    let at_demo = paper_mount_events(&keyed(&AccountLabel::Default, &demo[..2]), false);
    assert_eq!(at_demo.len(), 1, "{at_demo:?}");
    assert!(is_report_naming(&at_demo[0], &demo[2].0), "{at_demo:?}");
    assert!(at_demo[0].message.contains(&format!("`vike-cli secrets set {}`", demo[2].0)));
    assert!(!at_demo[0].message.contains("secrets.env"), "{at_demo:?}");

    let at_live = paper_mount_events(&keyed(&AccountLabel::Default, &live[..2]), true);
    assert_eq!(at_live.len(), 2, "{at_live:?}");
    assert!(is_report_naming(&at_live[0], &live[2].0), "{at_live:?}");
    assert_eq!(at_live[1].level, Level::WARN, "{at_live:?}");
    assert_eq!(at_live[1].venue.as_deref(), Some(VENUE), "{at_live:?}");
    assert_eq!(at_live[1].message, format!("{VENUE}: {NO_LIVE_CREDENTIALS}"));

    for (trio, ceiling_live) in [(&demo, false), (&live, true)] {
        let mut complete = keyed(&AccountLabel::Default, trio);
        complete.account = alt();
        let events = paper_mount_events(&complete, ceiling_live);
        assert!(events.iter().all(|e| e.level != Level::ERROR), "{events:?}");
    }
}

/// THE REPORT IS ABOUT THE ACCOUNT BEING MOUNTED. A labelled account with key and secret but no
/// passphrase of its own is reported under ITS key name (`…_API_PASSPHRASE__ALT` — the variable the
/// operator has to write), at each ceiling's tier, and the line names the store verb that writes it
/// rather than a file a migrated box does not read. Read through the default account's names it
/// found nothing to say here, because the default account holds no keys at all.
#[test]
fn a_labelled_accounts_half_credential_report_names_its_own_passphrase_key() {
    let _turn = mount_turn();
    let (demo, live) = (demo(), live());
    for (trio, ceiling_live) in [(&demo, false), (&live, true)] {
        let want = account_key(&trio[2].0, &alt());
        let events = paper_mount_events(&keyed(&alt(), &trio[..2]), ceiling_live);
        let reports: Vec<&Captured> = events.iter().filter(|e| e.level == Level::ERROR).collect();
        assert_eq!(reports.len(), 1, "{events:?}");
        let report = reports[0];
        assert_eq!(report.venue.as_deref(), Some(VENUE), "{report:?}");
        assert!(
            report.message.starts_with(&format!("{want} is unset or blank, but {VENUE} REQUIRES")),
            "{report:?}"
        );
        assert!(
            report.message.contains(&format!("`vike-cli secrets set {want}`")),
            "the line names the store verb that writes the key: {report:?}"
        );
        assert!(
            !report.message.contains("secrets.env"),
            "a migrated box does not read that file: {report:?}"
        );
    }
}

/// The other direction: a labelled account whose passphrase sits under its OWN key name is a
/// complete trio and arms — and the DEFAULT account's half trio next to it is not its finding. A
/// report read through the default account's names named the default account's passphrase on
/// every mount of the labelled one.
#[test]
fn a_labelled_account_with_its_own_passphrase_is_not_reported_for_the_default_accounts() {
    let (demo, live) = (demo(), live());
    for (trio, ceiling_live) in [(&demo, false), (&live, true)] {
        let mut fx = keyed(&alt(), trio);
        for (k, v) in &trio[..2] {
            fx.vars.insert(k.clone(), v.clone());
        }
        let inputs = fx.inputs(ceiling_live);
        assert!(
            matches!(OkxVenueMount.resolve(&inputs), Resolution::Armed { .. }),
            "precondition: the labelled account's trio is complete"
        );
        assert_eq!(OkxVenueMount::missing_passphrase(&inputs), None);
        // …and the same store mounted AS the default account is the half trio, named unlabelled.
        let mut as_default = fx;
        as_default.account = AccountLabel::Default;
        assert_eq!(
            OkxVenueMount::missing_passphrase(&as_default.inputs(ceiling_live)),
            Some(trio[2].0.clone())
        );
    }
}

/// The account 2×2: a labelled account reads its OWN trio and never the default account's.
#[test]
fn a_labelled_account_reads_only_its_own_keys() {
    let mut alt_asks_default_keys = keyed(&AccountLabel::Default, &demo());
    alt_asks_default_keys.account = alt();
    assert_eq!(
        OkxVenueMount.resolve(&alt_asks_default_keys.inputs(false)),
        Resolution::Paper(PaperCause::NoCredentials),
        "ALT must not arm off the default account's keys"
    );
    assert!(matches!(
        OkxVenueMount.resolve(&keyed(&alt(), &demo()).inputs(false)),
        Resolution::Armed { .. }
    ));
    let mut default_asks_alt_keys = keyed(&alt(), &demo());
    default_asks_alt_keys.account = AccountLabel::Default;
    assert_eq!(
        OkxVenueMount.resolve(&default_asks_alt_keys.inputs(false)),
        Resolution::Paper(PaperCause::NoCredentials)
    );
}

/// No key set for the ceiling's tier: paper, nothing to reconcile, nothing recorded, OFFLINE — and
/// no startup credential probe either. The LIVE-only store under a `demo` ceiling is the row that
/// pins the TIER on both legs: a mount or a probe that read the LIVE trio there would arm on the
/// real-money account under a ceiling that names demo (the probe's `bound_tier` label alone, as
/// `a_demo_ceiling_never_signs_against_the_real_money_tier` asserts it, cannot tell).
#[test]
fn without_a_key_set_for_the_ceilings_tier_the_mount_is_paper_and_offline() {
    let _turn = mount_turn();
    let (tx, _rx) = vike_exec::event_channel(8);
    let empty = MountFixture::new(&[]);
    let demo_only = keyed(&AccountLabel::Default, &demo());
    let live_only = keyed(&AccountLabel::Default, &live());
    for (fx, ceiling_live) in
        [(&empty, false), (&empty, true), (&demo_only, true), (&live_only, false)]
    {
        let out = OkxVenueMount.mount(fx.request(ceiling_live, "BTC-USDT-SWAP", &tx));
        assert!(matches!(out.exec, ExecOutcome::Paper));
        assert!(out.recon.is_none() && out.identity.is_none());
        assert!(OkxVenueMount.credential_probe(&fx.inputs(ceiling_live)).is_none());
    }
}

/// THE TIER at the one leg that SIGNS — the property `vike-mount`'s `startup_tests.rs` pinned for
/// this venue under this name until `probe_mainnet` left with this port: with BOTH trios present,
/// the probe binds the tier the ceiling names. Pure — the reconcile client is a signer plus a
/// transport, scoped with `FALLBACK_CTVAL`.
#[test]
fn a_demo_ceiling_never_signs_against_the_real_money_tier() {
    let both = keyed(&AccountLabel::Default, &[demo(), live()].concat());
    assert!(matches!(
        OkxVenueMount.credential_probe(&both.inputs(false)),
        Some(CredentialProbe::RecordsIdentity { bound_tier: Tier::Demo, .. })
    ));
    assert!(matches!(
        OkxVenueMount.credential_probe(&both.inputs(true)),
        Some(CredentialProbe::RecordsIdentity { bound_tier: Tier::Live, .. })
    ));
}

/// Moved from `crates/vike-mount/src/recon.rs`'s `recon_client_tests` (its okx row and the okx
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

// ---- the clock PARSE, against the real captured body (moved from vike-mount) ---------------

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
