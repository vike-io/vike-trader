//! Unit tests of the fee-schedule resolution and the contract-size and margin-mode grids.

use super::resolve_fee_schedule;
use vike_exec::recon::ReconClient;
use vike_model::FeeSchedule;

/// A `ReconClient` whose `fetch_fee_rates` returns a configurable outcome.
struct FeeClient(Result<Option<FeeSchedule>, String>);
impl ReconClient for FeeClient {
    crate::testutil::empty_report_fetches!();
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
