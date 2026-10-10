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
