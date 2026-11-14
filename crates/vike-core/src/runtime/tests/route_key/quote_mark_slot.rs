//! The ACCOUNT mark slot follows a fresher quote as the price board does (decision 0221's amended
//! verdict 2): a venue mark nothing refreshed for `DEFAULT_MARK_STALENESS_MS` yields to the quote, and
//! one inside that window keeps the slot, the published `marks` row and the gate's `mark_price`.
//!
//! Two engine shapes, each engine 0 so its slot is the published `marks` list: the venue's DEFAULT
//! account, whose slot takes the quote's mid through `drive_strategy_tick`, and a LABELLED account
//! admitted the symbol with no default account beside it, which hears the quote only through
//! `fan_out_quote`. The second took the quote on its board alone, so its slot kept a stale mark.

use super::*;
use vike_execution::DEFAULT_MARK_STALENESS_MS;

const STALE: f64 = 3000.0;
const BID: f64 = 3100.0;
const ASK: f64 = 3102.0;
const MARKED_AT: i64 = 1_000;

/// Engine 0 on a clock the test steps, its gate judging a limit order against a 1% collar, and the
/// symbol under test: binance's default account on [`SYMBOL`] (`fan_out == false`), or the labelled
/// [`ALT`] admitted [`ETH`] with no default account (`fan_out == true`).
fn core_on_a_clock(
    fan_out: bool,
) -> (CoreThread<RecordingClient>, Arc<std::sync::atomic::AtomicI64>, &'static str) {
    let clock = Arc::new(std::sync::atomic::AtomicI64::new(MARKED_AT));
    let read = Arc::clone(&clock);
    let route_key = if fan_out { ALT } else { ACCOUNT_A };
    let mut core = core_of(
        engine_with_account_venue(CANON, route_key, CANON, SYMBOL),
        Vec::new(),
        CoreConfig {
            clock: Box::new(move || read.load(std::sync::atomic::Ordering::Relaxed)),
            ..CoreConfig::default()
        },
    );
    core.engine_mut(0).gate.limits.price_collar =
        Some(vike_model::PriceCollar { percent: 0.01, abs_floor: 0.0 });
    if !fan_out {
        return (core, clock, SYMBOL);
    }
    admit(&mut core, ALT);
    assert_eq!(core.admitted_fanout, vec![0], "PREMISE: ALT hears ETH only through the fan-out");
    (core, clock, ETH)
}

/// A venue mark of [`STALE`] at [`MARKED_AT`], written into engine 0's slot and board the way a fill's
/// `mark_price` is (`Engine`'s fill fold).
fn venue_mark(core: &mut CoreThread<RecordingClient>, symbol: &str) {
    let e = core.engine_mut(0);
    assert!(e.account.set_mark_from(CANON, symbol, STALE, MarkSource::VenueMark, MARKED_AT));
    e.price_board.set_mark(CANON, symbol, STALE, MARKED_AT);
}

/// A [`BID`]/[`ASK`] quote stamped `at` on a core clock at `at`, then a publish.
fn quote_at(
    core: &mut CoreThread<RecordingClient>,
    clock: &std::sync::atomic::AtomicI64,
    symbol: &str,
    at: i64,
) {
    clock.store(at, std::sync::atomic::Ordering::Relaxed);
    core.dispatch(Ingest::Quote(Box::new(vike_execution::QuoteUpdate {
        venue: CANON.into(),
        symbol: symbol.into(),
        quote: vike_marketdata::TickQuote {
            timestamp: at,
            local_timestamp: at,
            bid: BID,
            ask: ASK,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: symbol.into(),
        },
    })));
    core.publish_guarded();
}

/// Whether engine 0's gate admits an opening limit buy at `px`: the 1% collar is judged at the gate's
/// `mark_price`.
fn gate_admits(
    core: &mut CoreThread<RecordingClient>,
    symbol: &str,
    px: f64,
    order_client_id: &str,
) -> bool {
    let now = core.engine(0).now_ms;
    let req = OrderRequest {
        order_client_id: order_client_id.into(),
        venue: CANON.into(),
        symbol: symbol.into(),
        side: 1,
        qty: 0.5,
        order_type: vike_model::OrderType::Limit,
        price: Some(px),
        timestamp: now,
        ..Default::default()
    };
    let mut out = EventBusOutbox::default();
    core.engine_mut(0).submit_order(&req, now, &mut out);
    !out.0.iter().any(|event| matches!(event, Event::OrderDenied(_)))
}

/// What engine 0's slot and the published `marks` row hold for `symbol`.
fn slot_and_published(
    core: &CoreThread<RecordingClient>,
    symbol: &str,
) -> (Option<f64>, Option<f64>) {
    (core.engine(0).account.mark_of(CANON, symbol), core.snapshot.load().last_mark(CANON, symbol))
}

/// **A mark older than the window yields to the quote everywhere it is read.** The slot and the
/// published row take the quote's mid; the gate collars at the long's side, the bid. The fan-out shape
/// failed here before `fan_out_quote` wrote the slot: it kept 3000 while its board read the bid.
#[test]
fn a_venue_mark_older_than_the_window_yields_the_account_slot_to_a_fresher_quote() {
    for fan_out in [false, true] {
        let (mut core, clock, symbol) = core_on_a_clock(fan_out);
        venue_mark(&mut core, symbol);
        quote_at(&mut core, &clock, symbol, MARKED_AT + DEFAULT_MARK_STALENESS_MS + 1);
        let mid = (BID + ASK) / 2.0;
        assert_eq!(slot_and_published(&core, symbol), (Some(mid), Some(mid)), "fan_out={fan_out}");
        assert!(
            gate_admits(&mut core, symbol, BID, "at-quote"),
            "fan_out={fan_out}: judged at the bid"
        );
        assert!(
            !gate_admits(&mut core, symbol, STALE, "at-mark"),
            "fan_out={fan_out}: not at the mark"
        );
    }
}

/// **A mark inside the window keeps the slot, the published row and the gate**, as today: the quote
/// lands on the board only, under the mark.
#[test]
fn a_venue_mark_inside_the_window_keeps_the_account_slot_and_the_gate() {
    for fan_out in [false, true] {
        let (mut core, clock, symbol) = core_on_a_clock(fan_out);
        venue_mark(&mut core, symbol);
        quote_at(&mut core, &clock, symbol, MARKED_AT + DEFAULT_MARK_STALENESS_MS);
        assert_eq!(
            slot_and_published(&core, symbol),
            (Some(STALE), Some(STALE)),
            "fan_out={fan_out}"
        );
        assert!(
            gate_admits(&mut core, symbol, STALE, "at-mark"),
            "fan_out={fan_out}: judged at the mark"
        );
        assert!(
            !gate_admits(&mut core, symbol, BID, "at-quote"),
            "fan_out={fan_out}: not at the quote"
        );
    }
}
