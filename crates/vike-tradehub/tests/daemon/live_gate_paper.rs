//! LIVE-gate SAFETY test over the PAPER fallback (headless two-layer plan, Layer 2 — live-strategy).
//!
//! Proves the load-bearing SAFE property of the opt-in LIVE path: with the LIVE builder exercised but
//! an EMPTY `.env` (no per-venue creds), [`vike_run::build_live_maker_core`] — the exact core the
//! daemon mounts under `VIKE_TRADEHUB_LIVE=1` — stands up the twelve-venue `build_node` core with
//! EVERY venue on its PAPER fallback (absent credentials ARE the live gate). NO creds, NO network, NO
//! live venue.
//!
//! The daemon's own `live_mount` (which additionally wires the Hyperliquid keyless feed) is NOT
//! exercised here — that would touch the network — so this test models
//! `crates/vike-run/tests/build_node_paper.rs` and asserts the mount-assembly half only: the maker's
//! `StrategyMount` folds into a `build_node` core that mounts all-paper without creds.

use std::collections::HashMap;

use vike_run::{MakerMountConfig, NodeConfig, build_live_maker_core};

/// build_node hardcodes Hyperliquid's market as "BTC" — the daemon's only live-wired symbol.
const TOKEN: &str = "BTC";
/// Far-future resolution so the A-S horizon is positive (mirrors the other mount tests).
const RESOLUTION_TS: i64 = 3_000_000_000;

#[test]
fn live_maker_core_over_empty_creds_mounts_all_paper_and_the_core_is_live() {
    vike_log::test_init();

    // The A-S maker config the daemon builds for a Hyperliquid BTC mount.
    let mut cfg = MakerMountConfig::polymarket(TOKEN, Some(RESOLUTION_TS));
    cfg.venue = "hyperliquid".to_string();

    // Build the LIVE core over an EMPTY credentials map: absent creds ⇒ every venue paper, no network.
    // The NodeConfig carries the same shape the daemon's `live_mount` builds — minus the live feed,
    // which is what keeps this test network-free.
    let node = build_live_maker_core(
        &cfg,
        NodeConfig {
            vars: HashMap::new(), // empty .env ⇒ absent creds for every venue ⇒ paper (no network)
            properties_rec: None, // recorder construction is the binary's job; None = disabled path
            seed_cash: cfg.seed_cash,
            recon_enabled: false,
            core_config: vike_core::CoreConfig {
                seed_cash: cfg.seed_cash,
                ..vike_core::CoreConfig::default()
            },
            risk_profile: None, // no operator profile configured — byte-identical to pre-wiring
            // No `policy.toml` on this machine (settings-unification Phase 6c).
            // `MountPolicy::default()` IS that answer, so every venue keeps its own compiled-in
            // literal and the assertions below are unchanged.
            policy: vike_run::MountPolicy::default(),
        },
    )
    .expect("the live maker core builds with no creds and no network")
    .node;

    assert!(node.live_venues.is_empty(), "empty creds ⇒ no venue mounts a live exec client");
    assert!(node.recon_clients.is_empty(), "paper mount ⇒ no venue has a reconcile handle");
    assert!(node.recon_trigger.is_none(), "recon disabled ⇒ no reconnect-trigger channel");
    assert!(node.handle.is_alive(), "the single-writer core thread spawned and is running");

    // Clean teardown in the load-bearing order: raise the forwarder stop BEFORE the core shutdown+join
    // (see `vike_run::node`'s forwarder teardown-safety note).
    node.forwarder_stop.store(true, std::sync::atomic::Ordering::Relaxed);
    node.handle.shutdown_and_join();
}
