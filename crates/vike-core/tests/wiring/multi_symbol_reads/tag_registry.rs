//! GAP 5: the tag registry keys on the MOUNT's series, so any lane's dispatch can pull a quote.

use vike_model::events::{OrderAccepted, OrderCanceled, OrderSubmitted};

use super::*;

// ---------------------------------------------------------------------------------------------
// GAP 5 — the tag registry keyed on the DISPATCHING series
// ---------------------------------------------------------------------------------------------

/// Place a tagged quote from one series' dispatch and pull it from another's. The mount is always
/// `(binance, LEG_A)`; `place_on`/`cancel_on` name the series whose bar drives each step.
///
/// Returns `(submissions, cancels)` as the CLIENT saw them — a cancel that never resolved shows up
/// as an EMPTY `cancels`, which is precisely the "quote left resting at the venue" failure.
fn run_tag(declare: bool, place_on: &str, cancel_on: &str) -> (Vec<(String, String)>, Vec<String>) {
    let looks = Arc::new(Mutex::new(Vec::new()));
    let symbols = if declare { vec![MountLeg::same_venue(LEG_B)] } else { Vec::new() };
    let probe = Probe::new(&looks, vec![(60_000, Act::SubmitTagged), (120_000, Act::CancelTagged)]);
    let w = wires();
    let handle = spawn_core(engine(VENUE, LEG_A, w.client), config(mount(symbols, probe)));

    close_bar(&handle, place_on, 60_000, PX_A);
    close_bar(&handle, cancel_on, 120_000, PX_A);
    handle.shutdown_and_join();

    let subs = w.submissions.lock().unwrap().clone();
    let cans = w.cancels.lock().unwrap().clone();
    (subs, cans)
}

fn assert_the_quote_was_pulled(subs: &[(String, String)], cans: &[String], how: &str) {
    assert_eq!(subs.len(), 1, "{how}: exactly one tagged quote must reach the venue: {subs:?}");
    assert_eq!(
        cans,
        &[subs[0].0.clone()],
        "{how}: the tagged quote ({}) must be CANCELED. An EMPTY list is the silent failure this \
         block exists for — the lookup missed, `drain_broker` did nothing, and a real order is \
         still resting at the venue with no error, no event and no ring line",
        subs[0].0
    );
}

/// **THE LOOKUP HALF.** A quote placed from the MOUNT's own series must be cancellable from a
/// DECLARED LEG's dispatch.
///
/// FAILS ON THE PRE-CHANGE CODE with `cancels == []`: the insert used the drain's
/// `0|binance|BTCUSDT|q` while the cancel, drained on leg B's bar, looked up `0|binance|ETHUSDT|q` —
/// a key nothing ever wrote.
///
/// This is the direction that stays RED if only the INSERT were moved onto the mount's series, which
/// the spec named as the tempting half-fix.
#[test]
fn a_tag_placed_on_the_mount_series_is_cancellable_from_a_leg_dispatch() {
    let (subs, cans) = run_tag(true, LEG_A, LEG_B);
    assert_the_quote_was_pulled(
        &subs,
        &cans,
        "placed on the mount's series, pulled from the leg's",
    );
}

/// **THE INSERT HALF.** The mirror: a quote placed from a DECLARED LEG's dispatch must be
/// cancellable from the mount's own series.
///
/// FAILS ON THE PRE-CHANGE CODE with `cancels == []` for the opposite reason — the insert was
/// `0|binance|ETHUSDT|q` (leg B's bar was draining) and the lookup `0|binance|BTCUSDT|q`. Together
/// with the test above, BOTH sides are proven to have moved: a fix that changed only one leaves
/// exactly one of these two red.
///
/// (The quote itself rests on leg B — a tagged verb names no symbol, so `resolve_intent_symbol`
/// gives it the DRAIN's series. That is unchanged and deliberate; only the KEY moved, which is why
/// the first assertion below is worth making.)
#[test]
fn a_tag_placed_on_a_leg_dispatch_is_cancellable_from_the_mount_series() {
    let (subs, cans) = run_tag(true, LEG_B, LEG_A);
    assert_eq!(subs[0].1, LEG_B, "the tagged quote rests on the DRAIN's series: {subs:?}");
    assert_the_quote_was_pulled(
        &subs,
        &cans,
        "placed on the leg's series, pulled from the mount's",
    );
}

/// ⚠ THE BYTE-IDENTITY GUARD for the registry: a single-symbol mount places and pulls on its own
/// series across two dispatches, exactly as every shipped maker does.
///
/// Passes before and after — that is the point. It is what goes red if the mount-keyed rewrite ever
/// resolves the mount slot wrongly (a lane draining while the slot is taken, say), which would
/// silently take EVERY maker's `cancel_tagged` down with it rather than just the multi-symbol ones.
#[test]
fn a_single_symbol_mounts_tag_round_trip_is_unchanged() {
    let (subs, cans) = run_tag(false, LEG_A, LEG_A);
    assert_the_quote_was_pulled(&subs, &cans, "single-symbol mount, own series both times");
}

/// ⚠ **`mass_cancel()` pulls the MOUNT's book, not the book the dispatch happened to be about.**
///
/// `Broker::mass_cancel` is documented as "cancel ALL of THIS ENGINE's live orders", and
/// `runtime/apply/exit.rs`'s `(Some(v), sym)` arm makes the VENUE the load-bearing half: it resolves that
/// venue's engine and mass-cancels the whole thing, using the symbol only to scope held exits and
/// conditional books. `drain_broker` stamped its own `venue`/`symbol` arguments onto the intent —
/// the DRAIN SERIES, which its doc explicitly says are not the mount's.
///
/// On the fill lane the drain series is the FILL's. So a maker calling `mass_cancel()` from inside
/// `on_fill` — the canonical use, pulling quotes on adverse selection — pulled the book of whichever
/// venue the fill arrived from. On a cross-venue mount that is routinely the HEDGE venue: its own
/// quotes stayed live at the maker venue while an unrelated book was cleared. The same class as
/// `a_same_venue_leg_reads_and_writes_the_same_book` and `tag_key`'s registry bug — a verb keyed on
/// the drain instead of the mount — and the last verb still keyed that way.
///
/// NON-VACUOUS in both directions, which is why both halves are asserted. The resting quote is
/// placed on the MAKER venue and the fill is delivered on the HEDGE venue precisely so the two
/// candidate venues differ: pre-fix the maker's cancel list is EMPTY (the real order keeps resting,
/// silently — no error, no event) and the hedge's engine is the one pulled. A same-venue fill would
/// make both readings agree by accident and prove nothing.
#[test]
fn mass_cancel_pulls_the_mounts_book_not_the_drains() {
    let looks = Arc::new(Mutex::new(Vec::new()));
    let probe = Probe::new(&looks, vec![(60_000, Act::QuoteAndHedge)]).mass_cancelling();
    let maker = wires();
    let hedge = wires();

    let handle = spawn_core_multi(
        engine(VENUE, LEG_A, maker.client),
        vec![(1_000_000.0, engine(VENUE_B, LEG_B, hedge.client))],
        config(mount(vec![MountLeg::at(LEG_B, VENUE_B)], probe)),
    );

    // A resting LIMIT on the mount's OWN venue, then ACCEPT it.
    //
    // ⚠ The accept is load-bearing, and it is the one place this fixture differs from its
    // `cancel_tagged` siblings above. `ExecutionEngine::mass_cancel` collects
    // `registry.filter(|mo| mo.status.is_live())`, and a SUBMITTED order is not live — so without an
    // accept the engine finds nothing to pull and BOTH venues' cancel lists come back empty, which
    // looks exactly like the bug this test is for. `cancel_tagged` has no such filter, which is why
    // those tests need no accept and this one does.
    close_bar(&handle, LEG_A, 60_000, PX_A);
    let maker_coids = wait_for_submissions(&maker.submissions, 1);
    let hedge_coids = wait_for_submissions(&hedge.submissions, 1);
    let ev = handle.event_sender();
    ev.blocking_send(Event::OrderAccepted(OrderAccepted {
        client_order_id: maker_coids[0].clone(),
        venue_order_id: None,
        ts: 1,
    }))
    .unwrap();

    // ...then a fill on the HEDGE venue. This is the dispatch whose drain venue differs from the
    // mount's, and the only place the two spellings could diverge.
    //
    // ⚠ ATTRIBUTED to the hedge order's own coid, which is required rather than tidy: `Act::SeedLegs`
    // documents why — `dispatch_applied_fills` routes by minted coid and falls back to a
    // `(venue, symbol)` match that knows nothing about declared legs, so an UNattributed leg-B fill
    // never reaches a mount whose own symbol is leg A. With `fill("")` this test ran to green
    // assertions having never entered `on_fill` at all.
    ev.blocking_send(Event::Fill(fill(&hedge_coids[0], VENUE_B, LEG_B, -1, -POS_B, PX_B))).unwrap();
    close_bar(&handle, LEG_A, 120_000, PX_A);
    handle.shutdown_and_join();

    let maker_submits = maker.submissions.lock().unwrap().clone();
    let maker_cancels = maker.cancels.lock().unwrap().clone();
    let hedge_cancels = hedge.cancels.lock().unwrap().clone();
    // ⚠ The FIRST thing asserted, because everything below is vacuous without it: if `on_fill` never
    // ran, `mass_cancel()` was never called and both cancel lists are empty for a reason that has
    // nothing to do with routing. An earlier draft of this test sat in exactly that state.
    let lanes: Vec<&str> = looks.lock().unwrap().iter().map(|l| l.lane).collect();
    assert!(
        lanes.contains(&"fill"),
        "fixture: the {VENUE_B} fill must reach `on_fill`, or `mass_cancel()` was never called and \
         this test asserts nothing: lanes={lanes:?}"
    );

    // Fixture check first: the quote this test is about must actually have reached the maker venue,
    // or "it got cancelled" and "it was never placed" are the same observation.
    assert!(
        maker_submits.iter().any(|(_, sym)| sym == LEG_A),
        "fixture: the tagged quote must rest on {VENUE}: {maker_submits:?}"
    );
    assert!(
        !maker_cancels.is_empty(),
        "mass_cancel() during a {VENUE_B} fill must pull the MOUNT's book on {VENUE} — an empty \
         cancel list is the pre-fix behaviour, where a REAL quote stays resting with no error and \
         no event: maker_submits={maker_submits:?} hedge_cancels={hedge_cancels:?}"
    );
    assert!(
        hedge_cancels.is_empty(),
        "...and NOT the book of the venue the fill happened to arrive on: {hedge_cancels:?}"
    );
}

/// ⚠ **A DECLARED CROSS-VENUE LEG'S FILL MUST REACH `on_fill`.** It did not.
///
/// `runtime/assemble.rs`'s `assemble_core` arms each extra engine's applied-fill capture with
/// `mounts.iter().flatten().any(|m| m.venue == e.venue)` — the mount's OWN series venue, and
/// nothing else. Its own comment says "only when a mount TRADES their venue", which is the right
/// rule; the code implemented a narrower one. A mount that declares `MountLeg::at(sym, other)` —
/// the shape an xEMM hedge requires by construction — therefore left the hedge engine's capture
/// DISARMED.
///
/// The failure is silent and asymmetric: the hedge engine folds the fill into its account
/// normally, so positions and PnL stay correct, but it records no `AppliedFill`. With nothing
/// buffered, `dispatch_applied_fills` delivers nothing and `Strategy::on_fill` never runs for that
/// leg. A cross-venue maker is never told its hedge filled — no error, no event, no ring line —
/// while its own inventory view silently diverges from the account the venue agrees with.
///
/// ⚠ WHY IT SURVIVED: no test in the workspace paired `spawn_core_multi` with a hook assertion on a
/// SECONDARY engine's venue. Every existing multi-engine test either mounts nothing on the extra
/// venue or never asks whether the hook ran, so the path had never been executed once.
///
/// FAILS ON THE PRE-FIX CODE at the first assertion, with `lanes == ["bar", "bar"]` — the hook
/// simply never runs. The read assertions after it are what prove the delivery is also CORRECT and
/// not merely present.
#[test]
fn a_cross_venue_leg_fill_reaches_on_fill() {
    let looks = Arc::new(Mutex::new(Vec::new()));
    let probe = Probe::new(&looks, vec![(60_000, Act::QuoteAndHedge)]);
    let maker = wires();
    let hedge = wires();

    let handle = spawn_core_multi(
        engine(VENUE, LEG_A, maker.client),
        vec![(1_000_000.0, engine(VENUE_B, LEG_B, hedge.client))],
        config(mount(vec![MountLeg::at(LEG_B, VENUE_B)], probe)),
    );

    close_bar(&handle, LEG_A, 60_000, PX_A);
    let hedge_coids = wait_for_submissions(&hedge.submissions, 1);
    // Price leg B in its own band, so a read that answered about leg A is identifiable.
    close_bar_on(&handle, VENUE_B, LEG_B, 90_000, PX_B);
    handle
        .event_sender()
        .blocking_send(Event::Fill(fill(&hedge_coids[0], VENUE_B, LEG_B, -1, -POS_B, PX_B)))
        .unwrap();
    close_bar(&handle, LEG_A, 120_000, PX_A);
    handle.shutdown_and_join();

    let out = looks.lock().unwrap().clone();
    let lanes: Vec<&str> = out.iter().map(|l| l.lane).collect();
    assert!(
        lanes.contains(&"fill"),
        "a fill on the DECLARED leg's venue ({VENUE_B}) must reach `on_fill`. lanes={lanes:?} — \
         `[\"bar\", \"bar\"]` is the pre-fix signature: the hedge engine's `collect_applied_fills` \
         was never armed, so the fill was folded into the account and never delivered"
    );

    // ...and the delivery is CORRECT, not merely present: the hook's per-symbol reads must answer
    // about the instrument NAMED, which is the law the rest of this file establishes.
    let l = last_of(&out, "fill");
    assert_eq!(l.pos_b, POS_B, "position({LEG_B}) inside the hedge leg's own fill: {l:?}");
    assert!(
        (50.0..100.0).contains(&l.px_b),
        "price({LEG_B}) = {} must sit in leg B's band — outside it means the read answered about \
         leg A: {l:?}",
        l.px_b
    );
}

/// ⚠ **THE ORDER-EVENT HALF of the cross-venue capture flag** — and the reason it looked broken for
/// a while when it is not.
///
/// `crates/vike-exec/src/execution_engine/mod.rs`'s `order_events` is gated by the SAME
/// `collect_applied_fills` flag the fill lane uses, so a declared cross-venue leg's lifecycle events
/// were lost by the identical mechanism and are repaired by the identical one-line arming fix. This
/// asserts that second lane directly, so a future narrowing of that condition cannot silently
/// re-break one of the two.
///
/// ⚠ **`Event::OrderSubmitted` FIRST IS LOAD-BEARING**, and omitting it is why an earlier version of
/// this test failed against a correct runtime. `ManagedOrder::new` starts at `Initialized`, and
/// nothing local publishes `OrderSubmitted` — the echo comes FROM THE ADAPTER, and `ProbeClient`
/// implements only `submit`/`cancel`. `order.rs`'s `transition` allows `OrderAccepted` only from
/// `Submitted`, so without the prefix `mo.apply` returns `Err(InvalidOrderTransition)` and
/// `on_event` takes its `Fold::Dropped` path — which sits ABOVE the capture block, making the flag's
/// value irrelevant and the whole lane untestable. `crates/vike-core/src/runtime/tests/apply.rs`'s
/// `fill_events` carries the same warning for the same reason.
///
/// The drop is also SILENT for the accept hop specifically: `is_kill_terminal(Initialized)` is
/// false, so no counter moves and nothing is logged. Only the cancel bumps
/// `dropped_terminal_on_live`. That asymmetry is what made the earlier failure read as a runtime
/// bug rather than a fixture one.
///
/// ⚠ Note the already-passing `on_order_event_reads_are_symbol_addressed_on_a_declared_mount` proves
/// nothing about this path: it drives a RiskGate/undeclared REFUSAL, i.e. the `Denied` push inside
/// `gate_and_register`, which never touches the FSM at all.
///
/// FAILS with the arming fix reverted, with no `"order_event"` lane at all.
#[test]
fn a_cross_venue_leg_order_event_reaches_on_order_event() {
    let looks = Arc::new(Mutex::new(Vec::new()));
    let probe = Probe::new(&looks, vec![(60_000, Act::QuoteAndHedge)]);
    let maker = wires();
    let hedge = wires();

    let handle = spawn_core_multi(
        engine(VENUE, LEG_A, maker.client),
        vec![(1_000_000.0, engine(VENUE_B, LEG_B, hedge.client))],
        config(mount(vec![MountLeg::at(LEG_B, VENUE_B)], probe)),
    );

    close_bar(&handle, LEG_A, 60_000, PX_A);
    let hedge_coids = wait_for_submissions(&hedge.submissions, 1);

    // The full adapter sequence: Initialized -> Submitted -> Accepted -> Canceled. Every real
    // adapter emits `[OrderSubmitted, OrderAccepted|OrderRejected]` synchronously at submit.
    let ev = handle.event_sender();
    ev.blocking_send(Event::OrderSubmitted(OrderSubmitted {
        client_order_id: hedge_coids[0].clone(),
        ts: 0,
    }))
    .unwrap();
    ev.blocking_send(Event::OrderAccepted(OrderAccepted {
        client_order_id: hedge_coids[0].clone(),
        venue_order_id: None,
        ts: 1,
    }))
    .unwrap();
    ev.blocking_send(Event::OrderCanceled(OrderCanceled {
        client_order_id: hedge_coids[0].clone(),
        reason: "venue".into(),
        ts: 2,
    }))
    .unwrap();
    close_bar(&handle, LEG_A, 120_000, PX_A);
    handle.shutdown_and_join();

    let lanes: Vec<&str> = looks.lock().unwrap().iter().map(|l| l.lane).collect();
    assert!(
        lanes.contains(&"order_event"),
        "a lifecycle event on the DECLARED leg's venue ({VENUE_B}) must reach `on_order_event` — \
         without it a strategy believes a hedge is still resting when the venue has already \
         canceled it. lanes={lanes:?}"
    );
}

/// ⚠ **A `same_venue` DECLARED LEG LIVES ON THE MOUNT'S VENUE — both halves must say so.**
///
/// `strategy_drive/views.rs`'s `leg_venue` names this test as its gate. The test did not exist: the doc
/// landed and the test did not, so the rule had no regression protection — and writing it found
/// that the rule itself was wrong.
///
/// The original defect was a DISAGREEMENT: the read side fell back to the mount's venue, the write
/// side to the drain's. `leg_venue` unified both onto the DRAIN's venue, which makes them agree and
/// leaves both WRONG. `MountLeg::same_venue` is documented as "a leg on the mount's OWN venue", so
/// during a foreign-venue fill that leg resolved to a book it was never declared on. Measured, with
/// `LEG_C` seeded to 11.0 on `binance` and the fill arriving on `bybit`:
///
/// ```text
///   read:   position(SOLUSDT) == 0.0      resolved against bybit, which holds none
///   write:  SOLUSDT order      -> bybit   an exchange the mount never named
/// ```
///
/// ⚠ The write half is the serious one: a REAL order routed to the wrong exchange, with no error.
///
/// NON-VACUOUS in both directions, which is why BOTH halves are asserted and why the fill is
/// delivered on `bybit` — that is the only dispatch whose venue differs from the mount's, so a
/// same-venue fill would make every spelling agree by accident. Agreement alone proves nothing
/// here: the pre-fix code already had it, which is exactly how it passed review.
///
/// ⚠ Reachable only because of the cross-venue capture fix — before it, `on_fill` never ran for a
/// declared foreign leg at all, so this gate could not have been written even by someone who tried.
/// That is the likeliest reason it never was.
#[test]
fn a_same_venue_leg_reads_and_writes_the_same_book() {
    let looks = Arc::new(Mutex::new(Vec::new()));
    let probe = Probe::new(&looks, vec![(60_000, Act::QuoteAndHedge)]).submitting_on_fill(LEG_C);
    let maker = wires();
    let hedge = wires();

    let handle = spawn_core_multi(
        engine(VENUE, LEG_A, maker.client),
        vec![(1_000_000.0, engine(VENUE_B, LEG_B, hedge.client))],
        config(mount(vec![MountLeg::at(LEG_B, VENUE_B), MountLeg::same_venue(LEG_C)], probe)),
    );

    // Seed the MOUNT's venue with a position in the same-venue leg, so a read served from the WRONG
    // engine (bybit, which holds nothing in it) is distinguishable from the right one. Routed by
    // venue, so it needs no attribution and never enters `on_fill`.
    handle
        .event_sender()
        .blocking_send(Event::Fill(fill("", VENUE, LEG_C, 1, POS_C, PX_C)))
        .unwrap();
    close_bar(&handle, LEG_A, 60_000, PX_A);
    let hedge_coids = wait_for_submissions(&hedge.submissions, 1);

    // ...then a fill on the HEDGE venue: the one dispatch whose venue differs from the mount's.
    handle
        .event_sender()
        .blocking_send(Event::Fill(fill(&hedge_coids[0], VENUE_B, LEG_B, -1, -POS_B, PX_B)))
        .unwrap();
    close_bar(&handle, LEG_A, 120_000, PX_A);
    handle.shutdown_and_join();

    let out = looks.lock().unwrap().clone();
    // Fixture check FIRST, and it is what separates the two ways this can fail. The BAR lane's
    // per-symbol read is already correct and already gated above, so if the seed landed at all the
    // last bar look shows it. Zero HERE means the seed never applied (a fixture fault); zero on the
    // fill lane alone means the read resolved against the wrong engine, which is the subject.
    let last_bar = last_of(&out, "bar");
    assert_eq!(
        last_bar.pos_c, POS_C,
        "fixture: the {LEG_C} seed must reach {VENUE}'s engine, or the assertion below cannot \
         distinguish a routing bug from an empty book. looks={out:#?}"
    );

    let l = last_of(&out, "fill");
    assert_eq!(
        l.pos_c, POS_C,
        "position({LEG_C}) read during a {VENUE_B} fill must come from {VENUE}'s book — the \
         mount's own venue, which is what `MountLeg::same_venue` MEANS. {} means the leg resolved \
         against the dispatching venue instead: {l:?}",
        l.pos_c
    );

    // The WRITE half, and the one that moves real money.
    let maker_sent: Vec<String> =
        maker.submissions.lock().unwrap().iter().map(|(_, s)| s.clone()).collect();
    let hedge_sent: Vec<String> =
        hedge.submissions.lock().unwrap().iter().map(|(_, s)| s.clone()).collect();
    assert!(
        maker_sent.iter().any(|s| s == LEG_C),
        "the {LEG_C} order must route to {VENUE}, the mount's own venue: \
         maker={maker_sent:?} hedge={hedge_sent:?}"
    );
    assert!(
        !hedge_sent.iter().any(|s| s == LEG_C),
        "...and NOT to {VENUE_B} — routing a declared same-venue leg's order to the venue a fill \
         happened to arrive from sends a REAL order to an exchange the mount never named: \
         hedge={hedge_sent:?}"
    );
}

/// ⚠ **AN UNATTRIBUTED EVENT ON A DECLARED CROSS-VENUE LEG REACHED NO STRATEGY** — in BOTH lanes.
///
/// When no mount minted an order's coid, `dispatch_applied_fills` and `dispatch_order_events` each
/// fell back to a `(venue, symbol)` scan. The comment above each names exactly what reaches it —
/// "an operator ticket, a liquidation, an adopted venue order" — but the scan compared against the
/// MOUNT'S OWN series:
///
/// ```text
///     m.venue == f.venue && m.symbol == f.symbol
/// ```
///
/// On a cross-venue mount that can never match a declared leg, so the `else { continue; }` dropped
/// the event entirely. A LIQUIDATION on the hedge venue — the case a hedging strategy most needs to
/// hear about — reached `on_fill` in neither lane, with no error and no ring line.
///
/// ⚠ The predicate was written out TWICE and was wrong identically in both, which is the same shape
/// as the three bugs fixed before it. `CoreThread::mount_owning` is now the single spelling, and it
/// resolves a leg's venue through `leg_venue` rather than adding a fourth.
///
/// NON-VACUOUS: the fill carries an EMPTY coid, so `mount_for_coid` misses by construction and the
/// fallback is the only path that can deliver it — which is what makes this test about the fallback
/// rather than about attribution. Its symbol/venue are the DECLARED LEG's, never the mount's own, so
/// the pre-fix scan cannot match. FAILS on the pre-fix code with no `"fill"` lane at all.
#[test]
fn an_unattributed_fill_on_a_declared_leg_reaches_the_mount() {
    let looks = Arc::new(Mutex::new(Vec::new()));
    let probe = Probe::new(&looks, vec![]);
    let maker = wires();
    let hedge = wires();

    let handle = spawn_core_multi(
        engine(VENUE, LEG_A, maker.client),
        vec![(1_000_000.0, engine(VENUE_B, LEG_B, hedge.client))],
        config(mount(vec![MountLeg::at(LEG_B, VENUE_B)], probe)),
    );

    close_bar(&handle, LEG_A, 60_000, PX_A);
    // A fill on the DECLARED leg's venue that NO mount minted — an empty coid is the shape a
    // liquidation, an operator ticket or a reconcile-adopted order arrives with.
    handle
        .event_sender()
        .blocking_send(Event::Fill(fill("", VENUE_B, LEG_B, -1, -POS_B, PX_B)))
        .unwrap();
    close_bar(&handle, LEG_A, 120_000, PX_A);
    handle.shutdown_and_join();

    let out = looks.lock().unwrap().clone();
    let lanes: Vec<&str> = out.iter().map(|l| l.lane).collect();
    assert!(
        lanes.contains(&"fill"),
        "an unattributed fill on the declared leg ({VENUE_B}/{LEG_B}) must reach `on_fill` through \
         the ownership fallback — a liquidation on a hedge venue is exactly this shape, and \
         dropping it leaves the strategy believing it still holds the leg. lanes={lanes:?}"
    );
    let l = last_of(&out, "fill");
    assert_eq!(l.pos_b, POS_B, "and it must be delivered with the leg's own position: {l:?}");
}
