//! LIVE-gate SAFETY test over the PAPER fallback (headless two-layer plan, Layer 2 — live-strategy).
//!
//! Proves the load-bearing SAFE property of the opt-in LIVE path: with the LIVE builder exercised but
//! an EMPTY `.env` (no per-venue creds), [`vike_mount::build_live_maker_core`] — the exact core the
//! daemon mounts under `VIKE_TRADEHUB_LIVE=1` — stands up the wired-market `build_node` core with
//! EVERY venue on its PAPER fallback (absent credentials ARE the live gate). NO creds, NO network, NO
//! live venue.
//!
//! The daemon's own `live_mount` (which additionally wires the Hyperliquid keyless feed) is NOT
//! exercised here — that would touch the network — so this test models
//! `crates/vike-tradehub/tests/build_node_paper.rs` and asserts the mount-assembly half only: the
//! maker's `StrategyMount` folds into a `build_node` core that mounts all-paper without creds.

use std::collections::HashMap;

use vike_mount::{MakerMountConfig, NodeConfig, build_live_maker_core};

/// build_node hardcodes Hyperliquid's market as "BTC" — the daemon's only live-wired symbol.
const TOKEN: &str = "BTC";
/// Far-future resolution so the A-S horizon is positive (mirrors the other mount tests).
const RESOLUTION_TS: i64 = 3_000_000_000;

#[test]
fn live_maker_core_over_empty_creds_mounts_all_paper_and_the_core_is_live() {
    vike_log::test_init();

    // The A-S maker config the daemon builds for a Hyperliquid BTC mount.
    let mut cfg = MakerMountConfig::outcome_token("polymarket", TOKEN, Some(RESOLUTION_TS));
    cfg.venue = "hyperliquid".to_string();

    // Build the LIVE core over an EMPTY credentials map: absent creds ⇒ every venue paper, no network.
    // The NodeConfig carries the same shape the daemon's `live_mount` builds — minus the live feed,
    // which is what keeps this test network-free.
    let node = build_live_maker_core(
        &cfg,
        NodeConfig {
            registry: vike_tradehub::registry::REGISTRY,
            markets: vike_tradehub::wired_markets::WIRED_MARKETS,
            vars: HashMap::new(), // empty .env ⇒ absent creds for every venue ⇒ paper (no network)
            properties_rec: None, // recorder construction is the binary's job; None = disabled path
            seed_cash: cfg.seed_cash,
            recon_enabled: false,
            core_config: vike_core::CoreConfig {
                seed_cash: cfg.seed_cash,
                ..vike_core::CoreConfig::default()
            },
            risk_profile: None, // no operator profile configured — byte-identical to pre-wiring
            // No `policy` rows on this machine (settings-unification Phase 6c).
            // `MountPolicy::default()` IS that answer, so every venue keeps its own compiled-in
            // literal and the assertions below are unchanged.
            policy: vike_mount::MountPolicy::default(),
        },
    )
    .expect("the live maker core builds with no creds and no network")
    .node;

    assert!(node.live_venues.is_empty(), "empty creds ⇒ no venue mounts a live exec client");
    assert!(node.recon_clients.is_empty(), "paper mount ⇒ no venue has a reconcile handle");
    assert!(node.recon_trigger.is_none(), "recon disabled ⇒ no reconnect-trigger channel");
    assert!(node.handle.is_alive(), "the single-writer core thread spawned and is running");

    // Clean teardown in the load-bearing order: raise the forwarder stop BEFORE the core shutdown+join
    // (see `crates/vike-mount/src/node.rs`'s forwarder teardown-safety note).
    node.forwarder_stop.store(true, std::sync::atomic::Ordering::Relaxed);
    node.handle.shutdown_and_join();
}

/// The order the core holds the DEFAULT engines in: `CoreSnapshot::portfolio`'s `venues`, one
/// block per engine, primary first, and the order every per-venue `py_sum` folds in. Pinned
/// from the hand-unrolled assembly before docs/decisions/0098 turned it into a table; it must
/// not move.
///
/// The ten default engines are spelled out; in a feature lane each optional wired venue follows
/// them in its `engine_rank` order, taken from the table, because the table's own literal pin
/// (`crates/vike-tradehub/src/wired_markets.rs`'s `the_core_holds_the_default_engines_in_rank_order`)
/// already names them — spelling all thirteen here would make this file read to
/// `crates/vike-ops/tests/venues/new_venue_gate.rs` as a second per-venue table.
///
/// Read off the snapshot the core publishes as it EXITS, not polled while it runs: an idle paper
/// core is never dirtied by a message, so it publishes nothing until its teardown, and the
/// teardown always publishes once (`CoreThread::run`'s final `publish_guarded`). The read is
/// therefore deterministic.
#[test]
fn the_paper_node_holds_its_default_engines_in_engine_order() {
    let node = vike_mount::build_node(NodeConfig {
        registry: vike_tradehub::registry::REGISTRY,
        markets: vike_tradehub::wired_markets::WIRED_MARKETS,
        vars: HashMap::new(), // absent creds ⇒ every venue paper, no network
        properties_rec: None,
        seed_cash: 10_000.0,
        recon_enabled: false,
        core_config: vike_core::CoreConfig::default(),
        risk_profile: None,
        policy: vike_mount::MountPolicy::default(),
    })
    .expect("paper node builds with no creds and no network");
    let cell = node.handle.snapshot_cell();
    node.forwarder_stop.store(true, std::sync::atomic::Ordering::Relaxed);
    node.handle.shutdown_and_join();

    let venues: Vec<String> =
        cell.load_full().portfolio.venues.iter().map(|b| b.venue.clone()).collect();
    let (default, optional) = venues.split_at(venues.len().min(10));
    assert_eq!(
        default,
        [
            "binance",
            "bybit",
            "okx",
            "hyperliquid",
            "aster",
            "deribit",
            "alpaca",
            "ctrader",
            "ig",
            "oanda"
        ],
        "the core's engine order is a behaviour: it orders every cross-venue fold"
    );
    let mut ranked = vike_tradehub::wired_markets::WIRED_MARKETS.to_vec();
    ranked.sort_by_key(|m| m.engine_rank);
    let want_optional: Vec<&str> = ranked.iter().skip(10).map(|m| m.venue).collect();
    assert_eq!(optional, want_optional, "this build's optional engines follow, in rank order");
}
