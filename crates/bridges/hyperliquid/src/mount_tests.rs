/// Decision 0095: a `live` ceiling is mainnet, anything below it testnet — no variable decides.
#[test]
fn the_ceiling_chooses_the_network() {
    use crate::config::{Env, Network};
    assert_eq!(Env::for_ceiling(true).network(), Network::Mainnet);
    assert_eq!(Env::for_ceiling(false).network(), Network::Testnet);
}

/// **THE SHARING GATE: one per-IP REST weight window per MOUNT, not one per transport.**
///
/// Hyperliquid meters every REST call it serves — keyless `/info` reads and signed `/exchange`
/// actions alike — out of a single per-IP budget, and
/// `vike_model::venue_rate_limits::HYPERLIQUID`'s `rest_ip_weight` admits exactly what the
/// venue publishes, so there is no headroom for a second window to hide in. Meanwhile
/// `crate::transport::HyperliquidTransport::new` mints a FRESH
/// `vike_bridge_core::ratelimit::RateGate` on every call. Those two facts together are the
/// hazard this pins: a mount that lets each consumer build its own transport runs N mutually
/// invisible full-budget windows, every one of them correctly reporting itself inside quota
/// while the process emits N times the venue's cap — answered with a 429 and then an IP ban,
/// from the box that also signs live orders. It was UNCONDITIONALLY two (the exec thread and
/// the funding poller) and three under `VIKE_RECONCILE=1`.
///
/// [`super::ip_gate_and_transport`] is where the mount refuses that, and both of its halves are
/// load-bearing: the handle goes to
/// `crate::exec::HyperliquidExecutionClient::spawn_with_market_slippage` (which clones it on
/// into the funding poller), and the transport is borrowed by the instruments load and then
/// owned by the `HyperliquidReconClient`. A `RateGate` is an `Arc` inside, so "same window" is
/// observable exactly the way `vike_bridge_core::ratelimit`'s `clone_shares_the_same_window`
/// observes it — spend through one handle, watch the spend show up on the other. Driven through
/// the REAL constructor rather than a rebuild of it, so substituting a fresh `ip_weight_gate()`
/// on either side turns this red.
#[test]
fn an_hl_mounts_rest_paths_ride_one_ip_weight_window() {
    use crate::config::Network;
    use crate::transport::HyperliquidTransport;

    // The budget is READ from the row that owns it, never restated here — this test cannot
    // drift from `vike_model::venue_rate_limits` and does not become a second authority for it.
    let budget = vike_model::venue_rate_limits::HYPERLIQUID.rest_ip_weight.admitted();

    // CONTROL — the defect, still reproducible on the raw constructor. Two transports built the
    // plain way share nothing, which is what makes the assertion below mean something rather
    // than merely pass.
    let a = HyperliquidTransport::new(Network::Testnet);
    let b = HyperliquidTransport::new(Network::Testnet);
    assert!(a.rate_gate().try_proceed_cost(budget), "a fresh window admits its whole budget");
    assert!(
        b.rate_gate().try_proceed(),
        "two plainly-constructed transports must be INDEPENDENT windows — if this ever fails, \
             the control is broken and the real assertion below proves nothing"
    );

    // THE ASSERTION — the mount's own seam, both halves, one window.
    let (exec_gate, transport) = super::ip_gate_and_transport(Network::Testnet);
    assert!(exec_gate.try_proceed_cost(budget), "the mount's window admits its whole budget");
    assert!(
        !transport.rate_gate().try_proceed(),
        "the exec half spent the whole per-IP budget, yet the instruments/recon transport \
             still admitted a call: the mount is running two full-budget windows against a venue \
             cap it declares with zero margin, each one reporting itself inside quota"
    );
}

/// **THE DERIVATION HALF of the wiring gate** — venue `meta` → [`super::margin_mode`], with no
/// network. The other half (`margin_mode` → `vike-mount`'s `margin_mode_grid` → `Account` →
/// `apply_fill`) cannot live here: `margin_mode_grid` is `vike-mount`-only, and the layer rule
/// runs the other way. `vike-mount`'s own wiring test drives THAT half against this function,
/// called across the crate boundary exactly as `("hyperliquid", _)` does — see its doc for why
/// splitting the test this way still proves the two halves stay connected.
#[test]
fn an_isolated_only_asset_resolves_isolated_and_an_ordinary_one_stays_cross() {
    use vike_model::MarginMode;

    // Real 2026-08-05 row shapes: CASHCAT is the one isolated-only asset still live.
    let meta = serde_json::json!({"universe": [
        {"name": "BTC", "szDecimals": 5, "maxLeverage": 40},
        {"name": "CASHCAT", "szDecimals": 0, "maxLeverage": 3, "onlyIsolated": true},
    ]});
    let symbology = crate::symbology::Symbology::from_meta(&meta, &serde_json::json!({}));

    assert_eq!(
        super::margin_mode(&symbology, "CASHCAT"),
        MarginMode::Isolated,
        "isolated-only asset: the venue's per-ASSET truth must reach the fold"
    );
    assert_eq!(
        super::margin_mode(&symbology, "BTC"),
        MarginMode::Cross,
        "ordinary asset in the SAME universe: unchanged, the per-venue default"
    );
    assert_eq!(
        super::margin_mode(&symbology, "NOT-LISTED"),
        vike_model::caps_for(crate::consts::VENUE).default_margin_mode,
        "an unlisted symbol falls back to the per-venue declaration, not a literal"
    );
}
