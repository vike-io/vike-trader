//! The lifecycle value types the seam delivers: feed status, order transitions, marks, toxicity.

use super::Event;
#[cfg(doc)]
use super::{Broker, HftBroker, Strategy};

/// A feed-health transition delivered to a mounted strategy (net-hardening §B). The
/// strategy-facing, vike-model-native mirror of the data layer's `vike_data::StreamStatus`
/// three-state machine — vike-model is the bottom layer and cannot depend on vike-data, so the
/// live seam maps `StreamStatus` onto this 1:1 at its sink boundary (`GapStart` → `Disconnected`,
/// `Stale` → `Stale`, `Live` → `Live`). The precise trip timestamps `StreamStatus` carries are
/// intentionally dropped here: they are diagnostic (they stay in the sink's log), whereas a
/// strategy reacts to the STATE ("my feed died / recovered"), not the exact stamp. Net-new Rust
/// surface — no Python twin.
///
/// Design note: this is delivered through ONE hook ([`Strategy::on_feed_status`]) rather than
/// separate `on_disconnect` / `on_trading_disabled` methods. A single reason-carrying hook maps
/// 1:1 onto the only feed-health signal that actually exists in the codebase (transport/data
/// liveness), avoids introducing a producer-less "trading disabled" method, and lets the same hook
/// deliver the RECOVERY (`Live`) signal a "pull my quotes on death, resume on recovery" strategy
/// needs — which a `..._degraded`-named hook could not carry cleanly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FeedStatus {
    /// The stream's transport is DOWN (disconnect / server close / idle-watchdog trip). Data is
    /// MISSING until a following `Live`. The canonical "pull my resting quotes" trigger for a
    /// market-making strategy. Maps from `vike_data::StreamStatus::GapStart`.
    Disconnected,
    /// Transport is alive but no fresh DATA has arrived past the freshness threshold (a silently
    /// failed re-subscribe — the hazard a socket-liveness check can't see). Same "my prices may be
    /// stale, stop quoting" concern as `Disconnected`, kept distinct because transport itself is
    /// fine. Maps from `vike_data::StreamStatus::Stale`.
    Stale,
    /// The stream RECOVERED — transport re-established / fresh data resumed. A strategy that pulled
    /// quotes on `Disconnected`/`Stale` may safely resume. Maps from `vike_data::StreamStatus::Live`.
    Live,
}

/// A NON-FILL order-lifecycle transition for one of a mounted strategy's own orders, delivered
/// through [`Strategy::on_order_event`] (net-hardening / position-executor stage 3). It names the
/// order by its `client_order_id` and carries WHICH transition fired ([`OrderEventKind`]).
///
/// Fills are deliberately NOT delivered here — they stay on the richer [`Strategy::on_fill`] path
/// (post-dedup, with the per-fill position/equity snapshot). This hook carries only the
/// accept / reject / deny / cancel / expire transitions an order or position manager needs to drive
/// **retry / refresh / early-abort** (a rejected or canceled entry it would otherwise only infer,
/// slowly, from a fill timeout, and a venue cancel it could not see at all). It is the seam the
/// `PositionExecutor` framework (`vike_strategy::position_executor`) feeds its
/// `PositionExecutor::on_order_rejected` reaction from.
///
/// Net-new Rust surface — no Python twin, and NOT part of the serde `Event`/wire union: it is an
/// INTERNAL, strategy-facing view the runtime derives from the venue events it already folds
/// ([`OrderLifecycle::from_event`]). Adding it changes no wire schema.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OrderLifecycle {
    /// The client-order-id the transition is about — the strategy matches this against the coid(s)
    /// it is tracking (e.g. a `PositionExecutor`'s entry/close order) to decide if it is "mine".
    pub client_order_id: String,
    /// The STRATEGY'S OWN NAME for this order — the `tag` it passed to
    /// [`HftBroker::submit_limit_tagged`] — or `None` for an order placed through the untagged
    /// [`Broker`] verbs.
    ///
    /// ⚠ **This field is the LEARNING half of the tagged-order contract, and it did not exist.**
    /// [`HftBroker`] is built on the premise that a strategy names its resting order with a tag and
    /// NEVER sees a client-order-id: the tag registry belongs to the runtime. Place by tag, re-price
    /// by tag, cancel by tag. But this hook spoke ONLY `client_order_id`, a name a tagged-order
    /// strategy cannot resolve by construction — so every such strategy was structurally deaf to the
    /// death of its own orders. Both of this workspace's `HftBroker` strategies hit it: `vike_mm`'s
    /// `SpreadMaker` believed a filled quote was still resting and re-priced a dead order forever
    /// (its `SideState::placed` never cleared, so `modify_tagged` resolved a terminal coid and
    /// `ExecutionEngine::modify_order` returned silently — a side that never quoted again for the
    /// life of the process), and `vike_mm::xemm`'s maker declined to implement the hook at all,
    /// naming this exact gap in its module doc.
    ///
    /// The RUNTIME stamps it, because the runtime is what owns the registry:
    /// [`OrderLifecycle::from_event`] maps a wire [`Event`] and has no registry to consult, so it
    /// always sets `None`. The stamp is VALUE-MATCHED against the live tag→coid entry, so a late
    /// terminal for an already-replaced order reports `None` rather than naming a tag that now
    /// belongs to a live order.
    pub tag: Option<String>,
    /// Which lifecycle transition fired.
    pub kind: OrderEventKind,
}

/// The transition an [`OrderLifecycle`] carries. Exactly the NON-fill order-outcome signals a
/// retry/refresh/abort machine reacts to; fills stay on [`Strategy::on_fill`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderEventKind {
    /// The venue ACKNOWLEDGED the order — it is now live/resting (a non-terminal confirmation; the
    /// "my limit entry is working" signal a refresh timer starts from).
    Accepted,
    /// The venue REJECTED the order (terminal). `reason` is the venue's text (may be empty).
    Rejected { reason: String },
    /// The `RiskGate` VETOED the order pre-venue (terminal). Semantically a retryable reject — the
    /// order never reached the venue. `reason` is the gate's veto text.
    Denied { reason: String },
    /// The order was CANCELED (terminal) — a venue cancel, a strategy/refresh cancel-replace, or an
    /// external pull. `reason` is the venue's/originator's text (may be empty).
    Canceled { reason: String },
    /// The order EXPIRED at the venue (terminal — GTD / time-in-force elapsed).
    Expired,
    /// The order is COMPLETE — it filled its whole quantity and is terminal at the venue.
    ///
    /// ⚠ **This is not a fill, and it does not double-deliver one.** The money is on
    /// [`Strategy::on_fill`], which is unchanged and fires FIRST, with its per-fill position/equity
    /// snapshot; [`OrderLifecycle::from_event`] still returns `None` for every fill event. This
    /// variant carries the ORDER's death, which `on_fill` cannot express at all: [`Fill`] has no
    /// client-order-id, no tag and no remaining quantity, so a strategy could only ever GUESS
    /// whether the order behind a fill is finished — and a maker that guesses from `fill.size`
    /// against its own intended size guesses wrong on any partial-fill sequence.
    ///
    /// Full fill is the THIRD and, for a market maker, the MOST COMMON of the three ways a resting
    /// order dies. Without it here, a strategy would have to learn about a cancel and a reject
    /// through one mechanism and a completion through another — the "one law spelled twice" shape
    /// this workspace keeps paying for. The runtime emits it from the ONE place that already knows:
    /// the post-fold registry status it already consults to retire the order's attribution entry.
    ///
    /// [`Fill`]: crate::Fill
    Filled,
}

impl OrderEventKind {
    /// Whether this transition ENDS the order — after it, nothing more can happen to that order and
    /// its tag names nothing. Every variant except [`OrderEventKind::Accepted`] (a non-terminal
    /// confirmation that the order is now working).
    ///
    /// Spelled ONCE here rather than re-matched at each consumer: the runtime's retirement sweep and
    /// every strategy that frees a slot on death ask the same question, and an unclassified new
    /// variant defaulting to "not terminal" at one of them is precisely how a strategy goes deaf.
    pub fn is_terminal(&self) -> bool {
        match self {
            OrderEventKind::Accepted => false,
            OrderEventKind::Rejected { .. }
            | OrderEventKind::Denied { .. }
            | OrderEventKind::Canceled { .. }
            | OrderEventKind::Expired
            | OrderEventKind::Filled => true,
        }
    }
}

impl OrderLifecycle {
    /// Derive the strategy-facing lifecycle view from a venue [`Event`], or `None` when the event is
    /// not a NON-fill order transition a strategy observes here (fills — `OrderFilled`/
    /// `OrderPartiallyFilled`/`Event::Fill` — return `None`: they flow [`Strategy::on_fill`]; so do
    /// `OrderSubmitted` (the local submit echo), triggers/modifications, liquidations, and every
    /// non-order event). This is the ONE mapping authority the runtime routes from.
    ///
    /// [`OrderEventKind::Filled`] is deliberately NOT produced here and has no `Event` arm: a wire
    /// fill event says a quantity executed, not that the ORDER is finished — only the post-fold
    /// registry status knows that, and only the runtime holds the registry. `tag` is likewise always
    /// `None` (see [`OrderLifecycle::tag`]); the runtime stamps it before delivery.
    pub fn from_event(ev: &Event) -> Option<Self> {
        let (client_order_id, kind) = match ev {
            Event::OrderAccepted(e) => (e.client_order_id.clone(), OrderEventKind::Accepted),
            Event::OrderRejected(e) => (
                e.client_order_id.clone(),
                OrderEventKind::Rejected { reason: e.reason.to_string() },
            ),
            Event::OrderDenied(e) => {
                (e.client_order_id.clone(), OrderEventKind::Denied { reason: e.reason.to_string() })
            }
            Event::OrderCanceled(e) => (
                e.client_order_id.clone(),
                OrderEventKind::Canceled { reason: e.reason.to_string() },
            ),
            Event::OrderExpired(e) => (e.client_order_id.clone(), OrderEventKind::Expired),
            _ => return None,
        };
        Some(OrderLifecycle { client_order_id, tag: None, kind })
    }
}

/// A REFERENCE/underlying mark tick delivered to a mounted strategy through [`Strategy::on_mark`]
/// (cross-symbol routing, "Option B"): a valuation price for a series the strategy WATCHES but is
/// not mounted on — e.g. the `btcusdt` RTDS spot a Polymarket BTC up/down maker anchors its fair mid
/// on, a DIFFERENT symbol than the outcome token it quotes. `symbol` names the UNDERLYING's own
/// series (not the mount's), `price` is its latest mark, `ts` the event time in epoch-ms.
///
/// Net-new Rust surface — no Python twin, and NOT part of the serde `Event`/wire union: it is an
/// INTERNAL, strategy-facing view the runtime derives from the venue marks it already drains. Adding
/// it changes no wire schema. `f64` field ⇒ `PartialEq` only (deliberately NO `Eq`).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MarkTick {
    /// The underlying/reference series' own symbol (e.g. `"btcusdt"`) — NOT the mounted symbol.
    pub symbol: String,
    /// The latest mark (spot) of the underlying series.
    pub price: f64,
    /// Event time, epoch milliseconds.
    pub ts: i64,
}

/// A per-side FLOW-TOXICITY reading delivered to a mounted strategy through [`Strategy::on_flow`]
/// (RTDS wallet-toxicity guard). Each field is the CURRENT normalized toxic-flow intensity in
/// `[0, 1]` on that side of the book, `ts` the event time in epoch-ms. The side mapping is the
/// taker's: a toxic BUY taker LIFTS our resting ASK, so buy toxicity feeds [`FlowToxicity::ask`],
/// and a toxic SELL taker HITS our resting BID, feeding [`FlowToxicity::bid`]. A maker widens (and
/// cuts size on) the side under toxic pressure.
///
/// Net-new Rust surface — no Python twin, and NOT part of the serde `Event`/wire union: it is an
/// INTERNAL, strategy-facing view the runtime derives from a toxicity aggregator it drains. Adding
/// it changes no wire schema. `f64` fields ⇒ `PartialEq` only (deliberately NO `Eq`). Delivered off
/// the OCCASIONAL control lane (a toxicity update, not a per-market-message call), like
/// [`FeedStatus`]/[`MarkTick`], so it stays off the live core's per-tick hot path.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FlowToxicity {
    /// Normalized toxic-flow intensity in `[0, 1]` on the BID side (fed by toxic SELL takers).
    pub bid: f64,
    /// Normalized toxic-flow intensity in `[0, 1]` on the ASK side (fed by toxic BUY takers).
    pub ask: f64,
    /// Event time, epoch milliseconds.
    pub ts: i64,
}

/// The ONE `[strategy.params]` key that carries a strategy's SOURCE rather than one of its knobs.
///
/// It is the key `vike-cli backtest --script` writes (`crates/vike-cli/src/cmd/backtest/profile.rs`'s
/// `inject_script_src`) and the key `vike_backtest::harness::registry`'s `"rhai"` arm reads to
/// reach a Rhai compiler. Everything else in that table is a knob a parameter search may vary.
///
/// ⚠ **It lives HERE because it had THREE spellings and they could not see each other.**
/// `crates/vike-cli/src/cmd/backtest.rs` declared `SRC_KEY`,
/// `crates/vike-studio-core/src/user_strategies/load.rs` declared `RESERVED_SRC_KEY`, and
/// `crates/vike-backtest/src/harness/registry.rs`'s `rhai_overrides` filtered a bare `"src"`
/// literal — three copies of one fact, in three crates none of which is below the other two.
/// `docs/decisions/0064-a-named-run-carries-no-source.md`'s decision 2 required a FOURTH reader
/// (the named-run belt) in a crate below all of them, which is what forced the unification rather
/// than merely recommending it. vike-model is the only home every reader can reach.
///
/// ⚠ **Naming it is a BELT, never the fence.** The fence against a wire-supplied script is
/// structural — the named-run request has no field a string can occupy, and its resolution closure
/// holds no compiler — and 0064's decision 2 says so in its own words: under that closure a `src`
/// is *unread*, not refused. A check that is the only thing between a socket and a compiler means
/// the structural argument has already been broken somewhere else.
pub const RESERVED_SRC_KEY: &str = "src";
