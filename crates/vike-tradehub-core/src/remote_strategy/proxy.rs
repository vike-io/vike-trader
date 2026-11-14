//! The core's side of a sandboxed mount: [`RemoteStrategy`] (the `Strategy<LiveBroker>` the core calls),
//! the queue it shares with the supervisor thread, and the handle the host keeps.
//!
//! Everything a hook does here runs ON THE FOLD THREAD and is bounded: a copy of the broker's read side
//! ([`FoldView::capture`]), one lock, a queue push (with a scan for the stream it conflates), and a
//! wake-up only when the supervisor is parked idle. No encoding, no I/O, no wait.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};

use vike_core::LiveBroker;
use vike_marketdata::{Bar, L2Book, TickQuote, TickTrade};
use vike_model::{FeedStatus, Fill, FlowToxicity, OrderEvent, Strategy, StrategyParams, TickMark};

use super::protocol::{FromChild, Hook, Opened, ViewRow};
use super::supervisor::{self, Launcher, QUEUE_CAP};

/// The `tracing` target every line of a sandboxed mount logs under.
pub(super) const TARGET: &str = "vike_tradehub_core::remote_strategy";

/// What a sandboxed mount runs: the strategy file, its params, and the mount it is.
#[derive(Debug, Clone)]
pub struct RemoteSpec {
    /// The strategy file, as the strategies folder names it.
    pub file: String,
    /// The file's sha256 (recorded: logged at open).
    pub sha256: String,
    /// The mount's params as TOML text.
    pub params: String,
    /// The mount FACTS the file is handed (interface 9): its own series (`symbol`, `venue`, `interval`, from
    /// which with `controller_id` the core's one rule, `vike_core::strategy_state::mount_id_with`, derives
    /// the id the core names the mount by), and what the host knows of the grid and the fee rates.
    pub facts: vike_model::params::MountFacts,
    pub controller_id: Option<String>,
}

impl RemoteSpec {
    /// The id the core names this mount by.
    #[must_use]
    pub fn mount_id(&self) -> String {
        vike_core::strategy_state::mount_id_with(
            self.controller_id.as_deref(),
            &self.facts.venue,
            &self.facts.symbol,
            &self.facts.interval,
        )
    }
}

/// What the operator is told about a sandboxed mount.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteStatus {
    Running,
    /// The child died and a fresh one is being started: `crashes` in the window, and why the last died.
    Restarting {
        crashes: usize,
        reason: String,
    },
    /// No more restarts until an operator restarts the mount.
    Stopped {
        reason: String,
    },
}

impl std::fmt::Display for RemoteStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RemoteStatus::Running => f.write_str("RUNNING"),
            RemoteStatus::Restarting { crashes, reason } => {
                write!(f, "RESTARTING ({crashes}/{}): {reason}", supervisor::CRASH_LIMIT)
            }
            RemoteStatus::Stopped { reason } => write!(f, "STOPPED: {reason}"),
        }
    }
}

/// The read side of one hook call, as the fold copied it: the rows the core's `LiveBroker` answered,
/// and the histories as `Arc` handles. What the child is SENT of them is decided at send time against
/// what it already holds, so a conflated event cannot lose a bar.
#[derive(Clone)]
pub(super) struct FoldView {
    pub(super) rows: Vec<ViewRow>,
    pub(super) declared: bool,
    pub(super) equity: f64,
    pub(super) index: usize,
    pub(super) now: i64,
    pub(super) bars: Vec<(String, Arc<Vec<Bar>>)>,
    pub(super) inputs: Option<vike_core::InputsAt>,
    pub(super) closed: Option<Arc<vike_analytics::closed_trades::TradeClosedLedger>>,
    /// On a declared mount, the row of the series this hook dispatched on: the one whose history IS the
    /// broker's own `bars` (the core lends both from one series, `Arc` for `Arc`). `None` on an undeclared
    /// mount, and on a declared one whose dispatching series holds no history yet (no `Arc` to match).
    pub(super) dispatched: Option<usize>,
}

impl FoldView {
    /// Copy `ctx`'s read side: one row per symbol the mount can be asked about (its own on an
    /// undeclared mount, its own and each declared one otherwise), read through `ctx`'s own answers.
    pub(super) fn capture(ctx: &LiveBroker) -> FoldView {
        let declared = !ctx.bar_views.is_empty();
        let (rows, bars) = if declared {
            let rows = ctx.bar_views.iter().map(|(name, _)| ViewRow::of(ctx, name)).collect();
            (rows, ctx.bar_views.clone())
        } else {
            (vec![ViewRow::of(ctx, "")], vec![(String::new(), Arc::clone(&ctx.bars))])
        };
        let dispatched = if declared {
            ctx.bar_views.iter().position(|(_, series)| Arc::ptr_eq(series, &ctx.bars))
        } else {
            None
        };
        FoldView {
            rows,
            declared,
            equity: vike_model::OrderBroker::equity(ctx),
            index: ctx.index,
            now: ctx.now,
            bars,
            inputs: ctx.inputs.clone(),
            closed: ctx.closed_trades.clone(),
            dispatched,
        }
    }
}

/// One hook call waiting to be sent.
#[derive(Clone)]
pub(super) struct Pending {
    pub(super) hook: Hook,
    pub(super) view: Option<FoldView>,
}

impl Pending {
    /// Whether a newer `other` (read with `view`) replaces this one while it waits: the same conflating
    /// stream. A book names no symbol, so on a DECLARED mount two books are one stream when they
    /// dispatched on the same leg ([`FoldView::dispatched`]); one whose leg is not known is queued.
    pub(super) fn conflates_with(&self, other: &Hook, view: Option<&FoldView>) -> bool {
        let leg = |v: Option<&FoldView>| v.map(|v| (v.declared, v.dispatched));
        match (&self.hook, other) {
            (Hook::TickQuote(a), Hook::TickQuote(b)) => a.symbol == b.symbol,
            (Hook::Mark(a), Hook::Mark(b)) => a.symbol == b.symbol,
            (
                Hook::ReferenceQuote { venue: va, quote: a },
                Hook::ReferenceQuote { venue: vb, quote: b },
            ) => va == vb && a.symbol == b.symbol,
            (Hook::Flow(_), Hook::Flow(_)) => true,
            (Hook::OrderBook(_), Hook::OrderBook(_)) => {
                match (leg(self.view.as_ref()), leg(view)) {
                    (Some((false, _)), Some((false, _))) => true,
                    (Some((true, Some(a))), Some((true, Some(b)))) => a == b,
                    _ => false,
                }
            }
            _ => false,
        }
    }
}

/// What the child sent, or that it is gone, tagged with the child's generation (a reader of a killed
/// child may still deliver; the supervisor ignores every generation but its current one).
pub(super) enum Inbound {
    Message(FromChild),
    Gone(String),
}

/// The queue the fold and the supervisor share.
#[derive(Default)]
pub(super) struct Outbox {
    pub(super) events: VecDeque<Pending>,
    pub(super) inbox: VecDeque<(u64, Inbound)>,
    /// The supervisor is parked waiting for an EVENT (not for a reply): only then does a push wake it.
    pub(super) idle: bool,
    /// The proxy was dropped (the mount went away, or the core stopped).
    pub(super) closing: bool,
    /// Stopped (a crash loop): events are dropped, counted.
    pub(super) refusing: bool,
    /// The queue passed [`QUEUE_CAP`]: the supervisor restarts the child.
    pub(super) overflow: bool,
    /// Events dropped since the last time it was said.
    pub(super) dropped: u64,
    /// While the core applies a reply (`Some(k)`): the CONSEQUENCES of those orders the core delivers (a
    /// `Denied`, a fill) go to the head of the queue, the next one at `k`, ahead of every hook queued while
    /// the reply travelled, so the strategy hears them first, as it would in-process.
    pub(super) applying: Option<usize>,
    /// The core said the last reply handed to it was applied, consequences and all.
    pub(super) acked: bool,
    /// A re-arm the host asked for (`RemoteHandle::fee_rearm`): the fee schedule the mount's account
    /// resolved, which the supervisor turns into the facts' account rate and a rebuilt strategy.
    pub(super) rearm: Option<Option<vike_model::FeeSchedule>>,
    /// The supervisor's answer to that re-arm, in words, for the host waiting on it.
    pub(super) rearmed: Option<String>,
}

/// The cache the core's synchronous reads are answered from.
#[derive(Default)]
pub(super) struct Cache {
    pub(super) state: Option<serde_json::Value>,
    pub(super) params: Option<StrategyParams>,
}

/// What the proxy, the supervisor and the handle share.
pub(super) struct Shared {
    pub(super) mount_id: String,
    pub(super) outbox: Mutex<Outbox>,
    pub(super) wake: Condvar,
    pub(super) cache: Mutex<Cache>,
    pub(super) status: Mutex<RemoteStatus>,
    /// Why the child last died, kept after the restart that followed.
    pub(super) last_reason: Mutex<Option<String>>,
    /// The core's lane the replies go to, attached once the core is spawned.
    pub(super) lane: OnceLock<vike_execution::EventSender>,
}

impl Shared {
    pub(super) fn outbox(&self) -> MutexGuard<'_, Outbox> {
        self.outbox.lock().unwrap_or_else(PoisonError::into_inner)
    }
    pub(super) fn cache(&self) -> MutexGuard<'_, Cache> {
        self.cache.lock().unwrap_or_else(PoisonError::into_inner)
    }
    pub(super) fn set_status(&self, status: RemoteStatus) {
        *self.status.lock().unwrap_or_else(PoisonError::into_inner) = status;
    }
    pub(super) fn set_last_reason(&self, reason: &str) {
        *self.last_reason.lock().unwrap_or_else(PoisonError::into_inner) = Some(reason.to_string());
    }
}

/// The host's handle on a sandboxed mount: it attaches the core's lane once the core runs, and reads the
/// mount's status.
#[derive(Clone)]
pub struct RemoteHandle {
    shared: Arc<Shared>,
}

impl RemoteHandle {
    /// Give the mount the core's lane (`vike_core::CoreHandle::event_sender`). Replies wait for it; a
    /// second call is ignored.
    pub fn attach(&self, lane: vike_execution::EventSender) {
        let _ = self.shared.lane.set(lane);
    }

    /// The mount's status now.
    #[must_use]
    pub fn status(&self) -> RemoteStatus {
        self.shared.status.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// The mount's FEE RE-ARM (`crate::FeeRearm`), the sandboxed twin of the in-process maker's: called with
    /// the fee schedule the mount's account resolved, once, after the engines are built and before the core
    /// is spawned (`crate::fee_rearm_mounts`), it moves the facts' `account_round_trip_fee_rate` to the
    /// value `vike_market_making::rearm_fee_floor` arms from that schedule and REBUILDS the file's strategy in a fresh
    /// child with those facts (a strategy file is handed its facts at create only), when and only when that
    /// law moves the floor; it waits for the rebuild. It returns no strategy: the mount keeps this proxy.
    #[must_use]
    pub fn fee_rearm(&self) -> crate::FeeRearm {
        let handle = self.clone();
        Box::new(move |schedule| {
            let answer = handle.rearm_fee(schedule);
            tracing::info!(target: TARGET, mount_id = %handle.shared.mount_id, %answer,
                "sandboxed strategy fee re-arm");
            None
        })
    }

    /// Ask the supervisor to re-arm at `schedule` and wait for its answer.
    fn rearm_fee(&self, schedule: Option<vike_model::FeeSchedule>) -> String {
        let mut out = self.shared.outbox();
        (out.rearm, out.rearmed, out.idle) = (Some(schedule), None, false);
        drop(out);
        self.shared.wake.notify_all();
        let deadline = std::time::Instant::now() + supervisor::OPEN_DEADLINE * 2;
        let mut out = self.shared.outbox();
        loop {
            if let Some(answer) = out.rearmed.take() {
                return answer;
            }
            if out.closing {
                return "the mount went away".to_string();
            }
            let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) else {
                return "no answer from the supervisor".to_string();
            };
            out =
                self.shared.wake.wait_timeout(out, left).unwrap_or_else(PoisonError::into_inner).0;
        }
    }

    /// Why the child last died (kept once it was restarted), or `None` while it never has.
    #[must_use]
    pub fn last_reason(&self) -> Option<String> {
        self.shared.last_reason.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// The id the core names the mount by.
    #[must_use]
    pub fn mount_id(&self) -> &str {
        &self.shared.mount_id
    }
}

/// The proxy the core mounts: every hook becomes an event for the child.
pub struct RemoteStrategy {
    shared: Arc<Shared>,
    opened: Opened,
}

impl RemoteStrategy {
    /// Start the mount's supervisor, which starts the child with `launcher` and opens the strategy in it;
    /// answers once the child said `Opened`, or the reason it did not (refused, no answer within
    /// [`supervisor::OPEN_DEADLINE`], no process). The mount does not run on an `Err`.
    ///
    /// # Errors
    /// The reason the strategy did not open, in words for the mount's `NOT RUNNING` line.
    pub fn open(
        spec: RemoteSpec,
        launcher: Launcher,
    ) -> Result<(RemoteStrategy, RemoteHandle), String> {
        let shared = Arc::new(Shared {
            mount_id: spec.mount_id(),
            outbox: Mutex::new(Outbox::default()),
            wake: Condvar::new(),
            cache: Mutex::new(Cache::default()),
            status: Mutex::new(RemoteStatus::Running),
            last_reason: Mutex::new(None),
            lane: OnceLock::new(),
        });
        let opened = supervisor::start(Arc::clone(&shared), spec, launcher)?;
        let handle = RemoteHandle { shared: Arc::clone(&shared) };
        Ok((RemoteStrategy { shared, opened }, handle))
    }

    /// The file's effective-params line (its `echo` export) as the child opened it, `(key, value)` in the
    /// file's order; `None` for a file with no `echo` export.
    #[must_use]
    pub fn echo(&self) -> Option<&[(String, String)]> {
        self.opened.echo.as_deref()
    }

    /// Queue one hook call for the child. On the fold thread: see the module doc.
    fn push(&self, hook: Hook, ctx: Option<&LiveBroker>) {
        let view = ctx.map(FoldView::capture);
        let mut out = self.shared.outbox();
        if out.refusing {
            out.dropped += 1;
            return;
        }
        if let Some(at) = out.applying {
            // A consequence of the reply the core is applying now: see `Outbox::applying`.
            out.events.insert(at, Pending { hook, view });
            out.applying = Some(at + 1);
            return self.wake_idle(out);
        }
        // Only a market stream conflates, so only one scans the queue for its older self.
        let stream = matches!(
            hook,
            Hook::TickQuote(_)
                | Hook::OrderBook(_)
                | Hook::Mark(_)
                | Hook::ReferenceQuote { .. }
                | Hook::Flow(_)
        );
        let older = if stream {
            out.events.iter().position(|p| p.conflates_with(&hook, view.as_ref()))
        } else {
            None
        };
        if let Some(at) = older {
            out.events.remove(at);
        } else if out.events.len() >= QUEUE_CAP {
            out.overflow = true;
            if !hook.is_lossless() {
                out.dropped += 1;
                return self.wake_idle(out);
            }
        }
        out.events.push_back(Pending { hook, view });
        self.wake_idle(out);
    }

    fn wake_idle(&self, mut out: MutexGuard<'_, Outbox>) {
        if out.idle {
            out.idle = false;
            drop(out);
            self.shared.wake.notify_all();
        }
    }
}

impl Drop for RemoteStrategy {
    fn drop(&mut self) {
        self.shared.outbox().closing = true;
        self.shared.wake.notify_all();
    }
}

impl Strategy<LiveBroker> for RemoteStrategy {
    fn warmup(&self) -> usize {
        self.opened.warmup
    }
    fn inputs(&self) -> Vec<String> {
        self.opened.inputs.clone()
    }
    fn symbols(&self) -> Vec<String> {
        self.opened.symbols.clone()
    }
    fn on_start(&mut self, broker: &mut LiveBroker) {
        self.push(Hook::Start, Some(broker));
    }
    fn on_bar(&mut self, broker: &mut LiveBroker, bar: &Bar) {
        self.push(Hook::Bar(bar.clone()), Some(broker));
    }
    fn on_quote_tick(&mut self, broker: &mut LiveBroker, q: &TickQuote) {
        self.push(Hook::TickQuote(q.clone()), Some(broker));
    }
    fn on_trade_tick(&mut self, broker: &mut LiveBroker, t: &TickTrade) {
        self.push(Hook::TickTrade(t.clone()), Some(broker));
    }
    fn on_order_book(&mut self, broker: &mut LiveBroker, book: &L2Book) {
        self.push(Hook::OrderBook(book.clone()), Some(broker));
    }
    fn on_schedule(&mut self, broker: &mut LiveBroker, tag: &str) {
        self.push(Hook::Schedule(tag.to_string()), Some(broker));
    }
    fn on_fill(&mut self, broker: &mut LiveBroker, fill: &Fill) {
        self.push(Hook::Fill(fill.clone()), Some(broker));
    }
    fn on_feed_status(&mut self, broker: &mut LiveBroker, status: FeedStatus) {
        self.push(Hook::FeedStatus(status), Some(broker));
    }
    fn on_mark(&mut self, broker: &mut LiveBroker, mark: &TickMark) {
        self.push(Hook::Mark(mark.clone()), Some(broker));
    }
    fn on_reference_quote(&mut self, broker: &mut LiveBroker, venue: &str, q: &TickQuote) {
        let hook = Hook::ReferenceQuote { venue: venue.to_string(), quote: q.clone() };
        self.push(hook, Some(broker));
    }
    fn on_flow(&mut self, broker: &mut LiveBroker, flow: FlowToxicity) {
        self.push(Hook::Flow(flow), Some(broker));
    }
    fn on_order_event(&mut self, broker: &mut LiveBroker, event: &OrderEvent) {
        self.push(Hook::OrderEvent(event.clone()), Some(broker));
    }
    fn on_params_updated(&mut self, broker: &mut LiveBroker, params: &StrategyParams) {
        self.push(Hook::ParamsUpdated(Box::new(params.clone())), Some(broker));
    }
    fn params(&self) -> Option<StrategyParams> {
        self.shared.cache().params.clone()
    }
    fn on_stop(&mut self, broker: &mut LiveBroker) {
        self.push(Hook::Stop, Some(broker));
    }
    fn save_state(&self) -> Option<serde_json::Value> {
        self.shared.cache().state.clone()
    }
    fn load_state(&mut self, state: &serde_json::Value) {
        self.shared.cache().state = Some(state.clone());
        self.push(Hook::LoadState(state.clone()), None);
    }
}
