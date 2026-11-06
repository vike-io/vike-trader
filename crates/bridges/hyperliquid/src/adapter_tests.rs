/// Decision 0095: a `live` tier is mainnet, anything below it testnet — no variable decides.
#[test]
fn the_tier_chooses_the_network() {
    use crate::base::config::Network;
    use vike_model::AccountTier;
    assert_eq!(Network::for_tier(AccountTier::Live), Network::Mainnet);
    assert_eq!(Network::for_tier(AccountTier::Demo), Network::Testnet);
    assert_eq!(Network::for_tier(AccountTier::Paper), Network::Testnet, "never mainnet");
}

/// **THE SHARING GATE: one per-IP REST weight window per START, not one per transport.**
///
/// Hyperliquid meters every REST call it serves — keyless `/info` reads and signed `/exchange`
/// actions alike — out of a single per-IP budget, and
/// `vike_venues::venue_rate_limits::HYPERLIQUID`'s `rest_ip_weight` admits exactly what the
/// venue publishes, so there is no headroom for a second window to hide in. Meanwhile
/// `crate::base::transport::HyperliquidTransport::new` mints a FRESH
/// `vike_bridge_core::ratelimit::RateGate` on every call. Those two facts together are the
/// hazard this pins: a start that lets each consumer build its own transport runs N mutually
/// invisible full-budget windows, every one of them correctly reporting itself inside quota
/// while the process emits N times the venue's cap — answered with a 429 and then an IP ban,
/// from the box that also signs live orders. It was UNCONDITIONALLY two (the exec thread and
/// the funding poller) and three under `VIKE_RECONCILE=1`.
///
/// [`super::ip_gate_and_transport`] is where the start refuses that, and both of its halves are
/// load-bearing: the handle goes to
/// `crate::exec::HyperliquidExecutionClient::spawn_with_market_slippage` (which clones it on
/// into the funding poller), and the transport is borrowed by the instruments load and then
/// owned by the `HyperliquidReconClient`. A `RateGate` is an `Arc` inside, so "same window" is
/// observable exactly the way `vike_bridge_core::ratelimit`'s `clone_shares_the_same_window`
/// observes it — spend through one handle, watch the spend show up on the other. Driven through
/// the REAL constructor rather than a rebuild of it, so substituting a fresh `ip_weight_gate()`
/// on either side turns this red.
#[test]
fn an_hl_adapters_rest_paths_ride_one_ip_weight_window() {
    use crate::base::config::Network;
    use crate::base::transport::HyperliquidTransport;

    // The budget is READ from the row that owns it, never restated here — this test cannot
    // drift from `vike_venues::venue_rate_limits` and does not become a second authority for it.
    let budget = vike_venues::venue_rate_limits::HYPERLIQUID.rest_ip_weight.admitted();

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

    // THE ASSERTION — the start's own seam, both halves, one window.
    let (exec_gate, transport) = super::ip_gate_and_transport(Network::Testnet);
    assert!(exec_gate.try_proceed_cost(budget), "the mount's window admits its whole budget");
    assert!(
        !transport.rate_gate().try_proceed(),
        "the exec half spent the whole per-IP budget, yet the instruments/recon transport \
             still admitted a call: the mount is running two full-budget windows against a venue \
             cap it declares with zero margin, each one reporting itself inside quota"
    );
}

/// **The leverage confirmation's reads spend the ADAPTER's per-IP budget, not a fresh one**
/// (v0.1.71 review, finding 7): `confirmed_leverage_cap` used `HyperliquidTransport::new`, a whole new
/// window per ask. [`super::adapter_window_transport_in`] rides the newest window a start opened, and
/// only a network with no start yet opens one (the way a start does). Own book per test, so no other
/// test's start moves the window under it.
#[test]
fn the_leverage_reads_ride_the_newest_adapter_window() {
    use super::WindowBook;
    use crate::base::config::Network;

    let budget = vike_venues::venue_rate_limits::HYPERLIQUID.rest_ip_weight.admitted();
    let book = WindowBook { newest: std::sync::Mutex::default() };

    // no start yet: the read opens a window the way a start does, and a second read rides that one
    let first = super::adapter_window_transport_in(&book, Network::Testnet);
    assert!(first.rate_gate().try_proceed_cost(budget));
    let second = super::adapter_window_transport_in(&book, Network::Testnet);
    assert!(!second.rate_gate().try_proceed(), "two reads, one window");

    // a start opens a newer window: the next read charges THAT one
    let (exec_gate, _transport) = super::ip_gate_and_transport_in(&book, Network::Testnet);
    let read = super::adapter_window_transport_in(&book, Network::Testnet);
    assert!(exec_gate.try_proceed_cost(budget), "the start's window is full");
    assert!(!read.rate_gate().try_proceed(), "the read spent the adapter's budget, not its own");

    // another network has its own window
    let other = super::adapter_window_transport_in(&book, Network::Mainnet);
    assert!(other.rate_gate().try_proceed(), "mainnet is a different window");
}

/// **THE DERIVATION LINK of the wiring gate** — venue `meta` → [`super::margin_mode`], with no
/// network. The gate has THREE links, each proven on its own:
///
/// 1. **Derivation** — this test.
/// 2. **Construction** — the value leaving the one instruments load as a `LiveRuntime` field and then
///    as `LiveExec::margin_mode`: `adapter_contract_tests.rs`'s
///    `the_engine_symbols_ruling_margin_mode_reaches_the_outcome`, over a `Symbology::from_meta`
///    universe.
/// 3. **Fold** — `LiveExec::margin_mode` → `vike-tradehub-core`'s `margin_mode_grid` → `Account` →
///    `apply_fill`, which cannot live here (`margin_mode_grid` is `vike-tradehub-core`-only, and the layer
///    rule runs the other way): `crates/vike-gatewayhub/src/trade/adapters/contract_tests/outcome_fold.rs`'s
///    `a_live_outcome_folds_its_grid_legs_multiplier_and_margin_mode_into_the_engine` over a planted
///    row, and `crates/vike-gatewayhub/tests/trade_roster/mount_roster/fee_schedule.rs`'s
///    `an_isolated_only_asset_folds_isolated_through_the_real_start_chain` against this function.
///
/// ⚠ Nothing drives the three END TO END. The step that joins 1 to 2 inside a real start —
/// `live_runtime_for_account` handing its own freshly fetched `instruments` to `RuntimeFacts::of` — runs
/// only after a live `meta` fetch, so no offline test reaches it.
#[test]
fn an_isolated_only_asset_resolves_isolated_and_an_ordinary_one_stays_cross() {
    use vike_model::MarginMode;

    // Real 2026-08-05 row shapes: CASHCAT is the one isolated-only asset still live.
    let meta = serde_json::json!({"universe": [
        {"name": "BTC", "szDecimals": 5, "maxLeverage": 40},
        {"name": "CASHCAT", "szDecimals": 0, "maxLeverage": 3, "onlyIsolated": true},
    ]});
    let symbology = crate::base::symbology::Symbology::from_meta(&meta, &serde_json::json!({}));

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
        vike_venues::caps_for(crate::base::consts::VENUE).default_margin_mode,
        "an unlisted symbol falls back to the per-venue declaration, not a literal"
    );
}

/// **A key with a bad character declines the start into paper, and the log it writes names no character
/// of the key.** `Signer::from_private_key`'s `hex::decode` error names the offending character and its
/// position, and the start's warning used to print that error. The key is made of characters the
/// warning's own words never use (digits and capitals), so ANY of its characters in an event is a leak.
#[test]
fn a_malformed_key_declines_the_start_and_the_log_names_no_character_of_it() {
    use std::collections::HashMap;
    use vike_model::AccountTier;
    use vike_model::accounts::account_keys::AccountLabel;
    use vike_secrets::account_credentials::{AccountCredentials, CredentialFields};

    let key = "0123456789QZXWJV".repeat(4);
    // The control: the error this used to print DOES carry a character of the key.
    assert!(hex::decode(&key).unwrap_err().to_string().contains('Q'), "the control leaks");

    let fields = CredentialFields::from_pairs([(crate::base::config::PRIVATE_KEY, key.as_str())]);
    let creds = AccountCredentials::new(Some(1), Some(&fields), None);
    let (events_tx, _events_rx) = vike_exec::event_channel(8);
    let (attempt, events) = vike_log::capture::captured(|| {
        super::live_runtime_for_account(
            "BTC",
            &[],
            AccountTier::Demo,
            &creds,
            &HashMap::new(),
            &AccountLabel::Default,
            &events_tx,
            None,
            None,
            std::path::PathBuf::new(),
        )
    });
    assert!(attempt.live.is_none() && attempt.master.is_none(), "a malformed key stays paper");
    let warn = events
        .iter()
        .find(|e| e.message.contains("invalid private key"))
        .unwrap_or_else(|| panic!("the refusal is logged: {events:?}"));
    assert_eq!(warn.level, tracing::Level::WARN);
    for event in &events {
        let said =
            format!("{} {}", event.message, event.fields.values().cloned().collect::<String>());
        assert!(
            key.chars().all(|c| !said.contains(c)),
            "a character of the key reached the log: {said}"
        );
    }
}
