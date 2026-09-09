//! The unified strategy seam — ONE `Strategy` trait + the `Broker` handle it trades through.
//!
//! `Broker` is the strategy's window onto whichever execution stack is running it:
//! - backtest: the sim engine core implements `Broker` with DIRECT mutation (fast path);
//! - live/paper: the per-`on_bar` live context implements `Broker` by BUFFERING submissions,
//!   drained through the one live path (mint → RiskGate → client) after the handler returns.
//!
//! Write-once rule: a strategy written as `impl<B: Broker> Strategy<B>` runs UNCHANGED on both
//! stacks (the r7 gate proves the results are bit-identical). A strategy that needs
//! backtest-only power (schedule registration, weighted/bracket submits, sizers, multi-TF
//! reads) is written against the concrete sim broker instead — `impl Strategy<SimBroker>` —
//! and the type system documents that it is not live-portable.
//!
//! Dispatch is GENERIC (monomorphized), never `dyn`, in the backtest hot loop — a vtable per
//! verb would regress the ~70 ns/bar engine. The live side may box the strategy
//! (`Box<dyn Strategy<LiveBroker> + Send>`): one virtual call per closed bar is noise there.
//!
//! # Where the line runs — and why widening [`Broker`] would not move it
//!
//! The recurring proposal is "add the missing accessors to [`Broker`] and every strategy becomes
//! portable". It is wrong, and the reason is worth stating once here rather than re-deriving it:
//! what keeps a backtest-only strategy backtest-only is not a missing READ, it is a CONCEPT the
//! live stack does not have. Five of them, each owned by the simulator alone:
//!
//! * the **position sizer** — the backtest's non-`raw` submit routes the requested size through a
//!   `PositionSizer` that may ignore it entirely (a percent-of-equity sizer returns
//!   `pct·equity/(price·mult)` and never reads the intent). The live path is ALWAYS raw. A verb
//!   added here would have to either always-raw (losing the concept) or invent a live sizer.
//! * the **shared-cash gate `weight`** — the priority order entries are DROPPED in when cash runs
//!   out. Live has no shared-cash gate, so a `weight` argument on a portable verb would name
//!   nothing on one of the two stacks.
//! * the **protective stop attached to the entry** — a field on the backtest order that also feeds
//!   the sizer's risk basis. Live arms a bracket as SEPARATE orders.
//! * the **symbol universe** — a backtest runs a slice of N symbols; a live mount pins ONE
//!   `(venue, symbol)` series, so there is no universe to enumerate.
//! * the **bar-count schedule** — `every_n_bars` has no wall-clock meaning, and the live schedule
//!   takes wall-clock rules supplied as mount CONFIG, unreachable from a strategy's `on_start`.
//!
//! So `impl Strategy<SimBroker>` is a deliberate, load-bearing SECOND form, not a backlog item:
//! it is how a strategy says "I drive simulator machinery", and the type system is what stops it
//! being mounted live. The genuinely portable ones are written `impl<B: Broker> Strategy<B>`, and
//! that is pinned by COMPILATION rather than by assertion — see
//! `crates/vike-backtest/src/harness/registry.rs`'s `buy_hold_mounts_on_any_broker`: a generic
//! probe fn whose body is type-checked at its DEFINITION, so narrowing an impl back to one
//! concrete broker stops compiling.
//!
//! ⚠ A second such probe once covered `TickPairMse`, and it was deleted with the portability it
//! claimed. Making that strategy generic silently swapped `position_of(&sym).size` — the exchange's
//! own state — for [`Broker::position`], which on `SimBroker` returns the response-latency SHADOW
//! whenever the opt-in latency gate is armed. Same code, different number, on a path a profile knob
//! reaches. `crates/vike-backtest/src/ref_strategies.rs`'s
//! `the_ref_strategies_are_deliberately_not_portable` records that, so the next reader does not
//! re-derive it from an impl that merely looks narrower than it needs to be.
//!
//! ⚠ **Adding a verb here whose NAME already exists as an inherent method on a concrete broker
//! creates a silent generic-vs-concrete split.** The inherent method wins at every concrete call
//! site while generic `B: Broker` code gets the trait's — the same source line meaning two
//! different things. The tree already dodges this once: `SimBroker`'s 7-argument inherent
//! `submit_limit` shadows this trait's 4-argument one, which is why its `impl Broker` body spells
//! the delegation as UFCS. Either pick a non-colliding name, or make the concrete broker's impl
//! delegate explicitly the way that one does.
//!
//! # What belongs in this module, and what does NOT
//!
//! This module is the SEAM — the traits and the event/lifecycle views every strategy on either
//! stack is written against. It is NOT a home for strategy configuration.
//!
//! A strategy family's tuning parameters enter this crate on exactly ONE criterion: the family
//! supports a LIVE re-tune, so its params ride [`StrategyParams`] — a serde field of the runtime's
//! journaled command lane (`vike_exec::lanes::Command::UpdateParams`), which sits BELOW every
//! strategy and must be able to name them. Two families qualify today: the `SpreadMaker` /
//! Avellaneda-Stoikov maker (its ~980-line params tree is [`maker_params`], split out of this file
//! so a re-tune diff never reads like a change to the seam above) and the `PositionController`
//! harness ([`crate::barrier::ControllerParams`]).
//!
//! Every OTHER family — `pairs_zscore`, `funding_carry`/`funding_capture`, `grid`/`dca_accumulate`,
//! `trailing_scalper`, `momentum`, the Polymarket takers, … — keeps its knobs as plain fields on
//! its own struct next to its implementation and reads them from the harness TOML params table in
//! its `from_params`. None of them appear here, and a new family should not unless it needs the
//! live-tune lane.

use crate::bar::{Bar, QuoteTick, TradeTick};
use crate::events::Event;
use crate::orderbook::L2Book;
use crate::position::Fill;

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
/// `PositionExecutor` framework (`crate::position_executor`) feeds its
/// [`PositionExecutor::on_order_rejected`](crate::PositionExecutor::on_order_rejected) reaction from.
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

/// The strategy's execution handle: portable order intake + account/market reads.
///
/// The verb surface is deliberately the COMMON DENOMINATOR of the two stacks. Engine-specific
/// verbs (weighted submits, protective-stop arming, `cancel_all`, target-percent, reduce-only)
/// stay inherent methods on the concrete broker types.
pub trait Broker {
    // --- order intake ---
    /// Market order: `side` +1 buy / -1 sell, `qty` in units. Backtest: rests and fills at the
    /// next bar per the fill model. Live: routed through mint → RiskGate → client.
    fn submit_market(&mut self, symbol: &str, side: i32, qty: f64);
    /// Limit order at `price`.
    fn submit_limit(&mut self, symbol: &str, side: i32, qty: f64, price: f64);

    // --- reads ---
    /// Signed position size in `symbol` (0.0 when flat).
    fn position(&self, symbol: &str) -> f64;
    /// Last mark for `symbol` (the engine's post-fill price; live: the bar close).
    fn price(&self, symbol: &str) -> f64;
    /// Account equity now (cash + Σ pos·price·mult / live: resolver-priced via `resolve_equity`).
    fn equity(&self) -> f64;
    /// Closed bars of `symbol` up to and INCLUDING the current step (no look-ahead).
    /// Single-symbol live brokers may ignore `symbol`.
    fn bars(&self, symbol: &str) -> &[Bar];
    /// Current step index (bars processed so far minus one).
    fn index(&self) -> usize;
    /// Current engine time, epoch milliseconds.
    fn now(&self) -> i64;

    // --- pre-trade liquidity reads (defaulted: an engine with no book answers "I don't know") ---

    /// **What would `qty` units actually cost me right now?** The volume-weighted price of
    /// consuming the resting book best-level-outward — `side` +1 buy (walks asks) / −1 sell
    /// (walks bids).
    ///
    /// This exists because a strategy cannot gate on a price it has no way to see. Without it the
    /// only price surface is [`Broker::price`], a single scalar, and every strategy that sizes off
    /// it is implicitly assuming infinite depth at the top of book. Reading this before ordering
    /// is what lets a strategy refuse a trade whose edge does not survive the liquidity actually
    /// on offer.
    ///
    /// `None` means **the answer is unavailable or the book cannot honour it** — no L2 data for
    /// that symbol, or the displayed depth does not cover `qty`. It is never a price. A strategy
    /// must treat `None` as "do not trade this size", not as "assume the last price": that
    /// distinction is the whole value of the verb. Pair it with [`Broker::depth_within_price`] to
    /// size DOWN to what is available instead of giving up.
    ///
    /// Displayed depth upper-bounds real fill quality (it ignores queue position, hidden size and
    /// the taker's own impact), so this is a bound, not a guarantee. The backtest answers it from
    /// the replayed L2 book with exactly the law its own fill path uses, so a strategy that gates
    /// on this read and then submits gets a fill consistent with what it was told.
    ///
    /// DEFAULT `None`: every existing broker — live, paper, bar-only backtest — is unchanged and
    /// needs no edit. A book-less engine truthfully says it does not know.
    fn quote_vwap(&self, symbol: &str, side: i32, qty: f64) -> Option<f64> {
        let _ = (symbol, side, qty);
        None
    }

    /// **How much could I get without paying worse than `limit_px`?** Total displayed size at
    /// `limit_px` or better (buy: asks at or below; sell: bids at or above).
    ///
    /// The sizing half of [`Broker::quote_vwap`]: a strategy that knows the worst price it can
    /// accept asks this for the quantity that price supports, and submits THAT rather than a size
    /// the market cannot fill. `0.0` — the default, and the honest answer for a book-less engine —
    /// means "nothing available", which correctly sizes such a strategy to no trade.
    fn depth_within_price(&self, symbol: &str, side: i32, limit_px: f64) -> f64 {
        let _ = (symbol, side, limit_px);
        0.0
    }
}

/// The HFT extension of [`Broker`]: the tagged-resting-order verbs a market-making strategy needs
/// that are NOT on the portable [`Broker`] common denominator. A strategy written as
/// `impl<B: HftBroker> Strategy<B>` stays engine-agnostic over ANY broker that can resolve a
/// strategy-chosen `tag` to a resting order and re-price/cancel it in place — the live `LiveBroker`
/// in `vike-core` today, a sim/backtest HFT broker later — and remains a `Strategy<B: Broker>`
/// because `HftBroker: Broker` (so a plain `Broker`-only engine simply cannot mount it, which the
/// type system documents).
///
/// Every verb here mirrors an existing inherent verb on the live broker 1:1: a `tag` is a
/// strategy-chosen stable id naming one resting order, so a later modify/cancel resolves it WITHOUT
/// the strategy ever seeing a client-order-id. The surface is deliberately minimal (position read +
/// tagged submit/modify/cancel) and carries NO runtime/wire types — pure `f64`/`&str`/`Option` — so
/// it stays in the bottom domain layer next to [`Broker`]. Net-new Rust surface, no Python twin.
pub trait HftBroker: Broker {
    /// Current SIGNED position in the mounted symbol (single-symbol scoped, so no `symbol` arg —
    /// the mount pins the (venue, symbol) series). `0.0` when flat. The read an inventory-skew
    /// maker biases its bid/ask sizes off. Distinct from [`Broker::position`], whose `symbol`
    /// argument the single-symbol live broker ignores anyway.
    fn position(&self) -> f64;
    /// Submit a TAGGED limit order. `tag` names this resting order (`side` +1 bid / −1 ask); a
    /// later [`HftBroker::modify_tagged`] on the same tag re-prices it IN PLACE (preserving venue
    /// queue priority), and [`HftBroker::cancel_tagged`] pulls it.
    fn submit_limit_tagged(&mut self, tag: &str, side: i32, qty: f64, price: f64);
    /// Modify a previously-tagged resting order IN PLACE (new qty and/or new price; `None` leaves
    /// that field unchanged) — the re-quote-without-losing-queue-priority verb. No-op if the tag is
    /// unknown or its order is already terminal.
    fn modify_tagged(&mut self, tag: &str, new_qty: Option<f64>, new_price: Option<f64>);
    /// Cancel ONE previously-tagged resting order (pull a single quote), leaving any other tagged
    /// order untouched. No-op if the tag is unknown or its order is already terminal.
    fn cancel_tagged(&mut self, tag: &str);
}

/// The MULTI-SYMBOL extension of [`HftBroker`]: the ONE extra read a correlation-aware market maker
/// needs that the single-symbol [`HftBroker`] cannot express — the signed net inventory of a NAMED
/// symbol, so a maker quoting N correlated instruments can read the WHOLE inventory vector `q` its
/// covariance-matrix skew (`γ·Σ·q`, see `vike_mm::multi_asset`) folds over.
///
/// [`HftBroker::position`] is single-symbol scoped (the mount pins ONE `(venue, symbol)` series and
/// takes no `symbol` arg); a multi-asset maker mounted over N series instead reads `position_of` for
/// each symbol it quotes to assemble the inventory vector. Kept deliberately MINIMAL — just the
/// per-symbol position read — carrying NO runtime/wire types (pure `f64`/`&str`), so it stays in the
/// bottom domain layer next to [`HftBroker`]. Because `MultiHftBroker: HftBroker`, a broker that can
/// answer `position_of` can still be mounted by any single-symbol `impl<B: HftBroker> Strategy<B>`,
/// and a plain single-symbol engine simply cannot mount a maker that needs the multi read (the type
/// system documents it). Net-new Rust surface, no Python twin.
pub trait MultiHftBroker: HftBroker {
    /// The current SIGNED net position (inventory) in `symbol` — `+` long, `−` short, `0.0` when flat
    /// or the symbol is not one this broker tracks. The multi-symbol generalization of
    /// [`HftBroker::position`]: a correlated-inventory maker calls this once per quoted symbol to
    /// build the inventory vector `q` its `γ·Σ·q` skew consumes.
    fn position_of(&self, symbol: &str) -> f64;
}

/// The ONE strategy trait, generic over its broker (monomorphized per engine).
///
/// All handlers default to no-ops; implement only what the strategy needs. Handlers are
/// skipped until `index >= warmup()` (the R2 gate) — enforced by the engines, not here.
#[allow(unused_variables)]
pub trait Strategy<B: Broker> {
    fn warmup(&self) -> usize {
        0
    }
    fn on_start(&mut self, broker: &mut B) {}
    /// Backtest: fires once per symbol per step (bar carries its symbol tag).
    /// Live: fires once per CLOSED bar of the mounted series.
    fn on_bar(&mut self, broker: &mut B, bar: &Bar) {}
    fn on_quote_tick(&mut self, broker: &mut B, q: &QuoteTick) {}
    fn on_trade_tick(&mut self, broker: &mut B, t: &TradeTick) {}
    /// L2 book update for the mounted symbol (R8 HFT track). Net-new Rust surface — no Python
    /// twin — so default no-op keeps every existing strategy compiling unchanged.
    fn on_order_book(&mut self, broker: &mut B, book: &L2Book) {}
    fn on_schedule(&mut self, broker: &mut B, tag: &str) {}
    fn on_fill(&mut self, broker: &mut B, fill: &Fill) {}
    /// A feed-health transition for the mounted stream (net-hardening §B): the feed disconnected,
    /// went stale, or recovered (see [`FeedStatus`]). This is the seam a strategy uses to "pull my
    /// quotes when my feed dies" (and resume on recovery). Fires ONLY on a status CHANGE (feed
    /// up↔down) — the data layer's stream-health machine already debounces to one signal per
    /// transition, so this is an OCCASIONAL control event, never a per-market-message call, and
    /// stays off the live core's per-tick hot path. Default no-op keeps every existing strategy
    /// (backtest and live) compiling and behaving unchanged; net-new Rust surface, no Python twin.
    /// The backtest engines never fire it (there is no live feed to disconnect); it is a live-only
    /// signal, harmless to implement in a portable `impl<B: Broker> Strategy<B>`.
    fn on_feed_status(&mut self, broker: &mut B, status: FeedStatus) {}
    /// A REFERENCE/underlying MARK for a series this strategy WATCHES but is NOT mounted on (see
    /// [`MarkTick`]) — the cross-symbol routing seam ("Option B"). The runtime routes a mark of the
    /// strategy's declared underlying symbol (a DIFFERENT symbol than the mounted one — e.g. the
    /// `btcusdt` RTDS spot a Polymarket BTC up/down maker anchors its fair mid on) here. Delivered off
    /// the OCCASIONAL mark-drain lane — an underlying mark drains WITH the venue marks (`drain_market`),
    /// well off the per-tick / per-market-message `p99 < 10µs` event fold, and touches no OMS fold —
    /// only the mounted strategy's hook + any orders it buffers, drained through the one live path. The
    /// broker ctx is built on the strategy's OWN (venue, symbol) — the underlying rides only in `mark`
    /// — so a strategy reads its own position/price and buffers orders exactly as on any other hook.
    /// NOT warmup-gated (an underlying observation is a market fact regardless of bar count, mirroring
    /// `on_feed_status`/`on_fill`).
    ///
    /// Default no-op: a strategy that ignores this hook is entirely UNAFFECTED — every existing
    /// strategy behaves identically, and a mount that declares no underlying is never routed one. Net-new
    /// Rust surface, no Python twin; a live-only signal the backtest engines never fire, harmless in a
    /// portable `impl<B: Broker> Strategy<B>`.
    fn on_mark(&mut self, broker: &mut B, mark: &MarkTick) {}
    /// An L1 touch from a DECLARED leg on a DIFFERENT VENUE — the cross-EXCHANGE reference-price
    /// seam, and the input a cross-exchange market maker (xEMM) prices its maker quotes off.
    ///
    /// The runtime routes a `QuoteTick` here when the mount declared a leg
    /// `MountLeg::at(symbol, venue)` whose venue is NOT the mount's own and the tick matches that
    /// `(venue, symbol)` pair. `venue` names WHICH venue the touch came from — carried as an
    /// argument, not in the tick, because **no tick type carries a venue**: [`QuoteTick`] and
    /// [`TradeTick`] carry `symbol` only and `L2Book` carries neither, and stamping a venue onto a
    /// journaled serde payload would be a wire change. Two venues can also legitimately use the
    /// SAME symbol string, so venue attribution cannot be inferred from the payload.
    ///
    /// WHY IT IS A DISTINCT HOOK, NOT `on_quote_tick`. Overloading `on_quote_tick` would make
    /// "which book am I resting in" depend on an operator not typing the same ticker for both legs
    /// — a maker that mistakes the reference venue's touch for its own quotes off its own quotes
    /// and self-references. A distinct hook makes the two lanes structurally disjoint: the runtime
    /// dispatches a tick to EITHER `on_quote_tick` (the mount's OWN venue) or here (a declared
    /// foreign venue), never both.
    ///
    /// THE BROKER IS THE MOUNT'S OWN. The `ctx` is built on the mount's `(venue, symbol)` — its own
    /// engine, its own bars, its own mark — and any order buffered here drains against the mount's
    /// OWN series, exactly as on `on_bar`/`on_quote_tick`. That is load-bearing rather than
    /// incidental: the tagged-order verbs are symbol-less by [`HftBroker`] contract, so their tag
    /// registry key is derived from the DRAIN's `(venue, symbol)`. Draining a reference-venue tick
    /// on the reference venue's series would key a maker quote under the WRONG venue AND route it
    /// there — placing the maker's own quote on the venue it meant to hedge on. So a strategy may
    /// safely place/re-price/pull its maker quotes from this hook; it must NOT assume the ctx
    /// describes the venue named by `venue`.
    ///
    /// Delivered off the same per-tick lane `on_quote_tick` rides, so it IS a per-market-message
    /// hook — but only for a mount that declared a foreign-venue leg. A runtime where no mount
    /// declared one never reaches the dispatch at all (one bool read), so the `p99 < 10µs` fold is
    /// untouched for every existing runtime. NOT warmup-gated: a reference touch is a market fact
    /// regardless of the mount's own bar count (mirroring `on_feed_status`/`on_mark`).
    ///
    /// Default no-op: a strategy that ignores this hook is entirely UNAFFECTED, and a mount that
    /// declares no foreign-venue leg is never routed one. Net-new Rust surface, no Python twin; a
    /// live-only signal the backtest engines never fire (a `SimBroker` slice is single-venue),
    /// harmless in a portable `impl<B: Broker> Strategy<B>`.
    fn on_reference_quote(&mut self, broker: &mut B, venue: &str, q: &QuoteTick) {}
    /// A per-side FLOW-TOXICITY update for the mounted stream (RTDS wallet-toxicity guard, see
    /// [`FlowToxicity`]): the current normalized toxic-flow intensity on each side of the book. This
    /// is the seam a market maker uses to WIDEN and CUT SIZE on the side toxic (edge-carrying /
    /// size-moving) flow is hitting. Delivered off the OCCASIONAL control lane — the runtime routes a
    /// toxicity reading for the mount's EXACT `(venue, symbol)` here (the toxic tape's asset IS the
    /// mounted token), NOT per market message, so an update folds at toxicity cadence and the
    /// `p99 < 10µs` market/tick fold is untouched (this hook touches NO OMS fold — only the mounted
    /// strategy's reaction + any orders it buffers, drained through the one live path). NOT
    /// warmup-gated (toxic flow is a market fact regardless of bar count, mirroring
    /// `on_feed_status`/`on_mark`).
    ///
    /// Default no-op: a strategy that ignores this hook is entirely UNAFFECTED — every existing
    /// strategy behaves identically, and a mount fed no toxicity is never routed one. Net-new Rust
    /// surface, no Python twin; a live-only signal the backtest engines never fire, harmless in a
    /// portable `impl<B: Broker> Strategy<B>`.
    fn on_flow(&mut self, broker: &mut B, flow: FlowToxicity) {}
    /// An order-lifecycle transition for one of THIS strategy's orders — the venue ACCEPTED /
    /// REJECTED / CANCELED / EXPIRED it, the `RiskGate` DENIED it, or it FILLED COMPLETELY (see
    /// [`OrderLifecycle`]). This is the seam a position/order manager (e.g. a `PositionExecutor`
    /// harness) uses to drive retry / refresh / early-abort off a reject or cancel it would
    /// otherwise only infer from a fill timeout — and to detect a venue cancel it could not see at
    /// all — and the seam a TAGGED-order strategy uses to free the slot its tag names, by that same
    /// tag ([`OrderLifecycle::tag`]).
    ///
    /// ⚠ The FILL ITSELF is not delivered here: the executed quantity, price and the resulting
    /// position/equity stay on the richer [`Strategy::on_fill`] path (post-dedup, per-fill
    /// snapshot), which fires FIRST and is unchanged. What [`OrderEventKind::Filled`] adds is the
    /// ORDER's death — the fact `on_fill` structurally cannot carry, and the one that makes all
    /// three ways a resting order dies arrive through ONE hook with ONE shape.
    ///
    /// Delivered off the OCCASIONAL order-outcome lane: the runtime routes each transition to the
    /// mount that owns the order's (venue, symbol) — the SAME routing `on_fill` uses — so this is
    /// OFF the per-tick / per-market-message hot path. An order event folds at ORDER cadence (a
    /// submit reply / a cancel), never per quote or bar, so the `p99 < 10µs` market/tick fold is
    /// untouched. NOT warmup-gated (an order outcome is an account fact regardless of bar count,
    /// mirroring `on_fill`). Because the live core is single-writer, this hook runs on the SAME core
    /// thread as the tick handlers, so any order it buffers drains through the one live path exactly
    /// like `on_fill`'s.
    ///
    /// Default no-op: a strategy that ignores this hook is entirely UNAFFECTED — SpreadMaker and
    /// every existing strategy behave identically. Net-new Rust surface, no Python twin; a live-only
    /// signal the backtest engines never fire (their fills flow the same `on_fill` path), harmless
    /// in a portable `impl<B: Broker> Strategy<B>`.
    fn on_order_event(&mut self, broker: &mut B, event: &OrderLifecycle) {}
    /// A LIVE PARAMETER update for the mounted strategy (the live-parameter plane). Delivered off
    /// an OCCASIONAL control lane (a GUI/operator re-tune, not a market message — the runtime's
    /// `Command::UpdateParams`), so it is off the per-tick hot path and touches NO OMS fold. The
    /// strategy hot-swaps its own tunables from `params` — typically by matching its own
    /// [`StrategyParams`] variant and ignoring any other — and the new values take effect on its
    /// NEXT tick (a re-price rides the existing modify-in-place path, so resting orders / queue
    /// position are preserved; nothing is canceled just because a knob changed).
    ///
    /// Default no-op: a strategy that ignores this hook is entirely UNAFFECTED — there is no
    /// behavior change until an update is actually sent. Because the live core is single-writer
    /// (this hook and the tick handlers run on the SAME core thread, serialized through the one
    /// ingest queue), the swap is a plain `&mut self` field set — no lock, no arc-swap, and so NO
    /// per-tick read cost is added on the hot path. Net-new Rust surface, no Python twin; a
    /// live-only signal the backtest engines never fire, harmless in a portable `impl<B: Broker>`.
    fn on_params_updated(&mut self, broker: &mut B, params: &StrategyParams) {}
    /// The READ side of the live-parameter plane: this strategy's CURRENT live tunables, or `None`
    /// when it publishes none. The exact inverse of [`Strategy::on_params_updated`] — what comes
    /// back is the bag that hook would consume, so an operator surface can read-modify-write one
    /// knob without re-typing (and thereby silently reverting) the rest.
    ///
    /// The AUTHORITY must be the strategy's own live fields, never an echo of the last bag it was
    /// handed: an implementor that transforms what it was given (as `vike_mm::SpreadMaker` does —
    /// it strips the A-S sub-bag into a live estimator and re-attaches it on read, which may have
    /// CLAMPED it) would otherwise report a value nobody holds. A clamp showing up in the read-back
    /// is information; a cached input reporting it away is the declared-but-unread failure this
    /// workspace engineers against.
    ///
    /// Default `None`, the [`Strategy::on_params_updated`] precedent verbatim: a strategy that
    /// ignores this hook is entirely UNAFFECTED. `None` is a FIRST-CLASS answer rather than a gap —
    /// the mounts that can answer this read are exactly the mounts `Command::UpdateParams` can
    /// address, so a strategy with no [`StrategyParams`] variant (a registry strategy mounted by
    /// name off a free-form TOML table) is honestly unreadable AND unwritable by this plane.
    ///
    /// Called off the COLD path only — the runtime fills it at the coalesced snapshot publish
    /// (`vike_core`'s `mount_views`), never on the per-message fold. It takes `&self` and returns an
    /// OWNED value, so an implementor whose params are expensive to materialise would pay that at
    /// every publish; every implementor today is a plain struct copy.
    fn params(&self) -> Option<StrategyParams> {
        None
    }
    fn on_stop(&mut self, broker: &mut B) {}
    /// Serialize this strategy's durable state (e.g. a market-maker's circuit-breaker counters +
    /// estimator) so it can survive a process restart, or `None` if it has nothing to persist —
    /// the default, so every existing strategy is unaffected. The runtime calls this at a
    /// checkpoint cadence it owns; the strategy owns the shape of the returned `Value` entirely.
    fn save_state(&self) -> Option<serde_json::Value> {
        None
    }
    /// Restore durable state previously returned by [`Strategy::save_state`] (e.g. on mount after
    /// a restart). Default no-op, so a strategy that never saves state is unaffected.
    fn load_state(&mut self, state: &serde_json::Value) {}
}

/// A boxed strategy is a strategy: every hook forwards to the inner `dyn Strategy<B>` through
/// one deref. This is what lets the live side box a strategy (`Box<dyn Strategy<LiveBroker> +
/// Send>`, per the module doc) and the backtest harness registry (`vike-backtest`'s
/// `harness::registry`) hand back a `Box<dyn Strategy<SimBroker>>` from a name lookup — neither
/// caller needs to know or care that the strategy underneath is boxed. NOT feature-gated: this
/// is a general blanket utility, not an HFT-only or live-only seam, and it does not touch the
/// generic monomorphized backtest hot loop (nothing calls it there unless a strategy is
/// deliberately boxed).
impl<B: Broker> Strategy<B> for Box<dyn Strategy<B>> {
    fn warmup(&self) -> usize {
        (**self).warmup()
    }
    fn on_start(&mut self, broker: &mut B) {
        (**self).on_start(broker)
    }
    fn on_bar(&mut self, broker: &mut B, bar: &Bar) {
        (**self).on_bar(broker, bar)
    }
    fn on_quote_tick(&mut self, broker: &mut B, q: &QuoteTick) {
        (**self).on_quote_tick(broker, q)
    }
    fn on_trade_tick(&mut self, broker: &mut B, t: &TradeTick) {
        (**self).on_trade_tick(broker, t)
    }
    fn on_order_book(&mut self, broker: &mut B, book: &L2Book) {
        (**self).on_order_book(broker, book)
    }
    fn on_schedule(&mut self, broker: &mut B, tag: &str) {
        (**self).on_schedule(broker, tag)
    }
    fn on_fill(&mut self, broker: &mut B, fill: &Fill) {
        (**self).on_fill(broker, fill)
    }
    fn on_feed_status(&mut self, broker: &mut B, status: FeedStatus) {
        (**self).on_feed_status(broker, status)
    }
    fn on_mark(&mut self, broker: &mut B, mark: &MarkTick) {
        (**self).on_mark(broker, mark)
    }
    fn on_reference_quote(&mut self, broker: &mut B, venue: &str, q: &QuoteTick) {
        (**self).on_reference_quote(broker, venue, q)
    }
    fn on_flow(&mut self, broker: &mut B, flow: FlowToxicity) {
        (**self).on_flow(broker, flow)
    }
    fn on_order_event(&mut self, broker: &mut B, event: &OrderLifecycle) {
        (**self).on_order_event(broker, event)
    }
    fn on_params_updated(&mut self, broker: &mut B, params: &StrategyParams) {
        (**self).on_params_updated(broker, params)
    }
    fn on_stop(&mut self, broker: &mut B) {
        (**self).on_stop(broker)
    }
    fn params(&self) -> Option<StrategyParams> {
        (**self).params()
    }
    fn save_state(&self) -> Option<serde_json::Value> {
        (**self).save_state()
    }
    fn load_state(&mut self, state: &serde_json::Value) {
        (**self).load_state(state)
    }
}

pub mod maker_params;
pub mod xemm_params;

pub use xemm_params::XemmParams;

// Re-exported so the split moved no public path: `vike_model::strategy::SpreadMakerParams` and
// `vike_model::SpreadMakerParams` (via lib.rs) both still resolve, as they did when these types
// were inline in this file.
pub use maker_params::{
    AsParams, HorizonMode, KappaMode, LadderLevel, LadderOffsetUnit, LadderParams,
    LadderSizeProfile, PriceDomain, QuoteStyle, RefreshTolerance, ReservationModel, RewardParams,
    SpreadMakerParams, SpreadModel, SpreadSource, ToxicityParams, VarianceMode,
};
/// The typed, serde-friendly payload of a live-parameter update — the runtime routes one of these
/// to a mounted strategy and calls [`Strategy::on_params_updated`]. A tagged union (one variant per
/// tunable strategy family) that a strategy MATCHES its own arm of and ignores the rest: a typed
/// alternative to a stringly-typed blob, and additive — a new strategy adds a variant without
/// disturbing the others (an externally-tagged enum, so a journal predating a variant simply never
/// carries it; old payloads still decode). It is journaled with its `Command`, so every variant is
/// serde.
// A live-parameter update is constructed one-at-a-time on a manual tune and journaled with its
// Command — never held in bulk on any hot path — so the size disparity between the maker-params
// variant and the smaller controller variant is immaterial; boxing would add an allocation per
// param update for no real benefit.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum StrategyParams {
    /// New tunables for the `SpreadMaker` market maker.
    SpreadMaker(SpreadMakerParams),
    /// New tunables for a [`crate::ControllerHarness`] mount (the controller → `PositionExecutor`
    /// live-params plane, position-executor stage 6): the harness post-exit cooldown + the
    /// controller's intent template (size / triple barrier) + the reference controller's decision
    /// knob. Consumed by [`crate::ControllerHarness::on_params_updated`], which hot-swaps the
    /// controller's tunables and the harness cooldown atomically and bumps its epoch. Additive —
    /// mirrors [`Self::SpreadMaker`]; a journal predating it never carries this variant.
    PositionController(crate::barrier::ControllerParams),
    /// New tunables for the CROSS-EXCHANGE maker (`vike_mm::XemmMaker`). Additive, exactly like the
    /// two above: this enum is externally tagged, so a journal predating this variant simply never
    /// carries it and old payloads still decode.
    ///
    /// The two SYMBOLS an xEMM trades are deliberately NOT in the payload — they are mount
    /// IDENTITY, held on the maker outside its config bag, so a live re-tune can widen the edge or
    /// cut size but can never repoint a leg while an unhedged position is open.
    Xemm(XemmParams),
}

#[cfg(test)]
mod order_lifecycle_tests {
    use super::*;
    use crate::events::{
        FillEvent, OrderAccepted, OrderCanceled, OrderDenied, OrderExpired, OrderFilled,
        OrderRejected, OrderSubmitted,
    };

    #[test]
    fn from_event_maps_the_nonfill_transitions() {
        assert_eq!(
            OrderLifecycle::from_event(&Event::OrderAccepted(OrderAccepted {
                client_order_id: "c1".into(),
                venue_order_id: None,
                ts: 0,
            })),
            Some(OrderLifecycle {
                client_order_id: "c1".into(),
                tag: None,
                kind: OrderEventKind::Accepted
            }),
        );
        assert_eq!(
            OrderLifecycle::from_event(&Event::OrderRejected(OrderRejected {
                client_order_id: "c2".into(),
                reason: "too big".into(),
                ts: 0,
            })),
            Some(OrderLifecycle {
                client_order_id: "c2".into(),
                tag: None,
                kind: OrderEventKind::Rejected { reason: "too big".into() },
            }),
        );
        assert_eq!(
            OrderLifecycle::from_event(&Event::OrderDenied(OrderDenied {
                client_order_id: "c3".into(),
                reason: "risk".into(),
                ts: 0,
            })),
            Some(OrderLifecycle {
                client_order_id: "c3".into(),
                tag: None,
                kind: OrderEventKind::Denied { reason: "risk".into() },
            }),
        );
        assert_eq!(
            OrderLifecycle::from_event(&Event::OrderCanceled(OrderCanceled {
                client_order_id: "c4".into(),
                reason: "pulled".into(),
                ts: 0,
            })),
            Some(OrderLifecycle {
                client_order_id: "c4".into(),
                tag: None,
                kind: OrderEventKind::Canceled { reason: "pulled".into() },
            }),
        );
        assert_eq!(
            OrderLifecycle::from_event(&Event::OrderExpired(OrderExpired {
                client_order_id: "c5".into(),
                ts: 0,
            })),
            Some(OrderLifecycle {
                client_order_id: "c5".into(),
                tag: None,
                kind: OrderEventKind::Expired
            }),
        );
    }

    #[test]
    fn from_event_ignores_fills_and_submit_echo() {
        // fills flow on_fill, not on_order_event
        let fe = FillEvent {
            trade_id: "t".into(),
            client_order_id: "c".into(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            last_qty: 1.0,
            last_px: 100.0,
            commission: 0.0,
            commission_asset: String::new().into(),
            liquidity_side: "taker".into(),
            ts: 0,
            mark_price: None,
            position_side: "BOTH".into(),
        };
        assert_eq!(
            OrderLifecycle::from_event(&Event::OrderFilled(OrderFilled {
                client_order_id: "c".into(),
                fill: fe.clone(),
                ts: 0,
            })),
            None,
        );
        assert_eq!(OrderLifecycle::from_event(&Event::Fill(fe)), None);
        // the local submit echo is not a venue outcome the strategy observes here
        assert_eq!(
            OrderLifecycle::from_event(&Event::OrderSubmitted(OrderSubmitted {
                client_order_id: "c".into(),
                ts: 0,
            })),
            None,
        );
    }

    #[test]
    fn order_lifecycle_round_trips_serde() {
        let lc = OrderLifecycle {
            client_order_id: "c9".into(),
            tag: Some("bid".into()),
            kind: OrderEventKind::Canceled { reason: "refresh".into() },
        };
        let json = serde_json::to_string(&lc).unwrap();
        assert_eq!(lc, serde_json::from_str::<OrderLifecycle>(&json).unwrap());
    }
}

/// A minimal recording [`Broker`] test double — the shared mock the strategy-layer tests drive
/// (`vike-strategy`'s position-executor + controller machine tests, and this crate's own
/// boxed-forwarding test below). It captures every market/limit submit and lets a test script
/// `now`/`price`/`position`; the rest of the `Broker` surface is inert. Consolidated here — the
/// [`Broker`] trait's home — from the near-identical per-crate copies (Phase 4 test-support
/// consolidation) so a new strategy-layer test inherits it instead of re-declaring a broker. Gated
/// behind `test-support` (compiled only under `cfg(test)` or when a consumer enables
/// `vike-model/test-support` as a dev-dependency), so a default build never sees it.
#[cfg(any(test, feature = "test-support"))]
#[derive(Default)]
pub struct MockBroker {
    /// Engine clock returned by [`Broker::now`], epoch ms.
    pub now: i64,
    /// Last mark returned by [`Broker::price`].
    pub px: f64,
    /// Signed position returned by [`Broker::position`].
    pub pos: f64,
    /// Every [`Broker::submit_market`] captured as `(symbol, side, qty)`.
    pub markets: Vec<(String, i32, f64)>,
    /// Every [`Broker::submit_limit`] captured as `(symbol, side, qty, price)`.
    pub limits: Vec<(String, i32, f64, f64)>,
}

#[cfg(any(test, feature = "test-support"))]
impl MockBroker {
    /// A broker whose clock starts at `now` — for tests that must stamp a submit time BEFORE the
    /// first `start` (so refresh/backoff timers key off it). Avoids the `field_reassign_with_default`
    /// lint an immediate `b.now = ..` after `default()` would trip.
    pub fn at(now: i64) -> Self {
        MockBroker { now, ..Default::default() }
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Broker for MockBroker {
    fn submit_market(&mut self, symbol: &str, side: i32, qty: f64) {
        self.markets.push((symbol.to_string(), side, qty));
    }
    fn submit_limit(&mut self, symbol: &str, side: i32, qty: f64, price: f64) {
        self.limits.push((symbol.to_string(), side, qty, price));
    }
    fn position(&self, _symbol: &str) -> f64 {
        self.pos
    }
    fn price(&self, _symbol: &str) -> f64 {
        self.px
    }
    fn equity(&self) -> f64 {
        0.0
    }
    fn bars(&self, _symbol: &str) -> &[Bar] {
        &[]
    }
    fn index(&self) -> usize {
        0
    }
    fn now(&self) -> i64 {
        self.now
    }
}

#[cfg(test)]
mod boxed_strategy_tests {
    use super::*;

    /// Records which hooks fired via a SHARED counter (`Rc<Cell<_>>`), so the boxed-forwarding
    /// test can keep an outside handle and observe what the strategy INSIDE the box actually saw
    /// — proving the blanket impl really forwards, not just that the box compiles.
    #[derive(Default, Clone)]
    struct RecordingStrategy {
        bar_calls: std::rc::Rc<std::cell::Cell<usize>>,
        quote_calls: std::rc::Rc<std::cell::Cell<usize>>,
        last_bar_close: std::rc::Rc<std::cell::Cell<f64>>,
    }

    impl Strategy<MockBroker> for RecordingStrategy {
        fn on_bar(&mut self, _broker: &mut MockBroker, bar: &Bar) {
            self.bar_calls.set(self.bar_calls.get() + 1);
            self.last_bar_close.set(bar.close);
        }
        fn on_quote_tick(&mut self, _broker: &mut MockBroker, _q: &QuoteTick) {
            self.quote_calls.set(self.quote_calls.get() + 1);
        }
    }

    fn bar(close: f64) -> Bar {
        Bar {
            ts: 0,
            open: close,
            high: close,
            low: close,
            close,
            volume: 0.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        }
    }

    fn quote() -> QuoteTick {
        QuoteTick {
            ts: 0,
            local_ts: 0,
            bid: 1.0,
            ask: 1.0,
            bid_size: 0.0,
            ask_size: 0.0,
            symbol: String::new(),
        }
    }

    #[test]
    fn boxed_strategy_forwards_on_bar_and_on_quote_tick_to_the_inner_strategy() {
        let recorder = RecordingStrategy::default();
        // `Box<dyn Strategy<MockBroker>>` must itself satisfy `Strategy<MockBroker>` — this line
        // only compiles because of the blanket impl.
        let mut boxed: Box<dyn Strategy<MockBroker>> = Box::new(recorder.clone());
        let mut broker = MockBroker::default();

        boxed.on_bar(&mut broker, &bar(123.5));
        boxed.on_quote_tick(&mut broker, &quote());
        boxed.on_quote_tick(&mut broker, &quote());

        // `recorder`'s shared cells see what the strategy INSIDE the box observed.
        assert_eq!(recorder.bar_calls.get(), 1);
        assert_eq!(recorder.quote_calls.get(), 2);
        assert_eq!(recorder.last_bar_close.get(), 123.5);
    }

    #[test]
    fn boxed_strategy_forwards_save_and_load_state() {
        // a tiny strategy that stores a counter in its state
        struct S {
            n: i64,
        }
        impl<B: Broker> Strategy<B> for S {
            fn save_state(&self) -> Option<serde_json::Value> {
                Some(serde_json::json!({ "n": self.n }))
            }
            fn load_state(&mut self, state: &serde_json::Value) {
                self.n = state["n"].as_i64().unwrap_or(0);
            }
        }
        let mut boxed: Box<dyn Strategy<MockBroker>> = Box::new(S { n: 7 });
        // save through the box must reach S, not the default None
        assert_eq!(boxed.save_state(), Some(serde_json::json!({ "n": 7 })));
        // load through the box must reach S, not the default no-op
        boxed.load_state(&serde_json::json!({ "n": 42 }));
        assert_eq!(boxed.save_state(), Some(serde_json::json!({ "n": 42 })));
    }
}
