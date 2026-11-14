//! **No FX venue logs in while the FX market is closed** (owner ruling 2026-11-13, `vike_bridge_core::fx_login`): the
//! REAL lane's adapter start and its preflight credential leg, on a FIXED clock.
//!
//! The FX accounts here hold no credentials, so no start below can dial anything whatever the gate says: on an open
//! market each is refused for what its row lacks, exactly as before the gate (the weekday pin, written down from the
//! code before the gate existed), and on a Saturday each is refused `fx_market_closed`, naming Sunday's open, before
//! any credential or venue setting is read (the gate is the FIRST answer after the registry's own).
//!
//! ⚠ It ARMS the process-wide gate, which is why it is a member of this binary: nextest runs each test in its own
//! process (`main.rs`'s module doc), and each test arms the clock it needs before it asks anything.

use std::collections::HashMap;

use vike_gatewayhub::registry::trade::REGISTRY;
use vike_gatewayhub::trade::lane::{TradeLane, TradeLaneTarget};
use vike_gatewayhub_client::proto::{
    AdapterStart, CredentialGapWire, PreflightLegWire, PreflightStep,
};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::{AccountTier, ArmingBlock};
use vike_tradehub_core::AdapterPolicy;

/// The venues `vike_venues::session::session_for` puts on the FX week.
const FX: [&str; 5] = ["ctrader", "dukascopy", "fxcm", "ig", "oanda"];

fn wednesday() -> i64 {
    vike_model::time::parse_utc_timestamp_ms("2026-11-11T12:00:00Z").expect("an instant")
}

fn saturday() -> i64 {
    vike_model::time::parse_utc_timestamp_ms("2026-11-14T12:00:00Z").expect("an instant")
}

/// The lane's answer to one `demo` start of `venue`'s default account, which holds no credentials:
/// `(block_word, refusal_text)`, or `None` when it started.
fn start(venue: &str) -> Option<(String, String)> {
    let policy =
        AdapterPolicy::default().with_account(venue, &AccountLabel::Default, AccountTier::Demo);
    let backend = crate::bridge_backend::BridgeBackend::new(REGISTRY, HashMap::new(), &policy);
    let target =
        TradeLaneTarget { venue: venue.to_string(), account: None, tier: AccountTier::Demo };
    let start = AdapterStart {
        rid: 0,
        symbol: "EURUSD".to_string(),
        declared_legs: Vec::new(),
        reconcile_enabled: false,
        takes_trigger: false,
        risk: None,
        market_slippage: None,
        halt_admit: Default::default(),
        withhold: None,
        account_leverage: None,
        read_account_currency: false,
        route_key: venue.to_string(),
    };
    let (events, _ingest) = vike_execution::event_channel(16);
    backend
        .lane()
        .start_adapter(&target, &start, events, None)
        .err()
        .map(|refusal| (refusal.block_word, refusal.refusal_text))
}

/// What each FX venue's no-credential `demo` start answered on the code BEFORE the gate: written down from base
/// `29d75677f4` on a the latency box lane (2026-11-13, `scratch/fx-login-gate-pin`), `(venue, block word, refusal text)`.
/// fxcm's row is a box without the ForexConnect shim, which is every CI runner.
const BEFORE_THE_GATE: [(&str, &str, &str); 5] = [
    (
        "ctrader",
        "no_credentials",
        "no usable ctrader credentials in the store for ctrader — absent credentials are still the live gate, so it \
         does not run",
    ),
    (
        "dukascopy",
        "no_credentials",
        "no usable dukascopy credentials in the store for dukascopy — absent credentials are still the live gate, \
         so it does not run",
    ),
    (
        "fxcm",
        "sdk_absent",
        "fxcm's ForexConnect shim did not load on this box, so its live mount is refused and fxcm does not run — a \
         live client there would reject every order",
    ),
    (
        "ig",
        "no_credentials",
        "no usable ig credentials in the store for ig — absent credentials are still the live gate, so it does not \
         run",
    ),
    (
        "oanda",
        "no_credentials",
        "no usable oanda credentials in the store for oanda — absent credentials are still the live gate, so it \
         does not run",
    ),
];

/// **A weekday start answers byte for byte what it did before the gate** (the pin, recorded on the old code): each
/// FX venue's start with no credentials is refused for that, never for the market.
#[test]
fn a_weekday_start_of_an_fx_venue_answers_exactly_as_before_the_gate() {
    vike_bridge_core::fx_login::arm(wednesday);
    for (venue, word, text) in BEFORE_THE_GATE {
        if venue == "fxcm" && vike_fxcm::sdk_available() {
            continue; // a box holding the shim answers past it: not the row written down
        }
        assert_eq!(start(venue), Some((word.to_string(), text.to_string())), "{venue}");
    }
}

/// **A Saturday start makes no login**: every FX venue's start is refused `fx_market_closed` in words that name
/// Sunday 21:00 UTC, before its credentials are even looked at (they would refuse it otherwise). A crypto venue is
/// not held.
#[test]
fn a_saturday_start_of_an_fx_venue_is_refused_until_the_open_before_anything_is_read() {
    vike_bridge_core::fx_login::arm(saturday);
    for venue in FX {
        let (word, text) = start(venue).expect("the market is closed: the start is refused");
        assert_eq!(word, ArmingBlock::FxMarketClosed.word(), "{venue}: {text}");
        assert_eq!(text, "FX market closed: login at 2026-11-15T21:00:00Z", "{venue}");
    }
    let (word, _) = start("bybit").expect("no credentials: the start is refused");
    assert_ne!(word, ArmingBlock::FxMarketClosed.word(), "crypto is never held by the FX week");
}

/// **A credential read is a login**: on a Saturday the preflight's credential leg of an FX venue is answered
/// `Unanswered` (a WARN that never demotes, never a FAIL) in the gate's words, and nothing is read.
#[test]
fn a_saturday_preflight_reads_no_fx_credential() {
    vike_bridge_core::fx_login::arm(saturday);
    let policy =
        AdapterPolicy::default().with_account("ctrader", &AccountLabel::Default, AccountTier::Demo);
    let backend = crate::bridge_backend::BridgeBackend::new(REGISTRY, HashMap::new(), &policy);
    let answer = backend.lane().preflight(&["ctrader".to_string()], PreflightStep::Credential);
    match answer.legs.as_slice() {
        [
            PreflightLegWire::Credential {
                venue,
                leg: CredentialGapWire::Unanswered { detail, .. },
                ..
            },
        ] => {
            assert_eq!(venue, "ctrader");
            assert!(detail.contains("FX market closed: login at 2026-11-15T21:00:00Z"), "{detail}");
        }
        other => panic!("one unanswered credential leg: {other:?}"),
    }
}
