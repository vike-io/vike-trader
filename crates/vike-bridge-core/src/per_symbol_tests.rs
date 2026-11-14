//! The shared router's own contract over a recording double. Each venue's composition (its clients,
//! factories, refusal line and, for bybit, its mode check and cancel notes) is held by its own
//! crate's `tests/offline/per_symbol.rs`; the rows here are the bodies the binance, bybit and okx
//! copies had in common, pinned once.

use std::sync::atomic::{AtomicUsize, Ordering};

use vike_model::events::OrderSubmitted;

use super::*;

type Log = Arc<Mutex<Vec<String>>>;

struct Client {
    label: String,
    log: Log,
}

impl Client {
    fn say(&self, what: String) {
        lock(&self.log).push(format!("{} {what}", self.label));
    }
}

impl VenueRest for Client {
    fn submit_order(&self, request: &OrderRequest) -> Vec<Event> {
        self.say(format!("submit {}", request.order_client_id));
        vec![Event::OrderSubmitted(OrderSubmitted {
            order_client_id: request.order_client_id.clone(),
            timestamp: request.timestamp,
        })]
    }
    fn cancel_order(
        &self,
        order_client_id: &str,
    ) -> Result<crate::rest::OrderCancelAck, VenueApiError> {
        self.say(format!("cancel {order_client_id}"));
        if order_client_id.starts_with("bad") {
            return Err(VenueApiError { code: 7, msg: order_client_id.to_string() });
        }
        Ok(crate::rest::OrderCancelAck::Canceled)
    }
    fn confirm_order(&self, order_client_id: &str, _ts: i64) -> Vec<Event> {
        self.say(format!("confirm {order_client_id}"));
        Vec::new()
    }
    fn modify_order(&self, order: &OrderRequest, _q: Option<f64>, _p: Option<f64>) -> Vec<Event> {
        self.say(format!("modify {}", order.order_client_id));
        Vec::new()
    }
    fn submit_batch(&self, requests: &[OrderRequest]) -> Vec<Event> {
        self.say(format!("submit_batch {}", requests.len()));
        Vec::new()
    }
    fn cancel_batch(&self, order_client_ids: &[String]) -> Result<(), VenueApiError> {
        self.say(format!("cancel_batch {}", order_client_ids.len()));
        Ok(())
    }
}

impl ConfirmByOrderClientId for Client {
    const LOOKUP_WINDOW: Option<Duration> = Some(Duration::from_secs(7));

    fn confirm_when(&self, order_client_id: &str, _ts: i64, absent_rejects: bool) -> Vec<Event> {
        self.say(format!("confirm_when {order_client_id} {absent_rejects}"));
        Vec::new()
    }
}

/// **The grid fetch of a first order on a symbol spends the order's wait, so the age is asked again before its POST**:
/// `MAX_SUBMIT_WAIT`'s sum counts from the POST, and the core's clock does not stop for a read. The client built for the
/// symbol stays, and the next order on it goes. A close has no age.
#[test]
fn an_order_past_its_wait_after_the_grid_fetch_is_refused_before_the_post() {
    use crate::exec_actor::{MAX_SUBMIT_WAIT, serving_submit};
    let (log, builds, order_client_ids) = (Log::default(), Arc::default(), Arc::default());
    let rest = router(&log, &builds, &order_client_ids);
    let order = OrderRequest {
        order_client_id: "old".into(),
        symbol: "ETHUSDT".into(),
        ..Default::default()
    };
    let old = Instant::now() - MAX_SUBMIT_WAIT - Duration::from_secs(1);
    let said = serving_submit(old, || rest.submit_order(&order));
    assert_eq!(said.len(), 2, "{said:?}");
    assert!(
        matches!(&said[1], Event::OrderRejected(r) if r.reason.contains("NOT sent")),
        "{said:?}"
    );
    assert!(lock(&log).is_empty(), "no POST: {:?}", lock(&log));
    assert_eq!(builds.load(Ordering::SeqCst), 1, "the client it built stays");

    let close = OrderRequest {
        order_client_id: "leg".into(),
        order_type: vike_model::OrderType::Market,
        reduce_only: true,
        ..order.clone()
    };
    serving_submit(old, || rest.submit_order(&close));
    assert_eq!(*lock(&log), ["ETHUSDT submit leg"], "a close has no age");

    rest.submit_order(&OrderRequest { order_client_id: "plain".into(), ..order });
    assert_eq!(lock(&log).len(), 2, "outside a served submit nothing asks");
}

static REFUSALS: AtomicUsize = AtomicUsize::new(0);
static REFUSAL_WORDS: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn refusal_line(request: &OrderRequest, why: &str) {
    REFUSALS.fetch_add(1, Ordering::SeqCst);
    lock(&REFUSAL_WORDS).push(format!("{} {why}", request.symbol));
}

const MOUNTED: &str = "BTCUSDT";

/// A router whose factory builds a client labelled with the symbol, refuses `NOPE`, and counts its
/// calls in `builds`.
fn router(
    log: &Log,
    builds: &Arc<AtomicUsize>,
    order_client_ids: &Arc<OrderClientIdSymbols>,
) -> VenueRestPerSymbol<Client> {
    let (log2, builds2) = (Arc::clone(log), Arc::clone(builds));
    let make: ClientFactory<Client> = Box::new(move |symbol| {
        builds2.fetch_add(1, Ordering::SeqCst);
        if symbol.eq_ignore_ascii_case("NOPE") {
            return Err(format!("no grid row for {symbol}"));
        }
        Ok(Client { label: symbol.to_string(), log: Arc::clone(&log2) })
    });
    VenueRestPerSymbol::new(
        MOUNTED,
        Client { label: "mounted".into(), log: Arc::clone(log) },
        make,
        Arc::clone(order_client_ids),
        refusal_line,
    )
}

fn order(order_client_id: &str, symbol: &str) -> OrderRequest {
    OrderRequest {
        order_client_id: order_client_id.into(),
        symbol: symbol.into(),
        timestamp: 9,
        ..Default::default()
    }
}

fn calls(log: &Log) -> Vec<String> {
    std::mem::take(&mut *lock(log))
}

#[test]
fn a_mounted_or_empty_symbol_goes_to_the_mounted_client_and_the_factory_is_never_asked() {
    let (log, builds, order_client_ids) =
        (Log::default(), Arc::new(AtomicUsize::new(0)), Arc::default());
    let r = router(&log, &builds, &order_client_ids);
    r.submit_order(&order("a", "btcusdt"));
    r.submit_order(&order("b", ""));
    r.modify_order(&order("a", "BTCUSDT"), Some(1.0), None);
    r.cancel_order("a").unwrap();
    assert_eq!(
        calls(&log),
        ["mounted submit a", "mounted submit b", "mounted modify a", "mounted cancel a"]
    );
    assert_eq!(builds.load(Ordering::SeqCst), 0);
    assert_eq!(order_client_ids.symbol_of("a"), None, "a mounted order is never recorded");
    assert!(order_client_ids.take_touched().is_empty(), "the mounted symbol is never touched");
}

#[test]
fn a_foreign_order_builds_its_client_once_records_its_order_client_id_and_routes_every_later_call()
{
    let (log, builds, order_client_ids) =
        (Log::default(), Arc::new(AtomicUsize::new(0)), Arc::default());
    let r = router(&log, &builds, &order_client_ids);
    r.submit_order(&order("e1", "ethusdt"));
    r.submit_order(&order("e2", "ETHUSDT"));
    r.cancel_order("e1").unwrap();
    r.confirm_order("e2", 3);
    r.confirm_when("e2", 3, true);
    // An amend naming no symbol reads the order client id map; one naming it is routed by it.
    r.modify_order(&order("e1", ""), None, Some(2.0));
    assert_eq!(
        calls(&log),
        [
            "ethusdt submit e1",
            "ethusdt submit e2",
            "ethusdt cancel e1",
            "ethusdt confirm e2",
            "ethusdt confirm_when e2 true",
            "ethusdt modify e1",
        ],
        "one client per symbol, built from the FIRST spelling and cached upper-cased"
    );
    assert_eq!(builds.load(Ordering::SeqCst), 1);
    assert_eq!(order_client_ids.symbol_of("e1").as_deref(), Some("ethusdt"));
    assert_eq!(order_client_ids.take_touched(), ["ETHUSDT"]);
    assert_eq!(
        <VenueRestPerSymbol<Client> as ConfirmByOrderClientId>::LOOKUP_WINDOW,
        Some(Duration::from_secs(7))
    );
}

#[test]
fn an_unseen_order_client_id_falls_back_to_the_mounted_symbol() {
    let (log, builds, order_client_ids) =
        (Log::default(), Arc::new(AtomicUsize::new(0)), Arc::default());
    let r = router(&log, &builds, &order_client_ids);
    r.cancel_order("never-seen").unwrap();
    r.confirm_when("never-seen", 1, false);
    assert_eq!(calls(&log), ["mounted cancel never-seen", "mounted confirm_when never-seen false"]);
    assert_eq!(builds.load(Ordering::SeqCst), 0);
}

#[test]
fn a_factory_failure_is_submitted_then_rejected_in_its_words_and_remembered() {
    let (log, builds, order_client_ids) =
        (Log::default(), Arc::new(AtomicUsize::new(0)), Arc::default());
    let r = router(&log, &builds, &order_client_ids);
    let before = REFUSALS.load(Ordering::SeqCst);
    let events = r.submit_order(&order("n1", "NOPE"));
    match events.as_slice() {
        [Event::OrderSubmitted(s), Event::OrderRejected(j)] => {
            assert_eq!((s.order_client_id.as_str(), s.timestamp), ("n1", 9));
            assert_eq!(j.order_client_id, "n1");
            assert_eq!(j.reason.as_str(), "no grid row for NOPE");
            assert_eq!((j.timestamp, j.cause.is_none()), (9, true));
        }
        other => panic!("a refused submit is [Submitted, Rejected]: {other:?}"),
    }
    assert!(REFUSALS.load(Ordering::SeqCst) > before, "the venue's own refusal line runs");
    assert!(lock(&REFUSAL_WORDS).contains(&"NOPE no grid row for NOPE".to_string()));
    // Inside the TTL the factory is not asked again, and every verb answers its own refusal shape.
    r.submit_order(&order("n2", "nope"));
    let cancel = r.cancel_order("n1").unwrap_err();
    assert_eq!((cancel.code, cancel.msg.as_str()), (0, "no grid row for NOPE"));
    match r.modify_order(&order("n1", "NOPE"), Some(1.0), None).as_slice() {
        [Event::OrderModifyRejected(m)] => {
            assert_eq!(
                (m.order_client_id.as_str(), m.reason.as_str()),
                ("n1", "no grid row for NOPE")
            );
            assert!(m.timestamp > 0, "stamped when made, never 0");
        }
        other => panic!("a refused amend is one OrderModifyRejected: {other:?}"),
    }
    assert!(r.confirm_order("n1", 1).is_empty(), "a status query with no client says nothing");
    assert_eq!(builds.load(Ordering::SeqCst), 1, "one blocking fetch for the burst");
    assert!(calls(&log).is_empty(), "nothing was signed on a guessed grid");
}

#[test]
fn cancel_and_status_take_the_light_client_unless_the_full_one_exists() {
    let (log, builds, order_client_ids) =
        (Log::default(), Arc::new(AtomicUsize::new(0)), Arc::<OrderClientIdSymbols>::default());
    let lights = Arc::new(AtomicUsize::new(0));
    let (log2, lights2) = (Arc::clone(&log), Arc::clone(&lights));
    let r = router(&log, &builds, &order_client_ids).with_light_factory(Box::new(move |symbol| {
        lights2.fetch_add(1, Ordering::SeqCst);
        Ok(Client { label: format!("light-{symbol}"), log: Arc::clone(&log2) })
    }));
    order_client_ids.record("s1", "SOLUSDT");
    r.cancel_order("s1").unwrap();
    r.confirm_order("s1", 1);
    assert_eq!(calls(&log), ["light-SOLUSDT cancel s1", "light-SOLUSDT confirm s1"]);
    assert_eq!((builds.load(Ordering::SeqCst), lights.load(Ordering::SeqCst)), (0, 1));
    // Once a submit built the full client, cancel takes it.
    r.submit_order(&order("s2", "SOLUSDT"));
    r.cancel_order("s2").unwrap();
    assert_eq!(calls(&log), ["SOLUSDT submit s2", "SOLUSDT cancel s2"]);
    // A modify still needs the grid.
    assert_eq!(builds.load(Ordering::SeqCst), 1);
}

#[test]
fn a_batch_is_native_only_when_every_order_is_mounted_and_a_fan_out_keeps_the_first_error() {
    let (log, builds, order_client_ids) =
        (Log::default(), Arc::new(AtomicUsize::new(0)), Arc::default());
    let r = router(&log, &builds, &order_client_ids);
    r.submit_batch(&[order("m1", MOUNTED), order("m2", "")]);
    assert_eq!(calls(&log), ["mounted submit_batch 2"]);
    r.submit_batch(&[order("m3", MOUNTED), order("x1", "XRPUSDT")]);
    assert_eq!(calls(&log), ["mounted submit m3", "XRPUSDT submit x1"]);

    r.cancel_batch(&["m1".into(), "m2".into()]).unwrap();
    assert_eq!(calls(&log), ["mounted cancel_batch 2"], "no foreign id: the native batch");
    let err = r.cancel_batch(&["bad-1".into(), "x1".into(), "bad-2".into()]).unwrap_err();
    assert_eq!((err.code, err.msg.as_str()), (7, "bad-1"), "first error wins");
    assert_eq!(
        calls(&log),
        ["mounted cancel bad-1", "XRPUSDT cancel x1", "mounted cancel bad-2"],
        "a foreign id fans out per id, each to its own symbol, every id attempted"
    );
}

#[test]
fn cancel_batch_with_runs_the_callers_own_cancel_for_each_id_of_the_fan_out() {
    let (log, builds, order_client_ids) =
        (Log::default(), Arc::new(AtomicUsize::new(0)), Arc::<OrderClientIdSymbols>::default());
    let r = router(&log, &builds, &order_client_ids);
    order_client_ids.record("f1", "DOGEUSDT");
    let seen = Mutex::new(Vec::new());
    r.cancel_batch_with(&["m1".into(), "f1".into()], |c| {
        lock(&seen).push(c.to_string());
        Ok(crate::rest::OrderCancelAck::Canceled)
    })
    .unwrap();
    assert_eq!(*lock(&seen), ["m1", "f1"]);
    assert!(calls(&log).is_empty(), "the caller's cancel, not the router's, ran");
}

#[test]
fn touch_is_upper_cased_deduplicated_never_empty_and_bounded_by_the_engines_cap() {
    let order_client_ids = OrderClientIdSymbols::default();
    order_client_ids.touch("eth");
    order_client_ids.touch("ETH");
    order_client_ids.touch("");
    assert_eq!(order_client_ids.take_touched(), ["ETH"]);
    assert!(order_client_ids.take_touched().is_empty(), "the list starts again");
    for i in 0..vike_execution::MAX_EXTRA_SYMBOLS + 5 {
        order_client_ids.touch(&format!("S{i}"));
    }
    assert_eq!(order_client_ids.take_touched().len(), vike_execution::MAX_EXTRA_SYMBOLS);
}

#[test]
fn note_submit_records_only_a_foreign_order() {
    let (log, builds, order_client_ids) =
        (Log::default(), Arc::new(AtomicUsize::new(0)), Arc::<OrderClientIdSymbols>::default());
    let r = router(&log, &builds, &order_client_ids);
    r.note_submit(&order("m", "BtcUsdt"));
    r.note_submit(&order("f", "adausdt"));
    assert_eq!(
        (order_client_ids.symbol_of("m"), order_client_ids.symbol_of("f").as_deref()),
        (None, Some("adausdt"))
    );
    assert_eq!(order_client_ids.take_touched(), ["ADAUSDT"]);
    assert!(r.all_mounted(&[order("1", ""), order("2", "btcusdt")]));
    assert!(!r.all_mounted(&[order("1", ""), order("2", "ADAUSDT")]));
    assert_eq!(r.mounted_symbol(), MOUNTED);
    assert!(
        calls(&log).is_empty() && builds.load(Ordering::SeqCst) == 0,
        "bookkeeping signs nothing"
    );
}

/// `adopt`, the body the binance, aster, bybit and okx `adopt_restored` each carried: every restored
/// order that names a symbol is recorded as named, an empty symbol is not recorded at all.
/// Kill proof: drop `record`'s empty guard and `"blank"` is mapped to `""`.
#[test]
fn adopt_records_each_restored_order_that_names_a_symbol() {
    let order_client_ids = OrderClientIdSymbols::default();
    order_client_ids.adopt(&[
        vike_execution::RestoredOrder::new("rest-eth", "ETHUSDT"),
        vike_execution::RestoredOrder::new("blank", ""),
        vike_execution::RestoredOrder {
            venue_order_id: Some("8812".into()),
            ..vike_execution::RestoredOrder::new("rest-sol", "SOL-USDT-SWAP")
        },
    ]);
    assert_eq!(order_client_ids.symbol_of("rest-eth").as_deref(), Some("ETHUSDT"));
    assert_eq!(order_client_ids.symbol_of("rest-sol").as_deref(), Some("SOL-USDT-SWAP"));
    assert_eq!(order_client_ids.symbol_of("blank"), None);
    assert!(
        order_client_ids.take_touched().is_empty(),
        "adopting is not a command: nothing is touched"
    );
}

/// A recorded foreign order is never forgotten to make room: the router's table (`OrderClientIdSymbols::default`)
/// has no cap and the router never calls `forget` (`record` says why). A cleared map would send an old foreign order's cancel to the MOUNTED
/// symbol, where the venue answers "unknown order" (binance -2011, bybit 110001, okx 51401) and the
/// cancel reads "already gone" over a resting order. Folded from the per-venue copies (binance, bybit
/// and okx `order_client_id_map_failure_repro.rs`, okx `tests/offline/per_symbol.rs`), with both symbol spellings
/// they recorded, kept verbatim.
#[test]
fn a_recorded_foreign_order_client_id_is_never_forgotten_to_make_room() {
    for foreign in ["ETHUSDT", "ETH-USDT-SWAP"] {
        let order_client_ids = OrderClientIdSymbols::default();
        for n in 0..10_000 {
            order_client_ids.record(&format!("c{n}"), foreign);
        }
        assert_eq!(order_client_ids.symbol_of("c0").as_deref(), Some(foreign));
        assert_eq!(order_client_ids.symbol_of("c9999").as_deref(), Some(foreign));
    }
}

/// The bounded table alpaca, oanda and vike-ibkr keep (their three copies' rows, folded): a recorded order client id
/// keeps its symbol and an unknown one the venue's spelling; past the cap a NEW order is not recorded and
/// the venue's line runs, nothing is evicted for it, re-recording a known order client id is not growth, and only an
/// order that ended leaves.
#[test]
fn a_capped_table_refuses_a_new_order_when_full_and_only_a_finished_order_leaves_it() {
    static FULL: Mutex<Vec<(String, usize)>> = Mutex::new(Vec::new());
    let s = OrderClientIdSymbols::with_cap(2, |order_client_id, cap| {
        lock(&FULL).push((order_client_id.to_string(), cap))
    });
    s.record("c1", "BTCUSD");
    s.record("c2", "ETH/USD");
    assert_eq!(s.label("c1", "BTC/USD"), "BTCUSD");
    assert_eq!(s.label("c2", "ETH/USD"), "ETH/USD");
    assert_eq!(s.label("foreign", "BTC/USD"), "BTC/USD");
    s.record("c3", "SOLUSD");
    assert_eq!(s.label("c3", "SOL/USD"), "SOL/USD", "past the cap: not recorded");
    assert_eq!(*lock(&FULL), [("c3".to_string(), 2)], "and the venue's line said so");
    assert_eq!(s.label("c1", "BTC/USD"), "BTCUSD", "and nothing was evicted for it");
    s.record("c1", "BTCUSD"); // re-recording a known order client id is not growth
    assert_eq!(lock(&FULL).len(), 1);
    s.forget("c1");
    s.forget("never-recorded");
    assert_eq!(s.label("c1", "BTC/USD"), "BTC/USD", "the ended order left");
    assert_eq!(s.label("c2", "ETH/USD"), "ETH/USD", "a live order stayed");
    s.record("c3", "SOLUSD");
    assert_eq!(s.label("c3", "SOL/USD"), "SOLUSD", "the freed slot took the new order");
}

/// An empty order client id or symbol is never recorded, so an unnamed fill keeps the venue's spelling.
#[test]
fn an_empty_order_client_id_or_symbol_is_never_recorded() {
    let s = OrderClientIdSymbols::default();
    s.record("", "BTCUSD");
    s.record("c1", "");
    assert_eq!(s.label("", "BTC/USD"), "BTC/USD");
    assert_eq!(s.symbol_of("c1"), None);
}
