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
use vike_config::VenueMode;
use vike_mount::{MountPolicy, VenuePolicy};
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
    #[allow(unused_mut)]
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
    let (_engine, recon) = vike_mount::make_engine(
        REGISTRY,
        venue,
        "BTCUSDT",
        store,
        &tx,
        &mut live,
        true,
        None,
        None,
        None,
        Some(&policy),
    )
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
        let loud: Vec<&Seen> = events
            .iter()
            .filter(|e| {
                e.level == Level::ERROR && e.fields.get("venue").map(String::as_str) == Some(venue)
            })
            .collect();
        assert!(loud.is_empty(), "{venue}: an EMPTY store must stay silent at error!: {loud:?}");
    }

    // THEN SPEECH: the live-tier-only store.
    for (venue, names) in live_only_stores() {
        let store: HashMap<String, String> =
            names.iter().map(|k| ((*k).to_string(), secret(venue))).collect();
        let (before, after) = mount_paper(venue, &store, &log);
        let events = log.lock().expect("log lock")[before..after].to_vec();
        let errors: Vec<&Seen> = events
            .iter()
            .filter(|e| {
                e.level == Level::ERROR && e.fields.get("venue").map(String::as_str) == Some(venue)
            })
            .collect();
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
}
