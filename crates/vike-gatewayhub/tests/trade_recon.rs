//! **Reconcile over the lease**, against the real server and the real client (B.7, R7 and R12 of the
//! wave-2 plan): a fetch that did not happen is never an empty answer. The venue's rows when it
//! answered (one empty page for an empty list), `ReconFailed` when it was asked and failed, NOT READY
//! when no pass was attempted; pages concatenate in order, a slow venue that keeps reporting progress
//! is never clipped, and the bridge's reconcile trigger reaches the client as `ReconcileNow`.
#![cfg(feature = "trade")]

use std::sync::mpsc;
use std::time::Duration;

use vike_exec::recon::ReconClient;
use vike_gatewayhub::testkit::gateway::TradeKit;
use vike_gatewayhub::testkit::trade::{ScriptedStart, ScriptedTrade};
use vike_gatewayhub_client::proto::AdapterReply;
use vike_gatewayhub_client::trade::{RECON_NOT_READY_PREFIX, is_recon_not_ready};
use vike_model::{FeeSchedule, OrderStatus, OrderStatusReport, OrderType};

#[allow(dead_code, clippy::allow_attributes)]
#[path = "support/trade_rig.rs"]
mod trade_rig;

use trade_rig::{Rig, open, open_with, quick_config, rig, rig_with, start_args};

const WAIT: Duration = Duration::from_secs(8);

fn order_report(coid: &str) -> OrderStatusReport {
    OrderStatusReport {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        venue_order_id: "v1".into(),
        client_order_id: Some(coid.into()),
        side: 1,
        order_type: OrderType::Limit,
        qty: 1.0,
        filled_qty: 0.0,
        avg_px: 0.0,
        status: OrderStatus::Accepted,
        ts: 1,
    }
}

fn started(
    rig: &Rig,
) -> (vike_gatewayhub_client::trade::TradeSession, vike_gatewayhub::testkit::trade::ScriptedAdapter)
{
    let (session, _lane) = open(rig, "tradehub-1");
    assert!(matches!(
        session.start_adapter(start_args()).expect("answered"),
        AdapterReply::Live { .. }
    ));
    let adapter = rig.lane.adapter("binance", None).expect("the adapter");
    (session, adapter)
}

#[test]
fn a_pass_before_the_adapter_exists_is_not_ready_never_an_empty_answer() {
    let rig = rig();
    let (session, _lane) = open(&rig, "tradehub-1");
    let recon = session.recon_client();
    let err = recon.fetch_order_status_reports(0).expect_err("no adapter yet");
    assert!(is_recon_not_ready(&err), "{err}");
    assert!(err.starts_with(RECON_NOT_READY_PREFIX));
    assert!(err.contains("no venue adapter"), "{err}");
}

#[test]
fn an_adapter_with_no_reconcile_handle_is_not_ready_with_its_reason() {
    let rig = rig_with(
        ScriptedTrade::new(&["binance"]).with_start(
            "binance",
            None,
            ScriptedStart::live().no_recon(),
        ),
        quick_config(),
        TradeKit::default(),
    );
    let (session, _adapter) = started(&rig);
    let err = session.recon_client().fetch_balance().expect_err("no handle");
    assert!(is_recon_not_ready(&err) && err.contains("no reconcile handle"), "{err}");
}

#[test]
fn the_venues_rows_come_back_and_an_empty_list_is_one_empty_page() {
    let rig = rig();
    let (session, adapter) = started(&rig);
    let recon = session.recon_client();
    // The venue says empty: that, and only that, is an empty Ok.
    assert_eq!(recon.fetch_order_status_reports(0), Ok(Vec::new()));
    assert_eq!(recon.fetch_fill_reports(0), Ok(Vec::new()));
    assert_eq!(recon.fetch_position_status_reports(), Ok(Vec::new()));
    assert_eq!(recon.fetch_balance(), Ok(None));
    assert_eq!(recon.fetch_fee_rates(), Ok(None));
    assert_eq!(recon.fetch_account_identity(), Ok(None));

    adapter.set_orders(Ok(vec![order_report("a"), order_report("b")]));
    adapter.set_balance(Ok(Some(1234.5)));
    adapter
        .set_fee_rates(Ok(Some(FeeSchedule::PercentMakerTaker { maker_bps: 1.0, taker_bps: 4.5 })));
    adapter.set_identity(Ok(Some("acct-42".to_string())));
    let orders = recon.fetch_order_status_reports(0).expect("rows");
    let ids: Vec<&str> = orders.iter().filter_map(|o| o.client_order_id.as_deref()).collect();
    assert_eq!(ids, ["a", "b"]);
    assert_eq!(recon.fetch_balance(), Ok(Some(1234.5)));
    assert_eq!(
        recon.fetch_fee_rates(),
        Ok(Some(FeeSchedule::PercentMakerTaker { maker_bps: 1.0, taker_bps: 4.5 }))
    );
    assert_eq!(recon.fetch_account_identity(), Ok(Some("acct-42".to_string())));
}

#[test]
fn a_fetch_the_venue_failed_is_an_error_that_is_not_a_skipped_pass() {
    let rig = rig();
    let (session, adapter) = started(&rig);
    adapter.set_fills(Err("binance: -1021 timestamp outside recvWindow".to_string()));
    let err = session.recon_client().fetch_fill_reports(0).expect_err("the venue failed");
    assert_eq!(err, "binance: -1021 timestamp outside recvWindow");
    assert!(!is_recon_not_ready(&err), "a failed fetch feeds the staleness escalation");
}

#[test]
fn a_long_list_is_paged_and_arrives_whole_in_order() {
    let mut config = quick_config();
    // A page holds about two reports.
    config.limits.fetch_page_ceiling = 700;
    let rig = rig_with(ScriptedTrade::new(&["binance"]), config, TradeKit::default());
    let (session, adapter) = started(&rig);
    let rows: Vec<OrderStatusReport> = (0..25).map(|n| order_report(&format!("o{n:02}"))).collect();
    adapter.set_orders(Ok(rows.clone()));
    let got = session.recon_client().fetch_order_status_reports(0).expect("paged");
    assert_eq!(got, rows, "every row, in order, across pages");
}

#[test]
fn a_slow_venue_that_keeps_reporting_progress_is_never_clipped() {
    // The client calls a silent daemon dead after max(900 ms, 3 x the progress cadence); the venue takes
    // longer than that, and only the progress frames keep the fetch alive.
    let mut config = quick_config();
    config.timings.fetch_progress_every = Duration::from_millis(150);
    let rig = rig_with(ScriptedTrade::new(&["binance"]), config, TradeKit::default());
    let (session, adapter) = started(&rig);
    adapter.set_recon_delay(Duration::from_millis(2_200));
    adapter.set_balance(Ok(Some(9.0)));
    assert_eq!(session.recon_client().fetch_balance(), Ok(Some(9.0)));
}

#[test]
fn the_identity_is_recorded_through_the_reconcile_worker_with_the_handle() {
    let rig = rig();
    let (session, _adapter) = started(&rig);
    session.record_identity().expect("recorded");
    assert!(rig.lane.calls().iter().any(|c| matches!(
        c,
        vike_gatewayhub::testkit::trade::TradeCall::RecordIdentity { had_recon: true, .. }
    )));
}

#[test]
fn the_bridges_reconcile_trigger_reaches_the_client_and_waits_for_one_that_is_away() {
    let rig = rig();
    let (trigger_tx, trigger_rx) = mpsc::channel::<()>();
    let (session, _lane) = open_with(&rig, "tradehub-1", |link| link.with_trigger(trigger_tx));
    let mut start = start_args();
    start.takes_trigger = true;
    assert!(matches!(session.start_adapter(start).expect("answered"), AdapterReply::Live { .. }));
    let adapter = rig.lane.adapter("binance", None).expect("the adapter");
    assert!(adapter.fire_trigger(), "the lane was handed a trigger channel");
    assert!(
        trigger_rx.recv_timeout(WAIT).is_ok(),
        "the bridge's trigger became a ReconcileNow for the client"
    );
}

/// **R31, the reconcile health gate over the hop.** A venue whose price feed is mid-gap is not
/// reconciled against: the daemon answers every `Recon` request NOT READY with the `Degraded` words
/// (the tradehub's manager skips that pass), never an empty answer and never a fetch of reports the
/// venue has not caught up on. It clears the moment the feed recovers, and another venue is untouched.
#[test]
fn a_venue_whose_feed_is_mid_gap_is_not_ready_degraded_and_recovers() {
    let rig = rig();
    let (session, adapter) = started(&rig);
    adapter.set_orders(Ok(vec![order_report("a")]));
    let recon = session.recon_client();
    assert_eq!(recon.fetch_order_status_reports(0).map(|o| o.len()), Ok(1));

    rig.lane.set_degraded("binance", Some(&vike_gatewayhub::health::degraded_reason("binance")));
    for err in [
        recon.fetch_order_status_reports(0).expect_err("degraded").to_string(),
        recon.fetch_balance().expect_err("degraded").to_string(),
    ] {
        assert!(
            is_recon_not_ready(&err),
            "a degraded venue is a skipped pass, not a fetch error: {err}"
        );
        assert!(err.contains(vike_gatewayhub::health::DEGRADED_PREFIX), "{err}");
    }

    rig.lane.set_degraded("bybit", Some("someone else's gap"));
    rig.lane.set_degraded("binance", None);
    assert_eq!(
        recon.fetch_order_status_reports(0).map(|o| o.len()),
        Ok(1),
        "the gate is per venue and clears on recovery"
    );
}

/// **The reconcile client learns the engine's admitted symbols from a COMMIT, not from a lookup.** The
/// node asks for a symbol's facts to look (a Preview, the Telegram look, an order the door then
/// refuses: all probes) and, once `AdmitSymbol` is sent, commits (`TradeSession::admit_symbol`). Only
/// a GRANTED commit reaches the account's reconcile client (`admit_symbol`); a probe, a refused commit
/// or an unlisted symbol never takes one of its sixteen slots. The admission is queued BEFORE the reply
/// goes out, and the worker is one FIFO, so the very next fetch already sees it (no waiting).
#[test]
fn only_a_granted_commit_tells_the_reconcile_client_and_it_does_so_before_the_reply() {
    let facts = vike_gatewayhub_client::proto::WireSymbolFacts {
        properties: vike_model::SymbolProperties {
            tick_size: 0.01,
            step_size: 0.001,
            ..Default::default()
        },
        ..Default::default()
    };
    let rig = rig_with(
        ScriptedTrade::new(&["binance"])
            .with_symbol_facts("ETHUSDT.P", Ok(facts.clone()))
            .with_symbol_facts("SOLUSDT.P", Ok(facts))
            .with_symbol_facts("NOPE", Err("binance: no instrument NOPE".into())),
        quick_config(),
        TradeKit::default(),
    );
    let (session, adapter) = started(&rig);
    let recon = session.recon_client();
    let seen = |adapter: &vike_gatewayhub::testkit::trade::ScriptedAdapter| {
        recon.fetch_order_status_reports(0).expect("fetch");
        adapter.admitted_at_order_fetches().last().cloned().expect("a fetch ran")
    };

    // A probe, granted or refused, tells the client nothing.
    assert!(session.symbol_facts("SOLUSDT.P").is_ok());
    assert!(session.symbol_facts("NOPE").is_err());
    assert_eq!(seen(&adapter), Vec::<String>::new(), "a lookup is not an admission");

    // A refused commit tells it nothing either.
    assert!(session.admit_symbol("NOPE").is_err());
    assert_eq!(seen(&adapter), Vec::<String>::new());

    // A granted commit: the very next fetch holds the symbol, with no waiting in between.
    assert!(session.admit_symbol("ETHUSDT.P").is_ok());
    assert_eq!(seen(&adapter), ["ETHUSDT.P"], "queued before the reply, ahead of any later fetch");
    assert_eq!(adapter.admitted_symbols(), ["ETHUSDT.P"], "the previewed SOLUSDT.P took no slot");
}
