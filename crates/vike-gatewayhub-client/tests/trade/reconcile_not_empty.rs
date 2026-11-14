#![cfg(feature = "trade")]
//! **A reconcile fetch that did not happen is never an empty answer.**
//!
//! The restore planner reads an empty order report as "nothing resting" and declares every in-scope
//! restored order gone, and a reconcile pass skips on `Err`. So `ReconcileClientGateway` has exactly three
//! kinds of outcome and this suite pins each: the venue's rows (empty only when the VENUE said empty), a
//! NOT READY `Err` (the pass was not attempted: a SKIPPED pass for the manager) and a FETCH `Err` (the
//! venue was asked or the link died mid-fetch: the staleness escalation). It also holds the framing
//! rules: pages concatenate in order and a lost or repeated page is an error, a slow fetch that keeps
//! reporting progress is never clipped while silence is called dead, and a restored order's wire id is
//! prefetched before any reconcile plans against the translation map.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use vike_execution::recon::ReconcileClient;
use vike_gatewayhub_client::proto::{
    ReconcileKind, ReconcilePayload, Response, TradeDown, TradeUp,
};
use vike_gatewayhub_client::trade::{
    RECONCILE_NOT_READY_PREFIX, TradeLinkState, is_reconcile_not_ready,
};
use vike_model::{FeeSchedule, FillReport};

use vike_gatewayhub_client::test_support::fake_trade_peer;

use fake_trade_peer::{Step, TradePeer, TradePeerConfig, empty_payload, open};
use vike_log::poll::wait_until;

const WAIT: Duration = Duration::from_secs(5);

use vike_gatewayhub_client::test_support::fake_trade_peer::order_report;

/// A peer whose Recon answer is `steps(rid, kind)`.
fn peer_answering(
    steps: impl Fn(u64, &ReconcileKind) -> Vec<Step> + Send + Sync + 'static,
) -> TradePeer {
    TradePeer::start(TradePeerConfig::new().with_script(move |frame| match frame {
        TradeUp::Reconcile { rid, kind } => steps(*rid, kind),
        _ => Vec::new(),
    }))
}

fn reply(rid: u64, page: u32, more: bool, payload: ReconcilePayload) -> Step {
    Step::Send(TradeDown::ReconcileReply { rid, page, more, payload, read: None })
}

#[test]
fn a_venue_that_says_empty_is_an_empty_ok_and_that_is_the_only_empty_ok() {
    let peer = peer_answering(|rid, kind| vec![reply(rid, 0, false, empty_payload(kind))]);
    let (session, _lane) = open(&peer, "tradehub-1");
    let recon = session.recon_client();
    assert_eq!(recon.fetch_order_status_reports(0), Ok(Vec::new()));
    assert_eq!(recon.fetch_fill_reports(0), Ok(Vec::new()));
    assert_eq!(recon.fetch_position_status_reports(), Ok(Vec::new()));
    assert_eq!(recon.fetch_balance(), Ok(None));
    assert_eq!(recon.fetch_fee_rates(), Ok(None));
    assert_eq!(recon.fetch_account_identity(), Ok(None));
}

#[test]
fn pages_are_concatenated_in_order_and_the_mass_status_composes_the_three_fetches() {
    let peer = peer_answering(|rid, kind| match kind {
        ReconcileKind::Orders { .. } => vec![
            reply(
                rid,
                0,
                true,
                ReconcilePayload::Orders(vec![order_report("a"), order_report("b")]),
            ),
            reply(rid, 1, false, ReconcilePayload::Orders(vec![order_report("c")])),
        ],
        ReconcileKind::Fills { .. } => {
            vec![reply(rid, 0, false, ReconcilePayload::Fills(Vec::new()))]
        }
        ReconcileKind::Positions => {
            vec![reply(rid, 0, false, ReconcilePayload::Positions(Vec::new()))]
        }
        other => vec![reply(rid, 0, false, empty_payload(other))],
    });
    let (session, _lane) = open(&peer, "tradehub-1");
    let recon = session.recon_client();
    let orders = recon.fetch_order_status_reports(0).expect("paged");
    let ids: Vec<&str> = orders.iter().filter_map(|o| o.order_client_id.as_deref()).collect();
    assert_eq!(ids, ["a", "b", "c"]);
    let mass = recon.fetch_mass_status(0).expect("the default composition");
    assert_eq!(mass.orders.len(), 3);
    // The trait's compose order - orders, fills, positions - is the order the frames crossed in.
    let kinds: Vec<&'static str> = peer
        .ups()
        .iter()
        .filter_map(|f| match f {
            TradeUp::Reconcile { kind: ReconcileKind::Orders { .. }, .. } => Some("orders"),
            TradeUp::Reconcile { kind: ReconcileKind::Fills { .. }, .. } => Some("fills"),
            TradeUp::Reconcile { kind: ReconcileKind::Positions, .. } => Some("positions"),
            _ => None,
        })
        .collect();
    assert_eq!(kinds, ["orders", "orders", "fills", "positions"]);
}

#[test]
fn the_scalar_kinds_cross_and_a_fee_schedule_survives_the_mirror() {
    let peer = peer_answering(|rid, kind| match kind {
        ReconcileKind::Balance => {
            vec![reply(rid, 0, false, ReconcilePayload::Balance(Some(1234.5)))]
        }
        ReconcileKind::FeeRates => vec![reply(
            rid,
            0,
            false,
            ReconcilePayload::FeeRates(Some(FeeSchedule::PercentMakerTaker {
                maker_bps: 1.0,
                taker_bps: 2.5,
            })),
        )],
        ReconcileKind::AccountIdentity => {
            vec![reply(rid, 0, false, ReconcilePayload::AccountIdentity(Some("spot-1".into())))]
        }
        other => vec![reply(rid, 0, false, empty_payload(other))],
    });
    let (session, _lane) = open(&peer, "tradehub-1");
    let recon = session.recon_client();
    assert_eq!(recon.fetch_balance(), Ok(Some(1234.5)));
    assert_eq!(
        recon.fetch_fee_rates(),
        Ok(Some(FeeSchedule::PercentMakerTaker { maker_bps: 1.0, taker_bps: 2.5 }))
    );
    assert_eq!(recon.fetch_account_identity(), Ok(Some("spot-1".to_string())));
}

#[test]
fn not_ready_and_a_fetch_error_are_two_different_errors() {
    let peer = peer_answering(|rid, kind| match kind {
        ReconcileKind::Orders { .. } => vec![Step::Send(TradeDown::ReconcileNotReady {
            rid,
            reason: "the session is not up yet".into(),
        })],
        ReconcileKind::Fills { .. } => {
            vec![Step::Send(TradeDown::ReconcileFailed { rid, error: "binance: HTTP 503".into() })]
        }
        other => vec![reply(rid, 0, false, empty_payload(other))],
    });
    let (session, _lane) = open(&peer, "tradehub-1");
    let recon = session.recon_client();
    let not_ready = recon.fetch_order_status_reports(0).expect_err("not ready is an Err");
    assert!(is_reconcile_not_ready(&not_ready), "{not_ready}");
    assert!(not_ready.starts_with(RECONCILE_NOT_READY_PREFIX));
    assert!(not_ready.contains("the session is not up yet"));
    let failed = recon.fetch_fill_reports(0).expect_err("a venue error is an Err");
    assert_eq!(failed, "binance: HTTP 503", "the venue's own words, unchanged");
    assert!(!is_reconcile_not_ready(&failed), "a fetch error must feed the staleness escalation");
}

#[test]
fn a_down_link_is_not_ready_and_never_an_empty_answer() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let (session, _lane) = open(&peer, "tradehub-1");
    peer.refuse_open(Some(Response::Error("down for the test".into())));
    peer.kill_lease();
    assert!(wait_until(WAIT, || session.link_state() == TradeLinkState::Down));
    let recon = session.recon_client();
    for outcome in [
        recon.fetch_order_status_reports(0).map(|v| v.len()),
        recon.fetch_fill_reports(0).map(|v| v.len()),
        recon.fetch_position_status_reports().map(|v| v.len()),
        recon.fetch_balance().map(|v| usize::from(v.is_some())),
    ] {
        let err = outcome.expect_err("a fetch that did not happen is an Err, not Ok(empty)");
        assert!(is_reconcile_not_ready(&err), "{err}");
    }
}

#[test]
fn a_link_lost_mid_fetch_is_a_fetch_error_and_not_a_skipped_pass() {
    let peer = peer_answering(|_, _| vec![Step::Close]);
    let (session, _lane) = open(&peer, "tradehub-1");
    let err = session.recon_client().fetch_order_status_reports(0).expect_err("the link died");
    assert!(!is_reconcile_not_ready(&err), "the venue may have been asked: {err}");
    assert!(err.contains("lost"), "{err}");
}

#[test]
fn a_lost_or_repeated_page_is_an_error_never_a_short_answer() {
    let peer = peer_answering(|rid, kind| match kind {
        ReconcileKind::Orders { .. } => {
            vec![reply(rid, 1, false, ReconcilePayload::Orders(vec![order_report("late")]))]
        }
        ReconcileKind::Fills { .. } => vec![
            reply(rid, 0, true, ReconcilePayload::Fills(Vec::new())),
            reply(rid, 0, false, ReconcilePayload::Fills(Vec::new())),
        ],
        ReconcileKind::Positions => vec![
            reply(rid, 0, true, ReconcilePayload::Positions(Vec::new())),
            reply(rid, 1, false, ReconcilePayload::Balance(Some(1.0))),
        ],
        other => vec![reply(rid, 0, false, empty_payload(other))],
    });
    let (session, _lane) = open(&peer, "tradehub-1");
    let recon = session.recon_client();
    let first_page_missing = recon.fetch_order_status_reports(0).expect_err("page 1 came first");
    assert!(first_page_missing.contains("page 1"), "{first_page_missing}");
    let repeated = recon.fetch_fill_reports(0).expect_err("page 0 twice");
    assert!(repeated.contains("page 0"), "{repeated}");
    let foreign = recon.fetch_position_status_reports().expect_err("a foreign kind");
    assert!(foreign.contains("Balance"), "{foreign}");
    assert!(!is_reconcile_not_ready(&foreign));
}

#[test]
fn progress_keeps_a_slow_fetch_alive_and_silence_is_called_dead() {
    // The fast timings' floor is 600 ms; each progress states 300 ms, so the bound is max(600, 900) ms.
    let peer = peer_answering(|rid, kind| match kind {
        ReconcileKind::Orders { .. } => vec![
            Step::Send(TradeDown::ReconcileProgress { rid, next_in_ms: 300 }),
            Step::Sleep(Duration::from_millis(500)),
            Step::Send(TradeDown::ReconcileProgress { rid, next_in_ms: 300 }),
            Step::Sleep(Duration::from_millis(500)),
            reply(rid, 0, false, ReconcilePayload::Orders(vec![order_report("slow")])),
        ],
        // One progress promising 100 ms, then silence: the bound is max(600 ms floor, 3 x 100 ms).
        _ => vec![
            Step::Send(TradeDown::ReconcileProgress { rid, next_in_ms: 100 }),
            Step::Sleep(Duration::from_secs(2)),
        ],
    });
    let (session, _lane) = open(&peer, "tradehub-1");
    let recon = session.recon_client();
    let started = std::time::Instant::now();
    let orders = recon.fetch_order_status_reports(0).expect("progress kept it alive");
    assert_eq!(orders.len(), 1);
    assert!(started.elapsed() > Duration::from_millis(900), "it ran longer than the bare floor");
    let started = std::time::Instant::now();
    let err = recon.fetch_fill_reports(0).expect_err("silence");
    assert!(err.contains("no reconcile frame"), "{err}");
    // The bound is 600 ms; an ignored bound waits `request_reply` (5 s in these timings). Between them: as much
    // slack as the box may need (a loaded run took 2.85 s over its bound elsewhere in this family).
    assert!(
        started.elapsed() < Duration::from_millis(4_500),
        "called dead by the bound, not by luck: {:?}",
        started.elapsed()
    );
}

#[test]
fn a_wire_id_prefetch_gates_the_reconcile_until_it_succeeds() {
    let healthy = Arc::new(AtomicBool::new(false));
    let h = Arc::clone(&healthy);
    let peer = TradePeer::start(TradePeerConfig::new().with_script(move |frame| match frame {
        TradeUp::WireIds { rid, order_client_ids } if h.load(Ordering::SeqCst) => {
            vec![Step::Send(TradeDown::WireIdsReply {
                rid: *rid,
                pairs: order_client_ids.iter().map(|c| (c.clone(), format!("0x{c}"))).collect(),
            })]
        }
        // A daemon whose prefetch fails: the reply never comes (the connection drops).
        TradeUp::WireIds { .. } => vec![Step::Close],
        TradeUp::Reconcile { rid, kind } => vec![reply(*rid, 0, false, empty_payload(kind))],
        _ => Vec::new(),
    }));
    let (session, _lane) = open(&peer, "tradehub-1");
    let prefetch = session.prefetch_wire_ids(&["hl-1".to_string()]);
    assert!(prefetch.is_err(), "the daemon failed the prefetch");

    let recon = session.recon_client();
    let gated = recon.fetch_order_status_reports(0).expect_err("the restore must not plan blind");
    assert!(is_reconcile_not_ready(&gated), "{gated}");
    assert!(gated.contains("wire-id"), "{gated}");
    assert!(
        peer.ups().iter().all(|f| !matches!(f, TradeUp::Reconcile { .. })),
        "no reconcile frame was sent while the translation map was owed"
    );

    healthy.store(true, Ordering::SeqCst);
    assert!(wait_until(WAIT, || session.link_state() == TradeLinkState::Up));
    // The reconcile client retries the owed prefetch itself, then fetches.
    assert_eq!(recon.fetch_order_status_reports(0), Ok(Vec::new()));
    let mut boxed: Box<dyn vike_execution::ExecutionClient + Send> =
        Box::new(session.execution_client());
    assert_eq!(boxed.wire_id_for("hl-1").as_deref(), Some("0xhl-1"));
    let _ = &mut boxed;
}

#[test]
fn a_wire_id_prefetch_beyond_one_frame_is_paged_and_nothing_is_cut() {
    // W13b finding 4: the old prefetch cut the set at 4096 by sort order, so a resting order past the cut
    // was a cache MISS (answered "the venue echoes our order client id") and a translating venue judged it gone.
    // Now the set goes in pages; the cache is filled only when every page was answered.
    let asked = Arc::new(std::sync::Mutex::new(Vec::<usize>::new()));
    let seen = Arc::clone(&asked);
    let fail_second_page = Arc::new(AtomicBool::new(true));
    let fail = Arc::clone(&fail_second_page);
    let peer = TradePeer::start(TradePeerConfig::new().with_script(move |frame| match frame {
        TradeUp::WireIds { rid, order_client_ids } => {
            let page = {
                let mut seen = seen.lock().expect("lock");
                seen.push(order_client_ids.len());
                seen.len()
            };
            if page >= 2 && fail.load(Ordering::SeqCst) {
                return vec![Step::Close]; // the daemon dies from the second page on
            }
            vec![Step::Send(TradeDown::WireIdsReply {
                rid: *rid,
                pairs: order_client_ids.iter().map(|c| (c.clone(), format!("0x{c}"))).collect(),
            })]
        }
        TradeUp::Reconcile { rid, kind } => vec![reply(*rid, 0, false, empty_payload(kind))],
        _ => Vec::new(),
    }));
    let (session, _lane) = open(&peer, "tradehub-1");
    let order_client_ids: Vec<String> = (0..9_000).map(|i| format!("hl-{i:05}")).collect();

    // One page fails: the whole prefetch stays owed, nothing is half-filled, the reconcile is NOT READY.
    assert!(session.prefetch_wire_ids(&order_client_ids).is_err(), "the second page was lost");
    let boxed: Box<dyn vike_execution::ExecutionClient + Send> =
        Box::new(session.execution_client());
    assert_eq!(boxed.wire_id_for("hl-00000"), None, "the first page alone fills nothing");
    let gated = session.recon_client().fetch_order_status_reports(0).expect_err("owed");
    assert!(is_reconcile_not_ready(&gated), "{gated}");

    // The daemon recovers; the owed prefetch is paid in full by the reconcile client's own retry.
    fail_second_page.store(false, Ordering::SeqCst);
    assert!(wait_until(WAIT, || session.link_state() == TradeLinkState::Up));
    assert_eq!(session.recon_client().fetch_order_status_reports(0), Ok(Vec::new()));
    let sizes = asked.lock().expect("lock").clone();
    assert!(sizes.iter().all(|n| *n <= 4096), "no frame over the page size: {sizes:?}");
    assert_eq!(
        sizes[sizes.len() - 3..].iter().sum::<usize>(),
        9_000,
        "the paid retry asked every order client id"
    );
    assert_eq!(boxed.wire_id_for("hl-00000").as_deref(), Some("0xhl-00000"));
    assert_eq!(boxed.wire_id_for("hl-08999").as_deref(), Some("0xhl-08999"), "the LAST page too");
}

#[test]
fn a_fill_report_crosses_row_for_row_and_bit_exact() {
    // The report types cross as the journal's own serde shape; one fill, bit-exact.
    let fill = FillReport {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        trade_id: "t-1".into(),
        venue_order_id: "v1".into(),
        order_client_id: Some("c1".into()),
        side: 1,
        last_qty: 0.1 + 0.2,
        last_price: 64_000.123_456_789,
        commission: 0.000_001,
        commission_asset: "USDT".into(),
        liquidity_side: vike_model::events::LiquiditySide::Maker,
        timestamp: 7,
    };
    let sent = fill.clone();
    let peer = peer_answering(move |rid, kind| match kind {
        ReconcileKind::Fills { .. } => {
            vec![reply(rid, 0, false, ReconcilePayload::Fills(vec![sent.clone()]))]
        }
        other => vec![reply(rid, 0, false, empty_payload(other))],
    });
    let (session, _lane) = open(&peer, "tradehub-1");
    let got = session.recon_client().fetch_fill_reports(0).expect("one fill");
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].last_qty.to_bits(), fill.last_qty.to_bits());
    assert_eq!(got[0].last_price.to_bits(), fill.last_price.to_bits());
    assert_eq!(got[0], fill);
}
