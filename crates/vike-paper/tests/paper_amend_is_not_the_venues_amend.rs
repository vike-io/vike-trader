//! **A PAPER MOUNT CARRIES THE REAL VENUE STRING**, so the pre-trade gate's per-venue amend netting
//! must not be keyed on that string alone.
//!
//! `vike_mount::make_engine` builds `ExecutionEngine::new(Account::new(1.0, venue, …), …, venue,
//! symbol)` with the REAL venue id even when the absent-credentials gate fell back to
//! [`vike_paper::PaperExecutionClient`], and `vike_run::build_paper_maker_core` does the same with
//! its profile's `venue`. A paper mount on binance/okx/bybit therefore resolves
//! `vike_model::AmendSemantics::InPlaceTotal` from the venue table — a value that tells the gate the
//! amend's quantity is the order's new TOTAL and that `filled_qty` of it has already executed.
//!
//! That is FALSE for this book. `PaperExecutionClient::modify` assigns `resting.size = q`: the
//! amend's quantity becomes the order's REMAINING size, and `WorkingOrder` carries no execution
//! history to subtract, so the whole `q` is what the next `on_bar` will fill. Netting there would
//! let the gate assume `q − filled_qty` while the book executes `q` — an under-statement of
//! projected exposure and an under-charge of margin, in the crate whose entire purpose is that
//! "backtest == paper == live" is a checkable law.
//!
//! The correction is `ExecutionClient::amend_semantics`, which this client overrides with
//! [`vike_model::AmendSemantics::InPlaceRemaining`]; `ExecutionEngine::modify_order` asks the CLIENT
//! first and the venue table only when it declines. These tests drive that whole path with the REAL
//! paper client and the REAL engine, because the finding was precisely that reading the two halves
//! separately (an engine that nets, a client that does not) looks correct on both sides.

use vike_exec::{Account, BalanceMode, EventBus, ExecutionEngine, Outbox, RiskGate, RiskLimits};
use vike_model::events::{Event, FillEvent, OrderAccepted, OrderPartiallyFilled, OrderSubmitted};
use vike_model::{AmendSemantics, OrderRequest};
use vike_paper::{MultiPaperExecutionClient, PaperExecutionClient};

/// A venue whose declared row is `InPlaceTotal` — i.e. the one class of venue whose amend netting is
/// non-zero, and therefore the only class where a paper mount could be mis-judged.
const VENUE: &str = "binance";
const SYMBOL: &str = "BTCUSDT";
const PX: f64 = 100.0;
const RESTING_QTY: f64 = 10.0;
const EXECUTED: f64 = 4.0;
/// Chosen so the two readings land on OPPOSITE sides: netting projects `|4 + (10 − 4)| × 100 = 1000`
/// and admits, while the honest paper reading projects `|4 + 10| × 100 = 1400` and refuses.
const EXPOSURE_CAP: f64 = 1_200.0;

fn limit(coid: &str, qty: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        side: 1,
        qty,
        order_type: "limit".into(),
        price: Some(PX),
        ts: 1,
        ..Default::default()
    }
}

fn fill(coid: &str, qty: f64) -> FillEvent {
    FillEvent {
        trade_id: "t1".into(),
        client_order_id: coid.to_string(),
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        side: 1,
        last_qty: qty,
        last_px: PX,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "maker".to_string().into(),
        ts: 0,
        mark_price: Some(PX),
        position_side: "BOTH".into(),
    }
}

/// The mount `make_engine` builds when binance credentials are absent: the paper book behind the
/// `ExecutionClient` seam, and the REAL venue string on the engine and the account.
fn paper_mount_on_a_real_venue() -> ExecutionEngine<PaperExecutionClient> {
    ExecutionEngine::new(
        Account::new(1.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits { max_total_exposure: Some(EXPOSURE_CAP), ..RiskLimits::new() }),
        PaperExecutionClient::new(VENUE, SYMBOL, 0.0, 0.0, 0.0),
        VENUE,
        SYMBOL,
    )
}

/// Rest the order through the real submit path, then deliver one partial execution exactly as a
/// venue lane does — the bare `Event::Fill` the account folds into the position plus the
/// `OrderPartiallyFilled` wrap the FSM folds into `filled_qty`.
fn rest_and_partly_fill(e: &mut ExecutionEngine<PaperExecutionClient>) {
    let mut outbox = Outbox::default();
    e.submit_order(&limit("c1", RESTING_QTY), 0, &mut outbox);
    let mut bus = EventBus::new();
    bus.publish(Event::OrderSubmitted(OrderSubmitted { client_order_id: "c1".into(), ts: 0 }), e);
    bus.publish(
        Event::OrderAccepted(OrderAccepted {
            client_order_id: "c1".into(),
            venue_order_id: Some("v1".into()),
            ts: 1,
        }),
        e,
    );
    bus.publish(Event::Fill(fill("c1", EXECUTED)), e);
    bus.publish(
        Event::OrderPartiallyFilled(OrderPartiallyFilled {
            client_order_id: "c1".into(),
            fill: fill("c1", EXECUTED),
            ts: 2,
        }),
        e,
    );
    assert_eq!(e.position_size("BOTH"), EXECUTED, "precondition: the executed part is position");
    assert_eq!(
        e.registry.get("c1").expect("resting").filled_qty,
        EXECUTED,
        "precondition: and the same quantity is the order's filled_qty"
    );
}

/// THE FINDING, end to end. Same engine, same venue string, same partially filled order: the amend
/// must be judged on its WHOLE quantity, because that is what this book will execute.
///
/// Without the client's declaration the engine reads `amend_semantics("binance") == InPlaceTotal`,
/// nets the 4 executed lots out of the 10, projects 1000 against a 1200 cap and ADMITS — while the
/// paper book would happily rest all 10 on top of a position of 4.
#[test]
fn a_paper_mount_on_an_in_place_venue_is_still_judged_on_the_whole_amend_qty() {
    let mut e = paper_mount_on_a_real_venue();
    rest_and_partly_fill(&mut e);

    let mut outbox = Outbox::default();
    e.modify_order("c1", Some(RESTING_QTY), None, 5, &mut outbox);

    let reason = outbox.0.iter().find_map(|ev| match ev {
        Event::OrderModifyRejected(r) => Some(r.reason.to_string()),
        _ => None,
    });
    assert!(
        reason.as_deref().is_some_and(|r| r.contains("over-max-exposure")),
        "a paper book executes the whole amend qty, so the exposure lane must judge the whole qty \
         — got {reason:?}"
    );
}

/// The complement, and the reason the assertion above is not vacuous: the SAME engine admits the
/// amend once it is genuinely small enough under the honest reading. Without this a broken gate
/// that refuses everything would pass the test above.
#[test]
fn the_same_paper_mount_admits_an_amend_that_fits_under_the_whole_qty_reading() {
    let mut e = paper_mount_on_a_real_venue();
    rest_and_partly_fill(&mut e);

    // |4 + 8| × 100 = 1200, exactly at the cap.
    let mut outbox = Outbox::default();
    e.modify_order("c1", Some(8.0), None, 5, &mut outbox);
    assert!(
        !outbox.0.iter().any(|ev| matches!(ev, Event::OrderModifyRejected(_))),
        "an amend whose WHOLE qty fits under the cap is admitted"
    );
}

/// The declaration itself, and the behaviour it describes, asserted together — so the two cannot
/// drift. `modify` puts the amend's quantity back on the book as the size that will fill; that is
/// [`AmendSemantics::InPlaceRemaining`] and not either venue convention.
#[test]
fn the_paper_book_declares_the_convention_its_modify_implements() {
    use vike_exec::ExecutionClient;

    let mut c = PaperExecutionClient::new(VENUE, SYMBOL, 0.0, 0.0, 0.0);
    assert_eq!(
        c.amend_semantics(),
        Some(AmendSemantics::InPlaceRemaining),
        "the paper book is not the venue it is mounted under"
    );
    assert_eq!(
        AmendSemantics::InPlaceRemaining.already_in_position(EXECUTED),
        0.0,
        "…and that convention nets nothing"
    );

    // …which is exactly what `modify` does: a BUY limit resting at 10 becomes an order for 2 — the
    // whole 2 still to execute, with nothing deducted.
    c.submit(&limit("c1", RESTING_QTY));
    c.modify(&limit("c1", RESTING_QTY), Some(2.0), None);
    let bar = vike_model::Bar {
        ts: 10,
        open: PX - 5.0,
        high: PX,
        low: PX - 5.0,
        close: PX - 5.0,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    };
    c.on_bar(&bar);
    let filled: f64 = std::iter::from_fn(|| c.poll_events())
        .filter_map(|e| match e {
            Event::Fill(f) => Some(f.last_qty),
            _ => None,
        })
        .sum();
    assert_eq!(filled, 2.0, "the whole amend qty executes — nothing was treated as already done");
}

/// The multi-symbol book is N of the same books, so it declares the same thing. Spelled as its own
/// assertion because `MultiPaperExecutionClient` implements the trait separately and a trait DEFAULT
/// would silently shadow the per-book override (the same trap the boxed `ExecutionClient` impl
/// documents).
#[test]
fn the_multi_symbol_paper_book_declares_the_same_convention() {
    use vike_exec::ExecutionClient;

    let mut multi = MultiPaperExecutionClient::new();
    multi.add_book(PaperExecutionClient::new(VENUE, SYMBOL, 0.0, 0.0, 0.0));
    assert_eq!(multi.amend_semantics(), Some(AmendSemantics::InPlaceRemaining));

    // …and through the boxed seam the cross-venue runtime actually mounts it behind.
    let boxed: Box<dyn ExecutionClient + Send> =
        Box::new(PaperExecutionClient::new(VENUE, SYMBOL, 0.0, 0.0, 0.0));
    assert_eq!(
        boxed.amend_semantics(),
        Some(AmendSemantics::InPlaceRemaining),
        "the boxed impl must delegate, not fall back to the trait default"
    );
}
