//! The shared tail's unit tests: what a bind becomes before the fold builds the engine.

use super::*;
use crate::testutil::NoopClient;
use vike_model::AccountTier;

/// The spec's Finding 1, generic: the tier the parts carry is the BOUND tier — at both tiers, so
/// a fold that carried one fixed tier fails one half. (And the venue-confirmed identity rides the
/// parts untouched: the record is made by the backend, AFTER the fee schedule.)
#[test]
fn the_parts_carry_the_bound_tier_and_the_confirmed_book() {
    for bound_tier in [AccountTier::Live, AccountTier::Demo] {
        let bind = AdapterBind {
            client: Box::new(NoopClient),
            recon: None,
            bound_tier,
            grid: None,
            contract_value: None,
            margin_mode: None,
            leg_grids: vec![],
            confirmed_book: Some(ConfirmedBook {
                book: "0xplanted".to_string(),
                evidence: "a planted answer",
                tier: bound_tier,
            }),
            leverage_cap: None,
            account_ccy: None,
            account_ccy_assumed: false,
            mount_symbol: None,
        };
        let parts = parts_from_bind(
            "BTCUSDT.P",
            &[],
            bind,
            GridSource::PerSymbolFetch,
            vike_model::FeeSchedule::Free,
        );
        assert_eq!(parts.bound_tier, bound_tier, "the bound tier, never a CEX conjunct");
        assert_eq!(parts.grid_source, GridSource::PerSymbolFetch);
        assert_eq!(parts.identity.map(|i| i.tier), Some(bound_tier));
    }
}

/// A bind with no venue-fetched grid builds the unbounded limits the old parts did, and a declared
/// leg the venue could not answer gets no row (the lookup closure answers `None`).
#[test]
fn a_bind_with_no_grid_builds_empty_limits() {
    let bind = AdapterBind {
        client: Box::new(NoopClient),
        recon: None,
        bound_tier: AccountTier::Demo,
        grid: None,
        contract_value: None,
        margin_mode: None,
        leg_grids: vec![],
        confirmed_book: None,
        leverage_cap: None,
        account_ccy: None,
        account_ccy_assumed: false,
        mount_symbol: None,
    };
    let legs = vec!["ETHUSDT".to_string()];
    let parts =
        parts_from_bind("BTCUSDT", &legs, bind, GridSource::InHand, vike_model::FeeSchedule::Free);
    assert_eq!(parts.contract_value, vike_model::ContractValue::default());
    assert_eq!(parts.default_margin_mode, vike_model::MarginMode::Cross);
    assert!(parts.symbol_grids.is_empty(), "no leg grid was answered");
    assert!(parts.identity.is_none());
}

/// A degenerate value from the wire (a zero or negative unit, NaN) is FOLDED to a unit of 1.0 with
/// its kind kept, so it can neither size every order at nothing nor flip a sign (decision 0170).
#[test]
fn a_degenerate_contract_value_is_folded_at_the_bind() {
    for bad in [0.0, -2.0, f64::NAN, f64::INFINITY] {
        let bind = AdapterBind {
            client: Box::new(NoopClient),
            recon: None,
            bound_tier: AccountTier::Demo,
            grid: None,
            contract_value: Some(vike_model::ContractValue::inverse(bad)),
            margin_mode: None,
            leg_grids: vec![],
            confirmed_book: None,
            leverage_cap: None,
            account_ccy: None,
            account_ccy_assumed: false,
            mount_symbol: None,
        };
        let parts = parts_from_bind(
            "BTC-PERPETUAL",
            &[],
            bind,
            GridSource::PerSymbolFetch,
            vike_model::FeeSchedule::Free,
        );
        assert_eq!(parts.contract_value, vike_model::ContractValue::inverse(1.0), "{bad}");
    }
}
