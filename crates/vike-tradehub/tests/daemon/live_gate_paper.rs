//! LIVE-gate SAFETY test over the PAPER tier (headless two-layer plan, Layer 2 — live-strategy).
//!
//! Proves the load-bearing SAFE property of the opt-in LIVE path: with the LIVE builder exercised but
//! an EMPTY `.env` (no per-venue creds), [`vike_tradehub_core::build_live_strategy_core`] over the A-S maker — the core the
//! daemon starts under `VIKE_TRADEHUB_LIVE=1` — stands up the wired-market `assemble_runtime` core with
//! EVERY venue's account at tier `paper`, on the paper simulator. NO creds, NO network, NO
//! live venue.
//!
//! The daemon's own `live_runtime` (which additionally wires the Hyperliquid keyless feed) is NOT
//! exercised here — that would touch the network — so this test models
//! `crates/vike-tradehub/tests/daemon/assemble_runtime_paper.rs` and asserts the mount-assembly half only: the
//! maker's `Mount` folds into a `assemble_runtime` core that mounts all-paper without creds.

use std::collections::HashMap;

use vike_strategies_check::makers::spread_maker::SpreadMaker;
use vike_tradehub_core::test_support::paper_maker::outcome_maker;
use vike_tradehub_core::{MountPlacement, RuntimeConfig, build_live_strategy_core};

// The paper node is the node of a box whose table says `paper` for each account it starts
// (`crate::support::paper_policy`).
use crate::support::paper_policy;

/// assemble_runtime hardcodes Hyperliquid's market as "BTC" — the daemon's only live-wired symbol.
const TOKEN: &str = "BTC";
/// Far-future resolution so the A-S horizon is positive (mirrors the other start tests).
const RESOLUTION_TIMESTAMP: i64 = 3_000_000_000;

#[test]
fn live_maker_core_over_empty_credentials_starts_all_paper_and_the_core_is_live() {
    vike_log::test_init();

    // The A-S maker on a Hyperliquid BTC mount.
    let config = MountPlacement::new("hyperliquid", TOKEN, 1_000.0, 0.01);

    // Build the LIVE core over an EMPTY credentials map: absent creds ⇒ every venue paper, no network.
    // The RuntimeConfig carries the same shape the daemon's `live_runtime` builds — minus the live feed,
    // which is what keeps this test network-free.
    let node = build_live_strategy_core(
        Box::new(outcome_maker(SpreadMaker::from_params, Some(RESOLUTION_TIMESTAMP))),
        &config,
        RuntimeConfig {
            backend: crate::local_backend::rig(&HashMap::new(), Some(&paper_policy())).backend(),
            vars: HashMap::new(), // empty .env ⇒ absent creds for every venue ⇒ paper (no network)
            properties_rec: None, // recorder construction is the binary's job; None = disabled path
            seed_cash: config.seed_cash,
            reconcile_enabled: false,
            core_config: vike_core::CoreConfig {
                seed_cash: config.seed_cash,
                ..vike_core::CoreConfig::default()
            },
            risk_profile: None, // no operator profile configured — byte-identical to pre-wiring
            // No `policy` setting rows on this machine (settings-unification Phase 6c), so every
            // venue keeps its own compiled-in literal — and a `paper` account row per venue.
            policy: paper_policy(),
        },
    )
    .expect("the live maker core builds with no creds and no network")
    .runtime;

    assert!(node.live_venues.is_empty(), "empty creds ⇒ no venue mounts a live exec client");
    assert!(node.reconcile_clients.is_empty(), "paper mount ⇒ no venue has a reconcile handle");
    assert!(node.reconcile_trigger.is_none(), "recon disabled ⇒ no reconnect-trigger channel");
    assert!(node.handle.is_alive(), "the single-writer core thread spawned and is running");

    // Clean teardown in the load-bearing order: raise the forwarder stop BEFORE the core shutdown+join
    // (see `crates/vike-tradehub-core/src/node.rs`'s forwarder teardown-safety note).
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
/// (`crates/vike-venues/src/venue_wiring_tests.rs`'s `the_core_holds_the_default_engines_in_rank_order`)
/// already names them — spelling all thirteen here would make this file read to
/// `crates/vike-ops/tests/venues/new_venue_gate.rs` as a second per-venue table.
///
/// Read off the snapshot the core publishes as it EXITS, not polled while it runs: an idle paper
/// core is never dirtied by a message, so it publishes nothing until its teardown, and the
/// teardown always publishes once (`CoreThread::run`'s final `publish_guarded`). The read is
/// therefore deterministic.
#[test]
fn the_paper_node_holds_its_default_engines_in_engine_order() {
    let node = vike_tradehub_core::assemble_runtime(RuntimeConfig {
        backend: crate::local_backend::rig(&HashMap::new(), Some(&paper_policy())).backend(),
        vars: HashMap::new(), // absent creds ⇒ every venue paper, no network
        properties_rec: None,
        seed_cash: 10_000.0,
        reconcile_enabled: false,
        core_config: vike_core::CoreConfig::default(),
        risk_profile: None,
        policy: paper_policy(),
    })
    .expect("paper runtime builds with no creds and no network");
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
    let mut ranked = crate::local_backend::wired_markets();
    ranked.sort_by_key(|m| m.engine_rank);
    let want_optional: Vec<&str> = ranked.iter().skip(10).map(|m| m.venue).collect();
    assert_eq!(optional, want_optional, "this build's optional engines follow, in rank order");
}
