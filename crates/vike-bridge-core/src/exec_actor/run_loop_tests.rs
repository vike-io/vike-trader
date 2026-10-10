//! `run_loop`'s command→event wiring over a mock `VenueRest` (NO network, NO real orders):
//! Submit→`submit_order` (forwarding its events), Cancel→`cancel_order` (failure → non-terminal
//! `OrderCancelRejected`), Modify→`modify_order` (forwarding a native amend's events; the
//! `VenueRest` default no-op forwards nothing), the gap-sentinel resync after an idle submit, and
//! the restart law's history floor. Venue-specific behaviour is tested in each venue crate.
use super::*;
use std::sync::Mutex;
use vike_exec::Ingest;
use vike_exec::event_channel;
use vike_model::events::{OrderAccepted, OrderModified, OrderSubmitted};

#[derive(Default)]
struct MockRest {
    submitted: Mutex<Vec<String>>,
    canceled: Mutex<Vec<String>>,
    modified: Mutex<Vec<(String, Option<f64>)>>,
    cancel_fails: bool,
    /// When true, `modify_order` returns a native `OrderModified` (the native-amend capability);
    /// when false it is the `VenueRest` default no-op (`[]`).
    native_modify: bool,
}

impl VenueRest for MockRest {
    fn submit_order(&self, request: &OrderRequest) -> Vec<Event> {
        self.submitted.lock().unwrap().push(request.client_order_id.clone());
        vec![
            Event::OrderSubmitted(OrderSubmitted {
                client_order_id: request.client_order_id.clone(),
                ts: 0,
            }),
            Event::OrderAccepted(OrderAccepted {
                client_order_id: request.client_order_id.clone(),
                venue_order_id: Some("v-1".to_string().into()),
                ts: 0,
            }),
        ]
    }
    fn cancel_order(&self, client_order_id: &str) -> Result<(), VenueApiError> {
        self.canceled.lock().unwrap().push(client_order_id.to_string());
        if self.cancel_fails {
            Err(VenueApiError { code: 1, msg: "boom".to_string() })
        } else {
            Ok(())
        }
    }
    fn modify_order(
        &self,
        order: &OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
    ) -> Vec<Event> {
        self.modified.lock().unwrap().push((order.client_order_id.clone(), new_price));
        if self.native_modify {
            vec![Event::OrderModified(OrderModified {
                client_order_id: order.client_order_id.clone(),
                venue_order_id: Some("v-1".to_string().into()),
                new_qty,
                new_price,
                ts: 0,
            })]
        } else {
            Vec::new()
        }
    }
}

fn req(coid: &str) -> OrderRequest {
    serde_json::from_value(serde_json::json!({
        "client_order_id": coid, "venue": "test", "symbol": "BTCUSDT",
        "side": 1, "qty": 0.01, "order_type": "limit", "price": 50000.0
    }))
    .unwrap()
}

/// Drain everything currently on the ingest lane into a flat `Vec<Event>` (the receiver is a
/// tokio mpsc; `try_recv` is non-blocking).
macro_rules! drain {
    ($rx:expr) => {{
        let mut out: Vec<Event> = Vec::new();
        while let Ok(ing) = $rx.try_recv() {
            if let Ingest::Event(e) = ing {
                out.push(e);
            }
        }
        out
    }};
}

/// Poll the ingest lane every 2 ms, collecting events, until `done(&seen)` holds or 5 s run out,
/// and return everything seen. It replaces a fixed sleep that had to outlast another thread's
/// work: the caller's assertions run on the returned events unchanged, so a missing effect still
/// fails them (with their own message) instead of passing on a short wait.
fn drain_until(
    rx: &mut tokio::sync::mpsc::Receiver<Ingest>,
    done: impl Fn(&[Event]) -> bool,
) -> Vec<Event> {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut seen = Vec::new();
    loop {
        seen.extend(drain!(rx));
        if done(&seen) || std::time::Instant::now() >= deadline {
            return seen;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn submit_forwards_events_and_cancel_confirms() {
    let (sender, mut ingest) = event_channel(64);
    let rest = MockRest::default();
    let (tx, cmd_rx) = channel();
    tx.send(ExecCommand::Submit(Box::new(req("c1")))).unwrap();
    tx.send(ExecCommand::Cancel {
        client_order_id: "c1".to_string(),
        intent: CancelIntent::Unspecified,
    })
    .unwrap();
    tx.send(ExecCommand::Shutdown).unwrap();
    // long sentinel + Shutdown already queued → the gap-sentinel never fires here
    run_loop(&rest, &sender, cmd_rx, Vec::<Event>::new, Duration::from_secs(3600));

    assert_eq!(*rest.submitted.lock().unwrap(), vec!["c1".to_string()]);
    assert_eq!(*rest.canceled.lock().unwrap(), vec!["c1".to_string()]);
    let events = drain!(ingest);
    let kinds: Vec<&str> = events
        .iter()
        .map(|e| match e {
            Event::OrderSubmitted(_) => "Submitted",
            Event::OrderAccepted(_) => "Accepted",
            Event::OrderCanceled(_) => "Canceled",
            Event::OrderCancelRejected(_) => "CancelRejected",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, vec!["Submitted", "Accepted", "Canceled"]);
}

#[test]
fn failed_cancel_surfaces_as_cancel_rejected() {
    let (sender, mut ingest) = event_channel(64);
    let rest = MockRest { cancel_fails: true, ..Default::default() };
    let (tx, cmd_rx) = channel();
    tx.send(ExecCommand::Cancel {
        client_order_id: "c9".to_string(),
        intent: CancelIntent::Unspecified,
    })
    .unwrap();
    tx.send(ExecCommand::Shutdown).unwrap();
    run_loop(&rest, &sender, cmd_rx, Vec::<Event>::new, Duration::from_secs(3600));

    let events = drain!(ingest);
    match events.as_slice() {
        [Event::OrderCancelRejected(e)] => {
            assert_eq!(e.client_order_id, "c9");
            assert_eq!(e.reason, "boom");
        }
        other => panic!("expected one OrderCancelRejected, got {other:?}"),
    }
}

/// Modify routing, BOTH arms: a native amend's `[OrderModified]` is forwarded to the ingest;
/// the `VenueRest` default no-op forwards nothing (the resting order keeps its terms). The
/// venue-reality ties (`caps_for(venue).supports_modify`) stay in each venue's own tests.
#[test]
fn modify_routes_to_modify_order_and_forwards_events() {
    // native amend: `[OrderModified]` reaches the ingest
    let (sender, mut ingest) = event_channel(64);
    let rest = MockRest { native_modify: true, ..Default::default() };
    let (tx, cmd_rx) = channel();
    tx.send(ExecCommand::Modify {
        order: Box::new(req("c1")),
        new_qty: None,
        new_price: Some(51_000.0),
    })
    .unwrap();
    tx.send(ExecCommand::Shutdown).unwrap();
    run_loop(&rest, &sender, cmd_rx, Vec::<Event>::new, Duration::from_secs(3600));

    assert_eq!(*rest.modified.lock().unwrap(), vec![("c1".to_string(), Some(51_000.0))]);
    let events = drain!(ingest);
    match events.as_slice() {
        [Event::OrderModified(e)] => {
            assert_eq!(e.client_order_id, "c1");
            assert_eq!(e.new_price, Some(51_000.0));
        }
        other => panic!("expected one OrderModified, got {other:?}"),
    }

    // default no-op: `modify_order` is still invoked, but nothing reaches the ingest
    let (sender, mut ingest) = event_channel(64);
    let rest = MockRest::default();
    let (tx, cmd_rx) = channel();
    tx.send(ExecCommand::Modify {
        order: Box::new(req("c2")),
        new_qty: None,
        new_price: Some(52_000.0),
    })
    .unwrap();
    tx.send(ExecCommand::Shutdown).unwrap();
    run_loop(&rest, &sender, cmd_rx, Vec::<Event>::new, Duration::from_secs(3600));

    assert_eq!(*rest.modified.lock().unwrap(), vec![("c2".to_string(), Some(52_000.0))]);
    assert!(drain!(ingest).is_empty(), "the default no-op must forward no events");
}

/// The gap-sentinel: after a Submit with NO follow-up command, `run_loop` fires the `resync`
/// closure `fill_sentinel` later and forwards its events — the recovery path for a fill the WS
/// lost. A normal WS delivery would just make this dedup'd downstream.
#[test]
fn sentinel_resyncs_after_idle_submit() {
    use std::sync::atomic::AtomicUsize;
    use vike_model::events::OrderCanceled;

    let (sender, mut ingest) = event_channel(64);
    let rest = MockRest::default();
    let (tx, cmd_rx) = channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let resync = {
        let calls = calls.clone();
        move || {
            calls.fetch_add(1, Ordering::Relaxed);
            // a marker event proves the resync's output is forwarded to the ingest
            vec![Event::OrderCanceled(OrderCanceled {
                client_order_id: "resync-marker".to_string(),
                reason: String::new().into(),
                ts: 0,
            })]
        }
    };

    tx.send(ExecCommand::Submit(Box::new(req("c1")))).unwrap();
    // NB: no follow-up command — the sentinel must fire on its own after `fill_sentinel`.
    let h = std::thread::spawn(move || {
        run_loop(&rest, &sender, cmd_rx, resync, Duration::from_millis(60))
    });
    // Wait for the one-shot resync's marker to reach the ingest (it fires `fill_sentinel` after the
    // submit), not for a duration that has to outlast the actor thread's start on a loaded box.
    let events = drain_until(&mut ingest, |seen| {
        seen.iter()
            .any(|e| matches!(e, Event::OrderCanceled(c) if c.client_order_id == "resync-marker"))
    });
    tx.send(ExecCommand::Shutdown).unwrap();
    h.join().unwrap();

    assert!(calls.load(Ordering::Relaxed) >= 1, "sentinel must resync after an idle submit");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::OrderCanceled(c) if c.client_order_id == "resync-marker")),
        "resync events must reach the ingest: {events:?}"
    );
}

/// One replayed fill, stamped.
fn fill_at(coid: &str, trade_id: &'static str, ts: i64) -> Event {
    let fill: vike_model::events::FillEvent = serde_json::from_value(serde_json::json!({
        "trade_id": trade_id, "client_order_id": coid, "venue": "test", "symbol": "BTCUSDT",
        "side": 1, "last_qty": 1.0, "last_px": 50000.0, "commission": 0.1,
        "commission_asset": "USDT", "ts": ts
    }))
    .unwrap();
    Event::Fill(fill)
}

/// **The restart law** (`exec_actor`'s module doc): without a floor, the first gap-sentinel firing
/// after a restart folds a PREVIOUS session's fills through the non-idempotent
/// `Account::apply_fill`.
///
/// A scripted history closure returns one fill stamped an hour BEFORE this `run_loop`'s spawn
/// and one stamped an hour after. Only the second may reach the ingest — and the second is the
/// legitimate case this must not break (a mid-process WS reconnect's gap fills).
#[test]
fn history_replay_drops_fills_older_than_the_actor_spawn() {
    let (sender, mut ingest) = event_channel(64);
    let rest = MockRest::default();
    let (tx, cmd_rx) = channel();
    let now = vike_model::now_ms();
    let resync = move || {
        vec![
            // a PREVIOUS session's fill — retained by the venue, never placed by this process
            fill_at("c_prev_session", "e_old", now - 3_600_000),
            // ...and one from THIS process's life (the WS-gap fill the replay exists for)
            fill_at("c_this_session", "e_new", now + 3_600_000),
            // unstamped: never fabricated into a verdict, so it rides through
            Event::OrderCanceled(vike_model::events::OrderCanceled {
                client_order_id: "c_unstamped".to_string(),
                reason: String::new().into(),
                ts: 0,
            }),
        ]
    };

    tx.send(ExecCommand::Submit(Box::new(req("c1")))).unwrap();
    // no follow-up command — the one-shot sentinel fires on its own
    let h = std::thread::spawn(move || {
        run_loop(&rest, &sender, cmd_rx, resync, Duration::from_millis(60))
    });
    // The batch's LAST event is the unstamped marker: once it lands, every fill the floor lets
    // through has landed before it, so the assertions below see the whole filtered batch.
    let events = drain_until(&mut ingest, |seen| {
        seen.iter()
            .any(|e| matches!(e, Event::OrderCanceled(c) if c.client_order_id == "c_unstamped"))
    });
    tx.send(ExecCommand::Shutdown).unwrap();
    h.join().unwrap();

    let trade_ids: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            Event::Fill(f) => Some(f.trade_id.to_string()),
            _ => None,
        })
        .collect();
    assert!(
        !trade_ids.iter().any(|t| t == "e_old"),
        "a fill stamped BEFORE the actor spawned is a previous session's — it must never \
             reach the ingest, where apply_fill folds its fee and PnL again: {events:?}"
    );
    assert!(
        trade_ids.iter().any(|t| t == "e_new"),
        "a fill stamped after the spawn is the WS-gap recovery this lane exists for and must \
             still be delivered: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::OrderCanceled(c) if c.client_order_id == "c_unstamped")),
        "an UNSTAMPED event carries no evidence of being historical and must ride through \
             (no timestamp is ever fabricated here): {events:?}"
    );
}

/// The floor's predicate, directly: only a POSITIVE stamp strictly below the floor is dropped.
/// The two boundaries are the ones a mutation would flip — `ts == 0` (unstamped) and
/// `ts == spawn_ms` (the instant itself) both survive.
#[test]
fn only_a_positive_stamp_strictly_below_the_floor_is_pre_spawn() {
    let floor = 1_000_000i64;
    assert!(is_pre_spawn(&fill_at("c", "e", floor - 1), floor));
    assert!(!is_pre_spawn(&fill_at("c", "e", floor), floor));
    assert!(!is_pre_spawn(&fill_at("c", "e", floor + 1), floor));
    assert!(!is_pre_spawn(&fill_at("c", "e", 0), floor), "unstamped is not historical");
    // and a floor of 0 (a clock that could not be read) can never drop anything
    assert!(!is_pre_spawn(&fill_at("c", "e", 1), 0));
}

/// ⚠ The core is gone (the ingest receiver is dropped) while the loop still has work: every event
/// it forwards is dropped. The first drop raises ONE `warn!`; the other five (three submits, two
/// events each) stay silent -- a line per message on the exec thread would be a flood.
#[test]
fn run_loop_warns_once_when_the_core_is_gone_not_per_dropped_event() {
    let (sender, ingest) = event_channel(64);
    drop(ingest);
    let rest = MockRest::default();
    let (tx, cmd_rx) = channel();
    for coid in ["c1", "c2", "c3"] {
        tx.send(ExecCommand::Submit(Box::new(req(coid)))).unwrap();
    }
    tx.send(ExecCommand::Shutdown).unwrap();

    let ((), captured) = vike_log::capture::captured(|| {
        run_loop(&rest, &sender, cmd_rx, Vec::<Event>::new, Duration::from_secs(3600));
    });

    assert_eq!(rest.submitted.lock().unwrap().len(), 3, "the loop kept serving commands");
    let warnings: Vec<_> = captured
        .iter()
        .filter(|e| e.level == tracing::Level::WARN && e.message.contains("the core has exited"))
        .collect();
    assert_eq!(warnings.len(), 1, "6 dropped events must log ONE warning, got: {captured:?}");
}

/// The negative: with the core alive the loop logs no core-gone warning.
#[test]
fn run_loop_logs_no_core_gone_warning_while_the_core_is_alive() {
    let (sender, mut ingest) = event_channel(64);
    let rest = MockRest::default();
    let (tx, cmd_rx) = channel();
    tx.send(ExecCommand::Submit(Box::new(req("c1")))).unwrap();
    tx.send(ExecCommand::Shutdown).unwrap();

    let ((), captured) = vike_log::capture::captured(|| {
        run_loop(&rest, &sender, cmd_rx, Vec::<Event>::new, Duration::from_secs(3600));
    });

    assert!(
        !captured.iter().any(|e| e.message.contains("the core has exited")),
        "got: {captured:?}"
    );
    assert_eq!(drain!(ingest).len(), 2);
}
