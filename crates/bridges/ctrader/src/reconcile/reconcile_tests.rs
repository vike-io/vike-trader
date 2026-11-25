//! **A reconcile pass between a close a dead socket left unanswered and the reconnect's settle** (the CI box, 2026-11-09,
//! the CT5 drill on demo account 5): a buy filled, an input cut dropped the venue's answers, the flatten went out one
//! second into it and filled unheard, and a pass ran 4 s before the reconnect read the venue's lists. It raised a
//! `MissingFill` for the close's deal 958656727 under the OPENER's order client id, which stayed held after the settle booked the
//! same deal under the close's. Replayed here against the fake venue, whose closing deal carries the position's label
//! as the venue's does: the pass before the settle raises nothing, and after it the close is named by its own order client id and
//! nothing diverges.

use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_execution::recon::{
    ReconcileClient, ReconcileDivergence, ReconcileLocalCash, ReconcileLocalState, diff,
};
use vike_execution::{ExecutionClient, event_channel};
use vike_model::OrderRequest;
use vike_model::events::{Event, PositionSide};

use super::ReconcileClientCtrader;
use crate::test_support::{
    self, FakeCtrader, NoopSink, UNHEARD_DEAL_ID, UNHEARD_OPENER_DEAL_ID, market,
};
use crate::transport::connection::{ConnectionConfig, connect_and_auth_execution};

/// The order that opened the position: the CLI's open long on account 5 (a v2 id, so ours to any book).
const OPENER: &str = "C000005k3x9q2m7d1p4OL";
/// The flatten: the engine's close long on account 5, the envelope id its `ProtoOAClosePositionReq` goes out under.
const CLOSE: &str = "E000005mluwn7ynzm64CL";

/// The engine's book: `long` units of EURUSD (none: flat) and the trade ids it folded.
fn book(long: Option<f64>, seen: &[&str]) -> ReconcileLocalState {
    ReconcileLocalState {
        venue: "ctrader".to_string(),
        orders: Default::default(),
        seen_trade_ids: seen.iter().map(|id| id.to_string()).collect(),
        positions: long
            .map(|qty| (("EURUSD".to_string(), PositionSide::Both), qty))
            .into_iter()
            .collect(),
        quantity_tolerance: 1e-9,
        cash: ReconcileLocalCash::default(),
    }
}

/// One pass as the manager runs it: every report, then the diff; a fetch that fails skips the pass and raises nothing.
fn pass(client: &ReconcileClientCtrader, local: &ReconcileLocalState) -> Vec<ReconcileDivergence> {
    let reports = (
        client.fetch_order_status_reports(0),
        client.fetch_fill_reports(0),
        client.fetch_position_status_reports(),
    );
    let (Ok(orders), Ok(fills), Ok(positions)) = reports else { return Vec::new() };
    diff(&orders, &fills, &positions, &local.as_view(), None)
}

#[test]
fn a_pass_before_a_lost_close_is_settled_raises_nothing_and_the_close_is_named_by_its_own_order_client_id()
 {
    let (server, gate) = FakeCtrader::start_unheard_close(&[(1, 100_000)], OPENER);
    let config = ConnectionConfig::for_test(server.addr(), "cid", "secret", "token");
    let (events, mut rx) = event_channel(64);
    let handle = connect_and_auth_execution(config.clone(), Arc::new(NoopSink), events.clone())
        .expect("auth ok");
    let execution_book = handle.positions.clone();
    let mut exec =
        test_support::execution_with_no_halt(handle, events, &std::env::temp_dir(), module_path!());
    let recon = ReconcileClientCtrader::connect_with_idle_revive(
        &config,
        "EURUSD",
        Duration::from_secs(86_400),
    )
    .expect("recon client connects");
    let recon = ReconcileClientCtrader { legs: Some(execution_book), ..recon };
    let opener_deal = UNHEARD_OPENER_DEAL_ID.to_string();

    // The flatten: the venue fills it and the socket dies before its answer.
    exec.submit(&OrderRequest { reduce_only: true, ..market(CLOSE, -1, 1000.0) });
    assert!(gate.wait_for_a_reconnect(Duration::from_secs(10)), "the exec called its socket dead");
    assert_eq!(
        pass(&recon, &book(Some(1000.0), &[opener_deal.as_str()])),
        Vec::new(),
        "a pass before the settle sets the venue against a book that has not heard the close"
    );

    // The settle: the venue's lists book the close's fill under the close's order client id and the venue's deal id.
    gate.open();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut close_fill = None;
    while close_fill.is_none() && Instant::now() < deadline {
        close_fill = crate::drain!(rx).into_iter().find_map(|e| match e {
            Event::Fill(f) if f.order_client_id == CLOSE => Some(f),
            _ => None,
        });
        std::thread::sleep(Duration::from_millis(20));
    }
    let close_fill = close_fill.expect("the settle books the close's fill");
    assert_eq!(close_fill.trade_id, UNHEARD_DEAL_ID.to_string().as_str());

    // After it: the close's deal is the close's, and nothing diverges.
    let fills = loop {
        match recon.fetch_fill_reports(0) {
            Ok(fills) => break fills,
            // The events go out a moment before the settle ends.
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => panic!("the settle never ended: {e}"),
        }
    };
    let named: Vec<_> = fills
        .iter()
        .map(|f| (f.trade_id.as_str().to_string(), f.order_client_id.clone()))
        .collect();
    assert_eq!(
        named,
        [
            (opener_deal.clone(), Some(OPENER.to_string())),
            (UNHEARD_DEAL_ID.to_string(), Some(CLOSE.to_string())),
        ],
        "the opener's deal keeps the opener's order client id and the close's is the close's, never the opener's"
    );
    let after = book(None, &[opener_deal.as_str(), close_fill.trade_id.as_str()]);
    assert_eq!(pass(&recon, &after), Vec::new(), "nothing is left to hold after the settle");
    drop(exec);
}
