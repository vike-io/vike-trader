//! GAPS 3 and 4: non-dispatch lanes read declared legs, and a cross-venue leg reads its own engine.

use vike_exec::StreamStatusUpdate;

use super::*;

// ---------------------------------------------------------------------------------------------
// GAP 3 — the non-dispatch lanes carried EMPTY per-symbol tables
// ---------------------------------------------------------------------------------------------

/// Open both legs on ONE venue, then poke the three lanes that carry no series of their own.
///
/// `declare` is the knob the byte-identity guard flips: with an empty declaration this is an
/// ordinary single-symbol mount and every table must stay empty. Everything else — the bars, the
/// fills, the status poke — is identical between the two runs on purpose, so the guard compares
/// like with like.
fn run_same_venue(declare: bool) -> Vec<Look> {
    let looks = Arc::new(Mutex::new(Vec::new()));
    let symbols = if declare { vec![MountLeg::same_venue(LEG_B)] } else { Vec::new() };
    let probe = Probe::new(
        &looks,
        vec![(60_000, Act::SeedLegs), (120_000, Act::Nothing), (180_000, Act::SubmitUndeclared)],
    );
    let w = wires();
    let handle = spawn_core(engine(VENUE, LEG_A, w.client), config(mount(symbols, probe)));

    // Bar 1 prices leg A and buffers both legs' orders (leg A first, leg B second, in push order).
    close_bar(&handle, LEG_A, 60_000, PX_A);
    // Leg B's own bar prices it in its own band. A declared mount also RECEIVES this one (scripted
    // to do nothing); an undeclared mount does not — but the mark/price-board write happens either
    // way, which is what keeps the two runs comparable.
    close_bar(&handle, LEG_B, 120_000, PX_B);

    let coids = wait_for_submissions(&w.submissions, 2);

    // The venue reports both orders filled — ATTRIBUTED, by the coids just observed. Leg B's fill is
    // what makes the mount's own symbol a NON-dispatching one on the fill lane.
    let ev = handle.event_sender();
    ev.blocking_send(Event::Fill(fill(&coids[0], VENUE, LEG_A, 1, POS_A, PX_A))).unwrap();
    ev.blocking_send(Event::Fill(fill(&coids[1], VENUE, LEG_B, -1, -POS_B, PX_B))).unwrap();

    // A feed-health transition on the mount's OWN pair — no series, no price, just the hook.
    handle
        .tick_sender()
        .stream_status(StreamStatusUpdate {
            venue: VENUE.into(),
            symbol: LEG_A.into(),
            stream: "trade".into(),
            status: FeedStatus::Disconnected,
        })
        .unwrap();

    // Bar 3 asks for an UNDECLARED symbol: refused, and the refusal comes back as a `Denied` order
    // event whose dispatch symbol is neither leg.
    close_bar(&handle, LEG_A, 180_000, PX_A + 1.0);
    handle.shutdown_and_join();

    looks.lock().unwrap().clone()
}

/// **THE `on_fill` GATE.** A declared mount's `position`/`price` inside `on_fill` must answer about
/// the symbol NAMED, not about the symbol that filled.
///
/// FAILS ON THE PRE-CHANGE CODE: `dispatch_applied_fills` built its `LiveBroker` with
/// `positions: Vec::new(), prices: Vec::new()`, so on leg B's fill `position(LEG_A)` fell through to
/// the `af.position_after` scalar and returned **leg B's** signed size (`POS_B` — opposite sign AND
/// a different magnitude), while `price(LEG_A)` returned leg B's price, outside leg A's band.
///
/// `on_fill` is where a hedging strategy decides how much of the other leg it still owes, so this
/// was the wrong number arriving in the one hook whose entire job is inventory.
#[test]
fn on_fill_reads_are_symbol_addressed_on_a_declared_mount() {
    let looks = run_same_venue(true);
    let fills: Vec<&Look> = looks.iter().filter(|l| l.lane == "fill").collect();
    assert_eq!(fills.len(), 2, "both legs' fills must reach the mount: {looks:#?}");
    // The SECOND fill is leg B's — the dispatch whose scalar describes the OTHER instrument.
    let l = fills[1];
    assert_eq!(
        l.pos_a, POS_A,
        "position({LEG_A}) during leg B's fill must be leg A's own ({POS_A}); {} is leg B's, i.e. \
         the answer came from the dispatching scalar: {l:?}",
        l.pos_a
    );
    assert_eq!(l.pos_b, POS_B, "position({LEG_B}) is the filling leg's own: {l:?}");
    assert!(
        (100.0..200.0).contains(&l.px_a),
        "price({LEG_A}) = {} is outside leg A's band — a price answered about leg B: {l:?}",
        l.px_a
    );
    assert!((50.0..100.0).contains(&l.px_b), "price({LEG_B}) stays in leg B's band: {l:?}");
}

/// **THE `on_order_event` GATE**, and the sharpest one: here the dispatching symbol is NEITHER leg
/// (it is the undeclared symbol the order path refused), so the scalar fallback describes an
/// instrument the account holds nothing in.
///
/// FAILS ON THE PRE-CHANGE CODE with `position(LEG_A) == 0.0` and `position(LEG_B) == 0.0`: the
/// tables were empty and the scalar was `position_size_of(SOLUSDT)`. A position manager reacting to
/// a rejection by re-reading its inventory would have seen a FLAT book and re-entered.
#[test]
fn on_order_event_reads_are_symbol_addressed_on_a_declared_mount() {
    let looks = run_same_venue(true);
    let l = last_of(&looks, "order_event");
    assert_eq!(l.pos_a, POS_A, "position({LEG_A}) inside on_order_event: {l:?}");
    assert_eq!(l.pos_b, POS_B, "position({LEG_B}) inside on_order_event: {l:?}");
    assert!((100.0..200.0).contains(&l.px_a), "price({LEG_A}) inside on_order_event: {l:?}");
    assert!((50.0..100.0).contains(&l.px_b), "price({LEG_B}) inside on_order_event: {l:?}");
}

/// **THE `on_feed_status` GATE.** A feed death is precisely when a two-leg strategy asks what it is
/// still holding on the OTHER leg — and that read was the dispatching leg's.
///
/// FAILS ON THE PRE-CHANGE CODE with `position(LEG_B) == POS_A`: the status update names the mount's
/// OWN pair, so the empty table sent `position(ETHUSDT)` to leg A's scalar — a strategy pulling
/// quotes on a disconnect saw itself LONG 3 of a leg it is SHORT 7 of.
#[test]
fn on_feed_status_reads_are_symbol_addressed_on_a_declared_mount() {
    let looks = run_same_venue(true);
    let l = last_of(&looks, "feed_status");
    assert_eq!(
        l.pos_b, POS_B,
        "position({LEG_B}) inside on_feed_status must be leg B's ({POS_B}); {} is leg A's: {l:?}",
        l.pos_b
    );
    assert_eq!(l.pos_a, POS_A, "position({LEG_A}) inside on_feed_status: {l:?}");
    assert!((50.0..100.0).contains(&l.px_b), "price({LEG_B}) inside on_feed_status: {l:?}");
}

/// ⚠ THE BYTE-IDENTITY GUARD for every lane above: a mount that declared NOTHING must still see
/// EMPTY tables, i.e. every per-symbol read answers with the dispatching scalar whatever symbol is
/// named — even a symbol the ENGINE genuinely holds a position in.
///
/// It passes before and after the change, on purpose: it is not the regression proof, it is the
/// acceptance condition. It FAILS on the tempting over-reach — building the tables from the ENGINE's
/// symbols instead of from `StrategyMount::symbols`, or dropping `declared_views`' early return —
/// because the account here really does hold leg B (the second injected fill names it), so
/// `position(LEG_B)` would start returning `POS_B` on a leg-A dispatch where the contract says it
/// must return leg A's number.
#[test]
fn an_undeclared_mount_still_reads_only_the_dispatch_scalar() {
    let looks = run_same_venue(false);
    for l in &looks {
        assert_eq!(
            l.pos_a, l.pos_b,
            "an undeclared mount answers EVERY symbol with the dispatch scalar, so two reads that \
             name different symbols cannot differ: {l:?}"
        );
        assert_eq!(
            l.px_a, l.px_b,
            "same for price — a per-symbol table must not exist on this mount at all: {l:?}"
        );
    }
    assert!(
        looks.iter().any(|l| l.lane == "fill"),
        "the sweep must observe a NON-bar lane, else it holds vacuously over the one lane that \
         always had tables: {looks:#?}"
    );
    assert!(
        looks.iter().any(|l| l.lane == "feed_status"),
        "...and the feed-status lane too: {looks:#?}"
    );
}

// ---------------------------------------------------------------------------------------------
// GAP 4 — a declared leg resolved against the DISPATCHING venue's engine
// ---------------------------------------------------------------------------------------------

/// **THE CROSS-VENUE GATE.** A leg declared with `MountLeg::at(sym, other_venue)` must report THAT
/// venue's book.
///
/// FAILS ON THE PRE-CHANGE CODE: `push_view` read every leg out of `self.eng(eidx)` — the
/// DISPATCHING venue's engine, i.e. the maker's — so `position(LEG_B)` was `0.0` (the maker engine
/// holds no hedge position) and `price(LEG_B)` was leg A's bar close (the maker engine cannot price
/// the hedge symbol at all, so no price row was written and the read fell through to the dispatch
/// scalar). An xEMM maker reading its hedge inventory through the `Broker` seam therefore saw itself
/// permanently unhedged; `vike_mm::xemm` works around it by folding the hedge leg from its own
/// `on_fill` stream, which is why the defect survived this long.
///
/// It is also the byte-identity companion for the SAME-venue case: a `MountLeg::same_venue` leg
/// still resolves the mount's own engine, which the `on_fill`/`on_feed_status` gates above exercise
/// (they read a same-venue leg and expect the mount engine's numbers).
#[test]
fn a_cross_venue_leg_reads_its_own_venues_engine() {
    let looks = Arc::new(Mutex::new(Vec::new()));
    let probe = Probe::new(&looks, Vec::new());
    let maker = wires();
    let hedge = wires();

    let handle = spawn_core_multi(
        engine(VENUE, LEG_A, maker.client),
        vec![(1_000_000.0, engine(VENUE_B, LEG_B, hedge.client))],
        config(mount(vec![MountLeg::at(LEG_B, VENUE_B)], probe)),
    );

    // The hedge venue's book: a position and a price, BOTH only on venue B's engine. The maker
    // engine is left holding nothing in leg B and pricing nothing in it, so an answer of `0.0` or of
    // leg A's price is unambiguously the maker engine's.
    handle
        .event_sender()
        .blocking_send(Event::Fill(fill("", VENUE_B, LEG_B, -1, -POS_B, PX_B)))
        .unwrap();
    close_bar_on(&handle, VENUE_B, LEG_B, 60_000, PX_B);
    // Now the MAKER venue's own bar — the dispatch the probe reads from.
    close_bar(&handle, LEG_A, 60_000, PX_A);
    handle.shutdown_and_join();

    let out = looks.lock().unwrap().clone();
    let l = last_of(&out, "bar");
    assert_eq!(
        l.pos_b, POS_B,
        "position({LEG_B}) must be the HEDGE venue's ({POS_B}); {} is the maker engine's book: \
         {l:?}",
        l.pos_b
    );
    assert!(
        (50.0..100.0).contains(&l.px_b),
        "price({LEG_B}) = {} must come from {VENUE_B}'s price board; leg A's band means the maker \
         engine answered — or could not, and the dispatch scalar did: {l:?}",
        l.px_b
    );
    // The mount's own leg is untouched by any of this.
    assert_eq!(l.pos_a, 0.0, "the maker leg was never filled: {l:?}");
    assert!((100.0..200.0).contains(&l.px_a), "price({LEG_A}) is the maker bar's close: {l:?}");
}
