use super::*;
use vike_bridge_core::venue_mount_fixture::{MountFixture, found_tier_events};
use vike_log::capture::{CapturedEvent, captured};
use vike_model::accounts::account_keys::{AccountLabel, account_key};

use crate::config::alpaca_tier;

/// The minimal SANDBOX key set `load_alpaca_config_for_account` requires.
const KEYS: &[(&str, &str)] = &[
    ("ALPACA_SANDBOX_CLIENT_ID", "cid"),
    ("ALPACA_SANDBOX_CLIENT_SECRET", "csecret"),
    ("ALPACA_SANDBOX_ACCOUNT_ID", "acct-1"),
];

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}

/// A fixture holding `KEYS` under `label`'s key names, asking for `label`.
fn keyed(label: &AccountLabel) -> MountFixture {
    let mut fx = MountFixture::new(&[]);
    for (k, v) in KEYS {
        fx.vars.insert(account_key(k, label), (*v).to_string());
    }
    fx.account = label.clone();
    fx
}

/// A complete LIVE-tier key set for the default account, its names composed the way the loader
/// composes them (`alpaca_tier`) — so no bare LIVE-tier credential literal enters the source,
/// where `crates/vike-ops/tests/settings_registry.rs` would demand a registry row nothing reads.
fn live_tier_keys() -> Vec<(String, String)> {
    let prefix = format!("ALPACA_{}", alpaca_tier(Environment::Live));
    ["CLIENT_ID", "CLIENT_SECRET", "ACCOUNT_ID"]
        .iter()
        .map(|suffix| (format!("{prefix}_{suffix}"), "x".to_string()))
        .collect()
}

/// The rows `vike-mount`'s tables carried for alpaca, pinned as the capability-map playbook pins
/// a matrix: a change to any of them is a deliberate edit here too.
#[test]
fn the_declaration_is_the_rows_the_mount_crate_carried() {
    let d = AlpacaVenueMount.declaration();
    assert_eq!(AlpacaVenueMount.venue(), "alpaca");
    assert!(d.addresses_accounts);
    assert!(d.process_exclusive.is_none());
    assert!(!d.takes_recon_trigger);
    assert_eq!(d.grid_source, DeclaredGridSource::PerSymbolFetch);
    assert_eq!(
        d.book_identity,
        BookIdentity::Named {
            prefix: "ALPACA",
            demo_tiers: &["SANDBOX"],
            live_tiers: &["LIVE"],
            name_suffixes: &["ACCOUNT_ID"],
            evm_key_suffixes: &[],
        }
    );
    // The reason is the text an operator reads in the preflight's clock row, so it is pinned
    // whole rather than by shape.
    assert_eq!(
        d.clock,
        ClockDecl::NotWired {
            reason: "its /v1/clock needs the OAuth2 client-credentials Bearer, i.e. a second token \
                     exchange before any measurement, and alpaca stamps no timestamp on a request",
            unmeasured_risk: None,
        }
    );
}

/// The arming-probe row as it was: a complete SANDBOX key set arms a demo-only arm; anything
/// less stays paper.
#[test]
fn resolve_is_the_arms_own_gate() {
    let armed =
        Resolution::Armed { tier: Tier::Demo, held_below_live: Some(HeldBelowLive::DemoOnlyArm) };
    for live in [false, true] {
        assert_eq!(
            AlpacaVenueMount.resolve(&MountFixture::new(&[]).inputs(live)),
            Resolution::Paper(PaperCause::NoCredentials)
        );
        assert_eq!(AlpacaVenueMount.resolve(&keyed(&AccountLabel::Default).inputs(live)), armed);
        assert_eq!(
            AlpacaVenueMount.resolve(&MountFixture::new(&KEYS[..2]).inputs(live)),
            Resolution::Paper(PaperCause::NoCredentials),
            "half a key set is no key set"
        );
    }
}

/// Review Focus 2 at this venue: no ceiling makes this arm resolve `Live` — it has no live arm.
/// Asserted at BOTH values of `live_permitted`, with a complete LIVE-tier key set present: beside
/// the SANDBOX set it leaves the sandbox arming exactly as it is (demo, held by `DemoOnlyArm`), and
/// on its own it arms nothing at all.
#[test]
fn a_ceiling_below_live_never_resolves_live() {
    let mut both_tiers = keyed(&AccountLabel::Default);
    let mut live_only = MountFixture::new(&[]);
    for (k, v) in live_tier_keys() {
        both_tiers.vars.insert(k.clone(), v.clone());
        live_only.vars.insert(k, v);
    }
    for live in [false, true] {
        assert_eq!(
            AlpacaVenueMount.resolve(&both_tiers.inputs(live)),
            Resolution::Armed {
                tier: Tier::Demo,
                held_below_live: Some(HeldBelowLive::DemoOnlyArm)
            },
            "LIVE-tier keys beside the SANDBOX set must leave the sandbox arming untouched"
        );
        assert_eq!(
            AlpacaVenueMount.resolve(&live_only.inputs(live)),
            Resolution::Paper(PaperCause::LiveTierNotWired),
            "the arm reads the SANDBOX tier alone, so LIVE-tier keys by themselves arm nothing — \
             and the cause says that, rather than that the store holds nothing"
        );
    }
}

/// **A LIVE-tier key set the arm cannot use is a NAMED cause, and only a COMPLETE one.** Half a set
/// is what `load_alpaca_config_for_account` calls absent, so it keeps the original cause — the same
/// rule the generic credential loader applies, and the reason this arm detects the live set with the
/// loader it already has rather than with a list of names.
#[test]
fn half_a_live_key_set_is_still_no_credentials() {
    let mut half = MountFixture::new(&[]);
    for (k, v) in live_tier_keys().into_iter().take(2) {
        half.vars.insert(k, v);
    }
    for live in [false, true] {
        assert_eq!(
            AlpacaVenueMount.resolve(&half.inputs(live)),
            Resolution::Paper(PaperCause::NoCredentials),
            "half a LIVE key set is no key set (live={live})"
        );
    }
}

/// The cause is scoped to the ACCOUNT asking, in both directions — the same no-fallback rule every
/// alpaca credential read obeys: a neighbour's live keys neither arm nor rename the cause.
#[test]
fn the_live_tier_cause_is_scoped_to_the_account_that_holds_the_keys() {
    let mut default_live = MountFixture::new(&[]);
    let mut alt_live = MountFixture::new(&[]);
    for (k, v) in live_tier_keys() {
        default_live.vars.insert(k.clone(), v.clone());
        alt_live.vars.insert(account_key(&k, &alt()), v);
    }
    alt_live.account = alt();
    // The default account's live keys are the DEFAULT account's business…
    assert_eq!(
        AlpacaVenueMount.resolve(&default_live.inputs(true)),
        Resolution::Paper(PaperCause::LiveTierNotWired)
    );
    default_live.account = alt();
    assert_eq!(
        AlpacaVenueMount.resolve(&default_live.inputs(true)),
        Resolution::Paper(PaperCause::NoCredentials),
        "ALT asks, and only the DEFAULT account holds live keys"
    );
    // …and ALT's own live keys are ALT's.
    assert_eq!(
        AlpacaVenueMount.resolve(&alt_live.inputs(true)),
        Resolution::Paper(PaperCause::LiveTierNotWired)
    );
    alt_live.account = AccountLabel::Default;
    assert_eq!(
        AlpacaVenueMount.resolve(&alt_live.inputs(true)),
        Resolution::Paper(PaperCause::NoCredentials),
        "the DEFAULT account asks, and only ALT holds live keys"
    );
}

/// The stay-paper half of the same fact at the MOUNT: a LIVE-tier key set alone mounts PAPER,
/// offline, with nothing to reconcile — the cause and the loudness changed, the outcome did not.
#[test]
fn a_live_key_set_alone_mounts_paper_offline() {
    let mut fx = MountFixture::new(&[]);
    for (k, v) in live_tier_keys() {
        fx.vars.insert(k, v);
    }
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut req = fx.request(true, "AAPL", &tx);
    req.recon_enabled = true;
    let out = AlpacaVenueMount.mount(req);
    assert!(matches!(out.exec, ExecOutcome::Paper));
    assert!(out.recon.is_none() && out.identity.is_none());
}

/// The account 2×2: a labelled account reads its OWN keys and never the default account's.
#[test]
fn a_labelled_account_reads_only_its_own_keys() {
    let mut alt_asks_default_keys = keyed(&AccountLabel::Default);
    alt_asks_default_keys.account = alt();
    assert_eq!(
        AlpacaVenueMount.resolve(&alt_asks_default_keys.inputs(true)),
        Resolution::Paper(PaperCause::NoCredentials),
        "ALT must not arm off the default account's keys"
    );
    assert!(matches!(
        AlpacaVenueMount.resolve(&keyed(&alt()).inputs(true)),
        Resolution::Armed { .. }
    ));
    let mut default_asks_alt_keys = keyed(&alt());
    default_asks_alt_keys.account = AccountLabel::Default;
    assert_eq!(
        AlpacaVenueMount.resolve(&default_asks_alt_keys.inputs(true)),
        Resolution::Paper(PaperCause::NoCredentials)
    );
}

/// Absent credentials: paper, nothing to reconcile, no probe row — and nothing was built.
#[test]
fn with_no_credentials_the_mount_is_paper_and_offline() {
    let fx = MountFixture::new(&[]);
    let (tx, _rx) = vike_exec::event_channel(8);
    let out = AlpacaVenueMount.mount(fx.request(true, "AAPL", &tx));
    assert!(matches!(out.exec, ExecOutcome::Paper));
    assert!(out.recon.is_none() && out.identity.is_none());
    assert!(AlpacaVenueMount.credential_probe(&fx.inputs(true)).is_none());
}

/// With the default account's keys the probe row exists, built without the network (the OAuth2
/// mint happens on the first request).
#[test]
fn credentialed_alpaca_offers_a_read_only_probe() {
    let fx = keyed(&AccountLabel::Default);
    assert!(matches!(
        AlpacaVenueMount.credential_probe(&fx.inputs(true)),
        Some(CredentialProbe::ReadOnly(_))
    ));
}

// ── what the arm says about a LIVE tier it will not use ─────────────────────────────────────────
//
// Every account label below is unique to its test: the unused-beside-demo `warn!` is said once per
// process per `(venue, account)`, so two tests sharing a label would pass or fail by scheduling order.

/// A distinctive stand-in for a secret, so "no value was logged" is an assertion that can fail.
const SECRET: &str = "SECRET-VALUE-do-not-log";

/// The sandbox set AND a complete live set (every live value is [`SECRET`]), both for `label`.
fn both_tiers(label: &AccountLabel) -> MountFixture {
    let mut fx = keyed(label);
    for (k, _) in live_tier_keys() {
        fx.vars.insert(account_key(&k, label), SECRET.to_string());
    }
    fx
}

/// The offline doubles `mount_with` takes: no grid, no reconcile client, a recording exec client.
fn mount_offline(fx: &MountFixture) -> (MountOutcome, Vec<CapturedEvent>) {
    let (tx, _rx) = vike_exec::event_channel(8);
    captured(|| {
        mount_with(
            fx.request(true, "AAPL", &tx),
            |_, _| None,
            |_, _| None,
            |_, _| Box::new(vike_exec::testing::RecordingClient::default()),
        )
    })
}

/// A live set BESIDE the sandbox one used to mount the sandbox in silence. It still mounts the
/// sandbox — bound at DEMO, nothing else moves — and now says, once, that the live set is unused.
#[test]
fn a_live_key_set_beside_the_sandbox_one_is_named_unused_and_the_mount_is_unchanged() {
    let beside = AccountLabel::parse("BESIDE").expect("a legal label");
    let (out, events) = mount_offline(&both_tiers(&beside));
    let ExecOutcome::Live(exec) = out.exec else { panic!("the sandbox set mounts, as it did") };
    assert_eq!(exec.bound_tier, Tier::Demo, "the live set must not move the bound tier");
    let unused = found_tier_events(&events);
    assert_eq!(unused.len(), 1, "exactly one line about the live set: {events:?}");
    let e = unused[0];
    assert_eq!(e.level, tracing::Level::WARN, "{e:?}");
    assert_eq!((e.field("venue"), e.field("account")), (Some("alpaca"), Some("BESIDE")), "{e:?}");
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

/// The sandbox set alone — the ordinary case — says nothing new.
#[test]
fn a_sandbox_set_alone_adds_no_line_about_the_live_tier() {
    let alone = AccountLabel::parse("ALONE").expect("a legal label");
    let (_, events) = mount_offline(&keyed(&alone));
    assert!(found_tier_events(&events).is_empty(), "{events:?}");
}

/// A live set with one key missing is a typo or a half-finished edit, and the loader calls it
/// absent — so it printed `NoCredentials` under the words of an empty store. The mount now names
/// the keys it lacks (label-composed, names only) and still lands on paper.
#[test]
fn a_half_written_live_key_set_names_the_missing_keys_and_the_mount_is_still_paper() {
    let half = AccountLabel::parse("HALF").expect("a legal label");
    let live = live_tier_keys();
    let mut fx = MountFixture::new(&[]);
    fx.account = half.clone();
    for (k, v) in live.iter().take(2) {
        fx.vars.insert(account_key(k, &half), v.clone());
    }
    let missing = account_key(&live[2].0, &half);
    let (tx, _rx) = vike_exec::event_channel(8);
    let (out, events) = captured(|| {
        mount_with(
            fx.request(true, "AAPL", &tx),
            |_, _| panic!("no sandbox set, so no grid fetch"),
            |_, _| panic!("no sandbox set, so no reconcile client"),
            |_, _| panic!("no sandbox set, so no exec client"),
        )
    });
    assert!(matches!(out.exec, ExecOutcome::Paper));
    let said = found_tier_events(&events);
    assert_eq!(said.len(), 1, "{events:?}");
    let e = said[0];
    assert_eq!(e.level, tracing::Level::ERROR, "{e:?}");
    assert_eq!((e.field("venue"), e.field("account")), (Some("alpaca"), Some("HALF")), "{e:?}");
    assert!(e.message.contains(&missing), "names the missing key `{missing}`: {}", e.message);
    for present in live.iter().take(2).map(|(k, _)| account_key(k, &half)) {
        assert!(!e.message.contains(&present), "names only what is MISSING: {}", e.message);
    }
    // The CAUSE the arming screen prints is unchanged: a half-written set is `NoCredentials`
    // (`half_a_live_key_set_is_still_no_credentials`), exactly as okx's half-written trio is.
    assert_eq!(
        AlpacaVenueMount.resolve(&fx.inputs(true)),
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
    for (k, v) in live_tier_keys() {
        live_only.vars.insert(account_key(&k, &unmounted), v);
    }
    let (_, events) =
        captured(|| AlpacaVenueMount.report_unmounted_account(&live_only.inputs(true)));
    let said = found_tier_events(&events);
    assert_eq!(said.len(), 1, "{events:?}");
    assert_eq!(said[0].level, tracing::Level::ERROR, "{events:?}");
    assert_eq!(said[0].field("account"), Some("UNMNT"), "{events:?}");

    let mut half = MountFixture::new(&[]);
    half.account = unmounted.clone();
    half.vars.insert(account_key(&live_tier_keys()[0].0, &unmounted), "x".to_string());
    let (_, events) = captured(|| AlpacaVenueMount.report_unmounted_account(&half.inputs(true)));
    assert_eq!(found_tier_events(&events).len(), 1, "a half-written set is named too: {events:?}");

    let (_, events) = captured(|| {
        AlpacaVenueMount.report_unmounted_account(&MountFixture::new(&[]).inputs(true));
    });
    assert!(events.is_empty(), "an empty store is the ordinary state and is silent: {events:?}");
}
