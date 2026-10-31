//! **One record per fact, through the REAL choke point** (`AccountSession::emit_event`, the one door every
//! producer of an order's events passes before the ring, the wire, the tradehub's journal and `recent`).
//!
//! The producers cannot see each other: the command loop answers a submit with `OrderAccepted` and a cancel with
//! `OrderCanceled { ts: 0, reason: "" }`, the venue's private stream reports its own row, and the history
//! replay (the gap-sentinel, five seconds after EVERY command and at every reconnect) lists the closed order
//! again. Measured on the v0.1.88 node (DEMO, 2026-10-25): one far order placed and cancelled left
//! `OrderAccepted` x2 and `OrderCanceled` x3 on Binance, OKX and Hyperliquid. Each test plays a duplicate
//! stream through a real `TradeHub` over a scripted adapter and reads the frames the lease writer would
//! write, so what it counts is what the tradehub would journal. The last section is the same for FILLS: one
//! record per EXECUTION (the exec id, the order and the numbers), dropped only when certainly the same.
#![cfg(feature = "trade")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_gatewayhub::budget::ledger::Budget;
use vike_gatewayhub::testkit::trade::{ScriptedAdapter, ScriptedTrade};
use vike_gatewayhub::trade::TradeConfig;
use vike_gatewayhub::trade::lease::{Flow, Lease, Next};
use vike_gatewayhub::trade::once::Repeats;
use vike_gatewayhub::trade::session::{OpenRequest, Opened, TradeHub};
use vike_gatewayhub_client::proto::{AdapterReply, TradeDown, TradeUp, WireCursor};
use vike_model::AccountTier;
use vike_model::events::{
    Event, FillEvent, OrderAccepted, OrderCancelRejected, OrderCanceled, OrderExpired, OrderFilled,
    OrderPartiallyFilled, OrderRejected, TradeId,
};

#[allow(dead_code, clippy::allow_attributes)]
#[path = "support/trade_rig.rs"]
mod trade_rig;

use trade_rig::{accepted, order, start_args, wait_until};

const WAIT: Duration = Duration::from_secs(5);

fn cfg() -> TradeConfig {
    TradeConfig {
        heartbeat: Duration::from_millis(100),
        read_floor: Duration::from_millis(300),
        ring_events: 64,
        dedup_coids: 64,
        command_queue: 64,
        control_queue: 64,
        event_channel: 64,
        max_leases: 8,
        orphan_ttl: Duration::from_secs(600),
        recon_progress: Duration::from_millis(100),
        recon_page_bytes: 4096,
        fence_drain: Duration::from_millis(500),
        account_ccy_retry_after: vike_gatewayhub::trade::ACCOUNT_CCY_RETRY_AFTER,
        state_dir: None,
        halt_path: None,
    }
}

fn hub(lane: &ScriptedTrade) -> Arc<TradeHub> {
    hub_with(lane, cfg())
}

fn hub_with(lane: &ScriptedTrade, config: TradeConfig) -> Arc<TradeHub> {
    TradeHub::new(Arc::new(lane.clone()), config, Arc::new(Budget::new()))
}

/// Read frames until the reply to the adapter start `rid` (a start answers after the events queued ahead of it).
fn await_reply(lease: &Lease, rid: u64) {
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        match lease.next(Duration::from_millis(100)) {
            Next::Frame(TradeDown::AdapterReply { rid: got, reply }) if got == rid => {
                assert!(matches!(*reply, AdapterReply::Live { .. }), "{reply:?}");
                return;
            }
            _ => {}
        }
    }
    panic!("no reply to adapter start {rid}");
}

fn request(account: Option<&str>, resume: Option<WireCursor>) -> OpenRequest {
    OpenRequest {
        client_id: "tradehub-1".to_string(),
        venue: "binance".to_string(),
        account: account.map(str::to_string),
        tier: AccountTier::Demo,
        resume,
        takeover: false,
        key_id: Some("nk-test".to_string()),
        closer: Arc::new(|| {}),
    }
}

/// Open a lease on `account` and start its adapter; the scripted adapter comes back with it.
fn up(
    hub: &Arc<TradeHub>,
    lane: &ScriptedTrade,
    account: Option<&str>,
) -> (Opened, ScriptedAdapter) {
    let opened = hub.open(request(account, None)).expect("the open is accepted");
    let mut start = start_args();
    start.rid = 7;
    start.route_key = "binance".into();
    opened.lease.handle(TradeUp::AdapterStart(start));
    await_reply(&opened.lease, 7);
    let adapter = lane.adapter("binance", account).expect("the adapter was started");
    (opened, adapter)
}

/// What the loop states for a venue that took the order: the venue's order id and the request's own time.
fn loop_accepted(coid: &str) -> Event {
    Event::OrderAccepted(OrderAccepted {
        client_order_id: coid.to_string(),
        venue_order_id: Some("v-1".into()),
        ts: 1_700_000_000_000,
    })
}

/// The stream's (and a replay's) second report of the same open order.
fn stream_accepted(coid: &str) -> Event {
    Event::OrderAccepted(OrderAccepted {
        client_order_id: coid.to_string(),
        venue_order_id: Some("v-1".into()),
        ts: 1_700_000_000_040,
    })
}

/// `cancel_event`'s answer: the REST cancel returned, no venue time, no reason.
fn loop_canceled(coid: &str) -> Event {
    Event::OrderCanceled(OrderCanceled {
        client_order_id: coid.to_string(),
        reason: "".into(),
        ts: 0,
    })
}

/// The venue's own cancel row, stream or replay: its time and its word.
fn venue_canceled(coid: &str) -> Event {
    Event::OrderCanceled(OrderCanceled {
        client_order_id: coid.to_string(),
        reason: "user".into(),
        ts: 1_700_000_000_050,
    })
}

fn venue_expired(coid: &str) -> Event {
    Event::OrderExpired(OrderExpired { client_order_id: coid.to_string(), ts: 1_700_000_000_060 })
}

/// One line per event: its class, its coid and (for a terminal) its time, so an assertion reads as the journal.
fn word(event: &Event) -> String {
    match event {
        Event::OrderAccepted(a) => format!("accepted {}", a.client_order_id),
        Event::OrderCanceled(c) => format!("canceled {} ts={}", c.client_order_id, c.ts),
        Event::OrderExpired(e) => format!("expired {} ts={}", e.client_order_id, e.ts),
        Event::OrderRejected(r) => format!("rejected {}", r.client_order_id),
        Event::OrderCancelRejected(r) => format!("cancel-rejected {}", r.client_order_id),
        Event::Fill(f) => format!("fill {}", execution_line(f)),
        Event::OrderPartiallyFilled(p) => format!("partial {}", execution_line(&p.fill)),
        Event::OrderFilled(w) => format!("filled {}", execution_line(&w.fill)),
        other => format!("{other:?}"),
    }
}

/// Every event frame the lease would write, in order, until the `marker` order's accept: `(seq, line)`.
/// The marker is the last thing a test emits, so by the time it arrives the forwarder thread has stamped (or
/// dropped) everything before it.
fn until_marker(lease: &Lease) -> Vec<(u64, String)> {
    let deadline = Instant::now() + WAIT;
    let mut out = Vec::new();
    while Instant::now() < deadline {
        if let Next::Frame(TradeDown::Event { seq, event, .. }) =
            lease.next(Duration::from_millis(100))
        {
            let line = word(&event);
            let last = line == "accepted marker";
            out.push((seq, line));
            if last {
                return out;
            }
        }
    }
    panic!("the marker never arrived; got {out:?}");
}

fn lines(frames: &[(u64, String)]) -> Vec<&str> {
    frames.iter().map(|(_, l)| l.as_str()).collect()
}

#[test]
fn one_cancelled_order_reaches_the_ring_as_one_accept_and_one_terminal_whoever_reports_it() {
    let lane = ScriptedTrade::new(&["binance"]);
    let hub = hub(&lane);
    let (opened, adapter) = up(&hub, &lane, None);
    // The v0.1.88 node's evidence, in the order the producers speak: the loop's accept, the stream's NEW row,
    // the loop's cancel answer, the stream's CANCELED row, the gap-sentinel's replay after the command, and the
    // replay after the NEXT command (the history window is re-listed every time).
    for e in [
        loop_accepted("c1"),
        stream_accepted("c1"),
        loop_canceled("c1"),
        venue_canceled("c1"),
        venue_canceled("c1"),
        stream_accepted("c1"),
        venue_canceled("c1"),
        accepted("marker"),
    ] {
        assert!(adapter.emit(e));
    }
    let got = until_marker(&opened.lease);
    assert_eq!(
        lines(&got),
        ["accepted c1", "canceled c1 ts=0", "accepted marker"],
        "one record per fact"
    );
    assert_eq!(
        got.iter().map(|(seq, _)| *seq).collect::<Vec<_>>(),
        [1, 2, 3],
        "a repeat takes no seq: the numbering has no hole"
    );
    assert_eq!(
        hub.session_repeats("binance", None),
        Some(Repeats { accepted: 2, terminal: 3, fill: 0 }),
        "and every repeat dropped is counted, by class (the retire line of the session reports it)"
    );
}

#[test]
fn the_stream_row_that_beats_the_rest_answer_is_the_one_record() {
    let lane = ScriptedTrade::new(&["binance"]);
    let hub = hub(&lane);
    let (opened, adapter) = up(&hub, &lane, None);
    for e in [
        loop_accepted("c1"),
        venue_canceled("c1"),
        loop_canceled("c1"),
        venue_canceled("c1"),
        accepted("marker"),
    ] {
        assert!(adapter.emit(e));
    }
    assert_eq!(
        lines(&until_marker(&opened.lease)),
        ["accepted c1", "canceled c1 ts=1700000000050", "accepted marker"]
    );
}

#[test]
fn a_lone_event_of_every_class_passes_untouched() {
    let lane = ScriptedTrade::new(&["binance"]);
    let hub = hub(&lane);
    let (opened, adapter) = up(&hub, &lane, None);
    let reject = Event::OrderRejected(OrderRejected {
        cause: None,
        client_order_id: "r1".to_string(),
        reason: "no".into(),
        ts: 1,
    });
    let advisory = Event::OrderCancelRejected(OrderCancelRejected {
        client_order_id: "c3".to_string(),
        reason: "late".into(),
        ts: 0,
    });
    for e in [
        // A lone accept.
        loop_accepted("c1"),
        // A terminal for an order this ring never saw accepted (a restart, a rebuilt adapter): not a repeat,
        // and stating it twice is the venue's doing, not this door's to judge.
        venue_canceled("c2"),
        venue_canceled("c2"),
        // Rejects and advisories are never judged.
        reject.clone(),
        reject,
        advisory.clone(),
        advisory,
        accepted("marker"),
    ] {
        assert!(adapter.emit(e));
    }
    assert_eq!(
        lines(&until_marker(&opened.lease)),
        [
            "accepted c1",
            "canceled c2 ts=1700000000050",
            "canceled c2 ts=1700000000050",
            "rejected r1",
            "rejected r1",
            "cancel-rejected c3",
            "cancel-rejected c3",
            "accepted marker",
        ]
    );
}

#[test]
fn the_same_coid_on_another_account_is_not_a_repeat() {
    let lane =
        ScriptedTrade::new(&["binance"]).with_tier("binance", Some("ALT"), Ok(AccountTier::Demo));
    let hub = hub(&lane);
    let (default, default_adapter) = up(&hub, &lane, None);
    let (alt, alt_adapter) = up(&hub, &lane, Some("ALT"));
    // The default account's order ends first; the other account then reports a coid of the same spelling.
    for e in [loop_accepted("c1"), loop_canceled("c1"), venue_canceled("c1"), accepted("marker")] {
        assert!(default_adapter.emit(e));
    }
    assert_eq!(
        lines(&until_marker(&default.lease)),
        ["accepted c1", "canceled c1 ts=0", "accepted marker"]
    );
    for e in [loop_accepted("c1"), loop_canceled("c1"), venue_canceled("c1"), accepted("marker")] {
        assert!(alt_adapter.emit(e));
    }
    assert_eq!(
        lines(&until_marker(&alt.lease)),
        ["accepted c1", "canceled c1 ts=0", "accepted marker"],
        "the other account's order is its own: nothing of it is dropped"
    );
}

#[test]
fn a_venue_row_of_another_word_after_the_loops_cancel_is_a_disagreement_and_is_kept_once() {
    let lane = ScriptedTrade::new(&["binance"]);
    let hub = hub(&lane);
    let (opened, adapter) = up(&hub, &lane, None);
    // The loop said Canceled before the venue spoke; the venue then says Expired (the order's final state
    // differs), once from the stream and again from the replay. The venue has spoken: nothing more passes.
    for e in [
        loop_accepted("c1"),
        loop_canceled("c1"),
        venue_expired("c1"),
        venue_expired("c1"),
        venue_canceled("c1"),
        accepted("marker"),
    ] {
        assert!(adapter.emit(e));
    }
    assert_eq!(
        lines(&until_marker(&opened.lease)),
        ["accepted c1", "canceled c1 ts=0", "expired c1 ts=1700000000060", "accepted marker"]
    );
}

#[test]
fn a_client_that_comes_back_with_its_cursor_does_not_make_a_replayed_row_look_new() {
    let lane = ScriptedTrade::new(&["binance"]);
    let hub = hub(&lane);
    let (first, adapter) = up(&hub, &lane, None);
    let epoch = first.reply.gw_epoch;
    for e in [loop_accepted("c1"), loop_canceled("c1"), accepted("marker-1")] {
        assert!(adapter.emit(e));
    }
    // Wait for the forwarder to stamp all three.
    let deadline = Instant::now() + WAIT;
    while hub.session_status("binance", None).is_none_or(|s| s.last_seq < 3) {
        assert!(Instant::now() < deadline, "the ring never reached 3");
        std::thread::sleep(Duration::from_millis(5));
    }
    first.lease.close();
    first.lease.finish();
    let second = hub
        .open(request(None, Some(WireCursor { gw_epoch: epoch, seq: 3, ts_ms: 0 })))
        .expect("accepted");
    // The reconnect's gap-sentinel re-lists the closed order.
    for e in [stream_accepted("c1"), venue_canceled("c1"), accepted("marker")] {
        assert!(adapter.emit(e));
    }
    assert_eq!(lines(&until_marker(&second.lease)), ["accepted marker"]);
}

#[test]
fn a_client_that_cannot_be_replayed_what_it_missed_is_told_every_row_again() {
    // It reconciles and may leave an order UNKNOWN; the venue's own row is what resolves that, so no row may
    // be taken for a repeat of one this client never got.
    let lane = ScriptedTrade::new(&["binance"]);
    let hub = hub(&lane);
    let (first, adapter) = up(&hub, &lane, None);
    let epoch = first.reply.gw_epoch;
    for e in [loop_accepted("c1"), loop_canceled("c1")] {
        assert!(adapter.emit(e));
    }
    let deadline = Instant::now() + WAIT;
    while hub.session_status("binance", None).is_none_or(|s| s.last_seq < 2) {
        assert!(Instant::now() < deadline, "the ring never reached 2");
        std::thread::sleep(Duration::from_millis(5));
    }
    first.lease.close();
    first.lease.finish();
    // A cursor of another epoch is no cursor (`ReplayUnavailable`).
    let second = hub
        .open(request(None, Some(WireCursor { gw_epoch: epoch ^ 1, seq: 2, ts_ms: 0 })))
        .expect("accepted");
    for e in [venue_canceled("c1"), accepted("marker")] {
        assert!(adapter.emit(e));
    }
    assert_eq!(
        lines(&until_marker(&second.lease)),
        ["canceled c1 ts=1700000000050", "accepted marker"],
        "the venue's own row reaches the client that cannot be replayed"
    );
}

/// Read exactly `n` event frames (skipping acks and heartbeats): `(seq, line)`.
fn events(lease: &Lease, n: usize) -> Vec<(u64, String)> {
    let deadline = Instant::now() + WAIT;
    let mut out = Vec::new();
    while out.len() < n && Instant::now() < deadline {
        if let Next::Frame(TradeDown::Event { seq, event, .. }) =
            lease.next(Duration::from_millis(100))
        {
            out.push((seq, word(&event)));
        }
    }
    assert_eq!(out.len(), n, "wanted {n} events, got {out:?}");
    out
}

#[test]
fn a_coid_submitted_again_after_the_adapter_restarted_is_a_new_life_and_its_accept_and_terminal_arrive()
 {
    // The Submit dedup set is per ADAPTER: a changed start (or a panic rebuild) empties it, so `submit --coid c1`
    // is admitted again while the first-wins memory still holds the first life of `c1` as ended. Taken for the
    // first life's repeats, the second life's accept and cancel would be dropped, the node's order would stick
    // at Initialized and the stuck-order watchdog could reject a live resting order.
    let lane = ScriptedTrade::new(&["binance"]);
    let hub = hub(&lane);
    let (opened, first) = up(&hub, &lane, None);
    assert_eq!(
        opened.lease.handle(TradeUp::Submit { cseq: 1, order: order("c1") }),
        Flow::Continue
    );
    assert!(wait_until(WAIT, || first.submitted().len() == 1), "the first life's submit was sent");
    for e in [loop_accepted("c1"), loop_canceled("c1")] {
        assert!(first.emit(e));
    }
    assert_eq!(
        events(&opened.lease, 2).into_iter().map(|(_, l)| l).collect::<Vec<_>>(),
        ["accepted c1", "canceled c1 ts=0"],
        "the first life"
    );
    // A different start tears the adapter down and starts another: a new dispatch thread, an empty dedup set.
    let mut other = start_args();
    other.rid = 8;
    other.symbol = "ETHUSDT".into();
    other.route_key = "binance".into();
    opened.lease.handle(TradeUp::AdapterStart(other));
    await_reply(&opened.lease, 8);
    let second = lane.adapter("binance", None).expect("the restarted adapter");
    assert_eq!(
        opened.lease.handle(TradeUp::Submit { cseq: 2, order: order("c1") }),
        Flow::Continue
    );
    assert!(wait_until(WAIT, || second.submitted().len() == 1), "the same coid was admitted again");
    for e in [loop_accepted("c1"), loop_canceled("c1"), accepted("marker")] {
        assert!(second.emit(e));
    }
    assert_eq!(
        lines(&until_marker(&opened.lease)),
        ["accepted c1", "canceled c1 ts=0", "accepted marker"],
        "the second life of the coid arrives whole"
    );
}

#[test]
fn an_accept_that_names_no_venue_order_is_not_remembered_so_the_venues_stamped_cancel_still_arrives()
 {
    // The resolver's optimistic answer to an UNKNOWN submit names no venue order. The client keeps such an order
    // UNKNOWN and states the loop's synthetic cancel of it as an advisory; the venue's own stamped cancel is what
    // ends it. Remembering the bare accept made that stamped cancel read as a same-word repeat of the synthetic one.
    let lane = ScriptedTrade::new(&["binance"]);
    let hub = hub(&lane);
    let (opened, adapter) = up(&hub, &lane, None);
    for e in [accepted("c1"), loop_canceled("c1"), venue_canceled("c1"), accepted("marker")] {
        assert!(adapter.emit(e));
    }
    assert_eq!(
        lines(&until_marker(&opened.lease)),
        ["accepted c1", "canceled c1 ts=0", "canceled c1 ts=1700000000050", "accepted marker"]
    );
}

#[test]
fn an_order_older_than_the_memory_is_forgotten_and_its_replayed_row_passes_again() {
    // The horizon: the memory holds `dedup_coids` orders, first in first out.
    let lane = ScriptedTrade::new(&["binance"]);
    let hub = hub_with(&lane, TradeConfig { dedup_coids: 3, ..cfg() });
    let (opened, adapter) = up(&hub, &lane, None);
    for n in 1..=4 {
        assert!(adapter.emit(loop_accepted(&format!("c{n}"))));
        assert!(adapter.emit(loop_canceled(&format!("c{n}"))));
    }
    // The replay re-lists c4 (still remembered: dropped) and c1 (pushed out by c4: a first report again).
    for e in [venue_canceled("c4"), venue_canceled("c1"), accepted("marker")] {
        assert!(adapter.emit(e));
    }
    assert_eq!(
        lines(&until_marker(&opened.lease)),
        [
            "accepted c1",
            "canceled c1 ts=0",
            "accepted c2",
            "canceled c2 ts=0",
            "accepted c3",
            "canceled c3 ts=0",
            "accepted c4",
            "canceled c4 ts=0",
            "canceled c1 ts=1700000000050",
            "accepted marker",
        ]
    );
}

// ---- FILLS: one record per EXECUTION ---------------------------------------------------------------------------
//
// The evidence (v0.1.88 node, 2026-10-25): `FillEvent 25115f7a14 0.001@80512` twice in a row in `recent`. The engine
// drops the second by its exec id, so the books were right; the journal and the ring kept both. A fill is money, so
// the door drops one only when it is CERTAINLY the same execution: the same exec id, the same order and the same
// quantity and price (the fee and the time are restated by a replay and are not part of the identity).

const QTY: f64 = 0.001;
const PX: f64 = 80_512.0;

/// One execution as a venue states it.
fn execution(coid: &str, exec: &str, qty: f64, px: f64) -> FillEvent {
    FillEvent {
        trade_id: TradeId::new(exec).expect("a non-empty exec id"),
        client_order_id: coid.to_string(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: qty,
        last_px: px,
        commission: 0.01,
        commission_asset: "USDT".into(),
        liquidity_side: Default::default(),
        ts: 1_700_000_000_100,
        mark_price: None,
        position_side: Default::default(),
    }
}

fn execution_line(f: &FillEvent) -> String {
    format!("{} {} {}@{}", f.client_order_id, f.trade_id, f.last_qty, f.last_px)
}

/// The bare execution report (`Event::Fill`).
fn fill(coid: &str, exec: &str, qty: f64, px: f64) -> Event {
    Event::Fill(execution(coid, exec, qty, px))
}

/// The wrap of a fill that does not finish the order.
fn partial(coid: &str, exec: &str, qty: f64, px: f64) -> Event {
    Event::OrderPartiallyFilled(OrderPartiallyFilled {
        client_order_id: coid.to_string(),
        fill: execution(coid, exec, qty, px),
        ts: 1_700_000_000_100,
    })
}

/// The wrap of the fill that finishes the order.
fn filled(coid: &str, exec: &str, qty: f64, px: f64) -> Event {
    Event::OrderFilled(OrderFilled {
        client_order_id: coid.to_string(),
        fill: execution(coid, exec, qty, px),
        ts: 1_700_000_000_100,
    })
}

/// A replay's row of the same execution: the fee and the time are restated (the history row carries its own).
fn replayed(event: &Event) -> Event {
    let restate = |f: &FillEvent| FillEvent { commission: 0.0, ts: f.ts + 5_000, ..f.clone() };
    match event {
        Event::Fill(f) => Event::Fill(restate(f)),
        Event::OrderPartiallyFilled(p) => Event::OrderPartiallyFilled(OrderPartiallyFilled {
            fill: restate(&p.fill),
            ts: p.ts + 5_000,
            ..p.clone()
        }),
        Event::OrderFilled(w) => Event::OrderFilled(OrderFilled {
            fill: restate(&w.fill),
            ts: w.ts + 5_000,
            ..w.clone()
        }),
        other => other.clone(),
    }
}

/// Emit `events` then the marker through `adapter`, and read what the lease would write up to the marker.
fn through(
    lease: &Lease,
    adapter: &ScriptedAdapter,
    events: impl IntoIterator<Item = Event>,
) -> Vec<(u64, String)> {
    for e in events {
        assert!(adapter.emit(e));
    }
    assert!(adapter.emit(accepted("marker")));
    until_marker(lease)
}

#[test]
fn one_execution_reaches_the_ring_once_whoever_reports_it() {
    let lane = ScriptedTrade::new(&["binance"]);
    let hub = hub(&lane);
    let (opened, adapter) = up(&hub, &lane, None);
    let (bare, wrap) = (fill("c1", "t1", QTY, PX), filled("c1", "t1", QTY, PX));
    let got = through(
        &opened.lease,
        &adapter,
        [
            loop_accepted("c1"),
            // The stream's own report of the execution ...
            bare.clone(),
            wrap.clone(),
            // ... the gap-sentinel's replay after the command, with the fee and the time the history row restates ...
            replayed(&bare),
            replayed(&wrap),
            // ... and the replay after the NEXT command (the window is re-listed every time).
            bare,
            wrap,
        ],
    );
    assert_eq!(
        lines(&got),
        ["accepted c1", "fill c1 t1 0.001@80512", "filled c1 t1 0.001@80512", "accepted marker"],
        "one record per execution"
    );
    assert_eq!(
        got.iter().map(|(seq, _)| *seq).collect::<Vec<_>>(),
        [1, 2, 3, 4],
        "a repeat takes no seq: the numbering has no hole"
    );
    assert_eq!(
        hub.session_repeats("binance", None),
        Some(Repeats { accepted: 0, terminal: 0, fill: 4 }),
        "and every repeat dropped is counted (the retire line of the session reports it)"
    );
}

#[test]
fn two_partial_fills_of_one_order_with_different_exec_ids_both_pass() {
    let lane = ScriptedTrade::new(&["binance"]);
    let hub = hub(&lane);
    let (opened, adapter) = up(&hub, &lane, None);
    let got = through(
        &opened.lease,
        &adapter,
        [
            loop_accepted("c1"),
            fill("c1", "t1", 0.4, PX),
            partial("c1", "t1", 0.4, PX),
            fill("c1", "t2", 0.6, PX),
            filled("c1", "t2", 0.6, PX),
            // A replay of the first one only: the second one must not hide behind it.
            fill("c1", "t1", 0.4, PX),
            partial("c1", "t1", 0.4, PX),
        ],
    );
    assert_eq!(
        lines(&got),
        [
            "accepted c1",
            "fill c1 t1 0.4@80512",
            "partial c1 t1 0.4@80512",
            "fill c1 t2 0.6@80512",
            "filled c1 t2 0.6@80512",
            "accepted marker"
        ]
    );
}

#[test]
fn two_different_executions_with_the_same_numbers_and_the_same_order_both_pass() {
    // The same quantity at the same price on the same order, twice: two executions, told apart by their exec ids.
    let lane = ScriptedTrade::new(&["binance"]);
    let hub = hub(&lane);
    let (opened, adapter) = up(&hub, &lane, None);
    let got = through(
        &opened.lease,
        &adapter,
        [
            loop_accepted("c1"),
            fill("c1", "t1", QTY, PX),
            fill("c1", "t2", QTY, PX),
            // Only the replay of the second is a repeat.
            fill("c1", "t2", QTY, PX),
        ],
    );
    assert_eq!(
        lines(&got),
        ["accepted c1", "fill c1 t1 0.001@80512", "fill c1 t2 0.001@80512", "accepted marker"]
    );
}

#[test]
fn an_exec_id_that_comes_back_with_other_numbers_or_for_another_order_is_not_a_repeat() {
    // Not CERTAINLY the same execution, so it passes; and it does not take the first one's place in the memory.
    let lane = ScriptedTrade::new(&["binance"]);
    let hub = hub(&lane);
    let (opened, adapter) = up(&hub, &lane, None);
    let got = through(
        &opened.lease,
        &adapter,
        [
            loop_accepted("c1"),
            loop_accepted("c2"),
            fill("c1", "t1", QTY, PX),
            fill("c1", "t1", 0.002, PX),
            fill("c1", "t1", QTY, 80_513.0),
            fill("c2", "t1", QTY, PX),
            // The first one's own replay is still a repeat.
            fill("c1", "t1", QTY, PX),
        ],
    );
    assert_eq!(
        lines(&got),
        [
            "accepted c1",
            "accepted c2",
            "fill c1 t1 0.001@80512",
            "fill c1 t1 0.002@80512",
            "fill c1 t1 0.001@80513",
            "fill c2 t1 0.001@80512",
            "accepted marker"
        ]
    );
}

#[test]
fn a_fill_of_an_order_this_ring_never_saw_accepted_is_never_judged() {
    // A restart, a rebuilt adapter, a venue that fills with no accept, an accept that names no venue order: the
    // fills of such an order pass however often they come (and nothing of them is remembered).
    let lane = ScriptedTrade::new(&["binance"]);
    let hub = hub(&lane);
    let (opened, adapter) = up(&hub, &lane, None);
    let got = through(
        &opened.lease,
        &adapter,
        [
            accepted("u1"),
            fill("u1", "t1", QTY, PX),
            fill("u1", "t1", QTY, PX),
            fill("ghost", "t2", QTY, PX),
            fill("ghost", "t2", QTY, PX),
            filled("ghost", "t2", QTY, PX),
            filled("ghost", "t2", QTY, PX),
        ],
    );
    assert_eq!(
        lines(&got),
        [
            "accepted u1",
            "fill u1 t1 0.001@80512",
            "fill u1 t1 0.001@80512",
            "fill ghost t2 0.001@80512",
            "fill ghost t2 0.001@80512",
            "filled ghost t2 0.001@80512",
            "filled ghost t2 0.001@80512",
            "accepted marker"
        ]
    );
    assert_eq!(hub.session_repeats("binance", None), Some(Repeats::default()));
}

#[test]
fn a_wrap_that_overtook_its_accept_is_not_remembered_so_the_redelivery_after_it_still_arrives() {
    // The engine refuses a wrap that finds its order still Submitted WITHOUT burning the exec id; the sentinel's
    // redelivery after the accept is what completes the order (`known_gap_a_fill_that_finds_the_order_submitted_is_
    // refused_and_not_kept`, vike-exec). Dropping that redelivery would leave the order Accepted for ever.
    let lane = ScriptedTrade::new(&["binance"]);
    let hub = hub(&lane);
    let (opened, adapter) = up(&hub, &lane, None);
    let got = through(
        &opened.lease,
        &adapter,
        [
            filled("c1", "t1", QTY, PX),
            loop_accepted("c1"),
            // The redelivery: the engine takes it now.
            filled("c1", "t1", QTY, PX),
            // And the one after that is a repeat.
            filled("c1", "t1", QTY, PX),
        ],
    );
    assert_eq!(
        lines(&got),
        ["filled c1 t1 0.001@80512", "accepted c1", "filled c1 t1 0.001@80512", "accepted marker"]
    );
}

#[test]
fn a_non_finite_commission_is_not_remembered_so_the_corrected_retransmission_arrives() {
    // The engine refuses it BEFORE it burns the exec id, so a well-formed retransmission must still fold.
    let lane = ScriptedTrade::new(&["binance"]);
    let hub = hub(&lane);
    let (opened, adapter) = up(&hub, &lane, None);
    let poisoned =
        Event::Fill(FillEvent { commission: f64::NAN, ..execution("c1", "t1", QTY, PX) });
    let got = through(
        &opened.lease,
        &adapter,
        [loop_accepted("c1"), poisoned, fill("c1", "t1", QTY, PX), fill("c1", "t1", QTY, PX)],
    );
    assert_eq!(
        lines(&got),
        ["accepted c1", "fill c1 t1 0.001@80512", "fill c1 t1 0.001@80512", "accepted marker"]
    );
}

#[test]
fn an_empty_exec_id_cannot_exist_so_no_fill_is_taken_for_a_repeat_of_nothing() {
    // The type forbids it at every door: the constructor, and the wire (a venue frame, a journal row).
    assert!(TradeId::new("").is_err());
    let mut json = serde_json::to_value(fill("c1", "t1", QTY, PX)).expect("an event serialises");
    json["trade_id"] = serde_json::Value::String(String::new());
    assert!(
        serde_json::from_value::<Event>(json).is_err(),
        "an empty exec id does not deserialise"
    );
}

#[test]
fn a_fill_that_names_no_order_is_never_judged() {
    // A foreign execution (no coid of ours) is the reconcile's business, not this door's.
    let lane = ScriptedTrade::new(&["binance"]);
    let hub = hub(&lane);
    let (opened, adapter) = up(&hub, &lane, None);
    let got = through(
        &opened.lease,
        &adapter,
        [
            fill("", "t1", QTY, PX),
            fill("", "t1", QTY, PX),
            filled("", "t1", QTY, PX),
            filled("", "t1", QTY, PX),
        ],
    );
    assert_eq!(
        lines(&got),
        [
            "fill  t1 0.001@80512",
            "fill  t1 0.001@80512",
            "filled  t1 0.001@80512",
            "filled  t1 0.001@80512",
            "accepted marker"
        ]
    );
}

#[test]
fn the_same_exec_id_on_another_account_is_not_a_repeat() {
    let lane =
        ScriptedTrade::new(&["binance"]).with_tier("binance", Some("ALT"), Ok(AccountTier::Demo));
    let hub = hub(&lane);
    let (default, default_adapter) = up(&hub, &lane, None);
    let (alt, alt_adapter) = up(&hub, &lane, Some("ALT"));
    let one = [loop_accepted("c1"), fill("c1", "t1", QTY, PX), fill("c1", "t1", QTY, PX)];
    assert_eq!(
        lines(&through(&default.lease, &default_adapter, one.clone())),
        ["accepted c1", "fill c1 t1 0.001@80512", "accepted marker"]
    );
    // Venue ids are per account (and on some venues per symbol): the other account's execution is its own.
    assert_eq!(
        lines(&through(&alt.lease, &alt_adapter, one)),
        ["accepted c1", "fill c1 t1 0.001@80512", "accepted marker"],
        "the other account's execution is its own: nothing of it is dropped"
    );
}

#[test]
fn a_client_that_comes_back_with_its_cursor_does_not_make_a_replayed_fill_look_new() {
    let lane = ScriptedTrade::new(&["binance"]);
    let hub = hub(&lane);
    let (first, adapter) = up(&hub, &lane, None);
    let epoch = first.reply.gw_epoch;
    for e in [
        loop_accepted("c1"),
        fill("c1", "t1", QTY, PX),
        filled("c1", "t1", QTY, PX),
        accepted("marker-1"),
    ] {
        assert!(adapter.emit(e));
    }
    let deadline = Instant::now() + WAIT;
    while hub.session_status("binance", None).is_none_or(|s| s.last_seq < 4) {
        assert!(Instant::now() < deadline, "the ring never reached 4");
        std::thread::sleep(Duration::from_millis(5));
    }
    first.lease.close();
    first.lease.finish();
    let second = hub
        .open(request(None, Some(WireCursor { gw_epoch: epoch, seq: 4, ts_ms: 0 })))
        .expect("accepted");
    // The reconnect's gap-sentinel re-lists the closed order.
    let got = through(
        &second.lease,
        &adapter,
        [replayed(&fill("c1", "t1", QTY, PX)), replayed(&filled("c1", "t1", QTY, PX))],
    );
    assert_eq!(lines(&got), ["accepted marker"], "the client already holds both");
}

#[test]
fn a_client_that_cannot_be_replayed_what_it_missed_is_told_the_execution_again() {
    // Permissive by design: the memory is cleared with the client's view of the ring, so the venue's own row
    // reaches a client that never got the first report.
    let lane = ScriptedTrade::new(&["binance"]);
    let hub = hub(&lane);
    let (first, adapter) = up(&hub, &lane, None);
    let epoch = first.reply.gw_epoch;
    for e in [loop_accepted("c1"), fill("c1", "t1", QTY, PX)] {
        assert!(adapter.emit(e));
    }
    let deadline = Instant::now() + WAIT;
    while hub.session_status("binance", None).is_none_or(|s| s.last_seq < 2) {
        assert!(Instant::now() < deadline, "the ring never reached 2");
        std::thread::sleep(Duration::from_millis(5));
    }
    first.lease.close();
    first.lease.finish();
    // A cursor of another epoch is no cursor (`ReplayUnavailable`).
    let second = hub
        .open(request(None, Some(WireCursor { gw_epoch: epoch ^ 1, seq: 2, ts_ms: 0 })))
        .expect("accepted");
    let got = through(&second.lease, &adapter, [replayed(&fill("c1", "t1", QTY, PX))]);
    assert_eq!(
        lines(&got),
        ["fill c1 t1 0.001@80512", "accepted marker"],
        "the venue's row reaches the client that never got the first report"
    );
    // Cleared means the order is unwitnessed again: its fills pass until an accept makes it known, as before.
    let got = through(&second.lease, &adapter, [replayed(&fill("c1", "t1", QTY, PX))]);
    assert_eq!(lines(&got), ["fill c1 t1 0.001@80512", "accepted marker"]);
}

#[test]
fn a_stalled_lease_whose_unread_fill_the_ring_evicted_gets_the_venues_restatement_of_it() {
    // The clear must not wait for the writer's next pull: the venue restates the fill that fell out unread BEFORE
    // the writer reads again, and the memory (which still holds the original) would drop the restatement. The
    // lease here never reads at all.
    let lane = ScriptedTrade::new(&["binance"]);
    let hub = hub_with(&lane, TradeConfig { ring_events: 4, ..cfg() });
    let (_opened, adapter) = up(&hub, &lane, None);
    for e in [loop_accepted("c1"), fill("c1", "t1", QTY, PX)] {
        assert!(adapter.emit(e));
    }
    for n in 1..=6 {
        assert!(adapter.emit(loop_accepted(&format!("x{n}"))));
    }
    let deadline = Instant::now() + WAIT;
    while hub.session_status("binance", None).is_none_or(|s| s.last_seq < 8) {
        assert!(Instant::now() < deadline, "the ring never reached 8");
        std::thread::sleep(Duration::from_millis(5));
    }
    // The venue restates the execution (the gap-sentinel's replay): it must take the next seq.
    assert!(adapter.emit(replayed(&fill("c1", "t1", QTY, PX))));
    let deadline = Instant::now() + WAIT;
    while hub.session_status("binance", None).is_none_or(|s| s.last_seq < 9) {
        assert!(
            Instant::now() < deadline,
            "the restatement was dropped as a repeat of a fill the lease never read"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(hub.session_repeats("binance", None).map(|r| r.fill), Some(0));
}

#[test]
fn a_lease_the_ring_overran_forgets_what_it_held_so_the_fill_that_fell_out_unread_comes_back() {
    // A slow attached lease whose unread events the ring evicts is told to reconcile (`ReplayUnavailable`), the
    // way an open without a replayable cursor is; the venue's own redelivery of the fill that fell out is what
    // recovers it, so the memory must let that redelivery through.
    let lane = ScriptedTrade::new(&["binance"]);
    let hub = hub_with(&lane, TradeConfig { ring_events: 4, ..cfg() });
    let (opened, adapter) = up(&hub, &lane, None);
    for e in [loop_accepted("c1"), fill("c1", "t1", QTY, PX)] {
        assert!(adapter.emit(e));
    }
    // Six more events push the first two out of the 4-event ring before the lease reads any of them.
    for n in 1..=6 {
        assert!(adapter.emit(loop_accepted(&format!("x{n}"))));
    }
    let deadline = Instant::now() + WAIT;
    while hub.session_status("binance", None).is_none_or(|s| s.last_seq < 8) {
        assert!(Instant::now() < deadline, "the ring never reached 8");
        std::thread::sleep(Duration::from_millis(5));
    }
    // The lease reads: the gap is found and the memory is cleared with it.
    let deadline = Instant::now() + WAIT;
    loop {
        if let Next::Frame(TradeDown::ReplayUnavailable { .. }) =
            opened.lease.next(Duration::from_millis(100))
        {
            break;
        }
        assert!(Instant::now() < deadline, "no ReplayUnavailable");
    }
    // The redelivery of the fill that fell out unread is a first report, not a repeat.
    assert!(adapter.emit(loop_accepted("c1")));
    let got = through(&opened.lease, &adapter, [fill("c1", "t1", QTY, PX)]);
    assert!(
        lines(&got).contains(&"fill c1 t1 0.001@80512"),
        "the redelivered fill reached the lease: {got:?}"
    );
}

#[test]
fn a_coid_submitted_again_is_a_new_life_and_the_marker_fill_a_venue_mints_from_the_coid_arrives_again()
 {
    // ctrader mints the exec id of its terminal marker (`CTRADER-FSMTERM-<coid>`, quantity and price 0) from the
    // coid alone, ig its fallback id the same way: the second life of a coid states the SAME id. The Submit the
    // dispatch thread admits starts a new life, so the marker is a first report again.
    let lane = ScriptedTrade::new(&["binance"]);
    let hub = hub(&lane);
    let (opened, first) = up(&hub, &lane, None);
    assert_eq!(
        opened.lease.handle(TradeUp::Submit { cseq: 1, order: order("c1") }),
        Flow::Continue
    );
    assert!(wait_until(WAIT, || first.submitted().len() == 1), "the first life's submit was sent");
    for e in [loop_accepted("c1"), filled("c1", "FSMTERM-c1", 0.0, 0.0)] {
        assert!(first.emit(e));
    }
    assert_eq!(
        events(&opened.lease, 2).into_iter().map(|(_, l)| l).collect::<Vec<_>>(),
        ["accepted c1", "filled c1 FSMTERM-c1 0@0"],
        "the first life"
    );
    let mut other = start_args();
    other.rid = 8;
    other.symbol = "ETHUSDT".into();
    other.route_key = "binance".into();
    opened.lease.handle(TradeUp::AdapterStart(other));
    await_reply(&opened.lease, 8);
    let second = lane.adapter("binance", None).expect("the restarted adapter");
    assert_eq!(
        opened.lease.handle(TradeUp::Submit { cseq: 2, order: order("c1") }),
        Flow::Continue
    );
    assert!(wait_until(WAIT, || second.submitted().len() == 1), "the same coid was admitted again");
    let got = through(
        &opened.lease,
        &second,
        [
            loop_accepted("c1"),
            filled("c1", "FSMTERM-c1", 0.0, 0.0),
            // Its own repeat is still a repeat.
            filled("c1", "FSMTERM-c1", 0.0, 0.0),
        ],
    );
    assert_eq!(
        lines(&got),
        ["accepted c1", "filled c1 FSMTERM-c1 0@0", "accepted marker"],
        "the second life arrives whole"
    );
}

#[test]
fn an_execution_older_than_the_memory_is_forgotten_and_its_replayed_row_passes_again() {
    // The horizon: the memory holds `dedup_coids` executions, first in first out.
    let lane = ScriptedTrade::new(&["binance"]);
    let hub = hub_with(&lane, TradeConfig { dedup_coids: 3, ..cfg() });
    let (opened, adapter) = up(&hub, &lane, None);
    // One order with four executions of the same size.
    let mut script = vec![loop_accepted("c1")];
    for n in 1..=4 {
        script.push(fill("c1", &format!("t{n}"), QTY, PX));
    }
    // The replay re-lists t4 (still remembered: dropped) and t1 (pushed out by t4: a first report again).
    script.push(fill("c1", "t4", QTY, PX));
    script.push(fill("c1", "t1", QTY, PX));
    assert_eq!(
        lines(&through(&opened.lease, &adapter, script)),
        [
            "accepted c1",
            "fill c1 t1 0.001@80512",
            "fill c1 t2 0.001@80512",
            "fill c1 t3 0.001@80512",
            "fill c1 t4 0.001@80512",
            "fill c1 t1 0.001@80512",
            "accepted marker",
        ]
    );
}
