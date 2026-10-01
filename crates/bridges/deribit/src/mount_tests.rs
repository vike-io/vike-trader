use super::*;
use vike_bridge_core::venue_mount_fixture::MountFixture;
use vike_model::account_keys::{AccountLabel, account_key};
use vike_model::credential_keys::{API_KEY_SUFFIX, API_SECRET_SUFFIX, key_owner, starter_keys};

const KEYS: &[(&str, &str)] = &[("DERIBIT_DEMO_API_KEY", "k"), ("DERIBIT_DEMO_API_SECRET", "s")];

/// This venue's LIVE-tier key pair, looked up in the credential grid's own table
/// (`vike_model::credential_keys`' `starter_keys` and `key_owner`) rather than composed here:
/// `crates/vike-ops/tests/settings_registry.rs`'s `generated_key_sites` reads ANY call to one of
/// the grid's builders — test modules included, since the gate folds a `#[path]` test module into
/// the file that declares it — as this crate reading the WHOLE grid, and then demands a registry
/// row under this crate for every name in it. (Nor are the names spelled out: the ports write no
/// LIVE-tier credential literal in a bridge's `src/`.)
fn live_keys() -> Vec<(String, &'static str)> {
    let live_tier = Some(Environment::Live.as_str());
    [(API_KEY_SUFFIX, "k"), (API_SECRET_SUFFIX, "s")]
        .into_iter()
        .map(|(suffix, v)| {
            let name = starter_keys(VENUE)
                .into_iter()
                .find(|k| k.ends_with(suffix) && key_owner(k) == Some((VENUE, live_tier)))
                .expect("the credential grid names this venue's LIVE-tier key");
            (name, v)
        })
        .collect()
}

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}

fn keyed(label: &AccountLabel) -> MountFixture {
    let mut fx = MountFixture::new(&[]);
    for (k, v) in KEYS {
        fx.vars.insert(account_key(k, label), (*v).to_string());
    }
    fx.account = label.clone();
    fx
}

/// The REAL body the testnet answered, captured from the CI box on 2026-08-09 (moved from
/// `vike-mount`, whose `fixture()` helper read it).
fn fixture() -> serde_json::Value {
    serde_json::from_str(include_str!("../tests/fixtures/server_time/deribit.json"))
        .expect("the captured fixture is valid JSON")
}

#[test]
fn the_declaration_is_the_rows_the_mount_crate_carried() {
    let d = DeribitVenueMount.declaration();
    assert_eq!(DeribitVenueMount.venue(), "deribit");
    assert!(d.addresses_accounts && d.process_exclusive.is_none() && !d.takes_recon_trigger);
    assert_eq!(d.grid_source, DeclaredGridSource::PerSymbolFetch);
    assert!(matches!(d.book_identity, BookIdentity::Undeterminable { .. }));
    assert_eq!(
        d.clock,
        ClockDecl::Wired {
            endpoint: "GET /api/v2/public/get_time (public, testnet host)",
            auth: ClockAuth::Public,
            risk: ClockRisk::NoTimestamp,
        }
    );
}

/// The arming row as it was: switchless, so a key set arms the DEMO tier and nothing else.
#[test]
fn resolve_is_the_arms_own_gate() {
    for live in [false, true] {
        assert_eq!(
            DeribitVenueMount.resolve(&MountFixture::new(&[]).inputs(live)),
            Resolution::Paper(PaperCause::NoCredentials)
        );
        assert_eq!(
            DeribitVenueMount.resolve(&keyed(&AccountLabel::Default).inputs(live)),
            Resolution::Armed {
                tier: Tier::Demo,
                held_below_live: Some(HeldBelowLive::DemoOnlyArm)
            }
        );
    }
}

/// Review Focus 2 at this venue: no ceiling makes this arm resolve `Live`, even with a LIVE key set
/// present beside the DEMO one — it has no live arm.
#[test]
fn a_ceiling_below_live_never_resolves_live() {
    let mut fx = keyed(&AccountLabel::Default);
    for (name, v) in live_keys() {
        fx.vars.insert(name, v.to_string());
    }
    for live in [false, true] {
        assert!(!matches!(
            DeribitVenueMount.resolve(&fx.inputs(live)),
            Resolution::Armed { tier: Tier::Live, .. }
        ));
    }
}

/// The DEMO pair is the only key set the arm reads: a store holding the LIVE pair ALONE arms
/// nothing, at either ceiling.
#[test]
fn a_live_key_set_alone_arms_nothing() {
    let mut fx = MountFixture::new(&[]);
    for (name, v) in live_keys() {
        fx.vars.insert(name, v.to_string());
    }
    for live in [false, true] {
        assert_eq!(
            DeribitVenueMount.resolve(&fx.inputs(live)),
            Resolution::Paper(PaperCause::NoCredentials)
        );
    }
}

#[test]
fn a_labelled_account_reads_only_its_own_keys() {
    let mut alt_asks_default_keys = keyed(&AccountLabel::Default);
    alt_asks_default_keys.account = alt();
    assert_eq!(
        DeribitVenueMount.resolve(&alt_asks_default_keys.inputs(true)),
        Resolution::Paper(PaperCause::NoCredentials)
    );
    assert!(matches!(
        DeribitVenueMount.resolve(&keyed(&alt()).inputs(true)),
        Resolution::Armed { .. }
    ));
    let mut default_asks_alt_keys = keyed(&alt());
    default_asks_alt_keys.account = AccountLabel::Default;
    assert_eq!(
        DeribitVenueMount.resolve(&default_asks_alt_keys.inputs(true)),
        Resolution::Paper(PaperCause::NoCredentials)
    );
}

#[test]
fn with_no_credentials_the_mount_is_paper_and_offline() {
    let fx = MountFixture::new(&[]);
    let (tx, _rx) = vike_exec::event_channel(8);
    let out = DeribitVenueMount.mount(fx.request(true, "BTC-PERPETUAL", &tx));
    assert!(matches!(out.exec, ExecOutcome::Paper));
    assert!(out.recon.is_none() && out.identity.is_none());
}

// ---- the clock PARSE, against the real captured body (moved from vike-mount) ---------------

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
