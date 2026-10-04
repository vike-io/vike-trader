use super::*;
use vike_exec::MarkSource;
use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, ConditionalIntent, OrderIntent, RiskGate, RiskLimits, TradingState,
};
use vike_model::OrderRequest;

fn test_core() -> CoreThread<RecordingClient> {
    test_core_with(RecordingClient::default(), Vec::new())
}

/// `test_core` over an arbitrary client + optional EXTRA engines — needed by the mid-expansion
/// fill regression (a client that yields queued events on `poll_events`) and the multi-engine
/// MarketExit walk.
fn test_core_with<C: ExecutionClient>(
    client: C,
    extra: Vec<(f64, ExecutionEngine<C>)>,
) -> CoreThread<C> {
    let engine = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        client,
        "sim",
        "BTCUSDT",
    );
    let market = Arc::new(Conflated {
        state: Mutex::new(ConflatedState::default()),
        drops: AtomicU64::new(0),
    });
    let snapshot =
        Arc::new(ArcSwap::from_pointee(CoreSnapshot::empty(&engine.venue, &engine.symbol)));
    assemble_core(
        engine,
        extra,
        CoreConfig::default(),
        market,
        snapshot,
        Arc::new(AtomicU64::new(0)),
    )
}

/// `test_core` over an explicit [`CoreConfig`] — the sibling-cancel knob test flips
/// `oco_cancel_sibling_on_dead_exit` on; every other field stays at its inert default.
fn test_core_cfg(config: CoreConfig) -> CoreThread<RecordingClient> {
    let engine = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    let market = Arc::new(Conflated {
        state: Mutex::new(ConflatedState::default()),
        drops: AtomicU64::new(0),
    });
    let snapshot =
        Arc::new(ArcSwap::from_pointee(CoreSnapshot::empty(&engine.venue, &engine.symbol)));
    assemble_core(engine, Vec::new(), config, market, snapshot, Arc::new(AtomicU64::new(0)))
}

fn extra_engine(venue: &str, symbol: &str) -> ExecutionEngine<QueuedEventClient> {
    ExecutionEngine::new(
        Account::new(1.0, venue, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        QueuedEventClient::default(),
        venue,
        symbol,
    )
}

/// A `RecordingClient` whose `poll_events` drains a queue the TEST seeds. That is the whole
/// point: it lets a fill be made to land INSIDE the mass-cancel's own `pump_client()`, i.e.
/// after the operator hit the panic button and before the flatten legs are derived.
#[derive(Debug, Default)]
struct QueuedEventClient {
    submissions: Vec<OrderRequest>,
    cancels: Vec<String>,
    pending: std::collections::VecDeque<vike_model::events::Event>,
}

impl vike_exec::ExecutionClient for QueuedEventClient {
    fn poll_events(&mut self) -> Option<vike_model::events::Event> {
        self.pending.pop_front()
    }
    fn submit(&mut self, request: &OrderRequest) {
        self.submissions.push(request.clone());
    }
    fn cancel(&mut self, client_order_id: &str) {
        self.cancels.push(client_order_id.to_string());
    }
}

/// The venue-event sequence a real fill arrives as, for `coid` on (venue, symbol).
///
/// ⚠ `OrderSubmitted` FIRST IS LOAD-BEARING, not decoration. The FSM's table is
/// `Initialized --OrderSubmitted--> Submitted --OrderAccepted--> Accepted --OrderFilled-->
/// Filled`, so without the first hop `OrderAccepted` is ILLEGAL from `Initialized`, the order
/// never leaves `Initialized`, and the closing `OrderFilled` is illegal too — the whole
/// sequence is refused and `dropped_terminal_on_live` moves.
///
/// This helper used to omit it, and the bracket/OCO/OTO tests below still passed — because the
/// contingency drive ran off the EVENT rather than off the fold's verdict, so it armed and
/// cancelled legs for a sequence the engine had rejected end to end. Gating the drive on
/// `Fold::Applied` turned all nine of them red at once, which is how the gap in this fixture was
/// found. `RecordingClient` emits nothing of its own, so the sequence has to be spelled here in
/// full; every REAL adapter emits `[OrderSubmitted, OrderAccepted|OrderRejected]` synchronously
/// at submit (the emitter split — see `vike_binance::exec`/`vike_aster::spot` and the paper
/// exchange's own `submit`), so this now matches what a venue actually delivers.
fn fill_events(
    coid: &str,
    venue: &str,
    symbol: &str,
    side: i32,
    qty: f64,
    px: f64,
) -> Vec<vike_model::events::Event> {
    use vike_model::events::*;
    let fill = FillEvent {
        // minted by this helper, not read off a wire — same `t-<coid>` bytes as before
        trade_id: TradeId::prefixed("t-", coid),
        client_order_id: coid.to_string(),
        venue: venue.into(),
        symbol: symbol.into(),
        side,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "maker".to_string().into(),
        ts: 0,
        mark_price: Some(px),
        position_side: "BOTH".into(),
    };
    vec![
        // Initialized -> Submitted. See this fn's doc: omitting this made every later hop
        // illegal, and the bracket tests only passed because the drive ignored the verdict.
        Event::OrderSubmitted(OrderSubmitted { client_order_id: coid.to_string(), ts: 0 }),
        Event::OrderAccepted(OrderAccepted {
            client_order_id: coid.to_string(),
            venue_order_id: Some(format!("v-{coid}").into()),
            ts: 0,
        }),
        Event::Fill(fill.clone()),
        Event::OrderFilled(OrderFilled { client_order_id: coid.to_string(), fill, ts: 0 }),
    ]
}

fn market_req(coid: &str) -> Box<OrderRequest> {
    Box::new(OrderRequest {
        client_order_id: coid.into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: "market".into(),
        ..Default::default()
    })
}

// Bar has no Default and carries funding/bid/ask/symbol beyond OHLCV — build it in full.
fn mk_bar(ts: i64, low: f64, close: f64) -> vike_model::Bar {
    vike_model::Bar {
        ts,
        open: 100.0,
        high: 100.0,
        low,
        close,
        volume: 1.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

#[test]
fn submit_empty_coid_is_minted_nonempty_respected() {
    let mut c = test_core();
    let minted = c.apply_intent(OrderIntent::Submit(market_req("")), 0);
    assert_eq!(minted.len(), 1);
    assert!(!minted[0].is_empty(), "empty coid must be minted");
    assert_eq!(c.engine.client.submissions[0].client_order_id, minted[0]);

    let kept = c.apply_intent(OrderIntent::Submit(market_req("mine")), 0);
    assert_eq!(kept, vec!["mine".to_string()]);
    assert_eq!(c.engine.client.submissions[1].client_order_id, "mine");
}

#[test]
fn halted_gate_denies_submit_no_client_call() {
    let mut c = test_core();
    c.engine.trading_state = TradingState::Halted;
    c.apply_intent(OrderIntent::Submit(market_req("c1")), 0);
    assert!(c.engine.client.submissions.is_empty(), "RiskGate veto: nothing reaches the client");
}

#[test]
fn bracket_mints_three_linked_coids_but_holds_the_exits() {
    // Live-runtime OTO/OCO: a bracket still mints THREE linked coids, but only the OTO ENTRY
    // goes live to the venue — its protective stop-loss / take-profit are HELD off the venue
    // (the emulation) until the entry fills, so a naked exit can never trigger first.
    let mut c = test_core();
    let spec = vike_model::BracketSpec {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 2.0,
        entry_price: Some(100.0),
        stop_loss: 95.0,
        take_profit: 110.0,
    };
    let coids = c.apply_intent(OrderIntent::Bracket(Box::new(spec)), 0);
    assert_eq!(coids.len(), 3, "three coids: entry + stop-loss + take-profit");
    assert_eq!(c.engine.client.submissions.len(), 1, "only the OTO entry reaches the venue");
    let venue_entry = &c.engine.client.submissions[0];
    assert_eq!(venue_entry.client_order_id, coids[0]);
    // the venue sees a PLAIN order — the OTO/OCO linkage lives in the CORE's book, not the wire
    assert_eq!(venue_entry.contingency_type, None, "the entry reaches the venue link-free");
    assert!(venue_entry.linked_order_ids.is_empty() && venue_entry.parent_order_id.is_none());
    // the entry IS recorded as an active OTO leg in the core's contingency book
    assert!(!c.contingency.is_empty() && !c.contingency.is_held(&coids[0]));
    // the two exits are held pending the entry fill, not at the venue
    assert_eq!(c.held_orders.len(), 2, "stop-loss + take-profit held");
    assert!(c.held_orders.contains_key(&coids[1]) && c.held_orders.contains_key(&coids[2]));
    assert!(c.contingency.is_held(&coids[1]) && c.contingency.is_held(&coids[2]));
}

// ---- live-runtime OTO/OCO drive (submit-hold + fill-drive) ----

fn bracket_spec() -> vike_model::BracketSpec {
    vike_model::BracketSpec {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 2.0,
        entry_price: Some(100.0),
        stop_loss: 95.0,
        take_profit: 110.0,
    }
}

/// Fold the [`OrderAccepted`, `Fill`, `OrderFilled`] sequence for `coid` into the core exactly as
/// a venue delivers a full fill — the `OrderFilled` drives any contingency (`drive_contingency_
/// on_fill`). Same helper shape the mid-expansion fill test uses (`fill_events`).
fn feed_full_fill(c: &mut CoreThread<RecordingClient>, coid: &str, side: i32, qty: f64, px: f64) {
    for ev in fill_events(coid, "sim", "BTCUSDT", side, qty, px) {
        c.dispatch(Ingest::Event(ev));
    }
}

/// Walk `coid` from `Initialized` to `Accepted` exactly as a real adapter does — the emitter
/// split's synchronous `[OrderSubmitted, OrderAccepted]` pair.
///
/// ⚠ REQUIRED before cancelling/expiring a resting order in these tests. `OrderCanceled` is
/// legal only from `Accepted`/`Triggered`/`PartiallyFilled`/`PendingCancel` — NEVER from
/// `Initialized` — and `RecordingClient` emits nothing of its own, so an order it "submitted"
/// sits at `Initialized` until a test says otherwise. Same fixture gap `fill_events` had: while
/// the contingency drive ignored the fold's verdict, cancelling an `Initialized` order still
/// drove the cascade, so the shortcut was invisible.
fn accept(c: &mut CoreThread<RecordingClient>, coid: &str) {
    c.dispatch(Ingest::Event(Event::OrderSubmitted(vike_model::events::OrderSubmitted {
        client_order_id: coid.to_string(),
        ts: 0,
    })));
    c.dispatch(Ingest::Event(Event::OrderAccepted(vike_model::events::OrderAccepted {
        client_order_id: coid.to_string(),
        venue_order_id: Some(format!("v-{coid}").into()),
        ts: 0,
    })));
}

#[test]
fn oto_entry_fill_releases_both_held_exits() {
    let mut c = test_core();
    let coids = c.apply_intent(OrderIntent::Bracket(Box::new(bracket_spec())), 0);
    let (entry, sl, tp) = (coids[0].clone(), coids[1].clone(), coids[2].clone());
    assert_eq!(c.engine.client.submissions.len(), 1, "only the entry is live pre-fill");
    // OTO: the entry's fill arms + releases both protective exits to the venue.
    feed_full_fill(&mut c, &entry, 1, 2.0, 100.0);
    let submitted: Vec<String> =
        c.engine.client.submissions.iter().map(|r| r.client_order_id.clone()).collect();
    assert!(
        submitted.contains(&sl) && submitted.contains(&tp),
        "both exits released: {submitted:?}"
    );
    assert!(c.held_orders.is_empty(), "nothing left held once the parent filled");
    assert!(
        !c.contingency.is_held(&sl) && !c.contingency.is_held(&tp),
        "the released exits are armed (active) in the book"
    );
}

#[test]
fn oco_stop_fill_cancels_the_take_profit() {
    let mut c = test_core();
    let coids = c.apply_intent(OrderIntent::Bracket(Box::new(bracket_spec())), 0);
    let (entry, sl, tp) = (coids[0].clone(), coids[1].clone(), coids[2].clone());
    feed_full_fill(&mut c, &entry, 1, 2.0, 100.0); // arms sl + tp
    feed_full_fill(&mut c, &sl, -1, 2.0, 95.0); // OCO direction 1: the stop fills
    assert!(c.engine.client.cancels.contains(&tp), "the stop's fill cancels the take-profit");
    assert!(!c.engine.client.cancels.contains(&sl), "the filled leg is never self-canceled");
    assert!(c.contingency.is_empty(), "the whole group resolved");
}

#[test]
fn oco_take_profit_fill_cancels_the_stop() {
    let mut c = test_core();
    let coids = c.apply_intent(OrderIntent::Bracket(Box::new(bracket_spec())), 0);
    let (entry, sl, tp) = (coids[0].clone(), coids[1].clone(), coids[2].clone());
    feed_full_fill(&mut c, &entry, 1, 2.0, 100.0); // arms sl + tp
    feed_full_fill(&mut c, &tp, -1, 2.0, 110.0); // OCO direction 2: the take-profit fills
    assert!(c.engine.client.cancels.contains(&sl), "the take-profit's fill cancels the stop");
    assert!(c.contingency.is_empty(), "the whole group resolved");
}

// ---- ADVERSARIAL: the contingency drive under a HOSTILE venue --------------------------
//
// The threat model and the conventions for this class of test are stated once, in
// `crates/vike-exec/tests/engine/hostile_venue_fold.rs`'s module doc. In short: TLS is verified, so
// this is not a man-in-the-middle — the actor is the VENUE ITSELF, returning well-formed frames
// with fabricated contents.
//
// These are the adversarial twins of the legitimate drive tests directly above. The property is
// narrow and total: an event the ENGINE DROPPED must drive NOTHING. Before the `Fold` verdict
// gated the drive, `contingency_terminal` classified the event ALONE, so the core law "invalid
// transitions are dropped" did not extend to the bracket/OCO/OTO machinery at all.

/// The bare `FillEvent` out of `fill_events` for `coid` — the wrap's embedded copy.
fn bare_fill(coid: &str, side: i32, qty: f64, px: f64) -> vike_model::events::FillEvent {
    fill_events(coid, "sim", "BTCUSDT", side, qty, px)
        .into_iter()
        .find_map(|e| match e {
            Event::Fill(f) => Some(f),
            _ => None,
        })
        .expect("fill_events yields a bare Fill")
}

// ⚠ WHERE THE ASYMMETRY ACTUALLY IS — this is what makes the drive reachable with a coid the
// FSM refuses, and the first two attempts at these tests were VACUOUS for missing it.
//
// A bracket's protective exits are HELD: `apply_intent` puts them in `held_orders` + the
// contingency book and deliberately never calls `submit_order` for them, so they are NOT in
// `ExecutionEngine::registry`. That is the gap: the CONTINGENCY BOOK knows those coids while the
// ORDER REGISTRY does not. A fabricated terminal naming a held exit is therefore dropped by the
// fold (`dropped_unknown_coid` moves) AND was still driven by the contingency machinery.
//
// Forging a coid that exists in NEITHER (`"{tp}-FORGED"`) proves nothing — `on_fill` on an
// unknown coid is a no-op whether or not the gate is there, so such a test passes with the fix
// reverted. Always mutation-check an adversarial test; a green one may simply be inert.

/// A fabricated `OrderFilled` naming the HELD take-profit. The FSM refuses it (that coid was
/// never registered), but the contingency book knows it — so before the gate it ran the OCO
/// sibling-cancel and **silently destroyed the held STOP-LOSS**, leaving the bracket with no
/// downside protection to release when the entry eventually fills.
#[test]
fn a_fabricated_fill_on_a_held_exit_does_not_destroy_its_oco_sibling() {
    let mut c = test_core();
    let coids = c.apply_intent(OrderIntent::Bracket(Box::new(bracket_spec())), 0);
    let (sl, tp) = (coids[1].clone(), coids[2].clone());
    assert_eq!(c.held_orders.len(), 2, "precondition: both exits held");
    assert!(!c.engine.registry.contains_key(&tp), "precondition: a held exit is UNREGISTERED");

    c.dispatch(Ingest::Event(Event::OrderFilled(vike_model::events::OrderFilled {
        client_order_id: tp.clone(),
        fill: bare_fill(&tp, -1, 2.0, 110.0),
        ts: 1,
    })));

    assert!(c.engine.dropped_unknown_coid > 0, "the fold must have REFUSED the forged fill");
    assert!(
        c.held_orders.contains_key(&sl),
        "the STOP-LOSS must survive a refused fill — losing it leaves the bracket unprotected"
    );
    assert!(c.held_orders.contains_key(&tp), "and the take-profit too");
    assert_eq!(c.held_orders.len(), 2, "the group is untouched");
    assert!(c.contingency.is_held(&sl) && c.contingency.is_held(&tp), "book untouched");
}

/// The terminal-without-fill half of the same hole: a fabricated `OrderCanceled` naming a held
/// exit ran `drive_contingency_on_terminal`, which drops that leg from the held map AND the
/// book — so the take-profit simply VANISHES and is never released when the entry fills.
#[test]
fn a_fabricated_cancel_on_a_held_exit_does_not_remove_it_from_the_bracket() {
    let mut c = test_core();
    let coids = c.apply_intent(OrderIntent::Bracket(Box::new(bracket_spec())), 0);
    let (entry, sl, tp) = (coids[0].clone(), coids[1].clone(), coids[2].clone());

    c.dispatch(Ingest::Event(Event::OrderCanceled(vike_model::events::OrderCanceled {
        client_order_id: tp.clone(),
        reason: "forged".to_string().into(),
        ts: 1,
    })));

    assert!(c.engine.dropped_unknown_coid > 0, "the fold must have REFUSED the forged cancel");
    assert!(c.held_orders.contains_key(&tp), "the take-profit must NOT be dropped");
    assert!(c.contingency.is_held(&tp), "and must still be in the book");

    // ...and it is still there to be released when the entry genuinely fills.
    feed_full_fill(&mut c, &entry, 1, 2.0, 100.0);
    let submitted: Vec<String> =
        c.engine.client.submissions.iter().map(|r| r.client_order_id.clone()).collect();
    assert!(submitted.contains(&tp), "the take-profit still releases: {submitted:?}");
    assert!(submitted.contains(&sl), "and so does the stop-loss");
}

/// THE MUTATION SENTINEL for the three tests above: a gate that suppressed EVERYTHING would
/// pass all of them and fail only this. An event the engine ACCEPTS must drive exactly what it
/// drove before — the fix is "a dropped event drives nothing", never "drive less".
#[test]
fn an_accepted_terminal_still_drives_the_contingency_exactly_as_before() {
    let mut c = test_core();
    let coids = c.apply_intent(OrderIntent::Bracket(Box::new(bracket_spec())), 0);
    let (entry, sl, tp) = (coids[0].clone(), coids[1].clone(), coids[2].clone());

    // OTO: a REAL entry fill still releases both held exits to the venue.
    feed_full_fill(&mut c, &entry, 1, 2.0, 100.0);
    let submitted: Vec<String> =
        c.engine.client.submissions.iter().map(|r| r.client_order_id.clone()).collect();
    assert!(submitted.contains(&sl) && submitted.contains(&tp), "both released: {submitted:?}");
    assert!(c.held_orders.is_empty(), "nothing left held");

    // OCO: a REAL stop fill still cancels the take-profit.
    feed_full_fill(&mut c, &sl, -1, 2.0, 95.0);
    assert!(c.engine.client.cancels.contains(&tp), "the real fill still cancels the sibling");
    assert!(c.contingency.is_empty(), "the group still resolves");
}

#[test]
fn oto_entry_terminating_unfilled_drops_the_held_exits() {
    let mut c = test_core();
    let coids = c.apply_intent(OrderIntent::Bracket(Box::new(bracket_spec())), 0);
    let entry = coids[0].clone();
    assert_eq!(c.held_orders.len(), 2, "sl + tp held");
    // The entry is canceled before it ever fills — its held children can never arm, so the
    // cascade drops them (they were never at the venue, so nothing to cancel there). It has to
    // REACH the venue first: a cancel is only legal on an accepted order (see `accept`).
    accept(&mut c, &entry);
    c.dispatch(Ingest::Event(Event::OrderCanceled(vike_model::events::OrderCanceled {
        client_order_id: entry,
        reason: "user".to_string().into(),
        ts: 0,
    })));
    assert!(c.held_orders.is_empty(), "held exits dropped when the parent terminates unfilled");
    assert!(c.contingency.is_empty(), "no orphaned linkage left behind");
}

#[test]
fn plain_order_is_inert_no_contingency_state_or_cancels() {
    let mut c = test_core();
    // a link-free order: submitted straight through, nothing enters the contingency book.
    let coids = c.apply_intent(OrderIntent::Submit(market_req("p1")), 0);
    assert_eq!(coids, vec!["p1".to_string()]);
    assert_eq!(c.engine.client.submissions.len(), 1, "plain order goes live immediately");
    assert!(c.contingency.is_empty() && c.held_orders.is_empty(), "no contingency state");
    // its fill drives nothing (the byte-identical no-bracket path).
    feed_full_fill(&mut c, "p1", 1, 1.0, 100.0);
    assert!(c.engine.client.cancels.is_empty(), "a plain fill cancels nothing");
    assert!(c.contingency.is_empty());
}

#[test]
fn denied_bracket_entry_drops_its_held_exits_not_orphans_them() {
    // The CRITICAL leak: a bracket entry vetoed by the RiskGate (synchronous `OrderDenied`)
    // must cascade-drop its held exits. Before the fix they were orphaned forever — never armed
    // (no fill ever comes), never removed, re-captured in every Snap.
    let mut c = test_core();
    c.engine.trading_state = TradingState::Halted; // the RiskGate vetoes every new order
    let coids = c.apply_intent(OrderIntent::Bracket(Box::new(bracket_spec())), 0);
    assert_eq!(coids.len(), 3, "coids are still minted+returned");
    assert!(c.engine.client.submissions.is_empty(), "Halted: nothing reaches the venue");
    assert!(c.held_orders.is_empty(), "the denied entry's held exits are DROPPED, not orphaned");
    assert!(c.contingency.is_empty(), "no orphaned contingency linkage survives the denial");
}

#[test]
fn cancel_a_held_bracket_exit_drops_it_and_never_hits_the_venue() {
    let mut c = test_core();
    let coids = c.apply_intent(OrderIntent::Bracket(Box::new(bracket_spec())), 0);
    let (sl, tp) = (coids[1].clone(), coids[2].clone());
    assert_eq!(c.held_orders.len(), 2);
    c.apply_intent(OrderIntent::Cancel(sl.clone()), 0);
    assert!(
        !c.held_orders.contains_key(&sl) && !c.contingency.contains(&sl),
        "the canceled held exit is gone from both the held map and the book"
    );
    assert!(c.held_orders.contains_key(&tp), "its sibling stays held");
    assert!(c.engine.client.cancels.is_empty(), "a held cancel never reaches the venue");
}

#[test]
fn modify_a_held_bracket_exit_updates_the_terms_it_is_released_with() {
    let mut c = test_core();
    let coids = c.apply_intent(OrderIntent::Bracket(Box::new(bracket_spec())), 0);
    let (entry, tp) = (coids[0].clone(), coids[2].clone());
    c.apply_intent(
        OrderIntent::Modify {
            client_order_id: tp.clone(),
            new_qty: Some(5.0),
            new_price: Some(115.0),
        },
        0,
    );
    let held = c.held_orders.get(&tp).expect("still held");
    assert_eq!(held.qty, 5.0);
    assert_eq!(held.price, Some(115.0));
    // and those updated terms are exactly what get released to the venue when the entry fills
    feed_full_fill(&mut c, &entry, 1, 2.0, 100.0);
    let released = c
        .engine
        .client
        .submissions
        .iter()
        .find(|r| r.client_order_id == tp)
        .expect("the take-profit was released");
    assert_eq!(released.qty, 5.0, "the release carries the modified qty");
    assert_eq!(released.price, Some(115.0), "and the modified price");
}

#[test]
fn a_released_exit_dying_unfilled_keeps_the_surviving_sibling() {
    // KNOB OFF (the default): a protective exit that dies unfilled (venue reject/cancel/expire)
    // cleans its OWN stale book entry, but the surviving OCO sibling is KEPT — a position that
    // just lost one protective leg should retain whatever protection it still has. This is the
    // inert default of `CoreConfig::oco_cancel_sibling_on_dead_exit` (`test_core` builds a
    // default config); the knob-ON twin below flips it.
    let mut c = test_core();
    let coids = c.apply_intent(OrderIntent::Bracket(Box::new(bracket_spec())), 0);
    let (entry, sl, tp) = (coids[0].clone(), coids[1].clone(), coids[2].clone());
    feed_full_fill(&mut c, &entry, 1, 2.0, 100.0); // arms + releases sl + tp
    accept(&mut c, &sl); // the released stop-loss reaches the venue and rests there
    // the venue cancels the released stop-loss without it ever filling
    c.dispatch(Ingest::Event(Event::OrderCanceled(vike_model::events::OrderCanceled {
        client_order_id: sl.clone(),
        reason: "venue".to_string().into(),
        ts: 0,
    })));
    assert!(!c.contingency.contains(&sl), "the dead exit's own book entry is cleaned");
    assert!(c.contingency.contains(&tp), "the surviving take-profit is KEPT (protection stays)");
    assert!(!c.engine.client.cancels.contains(&tp), "the sibling is NOT auto-canceled");
}

#[test]
fn a_released_exit_dying_unfilled_cancels_the_sibling_when_the_knob_is_on() {
    // KNOB ON (`CoreConfig::oco_cancel_sibling_on_dead_exit = true`): the same dead released
    // stop-loss now ALSO cancels its surviving OCO take-profit AND cleans its book entry — the
    // fully-flat-book deployment choice. Everything up to the death is identical to the OFF
    // twin above; only the sibling's fate differs.
    let mut c =
        test_core_cfg(CoreConfig { oco_cancel_sibling_on_dead_exit: true, ..Default::default() });
    let coids = c.apply_intent(OrderIntent::Bracket(Box::new(bracket_spec())), 0);
    let (entry, sl, tp) = (coids[0].clone(), coids[1].clone(), coids[2].clone());
    feed_full_fill(&mut c, &entry, 1, 2.0, 100.0); // arms + releases sl + tp to the venue
    accept(&mut c, &sl); // the released stop-loss reaches the venue and rests there
    // the venue cancels the released stop-loss without it ever filling
    c.dispatch(Ingest::Event(Event::OrderCanceled(vike_model::events::OrderCanceled {
        client_order_id: sl.clone(),
        reason: "venue".to_string().into(),
        ts: 0,
    })));
    assert!(!c.contingency.contains(&sl), "the dead exit's own book entry is cleaned");
    assert!(
        !c.contingency.contains(&tp),
        "knob ON: the surviving take-profit is CANCELED and cleaned from the book"
    );
    assert!(
        c.engine.client.cancels.contains(&tp),
        "knob ON: the surviving OCO sibling is canceled at the venue"
    );
    assert!(c.contingency.is_empty(), "the whole group resolved — the book is left flat");
}

/// The two-leg call spread every combo test below is built from. `venue`/`symbol`s match the
/// `test_core` engine ("sim") so the per-leg mark/position lookups actually resolve.
fn combo_spec(qty: f64) -> vike_model::ComboSpec {
    vike_model::ComboSpec {
        venue: "sim".into(),
        side: 1,
        qty,
        legs: vec![
            vike_model::ComboLeg { symbol: "BTC-27MAR26-100000-C".into(), ratio: 1 },
            vike_model::ComboLeg { symbol: "BTC-27MAR26-120000-C".into(), ratio: -1 },
        ],
        net_limit: Some(-0.0125), // a CREDIT combo, the awkward case
        time_in_force: vike_model::TimeInForce::Gtc,
    }
}

/// Give both legs of [`combo_spec`] a real mark on the primary engine. `check_combo` DENIES a
/// leg with no mark (`leg <sym>: no-mark`), so every admitted-path test needs this. The combo
/// gate's per-leg reference is resolver-priced, so feed BOTH the `Account.marks` scalar and the
/// price board — the same pair every live mark write-site writes; a fresh board price equal to
/// the scalar keeps every admitted-path verdict byte-identical.
fn mark_combo_legs<C: ExecutionClient>(c: &mut CoreThread<C>, px: f64) {
    for leg in combo_spec(1.0).legs {
        c.engine.account.set_mark_from("sim", &leg.symbol, px, MarkSource::VenueMark, 0);
        c.engine.price_board.set_mark("sim", &leg.symbol, px, 0);
    }
}

#[test]
fn combo_on_unsupported_venue_gets_a_terminal_reject_never_a_silent_drop() {
    // The emitter-split contract: no order may vanish. A venue that cannot take a combo must
    // still produce a full terminal lifecycle locally, because no venue client will.
    let mut c = test_core();
    mark_combo_legs(&mut c, 100.0);
    let before_seq = c.coid_gen.state().1;

    let coids = c.lower_combo(combo_spec(2.0), 7, false, EngineRoute::Payload);

    assert!(coids.is_empty(), "nothing reached the venue, so no coid is returned");
    assert!(c.engine.client.submissions.is_empty(), "nothing reaches the venue");
    assert_eq!(c.coid_gen.state().1, before_seq + 1, "ONE coid is minted for the combo");
    // the order exists and is TERMINAL — not dropped
    assert_eq!(c.engine.registry.len(), 1);
    let mo = c.engine.registry.values().next().unwrap();
    assert_eq!(mo.status, vike_exec::OrderStatus::Rejected, "must reach a TERMINAL state");
    assert!(c.recent.back().unwrap().contains("no combo support"));
}

#[test]
fn combo_passing_the_gate_mints_exactly_one_coid_and_submits() {
    // ONE coid for the WHOLE combo — not one per leg — carrying both legs on one request.
    let mut c = test_core();
    mark_combo_legs(&mut c, 100.0);
    let before_seq = c.coid_gen.state().1;

    let coids = c.lower_combo(combo_spec(2.0), 7, true, EngineRoute::Payload);

    assert_eq!(coids.len(), 1, "ONE coid for the whole combo, never one per leg");
    assert_eq!(c.coid_gen.state().1, before_seq + 1, "exactly one mint");
    assert_eq!(c.engine.client.submissions.len(), 1, "ONE order reaches the venue");
    let sent = &c.engine.client.submissions[0];
    assert_eq!(sent.client_order_id, coids[0]);
    assert_eq!(sent.combo_legs.len(), 2, "both legs ride the one request");
    // the SIGNED net limit rides through verbatim — never absolute-valued, never clamped
    assert_eq!(sent.price, Some(-0.0125));
    assert!(c.engine.registry.contains_key(&coids[0]), "registered as ONE ManagedOrder");
}

#[test]
fn combo_denied_by_the_gate_emits_order_denied_and_submits_nothing() {
    // A combo veto must surface exactly as a single-order veto does: an `OrderDenied` event,
    // nothing on the wire, and no registry entry.
    let mut c = test_core();
    mark_combo_legs(&mut c, 100.0);
    c.engine.trading_state = TradingState::Halted; // the gate's kill switch precedes all else
    let denied_before = c.engine.dropped_unknown_coid;

    let coids = c.lower_combo(combo_spec(2.0), 7, true, EngineRoute::Payload);

    assert!(coids.is_empty(), "a denied combo returns no coid");
    assert!(c.engine.client.submissions.is_empty(), "nothing reaches the venue");
    assert!(c.engine.registry.is_empty(), "a denied combo never enters the registry");
    assert!(c.recent.back().unwrap().contains("combo DENIED"));
    // the OrderDenied really was published (an unregistered coid is counted as it routes)
    assert!(c.engine.dropped_unknown_coid > denied_before, "OrderDenied was published");
    // the denied path stamps the engine clock BEFORE the gate, like the single-order path
    // (adversarial review, minor #3)
    assert_eq!(c.engine.now_ms, 7, "a denied combo must not leave now_ms stale");
}

#[test]
fn combo_legs_accumulate_so_individually_affordable_legs_are_collectively_denied() {
    // THE point of the whole PR: #453 built leg-by-leg ACCUMULATION (each admitted leg's
    // initial margin is threaded onto the next leg's `margin_used`) precisely so N legs cannot
    // each fit inside the same unchanged free buying power. That behavior only means something
    // once a production caller supplies real per-symbol facts — this test proves it does.
    //
    // Sized so ONE leg fits the account and TWO do not: equity 1000, 100% IM, mark 100,
    // multiplier 1, qty 6 ⇒ each leg needs 6 × 100 × 1 × 1.0 = 600. Leg 1 is admitted
    // (600 <= 1000 free); leg 2 then sees margin_used 600, i.e. only 400 free against another
    // 600 ⇒ the COMBO is denied. (`Account::new`'s first argument is the contract MULTIPLIER,
    // not cash — the account's equity comes from `equity_seed` below.)
    let mut limits = RiskLimits::new();
    limits.im_requirement = Some(1.0);
    let engine = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(limits),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    let market = Arc::new(Conflated {
        state: Mutex::new(ConflatedState::default()),
        drops: AtomicU64::new(0),
    });
    let snapshot =
        Arc::new(ArcSwap::from_pointee(CoreSnapshot::empty(&engine.venue, &engine.symbol)));
    let mut c = assemble_core(
        engine,
        Vec::new(),
        CoreConfig::default(),
        market,
        snapshot,
        Arc::new(AtomicU64::new(0)),
    );
    c.engine.equity_seed = 1000.0;
    mark_combo_legs(&mut c, 100.0);

    let coids = c.lower_combo(combo_spec(6.0), 7, true, EngineRoute::Payload);

    assert!(coids.is_empty(), "legs that individually fit must COLLECTIVELY be denied");
    assert!(c.engine.client.submissions.is_empty(), "nothing reaches the venue");
    let reason = c.recent.back().unwrap();
    assert!(reason.contains("combo DENIED"), "unexpected refusal: {reason}");
    // the SECOND leg is the one that breaks the budget — proving leg 1 was admitted first and
    // its commitment was carried forward, which is the accumulation contract itself.
    assert!(
        reason.contains("BTC-27MAR26-120000-C"),
        "the SECOND leg must be the one denied (accumulation), got: {reason}"
    );

    // And the same combo at a size BOTH legs fit (qty 2 ⇒ 200 each, 400 total <= 1000) passes,
    // so the denial above is a budget verdict rather than a blanket combo refusal.
    let ok = c.lower_combo(combo_spec(2.0), 8, true, EngineRoute::Payload);
    assert_eq!(ok.len(), 1, "a combo that fits in aggregate is admitted");
}

/// **THE ACCOUNT-AGGREGATE CEILING ACCUMULATES ACROSS COMBO LEGS TOO** — the exposure twin of
/// the margin accumulation directly above, and the only test that drives
/// `RiskGate::check_combo`'s `committed_notional`.
///
/// `leg_ctx` reports every leg the account as it stood BEFORE the combo — it is a snapshot, and
/// it has to be, because the legs have not been sent. So without a running total each leg is
/// judged against the same unchanged ceiling and an N-leg combo consumes N times what one leg
/// was allowed: the buying-power hole #453 closed, wearing the exposure axis. This is the
/// production caller that makes the accumulation mean something.
///
/// Sized so ONE leg fits and TWO do not: ceiling 900, mark 100, qty 6 ⇒ each leg projects 600.
/// Leg 1 is admitted (600 ≤ 900); leg 2 then sees 600 already committed against its own 600 and
/// the COMBO is denied — under the account reason, not the per-symbol one, which is never armed
/// here at all.
#[test]
fn combo_legs_accumulate_against_the_account_ceiling_too() {
    let engine = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits { max_account_exposure: Some(900.0), ..RiskLimits::new() }),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    let market = Arc::new(Conflated {
        state: Mutex::new(ConflatedState::default()),
        drops: AtomicU64::new(0),
    });
    let snapshot =
        Arc::new(ArcSwap::from_pointee(CoreSnapshot::empty(&engine.venue, &engine.symbol)));
    let mut c = assemble_core(
        engine,
        Vec::new(),
        CoreConfig::default(),
        market,
        snapshot,
        Arc::new(AtomicU64::new(0)),
    );
    mark_combo_legs(&mut c, 100.0);

    let coids = c.lower_combo(combo_spec(6.0), 7, true, EngineRoute::Payload);

    assert!(coids.is_empty(), "two legs that each fit must COLLECTIVELY be denied");
    assert!(c.engine.client.submissions.is_empty(), "nothing reaches the venue");
    let reason = c.recent.back().expect("a denial is noted").clone();
    assert!(
        reason.contains("over-account-exposure"),
        "the ACCOUNT ceiling must be what refused it, under its own reason: {reason}"
    );
    assert!(
        reason.contains("BTC-27MAR26-120000-C"),
        "…and the SECOND leg must be the one denied, which is the accumulation itself: \
             {reason}"
    );

    // And the same combo at a size both legs fit in AGGREGATE (2 × 200 = 400 ≤ 900) is
    // admitted, so the denial above is a budget verdict rather than a blanket combo refusal —
    // and the ceiling is genuinely armed on this path in both directions.
    let ok = c.lower_combo(combo_spec(2.0), 8, true, EngineRoute::Payload);
    assert_eq!(ok.len(), 1, "a combo that fits in aggregate is admitted");
}

/// REGRESSION (adversarial review, MAJOR #1). The `margin_used` baseline must price a MARKED
/// open position with NO per-symbol IM override exactly as `gate_and_register` does — falling
/// back to the priced symbol's own `im_req` — never silently skipping it. The divergence arms
/// exactly when the global `im_requirement` is None and margin was armed per-symbol, which is
/// precisely what `Command::SetMargin` produces (it only writes `im_by_symbol`): the old skip
/// saw the whole equity as free and ADMITTED a combo whose naked legs would be DENIED —
/// violating the gate's own invariant ("a combo must never pass a gate its naked legs would
/// fail").
#[test]
fn combo_margin_baseline_counts_no_override_positions_like_the_single_path() {
    let mut c = test_core();
    c.engine.equity_seed = 1000.0;
    // margin armed PER-SYMBOL only (Command::SetMargin's exact shape): global im stays None
    for leg in combo_spec(1.0).legs {
        c.engine.gate.limits.im_by_symbol.insert(leg.symbol, 1.0);
    }
    mark_combo_legs(&mut c, 100.0);
    // a MARKED open position on a FOREIGN symbol with NO per-symbol IM override:
    // 9 × 100 × 1, priced at the order/leg symbol's fallback rate 1.0 ⇒ 900 of the 1000
    // equity is already spoken for (mark == avg_px, so equity stays exactly 1000)
    c.engine.account.positions.insert(
        ("sim".into(), "FOREIGN".into(), "BOTH".into()),
        vike_exec::PositionEntry { size: 9.0, avg_px: 100.0, ..Default::default() },
    );
    c.engine.account.set_mark_from("sim", "FOREIGN", 100.0, MarkSource::VenueMark, 0);
    // the margin fold is resolver-priced now — feed the board the same price the live
    // write-sites would store alongside `account.set_mark`
    c.engine.price_board.set_mark("sim", "FOREIGN", 100.0, 0);

    // the NAKED leg is denied by the single-order path: free = 1000 − 900 = 100 while the
    // leg needs 2 × 100 × 1.0 = 200 (explicit limit price: the single-symbol engine's
    // `mark()` prices off the MOUNTED symbol, which this test never marks)
    let leg1 = combo_spec(1.0).legs[0].symbol.clone();
    c.apply_intent(
        OrderIntent::Submit(Box::new(OrderRequest {
            client_order_id: "naked".into(),
            venue: "sim".into(),
            symbol: leg1.clone(),
            side: 1,
            qty: 2.0,
            order_type: "limit".into(),
            price: Some(100.0),
            ..Default::default()
        })),
        0,
    );
    assert!(
        c.engine.client.submissions.is_empty(),
        "precondition: the naked leg is DENIED by the single-order path"
    );

    // CONSISTENCY, asserted directly: the combo carrying that same leg must be denied too.
    // (The old skip priced FOREIGN at 0, saw 1000 free, and admitted BOTH legs.)
    let coids = c.lower_combo(combo_spec(2.0), 7, true, EngineRoute::Payload);
    assert!(coids.is_empty(), "the combo must fail exactly where its naked leg fails");
    assert!(c.engine.client.submissions.is_empty(), "nothing reaches the venue");
    let reason = c.recent.back().unwrap();
    assert!(
        reason.contains(&leg1) && reason.contains("insufficient-margin"),
        "the leg must fail the margin check against the foreign position, got: {reason}"
    );
}

/// MAJOR-3 (the liquidation law's partition in the COMBO leg baseline): an ISOLATED open
/// position is backed by its own walled-off wallet, not the shared equity the combo is
/// admitted against, so it must no longer inflate the per-leg `margin_used` baseline. The
/// scenario is the test above with FOREIGN flipped Isolated: counted (the old fold) it
/// spoke for 900 of the 1000 equity and DENIED the combo; excluded, the combo fits with
/// room to spare and is ADMITTED. Cross books are untouched (the test above still denies).
#[test]
fn combo_margin_baseline_excludes_isolated_positions() {
    let mut c = test_core();
    c.engine.equity_seed = 1000.0;
    for leg in combo_spec(1.0).legs {
        c.engine.gate.limits.im_by_symbol.insert(leg.symbol, 1.0);
    }
    mark_combo_legs(&mut c, 100.0);
    // the SAME foreign position as the consistency test above — 9 × 100 × 1.0 = 900 if
    // counted — but ISOLATED with its own wallet: it never consumes the shared equity.
    c.engine.account.positions.insert(
        ("sim".into(), "FOREIGN".into(), "BOTH".into()),
        vike_exec::PositionEntry {
            size: 9.0,
            avg_px: 100.0,
            margin_mode: vike_model::MarginMode::Isolated,
            isolated_margin: Some(900.0),
        },
    );
    c.engine.account.set_mark_from("sim", "FOREIGN", 100.0, MarkSource::VenueMark, 0);

    // both legs need 2 × 100 × 1.0 = 200 each; baseline 0 + accumulation 200 → 400 of the
    // 1000 equity → ADMITTED. (Counted at 900, leg 1 alone would already fail: 100 free.)
    let coids = c.lower_combo(combo_spec(2.0), 7, true, EngineRoute::Payload);
    assert_eq!(
        coids.len(),
        1,
        "an isolated position must not consume the combo's shared margin baseline: {:?}",
        c.recent.back()
    );
    assert_eq!(c.engine.client.submissions.len(), 1, "the admitted combo reaches the venue");
}

/// The combo twin of #550's `gate_exposure_reads_the_resolver_not_the_stale_mark`: a combo
/// leg's projected-exposure REFERENCE used to be the raw `Account.marks` scalar (`mark_of`),
/// while the SAME crossing's per-leg equity/margin were already resolver-priced. That split let
/// a stale-LOW scalar UNDER-measure a leg's projected exposure and admit a combo the fresh
/// board denies — the risk-unsafe direction, and exactly what the single-order gate closed. The
/// reference now shares the resolver, so the whole crossing speaks ONE price. Long 10 on the
/// FIRST leg, board fresh at 200, stale `Account.marks` at 100, `max_total_exposure` 2500: a
/// combo buy 3 projects (10+3)·200 = 2600 > 2500 on that leg → DENY. Under the split basis it
/// was (10+3)·100 = 1300 → admitted.
#[test]
fn combo_exposure_reads_the_resolver_not_the_stale_mark() {
    let mut c = test_core();
    c.engine.gate.limits.max_total_exposure = Some(2500.0);
    let legs = combo_spec(1.0).legs;
    // pre-existing long on the FIRST leg — the one the exposure cap will trip
    c.engine.account.positions.insert(
        ("sim".into(), legs[0].symbol.as_str().into(), "BOTH".into()),
        vike_exec::PositionEntry { size: 10.0, avg_px: 100.0, ..Default::default() },
    );
    // stale-LOW scalar vs fresh-HIGH board, on BOTH legs (the second leg must also price, or it
    // would `no-mark`-deny under the OLD basis and mask the real verdict)
    for leg in &legs {
        c.engine.account.set_mark_from("sim", &leg.symbol, 100.0, MarkSource::VenueMark, 0);
        c.engine.price_board.set_mark("sim", &leg.symbol, 200.0, 1);
    }

    let coids = c.lower_combo(combo_spec(3.0), 7, true, EngineRoute::Payload);

    assert!(coids.is_empty(), "a fresh board must not be under-measured by a stale mark");
    assert!(c.engine.client.submissions.is_empty(), "nothing reaches the venue");
    let reason = c.recent.back().unwrap();
    assert!(
        reason.contains(&legs[0].symbol) && reason.contains("over-max-exposure"),
        "the first leg must trip the resolver-priced exposure cap, got: {reason}"
    );
}

/// Stated as the invariant: the combo exposure verdict is a function of the RESOLVED price
/// alone — three wildly different `Account.marks` scalars over ONE fresh board all reach the
/// identical DENY, so the raw scalar is no longer an input (the combo twin of
/// `the_notional_lane_verdict_is_independent_of_the_mark_scalar`). Under the split basis, stale
/// 0 and 100 ADMITTED while stale 5_000 denied — the verdict tracked the scalar, not the board.
#[test]
fn the_combo_exposure_verdict_is_independent_of_the_mark_scalar() {
    for stale in [0.0, 100.0, 5_000.0] {
        let mut c = test_core();
        c.engine.gate.limits.max_total_exposure = Some(2500.0);
        let legs = combo_spec(1.0).legs;
        c.engine.account.positions.insert(
            ("sim".into(), legs[0].symbol.as_str().into(), "BOTH".into()),
            vike_exec::PositionEntry { size: 10.0, avg_px: 100.0, ..Default::default() },
        );
        for leg in &legs {
            c.engine.account.set_mark_from("sim", &leg.symbol, stale, MarkSource::VenueMark, 0);
            c.engine.price_board.set_mark("sim", &leg.symbol, 200.0, 1);
        }

        let coids = c.lower_combo(combo_spec(3.0), 7, true, EngineRoute::Payload);

        assert!(
            coids.is_empty() && c.engine.client.submissions.is_empty(),
            "stale scalar {stale} changed a resolver-priced combo exposure verdict"
        );
        let reason = c.recent.back().unwrap();
        assert!(
            reason.contains("over-max-exposure"),
            "stale {stale}: expected the board-priced exposure DENY, got: {reason}"
        );
    }
}

/// REGRESSION (sibling #457 review, MAJOR — the fix belongs in the lowering). A combo's leg
/// fills arrive as bare `Event::Fill`s carrying LEG symbols — neither the engine's mounted
/// symbol nor (before this fix) in `extra_symbols` — so `on_event`'s account-wide-WS symbol
/// filter DROPPED them: the FSM reached Filled (the wraps route by coid) while the Account
/// stayed flat and `Strategy::on_fill` never fired. Registration must admit every leg symbol
/// into the engine's scope.
#[test]
fn combo_leg_fills_fold_into_the_account_for_both_legs() {
    use vike_model::events::{Event, FillEvent, OrderAccepted, OrderFilled};
    let mut c = test_core_with(QueuedEventClient::default(), Vec::new());
    mark_combo_legs(&mut c, 100.0);
    c.engine.collect_applied_fills = true; // a strategy is mounted: on_fill delivery matters

    let coids = c.lower_combo(combo_spec(2.0), 7, true, EngineRoute::Payload);
    assert_eq!(coids.len(), 1, "precondition: the combo was gated + registered + submitted");
    let coid = coids[0].clone();

    // the venue's leg-fill stream: ONE coid, one bare Fill PER LEG carrying the LEG symbol
    // (distinct trade_ids — same-id fills are reconnect-deduped), then the terminal wrap
    let legs = combo_spec(2.0).legs;
    // `&'static str`: both call sites pass a source literal, so `TradeId: From<&'static str>`
    let mk_fill = |tid: &'static str, sym: &str, side: i32| FillEvent {
        trade_id: tid.into(),
        client_order_id: coid.clone(),
        venue: "sim".into(),
        symbol: sym.into(),
        side,
        last_qty: 2.0,
        last_px: 10.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".to_string().into(),
        ts: 8,
        mark_price: Some(10.0),
        position_side: "BOTH".into(),
    };
    let f1 = mk_fill("t-leg1", &legs[0].symbol, 1); // ratio +1, combo bought ⇒ buy
    let f2 = mk_fill("t-leg2", &legs[1].symbol, -1); // ratio −1 ⇒ sell
    c.engine.client.pending.push_back(Event::OrderAccepted(OrderAccepted {
        client_order_id: coid.clone(),
        venue_order_id: Some("v-combo".to_string().into()),
        ts: 8,
    }));
    c.engine.client.pending.push_back(Event::Fill(f1));
    c.engine.client.pending.push_back(Event::Fill(f2.clone()));
    c.engine.client.pending.push_back(Event::OrderFilled(OrderFilled {
        client_order_id: coid,
        fill: f2,
        ts: 8,
    }));
    c.pump_client();

    // the Account gained BOTH leg positions — the whole point of the symbol admission
    assert_eq!(c.engine.position_size_of(&legs[0].symbol, "BOTH"), 2.0);
    assert_eq!(c.engine.position_size_of(&legs[1].symbol, "BOTH"), -2.0);
    // and the strategy actually HEARS its fills
    assert_eq!(c.engine.applied_fills.len(), 2, "on_fill delivery for both leg fills");

    // idempotent: re-registering the same legs must not grow extra_symbols again
    let n = c.engine.extra_symbols.len();
    let again = c.lower_combo(combo_spec(2.0), 9, true, EngineRoute::Payload);
    assert_eq!(again.len(), 1);
    assert_eq!(c.engine.extra_symbols.len(), n, "leg symbols are admitted exactly once");
}

/// A hand-built INVALID spec must refuse BEFORE the mint (adversarial review, minor #1): a
/// burned coid would leave a sequence gap, and a gap is only diagnostic while ids that name
/// no order stay impossible — the same mint-after-validation rule the `ArmConditional` arm
/// documents.
#[test]
fn combo_invalid_spec_refuses_without_burning_a_coid() {
    let mut c = test_core();
    let before_seq = c.coid_gen.state().1;
    let mut spec = combo_spec(1.0);
    spec.legs.truncate(1); // < 2 legs: the constructor/Deserialize would refuse this
    let coids = c.lower_combo(spec, 7, true, EngineRoute::Payload);
    assert!(coids.is_empty());
    assert_eq!(c.coid_gen.state().1, before_seq, "a refused spec must not burn a coid");
    assert!(c.engine.client.submissions.is_empty());
    assert!(c.recent.back().unwrap().contains("combo REFUSED"));
}

/// A combo veto must reach `Strategy::on_order_event` exactly as a single-order veto does
/// (`gate_and_register` pushes a Denied `OrderEventOut` at its veto site) — routed by the
/// FIRST leg's symbol, because the combo request's own symbol is EMPTY by design
/// (adversarial review, minor #2).
#[test]
fn combo_denied_captures_an_order_event_for_the_strategy() {
    let mut c = test_core();
    mark_combo_legs(&mut c, 100.0);
    c.engine.collect_applied_fills = true; // a strategy is mounted
    c.engine.trading_state = TradingState::Halted;
    let coids = c.lower_combo(combo_spec(2.0), 7, true, EngineRoute::Payload);
    assert!(coids.is_empty());
    assert_eq!(c.engine.order_events.len(), 1, "the veto must be captured for the strategy");
    let ev = &c.engine.order_events[0];
    assert_eq!(ev.venue, "sim");
    assert_eq!(ev.symbol, combo_spec(1.0).legs[0].symbol, "routed by the FIRST leg's symbol");
    assert!(matches!(
        &ev.event.kind,
        vike_model::strategy::OrderEventKind::Denied { reason } if reason == "halted"
    ));
}

#[test]
fn confirm_routes_to_client_confirm() {
    let mut c = test_core();
    c.apply_intent(
        OrderIntent::Submit(Box::new(OrderRequest {
            client_order_id: "c1".into(),
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            order_type: "limit".into(),
            price: Some(100.0),
            ..Default::default()
        })),
        0,
    );
    c.apply_intent(OrderIntent::Confirm("c1".into()), 0);
    assert_eq!(c.engine.client.confirms, vec!["c1".to_string()]);
}

#[test]
fn flatten_submits_reduce_only_opposite_of_position() {
    let mut c = test_core();
    // seed a +2 long directly (no instant-fill needed); key = (venue, symbol, position_side)
    c.engine.account.positions.insert(
        ("sim".into(), "BTCUSDT".into(), "BOTH".into()),
        vike_exec::PositionEntry { size: 2.0, avg_px: 100.0, ..Default::default() },
    );
    let coids = c.apply_intent(
        OrderIntent::Flatten { venue: "sim".into(), symbol: "BTCUSDT".into(), account: None },
        0,
    );
    assert_eq!(coids.len(), 1);
    let o = &c.engine.client.submissions[0];
    assert_eq!(o.side, -1, "long +2 ⇒ sell to flatten");
    assert_eq!(o.qty, 2.0);
    assert!(o.reduce_only);
    assert_eq!(o.order_type, "market");

    // flat position ⇒ no order
    let mut c2 = test_core();
    let none = c2.apply_intent(
        OrderIntent::Flatten { venue: "sim".into(), symbol: "BTCUSDT".into(), account: None },
        0,
    );
    assert!(none.is_empty());
}

#[test]
fn market_exit_expands_to_mass_cancel_then_flatten_per_open_position() {
    let mut c = test_core();
    c.engine.account.positions.insert(
        ("sim".into(), "BTCUSDT".into(), "BOTH".into()),
        vike_exec::PositionEntry { size: 2.0, avg_px: 100.0, ..Default::default() },
    );
    c.engine.account.positions.insert(
        ("sim".into(), "ETHUSDT".into(), "BOTH".into()),
        vike_exec::PositionEntry { size: -3.0, avg_px: 50.0, ..Default::default() },
    );
    // a FLAT leftover row (a closed position never leaves the map) must NOT expand
    c.engine.account.positions.insert(
        ("sim".into(), "SOLUSDT".into(), "BOTH".into()),
        vike_exec::PositionEntry { size: 0.0, avg_px: 10.0, ..Default::default() },
    );
    let intents = c.expand_market_exit(None);
    assert_eq!(intents.len(), 3, "mass-cancel + 2 flattens (the flat row is skipped)");
    assert!(matches!(
        &intents[0],
        OrderIntent::MassCancel { venue: None, symbol: None, account: None }
    ));
    assert!(
        matches!(&intents[1], OrderIntent::Flatten { symbol, .. } if symbol == "BTCUSDT"),
        "Account insertion order is the expansion order"
    );
    assert!(matches!(&intents[2], OrderIntent::Flatten { symbol, .. } if symbol == "ETHUSDT"));
}

#[test]
fn market_exit_submits_reduce_only_closing_orders() {
    let mut c = test_core();
    c.engine.account.positions.insert(
        ("sim".into(), "BTCUSDT".into(), "BOTH".into()),
        vike_exec::PositionEntry { size: 2.0, avg_px: 100.0, ..Default::default() },
    );
    c.engine.account.positions.insert(
        ("sim".into(), "ETHUSDT".into(), "BOTH".into()),
        vike_exec::PositionEntry { size: -3.0, avg_px: 50.0, ..Default::default() },
    );
    let coids = c.apply_intent(OrderIntent::MarketExit { venue: None, account: None }, 0);
    assert_eq!(coids.len(), 2, "one closing order per open position");
    let subs = &c.engine.client.submissions;
    assert_eq!(subs.len(), 2);
    assert_eq!((subs[0].symbol.as_str(), subs[0].side, subs[0].qty), ("BTCUSDT", -1, 2.0));
    assert_eq!((subs[1].symbol.as_str(), subs[1].side, subs[1].qty), ("ETHUSDT", 1, 3.0));
    assert!(subs.iter().all(|o| o.reduce_only && o.order_type == "market"));
}

#[test]
fn market_exit_is_a_noop_beyond_mass_cancel_when_flat() {
    let mut c = test_core();
    let coids = c.apply_intent(OrderIntent::MarketExit { venue: None, account: None }, 0);
    assert!(coids.is_empty());
    assert!(c.engine.client.submissions.is_empty());
}

#[test]
fn market_exit_scoped_to_a_foreign_venue_flattens_nothing_here() {
    let mut c = test_core();
    c.engine.account.positions.insert(
        ("sim".into(), "BTCUSDT".into(), "BOTH".into()),
        vike_exec::PositionEntry { size: 2.0, avg_px: 100.0, ..Default::default() },
    );
    let intents = c.expand_market_exit(Some("binance"));
    assert_eq!(intents.len(), 1, "only the venue-scoped mass-cancel; sim is not the target");
    assert!(
        matches!(&intents[0], OrderIntent::MassCancel { venue: Some(v), symbol: None, account: None } if v == "binance")
    );
}

#[test]
fn market_exit_expansion_is_replay_deterministic() {
    // The property the compound verb relies on to need NO journal record kind of its own: TWO
    // cores that folded the SAME record prefix (here: the same submits + the same venue fills,
    // applied in the same order) expand `MarketExit` into the same intent list — the "replay"
    // core is a second, independently-built core, not the same one asked twice.
    let build = || {
        let mut c = test_core_with(QueuedEventClient::default(), Vec::new());
        // the engine folds venue events only for symbols it accepts
        c.engine.extra_symbols = vec!["ETHUSDT".into(), "SOLUSDT".into()];
        for (coid, sym, side, qty) in
            [("a", "BTCUSDT", 1, 1.0), ("b", "ETHUSDT", -1, 2.0), ("c", "SOLUSDT", 1, 3.5)]
        {
            c.apply_intent(
                OrderIntent::Submit(Box::new(OrderRequest {
                    client_order_id: coid.into(),
                    venue: "sim".into(),
                    symbol: sym.into(),
                    side,
                    qty,
                    order_type: "limit".into(),
                    price: Some(10.0),
                    ..Default::default()
                })),
                0,
            );
            c.engine.client.pending.extend(fill_events(coid, "sim", sym, side, qty, 10.0));
            c.pump_client();
        }
        c
    };
    let live = build();
    let replayed = build();
    assert_eq!(
        format!("{:?}", live.expand_market_exit(None)),
        format!("{:?}", replayed.expand_market_exit(None))
    );
    // and it is not vacuously equal — the fills really did open three positions
    assert_eq!(live.market_exit_flatten_legs(EngineRoute::Payload, None).len(), 3);
}

/// REGRESSION (adversarial review, major #1). The flatten legs MUST be derived AFTER the
/// mass-cancel, because `MassCancel`'s own arm ends in `pump_client()` — which can fold a FILL
/// that opens a position on a symbol that was FLAT when the operator hit the panic button. A
/// plan snapshotted up front carries no leg for it and the "get me out" verb hands back an
/// open position.
#[test]
fn market_exit_flattens_a_position_opened_by_the_mass_cancels_own_pump() {
    let mut c = test_core_with(QueuedEventClient::default(), Vec::new());
    c.engine.extra_symbols = vec!["ETHUSDT".into()];
    // a resting BUY on ETHUSDT; ETH is FLAT at this point (no fill folded yet)
    c.apply_intent(
        OrderIntent::Submit(Box::new(OrderRequest {
            client_order_id: "e1".into(),
            venue: "sim".into(),
            symbol: "ETHUSDT".into(),
            side: 1,
            qty: 4.0,
            order_type: "limit".into(),
            price: Some(50.0),
            ..Default::default()
        })),
        0,
    );
    assert!(
        c.market_exit_flatten_legs(EngineRoute::Payload, None).is_empty(),
        "precondition: flat at button-press"
    );
    // the venue had already filled it — the events are queued and will surface on the NEXT
    // poll, i.e. inside the mass-cancel's pump
    c.engine.client.pending.extend(fill_events("e1", "sim", "ETHUSDT", 1, 4.0, 50.0));

    c.apply_intent(OrderIntent::MarketExit { venue: None, account: None }, 0);

    assert_eq!(
        c.engine.position_size_of("ETHUSDT", "BOTH"),
        4.0,
        "precondition: the mass-cancel pump really did open the position"
    );
    let close = c
        .engine
        .client
        .submissions
        .iter()
        .find(|o| o.symbol == "ETHUSDT" && o.reduce_only)
        .expect("a flatten leg for the position the mass-cancel pump opened");
    assert_eq!(
        (close.side, close.qty, close.order_type.as_str()),
        (-1, 4.0, "market"),
        "the exit must SEND the closing order; without the post-pump re-derivation the              operator is left long 4 ETH with nothing on the wire"
    );
}

/// THE PANIC BUTTON WORKS FROM A HALTED CORE — the guarantee this test now pins, and the exact
/// REVERSAL of what it pinned before.
///
/// It used to be `market_exit_under_halted_denies_every_flatten_leg_and_says_so`, asserting that
/// `RiskGate`'s kill switch denied EVERY order under `Halted`, `reduce_only` included, so the
/// exit was disarmed in precisely the safe-state / dead-man situations an operator reaches for
/// it. That was pinned as a "deliberate non-bypass"; it was really a trap — halted WITH the
/// position open and no way to close it, the escape being to un-halt the whole core (strategy
/// included) and re-issue. The gate now admits a POSITION-COVERED reduce under `Halted`
/// (`vike_model::is_covered_reduce`), which is exactly the shape `Flatten` mints.
///
/// A kill switch must stop OPENING risk, never trap you in it.
#[test]
fn market_exit_under_halted_still_flattens_because_a_halt_must_not_trap_you() {
    let mut c = test_core();
    c.engine.account.positions.insert(
        ("sim".into(), "BTCUSDT".into(), "BOTH".into()),
        vike_exec::PositionEntry { size: 2.0, avg_px: 100.0, ..Default::default() },
    );
    c.engine.trading_state = TradingState::Halted;
    c.apply_intent(OrderIntent::MarketExit { venue: None, account: None }, 0);
    assert_eq!(
        c.engine.client.submissions.len(),
        1,
        "the flatten leg MUST reach the client while Halted — the operator has to be able to \
             get out: {:?}",
        c.engine.client.submissions
    );
    let leg = &c.engine.client.submissions[0];
    assert_eq!(
        (leg.side, leg.qty, leg.order_type.as_str(), leg.reduce_only),
        (-1, 2.0, "market", true),
        "and it is the closing leg: reduce_only market for the whole position"
    );
    assert!(
        c.recent.iter().any(|m| m.contains("HALTED")),
        "the operator is still TOLD the exit ran under a halt — silence would be worse now \
             that it works, not better: {:?}",
        c.recent
    );
}

// ── the opt-in shutdown sweep (`CoreConfig::cancel_orders_on_shutdown`) ──────────────────
//
// The wiring — that the core's TEARDOWN calls this when the flag is on and not when it is off —
// is proved end to end through a real `spawn_core` in
// `crates/vike-core/tests/wiring/shutdown_cancel_policy.rs`. These two prove what the sweep DOES, from
// in-crate where the client's recorded cancels are directly readable.

/// The sweep cancels every resting order, naming each one to the client.
#[test]
fn the_shutdown_sweep_cancels_every_resting_order() {
    let mut c = test_core();
    c.apply_intent(OrderIntent::Submit(market_req("rest-1")), 0);
    c.apply_intent(OrderIntent::Submit(market_req("rest-2")), 0);
    // `RecordingClient` emits nothing of its own, so both orders sit non-terminal — which is
    // exactly the state a resting order is in when a daemon is stopped.
    assert!(c.engine.client.cancels.is_empty(), "precondition: nothing cancelled yet");

    c.cancel_resting_on_shutdown();

    let mut cancelled = c.engine.client.cancels.clone();
    cancelled.sort();
    assert_eq!(cancelled, vec!["rest-1".to_string(), "rest-2".to_string()]);
    assert!(
        c.recent.iter().any(|m| m.contains("shutdown") && m.contains("positions untouched")),
        "the operator is told what the stop did — and what it did NOT do: {:?}",
        c.recent
    );
}

/// MUTATION SENTINEL: an empty book must not manufacture a cancel, and must not leave a note
/// claiming one happened. A sweep that unconditionally logged would pass the test above.
#[test]
fn the_shutdown_sweep_is_a_no_op_when_nothing_is_resting() {
    let mut c = test_core();
    c.cancel_resting_on_shutdown();
    assert!(c.engine.client.cancels.is_empty(), "nothing rested, so nothing is cancelled");
    assert!(
        !c.recent.iter().any(|m| m.contains("shutdown: cancelled")),
        "and no note claims otherwise: {:?}",
        c.recent
    );
}

/// It CANCELS; it does not FLATTEN. A stop must not decide on its own to realize PnL — closing a
/// position is `MarketExit`, an operator action. Pinned because "cancel on shutdown" is one
/// short step from "go flat on shutdown" in a reader's head.
#[test]
fn the_shutdown_sweep_never_closes_a_position() {
    let mut c = test_core();
    c.engine.account.positions.insert(
        ("sim".into(), "BTCUSDT".into(), "BOTH".into()),
        vike_exec::PositionEntry { size: 2.0, avg_px: 100.0, ..Default::default() },
    );
    c.apply_intent(OrderIntent::Submit(market_req("rest-1")), 0);
    let submitted_before = c.engine.client.submissions.len();

    c.cancel_resting_on_shutdown();

    assert_eq!(c.engine.client.cancels, vec!["rest-1".to_string()], "the order is cancelled");
    assert_eq!(
        c.engine.client.submissions.len(),
        submitted_before,
        "but NO closing order is sent — the position survives the stop"
    );
    assert_eq!(c.engine.position_size_of("BTCUSDT", "BOTH"), 2.0, "the position is untouched");
}

/// THE OTHER HALF, and the mutation sentinel for the test above: admitting the flatten must not
/// have turned `Halted` into a state that admits ORDERS generally. An ordinary opening order on
/// the same halted core still dies at the gate and never reaches the venue.
#[test]
fn market_exit_flattening_under_halt_did_not_open_the_gate_to_opening_orders() {
    let mut c = test_core();
    c.engine.account.positions.insert(
        ("sim".into(), "BTCUSDT".into(), "BOTH".into()),
        vike_exec::PositionEntry { size: 2.0, avg_px: 100.0, ..Default::default() },
    );
    c.engine.trading_state = TradingState::Halted;
    // an opening BUY on the same symbol — risk-INCREASING, and the thing a halt exists to stop
    c.apply_intent(OrderIntent::Submit(market_req("open-me")), 0);
    assert!(
        c.engine.client.submissions.is_empty(),
        "a halt must still refuse an opening order: {:?}",
        c.engine.client.submissions
    );
    // ...and the exit still works on the very same core, in the same state.
    c.apply_intent(OrderIntent::MarketExit { venue: None, account: None }, 0);
    assert_eq!(c.engine.client.submissions.len(), 1, "the exit is still admitted");
    assert!(c.engine.client.submissions[0].reduce_only);
}

/// The documented counterpart: under `Reducing` the flatten legs ARE permitted (they are
/// `reduce_only`). `lanes.rs` claims this; nothing pinned it before.
#[test]
fn market_exit_under_reducing_still_flattens() {
    let mut c = test_core();
    c.engine.account.positions.insert(
        ("sim".into(), "BTCUSDT".into(), "BOTH".into()),
        vike_exec::PositionEntry { size: 2.0, avg_px: 100.0, ..Default::default() },
    );
    c.engine.trading_state = TradingState::Reducing;
    c.apply_intent(OrderIntent::MarketExit { venue: None, account: None }, 0);
    assert_eq!(c.engine.client.submissions.len(), 1, "reduce_only passes the Reducing gate");
    assert!(c.engine.client.submissions[0].reduce_only);
    assert!(c.recent.iter().all(|m| !m.contains("HALTED")));
}

/// The cross-engine walk, the `*pv == eng_venue` filter and `Flatten`'s venue routing were
/// entirely unexercised (review test-gap #3).
///
/// ⚠ **READ WHAT THIS MOUNTS BEFORE TRUSTING ITS NAME.** The two engines are `"sim"` and
/// `"bin"` — TWO EXCHANGES — so each leg's own venue STRING already names its engine and the
/// payload route resolves it correctly with no index at all. This proves the cross-VENUE walk
/// and could never have failed on the cross-ACCOUNT one, which is the case where both legs
/// carry the same venue string and the routing has nothing but the index to go on. That case
/// is `mount_account_tests.rs`'s
/// `a_venue_scoped_market_exit_reaches_each_account_of_the_exchange`, and this test's name
/// read as if it already covered it for a whole round of investigation.
#[test]
fn market_exit_walks_every_engine_and_routes_each_flatten_to_its_own() {
    let mut c =
        test_core_with(QueuedEventClient::default(), vec![(1.0, extra_engine("bin", "BTCUSDT"))]);
    c.engine.account.positions.insert(
        ("sim".into(), "BTCUSDT".into(), "BOTH".into()),
        vike_exec::PositionEntry { size: 2.0, avg_px: 100.0, ..Default::default() },
    );
    c.extra_engines[0].1.account.positions.insert(
        ("bin".into(), "ETHUSDT".into(), "BOTH".into()),
        vike_exec::PositionEntry { size: -3.0, avg_px: 50.0, ..Default::default() },
    );
    let legs = c.market_exit_flatten_legs(EngineRoute::Payload, None);
    assert_eq!(legs.len(), 2, "primary engine first, then extras in registration order");
    assert!(
        matches!(&legs[0], (0, OrderIntent::Flatten { venue, symbol, .. }) if venue == "sim" && symbol == "BTCUSDT")
    );
    assert!(
        matches!(&legs[1], (1, OrderIntent::Flatten { venue, symbol, .. }) if venue == "bin" && symbol == "ETHUSDT")
    );

    c.apply_intent(OrderIntent::MarketExit { venue: None, account: None }, 0);
    assert_eq!(c.engine.client.submissions.len(), 1, "sim leg goes to the sim client");
    assert_eq!(c.engine.client.submissions[0].symbol, "BTCUSDT");
    assert_eq!(c.extra_engines[0].1.client.submissions.len(), 1, "bin leg goes to the bin one");
    assert_eq!(c.extra_engines[0].1.client.submissions[0].symbol, "ETHUSDT");

    // and a venue-scoped exit touches only that engine
    let scoped = c.market_exit_flatten_legs(EngineRoute::Payload, Some("bin"));
    assert!(
        scoped
            .iter()
            .all(|l| matches!(l, (1, OrderIntent::Flatten { venue, .. }) if venue == "bin"))
    );
}

/// Pins the documented hedge-mode scope note (review test-gap #6): non-`BOTH` position rows are
/// SKIPPED, so a future change that starts expanding LONG/SHORT legs cannot land silently.
#[test]
fn market_exit_skips_hedge_mode_long_short_rows() {
    let mut c = test_core();
    for side in ["LONG", "SHORT"] {
        c.engine.account.positions.insert(
            ("sim".into(), "BTCUSDT".into(), side.into()),
            vike_exec::PositionEntry { size: 2.0, avg_px: 100.0, ..Default::default() },
        );
    }
    assert!(
        c.market_exit_flatten_legs(EngineRoute::Payload, None).is_empty(),
        "hedge-mode legs are out of scope until the primitives carry per-leg closes"
    );
    let coids = c.apply_intent(OrderIntent::MarketExit { venue: None, account: None }, 0);
    assert!(coids.is_empty());
    assert!(c.engine.client.submissions.is_empty());
}

/// CHARACTERIZATION (this REVEALS the current verdict; it argues for no cure): a multi-position
/// `MarketExit` METERS ITS OWN LEGS against one order-rate window and starves the tail of them.
///
/// Three facts compose into it, each true on its own:
///
///   1. `Self::market_exit_flatten_legs` mints ONE `OrderIntent::Flatten` per non-flat
///      position, and each lowers to an ordinary `OrderIntent::Submit` — so each crosses
///      `vike_exec::RiskGate::check` with `consume_throttle: true`, like any opening order;
///   2. `RiskGate`'s sliding window is per-GATE, and `vike_mount::make_engine` builds ONE
///      engine — so ONE gate, so ONE window — per venue, shared by every symbol routed
///      through it (the field's own doc says so);
///   3. `Self::apply_intent`'s `MarketExit` arm applies every leg under the SAME `now`, so the
///      window cannot slide between them. `admit_throttle` evicts on `now_ms - window_ms`, and
///      all N stamps are identical.
///
/// `RiskGate::check_inner`'s throttle lane carries no `covered_reduce` term (unlike the min
/// floors, the price collar, the buying-power charge and the impact veto, which all bypass for
/// a covered reduce, and unlike the `Halted` kill switch, which admits one). Neither does
/// `max_notional_per_order`, which `vike_mount::require_live_risk_budget` makes MANDATORY on a
/// live mount and is therefore the more reachable denial there.
/// `crates/vike-exec/tests/risk/risk_lane_completion.rs`'s
/// `a_position_covered_reduce_is_metered_by_the_shared_order_rate_window` and
/// `the_mandatory_live_caps_have_no_covered_reduce_bypass_either` pin those two lanes directly.
///
/// Why nothing caught it: `test_core`/`test_core_with` build the gate from `RiskLimits::new()`,
/// whose `max_orders_per_window` is `None`, so EVERY other `MarketExit` test here — the
/// halt-does-not-trap-you one included — runs with the throttle DISARMED.
#[test]
fn a_multi_position_market_exit_meters_its_own_legs_against_one_window() {
    let mut c = test_core();
    // Arm the window at 2 orders (`RiskLimits::new()` supplies `window_ms = 1000`) on the ONE
    // gate this engine owns, then open THREE positions for it to flatten.
    c.engine.gate.limits.max_orders_per_window = Some(2);
    for symbol in ["BTCUSDT", "ETHUSDT", "SOLUSDT"] {
        c.engine.account.positions.insert(
            ("sim".into(), symbol.into(), "BOTH".into()),
            vike_exec::PositionEntry { size: 2.0, avg_px: 100.0, ..Default::default() },
        );
    }
    assert_eq!(
        c.market_exit_flatten_legs(EngineRoute::Payload, None).len(),
        3,
        "precondition: three legs to mint"
    );

    let denied_before = c.engine.dropped_unknown_coid;
    let coids = c.apply_intent(OrderIntent::MarketExit { venue: None, account: None }, 0);

    // The verb still MINTS a leg per position — a denied submit returns its coid like any
    // other, so the coid list alone reports success.
    assert_eq!(coids.len(), 3, "one leg minted per open position");
    // ...but only the window's worth of them reaches the venue.
    let sent: Vec<&str> = c.engine.client.submissions.iter().map(|o| o.symbol.as_str()).collect();
    assert_eq!(
        sent,
        vec!["BTCUSDT", "ETHUSDT"],
        "the exit spends its own rate window on its first legs and the last one is refused"
    );
    // The refusal is PUBLISHED as an ordinary `OrderDenied`, observed the way
    // `combo_denied_by_the_gate_emits_order_denied_and_submits_nothing` observes it: a denied
    // order was never registered, so its coid is counted as unknown on the way out.
    //
    // ⚠ Deliberately NOT asserted through `c.recent`. An earlier draft did, and it was wrong
    // about the harness rather than the behaviour: `apply_intent` publishes the event but folds
    // nothing, so `recent` — which the combo paths above populate by pushing to it directly —
    // stays EMPTY here even though the leg really was refused. The first two assertions in this
    // test already prove the refusal happened (one leg short on the wire); this proves the
    // engine said so rather than dropping it silently.
    assert!(
        c.engine.dropped_unknown_coid > denied_before,
        "the starved leg's OrderDenied was published"
    );
    // and the starved position is still OPEN — the operator pressed the panic button and is
    // still short of flat by one symbol.
    assert_eq!(c.engine.position_size_of("SOLUSDT", "BOTH"), 2.0);

    // MUTATION SENTINEL: it was the WINDOW that refused it — not the symbol, and not some
    // later lane. The identical leg, on the same core, one window later (`admit_throttle`
    // evicts stamps at or before `now_ms - window_ms`) reaches the venue.
    c.apply_intent(
        OrderIntent::Flatten { venue: "sim".into(), symbol: "SOLUSDT".into(), account: None },
        1_002,
    );
    let sent: Vec<&str> = c.engine.client.submissions.iter().map(|o| o.symbol.as_str()).collect();
    assert_eq!(
        sent,
        vec!["BTCUSDT", "ETHUSDT", "SOLUSDT"],
        "the starved leg must go out once the window slid — otherwise this pins the wrong lane"
    );
}

#[test]
fn tagged_submit_registers_minted_coid_for_modify() {
    let mut c = test_core();
    // simulate what drain_broker does for a tagged limit: apply Submit(empty coid), then register
    let coids = c.apply_intent(
        OrderIntent::Submit(Box::new(OrderRequest {
            client_order_id: String::new(),
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            order_type: "limit".into(),
            price: Some(100.0),
            ts: 0,
            ..Default::default()
        })),
        0,
    );
    c.strategy_tags.insert("0|sim|BTCUSDT|q1".to_string(), coids[0].clone());
    // resolve + modify by tag (the drain's path)
    let coid = c.strategy_tags.get("0|sim|BTCUSDT|q1").cloned().unwrap();
    c.apply_intent(
        OrderIntent::Modify { client_order_id: coid.clone(), new_qty: Some(2.0), new_price: None },
        0,
    );
    // the recording client saw exactly one submit; the modify targets the accepted order (no-op
    // pre-accept is fine — this asserts the tag→coid wiring, not the venue modify)
    assert_eq!(c.engine.client.submissions.len(), 1);
    assert_eq!(c.engine.client.submissions[0].client_order_id, coid);
}

#[test]
fn conditional_fire_is_gated() {
    let mut c = test_core();
    c.engine.account.set_mark_from("sim", "BTCUSDT", 100.0, MarkSource::VenueMark, 0);
    // arm a stop that a downward bar crosses
    c.apply_intent(
        OrderIntent::ArmConditional(ConditionalIntent {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: -1,
            qty: 1.0,
            price: Some(95.0),
            trail: None,
            trigger_by: None,
        }),
        0,
    );
    c.engine.trading_state = TradingState::Halted;
    // fire against a crossing bar via submit_fired's public entry (fire_conditionals_bar)
    let bar = mk_bar(1, 90.0, 92.0);
    c.fire_conditionals_bar("sim", "BTCUSDT", &bar);
    assert!(c.engine.client.submissions.is_empty(), "a fired conditional still crosses RiskGate");
}

#[test]
fn global_mass_cancel_clears_conditional_books() {
    let mut c = test_core();
    c.engine.account.set_mark_from("sim", "BTCUSDT", 100.0, MarkSource::VenueMark, 0);
    c.apply_intent(
        OrderIntent::ArmConditional(ConditionalIntent {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: -1,
            qty: 1.0,
            price: Some(95.0),
            trail: None,
            trigger_by: None,
        }),
        0,
    );
    c.apply_intent(OrderIntent::MassCancel { venue: None, symbol: None, account: None }, 0);
    // a bar that WOULD have crossed the stop now fires nothing (book cleared)
    let bar = mk_bar(1, 90.0, 92.0);
    c.fire_conditionals_bar("sim", "BTCUSDT", &bar);
    assert!(
        c.engine.client.submissions.is_empty(),
        "global mass-cancel must clear armed conditionals"
    );
}

/// The disarm verb (emulator PR-2): removing an arm by its minted id means the crossing bar
/// that would have fired it releases nothing, and the operator got a confirmation note.
#[test]
fn disarm_conditional_removes_the_arm_so_it_no_longer_fires() {
    let mut c = test_core();
    c.engine.account.set_mark_from("sim", "BTCUSDT", 100.0, MarkSource::VenueMark, 0);
    c.apply_intent(
        OrderIntent::ArmConditional(ConditionalIntent {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: -1,
            qty: 1.0,
            price: Some(95.0),
            trail: None,
            trigger_by: None,
        }),
        0,
    );
    // the runtime minted `{coid_session}a0` for the first arm of this session
    let arm_id = format!("{}a0", c.coid_gen.state().0);
    c.apply_intent(OrderIntent::DisarmConditional { arm_id: arm_id.clone() }, 1);
    assert!(
        c.recent.back().unwrap().contains("DISARMED"),
        "the disarm is confirmed on the recent-events surface: {:?}",
        c.recent
    );
    let bar = mk_bar(2, 90.0, 92.0); // would have crossed the 95 stop
    c.fire_conditionals_bar("sim", "BTCUSDT", &bar);
    assert!(c.engine.client.submissions.is_empty(), "a disarmed conditional must not fire");
}

/// An unknown/stale arm id is a LOUD no-op — surfaced to recent-events, nothing disturbed,
/// never a panic (the stale-click tolerance the book's own `disarm` documents).
#[test]
fn disarm_unknown_arm_id_is_a_loud_noop_that_disturbs_nothing() {
    let mut c = test_core();
    c.engine.account.set_mark_from("sim", "BTCUSDT", 100.0, MarkSource::VenueMark, 0);
    c.apply_intent(
        OrderIntent::ArmConditional(ConditionalIntent {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: -1,
            qty: 1.0,
            price: Some(95.0),
            trail: None,
            trigger_by: None,
        }),
        0,
    );
    c.apply_intent(OrderIntent::DisarmConditional { arm_id: "nope".into() }, 1);
    assert!(
        c.recent.back().unwrap().contains("unknown arm id"),
        "the refusal must be loud: {:?}",
        c.recent
    );
    // and the REAL arm is untouched — the crossing bar still fires it
    c.fire_conditionals_bar("sim", "BTCUSDT", &mk_bar(2, 90.0, 92.0));
    assert_eq!(c.engine.client.submissions.len(), 1, "the resting arm still fires");
}

/// The disarm routes across (venue, symbol) books by PROBING for the id (the intent carries
/// only `arm_id`), and targets exactly ONE arm — siblings on the same and other symbols keep
/// firing.
#[test]
fn disarm_targets_exactly_one_arm_across_books() {
    let mut c = test_core();
    for (sym, px) in [("BTCUSDT", 95.0), ("BTCUSDT", 93.0), ("ETHUSDT", 95.0)] {
        c.apply_intent(
            OrderIntent::ArmConditional(ConditionalIntent {
                venue: "sim".into(),
                symbol: sym.into(),
                side: -1,
                qty: 1.0,
                price: Some(px),
                trail: None,
                trigger_by: None,
            }),
            0,
        );
    }
    // arms minted a0 (BTC@95), a1 (BTC@93), a2 (ETH@95); disarm the FIRST BTC one
    let session = c.coid_gen.state().0;
    c.apply_intent(OrderIntent::DisarmConditional { arm_id: format!("{session}a0") }, 1);
    // a bar crossing BOTH BTC stops fires only the surviving a1
    c.fire_conditionals_bar("sim", "BTCUSDT", &mk_bar(2, 90.0, 92.0));
    assert_eq!(c.engine.client.submissions.len(), 1, "only the surviving BTC arm fires");
    // and the ETH book was never touched
    c.fire_conditionals_bar("sim", "ETHUSDT", &mk_bar(3, 90.0, 92.0));
    assert_eq!(c.engine.client.submissions.len(), 2, "the ETH arm still fires");
}

#[test]
fn scoped_mass_cancel_clears_only_its_book() {
    let mut c = test_core();
    c.engine.account.set_mark_from("sim", "BTCUSDT", 100.0, MarkSource::VenueMark, 0);
    c.apply_intent(
        OrderIntent::ArmConditional(ConditionalIntent {
            venue: "sim".into(),
            symbol: "ETHUSDT".into(),
            side: -1,
            qty: 1.0,
            price: Some(95.0),
            trail: None,
            trigger_by: None,
        }),
        0,
    );
    c.engine.account.set_mark_from("sim", "ETHUSDT", 100.0, MarkSource::VenueMark, 0);
    c.apply_intent(
        OrderIntent::MassCancel {
            venue: Some("sim".into()),
            symbol: Some("BTCUSDT".into()),
            account: None,
        },
        0,
    );
    let bar = mk_bar(1, 90.0, 92.0);
    c.fire_conditionals_bar("sim", "ETHUSDT", &bar);
    assert_eq!(c.engine.client.submissions.len(), 1, "the untouched (sim,ETH) book still fires");
}

// ── capability preflight (w2-task-5) ─────────────────────────────────────────────────────

fn venue_req(venue: &str, order_type: &str, tif: vike_model::TimeInForce) -> Box<OrderRequest> {
    Box::new(OrderRequest {
        client_order_id: format!("pf-{venue}-{order_type}"),
        venue: venue.into(),
        symbol: "X".into(),
        side: 1,
        qty: 1.0,
        order_type: order_type.into(),
        price: Some(1.0),
        time_in_force: tif,
        ..Default::default()
    })
}

/// A refused submit follows the Combo arm's shape: NOTHING reaches the client, and the order
/// terminalizes locally as `OrderSubmitted` → `OrderRejected` (status `Rejected`) with the
/// machine-readable reason surfaced on the recent-events strip.
#[test]
fn preflight_refusal_synthesizes_terminal_reject_and_never_reaches_client() {
    let mut c = test_core();
    // deribit wires no trigger orders — a "stop" would coerce to an IMMEDIATE market there
    let coids = c.apply_intent(
        OrderIntent::Submit(venue_req("deribit", "stop", vike_model::TimeInForce::Gtc)),
        7,
    );
    assert_eq!(coids.len(), 1, "the refusal still names the order");
    assert!(c.engine.client.submissions.is_empty(), "refused order must not reach the client");
    let mo = c.engine.registry.get(&coids[0]).expect("registered for the FSM to advance");
    assert_eq!(mo.status, vike_exec::OrderStatus::Rejected, "terminal reject");
    assert!(
        c.recent.iter().any(|n| n.contains("TRIGGER_UNSUPPORTED: kind=stop venue=deribit")),
        "machine-readable reason surfaced: {:?}",
        c.recent
    );
}

/// The aster/ig flip this task ships: a non-GTC limit that would silently rest GTC is now a
/// loud deny — while a GTC limit (what actually rests) still submits.
#[test]
fn preflight_flips_silent_tif_ignores_to_loud_denies() {
    let mut c = test_core();
    c.apply_intent(
        OrderIntent::Submit(venue_req("aster", "limit", vike_model::TimeInForce::Ioc)),
        0,
    );
    assert!(c.engine.client.submissions.is_empty(), "aster Ioc limit is refused");
    assert!(c.recent.iter().any(|n| n.contains("TIF_UNSUPPORTED: tif=Ioc venue=aster")));
    c.apply_intent(
        OrderIntent::Submit(venue_req("aster", "limit", vike_model::TimeInForce::Gtc)),
        0,
    );
    assert_eq!(c.engine.client.submissions.len(), 1, "aster GTC limit still submits");
}

/// THE COMPAT LAW at the core edge: everything venues accept-and-honor today still submits —
/// binance's perp-lane GTD (lane union), the coercion venues' coerced TIFs, and every
/// non-roster (sim/paper) venue id.
#[test]
fn preflight_passes_accepted_requests_through() {
    let mut c = test_core();
    for (venue, ot, tif) in [
        ("binance", "limit", vike_model::TimeInForce::Gtd), // perp lane wires native GTD
        ("polymarket", "limit", vike_model::TimeInForce::Ioc), // live Ioc→FOK coercion
        ("hyperliquid", "take_profit", vike_model::TimeInForce::Gtc), // native tpsl trigger
        ("sim", "limit", vike_model::TimeInForce::Ioc),     // non-roster venue: no row
    ] {
        let before = c.engine.client.submissions.len();
        c.apply_intent(OrderIntent::Submit(venue_req(venue, ot, tif)), 0);
        assert_eq!(
            c.engine.client.submissions.len(),
            before + 1,
            "{venue}/{ot}/{tif:?} must reach the client"
        );
    }
}

/// A bracket with a preflight-refused child is refused ATOMICALLY: no leg reaches the
/// client, the culprit carries its own reason, the siblings carry the culprit's coid.
#[test]
fn bracket_with_unsupported_child_is_refused_whole() {
    let mut c = test_core();
    let spec = vike_model::BracketSpec {
        venue: "deribit".into(), // no native trigger orders → the SL "stop" child is refused
        symbol: "X".into(),
        side: 1,
        qty: 1.0,
        entry_price: Some(100.0),
        stop_loss: 95.0,
        take_profit: 110.0,
    };
    let coids = c.apply_intent(OrderIntent::Bracket(Box::new(spec)), 0);
    assert_eq!(coids.len(), 3);
    assert!(c.engine.client.submissions.is_empty(), "no bracket leg may reach the client");
    for coid in &coids {
        let mo = c.engine.registry.get(coid).expect("every leg registered");
        assert_eq!(mo.status, vike_exec::OrderStatus::Rejected, "{coid} terminal");
    }
    assert!(c.recent.iter().any(|n| n.contains("TRIGGER_UNSUPPORTED: kind=stop venue=deribit")));
    assert!(c.recent.iter().any(|n| n.contains("BRACKET_ATOMIC_REFUSED: culprit=")));
}

/// SubmitBatch on a SINGLE-ENGINE core: a refused leg terminalizes locally while the healthy
/// legs proceed.
///
/// ⚠ **This one is also a byte-identity witness for the `all_primary` narrowing**, and it is
/// worth saying because the doc line used to read "single-engine PATH" and that is no longer
/// which path it takes. The `deribit` leg names a venue this core runs no engine for, so
/// `route_of` answers `None`: under the old `unwrap_or(0) == 0` the batch was `all_primary` and
/// went to engine 0's `submit_order_batch`; under `== Some(0)` it is not, and both legs
/// re-enter the single-submit arm. Every assertion below is unchanged, because the arm they
/// land in resolves the same engine (`unwrap_or(0)`, §4.2's decided `N = 0` cell) and runs the
/// same `caps_venue(routed, …)` preflight that refused the leg here.
#[test]
fn submit_batch_refuses_only_the_unsupported_leg() {
    let mut c = test_core();
    let good = *market_req("b-good");
    let bad = *venue_req("deribit", "stop", vike_model::TimeInForce::Gtc);
    c.apply_intent(OrderIntent::SubmitBatch(vec![good, bad]), 0);
    assert_eq!(c.engine.client.submissions.len(), 1, "only the healthy leg reaches the client");
    assert_eq!(c.engine.client.submissions[0].client_order_id, "b-good");
    let mo = c.engine.registry.get("pf-deribit-stop").expect("refused leg registered");
    assert_eq!(mo.status, vike_exec::OrderStatus::Rejected);
}

/// One leg on this core's own venue, carrying an OTO parent that has not filled, so the
/// single-submit arm must HOLD it rather than send it.
fn held_child_req(coid: &str) -> OrderRequest {
    let mut r = *market_req(coid);
    r.parent_order_id = Some("p-unfilled".into());
    r
}

/// An UNROUTABLE leg — a venue no engine of this core claims, so `route_of` answers `None`. A
/// non-roster id, so `caps_for` resolves the permissive default and the capability preflight
/// has nothing to say about it; the only thing under test here is the ROUTING.
fn unroutable_req(coid: &str) -> OrderRequest {
    let mut r = *market_req(coid);
    r.venue = "nowhere".into();
    r
}

/// ⚠ **A BATCH CARRYING ONE UNROUTABLE LEG ROUTES EVERY LEG BACK THROUGH THE PER-LEG ARM**,
/// instead of taking the primary engine's single batch submit.
///
/// The twin, one row down §4.2's table, of `mount_account_tests`'
/// `an_ambiguous_batch_leg_is_refused_while_its_unambiguous_sibling_is_submitted`.
/// `all_primary` asks whether every leg resolves engine 0, and `unwrap_or(0) == 0` answered
/// `true` for a leg that resolved
/// NOTHING — so a batch containing one could skip the single-submit arm entirely, which is
/// where the contingency book, the refusals and the per-leg lowering live. `== Some(0)` is the
/// fix, and this is what pins it.
///
/// **The probe is the CONTINGENCY HOLD, deliberately, because the destination is not a
/// discriminator.** §4.2's `N = 0` cell is a decision rather than an oversight
/// (`CoreThread::ambiguous_accounts`' own doc), so the per-leg arm still lands an unroutable leg
/// on engine 0 — exactly where the batch arm would have put it. What the batch arm does NOT do
/// is enter `has_contingency_links`: it would have sent a HELD OTO child straight to the venue.
/// So a linked sibling riding along is the one leg whose treatment says which arm ran, and its
/// hold is the evidence that every leg took the per-leg path.
#[test]
fn a_batch_with_an_unroutable_leg_routes_every_leg_through_the_per_leg_arm() {
    let mut c = test_core();
    let coids = c.apply_intent(
        OrderIntent::SubmitBatch(vec![held_child_req("b-child"), unroutable_req("b-nowhere")]),
        0,
    );

    assert_eq!(coids, vec!["b-child".to_string(), "b-nowhere".to_string()], "both legs named");
    assert!(
        c.held_orders.contains_key("b-child"),
        "the OTO child is HELD — only the per-leg arm holds one, so the batch arm was skipped"
    );
    assert!(c.contingency.is_held("b-child"), "…and the book agrees it is not armed");
    assert_eq!(
        c.engine.client.submissions.len(),
        1,
        "so exactly ONE leg reached the client: {:?}",
        c.engine.client.submissions
    );
    assert_eq!(
        c.engine.client.submissions[0].client_order_id, "b-nowhere",
        "and it is the unroutable one, NOT the held child"
    );
}

/// ⚠ **…AND THE UNROUTABLE LEG STILL LANDS ON ENGINE 0** — the path moved, the destination did
/// not.
///
/// §4.2's `N = 0` cell keeps `unwrap_or(0)` and does NOT refuse; that is written down on
/// `CoreThread::ambiguous_accounts` and this narrowing may not change it. The core here is a
/// CROSS-VENUE single-account-per-venue one (`multi_account` is `false`), which is the shape
/// the `all_primary` predicate exists for in the first place: every leg still lands on the
/// primary book, in order, and the second venue's engine is never touched.
///
/// ⚠ What DID move is the CALL SHAPE, and it is the accepted cost rather than a claim of
/// byte-identity: the two legs now reach that book through two `ExecutionClient::submit` calls
/// instead of one `submit_batch`. That is exactly what the mixed-venue path has always cost,
/// and this double records neither shape (its `submit_batch` is the trait default, which fans
/// out to `submit`), so this test pins the DESTINATION — it passes against the old predicate
/// too, deliberately. Its sibling above,
/// `a_batch_with_an_unroutable_leg_routes_every_leg_through_the_per_leg_arm`, is the one that
/// does not.
#[test]
fn an_unroutable_batch_leg_still_lands_on_engine_zero() {
    let bybit = ExecutionEngine::new(
        Account::new(1.0, "bybit", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "bybit",
        "BTCUSDT",
    );
    let mut c = test_core_with(RecordingClient::default(), vec![(1.0, bybit)]);
    assert!(!c.multi_account, "one account per venue — the single-account shape");

    c.apply_intent(
        OrderIntent::SubmitBatch(vec![*market_req("b-own"), unroutable_req("b-nowhere")]),
        0,
    );

    let sent: Vec<&str> =
        c.engine.client.submissions.iter().map(|r| r.client_order_id.as_str()).collect();
    assert_eq!(sent, vec!["b-own", "b-nowhere"], "both legs on the primary book, in order");
    assert!(
        c.extra_engines[0].1.client.submissions.is_empty(),
        "and the other venue's engine was never touched"
    );
}
