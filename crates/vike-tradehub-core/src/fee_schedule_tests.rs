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
    let default = vike_venues::fee_schedule_for("binance");
    assert_eq!(resolve_fee_schedule("binance", None, default), default);
}

#[test]
fn live_some_is_preferred_over_static() {
    let live = FeeSchedule::PercentMakerTaker { maker_bps: 1.0, taker_bps: 2.0 };
    let rc = FeeClient(Ok(Some(live)));
    assert_eq!(
        resolve_fee_schedule("binance", Some(&rc), vike_venues::fee_schedule_for("binance")),
        live
    );
}

#[test]
fn none_and_error_both_fall_back_to_static() {
    let none = FeeClient(Ok(None));
    let err = FeeClient(Err("boom".to_string()));
    let expect = vike_venues::fee_schedule_for("okx");
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

/// The default value (`Linear`, 1.0) yields NO grid, so `Account::contract_value_of` falls through to
/// its default and every venue but deribit and ibkr mounts byte-identically.
#[test]
fn the_default_contract_value_yields_no_grid() {
    use vike_model::ContractValue;
    assert!(super::contract_value_grid("BTCUSDT", ContractValue::default()).is_none());
    // an explicit linear 1.0 is arithmetically the same as no grid: collapsed, not carried
    assert!(super::contract_value_grid("BTC-8JUL26-62000-C", ContractValue::linear(1.0)).is_none());
}

/// A real value lands under the mounted symbol: the key `Account::contract_value_of` (hence the
/// control door's notional cap and the GUI snapshot) looks up. An INVERSE value of 1.0 is NOT the
/// default, so it lands (decision 0170: the kind is the point).
#[test]
fn a_real_contract_value_lands_in_the_grid() {
    use vike_model::ContractValue;
    let inverse = super::contract_value_grid("BTC-PERPETUAL", ContractValue::inverse(1.0))
        .expect("an inverse 1.0 is not the default → a grid");
    assert_eq!(inverse.get("BTC-PERPETUAL"), Some(&ContractValue::inverse(1.0)));
    assert_eq!(inverse.len(), 1, "only the mounted symbol");
    let unit = super::contract_value_grid("ESZ6", ContractValue::linear(50.0))
        .expect("a linear unit other than 1.0 → a grid");
    assert_eq!(unit.get("ESZ6"), Some(&ContractValue::linear(50.0)));
}

/// A degenerate unit is folded to 1.0 with the kind kept (a zero would size every order at nothing).
#[test]
fn a_degenerate_unit_is_folded_in_the_grid() {
    use vike_model::ContractValue;
    for bad in [0.0, -1.0, f64::NAN] {
        assert!(super::contract_value_grid("X", ContractValue::linear(bad)).is_none(), "{bad}");
        let g = super::contract_value_grid("X", ContractValue::inverse(bad)).expect("inverse");
        assert_eq!(g.get("X"), Some(&ContractValue::inverse(1.0)));
    }
}

/// End-to-end through `Account::contract_value_of`: the grid's value, the default for an unlisted
/// symbol.
#[test]
fn grid_drives_account_contract_value_of() {
    use vike_model::ContractValue;
    let acct = vike_exec::Account::new(
        1.0,
        "deribit",
        super::contract_value_grid("BTC-PERPETUAL", ContractValue::inverse(1.0)),
        vike_exec::BalanceMode::Delta,
    );
    assert_eq!(acct.contract_value_of("BTC-PERPETUAL"), ContractValue::inverse(1.0));
    assert_eq!(
        acct.contract_value_of("ETH-PERPETUAL"),
        ContractValue::linear(1.0),
        "unlisted → the default"
    );

    // and the no-value venue is identical to `None`
    let plain = vike_exec::Account::new(1.0, "binance", None, vike_exec::BalanceMode::Delta);
    let wired = vike_exec::Account::new(
        1.0,
        "binance",
        super::contract_value_grid("BTCUSDT", ContractValue::default()),
        vike_exec::BalanceMode::Delta,
    );
    assert_eq!(plain.contract_value_of("BTCUSDT"), wired.contract_value_of("BTCUSDT"));
}

/// [`super::margin_mode_grid`]'s collapse rule (the margin twin of `contract_value_grid`): `Cross`,
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
