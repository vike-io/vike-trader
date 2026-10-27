//! The cTrader reconcile reports an open position under the key the engine's book holds it by.
//!
//! The engine's book is ONE signed net per `(symbol, BOTH)`: `event_mapper::deal_fill` publishes every
//! fill with the default `position_side`, and `vike_exec::recon::diff` matches a report to the book by
//! `(symbol, position_side)` exactly. The venue lists one row per `positionId`, so a report spelled
//! `Long` never met the book's `BOTH` key and an open position held an `OrphanLocalPosition` plus a
//! `PositionOnlyExternal` on every pass until it closed (measured live, 2026-10-26, a hedged demo
//! account, v0.1.93). These tests drive the REAL `CtraderReconClient::fetch_position_status_reports`
//! over the in-process fake venue (rows by `positionId`, sides of either sign) and hand the answer to
//! the REAL `diff` against a book folded from the REAL mapped fill: the same position is no divergence,
//! and a genuinely missing, extra, resized or locked position still is.

mod common;

use std::time::Duration;

use vike_ctrader::CtraderReconClient;
use vike_ctrader::conn::ConnConfig;
use vike_ctrader::event_mapper::deal_fill;
use vike_ctrader::proto::{
    ProtoOaDeal, ProtoOaExecutionEvent, ProtoOaLightSymbol, ProtoOaTradeSide,
};
use vike_ctrader::symbols::SymbolMap;
use vike_exec::recon::{Divergence, LocalCash, OwnedLocalState, ReconClient, diff};
use vike_exec::{Account, BalanceMode};
use vike_model::PositionStatusReport;
use vike_model::events::PositionSide;

use common::FakeCtrader;

/// The venue's `(side_sign, volume_centi)` positions on EURUSD, as `fetch_position_status_reports` reports them.
fn reports(seed: &[(i32, i64)]) -> Vec<PositionStatusReport> {
    let server = FakeCtrader::start_close_preseeded(seed);
    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    let client =
        CtraderReconClient::connect_with_idle_revive(&cfg, "EURUSD", Duration::from_secs(86_400))
            .expect("recon client connects");
    client.fetch_position_status_reports().expect("position reports")
}

/// The engine's local state after the node's own BUY of `volume_centi` EURUSD: the real mapped fill folded
/// into an `Account`, read the way `ExecutionEngine::local_view` reads it.
fn book_long(volume_centi: i64) -> OwnedLocalState {
    let symbols = SymbolMap::from_symbols(
        &[ProtoOaLightSymbol {
            symbol_id: 1,
            symbol_name: Some("EURUSD".to_string()),
            ..Default::default()
        }],
        &[],
    );
    let ev = ProtoOaExecutionEvent {
        deal: Some(ProtoOaDeal {
            deal_id: 1,
            order_id: 1,
            symbol_id: 1,
            volume: volume_centi,
            filled_volume: volume_centi,
            execution_price: Some(1.10),
            trade_side: ProtoOaTradeSide::Buy as i32,
            ..Default::default()
        }),
        ..Default::default()
    };
    let fill = deal_fill(&ev, "c-1", &symbols, 2).expect("a deal");
    let mut account = Account::new(1.0, "ctrader", None, BalanceMode::Delta);
    account.apply_fill(&fill);
    OwnedLocalState {
        venue: "ctrader".to_string(),
        orders: Default::default(),
        seen_trade_ids: Default::default(),
        positions: account
            .positions
            .iter()
            .map(|((_, symbol, side), entry)| ((symbol.to_string(), *side), entry.size))
            .collect(),
        qty_tol: 1e-9,
        cash: LocalCash::default(),
    }
}

/// A book that holds nothing.
fn book_flat() -> OwnedLocalState {
    OwnedLocalState { positions: Default::default(), ..book_long(100) }
}

fn held(reports: &[PositionStatusReport], local: &OwnedLocalState) -> Vec<Divergence> {
    diff(&[], &[], reports, &local.as_view(), None)
}

/// THE FINDING: a hedged account's one open long, which the node itself bought, is no divergence.
#[test]
fn a_position_the_node_holds_is_reported_under_the_books_key_and_raises_nothing() {
    let rows = reports(&[(1, 100_000)]);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].position_side, PositionSide::Both, "the book's key, not the venue's side");
    assert_eq!(rows[0].qty, 1000.0, "the sign stays in qty");
    assert_eq!(held(&rows, &book_long(100_000)), Vec::new());
}

/// Several positions of one direction (a hedged account opens a fresh `positionId` per order) are the
/// book's one net.
#[test]
fn several_positions_of_one_direction_are_the_books_one_net() {
    let rows = reports(&[(1, 100_000), (1, 50_000)]);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!((rows[0].position_side, rows[0].qty), (PositionSide::Both, 1500.0));
    assert_eq!(held(&rows, &book_long(150_000)), Vec::new());
}

/// A short is the same row with the sign in `qty`.
#[test]
fn a_short_is_a_negative_both_row() {
    let rows = reports(&[(-1, 100_000)]);
    assert_eq!((rows[0].position_side, rows[0].qty), (PositionSide::Both, -1000.0));
}

/// Real drift is still held: a venue position the book never opened.
#[test]
fn a_position_the_book_does_not_hold_is_still_held() {
    let d = held(&reports(&[(1, 100_000)]), &book_flat());
    assert!(
        matches!(d.as_slice(), [Divergence::PositionOnlyExternal(r)] if r.qty == 1000.0 && r.position_side == PositionSide::Both),
        "{d:?}"
    );
}

/// Real drift is still held: the book and the venue disagree on the size.
#[test]
fn a_size_the_book_disagrees_with_is_still_a_drift() {
    let d = held(&reports(&[(1, 100_000)]), &book_long(50_000));
    assert!(
        matches!(d.as_slice(), [Divergence::PositionDrift { local_qty, report }] if *local_qty == 500.0 && report.qty == 1000.0),
        "{d:?}"
    );
}

/// Real drift is still held: the book holds a position the venue closed (cTrader then omits it, and the
/// client reports the flat row).
#[test]
fn a_position_the_venue_closed_is_still_a_drift_against_the_flat_row() {
    let rows = reports(&[]);
    assert_eq!((rows.len(), rows[0].position_side, rows[0].qty), (1, PositionSide::Both, 0.0));
    let d = held(&rows, &book_long(100_000));
    assert!(
        matches!(d.as_slice(), [Divergence::PositionDrift { local_qty, .. }] if *local_qty == 1000.0),
        "{d:?}"
    );
}

/// A locked pair (a long and a short of the same size) nets to nothing in the book, so it must NOT be
/// netted away in the report: it is the divergence the 2026-10-25 stop that opened a short beside its
/// long raised, and each leg stays a held `PositionOnlyExternal`. The legs still sum to the net.
#[test]
fn a_locked_pair_stays_two_held_rows() {
    let rows = reports(&[(1, 100_000), (-1, 100_000)]);
    let sides: Vec<_> = rows.iter().map(|r| (r.position_side, r.qty)).collect();
    assert_eq!(sides, [(PositionSide::Long, 1000.0), (PositionSide::Short, -1000.0)]);
    assert_eq!(rows.iter().map(|r| r.qty).sum::<f64>(), 0.0, "the core sums the rows to the net");
    let d = held(&rows, &book_flat());
    assert!(
        matches!(d.as_slice(), [Divergence::PositionOnlyExternal(a), Divergence::PositionOnlyExternal(b)]
            if a.position_side == PositionSide::Long && b.position_side == PositionSide::Short),
        "{d:?}"
    );
}
