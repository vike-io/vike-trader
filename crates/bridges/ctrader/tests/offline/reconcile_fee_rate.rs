//! **The broker's own commission on the mounted symbol is the account's fee schedule** (FEES-AUDIT
//! 2026-11-08): `ReconcileClientCtrader::fetch_fee_rates` reads `ProtoOASymbol.commissionType`,
//! `preciseTradingCommissionRate` and `preciseMinCommission` off the handshake's `SymbolById` answer.
//!
//! ⚠ No recorded frame in this repository carries these fields (the catalog capture and the CFD round
//! trips kept none, and cTrader is not contacted before the FX open), so the symbols here are BUILT from
//! the vendored proto schema (`crates/bridges/ctrader/proto/OpenApiModelMessages.proto`'s `ProtoOASymbol`)
//! and cross the in-process fake's protobuf socket like any other frame. The figures are the ones measured
//! on the hedged demo account's fills (`crates/bridges/ctrader/CLAUDE.md`): 30 USD per million USD of
//! notional per side on FX and metals, nothing on the CFDs.
//!
//! The FX-open check: on the demo account, the reconcile client of an EURUSD mount must answer 0.3 bps a
//! side, and a 1000-unit EURUSD fill's `ProtoOADeal.commission` must equal that schedule to the cent.

use std::time::Duration;

use vike_ctrader::ReconcileClientCtrader;
use vike_ctrader::instrument::symbols::SymbolMap;
use vike_ctrader::test_support::{FakeCtrader, ForeignBook};
use vike_ctrader::transport::connection::ConnectionConfig;
use vike_ctrader::transport::proto::{ProtoOaCommissionType, ProtoOaLightSymbol, ProtoOaSymbol};
use vike_execution::recon::ReconcileClient;
use vike_model::FeeSchedule;

const EURUSD: i64 = 1;
/// 30 USD per million USD, times 10^8 (`preciseTradingCommissionRate` of an amount type).
const THIRTY_PER_MILLION: i64 = 3_000_000_000;

/// A client mounted on EURUSD over `book`.
fn client(book: ForeignBook) -> (FakeCtrader, ReconcileClientCtrader) {
    let server = FakeCtrader::start_foreign_book(book);
    let config = ConnectionConfig::for_test(server.addr(), "cid", "secret", "token");
    let client = ReconcileClientCtrader::connect_with_idle_revive(
        &config,
        "EURUSD",
        Duration::from_secs(86_400),
    )
    .expect("recon client connects");
    (server, client)
}

/// EURUSD states `USD_PER_MILLION_USD` at 30: the account's schedule for the mount is 0.3 bps a side, so a
/// 1000-unit fill at 1.17 costs 0.0351 USD, the 3 cents the demo broker charged such a deal.
///
/// Base: the client kept the trait's `Ok(None)` and the mount was priced by the static `ctrader` row.
#[test]
fn the_mounted_symbols_commission_is_the_accounts_fee_schedule() {
    let commissions =
        vec![(EURUSD, ProtoOaCommissionType::UsdPerMillionUsd as i32, THIRTY_PER_MILLION)];
    let (_server, client) = client(ForeignBook { commissions, ..Default::default() });
    let schedule = client.fetch_fee_rates().expect("the read").expect("a stated commission");
    assert_eq!(schedule, FeeSchedule::PercentMakerTaker { maker_bps: 0.3, taker_bps: 0.3 });
    let fee = schedule.commission(false, 1_000.0, 1.17);
    assert!((fee - 0.0351).abs() < 1e-12, "{fee}");
}

/// A symbol whose full entity states no rate leaves the static row standing (`Ok(None)`), as before.
#[test]
fn a_symbol_that_states_no_commission_reads_none() {
    let (_server, client) = client(ForeignBook::default());
    assert_eq!(client.fetch_fee_rates(), Ok(None));
}

/// One full symbol built from the schema, through the map the handshake builds.
fn terms(kind: ProtoOaCommissionType, rate: i64, min: Option<i64>, lot_size: i64) -> SymbolMap {
    let light = [ProtoOaLightSymbol {
        symbol_id: 7,
        symbol_name: Some("X".to_string()),
        ..Default::default()
    }];
    let full = [ProtoOaSymbol {
        symbol_id: 7,
        digits: 2,
        lot_size: Some(lot_size),
        commission_type: Some(kind as i32),
        precise_trading_commission_rate: Some(rate),
        precise_min_commission: min,
        ..Default::default()
    }];
    SymbolMap::from_symbols(&light, &full)
}

/// Each commission type maps to the schedule it states: a CFD at a zero per-lot rate is a stated zero
/// (the demo broker's indices, energy and crypto), a percentage of value is basis points, an amount per lot
/// in the quote currency is per base unit, and a USD amount per lot on a symbol quoted in another currency
/// is refused in words, never booked as quote currency.
#[test]
fn each_commission_type_states_its_schedule() {
    let read = |map: SymbolMap, quote: Option<&str>| {
        let lot = map.volume_grid(7).and_then(|g| g.lot_units());
        map.commission(7).expect("terms").fee_schedule(lot, quote)
    };
    let zero = FeeSchedule::PercentMakerTaker { maker_bps: 0.0, taker_bps: 0.0 };
    assert_eq!(
        read(terms(ProtoOaCommissionType::UsdPerLot, 0, None, 100), Some("USD")),
        Ok(Some(zero))
    );
    assert_eq!(
        read(terms(ProtoOaCommissionType::PercentageOfValue, 500, None, 100), Some("EUR")),
        Ok(Some(FeeSchedule::PercentMakerTaker { maker_bps: 0.5, taker_bps: 0.5 })),
        "0.005 % of the notional, in any currency"
    );
    // 15 EUR per lot of 10 units (lot_size 1000 centi-units), a 1 EUR minimum.
    assert_eq!(
        read(
            terms(ProtoOaCommissionType::QuoteCcyPerLot, 1_500_000_000, Some(100_000_000), 1_000),
            Some("EUR")
        ),
        Ok(Some(FeeSchedule::PerShareWithFloor { per_share: 1.5, min: 1.0, max_percent: 0.0 }))
    );
    let refused =
        read(terms(ProtoOaCommissionType::UsdPerLot, 1_500_000_000, None, 1_000), Some("JPY"))
            .expect_err("a USD amount is not a JPY one");
    assert!(refused.contains("quoted in JPY"), "{refused}");
}
