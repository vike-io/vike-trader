//! The B11 live-account lock's two vike-run halves: the PRE-MOUNT armed set
//! ([`vike_run::armed_live_venues`]) the composition roots claim their sentinels from, and the
//! POST-MOUNT backstop ([`vike_run::refuse_unarmed_live_venues`]) that refuses a node in which
//! anything armed outside it.
//!
//! # Why the backstop is tested here and the SET is tested in vike-tradehub
//!
//! A meaningful armed set needs a `vike_config::VenuePolicy` naming a venue, and vike-run has no
//! vike-config edge (its `MountPolicy` arrives already projected). So the ceiling-aware behaviour
//! — a profile naming one venue while credentials arm several, a paper-capped venue with live
//! credentials claiming nothing — is gated where the ceiling type is nameable and the REAL mount
//! path is drivable: `crates/vike-tradehub/src/feed_splice_seam_tests.rs`'s
//! `the_live_account_locks_cover_the_armed_set_not_the_profiles_mount_set` and its ordering twin.
//! What is gated HERE is the part that needs no policy at all: the default's fail-safe answer, and
//! the backstop's own verdict in both directions.
//!
//! # What the backstop's firing can and cannot be driven by
//!
//! `build_node` calls it on every build (`crates/vike-run/tests/build_node_paper.rs` is the green
//! path — a paper node compares two empty sets and returns `Ok`). Driving it RED end-to-end would
//! need `build_node` to arm a venue live, and every live arm dials: `vike_mount`'s own
//! arming-ceiling suite records that reaching `live_venues.insert` means "a blocking instrument
//! fetch and a dialing exec thread", which is why that suite asserts the live side through the
//! pre-connect budget refusal instead. So the firing is driven through the function directly, with
//! the scenario it exists for — a live arm whose probe row does not exist.

use std::collections::HashSet;

use vike_run::{MountPolicy, WIRED_MARKETS, armed_live_venues, refuse_unarmed_live_venues};

fn set(venues: &[&str]) -> HashSet<String> {
    venues.iter().map(|v| (*v).to_string()).collect()
}

/// The armed side is `[String]` — ROUTE KEYS — so a literal list has to be owned. A venue id IS its
/// default account's route key, which is why every case below still reads as venue names.
fn owned(keys: &[&str]) -> Vec<String> {
    keys.iter().map(|k| (*k).to_string()).collect()
}

/// The no-`policy.toml` answer: **nothing arms, so nothing is locked.** `MountPolicy::default()`
/// caps every venue at `paper`, and a paper-capped venue has no live arm to probe — so this holds
/// on a box whose credential store is full, which is the property that makes a default-policy
/// deployment safe to start twice.
#[test]
fn the_default_ceiling_arms_nothing_so_no_account_lock_is_claimed() {
    // Credentials for the three CEX venues, in the shape their own live arms read. They are real
    // ARMING shapes — the point of the assertion is that the CEILING refuses them, not that the
    // map is empty (an empty map would make this test pass for the wrong reason).
    let vars = std::collections::HashMap::from([
        ("BINANCE_DEMO_API_KEY".to_string(), "k".to_string()),
        ("BINANCE_DEMO_API_SECRET".to_string(), "s".to_string()),
        ("BYBIT_DEMO_API_KEY".to_string(), "k".to_string()),
        ("BYBIT_DEMO_API_SECRET".to_string(), "s".to_string()),
    ]);
    // ANTI-VACUITY: the same map under the WIDEST ceiling does arm those venues, so the empty
    // answer below is the ceiling's doing and not a fixture that never armed anything.
    assert!(
        vike_mount::would_mount_live("binance", &vars),
        "the fixture must be a map that ARMS, or this test proves nothing"
    );
    assert!(vike_mount::would_mount_live("bybit", &vars));

    assert!(
        armed_live_venues(&vars, &MountPolicy::default()).is_empty(),
        "no policy.toml ⇒ every venue paper ⇒ no venue arms ⇒ no live-account lock is claimed"
    );
}

/// The armed set can only ever name venues this node actually MOUNTS. A row outside
/// [`WIRED_MARKETS`] could never appear in `build_node`'s `live_venues`, so locking one would
/// refuse a second process over an account this binary cannot trade.
#[test]
fn the_armed_set_is_a_subset_of_the_wired_markets_table() {
    let vars = std::collections::HashMap::new();
    let armed = armed_live_venues(&vars, &MountPolicy::default());
    for v in &armed {
        // ⚠ The armed set is ROUTE KEYS now (`binance`, `binance#ALT`), so the venue half is the
        // part before any `#`. With no `[accounts]` table and an empty store the two are the same
        // string, which is what makes this test the same test it was.
        let v = v.split('#').next().expect("a non-empty route key");
        assert!(
            WIRED_MARKETS.iter().any(|(w, _)| *w == v),
            "{v} is not a wired market — build_node mounts no engine for it, so it can never arm"
        );
    }
}

/// **THE BACKSTOP FIRES ON AN UNDER-COUNT** — the failure mode the lock cannot survive.
///
/// The scenario is the one `vike_mount::would_mount_live_under`'s own doc predicts: a live arm
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
        msg.contains("would_mount_live_under"),
        "…and where the missing row belongs, which is the only fix: {msg}"
    );
}

/// Every unlocked venue is named, not just the first — a probe that fell behind two arms must be
/// fixable in one cycle, the same reason `vike_mount::require_live_risk_budget` lists every
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
/// (hyperliquid/polymarket) probes live and mounts paper. Refusing that direction would fail an
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
