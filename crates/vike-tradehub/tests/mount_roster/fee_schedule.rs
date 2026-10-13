//! The fee-schedule, margin-mode and budget tests of `vike-mount`'s generic fold that drive it with
//! real venue ids. They lived in `crates/vike-mount/src/fee_schedule_tests.rs`, which keeps the
//! tests that name no venue row, until the venue mount contract finished (docs/decisions/0096).

use vike_tradehub::registry::REGISTRY;

use crate::permissive_grid::assert_permissive_grid;

/// The account a mount reported NOT RUNNING (decision 0156), or a panic naming what came back.
fn not_running(
    venue: &str,
    out: Result<vike_mount::EngineAndRecon, vike_mount::MountError>,
) -> vike_mount::NotRunning {
    match out {
        Err(vike_mount::MountError::NotRunning(stopped)) => stopped,
        Err(other) => panic!("{venue}: expected NOT RUNNING, got the refusal: {other}"),
        Ok((engine, _)) => {
            panic!("{venue}: expected NOT RUNNING, got an engine at {}", engine.tier)
        }
    }
}

/// ROSTER-PARAMETERIZED inert-default contract — the successor to the six hand-written
/// near-duplicate `*_absent_creds_stays_paper_and_inert` twins (binance/ctrader/alpaca/ig/oanda/
/// ibkr). For EVERY venue in the canonical `vike_model::VENUES` roster, mounting with an EMPTY
/// `.env` map and no account holding a field — no API key, agent-wallet private key or OAuth grant
/// anywhere — must
/// yield the byte-identical PAPER engine: NO `ReconClient` handle (a paper venue never
/// reconciles); NOT marked live (`live_venues` stays empty); tagged with the venue's static
/// published fee schedule (a paper venue has no recon, so `resolve_fee_schedule` returns
/// `static_default` == `fee_schedule_for(venue)` — the invariant the retired
/// `make_engine_tags_paper_engine_with_resolved_schedule` pinned for binance, now generalized to
/// the whole roster); and a PERMISSIVE RiskGate grid (`from_properties` never ran → every
/// constraint field `None`).
///
/// It also touches NO network: with an empty map every bridge's `VenueMount::resolve` declines
/// before its `mount` is asked for anything (the registry's own
/// `with_no_credentials_every_contract_row_resolves_and_mounts_paper` pins that row by row), and a
/// `FeatureAbsent` row builds nothing.
///
/// `recon_enabled` is deliberately `true` here, NOT `false`: the assertions are about ABSENT
/// CREDENTIALS being the live gate, and passing `false` would satisfy the reconcile half through
/// the global reconcile gate instead. With the gate on and an empty `.env`, every bridge's
/// `resolve` still declines first, so no reconcile factory is reached and the test stays offline.
///
/// Iterating the roster is the whole point: a newly-added bridge crate (its id landing in
/// `VENUES`) is AUTOMATICALLY held to this contract with ZERO new test code — the copy-drift
/// surface the six hand-written twins were is gone (the `venues.rs`/`fees.rs` completeness-gate
/// idiom, applied to the mount contract). aster is NOT special-cased despite being the one venue
/// whose live tier arms on credential PRESENCE alone: with empty vars it resolves no
/// agent-wallet creds, so the loop is uniform.
/// ibkr, fxcm and polymarket are `FeatureAbsent` in the default build this module runs in, so the
/// loop reaches each as the generic `FeatureAbsent` row; ibkr's bridge's own no-credential path is
/// `crates/bridges/vike-ibkr/src/mount/tests.rs`'s, and polymarket's no-key path is
/// `crates/bridges/polymarket/src/exec_plane/mount_contract_tests.rs`'s
/// `with_no_key_the_mount_is_paper_whatever_the_gates_say`.
#[test]
fn all_roster_venues_absent_creds_stay_paper_and_inert() {
    const SYMBOL: &str = "BTCUSDT";
    let (tx, _rx) = vike_exec::event_channel(16);
    let vars = std::collections::HashMap::new(); // empty .env ⇒ absent creds for every venue
    // ⚠ EVERY roster venue gets an active `live` account row here, deliberately: this test's
    // subject is the OTHER gate — absent credentials — and a default (no-row) policy would satisfy
    // every assertion below at the account table's early return, without any arm's own
    // cred/config check ever running.
    let armed = crate::support::all_armed_policy(vike_model::AccountTier::Live);
    for &venue in vike_model::VENUES {
        let mut live = std::collections::HashSet::new();
        let mut env = vike_mount::MountEnv::new(REGISTRY, &vars, &tx, &mut live);
        env.recon_enabled = true;
        env.policy = Some(&armed);
        let stopped = not_running(venue, vike_mount::make_engine(&mut env, venue, SYMBOL));
        assert!(!stopped.block.runs(), "{venue}: absent creds → {:?} does not run", stopped.block);
        assert_eq!(stopped.tier, Some(vike_model::AccountTier::Live), "{venue}: the row's tier");
        assert!(live.is_empty(), "{venue}: absent creds → venue not marked live");
    }
}

/// **The paper half of the inert contract, over the whole roster**: a `paper` account of EVERY
/// venue runs the paper simulator — NO `ReconClient` handle (a paper engine never reconciles), NOT
/// marked live, tagged with the venue's static published fee schedule (`resolve_fee_schedule`
/// returns `static_default` == `fee_schedule_for(venue)` with no recon), and a PERMISSIVE RiskGate
/// grid (`from_properties` never ran → every constraint field `None`). Offline: the account table
/// answers `paper` above every bridge.
#[test]
fn every_roster_venues_paper_account_runs_inert_with_its_fee_schedule() {
    const SYMBOL: &str = "BTCUSDT";
    let (tx, _rx) = vike_exec::event_channel(16);
    let vars = std::collections::HashMap::new();
    let paper = crate::support::all_armed_policy(vike_model::AccountTier::Paper);
    for &venue in vike_model::VENUES {
        let mut live = std::collections::HashSet::new();
        let mut env = vike_mount::MountEnv::new(REGISTRY, &vars, &tx, &mut live);
        env.recon_enabled = true;
        env.policy = Some(&paper);
        let (engine, recon) = vike_mount::make_engine(&mut env, venue, SYMBOL)
            .unwrap_or_else(|e| panic!("{venue}: a paper account runs: {e}"));
        assert_eq!(engine.tier, vike_model::AccountTier::Paper, "{venue}");
        assert!(recon.is_none(), "{venue}: a paper engine has no reconcile handle");
        assert!(live.is_empty(), "{venue}: a paper engine is not marked live");
        assert_eq!(
            engine.fee_schedule,
            Some(vike_venues::fee_schedule_for(venue)),
            "{venue}: paper engine tagged with the venue's static fee schedule"
        );
        assert_permissive_grid(venue, &engine.gate.limits);
    }
}

/// THE WIRING GATE for the fee LANE: `make_engine` must key the fee table off the LANE the
/// symbol routes to, not off the bare venue string.
///
/// The table's lane rows are worth nothing unless this call site passes them, and this is the
/// site that fills every `paper_client` fallback arm *and* supplies
/// `resolve_fee_schedule`'s fallback — so mounting `BTCUSDT.P` on binance used to charge the
/// SPOT 10/10 on a lane that costs 2/5 (5x the maker fee). Both symbol forms are asserted, so a
/// revert to `fee_schedule_for(venue)` fails on the `.P` case while an over-eager lane that ate
/// bare symbols fails on the other. The value pins live in `vike_venues::fees`
/// (`lane_rows_are_pinned`); this only proves the lane REACHES the engine.
#[test]
fn make_engine_keys_the_fee_schedule_off_the_symbol_lane() {
    let (tx, _rx) = vike_exec::event_channel(16);
    let vars = std::collections::HashMap::new(); // empty .env, no network
    // Every account a `paper` row: the simulator runs offline (decision 0156 — an account with no
    // usable key set would not run at all).
    let paper = crate::support::all_armed_policy(vike_model::AccountTier::Paper);
    let mount = |venue: &str, symbol: &str| {
        let mut live = std::collections::HashSet::new();
        // The fields `MountEnv::new` leaves `None`: no recon client, no properties recorder and no
        // operator risk_profile. This test is about which fee ROW the lane key selects, so every
        // other input stays at its absent default.
        let mut env = vike_mount::MountEnv::new(REGISTRY, &vars, &tx, &mut live);
        env.recon_enabled = true;
        env.policy = Some(&paper);
        vike_mount::make_engine(&mut env, venue, symbol)
            .unwrap_or_else(|e| panic!("{venue}/{symbol}: paper mount must start: {e}"))
            .0
            .fee_schedule
            .expect("make_engine always tags a schedule")
    };
    for venue in ["binance", "aster"] {
        assert_eq!(
            mount(venue, "BTCUSDT.P"),
            vike_venues::fee_schedule_for(vike_catalog::fee_lane(venue, "BTCUSDT.P")),
            "{venue}: a `.P` mount must be tagged with its PERP lane's schedule"
        );
        assert_eq!(
            mount(venue, "BTCUSDT"),
            vike_venues::fee_schedule_for(venue),
            "{venue}: a bare mount must stay byte-identical to the pre-lane behavior"
        );
    }
    // binance is the venue whose lanes are actually priced apart — assert the engine really
    // ends up with two DIFFERENT schedules, so a lane resolution that silently collapsed
    // (`fee_lane` returning the bare id, a reverted call site) cannot pass this test.
    assert_ne!(
        mount("binance", "BTCUSDT.P"),
        mount("binance", "BTCUSDT"),
        "binance perp and spot mounts must not share a fee schedule"
    );
    // A single-exec-lane venue is unaffected by the suffix (bybit's exec is linear-perp only).
    assert_eq!(mount("bybit", "BTCUSDT.P"), mount("bybit", "BTCUSDT"));
}

/// **THE WIRING GATE, MOUNT'S HALF: `margin_mode_grid` → `Account` → `apply_fill`.**
///
/// The DERIVATION half (venue `meta` → `vike_hyperliquid::mount::margin_mode`) is gated in the
/// bridge now — `crates/bridges/hyperliquid/src/mount_tests.rs`'s
/// `an_isolated_only_asset_resolves_isolated_and_an_ordinary_one_stays_cross` — because
/// `margin_mode_grid` is `vike-mount`-only and the layer rule runs the other way (a bridge may
/// not depend on `vike-mount`). This half drives the REAL bridge function across the crate
/// boundary into `vike-mount`'s fold — a real `meta` body → the real `Symbology` →
/// `vike_hyperliquid::mount::margin_mode` → `vike_mount::margin_mode_grid` → `Account` →
/// `apply_fill` — so derivation and fold are joined by the real function rather than by a copy of
/// it. The link between them inside a real mount — the value leaving the bridge as
/// `LiveExec::margin_mode` (`vike_hyperliquid::mount`'s `outcome_from_attempt`) and the fold
/// reading it (`contract`'s `parts_from_live`) — is proven apart: by the bridge's
/// `mount_contract_tests.rs`, and by `crates/vike-mount/src/contract_tests/outcome_fold.rs`'s
/// `a_live_outcome_folds_its_grid_legs_multiplier_and_margin_mode_into_the_engine`.
///
/// It moved from `vike-mount`'s `hyperliquid` module into that crate's `fee_schedule_tests` with
/// the venue mount contract's hyperliquid port, which deleted the module, and on to here with the
/// contract's finish, beside the other tests that name a bridge (docs/decisions/0096).
#[test]
fn an_isolated_only_asset_folds_isolated_through_the_real_mount_chain() {
    use vike_exec::{Account, BalanceMode};
    use vike_hyperliquid::base::symbology::Symbology;
    use vike_model::MarginMode;

    // Real 2026-08-05 row shapes: CASHCAT is the one isolated-only asset still live.
    let meta = serde_json::json!({"universe": [
        {"name": "BTC", "szDecimals": 5, "maxLeverage": 40},
        {"name": "CASHCAT", "szDecimals": 0, "maxLeverage": 3, "onlyIsolated": true},
    ]});
    let symbology = Symbology::from_meta(&meta, &serde_json::json!({}));

    let fill = |symbol: &str| vike_model::events::FillEvent {
        trade_id: "t1".into(),
        client_order_id: "c1".to_string(),
        venue: vike_hyperliquid::base::consts::VENUE.into(),
        symbol: symbol.into(),
        side: 1,
        last_qty: 1.0,
        last_px: 1.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts: 1,
        mark_price: None,
        position_side: vike_model::events::PositionSide::Both,
    };
    // Exactly what `assemble_engine` does with the margin mode a live outcome carries.
    let booked = |symbol: &str| {
        let mode = vike_hyperliquid::mount::margin_mode(&symbology, symbol);
        let mut account =
            Account::new(1.0, vike_hyperliquid::base::consts::VENUE, None, BalanceMode::Delta)
                .with_default_margin_modes(vike_mount::margin_mode_grid(symbol, mode));
        account.apply_fill(&fill(symbol));
        let key: vike_exec::PositionKey = (
            vike_hyperliquid::base::consts::VENUE.into(),
            symbol.into(),
            vike_model::events::PositionSide::Both,
        );
        account.positions[&key].margin_mode
    };

    assert_eq!(
        booked("CASHCAT"),
        MarginMode::Isolated,
        "isolated-only asset: the venue's per-ASSET truth must reach the fold"
    );
    assert_eq!(
        booked("BTC"),
        MarginMode::Cross,
        "ordinary asset in the SAME universe: unchanged, the per-venue default"
    );
    assert_eq!(
        booked("NOT-LISTED"),
        vike_venues::caps_for(vike_hyperliquid::base::consts::VENUE).default_margin_mode,
        "an unlisted symbol falls back to the per-venue declaration, not a literal"
    );
}

/// Required test 5 (the make_engine-level proof): a `paper` account succeeds with `Ok` even though
/// NEITHER account-dependent cap was ever supplied (no risk_profile at all) — `paper_engine` never
/// consults `require_live_risk_budget`. And the budget refusal is not what stops an account with no
/// usable key set either: a `demo` binance row over an empty store is NOT RUNNING with its own
/// credential block (decision 0156), never `MissingRiskBudget` — the refusal is for live intent.
#[test]
fn paper_mount_starts_with_no_account_dependent_budget_at_all() {
    let (tx, _rx) = vike_exec::event_channel(16);
    let no_creds = std::collections::HashMap::new();
    let mut live = std::collections::HashSet::new();
    // `new` leaves reconcile off — this test is about the risk budget, not recon — and no
    // operator risk_profile either.
    let paper = crate::support::armed_policy("binance", vike_model::AccountTier::Paper);
    let mut env = vike_mount::MountEnv::new(REGISTRY, &no_creds, &tx, &mut live);
    env.policy = Some(&paper);
    let (engine, _recon) = vike_mount::make_engine(&mut env, "binance", "BTCUSDT")
        .unwrap_or_else(|e| panic!("a paper account never refuses over an unset cap: {e}"));
    assert_eq!(engine.gate.limits.max_notional_per_order, None);
    assert_eq!(engine.gate.limits.max_total_exposure, None);

    // ⚠ binance ARMED by an active `demo` row, over the same empty store: ABSENT CREDENTIALS stop
    // the account before the budget refusal's own gate is consulted.
    let demo = crate::support::armed_policy("binance", vike_model::AccountTier::Demo);
    let mut env = vike_mount::MountEnv::new(REGISTRY, &no_creds, &tx, &mut live);
    env.policy = Some(&demo);
    let stopped = not_running("binance", vike_mount::make_engine(&mut env, "binance", "BTCUSDT"));
    assert_eq!(stopped.block, vike_config::ArmingBlock::NoCredentials);
    assert!(live.is_empty());
}
