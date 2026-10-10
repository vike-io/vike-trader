//! **A LIVE-tier key set the arm cannot use is said out loud — through the REAL mount, over the REAL
//! registry, and read back out of the log.**
//!
//! `crates/vike-tradehub/tests/mount_roster/stay_paper_matrix.rs` pins the CAUSE (what the arming
//! screen and `vike-backend venues` print); this file pins the LOUDNESS, which is the other half of
//! the same finding. Seven arms mount only a demo / practice / sandbox tier (alpaca, ig, deribit,
//! ibkr, ctrader, fxcm and oanda), and on a store holding only LIVE-tier keys six of them used to
//! stay paper in silence — the credential doctrine is that an ABSENT credential is silent and a
//! PRESENT-and-unusable one is an `error!`, and only oanda kept it.
//!
//! What each venue must emit, with a live-only store under a `demo` ceiling (the ceiling that reaches
//! the bridge and asks for nothing live):
//!
//! * exactly one `ERROR` event for that venue, carrying `venue`, `account` and **`found_tier`**
//!   (`live` — the tier that was found and refused). It carries NO `tier` field: on every
//!   live-MOUNT line `tier` is the tier that was MOUNTED, so a pipeline filtering on `tier=live`
//!   must match only mounts that went live, never a line that says nothing did;
//! * a message that names the tier and says where the venue stays;
//! * **no key, no value** anywhere in any captured line (the fixture's values are distinctive, so
//!   "nothing leaked" is an assertion that can fail);
//!
//! and with an EMPTY store, nothing at all from that venue at `ERROR`: absent is still silent.
//!
//! Two more cells of the same table are held through the same REAL registry:
//!
//! * **a LABELLED account** `ALT`, armed at `demo` by its own line and holding only a live-tier set,
//!   is never mounted (a labelled account mounts only when it armed), so its bridge's `mount` — where
//!   the default account's line is said — is never reached. The fan-out asks the bridge to speak for
//!   it: the same one `ERROR`, with `account` = `ALT`, once per start. An EMPTY `ALT` is silent.
//! * **a HALF-written live set** (a typo'd or forgotten secret) is one `ERROR` naming the keys it
//!   LACKS — for the default account through its `mount`, and for `ALT` through the fan-out, with the
//!   label-composed names — and never a value, and never a key that IS stored.
//!
//! ⚠ **One test function, deliberately.** The collector is installed as the process's GLOBAL default
//! — the shape `log_capture/mod.rs` argues for (a thread-local `with_default` leaves the first
//! evaluation of a callsite's `Interest` racing the rest of the binary) — and a global default may be
//! installed once, so silence and speech are observed in one pass, silence first.
//!
//! ibkr and fxcm are rows of this table only under their features (`bash scripts/ci_feature_suite.sh
//! ibkr` / `fxcm`); the default build exercises the five that always compile.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex};

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Metadata, Subscriber};
use vike_config::{VenueMode, VenuePolicy};
use vike_model::accounts::account_keys::AccountLabel;
use vike_mount::MountPolicy;
use vike_tradehub::registry::REGISTRY;

/// One captured event.
#[derive(Clone, Debug)]
struct Seen {
    level: Level,
    fields: BTreeMap<String, String>,
    message: String,
}

type Log = Arc<Mutex<Vec<Seen>>>;

struct Collector(Log);

struct Fields<'a>(&'a mut Seen);

impl Visit for Fields<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            self.0.message = format!("{value:?}");
        } else {
            self.0.fields.insert(field.name().to_string(), format!("{value:?}"));
        }
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.0.message = value.to_string();
        } else {
            self.0.fields.insert(field.name().to_string(), value.to_string());
        }
    }
}

impl Subscriber for Collector {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(1) // `from_u64` panics on 0; no span is ever entered here
    }
    fn record(&self, _: &Id, _: &Record<'_>) {}
    fn record_follows_from(&self, _: &Id, _: &Id) {}
    fn event(&self, event: &Event<'_>) {
        let mut seen = Seen {
            level: *event.metadata().level(),
            fields: BTreeMap::new(),
            message: String::new(),
        };
        event.record(&mut Fields(&mut seen));
        self.0.lock().expect("log lock").push(seen);
    }
    fn enter(&self, _: &Id) {}
    fn exit(&self, _: &Id) {}
}

/// A distinctive stand-in for a secret, so "no value was logged" can fail.
fn secret(venue: &str) -> String {
    format!("SECRET-VALUE-{venue}-do-not-log")
}

/// The LIVE-tier key set of every venue this file covers, as `(venue, key names)`; every value is
/// [`secret`]. ctrader's app registration pair is tier-less and rides along with its LIVE tokens.
fn live_only_stores() -> Vec<(&'static str, Vec<&'static str>)> {
    // `mut` is used only under the `ibkr` / `fxcm` features, which append their rows below.
    // allow, not expect: unused_mut fires only under some feature suites (measured 2026-10-10).
    #[allow(unused_mut, clippy::allow_attributes)]
    let mut out: Vec<(&'static str, Vec<&'static str>)> = vec![
        ("deribit", vec!["DERIBIT_LIVE_API_KEY", "DERIBIT_LIVE_API_SECRET"]),
        ("oanda", vec!["OANDA_LIVE_API_KEY", "OANDA_LIVE_ACCOUNT_ID"]),
        ("ig", vec!["IG_LIVE_API_KEY", "IG_LIVE_IDENTIFIER", "IG_LIVE_PASSWORD"]),
        (
            "alpaca",
            vec!["ALPACA_LIVE_CLIENT_ID", "ALPACA_LIVE_CLIENT_SECRET", "ALPACA_LIVE_ACCOUNT_ID"],
        ),
        (
            "ctrader",
            vec![
                "CTRADER_CLIENT_ID",
                "CTRADER_CLIENT_SECRET",
                "CTRADER_LIVE_ACCESS_TOKEN",
                "CTRADER_LIVE_REFRESH_TOKEN",
            ],
        ),
    ];
    #[cfg(feature = "ibkr")]
    out.push(("ibkr", vec!["IBKR_LIVE_ACCOUNT"]));
    #[cfg(feature = "fxcm")]
    out.push(("fxcm", vec!["FXCM_LIVE_USER", "FXCM_LIVE_PASSWORD"]));
    out
}

/// The HALF-WRITTEN live set of every venue this file covers, as `(venue, the keys that ARE
/// stored, the keys that are MISSING)` — a typo'd or forgotten secret. oanda is not a row: its
/// refusal fires on ANY live-named variable, half a pair included, and names the variables it found.
fn half_written_stores() -> Vec<(&'static str, Vec<&'static str>, Vec<&'static str>)> {
    // `mut` is used only under the `ibkr` / `fxcm` features, which append their rows below.
    // allow, not expect: unused_mut fires only under some feature suites (measured 2026-10-10).
    #[allow(unused_mut, clippy::allow_attributes)]
    let mut out: Vec<(&'static str, Vec<&'static str>, Vec<&'static str>)> = vec![
        ("deribit", vec!["DERIBIT_LIVE_API_KEY"], vec!["DERIBIT_LIVE_API_SECRET"]),
        ("ig", vec!["IG_LIVE_API_KEY"], vec!["IG_LIVE_IDENTIFIER", "IG_LIVE_PASSWORD"]),
        (
            "alpaca",
            vec!["ALPACA_LIVE_CLIENT_ID"],
            vec!["ALPACA_LIVE_CLIENT_SECRET", "ALPACA_LIVE_ACCOUNT_ID"],
        ),
        (
            "ctrader",
            // The app pair is every tier's; the LIVE access token is what starts a live grant.
            vec!["CTRADER_CLIENT_ID", "CTRADER_CLIENT_SECRET", "CTRADER_LIVE_ACCESS_TOKEN"],
            vec!["CTRADER_LIVE_REFRESH_TOKEN"],
        ),
    ];
    #[cfg(feature = "ibkr")]
    out.push(("ibkr", vec!["IBKR_LIVE_CLIENT_ID"], vec!["IBKR_LIVE_ACCOUNT"]));
    #[cfg(feature = "fxcm")]
    out.push(("fxcm", vec!["FXCM_LIVE_USER"], vec!["FXCM_LIVE_PASSWORD"]));
    out
}

/// A store holding each of `keys` with [`secret`] as its value.
fn secret_store(venue: &str, keys: &[&str]) -> HashMap<String, String> {
    keys.iter().map(|k| ((*k).to_string(), secret(venue))).collect()
}

/// The `error!` events in `events` that name `venue` — the lines every scenario below judges.
fn venue_errors<'a>(events: &'a [Seen], venue: &str) -> Vec<&'a Seen> {
    events
        .iter()
        .filter(|e| {
            e.level == Level::ERROR && e.fields.get("venue").map(String::as_str) == Some(venue)
        })
        .collect()
}

/// The same store with every key renamed to the labelled account `label`'s own.
fn labelled(store: &HashMap<String, String>, label: &AccountLabel) -> HashMap<String, String> {
    store
        .iter()
        .map(|(k, v)| (vike_model::accounts::account_keys::account_key(k, label), v.clone()))
        .collect()
}

/// Run the venue's REAL fan-out (`make_engine_accounts`) for a store whose only keys belong to the
/// labelled account `ALT`, whose line arms it at `demo`; return the events it logged. The default
/// account is paper and is mounted (it always is); `ALT` resolves paper for want of a usable key
/// set and is NOT mounted — the case that said nothing.
fn fan_out_with_alt(venue: &str, store: &HashMap<String, String>, log: &Log) -> Vec<Seen> {
    let (tx, _rx) = vike_exec::event_channel(16);
    let mut live = HashSet::new();
    let alt = AccountLabel::parse("ALT").expect("a legal label");
    let policy = MountPolicy {
        venues: VenuePolicy::default().declare(venue, VenueMode::Demo).declare_account(
            venue,
            &alt,
            VenueMode::Demo,
        ),
        ..MountPolicy::default()
    };
    let before = log.lock().expect("log lock").len();
    let mut env = vike_mount::MountEnv::new(REGISTRY, store, &tx, &mut live);
    env.recon_enabled = true;
    env.policy = Some(&policy);
    let mounted = vike_mount::make_engine_accounts(&mut env, venue, &[], &[])
        .unwrap_or_else(|e| panic!("{venue}: a paper fan-out must never refuse to start: {e}"));
    assert!(live.is_empty(), "{venue}: nothing here may mount live");
    assert_eq!(mounted.len(), 1, "{venue}: only the DEFAULT account is mounted — ALT is unarmed");
    log.lock().expect("log lock")[before..].to_vec()
}

/// Mount `venue` for real under a `demo` ceiling with `store`; return how many events the log held
/// before and after. The mount must be paper and must not refuse.
fn mount_paper(venue: &str, store: &HashMap<String, String>, log: &Log) -> (usize, usize) {
    let (tx, _rx) = vike_exec::event_channel(16);
    let mut live = HashSet::new();
    let policy = MountPolicy {
        venues: VenuePolicy::default().declare(venue, VenueMode::Demo),
        ..MountPolicy::default()
    };
    let before = log.lock().expect("log lock").len();
    let mut env = vike_mount::MountEnv::new(REGISTRY, store, &tx, &mut live);
    env.recon_enabled = true;
    env.policy = Some(&policy);
    let (_engine, recon) = vike_mount::make_engine(&mut env, venue, "BTCUSDT")
        .unwrap_or_else(|e| panic!("{venue}: a paper mount must never refuse to start: {e}"));
    assert!(live.is_empty(), "{venue}: nothing here may mount live");
    assert!(recon.is_none(), "{venue}: a paper venue never reconciles");
    let after = log.lock().expect("log lock").len();
    (before, after)
}

#[test]
fn a_live_tier_key_set_the_arm_cannot_use_is_an_error_naming_venue_account_and_tier() {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    tracing::subscriber::set_global_default(Collector(Arc::clone(&log)))
        .expect("this test binary installs exactly one subscriber");

    // SILENCE FIRST: an empty store is the ordinary unconfigured state.
    for (venue, _) in live_only_stores() {
        let (before, after) = mount_paper(venue, &HashMap::new(), &log);
        let events = log.lock().expect("log lock")[before..after].to_vec();
        let loud = venue_errors(&events, venue);
        assert!(loud.is_empty(), "{venue}: an EMPTY store must stay silent at error!: {loud:?}");
    }

    // THEN SPEECH: the live-tier-only store.
    for (venue, names) in live_only_stores() {
        let store = secret_store(venue, &names);
        let (before, after) = mount_paper(venue, &store, &log);
        let events = log.lock().expect("log lock")[before..after].to_vec();
        let errors = venue_errors(&events, venue);
        assert_eq!(
            errors.len(),
            1,
            "{venue}: exactly one error! for the live-tier key set it will not use, got {events:?}"
        );
        let e = errors[0];
        assert_eq!(
            e.fields.get("account").map(String::as_str),
            Some("DEFAULT"),
            "{venue}: the account field (the default account renders the way every mount line renders it): {e:?}"
        );
        assert_eq!(
            e.fields.get("found_tier").map(String::as_str),
            Some("live"),
            "{venue}: the tier that was found and refused: {e:?}"
        );
        assert!(
            !e.fields.contains_key("tier"),
            "{venue}: `tier` is reserved for the tier a mount BOUND; a refusal must not carry it: {e:?}"
        );
        assert!(
            e.message.contains("LIVE") && e.message.to_uppercase().contains("PAPER"),
            "{venue}: names the tier and where the venue stays: {}",
            e.message
        );
        // NO VALUE ANYWHERE in anything the mount logged — and, for the six arms that share the
        // one helper, no key NAME either. oanda's refusal predates the helper and names the
        // live-named VARIABLES it found by design (`UnreachableLiveTier` holds names and never a
        // value), which is the one disposition the credential doctrine allows for a name.
        for line in &events {
            let text = format!("{line:?}");
            assert!(
                !text.contains(&secret(venue)),
                "{venue}: a credential VALUE reached the log: {text}"
            );
            if venue != "oanda" {
                for key in &names {
                    assert!(
                        !text.contains(key),
                        "{venue}: a credential key NAME reached the log: {text}"
                    );
                }
            }
        }
    }

    // THE LABELLED ACCOUNT. `ALT` holds only a LIVE-tier key set and its line arms it at `demo`; it
    // resolves paper, so the fan-out never mounts it and its bridge's `mount` is never reached —
    // which is why the default account's line had no labelled twin. The fan-out now asks the bridge
    // to speak for it: the SAME sentence, once, naming `ALT`, at the same level and with the same
    // fields. The default account (empty, and mounted) says nothing, and an EMPTY `ALT` is silent.
    for (venue, _) in live_only_stores() {
        let events = fan_out_with_alt(venue, &labelled(&HashMap::new(), &alt_label()), &log);
        let loud = venue_errors(&events, venue);
        assert!(loud.is_empty(), "{venue}: an EMPTY labelled account must stay silent: {loud:?}");
    }
    for (venue, names) in live_only_stores() {
        let store = secret_store(venue, &names);
        let events = fan_out_with_alt(venue, &labelled(&store, &alt_label()), &log);
        let errors = venue_errors(&events, venue);
        assert_eq!(
            errors.len(),
            1,
            "{venue}: exactly one error! for the labelled account's unused live-tier key set, got {events:?}"
        );
        let e = errors[0];
        assert_eq!(
            e.fields.get("account").map(String::as_str),
            Some("ALT"),
            "{venue}: it names the LABELLED account: {e:?}"
        );
        assert_eq!(
            e.fields.get("found_tier").map(String::as_str),
            Some("live"),
            "{venue}: the tier that was found and refused: {e:?}"
        );
        assert!(!e.fields.contains_key("tier"), "{venue}: `tier` is reserved for mounts: {e:?}");
        assert!(
            e.message.contains("LIVE") && e.message.to_uppercase().contains("PAPER"),
            "{venue}: names the tier and where the venue stays: {}",
            e.message
        );
        for line in &events {
            assert!(
                !format!("{line:?}").contains(&secret(venue)),
                "{venue}: a credential VALUE reached the log: {line:?}"
            );
        }
    }

    // A HALF-WRITTEN live set (a typo'd or forgotten secret): the DEFAULT account's `mount` names
    // the keys it lacks — names only — and so does the fan-out for a labelled account.
    for (venue, present, missing) in half_written_stores() {
        let store = secret_store(venue, &present);
        for (who, store, account) in [
            ("default", store.clone(), "DEFAULT"),
            ("labelled", labelled(&store, &alt_label()), "ALT"),
        ] {
            let events = if who == "default" {
                let (before, after) = mount_paper(venue, &store, &log);
                log.lock().expect("log lock")[before..after].to_vec()
            } else {
                fan_out_with_alt(venue, &store, &log)
            };
            let errors = venue_errors(&events, venue);
            assert_eq!(
                errors.len(),
                1,
                "{venue} ({who}): exactly one error! naming the missing keys, got {events:?}"
            );
            let e = errors[0];
            assert_eq!(
                e.fields.get("account").map(String::as_str),
                Some(account),
                "{venue}: {e:?}"
            );
            assert_eq!(
                e.fields.get("found_tier").map(String::as_str),
                Some("live"),
                "{venue}: {e:?}"
            );
            for key in &missing {
                let named = if who == "default" {
                    (*key).to_string()
                } else {
                    vike_model::accounts::account_keys::account_key(key, &alt_label())
                };
                assert!(
                    e.message.contains(&named),
                    "{venue} ({who}): names `{named}`: {}",
                    e.message
                );
            }
            for key in &present {
                let stored = if who == "default" {
                    (*key).to_string()
                } else {
                    vike_model::accounts::account_keys::account_key(key, &alt_label())
                };
                assert!(
                    !e.message.contains(&stored),
                    "{venue} ({who}): names only what is MISSING, not the stored `{stored}`: {}",
                    e.message
                );
            }
            assert!(
                events.iter().all(|line| !format!("{line:?}").contains(&secret(venue))),
                "{venue} ({who}): a credential VALUE reached the log: {events:?}"
            );
        }
    }
}

fn alt_label() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}
