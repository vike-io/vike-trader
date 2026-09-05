//! The CROSS-VENUE reference-quote lane — the inbound half of cross-exchange trading, and the
//! prerequisite an xEMM could not be built without.
//!
//! #997 routed cross-venue ORDERS (`multi_symbol_routing.rs`'s
//! `a_leg_declared_on_another_venue_routes_there`, titled "THE xEMM UNBLOCK"). It read
//! `MountLeg::venue` at exactly ONE site — `resolve_intent_venue`, on the OUTBOUND path. Every
//! inbound predicate gated on `m.venue == <the event's venue>` BEFORE consulting a declared leg, so
//! a leg declared `MountLeg::at(sym, other_venue)` had its orders routed there and NEVER received
//! that venue's quotes, books, bars or marks. A cross-exchange maker prices its resting quotes off
//! the OTHER venue's touch, so it had no input at all.
//!
//! `Strategy::on_reference_quote` + `CoreThread::drive_strategy_reference_quote` are that input.
//! The tests below pin the four things that make the lane usable and the two that make it safe:
//!
//! 1. a foreign venue's QUOTE reaches a mount that declared it (R-1), and so does a foreign venue's
//!    BOOK — as its derived L1, through the SAME hook (R-2);
//! 2. the lane is DISJOINT from `on_quote_tick`: a foreign tick never reaches it, and an own-venue
//!    tick never reaches `on_reference_quote` (R-3);
//! 3. an order buffered FROM a reference tick routes to the MOUNT's venue, not the tick's (R-4) —
//!    the safety property the whole design rests on;
//! 4. a tagged quote placed on a reference tick is CANCELLABLE from an own-venue tick (R-5) — the
//!    tag registry keys identically from both lanes;
//! 5. a symbol-carrying hedge submitted from a reference tick still reaches the FOREIGN venue's
//!    engine (R-6);
//! 6. an undeclared venue's quote reaches nobody (R-7), and a runtime with no foreign leg never
//!    enters the lane at all (R-8).

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use vike_core::{CoreConfig, LiveBroker, MountLeg, StrategyMount, spawn_core, spawn_core_multi};
use vike_exec::{
    Account, BalanceMode, BookUpdate, ExecutionClient, ExecutionEngine, QuoteUpdate, RiskGate,
    RiskLimits,
};
use vike_model::{Broker, HftBroker, L2Book, OrderRequest, QuoteTick, Strategy};

const VENUE_A: &str = "hyperliquid"; // the MAKER venue — the mount's own
const SYM_A: &str = "BTC";
const VENUE_B: &str = "okx"; // the REFERENCE + hedge venue
const SYM_B: &str = "BTC-USDT-SWAP";

/// What actually reached each venue's `ExecutionClient` — the ONLY place a cancel is observable
/// (a `cancel` publishes nothing; the authoritative `OrderCanceled` comes back from the venue,
/// which a test client does not simulate). Shared so both engines write into one log.
#[derive(Default)]
struct Wire {
    /// `(venue, symbol, side, qty, client_order_id)` per `submit`.
    submits: Vec<(String, String, i32, f64, String)>,
    /// `(venue, client_order_id)` per `cancel`.
    cancels: Vec<(String, String)>,
}

/// A recording [`ExecutionClient`] that tags every call with the venue it was mounted on, so ONE
/// shared log answers "which venue did this order actually reach". `spawn_core_multi` takes ONE
/// concrete client type for every engine, which is why the venue is a field rather than a type.
struct SpyClient {
    venue: &'static str,
    wire: Arc<Mutex<Wire>>,
}

impl ExecutionClient for SpyClient {
    fn submit(&mut self, request: &OrderRequest) {
        self.wire.lock().unwrap().submits.push((
            self.venue.to_string(),
            request.symbol.clone(),
            request.side,
            request.qty,
            request.client_order_id.clone(),
        ));
    }
    fn cancel(&mut self, client_order_id: &str) {
        self.wire
            .lock()
            .unwrap()
            .cancels
            .push((self.venue.to_string(), client_order_id.to_string()));
    }
}

/// Everything the probe observed, shared out of the boxed strategy (a mount OWNS its strategy, so
/// the only way to read it back is a handle the test also holds).
#[derive(Default)]
struct Seen {
    /// `(venue, symbol, bid, ask)` per `on_reference_quote` call.
    reference: Vec<(String, String, f64, f64)>,
    /// `(symbol, bid, ask)` per `on_quote_tick` call.
    own: Vec<(String, f64, f64)>,
}

/// What the probe should DO when a reference quote arrives — one behaviour per test, so each
/// assertion drives exactly the emission it is about.
#[derive(Clone, Copy, PartialEq, Eq)]
enum OnRef {
    /// Observe only.
    Nothing,
    /// Buffer a SYMBOL-LESS market order — the shape a maker's own intent has.
    SymbolLessMarket,
    /// Buffer a TAGGED limit — the maker-quote shape.
    TaggedQuote,
    /// Buffer a symbol-carrying market naming the FOREIGN leg — the hedge shape.
    HedgeOnLegB,
}

struct RefProbe {
    seen: Arc<Mutex<Seen>>,
    on_ref: OnRef,
    /// Set once the probe has emitted, so a repeated tick does not pile up orders.
    emitted: bool,
    /// When true, `on_quote_tick` (the OWN-venue lane) cancels the `"bid"` tag — the cross-lane
    /// tag-registry proof.
    cancel_on_own_tick: bool,
}

impl RefProbe {
    fn new(seen: Arc<Mutex<Seen>>, on_ref: OnRef) -> Self {
        RefProbe { seen, on_ref, emitted: false, cancel_on_own_tick: false }
    }
}

impl Strategy<LiveBroker> for RefProbe {
    fn on_reference_quote(&mut self, broker: &mut LiveBroker, venue: &str, q: &QuoteTick) {
        self.seen.lock().unwrap().reference.push((
            venue.to_string(),
            q.symbol.clone(),
            q.bid,
            q.ask,
        ));
        if self.emitted {
            return;
        }
        match self.on_ref {
            OnRef::Nothing => return,
            OnRef::SymbolLessMarket => Broker::submit_market(broker, "", 1, 1.0),
            OnRef::TaggedQuote => {
                HftBroker::submit_limit_tagged(broker, "bid", 1, 1.0, q.bid - 10.0)
            }
            OnRef::HedgeOnLegB => Broker::submit_market(broker, SYM_B, -1, 2.0),
        }
        self.emitted = true;
    }

    fn on_quote_tick(&mut self, broker: &mut LiveBroker, q: &QuoteTick) {
        self.seen.lock().unwrap().own.push((q.symbol.clone(), q.bid, q.ask));
        if self.cancel_on_own_tick {
            HftBroker::cancel_tagged(broker, "bid");
        }
    }
}

fn engine(
    venue: &'static str,
    symbol: &'static str,
    wire: &Arc<Mutex<Wire>>,
) -> ExecutionEngine<SpyClient> {
    ExecutionEngine::new(
        Account::new(1.0, venue, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        SpyClient { venue, wire: Arc::clone(wire) },
        venue,
        symbol,
    )
}

fn config(strategy: RefProbe, legs: Vec<MountLeg>) -> CoreConfig {
    let t = Arc::new(AtomicI64::new(0));
    CoreConfig {
        seed_cash: 1_000_000.0,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        strategy: Some(StrategyMount {
            account: None,
            venue: VENUE_A.into(),
            symbol: SYM_A.into(),
            interval: "1m".into(),
            strategy: Box::new(strategy),
            symbols: legs,
            underlying_symbol: None,
            controller_id: None,
        }),
        ..CoreConfig::default()
    }
}

fn quote(ts: i64, bid: f64, ask: f64, symbol: &str) -> QuoteTick {
    QuoteTick {
        ts,
        local_ts: 0,
        bid,
        ask,
        bid_size: 3.0,
        ask_size: 4.0,
        symbol: symbol.to_string(),
    }
}

/// A two-level book whose derived L1 is `(bid, ask)` with sizes `(3.0, 4.0)` — the same touch the
/// `quote` helper builds, so R-2 can assert the two lanes deliver an INDISTINGUISHABLE tick.
fn book(bid: f64, ask: f64) -> Arc<L2Book> {
    let mut b = L2Book::new(0.1);
    b.apply_snapshot(1, &[(bid, 3.0), (bid - 1.0, 9.0)], &[(ask, 4.0), (ask + 1.0, 9.0)]);
    Arc::new(b)
}

/// The full two-engine rig: a hyperliquid maker mount declaring an okx leg, both engines live.
/// Returns what the strategy SAW and what actually reached each venue's client.
fn run_two_venue(
    on_ref: OnRef,
    cancel_on_own_tick: bool,
    legs: Vec<MountLeg>,
    drive: impl FnOnce(&vike_core::CoreHandle),
) -> (Seen, Wire) {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let wire = Arc::new(Mutex::new(Wire::default()));
    let mut probe = RefProbe::new(Arc::clone(&seen), on_ref);
    probe.cancel_on_own_tick = cancel_on_own_tick;
    let handle = spawn_core_multi(
        engine(VENUE_A, SYM_A, &wire),
        vec![(1_000_000.0, engine(VENUE_B, SYM_B, &wire))],
        config(probe, legs),
    );
    drive(&handle);
    handle.shutdown_and_join();
    let seen = std::mem::take(&mut *seen.lock().unwrap());
    let wire = std::mem::take(&mut *wire.lock().unwrap());
    (seen, wire)
}

/// `(venue, symbol, side, qty)` of everything that reached a venue — the submits without their
/// (random) coids, which most assertions do not care about.
fn routed(wire: &Wire) -> Vec<(String, String, i32, f64)> {
    wire.submits.iter().map(|(v, s, side, q, _)| (v.clone(), s.clone(), *side, *q)).collect()
}

fn send_quote(handle: &vike_core::CoreHandle, venue: &str, symbol: &str, q: QuoteTick) {
    handle
        .tick_sender()
        .quote(QuoteUpdate { venue: venue.into(), symbol: symbol.into(), quote: q })
        .unwrap();
}

// --- R-1: a foreign venue's QUOTE reaches the declaring mount ---------------------------------

/// The lane's reason to exist: a quote on `(okx, BTC-USDT-SWAP)` reaches a mount on
/// `(hyperliquid, BTC)` that declared that leg, carrying the SOURCE VENUE as an argument (no tick
/// type carries one) and the full touch including sizes.
#[test]
fn a_foreign_venue_quote_reaches_a_mount_that_declared_it() {
    let (seen, _) = run_two_venue(OnRef::Nothing, false, vec![MountLeg::at(SYM_B, VENUE_B)], |h| {
        send_quote(h, VENUE_B, SYM_B, quote(1_000, 100.0, 100.5, SYM_B))
    });
    assert_eq!(
        seen.reference,
        vec![(VENUE_B.to_string(), SYM_B.to_string(), 100.0, 100.5)],
        "the reference touch must arrive tagged with the venue it came from"
    );
    assert!(seen.own.is_empty(), "a foreign tick must NOT also fire the own-venue quote lane");
}

// --- R-2: a foreign venue's BOOK arrives through the same hook, as its derived L1 --------------

/// A CEX-family venue serves depth, not a standalone book-ticker, so the reference touch usually
/// arrives as an `L2Book`. `L2Book` carries neither a venue nor a symbol, so it cannot be
/// attributed — the runtime delivers its DERIVED L1 instead, and that tick must be
/// INDISTINGUISHABLE from the venue's native L1 (same hook, same numbers).
#[test]
fn a_foreign_venue_book_arrives_as_a_derived_l1_on_the_same_hook() {
    let (seen, _) = run_two_venue(OnRef::Nothing, false, vec![MountLeg::at(SYM_B, VENUE_B)], |h| {
        h.tick_sender()
            .book(BookUpdate {
                venue: VENUE_B.into(),
                symbol: SYM_B.into(),
                book: book(100.0, 100.5),
            })
            .unwrap();
    });
    assert_eq!(
        seen.reference,
        vec![(VENUE_B.to_string(), SYM_B.to_string(), 100.0, 100.5)],
        "the book's derived top must reach on_reference_quote as an ordinary touch"
    );
}

// --- R-3: the two lanes are DISJOINT ------------------------------------------------------------

/// One tick, one lane. The own-venue tick predicate requires `m.venue == venue`; the reference
/// predicate requires `!=`. So an own-venue quote fires `on_quote_tick` ONLY, and a foreign one
/// fires `on_reference_quote` ONLY — a mount is never dispatched twice for one tick.
#[test]
fn the_own_and_reference_lanes_are_disjoint() {
    let (seen, _) = run_two_venue(OnRef::Nothing, false, vec![MountLeg::at(SYM_B, VENUE_B)], |h| {
        send_quote(h, VENUE_A, SYM_A, quote(1_000, 90.0, 90.5, SYM_A));
        send_quote(h, VENUE_B, SYM_B, quote(2_000, 100.0, 100.5, SYM_B));
    });
    assert_eq!(
        seen.own,
        vec![(SYM_A.to_string(), 90.0, 90.5)],
        "the own-venue tick fires on_quote_tick exactly once"
    );
    assert_eq!(
        seen.reference,
        vec![(VENUE_B.to_string(), SYM_B.to_string(), 100.0, 100.5)],
        "the foreign tick fires on_reference_quote exactly once"
    );
}

// --- R-4: THE SAFETY PROPERTY -------------------------------------------------------------------

/// An order buffered from a REFERENCE tick routes to the MOUNT's venue, not the tick's.
///
/// This is why the lane drains on the mount's own `(venue, symbol)` rather than the tick's. A
/// maker's own quote is symbol-less (a tagged submit carries `symbol: None` by `HftBroker`
/// contract, and a plain `submit_market("")` means "my mount"), so `resolve_intent_symbol` yields
/// the DRAIN's symbol. Drained on the reference series instead, that same buffered quote would
/// resolve to `SYM_B` and `resolve_intent_venue` would then route it to `VENUE_B` — the maker's
/// own quote placed on the venue it meant to HEDGE on, with no error.
#[test]
fn orders_buffered_from_a_reference_quote_route_to_the_mounts_own_venue() {
    let (_, wire) =
        run_two_venue(OnRef::SymbolLessMarket, false, vec![MountLeg::at(SYM_B, VENUE_B)], |h| {
            send_quote(h, VENUE_B, SYM_B, quote(1_000, 100.0, 100.5, SYM_B))
        });
    assert_eq!(
        routed(&wire),
        vec![(VENUE_A.to_string(), SYM_A.to_string(), 1, 1.0)],
        "a symbol-less intent buffered on a reference tick belongs to the MAKER venue"
    );
}

// --- R-5: the tag registry keys identically from both lanes -------------------------------------

/// A tagged quote PLACED on a reference tick must be CANCELLABLE from an own-venue tick.
///
/// The registry keys on `{mount_idx}|{venue}|{symbol}|{tag}` off the drain arguments. Both lanes
/// drain on the mount's own series, so the insert and the lookup agree by construction — if the
/// reference lane drained on the tick's series they would not, and `cancel_tagged` would become a
/// silent no-op leaving a real order resting at the venue.
#[test]
fn a_tagged_quote_placed_on_a_reference_tick_is_cancellable_from_an_own_tick() {
    let (_, wire) =
        run_two_venue(OnRef::TaggedQuote, true, vec![MountLeg::at(SYM_B, VENUE_B)], |h| {
            send_quote(h, VENUE_B, SYM_B, quote(1_000, 100.0, 100.5, SYM_B));
            send_quote(h, VENUE_A, SYM_A, quote(2_000, 90.0, 90.5, SYM_A));
        });
    assert_eq!(wire.submits.len(), 1, "exactly one maker quote was placed: {:?}", wire.submits);
    let (venue, symbol, _, _, coid) = &wire.submits[0];
    assert_eq!(
        (venue.as_str(), symbol.as_str()),
        (VENUE_A, SYM_A),
        "the tagged quote rests on the MAKER venue"
    );
    assert_eq!(
        wire.cancels,
        vec![(VENUE_A.to_string(), coid.clone())],
        "the OWN-venue lane's cancel_tagged(\"bid\") must resolve to the coid the REFERENCE lane \
         registered — the two lanes key `{{mount}}|{{venue}}|{{symbol}}|{{tag}}` identically. An \
         empty cancel list here means the lookup missed and a real order is left resting."
    );
}

// --- R-6: the hedge leg still reaches the foreign venue ----------------------------------------

/// A SYMBOL-CARRYING order naming the declared foreign leg, buffered from a reference tick, routes
/// to that leg's venue — the hedge half of an xEMM. This is #997's `resolve_intent_venue` working
/// through the new lane: symbol-less intents stay home (R-4), named ones travel.
#[test]
fn a_hedge_naming_the_declared_leg_routes_to_the_foreign_venue() {
    let (_, wire) =
        run_two_venue(OnRef::HedgeOnLegB, false, vec![MountLeg::at(SYM_B, VENUE_B)], |h| {
            send_quote(h, VENUE_B, SYM_B, quote(1_000, 100.0, 100.5, SYM_B))
        });
    assert_eq!(
        routed(&wire),
        vec![(VENUE_B.to_string(), SYM_B.to_string(), -1, 2.0)],
        "a named foreign-leg order routes to that leg's venue and engine"
    );
}

// --- R-7 / R-8: nothing is delivered to a mount that did not ask for it -------------------------

/// A quote from a venue/symbol NO mount declared reaches nobody. `SYM_B` here is declared on
/// `bybit`, so the same symbol arriving from `okx` must not match.
#[test]
fn an_undeclared_venues_quote_reaches_nobody() {
    let (seen, wire) =
        run_two_venue(OnRef::SymbolLessMarket, false, vec![MountLeg::at(SYM_B, "bybit")], |h| {
            send_quote(h, VENUE_B, SYM_B, quote(1_000, 100.0, 100.5, SYM_B))
        });
    assert!(seen.reference.is_empty(), "a leg declared on bybit must not match an okx tick");
    assert!(wire.submits.is_empty(), "and nothing may be ordered off a tick nobody heard");
}

/// A runtime with NO foreign-venue leg never enters the lane: `any_mount_ref` is false, so the two
/// per-market-message call sites are one bool load and `on_reference_quote` is unreachable. This is
/// the byte-identical gate every existing runtime rides — including a mount that declared a
/// SAME-VENUE leg, which must not turn the lane on.
#[test]
fn a_runtime_with_no_foreign_leg_never_reaches_the_reference_lane() {
    for legs in [Vec::new(), vec![MountLeg::same_venue("ETH")]] {
        let seen = Arc::new(Mutex::new(Seen::default()));
        let wire = Arc::new(Mutex::new(Wire::default()));
        let probe = RefProbe::new(Arc::clone(&seen), OnRef::SymbolLessMarket);
        let handle = spawn_core(engine(VENUE_A, SYM_A, &wire), config(probe, legs.clone()));
        // A quote on the mount's OWN venue (the ordinary lane) and one on a venue it never named.
        send_quote(&handle, VENUE_A, SYM_A, quote(1_000, 90.0, 90.5, SYM_A));
        send_quote(&handle, VENUE_B, SYM_B, quote(2_000, 100.0, 100.5, SYM_B));
        handle.shutdown_and_join();
        let seen = seen.lock().unwrap();
        assert!(
            seen.reference.is_empty(),
            "legs {legs:?}: no foreign-venue declaration ⇒ the reference lane is unreachable"
        );
        assert_eq!(seen.own.len(), 1, "legs {legs:?}: the ordinary tick lane is untouched");
    }
}
