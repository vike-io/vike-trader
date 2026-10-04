use super::*;
use vike_bridge_core::venue_mount_fixture::{MountFixture, found_tier_events};
use vike_log::capture::{CapturedEvent, captured};
use vike_model::accounts::account_keys::{AccountLabel, account_key};
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
/// nothing, at either ceiling — and says WHY, with the cause that names the live tier rather than
/// the one that says the store holds nothing.
#[test]
fn a_live_key_set_alone_arms_nothing() {
    let mut fx = MountFixture::new(&[]);
    for (name, v) in live_keys() {
        fx.vars.insert(name, v.to_string());
    }
    for live in [false, true] {
        assert_eq!(
            DeribitVenueMount.resolve(&fx.inputs(live)),
            Resolution::Paper(PaperCause::LiveTierNotWired)
        );
    }
}

/// Only a COMPLETE LIVE pair is a live key set: half of it is absent by the generic loader's own
/// rule, so the original cause stands.
#[test]
fn half_a_live_key_set_is_still_no_credentials() {
    let mut half = MountFixture::new(&[]);
    for (name, v) in live_keys().into_iter().take(1) {
        half.vars.insert(name, v.to_string());
    }
    for live in [false, true] {
        assert_eq!(
            DeribitVenueMount.resolve(&half.inputs(live)),
            Resolution::Paper(PaperCause::NoCredentials)
        );
    }
}

/// The cause is scoped to the ACCOUNT asking, in both directions.
#[test]
fn the_live_tier_cause_is_scoped_to_the_account_that_holds_the_keys() {
    let mut default_live = MountFixture::new(&[]);
    let mut alt_live = MountFixture::new(&[]);
    for (name, v) in live_keys() {
        default_live.vars.insert(name.clone(), v.to_string());
        alt_live.vars.insert(account_key(&name, &alt()), v.to_string());
    }
    alt_live.account = alt();
    assert_eq!(
        DeribitVenueMount.resolve(&default_live.inputs(true)),
        Resolution::Paper(PaperCause::LiveTierNotWired)
    );
    default_live.account = alt();
    assert_eq!(
        DeribitVenueMount.resolve(&default_live.inputs(true)),
        Resolution::Paper(PaperCause::NoCredentials),
        "ALT asks, and only the DEFAULT account holds live keys"
    );
    assert_eq!(
        DeribitVenueMount.resolve(&alt_live.inputs(true)),
        Resolution::Paper(PaperCause::LiveTierNotWired)
    );
    alt_live.account = AccountLabel::Default;
    assert_eq!(
        DeribitVenueMount.resolve(&alt_live.inputs(true)),
        Resolution::Paper(PaperCause::NoCredentials),
        "the DEFAULT account asks, and only ALT holds live keys"
    );
}

/// The stay-paper half at the MOUNT: a LIVE pair alone mounts PAPER, offline, reconcile asked for
/// and not built.
#[test]
fn a_live_key_set_alone_mounts_paper_offline() {
    let mut fx = MountFixture::new(&[]);
    for (name, v) in live_keys() {
        fx.vars.insert(name, v.to_string());
    }
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut req = fx.request(true, "BTC-PERPETUAL", &tx);
    req.recon_enabled = true;
    let out = DeribitVenueMount.mount(req);
    assert!(matches!(out.exec, ExecOutcome::Paper));
    assert!(out.recon.is_none() && out.identity.is_none());
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

// ── what the arm says about a LIVE tier it will not use ─────────────────────────────────────────
//
// Every account label below is unique to its test: the unused-beside-demo `warn!` is said once per
// process per `(venue, account)`, so two tests sharing a label would pass or fail by scheduling order.

/// A distinctive stand-in for a secret, so "no value was logged" is an assertion that can fail.
const SECRET: &str = "SECRET-VALUE-do-not-log";

/// The demo pair AND the live pair, both for `label` (every live value is [`SECRET`]).
fn both_tiers(label: &AccountLabel) -> MountFixture {
    let mut fx = keyed(label);
    for (name, _) in live_keys() {
        fx.vars.insert(account_key(&name, label), SECRET.to_string());
    }
    fx
}

/// `mount_with` over offline doubles: no grid, no reconcile client, a recording exec client.
fn mount_offline(fx: &MountFixture) -> (MountOutcome, Vec<CapturedEvent>) {
    let (tx, _rx) = vike_exec::event_channel(8);
    captured(|| {
        mount_with(
            fx.request(true, "BTC-PERPETUAL", &tx),
            |_, _| None,
            |_, _| None,
            |_, _, _, _| Box::new(vike_exec::testing::RecordingClient::default()),
        )
    })
}

/// A live pair BESIDE the demo one used to mount the testnet in silence. It still mounts the
/// testnet — bound at DEMO, nothing else moves — and now says, once, that the live pair is unused.
#[test]
fn a_live_key_set_beside_the_demo_one_is_named_unused_and_the_mount_is_unchanged() {
    let beside = AccountLabel::parse("BESIDE").expect("a legal label");
    let (out, events) = mount_offline(&both_tiers(&beside));
    let ExecOutcome::Live(exec) = out.exec else { panic!("the demo pair mounts, as it did") };
    assert_eq!(exec.bound_tier, Tier::Demo, "the live pair must not move the bound tier");
    let unused = found_tier_events(&events);
    assert_eq!(unused.len(), 1, "exactly one line about the live pair: {events:?}");
    let e = unused[0];
    assert_eq!(e.level, tracing::Level::WARN, "{e:?}");
    assert_eq!((e.field("venue"), e.field("account")), (Some("deribit"), Some("BESIDE")), "{e:?}");
    assert_eq!(e.field("found_tier"), Some("live"), "{e:?}");
    assert!(e.field("tier").is_none(), "a diagnostic carries `found_tier`, never `tier`: {e:?}");
    assert!(
        events.iter().any(|e| e.field("tier") == Some("demo")),
        "the mount announcement is still said, at the tier it bound: {events:?}"
    );
    assert!(
        events.iter().all(|e| !format!("{e:?}").contains(SECRET)),
        "no credential VALUE may reach a line: {events:?}"
    );
}

/// The demo pair alone — the ordinary case — says nothing new.
#[test]
fn a_demo_pair_alone_adds_no_line_about_the_live_tier() {
    let alone = AccountLabel::parse("ALONE").expect("a legal label");
    let (_, events) = mount_offline(&keyed(&alone));
    assert!(found_tier_events(&events).is_empty(), "{events:?}");
}

/// A live pair with its secret missing is a typo or a half-finished edit, and the loader calls it
/// absent — so it printed `NoCredentials` under the words of an empty store. The mount now names
/// the key it lacks (label-composed, names only) and still lands on paper.
#[test]
fn a_half_written_live_key_set_names_the_missing_key_and_the_mount_is_still_paper() {
    let half = AccountLabel::parse("HALF").expect("a legal label");
    let live = live_keys();
    let mut fx = MountFixture::new(&[]);
    fx.account = half.clone();
    fx.vars.insert(account_key(&live[0].0, &half), SECRET.to_string());
    let missing = account_key(&live[1].0, &half);
    let (tx, _rx) = vike_exec::event_channel(8);
    let (out, events) = captured(|| {
        mount_with(
            fx.request(true, "BTC-PERPETUAL", &tx),
            |_, _| panic!("no demo pair, so no grid fetch"),
            |_, _| panic!("no demo pair, so no reconcile client"),
            |_, _, _, _| panic!("no demo pair, so no exec client"),
        )
    });
    assert!(matches!(out.exec, ExecOutcome::Paper));
    let said = found_tier_events(&events);
    assert_eq!(said.len(), 1, "{events:?}");
    let e = said[0];
    assert_eq!(e.level, tracing::Level::ERROR, "{e:?}");
    assert_eq!((e.field("venue"), e.field("account")), (Some("deribit"), Some("HALF")), "{e:?}");
    assert!(e.message.contains(&missing), "names the missing key `{missing}`: {}", e.message);
    let present = account_key(&live[0].0, &half);
    assert!(!e.message.contains(&present), "names only what is MISSING: {}", e.message);
    assert!(!format!("{e:?}").contains(SECRET), "no credential VALUE: {e:?}");
    // The CAUSE the arming screen prints is unchanged: a half-written pair is `NoCredentials`.
    assert_eq!(
        DeribitVenueMount.resolve(&fx.inputs(true)),
        Resolution::Paper(PaperCause::NoCredentials)
    );
}

/// A LABELLED account is mounted only when it armed, so one holding only live keys never reaches
/// `mount` and said nothing at all — `vike-mount` asks the arm to speak for it instead, and the arm
/// says exactly what `mount` says for the default account.
#[test]
fn an_account_that_is_never_mounted_is_spoken_for_in_the_same_words() {
    let unmounted = AccountLabel::parse("UNMNT").expect("a legal label");
    let mut live_only = MountFixture::new(&[]);
    live_only.account = unmounted.clone();
    for (name, v) in live_keys() {
        live_only.vars.insert(account_key(&name, &unmounted), v.to_string());
    }
    let (_, events) =
        captured(|| DeribitVenueMount.report_unmounted_account(&live_only.inputs(true)));
    let said = found_tier_events(&events);
    assert_eq!(said.len(), 1, "{events:?}");
    assert_eq!(said[0].level, tracing::Level::ERROR, "{events:?}");
    assert_eq!(said[0].field("account"), Some("UNMNT"), "{events:?}");

    let mut half = MountFixture::new(&[]);
    half.account = unmounted.clone();
    half.vars.insert(account_key(&live_keys()[0].0, &unmounted), "x".to_string());
    let (_, events) = captured(|| DeribitVenueMount.report_unmounted_account(&half.inputs(true)));
    assert_eq!(found_tier_events(&events).len(), 1, "a half-written pair is named too: {events:?}");

    let (_, events) = captured(|| {
        DeribitVenueMount.report_unmounted_account(&MountFixture::new(&[]).inputs(true));
    });
    assert!(events.is_empty(), "an empty store is the ordinary state and is silent: {events:?}");
}
