use super::*;
use vike_core::LiveBroker;
use vike_marketdata::test_support::quote;
use vike_model::{AsParams, HorizonMode, KappaMode, OrderEventKind, QuoteStyle};

/// A bare in-crate `LiveBroker` at a given inventory + EVENT ts, empty order buffers —
/// mirrors `lib.rs`'s own test helper of the same shape (this module can't reach that
/// private helper, so it gets its own minimal copy).
fn broker(position: f64, now: i64) -> LiveBroker {
    LiveBroker {
        positions: Vec::new(),
        prices: Vec::new(),
        bar_views: Vec::new(),
        position,
        price: 0.0,
        equity: 0.0,
        bars: std::sync::Arc::new(Vec::new()),
        index: 0,
        now,
        multiplier: 1.0,
        lot_size: 0.0,
        submissions: Vec::new(),
        modifications: Vec::new(),
        cancels: Vec::new(),
        brackets: Vec::new(),
        conditionals: Vec::new(),
        mass_cancel: false,
    }
}

fn trade(ts: i64, price: f64, size: f64) -> TradeTick {
    TradeTick { ts, local_ts: 0, price, size, is_buyer_maker: false, symbol: String::new() }
}

fn a_fill(side: i32, size: f64, ts: i64) -> Fill {
    Fill { side, size, price: 100.0, fee: 0.0, ts, is_maker: true, symbol: String::new() }
}

/// A maker with BOTH the fill-rate breaker and A-S enabled — the fixed config the roundtrip
/// test builds two instances of (the "driven" maker and the "fresh, restart-simulating" one
/// that loads its saved state — a real mount always reconstructs from the same config first).
fn maker_with_breaker_and_as() -> SpreadMaker {
    SpreadMaker::new(1.0, 0.5)
        .with_fill_breaker(1_000, 2.5, 5_000)
        .with_avellaneda_stoikov(AsParams::default())
}

// The full round trip (portfolio-observer PR-4 T4): drive a maker until BOTH the breaker has
// tripped a side AND the A-S estimator has warmed accumulators, save it, load it into a FRESH
// same-config maker (as a restart would construct before load_state), and confirm both the
// breaker deadline and the A-S accumulators — but NOT the params/alpha, which come from the
// fresh config either way — land bit-for-bit.
#[test]
fn spreadmaker_state_roundtrips_breaker_and_as() {
    let mut m = maker_with_breaker_and_as();

    // two priced quote ticks seed sigma2/last_mid/last_quote_mid (the first only seeds
    // last_mid; the second computes the first sigma2 sample from the mid delta over dt).
    m.on_quote_tick(&mut broker(0.0, 100), &quote(100, 0.40, 0.42));
    m.on_quote_tick(&mut broker(0.0, 200), &quote(200, 0.41, 0.43));
    // a trade AFTER the first priced tick folds into the kappa-MLE running sums.
    m.on_trade_tick(&mut broker(0.0, 200), &trade(210, 0.415, 5.0));

    // three same-side (bid) fills inside the window trip the breaker's bid suppression.
    for t in [110, 120, 130] {
        m.on_fill(&mut broker(0.0, t), &a_fill(1, 1.0, t));
    }

    assert_ne!(m.bid_suppressed_until(), 0, "precondition: the breaker tripped");
    assert!(m.as_sigma2().is_some(), "precondition: sigma2 seeded by the second priced tick");
    let (sum_w, _sum_w_delta, n_trades) = m.as_accumulators().expect("A-S is enabled");
    assert!(sum_w > 0.0, "precondition: the trade tick folded into sum_w");
    assert_eq!(n_trades, 1, "precondition: one trade recorded");

    let saved = Strategy::<LiveBroker>::save_state(&m).expect("SpreadMaker always saves Some");

    // a FRESH maker, same config — as a restart would construct at mount, before load_state
    let mut m2 = maker_with_breaker_and_as();
    assert_eq!(m2.bid_suppressed_until(), 0, "precondition: fresh maker starts untripped");
    assert_eq!(m2.as_sigma2(), None, "precondition: fresh maker's estimator starts cold");

    Strategy::<LiveBroker>::load_state(&mut m2, &saved);

    assert_eq!(m2.bid_suppressed_until(), m.bid_suppressed_until(), "breaker deadline restored");
    assert_eq!(m2.ask_suppressed_until(), m.ask_suppressed_until(), "untripped side stays 0");
    assert_eq!(m2.as_sigma2(), m.as_sigma2(), "sigma2 accumulator restored");
    assert_eq!(m2.as_accumulators(), m.as_accumulators(), "kappa-MLE sums + trade tape restored");
}

// A future/foreign version tag must be ignored (fail-open): no panic, the maker keeps its
// freshly-constructed state rather than misapplying a payload shape it doesn't recognize.
#[test]
fn spreadmaker_load_ignores_version_mismatch() {
    let mut m = maker_with_breaker_and_as();
    Strategy::<LiveBroker>::load_state(&mut m, &serde_json::json!({"v": 999}));
    assert_eq!(m.bid_suppressed_until(), 0, "fail-open: deadlines stay at constructed defaults");
    assert_eq!(m.ask_suppressed_until(), 0, "fail-open: deadlines stay at constructed defaults");
    assert_eq!(m.as_sigma2(), None, "fail-open: A-S estimator stays cold");
}

// A maker with A-S DISABLED loading a payload that DOES carry an as_state (e.g. saved by a
// differently-configured maker) must restore the breaker deadlines but silently skip the A-S
// restore — never turning A-S on as a side effect of loading state.
#[test]
fn spreadmaker_load_without_as_skips_as_restore() {
    let mut m = SpreadMaker::new(1.0, 0.5).with_fill_breaker(1_000, 2.5, 5_000);
    assert!(!m.as_enabled(), "precondition: A-S is off on this config");

    let payload = serde_json::json!({
        "v": 1,
        "bid_suppressed_until": 777,
        "ask_suppressed_until": 0,
        "as_state": {
            "sigma2": 0.001,
            "last_mid": [0.41, 200],
            "last_quote_mid": 0.41,
            "trades": [[0.01, 5.0, 210]],
            "sum_w": 5.0,
            "sum_w_delta": 0.05
        }
    });
    Strategy::<LiveBroker>::load_state(&mut m, &payload);

    assert_eq!(m.bid_suppressed_until(), 777, "the breaker deadline still restores");
    assert!(!m.as_enabled(), "A-S stays off — the payload's as_state is silently skipped");
}

/// A maker with A-S in [`KappaMode::OwnFillFit`] mode. The fixed config the wiring tests build.
fn own_fill_fit_maker() -> SpreadMaker {
    SpreadMaker::new(1.0, 0.5)
        .with_avellaneda_stoikov(AsParams {
            kappa_mode: KappaMode::OwnFillFit,
            ..AsParams::default()
        })
        .with_fill_breaker(1_000, 2.5, 5_000)
}

// OwnFillFit WIRING: a fill folds a FILLED own outcome, and a subsequent breaker PULL folds a
// CENSORED one — the two live sources that feed the censored-hazard κ MLE.
#[test]
fn own_fill_fit_records_fills_and_breaker_pulls() {
    let mut m = own_fill_fit_maker();
    // a priced quote tick places the bid and sets its placement clock + the A-S fair mid.
    m.on_quote_tick(&mut broker(0.0, 100), &quote(100, 0.40, 0.42));
    // a bid fill folds a FILLED own outcome (and the 5.0 same-side size trips the bid breaker).
    m.on_fill(&mut broker(0.0, 110), &a_fill(1, 5.0, 110));
    let (len1, nfill1, _) = m.own_fill_stats().expect("A-S enabled");
    assert_eq!((len1, nfill1), (1, 1), "the fill recorded one FILLED own outcome");
    // the next tick: the tripped breaker PULLS the bid, folding a CENSORED own outcome.
    m.on_quote_tick(&mut broker(0.0, 120), &quote(120, 0.40, 0.42));
    let (len2, nfill2, _) = m.own_fill_stats().expect("A-S enabled");
    assert_eq!((len2, nfill2), (2, 1), "the breaker pull recorded one CENSORED own outcome");
}

// OFF / byte-identical: a maker NOT selecting OwnFillFit (here the DEFAULT Fixed κ, A-S on)
// never touches the own-fill tape — the whole wiring is inert, so its behavior is unchanged. It
// drives the exact fill + breaker-pull sequence the OwnFillFit test does; nothing is recorded.
#[test]
fn non_own_fill_fit_maker_never_records_own_outcomes() {
    let mut m = SpreadMaker::new(1.0, 0.5)
        .with_avellaneda_stoikov(AsParams::default()) // KappaMode::Fixed, the default
        .with_fill_breaker(1_000, 2.5, 5_000);
    m.on_quote_tick(&mut broker(0.0, 100), &quote(100, 0.40, 0.42));
    m.on_fill(&mut broker(0.0, 110), &a_fill(1, 5.0, 110));
    m.on_quote_tick(&mut broker(0.0, 120), &quote(120, 0.40, 0.42));
    assert_eq!(
        m.own_fill_stats(),
        Some((0, 0, None)),
        "Fixed κ never records own outcomes — the OwnFillFit wiring is inert"
    );
}

// --- the order-DEATH lane: a dead quote frees its side, so the next tick PLACES ---------------

/// A plain single-quote maker on the 0.01 grid — no breaker, no A-S, no ladder, no tolerance.
/// The exact default shape the live the CI box mount runs, so the wedge below is the live one.
fn plain_maker() -> SpreadMaker {
    SpreadMaker::new(1.0, 0.01)
}

/// `(tag, price)` of every SUBMIT on a driven broker.
fn submits(b: &LiveBroker) -> Vec<(String, f64)> {
    b.submissions
        .iter()
        .map(|s| (s.tag.clone().unwrap_or_default(), s.price.unwrap_or(f64::NAN)))
        .collect()
}

/// The tags of every MODIFY on a driven broker.
fn modify_tags(b: &LiveBroker) -> Vec<String> {
    b.modifications.iter().map(|m| m.tag.clone()).collect()
}

/// A terminal lifecycle event naming one of the maker's own tags.
fn terminal(tag: &str, kind: OrderEventKind) -> OrderLifecycle {
    OrderLifecycle { client_order_id: "coid-1".into(), tag: Some(tag.into()), kind }
}

// ⚠ THE WEDGE. A FULL FILL kills the resting bid, but nothing used to clear `SideState::placed`
// — so the next tick took the modify arm, the runtime resolved `"bid"` to the now-terminal
// coid, and `ExecutionEngine::modify_order` returned silently. That side never quoted again.
// Drive exactly that sequence and assert the next tick SUBMITS the bid rather than modifying it.
#[test]
fn a_fully_filled_quote_is_resubmitted_not_modified_on_the_next_tick() {
    let mut m = plain_maker();

    // tick 1: a two-sided quote goes on the book.
    let mut b1 = broker(0.0, 100);
    m.on_quote_tick(&mut b1, &quote(100, 0.40, 0.42));
    assert_eq!(submits(&b1).len(), 2, "precondition: both sides placed on the first tick");
    assert!(modify_tags(&b1).is_empty(), "precondition: nothing to modify yet");

    // the bid fills COMPLETELY: the fill itself, then the order's death by tag.
    m.on_fill(&mut broker(1.0, 110), &a_fill(1, 1.0, 110));
    m.on_order_event(&mut broker(1.0, 110), &terminal("bid", OrderEventKind::Filled));

    // tick 2 at a MOVED book, so a still-resting side genuinely wants a re-price.
    let mut b2 = broker(1.0, 120);
    m.on_quote_tick(&mut b2, &quote(120, 0.41, 0.43));

    let submitted: Vec<String> = submits(&b2).into_iter().map(|(t, _)| t).collect();
    assert!(
        submitted.contains(&"bid".to_string()),
        "the dead bid must be SUBMITTED again, got submits {submitted:?} / modifies {:?}",
        modify_tags(&b2)
    );
    assert!(
        !modify_tags(&b2).contains(&"bid".to_string()),
        "the dead bid must NOT be re-priced — that is the silent no-op that wedged the mount"
    );
    // ...and the surviving ask must still take the WORKING modify lane, untouched.
    assert_eq!(modify_tags(&b2), vec!["ask".to_string()], "the resting ask still re-prices");
    assert!(
        !submitted.contains(&"ask".to_string()),
        "the resting ask must NOT be re-submitted (that would orphan it)"
    );
}

// The other two death modes reach the same slot through the same hook: an externally-originated
// CANCEL, and a REJECT of a submit the place arm had already marked `placed`. Each frees only
// its own side.
#[test]
fn a_canceled_or_rejected_quote_also_frees_its_side() {
    for (tag, kind) in [
        ("bid", OrderEventKind::Canceled { reason: "venue pull".into() }),
        ("ask", OrderEventKind::Rejected { reason: "post-only would cross".into() }),
        ("bid", OrderEventKind::Denied { reason: "risk: min_qty".into() }),
        ("ask", OrderEventKind::Expired),
    ] {
        let mut m = plain_maker();
        m.on_quote_tick(&mut broker(0.0, 100), &quote(100, 0.40, 0.42));
        m.on_order_event(&mut broker(0.0, 110), &terminal(tag, kind.clone()));

        let mut b = broker(0.0, 120);
        m.on_quote_tick(&mut b, &quote(120, 0.41, 0.43));
        let submitted: Vec<String> = submits(&b).into_iter().map(|(t, _)| t).collect();
        assert_eq!(submitted, vec![tag.to_string()], "{kind:?} on {tag} must re-place it");
        let other = if tag == "bid" { "ask" } else { "bid" };
        assert_eq!(
            modify_tags(&b),
            vec![other.to_string()],
            "{kind:?} on {tag} must leave the OTHER side on the modify lane"
        );
    }
}

// INERT for everything that is not this maker's own dead single quote: a non-terminal Accept, a
// LADDER rung tag (`"bid0"` — a prefix of `"bid"`'s tag, and explicitly out of scope), and an
// UNTAGGED event. Each must leave both sides resting, i.e. the next tick still modifies both.
#[test]
fn non_terminal_foreign_and_untagged_events_change_nothing() {
    for ev in [
        terminal("bid", OrderEventKind::Accepted),
        terminal("bid0", OrderEventKind::Filled),
        terminal("hedge", OrderEventKind::Canceled { reason: String::new() }),
        OrderLifecycle {
            client_order_id: "coid-9".into(),
            tag: None,
            kind: OrderEventKind::Filled,
        },
    ] {
        let mut m = plain_maker();
        m.on_quote_tick(&mut broker(0.0, 100), &quote(100, 0.40, 0.42));
        m.on_order_event(&mut broker(0.0, 110), &ev);

        let mut b = broker(0.0, 120);
        m.on_quote_tick(&mut b, &quote(120, 0.41, 0.43));
        assert!(b.submissions.is_empty(), "{ev:?} must place nothing");
        assert_eq!(
            modify_tags(&b),
            vec!["bid".to_string(), "ask".to_string()],
            "{ev:?} must leave BOTH sides on the working modify lane"
        );
    }
}

// --- restart: the resting quote is PERSISTED, so the maker continues it ----------------------
//
// After a restart the core puts the previous session's resting orders (and their tags) back, so
// a maker that boots with `placed = false` would submit a SECOND bid/ask under the tag the first
// one still rests under — overwriting the core's tag entry and orphaning the old order.

/// A maker that quoted both sides at ts 100, saved, and a FRESH same-config maker that loaded it —
/// the shape of a real restart (`(restored, original)`).
fn restart_pair() -> (SpreadMaker, SpreadMaker) {
    let mut before = plain_maker();
    before.on_quote_tick(&mut broker(0.0, 100), &quote(100, 0.40, 0.42));
    let saved = Strategy::<LiveBroker>::save_state(&before).expect("always Some");
    let mut after = plain_maker();
    Strategy::<LiveBroker>::load_state(&mut after, &saved);
    (after, before)
}

// The new fields round-trip: `placed`, the intended `(price, size)` the refresh logic compares
// against, and the placement clock. A fresh maker would hold none of them.
#[test]
fn a_resting_quote_roundtrips_through_save_and_load() {
    let (after, before) = restart_pair();
    for is_bid in [true, false] {
        let (a, b) = (after.side(is_bid), before.side(is_bid));
        assert!(b.placed && b.own.is_some(), "precondition: the original maker rests both sides");
        assert!(a.placed, "placed restored (is_bid={is_bid})");
        assert_eq!(a.own, b.own, "intended (price, size) restored (is_bid={is_bid})");
        assert_eq!(a.quoted_ts, b.quoted_ts, "placement clock restored (is_bid={is_bid})");
        assert!(a.refresh_stale, "a restored snapshot is stale: a partial fill may have landed");
        assert_eq!(a.unverified, Some(0), "restored but not yet confirmed alive");
    }
}

// A state file written BEFORE this field existed (the shape in the wild) still loads: the breaker
// deadline restores and both sides come up unplaced, exactly as they always did.
#[test]
fn json_of_the_old_shape_loads_with_both_sides_unplaced() {
    let mut m = plain_maker();
    let old = serde_json::json!({
        "v": 1,
        "bid_suppressed_until": 777,
        "ask_suppressed_until": 0,
        "as_state": null
    });
    Strategy::<LiveBroker>::load_state(&mut m, &old);
    assert_eq!(m.bid_suppressed_until(), 777, "the old fields still restore");
    for is_bid in [true, false] {
        let s = m.side(is_bid);
        assert!(!s.placed && s.own.is_none() && s.quoted_ts == 0, "unplaced (is_bid={is_bid})");
        assert_eq!(s.unverified, None, "not a restored side (is_bid={is_bid})");
        assert!(!s.refresh_stale);
    }
    // ...and its first tick quotes as a fresh maker does.
    let mut b = broker(0.0, 100);
    m.on_quote_tick(&mut b, &quote(100, 0.40, 0.42));
    assert_eq!(submits(&b).len(), 2, "an unplaced side submits on its first tick");
}

// A `placed = true` that carries no usable quote (hand-edited / half-written) is not trusted:
// there is nothing to compare a refresh against, so the side stays unplaced.
#[test]
fn a_placed_flag_without_a_quote_is_not_restored() {
    for bid in [
        serde_json::json!({"placed": true}),
        serde_json::json!({"placed": true, "own": [0.4, 0.0], "quoted_ts": 5}),
        serde_json::json!({"placed": false, "own": [0.4, 1.0], "quoted_ts": 5}),
    ] {
        let mut m = plain_maker();
        let payload = serde_json::json!({
            "v": 1, "bid_suppressed_until": 0, "ask_suppressed_until": 0,
            "as_state": null, "bid": bid
        });
        Strategy::<LiveBroker>::load_state(&mut m, &payload);
        assert!(!m.bid.placed && m.bid.own.is_none() && m.bid.unverified.is_none(), "{bid}");
    }
}

// THE POINT: a restored resting side does NOT submit on its first tick (that would orphan the old
// order). It takes the modify arm — by tag, which the core resolves through its restored registry —
// and it does so even with a wide refresh tolerance, because the persisted snapshot is only the
// INTENDED quote and a partial fill may have landed while the daemon was down.
#[test]
fn a_restored_resting_side_does_not_requote_on_its_first_tick() {
    let (mut after, _) = restart_pair();
    let mut b = broker(0.0, 5_000);
    after.on_quote_tick(&mut b, &quote(5_000, 0.40, 0.42));
    assert!(b.submissions.is_empty(), "no NEW quote while the old one rests: {:?}", submits(&b));
    assert!(b.cancels.is_empty(), "nothing is pulled on the first tick");
    assert_eq!(modify_tags(&b), vec!["bid".to_string(), "ask".to_string()], "re-priced by tag");

    // the same under a wide tolerance, where an UNCHANGED target would otherwise be skipped
    let mut tol = plain_maker().with_refresh_tolerance(1_000.0, 1_000.0);
    let mut before = broker(0.0, 100);
    tol.on_quote_tick(&mut before, &quote(100, 0.40, 0.42));
    let saved = Strategy::<LiveBroker>::save_state(&tol).expect("always Some");
    let mut tol2 = plain_maker().with_refresh_tolerance(1_000.0, 1_000.0);
    Strategy::<LiveBroker>::load_state(&mut tol2, &saved);
    let mut b2 = broker(0.0, 200);
    tol2.on_quote_tick(&mut b2, &quote(200, 0.40, 0.42));
    assert!(b2.submissions.is_empty());
    assert_eq!(modify_tags(&b2).len(), 2, "stale snapshot bypasses the tolerance skip once");
}

// A tagged TERMINAL event for the restored order (the core stamps `tag` from its restored registry)
// clears the side exactly as it does for a live one; the next tick re-quotes it, and the other
// side is untouched.
#[test]
fn a_tagged_terminal_event_after_restore_clears_the_side_and_the_next_tick_requotes() {
    let (mut after, _) = restart_pair();
    after.on_order_event(&mut broker(0.0, 200), &terminal("bid", OrderEventKind::Filled));
    assert!(!after.bid.placed && after.bid.own.is_none() && after.bid.unverified.is_none());
    assert!(after.ask.placed, "the other side is untouched");

    let mut b = broker(0.0, 300);
    after.on_quote_tick(&mut b, &quote(300, 0.40, 0.42));
    let submitted: Vec<String> = submits(&b).into_iter().map(|(t, _)| t).collect();
    assert_eq!(submitted, vec!["bid".to_string()], "the freed side re-places");
    assert_eq!(modify_tags(&b), vec!["ask".to_string()], "the surviving restored ask re-prices");
}

// SAFETY NET. A restored `placed = true` whose order is gone with no event (the core's ownership
// entry expired, or the venue dropped it while the daemon was down) must not wedge: the modify arm
// resolves the tag to nothing and is swallowed, forever. After `RESTORED_QUOTE_VERIFY_MS` without
// a sign of life the side is PULLED (cancel by tag — a no-op if the order is gone), and the NEXT
// tick places. The pull and the place are NEVER in one handler call: the core drains submits
// BEFORE cancels, so a same-tick cancel_tagged("bid") would resolve to the NEW order and kill it.
#[test]
fn an_unconfirmed_restored_side_is_pulled_after_the_grace_then_requoted_on_the_next_tick() {
    use crate::quote::RESTORED_QUOTE_VERIFY_MS;
    let (mut after, _) = restart_pair();
    let t0 = 1_000;
    // inside the grace: the quote is continued in place (the clock starts at the first tick)
    for ts in [t0, t0 + 1, t0 + RESTORED_QUOTE_VERIFY_MS - 1] {
        let mut b = broker(0.0, ts);
        after.on_quote_tick(&mut b, &quote(ts, 0.40, 0.42));
        assert!(b.submissions.is_empty() && b.cancels.is_empty(), "grace: ts={ts}");
        assert_eq!(modify_tags(&b).len(), 2, "grace: still continued in place, ts={ts}");
    }
    // grace elapsed: pull both, place nothing, modify nothing
    let ts = t0 + RESTORED_QUOTE_VERIFY_MS;
    let mut pull = broker(0.0, ts);
    after.on_quote_tick(&mut pull, &quote(ts, 0.40, 0.42));
    assert_eq!(pull.cancels, vec!["bid".to_string(), "ask".to_string()], "pulled by tag");
    assert!(pull.submissions.is_empty(), "never cancel and place the same tag in one call");
    assert!(modify_tags(&pull).is_empty());
    assert!(!after.bid.placed && !after.ask.placed && after.bid.unverified.is_none());

    // the next tick places a fresh pair — and from then on it is an ordinary live side
    let mut next = broker(0.0, ts + 1);
    after.on_quote_tick(&mut next, &quote(ts + 1, 0.40, 0.42));
    assert_eq!(submits(&next).len(), 2, "re-placed");
    assert!(next.cancels.is_empty() && next.modifications.is_empty());
    let mut later = broker(0.0, ts + 10 * RESTORED_QUOTE_VERIFY_MS);
    after.on_quote_tick(&mut later, &quote(ts + 10 * RESTORED_QUOTE_VERIFY_MS, 0.41, 0.43));
    assert!(later.cancels.is_empty() && later.submissions.is_empty(), "no further pulls");
    assert_eq!(modify_tags(&later).len(), 2);
}

// A sign of life ends the unverified state: a non-terminal tagged event (the core could resolve
// the tag) or a fill on that side (only one of our orders rests there). The confirmed side is
// then never pulled, however old; the other, still unconfirmed side is.
#[test]
fn a_sign_of_life_confirms_a_restored_side_and_spares_it_the_pull() {
    use crate::quote::RESTORED_QUOTE_VERIFY_MS;
    let (mut after, _) = restart_pair();
    after.on_order_event(&mut broker(0.0, 500), &terminal("bid", OrderEventKind::Accepted));
    after.on_fill(&mut broker(0.0, 500), &a_fill(-1, 0.1, 500));
    assert_eq!(after.bid.unverified, None, "a tagged Accepted confirms the bid");
    assert_eq!(after.ask.unverified, None, "a fill on the ask confirms the ask");
    let mut b = broker(0.0, 500);
    after.on_quote_tick(&mut b, &quote(500, 0.40, 0.42));
    let ts = 500 + 3 * RESTORED_QUOTE_VERIFY_MS;
    let mut late = broker(0.0, ts);
    after.on_quote_tick(&mut late, &quote(ts, 0.40, 0.42));
    assert!(late.cancels.is_empty(), "confirmed sides are never pulled");
    assert_eq!(modify_tags(&late).len(), 2);

    // an unrelated / foreign / untagged event confirms nothing
    let (mut other, _) = restart_pair();
    other.on_order_event(&mut broker(0.0, 500), &terminal("bid0", OrderEventKind::Accepted));
    other.on_order_event(&mut broker(0.0, 500), &terminal("hedge", OrderEventKind::Accepted));
    assert_eq!((other.bid.unverified, other.ask.unverified), (Some(0), Some(0)));
}

// A side that was NOT restored behaves exactly as today: the fresh side submits on its first tick,
// is never pulled for age, and a half-restored maker (bid only) restores only the bid.
#[test]
fn a_side_that_was_not_restored_behaves_as_before() {
    use crate::quote::RESTORED_QUOTE_VERIFY_MS;
    let mut fresh = plain_maker();
    let mut b = broker(0.0, 100);
    fresh.on_quote_tick(&mut b, &quote(100, 0.40, 0.42));
    assert_eq!(submits(&b).len(), 2);
    let ts = 100 + 100 * RESTORED_QUOTE_VERIFY_MS;
    let mut old = broker(0.0, ts);
    fresh.on_quote_tick(&mut old, &quote(ts, 0.40, 0.42));
    assert!(old.cancels.is_empty() && old.submissions.is_empty(), "a live side is never aged out");
    assert_eq!(fresh.bid.unverified, None);

    // only the bid placed in the saved state
    let mut m = plain_maker();
    let payload = serde_json::json!({
        "v": 1, "bid_suppressed_until": 0, "ask_suppressed_until": 0, "as_state": null,
        "bid": {"placed": true, "own": [0.39, 1.0], "quoted_ts": 90}
    });
    Strategy::<LiveBroker>::load_state(&mut m, &payload);
    let mut b = broker(0.0, 100);
    m.on_quote_tick(&mut b, &quote(100, 0.40, 0.42));
    let submitted: Vec<String> = submits(&b).into_iter().map(|(t, _)| t).collect();
    assert_eq!(submitted, vec!["ask".to_string()], "only the unrestored ask is placed");
    assert_eq!(modify_tags(&b), vec!["bid".to_string()], "the restored bid continues");
}

// --- adoption: a tagged live event on a side that holds nothing -------------------------------
//
// The crash window `save_state` cannot close: the maker placed AFTER its last save and the daemon
// died, so the restored maker is unplaced while the core has put the order and its tag back and
// tells the maker through a synthetic tagged `Accepted`. Ignoring it made the first tick submit a
// SECOND order under the tag and orphan the first.

/// One side of `m`, by tag.
fn side_of<'a>(m: &'a SpreadMaker, tag: &str) -> &'a SideState {
    if tag == "bid" { &m.bid } else { &m.ask }
}

// THE POINT: an unplaced maker told a tagged order lives ADOPTS it. Its next tick modifies that tag
// (price and full size, even under a wide tolerance, since the quoted price/size are unknown) and
// submits nothing on that side; the other side, which heard nothing, still places.
#[test]
fn an_unplaced_maker_adopts_a_tagged_accepted_and_modifies_instead_of_submitting() {
    for (tag, other) in [("bid", "ask"), ("ask", "bid")] {
        for tolerance in [None, Some((1_000.0, 1_000.0))] {
            let mut m = plain_maker();
            if let Some((px, qty)) = tolerance {
                m = m.with_refresh_tolerance(px, qty);
            }
            m.on_order_event(&mut broker(0.0, 100), &terminal(tag, OrderEventKind::Accepted));
            let s = side_of(&m, tag);
            assert!(s.placed && s.own.is_none() && s.refresh_stale && s.unverified.is_none());
            assert!(!side_of(&m, other).placed, "the other side heard nothing");

            let mut b = broker(0.0, 200);
            m.on_quote_tick(&mut b, &quote(200, 0.40, 0.42));
            let submitted: Vec<String> = submits(&b).into_iter().map(|(t, _)| t).collect();
            assert_eq!(submitted, vec![other.to_string()], "{tag}/{tolerance:?}: no second {tag}");
            assert_eq!(modify_tags(&b), vec![tag.to_string()], "{tag}/{tolerance:?}: re-priced");
            assert!(b.cancels.is_empty(), "nothing is pulled");
            let s = side_of(&m, tag);
            assert!(s.own.is_some() && !s.refresh_stale, "the modify recorded what it asked for");
        }
    }
}

// Both sides adopted: the first tick re-prices both and submits nothing at all.
#[test]
fn an_unplaced_maker_that_adopts_both_sides_quotes_nothing_new() {
    let mut m = plain_maker();
    m.on_order_event(&mut broker(0.0, 100), &terminal("bid", OrderEventKind::Accepted));
    m.on_order_event(&mut broker(0.0, 100), &terminal("ask", OrderEventKind::Accepted));
    let mut b = broker(0.0, 200);
    m.on_quote_tick(&mut b, &quote(200, 0.40, 0.42));
    assert!(b.submissions.is_empty(), "{:?}", submits(&b));
    assert_eq!(modify_tags(&b), vec!["bid".to_string(), "ask".to_string()]);
}

// A side that IS placed keeps today's behaviour: the event only confirms it alive. Its quoted
// `(price, size)` and freshness are untouched, so a tolerance still skips an unchanged re-price.
#[test]
fn a_tagged_accepted_on_a_placed_side_changes_nothing_but_the_confirmation() {
    let mut m = plain_maker().with_refresh_tolerance(1_000.0, 1_000.0);
    m.on_quote_tick(&mut broker(0.0, 100), &quote(100, 0.40, 0.42));
    let (bid, ask) = (m.bid.own, m.ask.own);
    assert!(bid.is_some() && ask.is_some());
    m.bid.unverified = Some(0); // as a restored side would read
    m.on_order_event(&mut broker(0.0, 110), &terminal("bid", OrderEventKind::Accepted));
    assert_eq!((m.bid.own, m.ask.own), (bid, ask), "the quoted price/size is not forgotten");
    assert!(!m.bid.refresh_stale, "no staleness was invented");
    assert_eq!(m.bid.unverified, None, "confirmed alive");

    let mut b = broker(0.0, 120);
    m.on_quote_tick(&mut b, &quote(120, 0.40, 0.42));
    assert!(b.submissions.is_empty() && b.modifications.is_empty(), "the tolerance still skips");
}

// A tagged terminal after the adoption frees the side like any other death; the next tick places.
#[test]
fn a_tagged_terminal_after_adoption_frees_the_side_and_the_next_tick_places() {
    let mut m = plain_maker();
    m.on_order_event(&mut broker(0.0, 100), &terminal("bid", OrderEventKind::Accepted));
    m.on_order_event(
        &mut broker(0.0, 150),
        &terminal("bid", OrderEventKind::Canceled { reason: "restore".into() }),
    );
    assert!(!m.bid.placed && m.bid.own.is_none() && !m.bid.refresh_stale);

    let mut b = broker(0.0, 200);
    m.on_quote_tick(&mut b, &quote(200, 0.40, 0.42));
    let submitted: Vec<String> = submits(&b).into_iter().map(|(t, _)| t).collect();
    assert_eq!(submitted, vec!["bid".to_string(), "ask".to_string()]);
    assert!(b.modifications.is_empty());
}

// The core refuses a submit over a restored tag with a tagged terminal `Denied`: the place arm had
// already marked the side placed, so the Denied must free it and the next tick must place again.
#[test]
fn a_denied_submit_frees_its_side_and_the_next_tick_places_again() {
    let mut m = plain_maker();
    m.on_quote_tick(&mut broker(0.0, 100), &quote(100, 0.40, 0.42));
    let denied = OrderEventKind::Denied { reason: "restart: a restored order still holds".into() };
    m.on_order_event(&mut broker(0.0, 110), &terminal("bid", denied));
    assert!(!m.bid.placed && m.ask.placed);

    let mut b = broker(0.0, 200);
    m.on_quote_tick(&mut b, &quote(200, 0.40, 0.42));
    let submitted: Vec<String> = submits(&b).into_iter().map(|(t, _)| t).collect();
    assert_eq!(submitted, vec!["bid".to_string()]);
    assert_eq!(modify_tags(&b), vec!["ask".to_string()]);
}

// Adoption covers the SINGLE-quote tags only. A ladder rung (`"bid0"`, `"ask1"`: the tag is the
// rung's index), a foreign tag and an untagged event leave an unplaced maker unplaced.
#[test]
fn adoption_ignores_ladder_foreign_and_untagged_events() {
    for ev in [
        terminal("bid0", OrderEventKind::Accepted),
        terminal("ask1", OrderEventKind::Accepted),
        terminal("hedge", OrderEventKind::Accepted),
        OrderLifecycle {
            client_order_id: "coid-9".into(),
            tag: None,
            kind: OrderEventKind::Accepted,
        },
    ] {
        let mut m = plain_maker();
        m.on_order_event(&mut broker(0.0, 100), &ev);
        assert!(!m.bid.placed && !m.ask.placed, "{ev:?} adopted nothing");

        let mut b = broker(0.0, 200);
        m.on_quote_tick(&mut b, &quote(200, 0.40, 0.42));
        assert_eq!(submits(&b).len(), 2, "{ev:?}: both sides place as on a fresh maker");
        assert!(b.modifications.is_empty());
    }
}

// --- "Option B" cross-symbol underlying routing (on_mark → set_underlying) -------------------

/// A-S params anchored on a 300 s time-to-resolution window closing at T = 300_000, with the
/// underlying blend at `weight` (a `0.0` weight = the blend OFF control). Blackout off + fixed κ
/// so the ONLY thing that moves between the two makers below is the underlying anchor.
fn underlying_as_params(weight: f64) -> AsParams {
    AsParams {
        underlying_weight: weight,
        underlying_beta: 1.0,
        window_secs: 300.0,
        horizon_mode: HorizonMode::TimeToResolution,
        resolution_ts: Some(300_000),
        resolution_blackout_ms: 0,
        gamma: 0.5,
        kappa_mode: KappaMode::Fixed,
        q_scale: 1.0,
        min_standoff_ticks: 1.0,
        ..AsParams::default()
    }
}

/// An A-S maker on the 0.01 grid with the underlying blend at `weight` (its L1 quote lane snaps
/// on `tick_size = 0.01`).
fn underlying_maker(weight: f64) -> SpreadMaker {
    SpreadMaker::new(1.0, 0.01)
        .with_quote_style(QuoteStyle::Mid, 1, 0.01)
        .with_avellaneda_stoikov(underlying_as_params(weight))
}

/// A mark of the underlying series (a DIFFERENT symbol than the token the maker trades).
fn a_mark(price: f64, ts: i64) -> MarkTick {
    MarkTick { symbol: "btcusdt".into(), price, ts }
}

/// The `"bid"`-tagged submit's price on a driven broker.
fn bid_px(b: &LiveBroker) -> f64 {
    b.submissions
        .iter()
        .find(|s| s.tag.as_deref() == Some("bid"))
        .and_then(|s| s.price)
        .expect("a bid submit with a price")
}

// on_mark ROUTES the underlying into the A-S state: the first mark captures s_open only (no σ
// yet ⇒ nothing fed), the second (a real move) seeds σ and feeds the full triple.
#[test]
fn on_mark_feeds_the_underlying_into_the_as_state() {
    let mut m = underlying_maker(0.5);
    assert_eq!(m.as_underlying(), None, "cold: no underlying until a mark warms the tracker");
    m.on_mark(&mut broker(0.0, 150_000), &a_mark(100.0, 150_000));
    assert_eq!(m.as_underlying(), None, "one mark ⇒ s_open only, still no σ ⇒ not fed");
    m.on_mark(&mut broker(0.0, 151_000), &a_mark(100.05, 151_000));
    let (s_now, s_open, sigma) = m.as_underlying().expect("fed after two marks");
    assert_eq!(s_now.to_bits(), 100.05_f64.to_bits(), "s_now is the latest mark");
    assert_eq!(s_open.to_bits(), 100.0_f64.to_bits(), "s_open is the window-open reference");
    assert!(sigma > 0.0, "a real move ⇒ a positive per-second σ, got {sigma}");
}

// OFF / byte-identical: a maker WITHOUT the A-S layer has no `as_state`, so on_mark is a no-op
// (never panics, feeds nothing) — the trait default is a no-op and this override honours it.
#[test]
fn on_mark_is_inert_without_the_as_layer() {
    let mut m = SpreadMaker::new(1.0, 0.01);
    assert!(!m.as_enabled(), "precondition: no A-S layer");
    m.on_mark(&mut broker(0.0, 151_000), &a_mark(100.05, 151_000));
    assert!(!m.as_enabled(), "on_mark never turns A-S on as a side effect");
    assert_eq!(m.as_underlying(), None, "nothing fed");
}

// The routed underlying reaches the PRICING: warm two makers identically with an UP-drift
// underlying and differ ONLY in the blend weight — the weighted maker anchors toward p_up > 0.5,
// so its next quote sits STRICTLY above the book-mid-anchored control's.
#[test]
fn on_mark_blend_shifts_the_next_quote() {
    let mut fed = underlying_maker(0.5);
    let mut ctrl = underlying_maker(0.0);
    // identical warmup (same marks) so only the weight differs.
    fed.on_mark(&mut broker(0.0, 150_000), &a_mark(100.0, 150_000));
    fed.on_mark(&mut broker(0.0, 151_000), &a_mark(100.05, 151_000));
    ctrl.on_mark(&mut broker(0.0, 150_000), &a_mark(100.0, 150_000));
    ctrl.on_mark(&mut broker(0.0, 151_000), &a_mark(100.05, 151_000));
    assert!(fed.as_underlying().is_some() && ctrl.as_underlying().is_some(), "both warmed");
    // one quote tick at a 0.50 book mid; both place a two-sided quote.
    let q = quote(152_000, 0.49, 0.51);
    let mut bf = broker(0.0, 152_000);
    let mut bc = broker(0.0, 152_000);
    fed.on_quote_tick(&mut bf, &q);
    ctrl.on_quote_tick(&mut bc, &q);
    assert!(
        bid_px(&bf) > bid_px(&bc),
        "the underlying blend pulls the bid up: fed {} vs ctrl {}",
        bid_px(&bf),
        bid_px(&bc)
    );
}
