//! The fee-schedule, margin-mode and budget tests of `vike-mount`'s generic fold that drive it with
//! real venue ids. They lived in `crates/vike-mount/src/fee_schedule_tests.rs`, which keeps the
//! tests that name no venue row, until the venue mount contract finished (docs/decisions/0096).

use vike_tradehub::registry::REGISTRY;

use crate::permissive_grid::assert_permissive_grid;

/// ROSTER-PARAMETERIZED inert-default contract — the successor to the six hand-written
/// near-duplicate `*_absent_creds_stays_paper_and_inert` twins (binance/ctrader/alpaca/ig/oanda/
/// ibkr). For EVERY venue in the canonical `vike_model::VENUES` roster, mounting with an EMPTY
/// `.env` map — no `{VENUE}_DEMO_*` / agent-wallet / private-key / OAuth creds anywhere — must
/// yield the byte-identical PAPER engine: NO `ReconClient` handle (a paper venue never
/// reconciles); NOT marked live (`live_venues` stays empty); tagged with the venue's static
/// published fee schedule (a paper venue has no recon, so `resolve_fee_schedule` returns
/// `static_default` == `fee_schedule_for(venue)` — the invariant the retired
/// `make_engine_tags_paper_engine_with_resolved_schedule` pinned for binance, now generalized to
/// the whole roster); and a PERMISSIVE RiskGate grid (`from_properties` never ran → every
/// constraint field `None`).
///
/// It also touches NO network: with an empty map every bridge's `VenueMount::resolve` answers
/// paper before its `mount` is asked for anything (the registry's own
/// `with_no_credentials_every_contract_row_resolves_and_mounts_paper` pins that row by row), and a
/// `FeatureAbsent` row mounts the generic paper client.
///
/// `recon_enabled` is deliberately `true` here, NOT `false`: this test's `recon.is_none()`
/// assertion is about ABSENT CREDENTIALS being the live gate, and passing `false` would satisfy
/// it through the new global reconcile gate instead, making it vacuous. With the gate on and an
/// empty `.env`, every bridge's `resolve` still declines first, so no reconcile factory is reached
/// and the test stays offline.
///
/// Iterating the roster is the whole point: a newly-added bridge crate (its id landing in
/// `VENUES`) is AUTOMATICALLY held to this contract with ZERO new test code — the copy-drift
/// surface the six hand-written twins were is gone (the `venues.rs`/`fees.rs` completeness-gate
/// idiom, applied to the mount contract). aster is NOT special-cased despite being the one venue
/// whose live tier arms on credential PRESENCE alone: with empty vars it resolves no
/// agent-wallet creds and stays paper, so the loop is uniform.
/// ibkr, fxcm and polymarket are `FeatureAbsent` in the default build this module runs in, so the
/// loop reaches each as the generic paper row; ibkr's bridge's own no-credential path is
/// `crates/bridges/vike-ibkr/src/mount_tests.rs`'s, and polymarket's no-key path is
/// `crates/bridges/polymarket/src/exec_plane/mount_contract_tests.rs`'s
/// `with_no_key_the_mount_is_paper_whatever_the_gates_say`.
#[test]
fn all_roster_venues_absent_creds_stay_paper_and_inert() {
    // The mounted symbol is IRRELEVANT on the paper path — no arm parses it before falling back
    // to paper, and `PaperExecutionClient` only stores it — so ONE representative symbol covers
    // every venue (each venue's own symbol format is exercised by its bridge's parser tests).
    const SYMBOL: &str = "BTCUSDT";
    let (tx, _rx) = vike_exec::event_channel(16);
    let vars = std::collections::HashMap::new(); // empty .env ⇒ absent creds for every venue
    // ⚠ EVERY roster venue gets an active `live` account row here, deliberately: this test's
    // subject is the OTHER gate — absent credentials — and a default (no-row, all-`paper`)
    // policy would satisfy every assertion below at the account table's early return, without
    // any arm's own cred/config check ever running. Arming everything is what keeps this the
    // absent-credentials contract rather than a second test of the account tier.
    let armed = crate::support::all_armed_policy(vike_config::VenueMode::Live);
    for &venue in vike_model::VENUES {
        let mut live = std::collections::HashSet::new();
        let mut env = vike_mount::MountEnv::new(REGISTRY, &vars, &tx, &mut live);
        env.recon_enabled = true;
        env.policy = Some(&armed);
        let (engine, recon) = vike_mount::make_engine(&mut env, venue, SYMBOL)
            .unwrap_or_else(|e| panic!("{venue}: paper mount must never refuse to start: {e}"));
        assert!(recon.is_none(), "{venue}: absent creds → paper, no reconcile handle");
        assert!(live.is_empty(), "{venue}: absent creds → venue not marked live");
        assert_eq!(
            engine.fee_schedule,
            Some(vike_model::fee_schedule_for(venue)),
            "{venue}: paper mount tagged with the venue's static fee schedule"
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
/// bare symbols fails on the other. The value pins live in `vike_model::money::fees`
/// (`lane_rows_are_pinned`); this only proves the lane REACHES the engine.
#[test]
fn make_engine_keys_the_fee_schedule_off_the_symbol_lane() {
    let (tx, _rx) = vike_exec::event_channel(16);
    let vars = std::collections::HashMap::new(); // empty .env ⇒ paper everywhere, no network
    let mount = |venue: &str, symbol: &str| {
        let mut live = std::collections::HashSet::new();
        // The fields `MountEnv::new` leaves `None`: no recon client, no properties recorder, no
        // operator risk_profile, and no machine policy (the last arrived with Phase 6c, #1057).
        // This test is about which fee ROW the lane key selects, so every other input stays at
        // its absent default.
        let mut env = vike_mount::MountEnv::new(REGISTRY, &vars, &tx, &mut live);
        env.recon_enabled = true;
        vike_mount::make_engine(&mut env, venue, symbol)
            .unwrap_or_else(|e| panic!("{venue}/{symbol}: paper mount must start: {e}"))
            .0
            .fee_schedule
            .expect("make_engine always tags a schedule")
    };
    for venue in ["binance", "aster"] {
        assert_eq!(
            mount(venue, "BTCUSDT.P"),
            vike_model::fee_schedule_for(vike_catalog::fee_lane(venue, "BTCUSDT.P")),
            "{venue}: a `.P` mount must be tagged with its PERP lane's schedule"
        );
        assert_eq!(
            mount(venue, "BTCUSDT"),
            vike_model::fee_schedule_for(venue),
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
/// reading it (`contract`'s `parts_from_outcome`) — is proven apart: by the bridge's
/// `mount_contract_tests.rs`, and by `crates/vike-mount/src/contract_tests/outcome_fold.rs`'s
/// `a_live_outcome_folds_its_grid_legs_multiplier_and_margin_mode_into_the_engine`.
///
/// It moved from `vike-mount`'s `hyperliquid` module into that crate's `fee_schedule_tests` with
/// the venue mount contract's hyperliquid port, which deleted the module, and on to here with the
/// contract's finish, beside the other tests that name a bridge (docs/decisions/0096).
#[test]
fn an_isolated_only_asset_folds_isolated_through_the_real_mount_chain() {
    use vike_exec::{Account, BalanceMode};
    use vike_hyperliquid::symbology::Symbology;
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
        venue: vike_hyperliquid::consts::VENUE.into(),
        symbol: symbol.into(),
        side: 1,
        last_qty: 1.0,
        last_px: 1.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts: 1,
        mark_price: None,
        position_side: "BOTH".into(),
    };
    // Exactly what `assemble_engine` does with the margin mode a live outcome carries.
    let booked = |symbol: &str| {
        let mode = vike_hyperliquid::mount::margin_mode(&symbology, symbol);
        let mut account =
            Account::new(1.0, vike_hyperliquid::consts::VENUE, None, BalanceMode::Delta)
                .with_default_margin_modes(vike_mount::margin_mode_grid(symbol, mode));
        account.apply_fill(&fill(symbol));
        let key: vike_exec::PositionKey =
            (vike_hyperliquid::consts::VENUE.into(), symbol.into(), "BOTH".into());
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
        vike_model::caps_for(vike_hyperliquid::consts::VENUE).default_margin_mode,
        "an unlisted symbol falls back to the per-venue declaration, not a literal"
    );
}

/// Required test 5 (the make_engine-level proof): a PAPER mount — the arm every venue in
/// `all_roster_venues_absent_creds_stay_paper_and_inert` takes with an empty `.env` — succeeds
/// with `Ok` even though NEITHER account-dependent cap was ever supplied (no risk_profile at
/// all). `require_live_risk_budget` is gated on `live_venues.contains(venue)` at the
/// `make_engine` call site, which stays empty on the paper path, so the refusal never fires.
#[test]
fn paper_mount_starts_with_no_account_dependent_budget_at_all() {
    let (tx, _rx) = vike_exec::event_channel(16);
    let mut live = std::collections::HashSet::new();
    let no_creds = std::collections::HashMap::new(); // no creds -> paper
    // ⚠ binance is ARMED by an active `demo` row here on purpose: this test's subject is that
    // ABSENT CREDENTIALS keep the budget refusal from firing, and a `paper` account would
    // reach the same `Ok` one step earlier, without the refusal's own gate being consulted.
    let policy = crate::support::armed_policy("binance", vike_config::VenueMode::Demo);
    // `new` leaves reconcile off — this test is about the risk budget, not recon — and no
    // operator risk_profile either.
    let mut env = vike_mount::MountEnv::new(REGISTRY, &no_creds, &tx, &mut live);
    env.policy = Some(&policy);
    let (engine, _recon) = vike_mount::make_engine(&mut env, "binance", "BTCUSDT")
        .expect("a paper mount must never refuse to start over an unset account-dependent cap");
    assert_eq!(engine.gate.limits.max_notional_per_order, None);
    assert_eq!(engine.gate.limits.max_total_exposure, None);
}
