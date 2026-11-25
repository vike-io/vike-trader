//! Which registry orders a publish carries: every order that has not ended, and an ended one for
//! [`ENDED_ORDER_PUBLISH_MS`] after a publish first found it ended (the owner's yes of 2026-11-19), let go
//! together with the others past their hour at most once every [`ENDED_ORDER_SWEEP_MS`].
//!
//! The engine registries never evict: the order FSM, reconcile, the risk gate, the core's refusals and
//! the journal read them whole, and nothing here touches them. Only what is PUBLISHED is bounded, so a
//! publish costs the orders the node holds open and those that ended lately, never every order it ended
//! since it started (the CI box on 2026-11-19: 2,023 orders after 3 h 20 m, 2,021 of them FILLED, each copied
//! into every publish).
//!
//! [`OrderWindows`] keeps, per engine, the registry indices still published. A publish walks those and
//! the orders registered since the last one, never the rest: an engine's registry only grows (no order
//! is removed or moved), so an index names one order for the life of the process.
//!
//! **It keeps the published row of each too, and a publish rebuilds only the rows whose order moved** (the owner's yes
//! of 2026-11-20): every other row is the `Arc` the last publish carried, so a publish allocates and frees a row per
//! CHANGED order, plus one list of pointers. An order moved when its registry write stamp did
//! (`vike_execution::execution_engine::OrderRegistry::write_stamp`: another registry in the engine's place moves every
//! one), or, for a refused or modifiable one, when the reason (with its refusal id) or the price of a modify in flight
//! read off the engine's side tables did (`super::build::order_rejection`, `order_pending_price`: those move without a
//! registry write). Measured
//! before it on the CI box's build: a list of 2,002 orders cost every publish 195 µs to build and 131 µs to free.
//!
//! ⚠ **An order made live again over its own key keeps its index** (a re-registration after a reconcile ended
//! it): a publish takes every LIVE order the window let go back in, from the registry's live index
//! (`vike_execution::execution_engine::OrderRegistry::live_positions`), so the desktop, the CLI and the MCP never
//! lose an order that is live. A merge of two ascending lists, no allocation while nothing came back.
//!
//! ⚠ **The hour is counted on the CORE clock** ([`OrderWindows::clock`], the reading `CoreThread::dispatch` takes of
//! each message), never on an engine's `now_ms`: a strategy's order stamps its engine with the time of the event that
//! caused it (a fill's, a bar's), which in a paper, scripted or replayed run is another clock, so an hour counted across
//! the two was over at once (the cross-venue hedge of `crates/vike-tradehub-core/tests/scripted/xemm_scripted.rs` left
//! the list the moment it filled).

use std::sync::Arc;

use vike_execution::{Engine, ExecutionClient, OrderView};
use vike_model::OrderStatus;
use vike_model::accounts::account_keys::AccountLabel;

/// How long an ENDED order stays on the published list after a publish first found it ended: an hour for a
/// person or a script to read how it ended; the registry, the journal and the closed-trade archive keep it after.
pub const ENDED_ORDER_PUBLISH_MS: i64 = vike_model::MS_PER_HOUR;

/// How far past its hour the oldest ended order gets before every one past its hour goes, together: each order
/// after one the list lets go moves up a place, and a patched observer is sent each of them again, so at most
/// once a minute rather than once per order.
const ENDED_ORDER_SWEEP_MS: i64 = vike_model::MS_PER_MINUTE;

/// **The published orders of each engine** ([`super::build`]'s), primary first, in the order the engines are
/// handed to it: the core only ever appends an engine. The core keeps one across its publishes; a fresh one
/// publishes every order of every registry and starts each ended order's hour at the first publish with a clock.
#[derive(Debug, Default)]
pub struct OrderWindows {
    engines: Vec<OrderWindow>,
    /// The core clock's reading of the message folded last ([`Self::clock`]); `None` before the first, when no ended
    /// order's hour starts and none ends.
    now_ms: Option<i64>,
}

/// One engine's published orders.
#[derive(Debug, Default)]
struct OrderWindow {
    /// How many registry entries this window has taken in.
    taken: usize,
    /// The registry orders still published, ascending by index.
    kept: Vec<Kept>,
    /// The venue and the account every kept row was built with: a row names both, so one that moved rebuilds them all.
    venue: String,
    account: Option<AccountLabel>,
}

/// One published registry order.
#[derive(Debug)]
struct Kept {
    index: usize,
    /// The core clock's reading at the publish that first found the order ended (`None` while it has not).
    ended: Option<i64>,
    /// The row the last publish carried and the registry write stamp it was built at; `None` until a publish builds it.
    row: Option<((u64, u64), Arc<OrderView>)>,
}

impl Kept {
    fn new(index: usize) -> Self {
        Kept { index, ended: None, row: None }
    }

    /// Bring the row up to date with its order, rebuilding it only when the order moved (the module doc); the order's
    /// status, `None` when the registry holds no order at the index.
    fn refresh<C: ExecutionClient>(
        &mut self,
        engine: &Engine<C>,
        account: &Option<AccountLabel>,
    ) -> Option<OrderStatus> {
        let Some(stamp) = engine.registry.write_stamp(self.index) else {
            self.row = None;
            return None;
        };
        if let Some((built, row)) = &self.row
            && *built == stamp
            && (row.reason.as_deref(), row.refusal_id.as_deref())
                == super::build::order_rejection(engine, &row.order_client_id, row.status)
                    .map_or((None, None), |(reason, refusal_id)| (Some(reason), refusal_id))
            && row.pending_price
                == super::build::order_pending_price(engine, &row.order_client_id, row.status)
        {
            return Some(row.status);
        }
        let row = super::build::order_view(engine, account, self.index)?;
        let status = row.status;
        self.row = Some((stamp, Arc::new(row)));
        Some(status)
    }
}

impl OrderWindows {
    /// The core clock's reading of the message being folded: what an ended order's hour is counted on.
    pub fn clock(&mut self, now_ms: i64) {
        self.now_ms = Some(now_ms);
    }

    /// Push onto `out` the rows the `at`-th engine publishes, in registry order, its account `account`: its window
    /// brought up to date as of [`Self::clock`] (the orders registered since the last publish taken in, the rows of
    /// those that moved rebuilt, an ended one's hour started, the ended ones past their hour let go once the oldest is
    /// [`ENDED_ORDER_SWEEP_MS`] past it).
    pub(crate) fn publish<C: ExecutionClient>(
        &mut self,
        at: usize,
        engine: &Engine<C>,
        account: &Option<AccountLabel>,
        out: &mut Vec<Arc<OrderView>>,
    ) {
        if self.engines.len() <= at {
            self.engines.resize_with(at + 1, OrderWindow::default);
        }
        let now = self.now_ms;
        let window = &mut self.engines[at];
        let registry = &engine.registry;
        // A registry that shrank is no core's (a test assigns the field): take it in again.
        if registry.len() < window.taken {
            *window = OrderWindow::default();
        }
        if window.venue != engine.venue || window.account != *account {
            window.venue.clone_from(&engine.venue);
            window.account.clone_from(account);
            window.kept.iter_mut().for_each(|kept| kept.row = None);
        }
        window.kept.extend((window.taken..registry.len()).map(Kept::new));
        window.taken = registry.len();
        readmit_live(&mut window.kept, registry.live_positions());
        let past = |now: i64, ended: Option<i64>, by: i64| {
            ended.is_some_and(|at| now.saturating_sub(at) >= ENDED_ORDER_PUBLISH_MS + by)
        };
        let sweep = now.is_some_and(|now| {
            window.kept.iter().any(|k| past(now, k.ended, ENDED_ORDER_SWEEP_MS))
        });
        window.kept.retain_mut(|kept| {
            let Some(status) = kept.refresh(engine, account) else { return now.is_none() };
            let Some(now) = now else { return true };
            kept.ended = status.is_terminal().then_some(kept.ended.unwrap_or(now));
            !(sweep && past(now, kept.ended, 0))
        });
        out.reserve(window.kept.len());
        out.extend(
            window.kept.iter().filter_map(|kept| kept.row.as_ref().map(|(_, row)| Arc::clone(row))),
        );
    }
}

/// Put back into the ascending `kept` every one of the ascending `live` positions it lacks: an order the window let
/// go after it ended, made live again since. One pass over both lists.
/// shortcut: a record written straight to `Liquidated` (not live, not ended) over a let-go key is not taken back;
/// no write site does that today, so upgrade when one does.
fn readmit_live(kept: &mut Vec<Kept>, live: impl Iterator<Item = usize>) {
    let mut at = 0;
    for index in live {
        while kept.get(at).is_some_and(|k| k.index < index) {
            at += 1;
        }
        if kept.get(at).is_none_or(|k| k.index != index) {
            kept.insert(at, Kept::new(index));
        }
        at += 1;
    }
}

#[cfg(test)]
#[path = "order_window_tests.rs"]
mod order_window_tests;
