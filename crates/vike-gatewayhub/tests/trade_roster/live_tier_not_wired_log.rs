//! **A LIVE account the arm cannot trade is said out loud — through the REAL start, over the REAL
//! registry, and read back out of the log.**
//!
//! `crates/vike-gatewayhub/tests/trade_roster/mount_roster/stay_paper_matrix.rs` pins the CAUSE (what the arming
//! screen and `vike-backend venues` print); this file pins the LOUDNESS. Seven arms start only a
//! demo / practice / sandbox tier (alpaca, ig, deribit, ibkr, ctrader, fxcm and oanda), and an
//! account whose row says `live` and that holds its keys used to do not run in silence — the
//! credential doctrine is that an ABSENT credential is silent and a PRESENT-and-unusable one is an
//! `error!`. A credential belongs to its ACCOUNT (decision 0156 item 6): the keys are the fields of
//! the `live` account row, and there is no demo set beside them to fall back to.
//!
//! What each venue must emit for a `live` account holding its complete set:
//!
//! * exactly one `ERROR` event for that venue, carrying `venue`, `account` and **`found_tier`**
//!   (`live`). It carries NO `tier` field: on every live-MOUNT line `tier` is the tier that was
//!   STARTED, so a pipeline filtering on `tier=live` must match only starts that went live;
//! * a message that names the tier and says where the venue stays;
//! * **no value** anywhere in any captured line (the fixture's values are distinctive, so "nothing
//!   leaked" is an assertion that can fail);
//!
//! and with an EMPTY account, or a HALF-written set, nothing at all from that venue at `ERROR`:
//! absent is silent. A LABELLED account `ALT` is never started (a labelled account starts only when
//! it armed), so the fan-out asks the bridge to speak for it: the same one `ERROR`, `account` =
//! `ALT`, once per start.
//!
//! Each start is read through the workspace's scoped capture (`vike_log::capture::captured`): the line
//! is `vike_bridge_core::venue_adapter::report_live_tier_not_wired`'s `error!`, said on the thread that
//! runs the fan-out (one line per `(venue, account)` per start, no process-wide latch), and the capture's
//! interest floor keeps a sibling test's uncaptured hit of the same callsite from emptying it. So this
//! file is a member of the `trade_roster` group (`main.rs`) and needs no global subscriber. Silence is
//! still checked first.
//!
//! ibkr and fxcm are rows of this table only under their features (`bash scripts/ci_feature_suite.sh
//! ibkr` / `fxcm`); the default build exercises the five that always compile.

use std::collections::{BTreeMap, HashMap, HashSet};

use tracing::Level;
use vike_gatewayhub::registry::trade::REGISTRY;
use vike_log::capture::{CapturedEvent, captured};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_tradehub_core::AdapterPolicy;

/// A distinctive stand-in for a secret, so "no value was logged" can fail.
fn secret(venue: &str) -> String {
    format!("SECRET-VALUE-{venue}-do-not-log")
}

/// The key set of every venue this file covers, as `(venue, account fields, venue fields)` —
/// held by an account whose row says `live`. Every value is [`secret`]. ctrader's app registration
/// pair is the VENUE's and rides along beside its LIVE account's tokens.
type Store = (&'static str, Vec<&'static str>, Vec<&'static str>);

fn live_only_stores() -> Vec<Store> {
    // `mut` is used only under the `ibkr` / `fxcm` features, which append their rows below.
    // allow, not expect: unused_mut fires only under some feature suites (measured 2026-10-10).
    #[allow(unused_mut, clippy::allow_attributes)]
    let mut out: Vec<Store> = vec![
        ("deribit", vec!["API_KEY", "API_SECRET"], vec![]),
        ("oanda", vec!["API_KEY", "ACCOUNT_ID"], vec![]),
        ("ig", vec!["API_KEY", "IDENTIFIER", "PASSWORD"], vec![]),
        ("alpaca", vec!["CLIENT_ID", "CLIENT_SECRET", "ACCOUNT_ID"], vec![]),
        ("ctrader", vec!["ACCESS_TOKEN", "REFRESH_TOKEN"], vec!["CLIENT_ID", "CLIENT_SECRET"]),
    ];
    #[cfg(feature = "ibkr")]
    out.push(("ibkr", vec!["ACCOUNT"], vec![]));
    #[cfg(feature = "fxcm")]
    out.push(("fxcm", vec!["USER", "PASSWORD"], vec![]));
    out
}

/// The HALF-WRITTEN set of every venue this file covers, as `(venue, the fields that ARE stored)` —
/// a typo'd or forgotten secret on a `live` account. An incomplete set is an ABSENT one, and absent
/// is silent (the live gate's vocabulary is `Some`/`None`) — except ibkr, whose start names the
/// missing `ACCOUNT` field.
fn half_written_stores() -> Vec<(&'static str, Vec<&'static str>)> {
    // `mut` is used only under the `ibkr` / `fxcm` features, which append their rows below.
    // allow, not expect: unused_mut fires only under some feature suites (measured 2026-10-10).
    #[allow(unused_mut, clippy::allow_attributes)]
    let mut out: Vec<(&'static str, Vec<&'static str>)> = vec![
        ("deribit", vec!["API_KEY"]),
        ("ig", vec!["API_KEY"]),
        ("alpaca", vec!["CLIENT_ID"]),
        ("ctrader", vec!["ACCESS_TOKEN"]),
    ];
    #[cfg(feature = "fxcm")]
    out.push(("fxcm", vec!["USER"]));
    out
}

/// A policy whose `label` account of `venue` is a `live` row holding `fields` (and `venue`'s
/// application `venue_fields`), each value [`secret`].
fn live_policy(
    venue: &str,
    label: &AccountLabel,
    fields: &[&str],
    venue_fields: &[&str],
) -> AdapterPolicy {
    let pairs: Vec<(&str, String)> = fields.iter().map(|f| (*f, secret(venue))).collect();
    let borrowed: Vec<(&str, &str)> = pairs.iter().map(|(f, v)| (*f, v.as_str())).collect();
    let app: Vec<(&str, String)> = venue_fields.iter().map(|f| (*f, secret(venue))).collect();
    let app: Vec<(&str, &str)> = app.iter().map(|(f, v)| (*f, v.as_str())).collect();
    let mut policy = AdapterPolicy::default()
        .with_credentials(venue, label, AccountTier::Live, &borrowed)
        .with_venue_credentials(venue, &app);
    if venue == "ibkr" {
        // ibkr arms a `live` account whose gateway loads (decision 0140), so the account the arm
        // "cannot trade" is one whose LIVE gateway rows name a backend it does not wire.
        let gateway = [vike_secrets::VenueSettingRow {
            venue: "ibkr".to_string(),
            tier: Some(AccountTier::Live),
            field: "BACKEND".to_string(),
            value: "oauth".to_string(),
        }];
        policy.venue_settings = BTreeMap::from([(
            "ibkr".to_string(),
            vike_secrets::venue_setting::VenueSettings::from_rows("ibkr", &gateway),
        )]);
    }
    policy
}

/// The `error!` events in `events` that name `venue` — the lines every scenario below judges.
fn venue_errors<'a>(events: &'a [CapturedEvent], venue: &str) -> Vec<&'a CapturedEvent> {
    events
        .iter()
        .filter(|e| {
            e.level == Level::ERROR && e.fields.get("venue").map(String::as_str) == Some(venue)
        })
        .collect()
}

/// Run the venue's REAL fan-out (`make_engine_accounts`) for a policy whose default account is a
/// `demo` row holding nothing and whose labelled account `ALT` is the `live` row of `policy`; return
/// the events it logged. Neither account runs (decision 0156): `ALT` is never started and its bridge
/// speaks for it — the case that said nothing.
fn fan_out_with_alt(venue: &str, alt_policy: AdapterPolicy) -> Vec<CapturedEvent> {
    let (tx, _rx) = vike_exec::event_channel(16);
    let mut live = HashSet::new();
    // ibkr rides ONE session per box, so its accounts must sit at ONE tier: its default account is
    // a `live` row beside ALT (every other venue's default is `demo`).
    let default_tier = if venue == "ibkr" { AccountTier::Live } else { AccountTier::Demo };
    let policy = alt_policy.with_account(venue, &AccountLabel::Default, default_tier);
    let vars = HashMap::new();
    let rig = crate::local::rig(REGISTRY, &vars, Some(&policy));
    let mut env = rig.env(&tx, &mut live);
    env.recon_enabled = true;
    env.policy = Some(&policy);
    let (mounted, events) =
        captured(|| vike_tradehub_core::make_engine_accounts(&mut env, venue, &[], &[]));
    let mounted =
        mounted.unwrap_or_else(|e| panic!("{venue}: an offline fan-out must never refuse: {e}"));
    assert!(live.is_empty(), "{venue}: nothing here may mount live");
    assert!(mounted.running.is_empty(), "{venue}: neither account has a usable key set");
    events
}

/// Start `venue` for real as the DEFAULT account of `policy`; return the events the start logged. The
/// account does not run and the fan-out must not refuse.
fn start_paper(venue: &str, policy: &AdapterPolicy) -> Vec<CapturedEvent> {
    let (tx, _rx) = vike_exec::event_channel(16);
    let mut live = HashSet::new();
    let vars = HashMap::new();
    let rig = crate::local::rig(REGISTRY, &vars, Some(policy));
    let mut env = rig.env(&tx, &mut live);
    env.recon_enabled = true;
    env.policy = Some(policy);
    let (mounted, events) =
        captured(|| vike_tradehub_core::make_engine_accounts(&mut env, venue, &[], &[]));
    let mounted =
        mounted.unwrap_or_else(|e| panic!("{venue}: an offline fan-out must never refuse: {e}"));
    assert!(live.is_empty(), "{venue}: nothing here may mount live");
    assert!(mounted.running.is_empty(), "{venue}: no usable key set, so the account does not run");
    events
}

#[test]
fn a_live_account_the_arm_cannot_trade_is_an_error_naming_venue_account_and_tier() {
    // SILENCE FIRST: a live account holding nothing is the ordinary unconfigured state.
    for (venue, _, _) in live_only_stores() {
        let policy =
            AdapterPolicy::default().with_account(venue, &AccountLabel::Default, AccountTier::Live);
        let events = start_paper(venue, &policy);
        let loud = venue_errors(&events, venue);
        assert!(loud.is_empty(), "{venue}: an EMPTY account must stay silent at error!: {loud:?}");
    }

    // THEN SPEECH: a LIVE account holding its complete key set.
    for (venue, fields, venue_fields) in live_only_stores() {
        let policy = live_policy(venue, &AccountLabel::Default, &fields, &venue_fields);
        let events = start_paper(venue, &policy);
        let errors = venue_errors(&events, venue);
        assert_eq!(
            errors.len(),
            1,
            "{venue}: exactly one error! for the live account it will not trade, got {events:?}"
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
        // NO VALUE ANYWHERE in anything the start logged.
        for line in &events {
            let text = format!("{line:?}");
            assert!(
                !text.contains(&secret(venue)),
                "{venue}: a credential VALUE reached the log: {text}"
            );
        }
    }

    // THE LABELLED ACCOUNT. `ALT` is a `live` row holding its set; it does not run, so the
    // fan-out never starts it and its bridge's `start` is never reached — which is why the default
    // account's line had no labelled twin. The fan-out asks the bridge to speak for it: the SAME
    // sentence, once, naming `ALT`, at the same level and with the same fields. An EMPTY `ALT` is
    // silent.
    for (venue, _, _) in live_only_stores() {
        let empty = AdapterPolicy::default().with_account(venue, &alt_label(), AccountTier::Live);
        let events = fan_out_with_alt(venue, empty);
        let loud = venue_errors(&events, venue);
        assert!(loud.is_empty(), "{venue}: an EMPTY labelled account must stay silent: {loud:?}");
    }
    for (venue, fields, venue_fields) in live_only_stores() {
        let policy = live_policy(venue, &alt_label(), &fields, &venue_fields);
        let events = fan_out_with_alt(venue, policy);
        let errors = venue_errors(&events, venue);
        assert_eq!(
            errors.len(),
            1,
            "{venue}: exactly one error! for the labelled live account, got {events:?}"
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

    // A HALF-WRITTEN set (a typo'd or forgotten secret) is an ABSENT one, and absent is silent.
    for (venue, present) in half_written_stores() {
        let policy = live_policy(venue, &AccountLabel::Default, &present, &[]);
        let events = start_paper(venue, &policy);
        let loud = venue_errors(&events, venue);
        assert!(loud.is_empty(), "{venue}: a half-written set is absent, so silent: {loud:?}");
    }
}

fn alt_label() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}
