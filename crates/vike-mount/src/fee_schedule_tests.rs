use super::resolve_fee_schedule;
use vike_exec::recon::ReconClient;
use vike_model::{FeeSchedule, FillReport, OrderStatusReport, PositionStatusReport};

/// A `ReconClient` whose `fetch_fee_rates` returns a configurable outcome (the other report
/// methods are irrelevant here). Mirrors the `FailingClient` test-double pattern in
/// `vike_exec::recon::client`.
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

/// The three grid shapes the ctrader, alpaca and ibkr mounts resolve, each run through the SAME
/// `RiskLimits::from_properties` call the contract fold makes on the grid a mount hands it,
/// tightens the gate to non-default limits —
/// the "sets non-default limits when properties are supplied" half of the contract, proven with
/// synthetic grids (the live fetch itself needs a running venue and is exercised by each bridge's
/// own parser tests: ctrader `risk_properties`, alpaca `parse_asset_properties`, ibkr
/// `contract_details_to_properties`).
#[test]
fn resolved_grids_yield_non_default_limits() {
    let default = vike_exec::RiskLimits::new();
    // ctrader `risk_properties`: 10^-digits tick + centi-unit volume grid (EURUSD demo values).
    let ctrader = vike_model::SymbolProperties {
        tick_size: 1.0 / 100_000.0,
        step_size: 1000.0,
        min_qty: 1000.0,
        ..Default::default()
    };
    let l = vike_exec::RiskLimits::from_properties(&ctrader);
    assert_eq!(l.tick_size, Some(1.0 / 100_000.0));
    assert_eq!(l.lot_size, Some(1000.0), "step_size → lot_size");
    assert_eq!(l.min_qty, Some(1000.0));
    assert_ne!(l.tick_size, default.tick_size, "tighter than the permissive default (None)");

    // alpaca `/v1/assets` equity default: penny tick, whole-share step.
    let alpaca =
        vike_model::SymbolProperties { tick_size: 0.01, step_size: 1.0, ..Default::default() };
    let l = vike_exec::RiskLimits::from_properties(&alpaca);
    assert_eq!(l.tick_size, Some(0.01));
    assert_eq!(l.lot_size, Some(1.0));

    // ibkr `contractDetails`: min_tick / size_increment / min_size.
    let ibkr = vike_model::SymbolProperties {
        tick_size: 0.01,
        step_size: 1.0,
        min_qty: 1.0,
        ..Default::default()
    };
    let l = vike_exec::RiskLimits::from_properties(&ibkr);
    assert_eq!(l.tick_size, Some(0.01));
    assert_eq!(l.lot_size, Some(1.0));
    assert_eq!(l.min_qty, Some(1.0));
    assert_ne!(l.min_qty, default.min_qty, "tighter than the permissive default (None)");
}

/// A venue that reports no contract size yields NO grid — the literal `None` this site passed
/// before the wiring existed, so `Account::multiplier_of` falls through to its 1.0 scalar and
/// every non-deribit venue mounts byte-identically.
#[test]
fn absent_contract_size_yields_no_grid() {
    assert!(super::multiplier_grid("BTCUSDT", 0.0).is_none());
    // an explicit 1.0 is arithmetically the same as no grid — collapsed, not carried
    assert!(super::multiplier_grid("BTC-8JUL26-62000-C", 1.0).is_none());
}

/// A real contract size lands in the grid under the mounted symbol, which is exactly what
/// `Account::multiplier_of` (and therefore `validate_with_multiplier` + the snapshot the GUI
/// reads) looks up. This is the assertion that the root bug is closed.
#[test]
fn real_contract_size_lands_in_the_grid() {
    let grid = super::multiplier_grid("BTC-PERPETUAL", 10.0).expect("non-1.0 → a grid");
    assert_eq!(grid.get("BTC-PERPETUAL"), Some(&10.0));
    assert_eq!(grid.len(), 1, "only the mounted symbol");
}

/// A degenerate venue value must not produce a 0.0/negative multiplier grid — it folds to the
/// absent case, since a 0.0 multiplier would make every order measure as zero notional.
#[test]
fn degenerate_contract_size_yields_no_grid() {
    for bad in [-5.0, f64::NAN, f64::INFINITY] {
        assert!(super::multiplier_grid("X", bad).is_none(), "{bad} must not build a grid");
    }
}

/// End-to-end through the type the engine actually consults: a grid built from a contract size
/// makes `Account::multiplier_of` return it, while an unlisted symbol stays 1.0.
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

    // and the no-contract-size venue is byte-identical to the pre-wiring `None`
    let plain = vike_exec::Account::new(1.0, "binance", None, vike_exec::BalanceMode::Delta);
    let wired = vike_exec::Account::new(
        1.0,
        "binance",
        super::multiplier_grid("BTCUSDT", 0.0),
        vike_exec::BalanceMode::Delta,
    );
    assert_eq!(plain.multiplier_of("BTCUSDT"), wired.multiplier_of("BTCUSDT"));
}

/// [`super::margin_mode_grid`]'s collapse rule, the margin-axis twin of the three
/// `multiplier_grid` tests above: `Cross` — the resolved mode on 13 of the 14 roster venues and
/// on every ordinary hyperliquid asset — yields NO grid, so `Account` keeps the empty-map
/// short-circuit and mounts byte-identically. A non-`Cross` mode lands under the mounted symbol,
/// which is exactly the key `Account::default_margin_mode_of` looks up on open-from-flat.
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

/// End-to-end through the type the fold actually consults, mirroring
/// `grid_drives_account_multiplier_of` directly above: the grid drives
/// `Account::default_margin_mode_of`, an unlisted symbol stays `Cross`, and the collapsed-`Cross`
/// mount is indistinguishable from the `None` every other venue passes.
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

// ---------------------------------------------------------------------------------------
// merge_operator_budget — the ACTUAL merge site `make_engine` calls, pinned DIRECTLY (unlike
// a test that only drives `ProfileRisk::apply_to` by hand and never touches this function —
// see this fn's own doc for why that distinction matters). The base below is shaped like a
// REAL `RiskLimits::from_properties` fetch (populated instrument grid), so flipping the
// hardcoded `GridSource::VenueFetched` inside `merge_operator_budget` to `NoGridFetched` would
// make `venue_owned_field_conflict_arms_the_operator_budget_via_the_fallback` below observe
// the profile's illegal `tick_size` silently WIN instead of being dropped — failing first.
// ---------------------------------------------------------------------------------------

fn venue_fetched_limits() -> vike_exec::RiskLimits {
    vike_exec::RiskLimits {
        tick_size: Some(0.5),
        lot_size: Some(0.01),
        min_qty: Some(0.01),
        min_notional: Some(10.0),
        ..vike_exec::RiskLimits::new()
    }
}

#[test]
fn merge_operator_budget_none_leaves_limits_untouched() {
    let base = venue_fetched_limits();
    let got = super::merge_operator_budget("binance", base.clone(), None);
    assert_eq!(got, base, "no profile threaded in must be a byte-identical no-op");
}

/// THE point of pinning this at the `merge_operator_budget` call site rather than only at
/// `ProfileRisk::apply_to`: a clean profile (no venue-owned fields) must both arm the operator
/// budget AND leave the REAL fetched venue grid alone — proving this function actually routes
/// through `VenueFetched`, not some other source.
#[test]
fn merge_operator_budget_arms_operator_fields_and_keeps_the_venue_grid() {
    let base = venue_fetched_limits();
    let profile = vike_exec::ProfileRisk {
        max_notional_per_order: Some(100.0),
        max_total_exposure: Some(500.0),
        ..vike_exec::ProfileRisk::default()
    };
    let got = super::merge_operator_budget("binance", base.clone(), Some(&profile));
    assert_eq!(got.tick_size, base.tick_size, "venue grid must stay the REAL fetched value");
    assert_eq!(got.lot_size, base.lot_size);
    assert_eq!(got.min_qty, base.min_qty);
    assert_eq!(got.min_notional, base.min_notional);
    assert_eq!(got.max_notional_per_order, Some(100.0));
    assert_eq!(got.max_total_exposure, Some(500.0));
}

/// BLOCKING-2(b) regression: a profile that ALSO (illegally) sets a venue-owned field over a
/// REAL fetched grid must not zero the operator's whole budget — only the offending venue
/// field is dropped (kept as the real fetched value), while every operator-owned field the
/// profile set still arms.
#[test]
fn venue_owned_field_conflict_arms_the_operator_budget_via_the_fallback() {
    let base = venue_fetched_limits();
    let profile = vike_exec::ProfileRisk {
        tick_size: Some(999.0), // illegal under a real venue fetch
        max_notional_per_order: Some(100.0),
        max_total_exposure: Some(500.0),
        max_orders_per_window: Some(5),
        window_ms: 2000,
        ..vike_exec::ProfileRisk::default()
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

// ---------------------------------------------------------------------------------------
// Task 6 (armed-risk-defaults) — `arm_universal_defaults` / `require_live_risk_budget`,
// pinned DIRECTLY (the same reasoning as `merge_operator_budget`'s own tests above: a
// network-free CI test cannot drive a genuinely LIVE `make_engine` arm end to end, so the
// pure functions the live call site actually invokes are the CI-safe proof). Every test below
// was broken (by commenting out the fix under test) and confirmed to fail, then restored,
// before being trusted — see the task report for the per-test confirmation.
// ---------------------------------------------------------------------------------------

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
    let armed = super::arm_universal_defaults(vike_exec::RiskLimits::new());
    assert_eq!(armed.max_orders_per_window, Some(super::ARMED_MAX_ORDERS_PER_WINDOW));
    assert_eq!(armed.window_ms, 1000, "untouched -> RiskLimits::new()'s own 1000ms default");
    // Issue #822: `max_leverage` is NOT armed here any more (#817's `Some(1.0)` enforced
    // nothing and duplicated the `im_requirement` rescue). It stays whatever reached this fn.
    assert_eq!(armed.max_leverage, None);
    // required_free_bp_pct needs no rescue in this fn: already 0.0 from RiskLimits::new().
    assert_eq!(armed.required_free_bp_pct, 0.0);
}

/// The compose-not-fight property (required test 3): an explicit value already present
/// (mirroring what `merge_operator_budget` would have left after a profile set it) must NOT
/// be clobbered by the default — `.or(..)` only fills a `None`.
#[test]
fn arm_universal_defaults_never_overrides_an_explicit_value() {
    let explicit = vike_exec::RiskLimits {
        max_orders_per_window: Some(7),
        window_ms: 500,
        max_leverage: Some(3.0),
        ..vike_exec::RiskLimits::new()
    };
    let armed = super::arm_universal_defaults(explicit);
    assert_eq!(armed.max_orders_per_window, Some(7), "an explicit value must win");
    assert_eq!(armed.window_ms, 500);
    assert_eq!(armed.max_leverage, Some(3.0), "carried through untouched, never clobbered");
}

/// THE HEADLINE test (required test 1, first half): "the field is populated" is not "the
/// check runs" — build a REAL `RiskGate` straight from the armed defaults (no profile
/// involved at all) and prove it actually DENIES a rate violation once `ARMED_MAX_ORDERS_PER_WINDOW`
/// orders have already landed in the window.
#[test]
fn armed_defaults_gate_actually_denies_a_rate_violation() {
    let armed = super::arm_universal_defaults(vike_exec::RiskLimits::new());
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

/// THE HEADLINE test (required test 1, second half) — the `max_leverage` field's honest
/// story, now with issue #822's resolution folded in. `RiskGate::check` still never evaluates
/// `RiskLimits::max_leverage` (no production caller of `clamp_leverage` exists either —
/// verified: `Command::SetMargin` writes `im_by_symbol` directly), so this test does NOT claim
/// that field denies anything. What enforces "no leverage unless asked" is `im_requirement`,
/// armed at 1.0 by the rescue one line below this fn's call site in `make_engine`
/// (`limits.im_requirement = limits.im_requirement.or(Some(1.0))`, pre-existing since PR #816)
/// — and, since #822, ALSO the destination an operator's `[risk] max_leverage` converts into.
/// This test proves THAT mechanism actually denies an over-leveraged order, mirroring exactly
/// what `make_engine` builds. #817's duplicate `max_leverage = 1.0` arming is gone, so the
/// field is `None` here: an inert knob is no longer populated to look like protection.
#[test]
fn armed_leverage_is_enforced_via_im_requirement_not_max_leverage() {
    let mut limits = super::arm_universal_defaults(vike_exec::RiskLimits::new());
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

/// Required test 4: over-arming, not under-arming, is the real risk of this change — a
/// perfectly normal order, comfortably inside every armed default, must still pass.
#[test]
fn armed_defaults_gate_still_passes_a_normal_order() {
    let mut limits = super::arm_universal_defaults(vike_exec::RiskLimits::new());
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
    let limits = vike_exec::RiskLimits {
        max_notional_per_order: Some(1.0),
        max_total_exposure: Some(1.0),
        ..vike_exec::RiskLimits::new()
    };
    assert!(super::require_live_risk_budget("binance", &limits, true).is_ok());
}

/// Required test 2: a live mount with NEITHER account-dependent cap set must refuse to
/// start, naming BOTH missing keys in one message (not just the first).
#[test]
fn require_live_risk_budget_names_both_missing_keys() {
    let limits = vike_exec::RiskLimits::new(); // neither cap set
    // `false` = no profile reached the mount, the shape that produces the whole-file variant
    // of the diagnostic — which is also the only way BOTH caps go missing in practice.
    let err = super::require_live_risk_budget("okx", &limits, false)
        .expect_err("neither account-dependent cap set -> must refuse to start");
    let msg = format!("{err}");
    assert!(msg.contains("okx"), "error must name the venue: {msg}");
    assert!(msg.contains("max_notional_per_order"), "must name the 1st missing key: {msg}");
    assert!(msg.contains("max_total_exposure"), "must name the 2nd missing key: {msg}");
}

/// Only the field that is ACTUALLY missing is named — a supplied cap must not be blamed.
#[test]
fn require_live_risk_budget_names_only_the_actually_missing_key() {
    let limits = vike_exec::RiskLimits {
        max_notional_per_order: Some(1.0), // supplied
        ..vike_exec::RiskLimits::new()     // max_total_exposure still None
    };
    // `true` = a profile DID reach the mount — the only way one cap can be set while the other
    // is not — so the diagnostic renders the add-these-lines variant, whose `[risk]` fragment
    // lists exactly the missing keys. (Under `false` the message deliberately prints a COMPLETE
    // minimal profile, both caps included, because there is no file to add a line to.)
    let err = super::require_live_risk_budget("bybit", &limits, true)
        .expect_err("one missing cap is still a refusal");
    let msg = format!("{err}");
    assert!(!msg.contains("max_notional_per_order"), "must not blame the supplied key: {msg}");
    assert!(msg.contains("max_total_exposure"), "must name the actually-missing key: {msg}");
}

/// The STARTUP DIAGNOSTIC contract, for the no-profile case — the wall every new user of a
/// live-mounting binary hits first. The old message named neither the knob nor the fix, so an
/// operator could only get past it by reading `run_profile.rs`'s `#[cfg(test)]` `LIVE_TOML`
/// fixture. Each assertion below is one thing that had to be discoverable from source before.
#[test]
fn no_profile_diagnostic_names_the_knob_the_keys_and_a_working_profile() {
    let err = super::require_live_risk_budget("binance", &vike_exec::RiskLimits::new(), false)
        .expect_err("no budget from any source -> refusal");
    let msg = format!("{err}");

    // 1. WHAT is wrong, in words, not a Debug dump.
    assert!(msg.contains("no risk budget"), "states the problem plainly: {msg}");
    assert!(!msg.contains("MissingRiskBudget"), "must not leak the Debug shape: {msg}");
    // 2. WHICH resolver to use. Both, because `resolve_profile`'s precedence is
    //    explicit-flag-beats-env and only some binaries pass an explicit path. Asserted on the
    //    ACTIONABLE spelling (`Set VIKE_RUN_PROFILE=`) rather than the bare name — a stronger
    //    check, and it keeps this library file free of a standalone env-shaped string literal,
    //    which `vike_ops::scan::find_map_lookups` would read as an injected-map env read here.
    assert!(msg.contains("Set VIKE_RUN_PROFILE=<run.toml>"), "names the env var: {msg}");
    assert!(msg.contains("--profile <run.toml>"), "names the flag: {msg}");
    // 3. WHICH keys — the error already knew them; it just never printed them usefully.
    assert!(msg.contains("max_notional_per_order"), "names the 1st key: {msg}");
    assert!(msg.contains("max_total_exposure"), "names the 2nd key: {msg}");
    // 4. The message IS the example: `mode` plus a `[risk]` table, which is now the WHOLE of a
    //    minimal live profile.
    //
    //    ⚠ This step used to also demand `[event_source]` and `[broker]`, because they were
    //    schema-required and a `[risk]`-only file earned a parse error. Both tables are
    //    DELETED from `RunProfile` and a profile carrying either is refused BY NAME, so the
    //    two assertions became the opposite of what they were for: they would have pinned this
    //    message to printing a paste-ready file that fails startup. They are asserted ABSENT
    //    below instead, which is the same intent — the printed example must be one an operator
    //    can actually paste.
    assert!(msg.contains("[risk]"), "shows a [risk] table: {msg}");
    assert!(msg.contains("mode = \"live\""), "shows the mode the live mount demands: {msg}");
    assert!(
        !msg.contains("[event_source]") && !msg.contains("[broker]"),
        "the example must not print a table `RunProfile::validate` now refuses by name — an \
             operator pasting this would earn a refusal from the message telling them how to fix \
             a refusal: {msg}"
    );
    // 5. WHERE the fuller template lives, so the message is not the only copy.
    assert!(msg.contains(super::EXAMPLE_PROFILE_PATH), "names the shipped template: {msg}");
}

/// The OTHER half of the two-case split: a profile exists but omits a cap. The fix is an edit,
/// not a new file, so the message must NOT print a whole profile (which an operator would
/// reasonably paste over the file they already have, losing the rest of their config).
#[test]
fn supplied_profile_diagnostic_asks_for_an_edit_not_a_new_file() {
    let limits =
        vike_exec::RiskLimits { max_notional_per_order: Some(1.0), ..vike_exec::RiskLimits::new() };
    let msg = format!(
        "{}",
        super::require_live_risk_budget("okx", &limits, true).expect_err("still a refusal")
    );
    assert!(msg.contains("[risk]"), "still shows the table to edit: {msg}");
    assert!(msg.contains("max_total_exposure = 25000.0"), "shows a usable value: {msg}");
    assert!(!msg.contains("[broker]"), "must not print a whole replacement profile: {msg}");
    assert!(!msg.contains("mode = \"live\""), "must not print a whole replacement profile: {msg}");
    assert!(msg.contains("VIKE_RUN_PROFILE / --profile"), "says which file to edit: {msg}");
}

/// [`super::BUDGET_EXAMPLES`] must cover every key [`super::require_live_risk_budget`] can
/// report. A third account-dependent cap added there without a row here would silently vanish
/// from the inline `[risk]` example — the message would name a key in its `missing:` line and
/// then fail to show how to set it, which is exactly the discoverability hole this work closed.
#[test]
fn every_missing_key_has_an_example() {
    let err = super::require_live_risk_budget("binance", &vike_exec::RiskLimits::new(), false)
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
