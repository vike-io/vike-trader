//! N per-symbol paper books behind one `ExecutionClient`: `ExecutionClientPaperMulti`.

use std::collections::VecDeque;

use vike_execution::ExecutionClient;
use vike_marketdata::Bar;
use vike_model::OrderRequest;
use vike_model::events::{Event, OrderSubmitted, TradeId};

use crate::client::ExecutionClientPaper;

/// Phase D remainder: N per-symbol paper books behind ONE `ExecutionClient` — the
/// multi-symbol paper twin. Each book keeps `ExecutionClientPaper`'s single-symbol
/// engine-primitive fill semantics untouched (r7 law); this wrapper only ROUTES:
/// submits by `request.symbol` (a miss synthesizes the terminal `OrderRejected` — the
/// dead-path rule: no order silently vanishes), bars by `bar.symbol` (None = every book,
/// the single-book compatibility case), cancels/modifies broadcast (books no-op silently
/// on unknown order client ids). One thing it rewrites: a book's fill is named `paper-<symbol>-<n>`, not the
/// book's own `paper-<n>` (every book counts from one, and the books share one account, which
/// refuses a fill under a trade id it already folded).
///
/// **Combos are NOT routed here** (combo PR-3): a combo's `symbol` is empty and its legs span
/// several instruments, so there is no single book to route it to. It therefore falls into the
/// no-book arm below and is terminally `OrderRejected` — loudly, per the dead-path rule, never
/// silently dropped or half-routed to one leg's book. Drive a combo through a single
/// `ExecutionClientPaper` fed symbol-tagged bars for every leg instead; that book prices legs from
/// `leg_marks` regardless of its own `symbol`.
#[derive(Default)]
pub struct ExecutionClientPaperMulti {
    /// (symbol, book) in registration order — few books, linear scan, no map dep
    books: Vec<(String, ExecutionClientPaper)>,
    events: VecDeque<Event>,
}

impl ExecutionClientPaperMulti {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one per-symbol book (keyed by its `symbol`; re-adding replaces).
    pub fn add_book(&mut self, book: ExecutionClientPaper) {
        let symbol = book.symbol.clone();
        if let Some(slot) = self.books.iter_mut().find(|(s, _)| *s == symbol) {
            slot.1 = book;
        } else {
            self.books.push((symbol, book));
        }
    }

    pub fn book(&self, symbol: &str) -> Option<&ExecutionClientPaper> {
        self.books.iter().find(|(s, _)| s == symbol).map(|(_, b)| b)
    }
}

impl ExecutionClient for ExecutionClientPaperMulti {
    fn submit(&mut self, request: &OrderRequest) {
        match self.books.iter_mut().find(|(s, _)| *s == request.symbol).map(|(_, b)| b) {
            Some(book) => book.submit(request),
            None => {
                // emitter-split rule: a dead route must synthesize a terminal
                self.events.push_back(Event::OrderSubmitted(OrderSubmitted {
                    order_client_id: request.order_client_id.clone(),
                    timestamp: request.timestamp,
                }));
                self.events.push_back(Event::OrderRejected(vike_model::events::OrderRejected {
                    cause: None,
                    order_client_id: request.order_client_id.clone(),
                    reason: format!("no paper book for symbol {}", request.symbol).into(),
                    timestamp: request.timestamp,
                }));
            }
        }
    }

    fn cancel(&mut self, order_client_id: &str) {
        for (_, book) in self.books.iter_mut() {
            book.cancel(order_client_id); // no-op + no event on books that don't hold it
        }
    }

    fn modify(&mut self, order: &OrderRequest, new_qty: Option<f64>, new_price: Option<f64>) {
        if let Some((_, book)) = self.books.iter_mut().find(|(s, _)| *s == order.symbol) {
            book.modify(order, new_qty, new_price);
        }
    }

    /// Routed to the order's book like `modify`, by hand: the trait's default would refuse every
    /// trigger amend here and shadow the book's own.
    fn modify_trigger(
        &mut self,
        order: &OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
        new_trigger_price: f64,
    ) -> Result<(), &'static str> {
        match self.books.iter_mut().find(|(s, _)| *s == order.symbol) {
            Some((_, book)) => book.modify_trigger(order, new_qty, new_price, new_trigger_price),
            None => Ok(()), // no book holds it, so nothing rests to move: `modify`'s no-op
        }
    }

    /// The same declaration its per-symbol books make — this client IS N of them, and routing an
    /// amend to one of them does not change what that amend means. Spelled out rather than
    /// inherited from the trait default, for the reason the boxed `ExecutionClient` impl gives:
    /// a default would silently shadow the book's own override.
    fn amend_semantics(&self) -> Option<vike_venues::AmendSemantics> {
        Some(vike_venues::AmendSemantics::InPlaceRemaining)
    }

    fn poll_events(&mut self) -> Option<Event> {
        if let Some(event) = self.events.pop_front() {
            return Some(event);
        }
        for (symbol, book) in self.books.iter_mut() {
            if let Some(mut event) = book.poll_events() {
                // Every book counts its fills from one; the books share ONE account, which refuses a
                // fill under a trade id it already folded, so the second book's fills vanished.
                if let Event::Fill(fill) = &mut event
                    && let Some(n) = fill.trade_id.as_str().strip_prefix("paper-")
                {
                    let named = TradeId::prefixed("paper-", format!("{symbol}-{n}"));
                    fill.trade_id = named;
                }
                return Some(event);
            }
        }
        None
    }

    fn on_bar(&mut self, bar: &Bar) {
        match &bar.symbol {
            Some(symbol) => {
                if let Some((_, book)) = self.books.iter_mut().find(|(s, _)| s == symbol) {
                    book.on_bar(bar);
                }
            }
            // symbol-less bar: the single-book compatibility case — every book sees it
            None => {
                for (_, book) in self.books.iter_mut() {
                    book.on_bar(bar);
                }
            }
        }
    }

    /// Phase-one teardown seam, fanned out to every book — the same shape `detach` below already
    /// has. Spelled out for the reason `amend_semantics` above gives: this wrapper delegates every
    /// other method, and a wrapper that delegates `detach` but inherits the trait's `begin_detach`
    /// no-op is exactly the shape `crates/vike-execution/src/execution_engine/client.rs`'s
    /// `ExecutionClient::begin_detach` warns about. Today every book is a `ExecutionClientPaper`,
    /// whose own `begin_detach` is the default no-op (an in-process book owns no thread and has no
    /// flag to raise), so this buys nothing yet — it costs nothing either, and the delegation is
    /// what keeps the trap closed the day a book grows a thread.
    fn begin_detach(&mut self) {
        for (_, book) in self.books.iter_mut() {
            book.begin_detach();
        }
    }

    fn detach(&mut self) {
        for (_, book) in self.books.iter_mut() {
            book.detach();
        }
    }
}
