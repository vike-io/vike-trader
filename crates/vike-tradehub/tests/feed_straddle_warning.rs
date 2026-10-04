//! **What the daemon SAYS when a venue's armed accounts dial BOTH networks — and that it says
//! nothing when they do not.**
//!
//! One market feed serves a venue, so a venue with a mainnet account and a testnet account on it
//! cannot have a feed that is right for both. `crates/vike-tradehub/src/venue_arming.rs`'s
//! `feed_tier_from_rows` settles it MAINNET WINS (a mainnet feed under a testnet account costs only
//! testnet fidelity; a testnet feed under a live account prices REAL orders off the testnet book) and
//! does NOT refuse the start (a refusal stops every venue and every mount, fires on the next restart
//! or deploy, and a node that will not start cannot flatten a position —
//! `docs/decisions/0013-degrade-vs-refuse.md`). What it owes the operator in exchange is a loud,
//! structured `warn!`, ONCE PER PROCESS per straddle, naming the venue and the testnet accounts that are
//! being priced off the wrong book — labels and settings keys, never a credential value. That line is
//! this file's subject, and so is its dedup: the plan is built again whenever a venue is mounted
//! again, and the same straddle said at every build is a line nobody reads twice. The dedup is keyed
//! by the straddle's CONTENT (the venue, the accounts on each network, the settings keys that move
//! them), so a DIFFERENT straddle — another testnet account joins — is a new finding and is said.
//! The PLAN each cell gets is `tests/daemon/feed_network_table.rs`'s.
//!
//! # Why a binary of its own
//!
//! The words are asserted through `log_capture::install`, which sets the process's GLOBAL `tracing`
//! default and may be called once — so this file is one test function, silence first and speech
//! second (the shape `unaddressable_account_silence.rs` documents), and it cannot be a `daemon` group
//! member: that group's members share a process with tests that install their own subscribers.
//!
//! The collector records an event's `message` only, so the message carries everything an operator
//! needs; the structured fields beside it (`venue`, `mainnet_accounts`, `testnet_accounts`,
//! `settings_keys`) are for a log pipeline.

mod log_capture;

use std::collections::HashMap;

use vike_config::{VenueMode, VenuePolicy};
use vike_hyperliquid::config::Network;
use vike_model::accounts::account_keys::{AccountLabel, account_key};
use vike_mount::{MakerMountConfig, MountPolicy};
use vike_tradehub::VenuePlan;
use vike_tradehub::feeds::venue_feed_plan;

/// The value every fixture credential carries — asserted ABSENT from what the daemon says.
const FAKE_SECRET: &str = "fixture-credential-value-that-must-never-be-echoed";

/// The phrase only the straddle warning contains — what every count below keys on.
const STRADDLE: &str = "ONE market feed per venue";

/// The tiers an account's keys are stored at.
const NONE: &[&str] = &[];
const DEMO: &[&str] = &["DEMO"];
const LIVE: &[&str] = &["LIVE"];
const BOTH: &[&str] = &["LIVE", "DEMO"];

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}

/// A credential map: the default account holds `default` tiers' keys and ALT holds `alt_tiers`'.
fn vars(default: &[&str], alt_tiers: &[&str]) -> HashMap<String, String> {
    let upper = "hyperliquid".to_uppercase();
    let mut vars = HashMap::new();
    for (tiers, label) in [(default, AccountLabel::Default), (alt_tiers, alt())] {
        for tier in tiers {
            vars.insert(
                account_key(&format!("{upper}_{tier}_PRIVATE_KEY"), &label),
                FAKE_SECRET.to_string(),
            );
        }
    }
    vars
}

/// The venue line at `live`, and ALT's own line when it has one.
fn venues(alt_line: Option<VenueMode>) -> VenuePolicy {
    let policy = VenuePolicy::default().declare("hyperliquid", VenueMode::Live);
    match alt_line {
        Some(mode) => policy.declare_account("hyperliquid", &alt(), mode),
        None => policy,
    }
}

fn beta() -> AccountLabel {
    AccountLabel::parse("BETA").expect("a legal label")
}

/// [`vars`] plus a BETA account holding DEMO keys — a second testnet account beside ALT.
fn vars_with_beta(default: &[&str], alt_tiers: &[&str]) -> HashMap<String, String> {
    let mut vars = vars(default, alt_tiers);
    vars.insert(
        account_key(&format!("{}_DEMO_PRIVATE_KEY", "hyperliquid".to_uppercase()), &beta()),
        FAKE_SECRET.to_string(),
    );
    vars
}

/// [`venues`] plus BETA's own line, `demo`.
fn venues_with_beta(alt_line: Option<VenueMode>) -> VenuePolicy {
    venues(alt_line).declare_account("hyperliquid", &beta(), VenueMode::Demo)
}

fn plan(vars: &HashMap<String, String>, venues: &VenuePolicy) -> Network {
    let cfg = MakerMountConfig::crypto("hyperliquid", "BTC", 0.5, 0.001);
    let policy = MountPolicy { venues: venues.clone(), ..MountPolicy::default() };
    match venue_feed_plan(&cfg, vars, &policy).expect("a straddle is not a refusal") {
        VenuePlan::Hyperliquid(network) => network,
        other => panic!("hyperliquid planned {other:?}"),
    }
}

fn straddle_lines(log: &log_capture::Log) -> Vec<String> {
    log_capture::lines(log).into_iter().filter(|line| line.contains(STRADDLE)).collect()
}

#[test]
fn a_straddle_is_said_once_per_process_and_nothing_else_is() {
    let log = log_capture::install();

    // ── SILENCE. Every shape in which the armed accounts do NOT straddle.
    let demo = Some(VenueMode::Demo);
    let live = Some(VenueMode::Live);
    for (label, default, alt_tiers, alt_line, want) in [
        ("one account, live", LIVE, NONE, None, Network::Mainnet),
        ("ALT capped to demo but holding no keys", LIVE, NONE, demo, Network::Mainnet),
        (
            "ALT stated live and armed live beside a live default",
            LIVE,
            LIVE,
            live,
            Network::Mainnet,
        ),
        ("only ALT armed, on testnet", NONE, DEMO, demo, Network::Testnet),
        ("ALT capped to demo holding only live keys", LIVE, LIVE, demo, Network::Mainnet),
    ] {
        let got = plan(&vars(default, alt_tiers), &venues(alt_line));
        assert_eq!(got, want, "{label}");
        assert!(
            straddle_lines(&log).is_empty(),
            "{label}: no straddle, so nothing may be said: {:?}",
            straddle_lines(&log)
        );
    }

    // ── SPEECH, ONCE. A live default account beside an ALT capped to demo and armed on its demo
    // keys. The FIRST plan build that sees this straddle says it; every later build of the SAME
    // straddle (a second mount of the venue, a re-plan) says nothing more — one line per straddle per
    // process, not one per plan build.
    let first = {
        let before = straddle_lines(&log).len();
        let got = plan(&vars(LIVE, DEMO), &venues(demo));
        assert_eq!(got, Network::Mainnet, "mainnet wins, and the plan is not refused");
        let said = straddle_lines(&log);
        assert_eq!(said.len(), before + 1, "the first build says it, once: {said:?}");
        said.last().expect("one line").clone()
    };
    assert!(first.starts_with("[WARN] "), "loud means warn!, not info!: {first}");
    for needle in ["hyperliquid", "ALT", "MAINNET", "TESTNET", "policy.accounts.hyperliquid.ALT"] {
        assert!(first.contains(needle), "the warning must name {needle}: {first}");
    }
    assert!(!first.contains(FAKE_SECRET), "no credential value, ever: {first}");

    // THE SAME STRADDLE AGAIN. A second plan build over the identical accounts — and one over a
    // store whose default account merely holds more keys, which is the same straddle by content —
    // adds NOTHING, and still plans mainnet.
    for default in [LIVE, BOTH, LIVE] {
        let before = straddle_lines(&log).len();
        let got = plan(&vars(default, DEMO), &venues(demo));
        assert_eq!(got, Network::Mainnet, "the plan is the same every time");
        assert_eq!(
            straddle_lines(&log).len(),
            before,
            "a straddle already said in this process is not said again: {:?}",
            straddle_lines(&log)
        );
    }

    // A DIFFERENT STRADDLE is a different finding and is said: a second testnet account (BETA)
    // changes WHICH accounts are priced off the wrong book, so the operator's picture changes. And
    // that one, in turn, is said once.
    for round in 0..2 {
        let before = straddle_lines(&log).len();
        let got = plan(&vars_with_beta(LIVE, DEMO), &venues_with_beta(demo));
        assert_eq!(got, Network::Mainnet);
        let said = straddle_lines(&log);
        let expected = before + usize::from(round == 0);
        assert_eq!(
            said.len(),
            expected,
            "round {round}: a new straddle says itself once, then is silent: {said:?}"
        );
        if round == 0 {
            let line = said.last().expect("one line");
            assert!(line.contains("ALT") && line.contains("BETA"), "names both accounts: {line}");
        }
    }
}
