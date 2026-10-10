use super::resolve_fee_schedule;
use vike_exec::recon::ReconClient;
use vike_model::{FeeSchedule, FillReport, OrderStatusReport, PositionStatusReport};

/// A `ReconClient` whose `fetch_fee_rates` returns a configurable outcome.
struct FeeClient(Result<Option<FeeSchedule>, String>);
impl ReconClient for FeeClient {
    fn fetch_order_status_reports(&self, _s: i64) -> Result<Vec<OrderStatusReport>, String> {
        Ok(vec![])
    }
    fn fetch_fill_reports(&self, _s: i64) -> Result<Vec<FillReport>, String> {
        Ok(vec![])
    }
    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        Ok(vec![])
    }
    fn fetch_fee_rates(&self) -> Result<Option<FeeSchedule>, String> {
        self.0.clone()
    }
}

#[test]
fn no_recon_falls_back_to_static_default() {
    let default = vike_model::fee_schedule_for("binance");
    assert_eq!(resolve_fee_schedule("binance", None, default), default);
}

#[test]
fn live_some_is_preferred_over_static() {
    let live = FeeSchedule::PercentMakerTaker { maker_bps: 1.0, taker_bps: 2.0 };
    let rc = FeeClient(Ok(Some(live)));
    assert_eq!(
        resolve_fee_schedule("binance", Some(&rc), vike_model::fee_schedule_for("binance")),
        live
    );
}

#[test]
fn none_and_error_both_fall_back_to_static() {
    let none = FeeClient(Ok(None));
    let err = FeeClient(Err("boom".to_string()));
    let expect = vike_model::fee_schedule_for("okx");
    assert_eq!(resolve_fee_schedule("okx", Some(&none), expect), expect);
    assert_eq!(resolve_fee_schedule("okx", Some(&err), expect), expect);
}

/// The ctrader, alpaca and ibkr grid shapes, run through the SAME `RiskLimits::from_properties`
/// the contract fold applies, yield non-default limits. Synthetic grids: the live fetch is each
/// bridge's parser tests (ctrader `risk_properties`, alpaca `parse_asset_properties`, ibkr
/// `contract_details_to_properties`).
#[test]
fn resolved_grids_yield_non_default_limits() {
    let default = vike_model::RiskLimits::new();
    // ctrader `risk_properties`: 10^-digits tick + centi-unit volume grid (EURUSD demo values).
    let ctrader = vike_model::SymbolProperties {
        tick_size: 1.0 / 100_000.0,
        step_size: 1000.0,
        min_qty: 1000.0,
        ..Default::default()
    };
    let l = vike_model::RiskLimits::from_properties(&ctrader);
    assert_eq!(l.tick_size, Some(1.0 / 100_000.0));
    assert_eq!(l.lot_size, Some(1000.0), "step_size → lot_size");
    assert_eq!(l.min_qty, Some(1000.0));
    assert_ne!(l.tick_size, default.tick_size, "tighter than the permissive default (None)");

    // alpaca `/v1/assets` equity default: penny tick, whole-share step.
    let alpaca =
        vike_model::SymbolProperties { tick_size: 0.01, step_size: 1.0, ..Default::default() };
    let l = vike_model::RiskLimits::from_properties(&alpaca);
    assert_eq!(l.tick_size, Some(0.01));
    assert_eq!(l.lot_size, Some(1.0));

    // ibkr `contractDetails`: min_tick / size_increment / min_size.
    let ibkr = vike_model::SymbolProperties {
        tick_size: 0.01,
        step_size: 1.0,
        min_qty: 1.0,
        ..Default::default()
    };
    let l = vike_model::RiskLimits::from_properties(&ibkr);
    assert_eq!(l.tick_size, Some(0.01));
    assert_eq!(l.lot_size, Some(1.0));
    assert_eq!(l.min_qty, Some(1.0));
    assert_ne!(l.min_qty, default.min_qty, "tighter than the permissive default (None)");
}

/// No contract size yields NO grid, so `Account::multiplier_of` falls through to its 1.0 scalar
/// and every non-deribit venue mounts byte-identically.
#[test]
fn absent_contract_size_yields_no_grid() {
    assert!(super::multiplier_grid("BTCUSDT", 0.0).is_none());
    // an explicit 1.0 is arithmetically the same as no grid — collapsed, not carried
    assert!(super::multiplier_grid("BTC-8JUL26-62000-C", 1.0).is_none());
}

/// A real contract size lands under the mounted symbol: the key `Account::multiplier_of` (hence
/// `validate_with_multiplier` and the GUI snapshot) looks up.
#[test]
fn real_contract_size_lands_in_the_grid() {
    let grid = super::multiplier_grid("BTC-PERPETUAL", 10.0).expect("non-1.0 → a grid");
    assert_eq!(grid.get("BTC-PERPETUAL"), Some(&10.0));
    assert_eq!(grid.len(), 1, "only the mounted symbol");
}

/// A degenerate value folds to the absent case: a 0.0 multiplier would make every order measure
/// as zero notional.
#[test]
fn degenerate_contract_size_yields_no_grid() {
    for bad in [-5.0, f64::NAN, f64::INFINITY] {
        assert!(super::multiplier_grid("X", bad).is_none(), "{bad} must not build a grid");
    }
}

/// End-to-end through `Account::multiplier_of`: the grid's value, 1.0 for an unlisted symbol.
#[test]
fn grid_drives_account_multiplier_of() {
    let acct = vike_exec::Account::new(
        1.0,
        "deribit",
        super::multiplier_grid("BTC-PERPETUAL", 10.0),
        vike_exec::BalanceMode::Delta,
    );
    assert_eq!(acct.multiplier_of("BTC-PERPETUAL"), 10.0, "the mounted symbol's contract size");
    assert_eq!(acct.multiplier_of("ETH-PERPETUAL"), 1.0, "unlisted → the scalar default");

    // and the no-contract-size venue is identical to `None`
    let plain = vike_exec::Account::new(1.0, "binance", None, vike_exec::BalanceMode::Delta);
    let wired = vike_exec::Account::new(
        1.0,
        "binance",
        super::multiplier_grid("BTCUSDT", 0.0),
        vike_exec::BalanceMode::Delta,
    );
    assert_eq!(plain.multiplier_of("BTCUSDT"), wired.multiplier_of("BTCUSDT"));
}

/// [`super::margin_mode_grid`]'s collapse rule (the margin twin of `multiplier_grid`): `Cross`,
/// the mode on 13 of the 14 roster venues and every ordinary hyperliquid asset, yields NO grid so
/// `Account` keeps its empty-map short-circuit; any other mode lands under the mounted symbol,
/// the key `Account::default_margin_mode_of` looks up on open-from-flat.
#[test]
fn margin_mode_grid_collapses_cross_and_carries_the_rest() {
    use vike_model::MarginMode;
    assert!(super::margin_mode_grid("BTC", MarginMode::Cross).is_none(), "Cross ⇒ no grid");

    for mode in [MarginMode::Isolated, MarginMode::Cash] {
        let grid = super::margin_mode_grid("CASHCAT", mode).expect("non-Cross ⇒ a grid");
        assert_eq!(grid.get("CASHCAT"), Some(&mode));
        assert_eq!(grid.len(), 1, "only the mounted symbol");
    }
}

/// End-to-end through `Account::default_margin_mode_of`: the grid drives it, an unlisted symbol
/// stays `Cross`, and a collapsed-`Cross` mount equals the `None` every other venue passes.
#[test]
fn margin_mode_grid_drives_account_default_margin_mode_of() {
    use vike_model::MarginMode;
    let acct = vike_exec::Account::new(1.0, "hyperliquid", None, vike_exec::BalanceMode::Delta)
        .with_default_margin_modes(super::margin_mode_grid("CASHCAT", MarginMode::Isolated));
    assert_eq!(acct.default_margin_mode_of("CASHCAT"), MarginMode::Isolated);
    assert_eq!(acct.default_margin_mode_of("BTC"), MarginMode::Cross, "unlisted → Cross");

    let plain = vike_exec::Account::new(1.0, "binance", None, vike_exec::BalanceMode::Delta);
    let wired = vike_exec::Account::new(1.0, "binance", None, vike_exec::BalanceMode::Delta)
        .with_default_margin_modes(super::margin_mode_grid("BTCUSDT", MarginMode::Cross));
    assert_eq!(plain.default_margin_mode_of("BTCUSDT"), wired.default_margin_mode_of("BTCUSDT"));
}

// ---- merge_operator_budget: the merge site `make_engine` calls, pinned DIRECTLY ----
// The base is shaped like a REAL `RiskLimits::from_properties` fetch, so flipping its hardcoded
// `GridSource::VenueFetched` to `NoGridFetched` makes
// `venue_owned_field_conflict_arms_the_operator_budget_via_the_fallback` see the profile's
// illegal `tick_size` WIN instead of being dropped — that test fails first.

fn venue_fetched_limits() -> vike_model::RiskLimits {
    vike_model::RiskLimits {
        tick_size: Some(0.5),
        lot_size: Some(0.01),
        min_qty: Some(0.01),
        min_notional: Some(10.0),
        ..vike_model::RiskLimits::new()
    }
}

#[test]
fn merge_operator_budget_none_leaves_limits_untouched() {
    let base = venue_fetched_limits();
    let got = super::merge_operator_budget("binance", base.clone(), None);
    assert_eq!(got, base, "no profile threaded in must be a byte-identical no-op");
}

/// A clean profile arms the operator budget AND leaves the REAL fetched grid alone: the call
/// site routes through `VenueFetched` (not only `ProfileRisk::apply_to` in isolation).
#[test]
fn merge_operator_budget_arms_operator_fields_and_keeps_the_venue_grid() {
    let base = venue_fetched_limits();
    let profile = vike_model::ProfileRisk {
        max_notional_per_order: Some(100.0),
        max_total_exposure: Some(500.0),
        ..vike_model::ProfileRisk::default()
    };
    let got = super::merge_operator_budget("binance", base.clone(), Some(&profile));
    assert_eq!(got.tick_size, base.tick_size, "venue grid must stay the REAL fetched value");
    assert_eq!(got.lot_size, base.lot_size);
    assert_eq!(got.min_qty, base.min_qty);
    assert_eq!(got.min_notional, base.min_notional);
    assert_eq!(got.max_notional_per_order, Some(100.0));
    assert_eq!(got.max_total_exposure, Some(500.0));
}

/// Regression: a profile that ALSO (illegally) sets a venue-owned field over a REAL fetched grid
/// must not zero the whole operator budget: only that field is dropped (the fetched value stays)
/// and every operator-owned field still arms.
#[test]
fn venue_owned_field_conflict_arms_the_operator_budget_via_the_fallback() {
    let base = venue_fetched_limits();
    let profile = vike_model::ProfileRisk {
        tick_size: Some(999.0), // illegal under a real venue fetch
        max_notional_per_order: Some(100.0),
        max_total_exposure: Some(500.0),
        max_orders_per_window: Some(5),
        window_ms: 2000,
        ..vike_model::ProfileRisk::default()
    };
    let got = super::merge_operator_budget("binance", base.clone(), Some(&profile));
    assert_eq!(
        got.tick_size, base.tick_size,
        "the illegal venue-owned override must be dropped, not honored"
    );
    assert_eq!(
        got.max_notional_per_order,
        Some(100.0),
        "the operator budget must still arm despite the unrelated venue-field conflict"
    );
    assert_eq!(got.max_total_exposure, Some(500.0));
    assert_eq!(got.max_orders_per_window, Some(5));
    assert_eq!(got.window_ms, 2000);
}

// ---- `arm_universal_defaults` / `require_live_risk_budget`, pinned DIRECTLY ----
// A network-free test cannot drive a LIVE `make_engine` arm, so the pure functions its live call
// site invokes are the proof. Each test below was confirmed to fail with its fix commented out.

fn market_order(symbol: &str, qty: f64) -> vike_model::OrderRequest {
    vike_model::OrderRequest {
        client_order_id: "t".into(),
        venue: "binance".into(),
        symbol: symbol.into(),
        side: 1,
        qty,
        order_type: "market".into(),
        ..Default::default()
    }
}

#[test]
fn arm_universal_defaults_arms_the_throttle_when_absent() {
    let armed = super::arm_universal_defaults(vike_model::RiskLimits::new());
    assert_eq!(armed.max_orders_per_window, Some(super::ARMED_MAX_ORDERS_PER_WINDOW));
    assert_eq!(armed.window_ms, 1000, "untouched -> RiskLimits::new()'s own 1000ms default");
    // `max_leverage` is NOT armed (#822): a `Some(1.0)` enforced nothing and duplicated the
    // `im_requirement` rescue. It stays whatever reached this fn.
    assert_eq!(armed.max_leverage, None);
    // required_free_bp_pct needs no rescue: already 0.0 from RiskLimits::new().
    assert_eq!(armed.required_free_bp_pct, 0.0);
}

/// Compose, not fight: an explicit value (as `merge_operator_budget` leaves a profile's) is never
/// clobbered by the default; `.or(..)` only fills a `None`.
#[test]
fn arm_universal_defaults_never_overrides_an_explicit_value() {
    let explicit = vike_model::RiskLimits {
        max_orders_per_window: Some(7),
        window_ms: 500,
        max_leverage: Some(3.0),
        ..vike_model::RiskLimits::new()
    };
    let armed = super::arm_universal_defaults(explicit);
    assert_eq!(armed.max_orders_per_window, Some(7), "an explicit value must win");
    assert_eq!(armed.window_ms, 500);
    assert_eq!(armed.max_leverage, Some(3.0), "carried through untouched, never clobbered");
}

/// "The field is populated" is not "the check runs": a REAL `RiskGate` built from the armed
/// defaults alone DENIES the order after `ARMED_MAX_ORDERS_PER_WINDOW` in the window.
#[test]
fn armed_defaults_gate_actually_denies_a_rate_violation() {
    let armed = super::arm_universal_defaults(vike_model::RiskLimits::new());
    let mut gate = vike_exec::RiskGate::new(armed);
    let req = market_order("BTCUSDT", 0.001);
    let ctx = vike_exec::RiskContext {
        mark_price: 100.0,
        equity: 1_000_000.0,
        ..vike_exec::RiskContext::default()
    };
    for i in 0..super::ARMED_MAX_ORDERS_PER_WINDOW {
        let v = gate.check(&req, &ctx);
        assert!(v.ok, "order {i} within the armed per-window cap must pass: {v:?}");
    }
    let v = gate.check(&req, &ctx);
    assert!(!v.ok, "the order beyond the armed per-window cap must be DENIED");
    assert_eq!(v.reason, "rate-limited");
}

/// `RiskGate::check` never evaluates `RiskLimits::max_leverage` (and nothing calls
/// `clamp_leverage`: `Command::SetMargin` writes `im_by_symbol` directly), so "no leverage unless
/// asked" is enforced by `im_requirement`: `make_engine`'s rescue arms it at 1.0, and an
/// operator's `[risk] max_leverage` converts into it (#822). This proves THAT denies an
/// over-leveraged order, as `make_engine` builds it; the inert knob stays `None`.
#[test]
fn armed_leverage_is_enforced_via_im_requirement_not_max_leverage() {
    let mut limits = super::arm_universal_defaults(vike_model::RiskLimits::new());
    assert_eq!(limits.max_leverage, None, "the inert knob is no longer armed");
    limits.im_requirement = limits.im_requirement.or(Some(1.0)); // make_engine's own rescue
    let mut gate = vike_exec::RiskGate::new(limits);
    // 20 units at $100 = $2,000 notional against $1,000 equity at 1x buying power -> denied.
    let req = market_order("BTCUSDT", 20.0);
    let ctx = vike_exec::RiskContext {
        mark_price: 100.0,
        equity: 1_000.0,
        ..vike_exec::RiskContext::default()
    };
    let v = gate.check(&req, &ctx);
    assert!(!v.ok, "an order needing more than 1x buying power must be denied: {v:?}");
    assert_eq!(v.reason, "insufficient-margin");
}

/// Over-arming is the real risk: a normal order inside every armed default still passes.
#[test]
fn armed_defaults_gate_still_passes_a_normal_order() {
    let mut limits = super::arm_universal_defaults(vike_model::RiskLimits::new());
    limits.im_requirement = limits.im_requirement.or(Some(1.0));
    let mut gate = vike_exec::RiskGate::new(limits);
    let req = market_order("BTCUSDT", 1.0);
    let ctx = vike_exec::RiskContext {
        mark_price: 100.0,
        equity: 1_000_000.0,
        ..vike_exec::RiskContext::default()
    };
    let v = gate.check(&req, &ctx);
    assert!(v.ok, "a normal order well within every armed default must pass: {v:?}");
}

#[test]
fn require_live_risk_budget_ok_when_both_set() {
    let limits = vike_model::RiskLimits {
        max_notional_per_order: Some(1.0),
        max_total_exposure: Some(1.0),
        ..vike_model::RiskLimits::new()
    };
    assert!(super::require_live_risk_budget("binance", &limits, true).is_ok());
}

/// A live mount with NEITHER cap set refuses, naming BOTH keys in one message.
#[test]
fn require_live_risk_budget_names_both_missing_keys() {
    let limits = vike_model::RiskLimits::new(); // neither cap set
    // `false` = no profile reached the mount (the whole-file diagnostic), the only way BOTH caps
    // go missing in practice.
    let err = super::require_live_risk_budget("okx", &limits, false)
        .expect_err("neither account-dependent cap set -> must refuse to start");
    let msg = format!("{err}");
    assert!(msg.contains("okx"), "error must name the venue: {msg}");
    assert!(msg.contains("max_notional_per_order"), "must name the 1st missing key: {msg}");
    assert!(msg.contains("max_total_exposure"), "must name the 2nd missing key: {msg}");
}

/// Only the ACTUALLY missing cap is named; a supplied one is not blamed.
#[test]
fn require_live_risk_budget_names_only_the_actually_missing_key() {
    let limits = vike_model::RiskLimits {
        max_notional_per_order: Some(1.0), // supplied
        ..vike_model::RiskLimits::new()    // max_total_exposure still None
    };
    // `true` = a profile reached the mount, so the add-these-lines variant lists exactly the
    // missing keys. (Under `false` it prints a COMPLETE minimal profile, both caps included.)
    let err = super::require_live_risk_budget("bybit", &limits, true)
        .expect_err("one missing cap is still a refusal");
    let msg = format!("{err}");
    assert!(!msg.contains("max_notional_per_order"), "must not blame the supplied key: {msg}");
    assert!(msg.contains("max_total_exposure"), "must name the actually-missing key: {msg}");
}

/// The STARTUP DIAGNOSTIC contract for the no-profile case, the first wall of every new live
/// user: the message must name the problem, where the budget lives, the keys and a runnable fix.
#[test]
fn no_profile_diagnostic_names_the_row_the_keys_and_a_working_command() {
    let err = super::require_live_risk_budget("binance", &vike_model::RiskLimits::new(), false)
        .expect_err("no budget from any source -> refusal");
    let msg = format!("{err}");

    // 1. WHAT is wrong, in words, not a Debug dump.
    assert!(msg.contains("no risk budget"), "states the problem plainly: {msg}");
    assert!(!msg.contains("MissingRiskBudget"), "must not leak the Debug shape: {msg}");
    // 2. WHERE the budget lives: the ACTIVE `run` row, written by one command (decision 0111 —
    //    no variable and no file names a run profile any more).
    assert!(msg.contains("ACTIVE `run` row"), "names the row: {msg}");
    assert!(msg.contains("vike-cli config bootstrap-run"), "names the writer: {msg}");
    assert!(!msg.contains("--profile <"), "no profile FILE is read any more: {msg}");
    // 3. WHICH keys.
    assert!(msg.contains("max_notional_per_order"), "names the 1st key: {msg}");
    assert!(msg.contains("max_total_exposure"), "names the 2nd key: {msg}");
    // 4. The message IS the fix: `--mode live` plus every `[risk]` cap, the WHOLE minimal live
    //    profile (`crates/vike-mount/tests/risk_budget_diagnostic.rs` parses it).
    assert!(msg.contains("--mode live"), "carries the mode the live mount demands: {msg}");
    assert!(msg.contains("--risk.max_notional_per_order 5000.0"), "a usable value: {msg}");
    assert!(msg.contains("--risk.max_total_exposure 25000.0"), "a usable value: {msg}");
    // 5. WHERE the commented reference of every key lives, so the message is not the only copy.
    assert!(msg.contains(super::EXAMPLE_PROFILE_PATH), "names the shipped reference: {msg}");
}

/// The OTHER half: a profile is in force but omits a cap. The fix is the missing keys added to
/// the active row's body, so the message names those keys and says the body is re-written whole.
#[test]
fn supplied_profile_diagnostic_names_only_the_missing_keys() {
    let limits = vike_model::RiskLimits {
        max_notional_per_order: Some(1.0),
        ..vike_model::RiskLimits::new()
    };
    let msg = format!(
        "{}",
        super::require_live_risk_budget("okx", &limits, true).expect_err("still a refusal")
    );
    assert!(msg.contains("--risk.max_total_exposure 25000.0"), "shows a usable value: {msg}");
    assert!(
        !msg.contains("--risk.max_notional_per_order"),
        "must not ask for a key the profile already carries: {msg}"
    );
    assert!(msg.contains("active run profile"), "says which profile to change: {msg}");
    assert!(msg.contains("config show"), "says where to read what it carries now: {msg}");
}

/// [`super::BUDGET_EXAMPLES`] covers every key [`super::require_live_risk_budget`] can report: a
/// third cap without a row would be named in `missing:` and absent from the `[risk]` example.
#[test]
fn every_missing_key_has_an_example() {
    let err = super::require_live_risk_budget("binance", &vike_model::RiskLimits::new(), false)
        .expect_err("no budget -> refusal naming every reportable key");
    let missing = match err {
        super::MountError::MissingRiskBudget { missing, .. } => missing,
    };
    for key in &missing {
        assert!(
            super::BUDGET_EXAMPLES.iter().any(|(k, _, _)| k == key),
            "`{key}` is reportable but has no BUDGET_EXAMPLES row, so the diagnostic cannot \
                 show how to set it"
        );
    }
}
