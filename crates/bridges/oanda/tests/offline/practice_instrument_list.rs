//! The REAL instrument list of the practice account (`tests/fixtures/practice_instruments_2026-10-26.json`),
//! captured with `GET /v3/accounts/{id}/instruments` on 2026-10-26: what OANDA lists to the account the
//! node trades, run through the bridge's own parser and the door's grid rule.
//!
//! **Captured, unlike `tests/fixtures/instruments.json`** (docs-shaped, illustrative values). Two facts
//! come out of the capture and are pinned here so a docs sentence cannot drift from them:
//!
//! 1. the account's list holds 68 rows and ALL of them are `CURRENCY`: OANDA lists no metal, index,
//!    commodity or bond to this account, and an order on `XAU_USD` or `SPX500_USD` is answered
//!    `INSTRUMENT_NOT_TRADEABLE` ("The instrument specified is not tradeable by the Account", measured the
//!    same day), so the unit semantics of those classes cannot be measured on it
//!    (`tests/oanda_unit_semantics_smoke.rs` is the measurement, for an account that lists them);
//! 2. every one of the 68 passes `vike_oanda::grid_refusal`: every ticker this account can trade is already
//!    admitted on its own grid, five decimals or three, whole units.

use serde_json::Value;
use vike_model::{Contract, Instrument};
use vike_oanda::{grid_refusal, parse_instruments};

const PRACTICE_LIST: &str = include_str!("../fixtures/practice_instruments_2026-10-26.json");

fn rows() -> Vec<Instrument> {
    let body: Value = serde_json::from_str(PRACTICE_LIST).expect("the capture is JSON");
    parse_instruments(&body)
}

#[test]
fn the_account_lists_68_currency_pairs_and_no_cfd_metal_index_or_bond() {
    let rows = rows();
    assert_eq!(rows.len(), 68, "the capture holds 68 rows");
    for row in &rows {
        let name = &row.raw_symbol;
        assert_eq!(row.contract_type.as_deref(), Some("CURRENCY"), "{name}");
        assert!(matches!(row.contract, Contract::Fx(_)), "{name} parses as a currency pair");
    }
    for cfd in
        ["XAU_USD", "XAG_USD", "SPX500_USD", "NAS100_USD", "DE30_EUR", "UK100_GBP", "BCO_USD"]
    {
        assert!(rows.iter().all(|r| r.raw_symbol != cfd), "{cfd} is not in the account's list");
    }
}

#[test]
fn every_listed_pair_is_admitted_on_its_own_grid() {
    let (mut five, mut three) = (0, 0);
    for row in rows() {
        let name = &row.raw_symbol;
        let p = &row.properties;
        assert_eq!(
            grid_refusal(name, row.contract_type.as_deref(), p),
            None,
            "{name} is refused by the door's grid rule"
        );
        assert_eq!(p.step_size, 1.0, "{name}: whole units");
        assert_eq!(p.min_qty, 1.0, "{name}: the minimum is one unit");
        assert!(p.max_qty >= 1e7, "{name}: maximumOrderUnits {}", p.max_qty);
        if p.tick_size == 0.00001 {
            five += 1;
        } else if p.tick_size == 0.001 {
            three += 1;
        } else {
            panic!("{name}: a price grid of {} is neither five nor three decimals", p.tick_size);
        }
        // the pip is ten ticks on every pair OANDA lists here (0.0001 / 0.01)
        let Contract::Fx(fx) = &row.contract else { unreachable!("checked above") };
        let pip = fx.pip_size.expect("pipLocation");
        assert!(
            (pip / p.tick_size - 10.0).abs() < 1e-6,
            "{name}: pip {pip} vs tick {}",
            p.tick_size
        );
    }
    assert_eq!((five, three), (55, 13), "55 five-decimal pairs and 13 three-decimal ones");
}
