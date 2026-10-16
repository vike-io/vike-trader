//! The B11 live-account lock's two node-assembly halves (vike-run's when this file was written;
//! `vike-tradehub-core`'s since docs/decisions/0098 merged the two): the PRE-MOUNT armed set
//! ([`vike_tradehub_core::armed_live_venues`]) the composition roots claim their sentinels from, and the
//! POST-MOUNT backstop ([`vike_tradehub_core::refuse_unarmed_live_venues`]) that refuses a node in which
//! anything armed outside it.
//!
//! # Why the backstop is tested here and the SET is tested in vike-tradehub
//!
//! A meaningful armed set needs an `account` table naming a venue's account, and vike-run had no
//! vike-config edge (its `AdapterPolicy` arrived already projected). So the tier-aware behaviour — a
//! profile naming one venue while credentials arm several, a paper-held account with live
//! credentials claiming nothing — is gated where the REAL mount path is drivable:
//! `crates/vike-tradehub/src/tradehub_cli/tests/feed_splice/account_locks.rs`'s
//! `the_armed_set_is_the_accounts_that_arm_and_this_process_claims_no_lock_for_it` (the armed set, and that no
//! sentinel is written here) and `a_lease_another_client_holds_refuses_the_whole_live_mount` (the refusal that replaced the
//! lock's); the gatewayhub claims the sentinel itself now.
//! What is gated HERE is the default's fail-safe answer (with its armed control), and the
//! backstop's own verdict in both directions.
//!
//! # What the backstop's firing can and cannot be driven by
//!
//! `assemble_runtime` calls it on every build (`crates/vike-tradehub/tests/assemble_runtime_paper.rs` is the
//! green path — a paper node compares two empty sets and returns `Ok`). Driving it RED end-to-end
//! would need `assemble_runtime` to arm a venue live, and every live arm dials: the mount's
//! account-tier suite (`crates/vike-gatewayhub/tests/trade_roster/mount_roster/preconnect.rs`, vike-tradehub-core's own
//! until the venue-adapter contract finished) records that reaching `live_venues.insert` means "a
//! blocking instrument fetch and a dialing exec thread", which is why that suite asserts the live
//! side through the pre-connect budget refusal instead. So the firing is driven through the
//! function directly, with the scenario it exists for — a live arm whose probe row does not exist.
//!
//! Moved from `crates/vike-run/tests/` when the venue-adapter contract finished
//! (docs/decisions/0096): `vike-run` held no registry, so it ran through the transitional one until
//! then. Default build only: `vike-tradehub-core`'s transitional registry carried ibkr, fxcm and polymarket
//! `FeatureAbsent` in every build and these assertions were written against that; each venue's
//! feature-on half is its own `crates/vike-gatewayhub/tests/trade_roster/ibkr_adapter.rs`,
//! `crates/vike-gatewayhub/tests/trade_roster/fxcm_adapter.rs` or `crates/vike-gatewayhub/tests/trade_roster/polymarket_adapter.rs`.
//! That crate-level `#![cfg]` also makes this its own test binary rather than a `daemon` member.
//! ⚠ It narrows where this runs: in `crates/vike-run/tests/` it also ran in the fxcm, ibkr and
//! polymarket-stack lanes, with vike-run's marker feature on. It runs in the default lane only
//! now; `crates/vike-tradehub/tests/daemon/live_gate_paper.rs` still drives `assemble_runtime` in those
//! lanes.
//! (W15: this crate has no venue features any more; the scripted table fixes ibkr, fxcm and polymarket
//! absent, as the default gatewayhub build answers, so these assertions run in every build.)

// The in-process backend the roster tests drive the fold through (wave 2, W9a).
#[path = "support/local_backend.rs"]
mod local;

use std::collections::HashSet;

use local::{armed_live_venues, wired_markets};
use vike_tradehub_core::{AdapterPolicy, refuse_unarmed_live_venues};

fn set(venues: &[&str]) -> HashSet<String> {
    venues.iter().map(|v| (*v).to_string()).collect()
}

/// The armed side is `[String]` — ROUTE KEYS — so a literal list has to be owned. A venue id IS its
/// default account's route key, which is why every case below still reads as venue names.
fn owned(keys: &[&str]) -> Vec<String> {
    keys.iter().map(|k| (*k).to_string()).collect()
}

/// The no-`account`-row answer: **nothing arms, so nothing is locked.** `AdapterPolicy::default()`
/// carries the `account` table UNREAD, so every account runs paper (`NoAccountRow`) before any
/// credential is read — and this holds on a box whose credential store is full, which is the
/// property that makes a default-policy deployment safe to start twice.
#[test]
fn no_active_account_row_arms_nothing_so_no_account_lock_is_claimed() {
    use vike_model::accounts::account_keys::AccountLabel;
    // Credentials for two CEX venues, in the shape their own live arms read. They are real
    // ARMING shapes — the point of the assertion is that the ACCOUNT TABLE refuses them, not that
    // the map is empty (an empty map would make this test pass for the wrong reason).
    //
    // Decision 0095: LIVE-tier, not DEMO-tier — a `live` account on binance/bybit requires
    // LIVE-tier keys to reach anything but Paper.
    let vars = std::collections::HashMap::new();
    let keys = [("API_KEY", "k"), ("API_SECRET", "s")];
    // ANTI-VACUITY: the same map with an active `live` row per venue DOES arm both, so the empty
    // answer below is the account table's doing and not a fixture that never armed anything.
    let armed_rows = AdapterPolicy::default()
        .with_credentials("binance", &AccountLabel::Default, vike_model::AccountTier::Live, &keys)
        .with_credentials("bybit", &AccountLabel::Default, vike_model::AccountTier::Live, &keys);
    let armed = armed_live_venues(&vars, &armed_rows);
    assert!(
        armed.iter().any(|k| k == "binance") && armed.iter().any(|k| k == "bybit"),
        "the fixture must be a map that ARMS under active rows, or this test proves nothing: \
         {armed:?}"
    );

    assert!(
        armed_live_venues(&vars, &AdapterPolicy::default()).is_empty(),
        "no account row ⇒ every account paper ⇒ nothing arms ⇒ no live-account lock is claimed"
    );
}

/// The armed set can only ever name venues this node actually MOUNTS. A row outside
/// [`wired_markets`] could never appear in `assemble_runtime`'s `live_venues`, so locking one would
/// refuse a second process over an account this binary cannot trade.
#[test]
fn the_armed_set_is_a_subset_of_the_wired_markets() {
    let vars = std::collections::HashMap::new();
    let armed = armed_live_venues(&vars, &AdapterPolicy::default());
    let mounted = wired_markets();
    for v in &armed {
        // ⚠ The armed set is ROUTE KEYS now (`binance`, `binance#ALT`), so the venue half is the
        // part before any `#`. With no labelled account and an empty store the two are the same
        // string, which is what makes this test the same test it was.
        let v = v.split('#').next().expect("a non-empty route key");
        assert!(
            mounted.iter().any(|m| m.venue == v),
            "{v} is not a wired market — assemble_runtime builds no engine for it, so it can never arm"
        );
    }
}

/// **THE BACKSTOP FIRES ON AN UNDER-COUNT** — the failure mode the lock cannot survive.
///
/// The scenario is the one `vike_tradehub_core::would_go_live_under`'s own doc predicts: a live arm
/// added to `make_engine` without a matching probe row. The venue then arms, places real orders,
/// and no sentinel was ever claimed for its account — a startup that looks completely correct.
/// `dukascopy` stands in for it because it is a real roster venue that genuinely has no probe row
/// today, so this is the shape of the next such arm rather than an invented one.
#[test]
fn the_backstop_refuses_a_venue_that_armed_outside_the_locked_set() {
    let armed = owned(&["ig", "oanda"]);
    let live = set(&["ig", "oanda", "dukascopy"]);
    let err = refuse_unarmed_live_venues(&armed, &live)
        .expect_err("a venue armed with no lock held must refuse the node");
    let msg = err.to_string();
    assert!(msg.contains("dukascopy"), "the refusal must NAME the unlocked venue: {msg}");
    assert!(
        msg.contains("ig") && msg.contains("oanda"),
        "…and the set that WAS locked, so the reader can see the gap: {msg}"
    );
    assert!(
        msg.contains("would_go_live_under"),
        "…and where the missing row belongs, which is the only fix: {msg}"
    );
}

/// Every unlocked venue is named, not just the first — a probe that fell behind two arms must be
/// fixable in one cycle, the same reason `vike_tradehub_core::require_live_risk_budget` lists every
/// missing cap at once.
#[test]
fn the_backstop_names_every_unlocked_venue_sorted() {
    let err =
        refuse_unarmed_live_venues(&owned(&["binance"]), &set(&["binance", "oanda", "dukascopy"]))
            .expect_err("two unlocked venues still refuse");
    let msg = err.to_string();
    let duka = msg.find("dukascopy").expect("dukascopy named");
    let oanda = msg.find("oanda").expect("oanda named");
    assert!(duka < oanda, "the venue list is sorted, so the message is deterministic: {msg}");
}

/// **The over-count is PERMITTED, deliberately.** The probe is intent-based: a venue whose
/// synchronous connect fails (ctrader/ibkr) or whose factory declines a present-but-bad key
/// (hyperliquid/polymarket) probes live and runs paper. Refusing that direction would fail an
/// ordinary bad-key startup, and it is the SAFE direction anyway — the sentinel was claimed, so
/// nothing is unlocked.
#[test]
fn the_backstop_permits_an_armed_set_wider_than_the_mount_record() {
    refuse_unarmed_live_venues(&owned(&["ig", "oanda", "ctrader"]), &set(&["ig"]))
        .expect("a claimed-but-unused lock is the accepted over-count, not a fault");
    refuse_unarmed_live_venues(&owned(&["binance"]), &HashSet::new())
        .expect("a paper mount under armed credentials refuses nothing");
    refuse_unarmed_live_venues(&[], &HashSet::new()).expect("the paper node's no-op comparison");
}
