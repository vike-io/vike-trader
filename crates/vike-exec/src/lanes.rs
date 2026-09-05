//! The producer-side ingest lanes — the types venue adapters hold (fire-and-forget senders +
//! payloads). The consumer (the single-writer core thread) lives in vike-core; this split is
//! why bridges depend on vike-exec but never on vike-core (crate-reorg spec D3/Phase 2).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use ustr::Ustr;

use vike_model::events::Event;
use vike_model::{
    Bar, FeedStatus, FillReport, FlowToxicity, L2Book, OrderRequest, OrderStatusReport,
    PositionStatusReport, QuoteTick, StrategyParams, TradeTick,
};

use crate::execution_engine::ReconcileSnapshot;
use crate::recon::{BalanceTol, ReconPolicy};
use crate::risk::TradingState;

/// The core thread has exited (shutdown or channel closed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreGone;

/// Every ORDER-SCOPED instruction the core accepts — the ONE write contract. Both the external
/// Command path and the LiveBroker strategy drain lower into the core's single `apply_intent`
/// site. Serde: journaled inside `Ingest::Command`. Payloads boxed like `Command` variants.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub enum OrderIntent {
    /// Empty `client_order_id` ⇒ the runtime mints one; non-empty respected.
    Submit(Box<OrderRequest>),
    SubmitBatch(Vec<OrderRequest>),
    Cancel(String),
    CancelBatch(Vec<String>),
    Modify {
        client_order_id: String,
        new_qty: Option<f64>,
        new_price: Option<f64>,
    },
    /// Actively re-confirm one order's status with the venue (order-scoped `QueryOrder`).
    Confirm(String),
    /// None/None = ALL engines + clear ALL conditional books; Some(v)/Some(s) = that engine +
    /// clear that (venue,symbol) book; Some(v)/None = that engine + clear that venue's books;
    /// None/Some = ignored (surfaced to recent-events).
    MassCancel {
        venue: Option<String>,
        symbol: Option<String>,
    },
    /// Close the (venue, symbol) net position: reduce-only market for |position|, resolved at
    /// apply time. No-op when flat.
    Flatten {
        venue: String,
        symbol: String,
    },
    /// PANIC BUTTON — "get me out": cancel every live order, then flatten every open position.
    ///
    /// A COMPOUND verb: it carries no state of its own and mints no orders directly. The runtime
    /// EXPANDS it, at apply time, into the existing primitive intents — one
    /// [`Self::MassCancel`] is applied FIRST, then one [`Self::Flatten`] per non-flat position
    /// (derived from the POST-cancel account, in the `Account`'s own (venue, symbol,
    /// position_side) insertion order) — and lowers each through the SAME `apply_intent` site, so
    /// mint → RiskGate → client stays structural (a flatten is `reduce_only`, so it is permitted
    /// under `TradingState::Reducing`).
    ///
    /// **USABLE FROM A HALTED CORE — no precondition.** `RiskGate`'s kill switch admits a
    /// POSITION-COVERED reduce under `Halted` (`vike_model::is_covered_reduce`), which is exactly
    /// the shape [`Self::Flatten`] mints: side opposite the position, qty `|position|`. So the
    /// mass-cancel runs AND every flatten leg is submitted, under `Active`, `Reducing` and `Halted`
    /// alike. The runtime still logs a warning + a `recent` note when the exit runs under a halt —
    /// now saying that it is PROCEEDING.
    ///
    /// ⚠ This is the REVERSE of the precondition documented here for this verb's whole life. The
    /// gate used to deny every order under `Halted`, `reduce_only` included, so the panic button was
    /// disarmed in exactly the situations that reach `Halted` on their own — the dead-man's switch
    /// and the safe-state sweep both Halt — and an operator had to
    /// `Command::SetTradingState(TradingState::Active)` first, un-halting the strategy that got them
    /// there in order to escape it. A kill switch must stop opening risk, never trap you in it.
    ///
    /// **BEST-EFFORT, NOT ATOMIC.** Against a real venue the mass-cancel is fire-and-forget over
    /// the adapter's actor thread; its ACKs land asynchronously, possibly after the flatten market
    /// orders are already on the wire. Ordering the legs is a local best effort — a resting order
    /// can still fill after a flatten leg. Re-issue the exit if the board is not flat afterwards.
    ///
    /// `venue: None` = every engine (global exit); `Some(v)` = only that venue's engine.
    /// A flat, order-free book is a no-op beyond the mass-cancel.
    MarketExit {
        venue: Option<String>,
    },
    /// Entry + protective SL + TP as one contingent batch (runtime mints 3 coids, wires OTO/OCO).
    Bracket(Box<vike_model::BracketSpec>),
    /// Arm an emulated stop/trailing in the core-owned ConditionalBook (ARM bypasses the gate;
    /// only the FIRE crosses it).
    ArmConditional(ConditionalIntent),
    /// Disarm ONE emulated conditional by the `arm_id` the runtime minted when it was armed —
    /// the individual-cancel primitive (`MassCancel` stays the coarse verb; emulator PR-2). The
    /// runtime drops exactly that arm from its ConditionalBook (the book is keyed by `arm_id`,
    /// so exactly-one is structural), journaling a `ConditionalDisarmed` record write-ahead of
    /// the book mutation. An unknown/stale id is a LOUD no-op (surfaced to recent-events), never
    /// a panic and never silent. Like ARM, a disarm bypasses the RiskGate — removing a resting
    /// emulated arm is not an order reaching a venue.
    ///
    /// Serde: a NEW externally-tagged variant — old journals replay unchanged; a journal
    /// containing `DisarmConditional` needs the new binary (the standing rule).
    DisarmConditional {
        arm_id: String,
    },
    /// An atomic multi-leg order at a SIGNED net price — ONE venue order, ONE coid, ONE
    /// `ManagedOrder` (the venue is the group manager; v1 is venue-native only). The runtime mints
    /// the single coid and lowers this through [`vike_model::build_combo`], exactly as `Bracket`
    /// lowers through `build_bracket`.
    ///
    /// Boxed like the other payloads. Serde: a NEW externally-tagged variant — old journals replay
    /// unchanged; a journal containing `Combo` needs the new binary (the standing rule).
    ///
    /// TODO(PR-2): the `apply_intent` lowering. The hand-off contract, inlined here because it is
    /// the only place it is written down:
    /// 1. VENUE CAPS — a venue that cannot do combos must yield a SYNTHESIZED terminal
    ///    `Event::OrderRejected` (never a silent drop: every order reaches exactly one terminal).
    /// 2. ONE minted coid via the runtime's `ClientOrderIdGenerator`, then `build_combo` +
    ///    `append_minted_submit` — the `Bracket` precedent, but ONE child, not three.
    /// 3. RISK — AVAILABLE, NOT YET WIRED: [`crate::RiskGate::check_combo`] is the entry point.
    ///    It crosses every leg atomically off each leg's OWN mark, accumulating margin and
    ///    projected position across legs, denies the whole combo naming the first failing leg,
    ///    and consumes ONE throttle slot only after all legs pass. **The lowering MUST call it
    ///    instead of `check` whenever `combo_legs` is non-empty. Until it does, a combo falls
    ///    through to `check` and is sized on the SIGNED net — which is wrong: a credit combo's
    ///    net is negative (see `ComboSpec::net_limit`), so it must never be sized on the net,
    ///    only on `net_limit.abs()` or a per-leg gross notional.**
    /// 4. PR-3 = paper fills (`combo_net_cross`), PR-4 = the Deribit adapter resolving the combo
    ///    instrument via `private/create_combo` and substituting it into the empty `symbol`.
    Combo(Box<vike_model::ComboSpec>),
}

/// `ArmConditional` payload — `price` = fixed stop trigger, `trail` = trailing distance (extreme
/// seeds from the current mark at apply time; refused into recent-events without a mark).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ConditionalIntent {
    pub venue: String,
    pub symbol: String,
    pub side: i32,
    pub qty: f64,
    pub price: Option<f64>,
    pub trail: Option<f64>,
    /// Requested trigger price SOURCE the emulated arm evaluates against (`vike_model::TriggerBy`):
    /// `None`/`Some(Last)` = trade/bar prices (today's law, byte-identical), `Some(Mark)` = the
    /// core's mark lane, `Some(Index)` = REFUSED at apply time (the core has no index lane —
    /// surfaced to recent-events, never silently evaluated off a different series).
    ///
    /// Serde: `default` + `skip_serializing_if` `None`, so every pre-existing journal `Cmd` /
    /// `StrategySubmit` record stays byte-identical and old journals replay unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_by: Option<vike_model::TriggerBy>,
}

/// GUI/session commands. Everything an outside thread may ask of the core.
/// Payloads are boxed so the queued message stays small (clippy large_enum_variant).
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub enum Command {
    /// The order-write contract — every order-scoped verb (see [`OrderIntent`]). Both the external
    /// Command path and the LiveBroker strategy drain lower into the core's single `apply_intent`
    /// site, so mint + RiskGate are STRUCTURAL on every order path.
    Order(OrderIntent),
    /// LIVE PARAMETER update for a mounted strategy — the live-parameter plane (RUST-NATIVE, no
    /// Python twin). Routes typed [`StrategyParams`] to the ONE mount on the payload's
    /// `(venue, symbol, interval)` and calls its `Strategy::on_params_updated` hook, so a running
    /// strategy is re-tuned WITHOUT unmounting (which would lose queue position). INTERNAL lane
    /// plumbing (like `Ingest::StreamStatus`), NOT the serde `Event` wire union. A session verb,
    /// not an order — an OCCASIONAL control command (a GUI/operator re-tune), never a market
    /// message, so it is off the p99<10µs event fold and touches NO OMS state. Boxed to keep the
    /// queued `Command` small.
    UpdateParams(Box<ParamsUpdate>),
    SetTradingState(TradingState),
    /// LIVE per-symbol leverage / margin change (RUST-NATIVE). An OCCASIONAL operator/GUI control
    /// command (like `SetTradingState`) — off the p99 event fold; writes `RiskLimits.im_by_symbol`
    /// on the target venue's engine so a running engine is re-leveraged without a restart. Boxed
    /// to keep the queued `Command` small.
    SetMargin(Box<MarginUpdate>),
    ApplySnapshot(Box<ReconcileSnapshot>),
    /// Fetched venue reconciliation reports awaiting fold-thread diff/resolve (the reconcile
    /// runtime driver's write verb — `vike_core::recon_manager`). The reconcile MANAGER thread
    /// does ONLY the blocking REST fetch off the fold, then enqueues this; the single-writer fold
    /// thread reads its OWN engine's `local_view()`, runs the PURE `recon::diff` + `recon::resolve`,
    /// and folds the synthesized events through the SAME `on_event` path real venue events use — so
    /// there is NO cross-thread local-state read and NO staleness window. Sibling of
    /// [`Command::ApplySnapshot`] (venue-truth snapshot seed): this one carries raw REPORTS the
    /// core diffs, that one carries an already-diffed net snapshot. Boxed (large payload) like the
    /// other command variants. An OCCASIONAL command (startup, and Task 16's interval cadence),
    /// never the per-message hot fold.
    ReconcileReports(Box<ReconcileReports>),
    /// Operator approval of one held Quarantine/Hybrid-quarantined reconcile alert (Task 17): the
    /// fold thread folds its `proposed_events` through the SAME `on_event` path real venue events
    /// use, then removes it from the held store — the write half of `CoreSnapshot.recon.alerts`.
    /// The `u64` is a `ReconAlertView.id`, assigned at runtime by `reconcile_reports` when the
    /// alert was first held. IDs are NOT stable across a journal replay (a fresh process run
    /// restarts the id counter and the held-alert store empty) — that is acceptable because
    /// `ConfirmRecon` only matters LIVE: an operator confirms an id off the CURRENT snapshot, and
    /// a replayed `ConfirmRecon` for an id from a prior run harmlessly hits the same unknown-id
    /// no-op path a stale/duplicate GUI click would (surfaced to recent-events, never a panic or
    /// silent corruption).
    ConfirmRecon(u64),
    /// RUNTIME strategy MOUNT (split-plane B5): add one strategy mount to the running core's slot
    /// vector WITHOUT a restart. The payload is a serializable SPEC, not a strategy object — the
    /// core resolves it through the composition root's injected factory
    /// (`vike_core::CoreConfig::strategy_factory`; vike-exec sits below the strategy crates and
    /// can name no `Strategy` type). An OCCASIONAL control command (an operator/GUI verb, like
    /// [`Command::UpdateParams`]), never a market message — off the p99<10µs event fold. Every
    /// failure (duplicate mount id, unknown venue, no factory, resolve error) surfaces as a
    /// REFUSAL note in recent-events, never a panic — the spawn-time duplicate PANIC stays what it
    /// is (a configuration fault at assembly), this is the runtime twin where the process must
    /// keep trading. ⚠ Deliberately NOT journaled (see the `journaled` match in
    /// `vike_core::runtime`'s `dispatch`): the mount-lifecycle is session topology, not order
    /// state — the mount-less replay core could never re-resolve a strategy, so a journaled
    /// mount could only replay as a refusal (the journal's own `StrategySubmit` provenance still
    /// names the mount id for attribution). Restart survival is DAEMON-LEVEL STATE instead: the
    /// core's arm records the spec in the `vike_core::mount_topology` sidecar (gated on
    /// `CoreConfig::state_dir`; unmount removes it), and the composition root replays that file
    /// at startup by re-sending this very command — same factory, same refusals (vike-tradehub's
    /// `mount_factory`'s `resurrect_runtime_mounts`). Boxed like every other command payload.
    MountStrategy(Box<MountSpec>),
    /// RUNTIME strategy UNMOUNT (split-plane B5): remove the mount whose MOUNT ID matches
    /// `controller_id` (the explicit [`MountSpec::controller_id`] when the mount was given one,
    /// else the derived `{venue}__{symbol}__{interval}` id — `vike_core::strategy_state`'s
    /// `mount_id_with` is the identity law). Same command discipline as
    /// [`Command::MountStrategy`]: occasional, refusal-on-unknown-id, not journaled. The resting-
    /// order safety decision (cancel the mount's attributed live orders BEFORE removal) is the
    /// core arm's contract — documented there, not here.
    UnmountStrategy {
        controller_id: String,
    },
    Shutdown,
}

/// The [`Command::MountStrategy`] payload: WHERE the mount sits (`venue`/`symbol`/`interval` — the
/// same addressing triple every other strategy verb uses), WHO it is (`controller_id`, optional —
/// required when a mount on the same triple already exists, exactly like spawn-time config), and
/// WHAT to run — the profile `[strategy]` vocabulary verbatim: a registry `name` XOR a `rhai`
/// script path (docs/decisions/0024-rhai-strategies-live.md), plus the free-form `params` table
/// carried as JSON (the same delegate-don't-mirror idiom as the wire's `UpdateParams`). The spec
/// is fully serde so it can ride the wire; RESOLUTION (name → `Box<dyn Strategy>`) happens in the
/// core via the injected factory, which the composition root builds from its own strategy
/// machinery.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MountSpec {
    /// Target venue — must name an engine the core already runs (a mount cannot conjure one).
    pub venue: String,
    /// The mount's own symbol.
    pub symbol: String,
    /// The mount's bar-series interval (e.g. `"1m"`).
    pub interval: String,
    /// **WHICH ACCOUNT of [`Self::venue`] this mount trades on.** `None` — every mount spec that
    /// existed before this field, and every one a pre-change file or wire frame carries — is the
    /// venue's DEFAULT account: the same engine, the same route key, byte-identical.
    ///
    /// `Some(label)` names a second account, and `vike_core`'s runtime mount REFUSES it by name
    /// when this core runs no engine for it rather than falling through to the default account.
    /// That refusal is the point of the field: a strategy executing on an account its author did
    /// not name is silent and unrecoverable.
    ///
    /// ⚠ Serde goes through `AccountLabel`'s hand-written impls, which are `AccountLabel::parse`
    /// verbatim — so `"DEFAULT"`, an underscore or an over-long name are refused AT THE WIRE rather
    /// than by a check somebody has to remember at each deserialization site. `skip_serializing_if`
    /// keeps an account-less spec's bytes exactly as they were.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<vike_model::account_keys::AccountLabel>,
    /// Optional explicit mount identity (`StrategyMount::controller_id` semantics): `None` derives
    /// the legacy `{venue}__{symbol}__{interval}` id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller_id: Option<String>,
    /// Registry strategy name (`[strategy] name = "..."`). Exactly one of `name`/`rhai`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Rhai script PATH (`[strategy] rhai = "..."`). Exactly one of `name`/`rhai`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rhai: Option<String>,
    /// The `[strategy.params]` table, as a JSON object (default `{}`). The factory validates it
    /// with the same refusals a profile load applies (unknown/mistyped keys refuse, never mount at
    /// a compiled default).
    #[serde(default = "default_mount_params")]
    pub params: serde_json::Value,
}

fn default_mount_params() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

/// The [`Command::ReconcileReports`] payload: the three venue report kinds a reconcile pass
/// fetched, plus the `venue` this pass READ (the canonical exchange id — the health-probe key, the
/// label on every note and log line, and the identity half of a held alert's dedup row), the
/// [`Self::route_key`] that selects the target ENGINE, the `since` lower bound the reports
/// were pulled with (diagnostics/forward use), and the `policy` that decides fold-vs-quarantine
/// per divergence kind. Serde like every journaled command payload (its `ReconPolicy` gained a
/// serde derive for exactly this). Built on the reconcile-manager thread; consumed on the fold
/// thread via `recon::diff`/`recon::resolve`.
///
/// ⚠ **`venue` and `route_key` are two facts, and this payload used to carry only the first.** The
/// fold thread (`vike_core::runtime::CoreThread::reconcile_reports`) resolves the engine this
/// pass's divergences fold into, so with one field it had to answer BOTH questions from one
/// string, and neither answer survives a second account of one exchange: spell it canonically and
/// the second account's reports diff against the FIRST account's local view and fold its venue
/// truth into the first account's book (under `hybrid`, `PositionDrift` auto-applies — it rewrites
/// position size onto the other account's number and books realized PnL at the other account's avg
/// price); spell it as the route key and the manager's per-venue health gate
/// (`ReconManager::should_reconcile`) reads an unknown venue, which it maps to `Healthy` — so the
/// degraded-feed suppression silently stops applying to that account.
///
/// Task 3 (balance activation): `balance` is the venue's authoritative cash
/// ([`crate::recon::ReconClient::fetch_balance`]), fetched off-fold alongside the three report
/// kinds. `None` (the default trait impl, or a fetch error) means "not reported" — the fold
/// thread's `reconcile_reports` leaves `Account` balance/mode untouched in that case, so an
/// un-wired or failing balance fetch stays byte-identical to pre-Task-3 behavior.
///
/// `generate_missing_orders` mirrors `vike_core::ReconConfig::generate_missing_orders` for this
/// one pass (the manager thread copies its config value in verbatim at build time) — it rides
/// ALONGSIDE `policy` because the fold thread's `reconcile_reports` has no other access to the
/// driver's `ReconConfig` (a different struct, built on a different thread). `false` (the
/// default) is byte-identical to before this field existed: `recon::resolve` never synthesizes
/// `UnknownOrder` adoption events.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReconcileReports {
    pub venue: String,
    pub since: i64,
    pub orders: Vec<OrderStatusReport>,
    pub fills: Vec<FillReport>,
    pub positions: Vec<PositionStatusReport>,
    pub policy: ReconPolicy,
    pub balance: Option<f64>,
    /// `#[serde(default)]` (= `false`, the inert value): journals written before this field
    /// existed must keep replaying — a journaled command payload is a persisted schema.
    #[serde(default)]
    pub generate_missing_orders: bool,
    /// First-class cash reconcile (Feature 2, `VIKE_RECONCILE_BALANCE`): mirrors
    /// `vike_core::ReconConfig::reconcile_balance` for this pass. `false` (the `#[serde(default)]`,
    /// so pre-existing journals replay) ⇒ the fold thread takes the LEGACY silent authoritative
    /// balance seed (byte-identical to before this field). `true` ⇒ venue cash is DIFFED against
    /// the realized-PnL-corrected local balance and any drift routed through `policy` (quarantined
    /// by default — a surprise cash move is never auto-folded). Rides alongside `balance`/`policy`
    /// because the fold thread has no other view of the driver's `ReconConfig`.
    #[serde(default)]
    pub reconcile_balance: bool,
    /// The env-tuned money tolerance the fold thread's `diff_balance` compares against (Feature 2 —
    /// only read when `reconcile_balance` is `true`). Mirrors `vike_core::ReconConfig::balance_tol`
    /// for this pass; rides the payload for the same reason `policy`/`reconcile_balance` do (the
    /// fold thread has no other view of the driver's `ReconConfig`). `#[serde(default)]` =
    /// [`BalanceTol::default`] so pre-existing journals — and any pass that never set it — replay
    /// with the conservative constant default, byte-identical to before this field existed.
    #[serde(default)]
    pub balance_tol: BalanceTol,
    /// Which ENGINE this pass's divergences fold into — `None` meaning "[`Self::venue`]'s sole
    /// account in this process", which is what every producer in this tree means and what every
    /// journal written before this field existed meant. `#[serde(default)]` for that second
    /// reason: a journaled command payload is a persisted schema, and a replayed pre-field pass
    /// must route exactly where it routed live.
    ///
    /// Read through [`Self::route`], never directly — that accessor is what makes `None` mean the
    /// venue rather than the empty string, and it hands back a [`crate::RouteKey`], which is the
    /// only thing the router accepts. `Some` is unreachable today (nothing mounts a second account
    /// of one exchange); it exists so the fold thread never has to GUESS which of the two jobs
    /// `venue` was doing.
    #[serde(default)]
    pub route_key: Option<String>,
}

impl ReconcileReports {
    /// The engine this pass routes to. `None` ⇒ [`Self::venue`]'s sole account — the inert
    /// default, and the only shape this workspace produces.
    ///
    /// ⚠ Route with THIS, never with `&self.venue`: the two are equal today and that is precisely
    /// why the wrong one cannot be spotted by reading. `vike_core`'s `confirm_recon` re-resolves
    /// the same engine from a HELD alert minutes later, so the choice outlives the pass that made
    /// it.
    #[inline]
    pub fn route(&self) -> crate::RouteKey<'_> {
        match &self.route_key {
            Some(k) => crate::RouteKey::declared(k),
            None => crate::RouteKey::sole_account_of(&self.venue),
        }
    }
}

/// The payload of a [`Command::UpdateParams`] live-parameter update. `(venue, symbol, interval)`
/// names the target mount EXACTLY (the same key the bar path resolves a mount by), and `params`
/// is the typed [`StrategyParams`] the mount's `on_params_updated` hook consumes. Serde like every
/// journaled command payload. Mirrors the `StreamStatusUpdate` shape (context fields + a typed
/// payload carried in a boxed `Command`/`Ingest` variant).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ParamsUpdate {
    pub venue: String,
    pub symbol: String,
    pub interval: String,
    pub params: StrategyParams,
}

/// The payload of a [`Command::SetMargin`] live leverage change: set `symbol`'s initial-margin
/// fraction (`1/leverage`) on `venue`'s engine (`RiskLimits.im_by_symbol`). Other engines and
/// symbols are untouched. Serde like every journaled command payload.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MarginUpdate {
    pub venue: String,
    pub symbol: String,
    pub im_requirement: f64,
}

/// One ingest message. `Market` is a marker — the payload rides the conflation slot.
/// Bar lanes (R5c): `BarSeed`/`BarClose` are LOSSLESS (a missed close = a hole in the
/// series); forming-bar updates conflate with the ticks (latest-wins per series key).
// large_enum_variant: Event IS the dominant hot-path variant; boxing it would trade a
// 240-byte move for a per-event heap allocation + pointer chase on exactly the path the
// p99<10µs gate protects. Commands (rare) are already boxed.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub enum Ingest {
    Event(Event),
    Command(Command),
    Market,
    /// REST warmup backfill: replace the whole series (boxed — rare, large)
    BarSeed(Box<BarSeed>),
    /// a bar CLOSED — append to the series (lossless lane)
    BarClose(Box<BarUpdate>),
    /// L2/tick lanes (R8 HFT track) — lossless, per-update strategy dispatch. Boxed so the
    /// hot `Event`/`Market` path keeps a small enum. Net-new Rust surface (no Python twin);
    /// they touch NO parity-gated OMS fold path, only the mounted strategy's tick handlers.
    Quote(Box<QuoteUpdate>),
    Trade(Box<TradeUpdate>),
    Book(Box<BookUpdate>),
    /// Opt-in stuck-order watchdog tick (audit C3): sweep orders stuck pre-ack past
    /// `submit_ack_timeout`. No payload; only enqueued when the watchdog is enabled.
    Watchdog,
    /// Feed-health status CHANGE (net-hardening §B) — the disconnect/stale/recover signal a
    /// mounted strategy reacts to (dispatched to `Strategy::on_feed_status`). An OCCASIONAL
    /// control event: the producer (a feed's stream-health machine → its `LiveDataSink`
    /// boundary) fires it ONLY on a `StreamStatus` transition, so it rides this lane without
    /// adding any per-market-message traffic — the p99<10µs event fold is untouched (this arm
    /// touches NO OMS fold path). Boxed to keep the hot `Event`/`Market` path's enum small; NOT
    /// journaled (it is advisory feed telemetry, not command state). Net-new Rust surface.
    StreamStatus(Box<StreamStatusUpdate>),
    /// Per-side FLOW-TOXICITY update on the control lane (RTDS wallet-toxicity guard) — the
    /// widen/cut signal a mounted market maker reacts to (dispatched to `Strategy::on_flow`). An
    /// OCCASIONAL control event: the producer (a toxicity aggregator over the wallet-attributed
    /// activity tape) fires it at toxicity cadence, NOT per market message, so it rides this lane
    /// without adding per-market-message traffic — the p99<10µs event fold is untouched (this arm
    /// touches NO OMS fold path). Boxed to keep the hot `Event`/`Market` path's enum small; NOT
    /// journaled (advisory telemetry, not command state). Net-new Rust surface — the exact twin of
    /// [`Ingest::StreamStatus`].
    Flow(Box<FlowUpdate>),
}

/// A per-side FLOW-TOXICITY update on the control lane (RTDS wallet-toxicity guard). Carries
/// `(venue, symbol)` context so the core can route it to the strategy mounted on that
/// `(venue, symbol)` — the toxic tape's `asset` IS the mounted token, so this routes by EXACT
/// `(venue, symbol)` like [`StreamStatusUpdate`] — plus the vike-model-native [`FlowToxicity`] its
/// `on_flow` hook receives. The exact twin of `StreamStatusUpdate`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FlowUpdate {
    pub venue: String,
    pub symbol: String,
    pub flow: FlowToxicity,
}

/// A feed-health status CHANGE on the control lane (net-hardening §B). Carries `(venue, symbol,
/// stream)` context so the core can route the change to the strategy mounted on that
/// `(venue, symbol)`, plus the vike-model-native [`FeedStatus`] its `on_feed_status` hook
/// receives. `stream` names the lane the way the data verbs do (an interval like `"1m"` for bar
/// streams, `"quotes"`/`"trades"`/`"book"` for tick lanes) — preserved for diagnostics/future
/// per-lane routing; the current dispatch routes by `(venue, symbol)` only.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StreamStatusUpdate {
    pub venue: String,
    pub symbol: String,
    pub stream: String,
    pub status: FeedStatus,
}

/// L1 quote update on the tick lane. Carries (venue, symbol) explicitly — `QuoteTick.symbol`
/// is empty on single-symbol paths and there is no venue on the tick itself.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct QuoteUpdate {
    pub venue: String,
    pub symbol: String,
    pub quote: QuoteTick,
}

/// Executed-trade update on the tick lane. (venue, symbol) explicit — see [`QuoteUpdate`].
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TradeUpdate {
    pub venue: String,
    pub symbol: String,
    pub trade: TradeTick,
}

/// L2 book update on the tick lane. `L2Book` carries neither venue/symbol nor a ts, so both
/// ride the wrapper; the dispatch clock stamps `now`.
///
/// **The book rides behind an [`Arc`], NOT by value** (perf audit finding #1 — the largest single
/// allocation source on the tick path). A venue pump keeps ONE standing book and folds each depth
/// delta into it; carrying the book by value meant every applied delta deep-cloned two whole
/// `BTreeMap`s (up to a 1000-level seed on binance) on the pump thread AND paid the matching full
/// drop on the single-writer core thread — the very thread the `p99 < 10µs` gate measures. With
/// an `Arc` the send is a refcount bump and the core's drop is a refcount decrement; a pump mutates
/// its own copy through [`Arc::make_mut`], so it deep-clones ONLY while a consumer still holds the
/// previous handle (copy-on-write) and that clone lands on the PUMP thread, never on the fold.
/// This mirrors the backtest engine's long-standing `Rc<L2Book>` replay slot
/// (`vike_backtest::engine`'s `apply_book_event`).
///
/// SERDE (this payload is a variant of the journaled `Ingest` enum, so it must keep round-tripping
/// — `Ingest::Book` itself is NOT journaled, but the enum's derive is all-or-nothing): the `book`
/// field goes through the `ser_book`/`de_book` pair below, which call `L2Book`'s OWN
/// `Serialize`/`Deserialize`, so the wire bytes are IDENTICAL to the pre-`Arc` shape and no
/// `serde/rc` feature is involved (the workspace does not enable it, so `Arc<T>: Serialize` does
/// not even exist here). Pinned byte-for-byte by
/// `arc_book_field_serializes_exactly_like_a_bare_book`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BookUpdate {
    pub venue: String,
    pub symbol: String,
    #[serde(serialize_with = "ser_book", deserialize_with = "de_book")]
    pub book: Arc<L2Book>,
}

/// Serialize `Arc<L2Book>` exactly as a bare `L2Book` — see [`BookUpdate`]'s serde note.
/// Hand-written rather than switching on serde's `rc` feature: that feature is off workspace-wide,
/// and enabling it would add `Rc`/`Arc` impls to EVERY derive in the tree rather than at this one
/// pinned site.
fn ser_book<S: serde::Serializer>(book: &Arc<L2Book>, s: S) -> Result<S::Ok, S::Error> {
    serde::Serialize::serialize(&**book, s)
}

/// The `ser_book` inverse: read a bare `L2Book` and wrap it. A round-trip always yields a FRESH
/// allocation — `Arc` identity is not preserved, and need not be: the book is value state on this
/// lane, never an identity.
fn de_book<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Arc<L2Book>, D::Error> {
    serde::Deserialize::deserialize(d).map(Arc::new)
}

/// A bar-series key: (venue, symbol, interval), e.g. ("binance", "BTCUSDT", "1m").
pub type SeriesKey = (String, String, String);

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BarSeed {
    pub venue: String,
    pub symbol: String,
    pub interval: String,
    pub bars: Vec<Bar>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BarUpdate {
    pub venue: String,
    pub symbol: String,
    pub interval: String,
    pub bar: Bar,
}

/// One bar series as the snapshot exposes it: closed bars behind an `Arc` (pointer-copy
/// per snapshot; the Vec is rebuilt only on a bar CLOSE — ~1/minute) + the small forming
/// bar copied per publish. The GUI converts to its render model at this boundary.
#[derive(Debug, Clone, Default)]
pub struct BarSeries {
    pub closed: Arc<Vec<Bar>>,
    pub forming: Option<Bar>,
}

/// Latest price tick for (venue, symbol) — the R5c live-feed seam. Which SLOT it fills downstream
/// depends on the publish verb: [`MarketSender::publish`] carries the venue's REAL mark price
/// (the `PriceBoard` MARK slot), [`MarketSender::publish_bar_close`] a kline feed's candle-close
/// snapshot (the BAR-CLOSE slot) — mark-slot semantics, law-map A1.
#[derive(Debug, Clone)]
pub struct MarketTick {
    pub venue: String,
    pub symbol: String,
    pub px: f64,
    pub ts: i64,
}

/// Per-symbol latest-wins slots (plan §1 pins per-symbol conflation) + the shared marker
/// state. `marker_in_flight` lives under the same lock as the slots so the
/// slot-write/marker-send decision is atomic.
// pub for vike-core (the lane consumer); hidden — not venue API.
#[doc(hidden)]
pub struct Conflated {
    // pub for vike-core (the lane consumer); hidden — not venue API.
    #[doc(hidden)]
    pub state: Mutex<ConflatedState>,
    // pub for vike-core (the lane consumer); hidden — not venue API.
    #[doc(hidden)]
    pub drops: AtomicU64,
}

#[derive(Default)]
// pub for vike-core (the lane consumer); hidden — not venue API.
#[doc(hidden)]
pub struct ConflatedState {
    /// (venue, symbol) -> freshest REAL-mark tick; insertion-ordered drain
    // pub for vike-core (the lane consumer); hidden — not venue API.
    #[doc(hidden)]
    pub slots: indexmap::IndexMap<(String, String), MarketTick>,
    /// (venue, symbol) -> freshest candle-close tick — a SEPARATE slot map from `slots` so a
    /// 1s-cadence real-mark stream can never conflate away a kline feed's bar-close tick for the
    /// same symbol (the two feed different `PriceBoard` slots downstream)
    // pub for vike-core (the lane consumer); hidden — not venue API.
    #[doc(hidden)]
    pub bar_close_slots: indexmap::IndexMap<(String, String), MarketTick>,
    /// (venue, symbol, interval) -> freshest FORMING bar (intrabar updates conflate;
    /// closes ride the lossless BarClose lane)
    // pub for vike-core (the lane consumer); hidden — not venue API.
    #[doc(hidden)]
    pub forming: indexmap::IndexMap<SeriesKey, Bar>,
    /// at most ONE marker is ever queued; the core clears it when it drains the slots
    // pub for vike-core (the lane consumer); hidden — not venue API.
    #[doc(hidden)]
    pub marker_in_flight: bool,
}

/// Producer half of the market-data conflation lane (Clone per feed thread).
#[derive(Clone)]
pub struct MarketSender {
    // pub for vike-core (the lane consumer); hidden — not venue API.
    #[doc(hidden)]
    pub inner: Arc<Conflated>,
    // pub for vike-core (the lane consumer); hidden — not venue API.
    #[doc(hidden)]
    pub ingest: mpsc::Sender<Ingest>,
}

impl MarketSender {
    /// Latest-wins, WAIT-FREE publish — safe from tokio tasks AND plain threads (no
    /// blocking_send: tokio panics on blocking calls inside a runtime). Overwriting a
    /// pending tick for the same symbol is counted (`conflated_market_drops`). The marker
    /// rides `try_send`: if the ingest queue is momentarily full the marker is NOT lost —
    /// `marker_in_flight` stays false and the next publish (or the same symbol's next
    /// tick) retries it; ticks refresh continuously, so the slot can never go stale while
    /// the feed is alive.
    pub fn publish(&self, tick: MarketTick) {
        let mut st = self.inner.state.lock().unwrap();
        let key = (tick.venue.clone(), tick.symbol.clone());
        if st.slots.insert(key, tick).is_some() {
            self.inner.drops.fetch_add(1, Ordering::Relaxed);
        }
        self.arm_marker(st);
    }

    /// Latest-wins CANDLE-CLOSE tick (a kline feed's last-price snapshot — closed or forming
    /// bar). Same wait-free marker protocol as [`MarketSender::publish`], but a separate slot map:
    /// bar-close and real-mark ticks for the same (venue, symbol) never conflate each other away,
    /// because they fill different `PriceBoard` slots downstream (mark-slot semantics).
    pub fn publish_bar_close(&self, tick: MarketTick) {
        let mut st = self.inner.state.lock().unwrap();
        let key = (tick.venue.clone(), tick.symbol.clone());
        if st.bar_close_slots.insert(key, tick).is_some() {
            self.inner.drops.fetch_add(1, Ordering::Relaxed);
        }
        self.arm_marker(st);
    }

    /// Latest-wins FORMING-bar update (intrabar tick of the still-open kline). Same
    /// wait-free marker protocol as ticks; a bar CLOSE must use [`BarSender`] instead.
    pub fn publish_forming(&self, venue: &str, symbol: &str, interval: &str, bar: Bar) {
        let mut st = self.inner.state.lock().unwrap();
        let key = (venue.to_string(), symbol.to_string(), interval.to_string());
        if st.forming.insert(key, bar).is_some() {
            self.inner.drops.fetch_add(1, Ordering::Relaxed);
        }
        self.arm_marker(st);
    }

    fn arm_marker(&self, mut st: std::sync::MutexGuard<'_, ConflatedState>) {
        if st.marker_in_flight {
            return; // core will drain the freshest values when it consumes the marker
        }
        if self.ingest.try_send(Ingest::Market).is_ok() {
            st.marker_in_flight = true;
        }
        // try_send Full/Closed: leave marker_in_flight false — retried on the next publish
    }
}

/// Lossless bar lane (REST seed + closed klines) — a missed close is a series hole, so
/// these back-pressure the feed thread instead of conflating.
#[derive(Clone)]
pub struct BarSender {
    // pub for vike-core (the lane consumer); hidden — not venue API.
    #[doc(hidden)]
    pub ingest: mpsc::Sender<Ingest>,
}

impl BarSender {
    /// Replace the whole series (REST warmup backfill). Blocking-lossless.
    pub fn seed(&self, seed: BarSeed) -> Result<(), CoreGone> {
        self.ingest.blocking_send(Ingest::BarSeed(Box::new(seed))).map_err(|_| CoreGone)
    }

    /// Append a CLOSED bar. Blocking-lossless.
    pub fn close(&self, update: BarUpdate) -> Result<(), CoreGone> {
        self.ingest.blocking_send(Ingest::BarClose(Box::new(update))).map_err(|_| CoreGone)
    }
}

/// Lossless L2/tick lane (R8 HFT track). Unlike [`MarketSender`] (latest-wins mark conflation),
/// each quote/trade/book update is a DISTINCT event a strategy may act on, so these back-pressure
/// the feed rather than conflate — mirroring [`BarSender`]'s lossless discipline.
#[derive(Clone)]
pub struct TickSender {
    // pub for vike-core (the lane consumer); hidden — not venue API.
    #[doc(hidden)]
    pub ingest: mpsc::Sender<Ingest>,
}

impl TickSender {
    /// Blocking-lossless quote update.
    pub fn quote(&self, update: QuoteUpdate) -> Result<(), CoreGone> {
        self.ingest.blocking_send(Ingest::Quote(Box::new(update))).map_err(|_| CoreGone)
    }

    /// Blocking-lossless trade update.
    pub fn trade(&self, update: TradeUpdate) -> Result<(), CoreGone> {
        self.ingest.blocking_send(Ingest::Trade(Box::new(update))).map_err(|_| CoreGone)
    }

    /// Blocking-lossless L2 book update. The book rides behind an `Arc` — a pump keeping ONE
    /// standing book sends `Arc::clone(&book)` (a refcount bump) and mutates through
    /// `Arc::make_mut`; see [`BookUpdate`] for why.
    pub fn book(&self, update: BookUpdate) -> Result<(), CoreGone> {
        self.ingest.blocking_send(Ingest::Book(Box::new(update))).map_err(|_| CoreGone)
    }

    /// Feed-health status CHANGE (net-hardening §B). The producer (a feed's stream-health machine
    /// → its `LiveDataSink` sink boundary) calls this ONLY on a `StreamStatus` transition
    /// (disconnect/stale/recover), NOT per market message, so an occasional lossless send costs
    /// nothing steady-state. Delivered to the mounted strategy's `on_feed_status` hook.
    pub fn stream_status(&self, update: StreamStatusUpdate) -> Result<(), CoreGone> {
        self.ingest.blocking_send(Ingest::StreamStatus(Box::new(update))).map_err(|_| CoreGone)
    }

    /// Per-side FLOW-TOXICITY update (RTDS wallet-toxicity guard). The producer (a toxicity
    /// aggregator over the wallet-attributed activity tape) calls this at toxicity cadence, NOT per
    /// market message, so an occasional lossless send costs nothing steady-state. Delivered to the
    /// mounted strategy's `on_flow` hook. The exact twin of [`TickSender::stream_status`].
    pub fn flow(&self, update: FlowUpdate) -> Result<(), CoreGone> {
        self.ingest.blocking_send(Ingest::Flow(Box::new(update))).map_err(|_| CoreGone)
    }
}

/// Lossless exec-event lane for venue tasks (async or blocking contexts).
#[derive(Clone)]
pub struct EventSender {
    // pub for vike-core (the lane consumer); hidden — not venue API.
    #[doc(hidden)]
    pub ingest: mpsc::Sender<Ingest>,
    /// **WHICH ACCOUNT the producer behind this handle speaks for** — set by
    /// [`EventSender::routed`] at the MOUNT, `None` on every handle the core hands out.
    ///
    /// It exists because a venue adapter cannot answer that question: it holds one credential set
    /// and stamps the canonical venue id, so `vike_core`'s router had nothing to disambiguate an
    /// account-wide [`vike_model::events::AccountState`] with and folded a second account's
    /// balances into the first account's book. The mount DOES know, so the answer is attached
    /// where the account identity lives instead of being taught to thirteen bridges.
    ///
    /// See [`EventSender::routed`] for why a default account's handle is inert rather than
    /// stamping its own venue back onto the payload.
    #[doc(hidden)]
    pub route_key: Option<Ustr>,
}

impl EventSender {
    /// This lane, scoped to ONE venue ACCOUNT: the returned handle stamps `route_key` onto every
    /// [`Event::AccountState`] it carries, so `vike_core`'s `route_event` can fold it into that
    /// account's own engine.
    ///
    /// ⚠ **A key equal to the payload's own venue stamps NOTHING**, and that is the whole
    /// byte-identity argument rather than an optimisation. `vike_mount::account_route_key` renders
    /// the bare venue id for `AccountLabel::Default`, so the default account — every account on
    /// every single-account box — produces a payload with `route_key: None`, which
    /// `skip_serializing_if` omits from the wire entirely. A box with no `[accounts]` table
    /// therefore writes the identical journal bytes it wrote before this existed, and the mount can
    /// call this unconditionally instead of deciding whether to.
    ///
    /// The comparison is per-EVENT and against the payload's venue rather than against a stored
    /// venue, because one mount's lane carries one venue's frames: the two are equal by
    /// construction and the payload is the one that cannot be wrong.
    ///
    /// Off the fold thread by construction — this runs on the venue adapter's own thread/task, on
    /// the way IN to the ingest channel, so the core-hop p99 gate never sees it.
    pub fn routed(&self, route_key: &str) -> EventSender {
        EventSender { ingest: self.ingest.clone(), route_key: Some(Ustr::from(route_key)) }
    }

    /// Stamp [`Self::route_key`] onto an account-wide payload that carries no other routing handle.
    ///
    /// **THE THREE venue-tagged payloads with no client-order-id**: [`Event::AccountState`],
    /// [`Event::Funding`] and [`Event::PositionLiquidated`]. [`Event::Fill`] is deliberately NOT
    /// here — it names the order it belongs to, which `vike_core`'s `route_event` resolves through
    /// its submit-time `coid_venue` map, exactly, for every order this process placed; stamping it
    /// too would put a second, redundant key on the one wire shape the frozen parity fixtures pin.
    ///
    /// ⚠ **The funding and liquidation arms are a CORRECTION, and the argument that left them out
    /// is worth keeping.** This used to stamp `AccountState` alone, justified as "every other
    /// venue-tagged payload names a SYMBOL, and the symbol disambiguates two accounts EXACTLY,
    /// because `vike_config` refuses to arm two active accounts of one venue on one symbol". That
    /// refusal is GONE — two accounts on one instrument is an ordinary SPREAD, which is the whole
    /// point of the mount `account` field — so the symbol became a LAST RESORT that answers
    /// `None` on exactly the configuration this seam exists to support. A coid covers the fill
    /// lane; funding and liquidation carry no coid, so on a shared symbol every one of them fell
    /// through to the venue lookup and folded into the venue's DEFAULT engine: a labelled
    /// account's funding debit landed on the default account's balance, and a labelled account's
    /// liquidation CLOSED the default account's position at the venue's liq price. Both books
    /// wrong, silently, in the supported configuration.
    ///
    /// ⚠ **`&mut`, not by-value, and that is about the LATENCY GATE.** `blocking_send` sits inside
    /// the measured core hop (`crates/vike-core/tests/runtime_latency.rs` feeds
    /// `Ingest::Event(Event::Fill)` through exactly this call), so a by-value `Event -> Event`
    /// signature would put a move of the largest enum variant on that path and leave its
    /// elimination to the optimiser. Taking `&mut` removes the question rather than arguing it:
    /// for every payload that is not an `AccountState` the whole of this function is one
    /// discriminant test on a `None`.
    #[inline]
    fn stamp(&self, event: &mut Event) {
        // ONE `Option` test guards all three arms, so a payload this lane does not stamp — and
        // every payload on every single-account box, whose `route_key` is `None` — still pays one
        // load and one branch, which is what the `&mut` signature above is protecting.
        let Some(key) = self.route_key else { return };
        // A default account's key IS its venue — nothing to say, and saying it would change the
        // bytes. See [`Self::routed`].
        //
        // `is_none` makes the stamp an answer to "nobody said", never an override of somebody who
        // did: a producer that already names an account knows something this lane does not, and it
        // also makes the stamp idempotent under any future re-send.
        //
        // The match SELECTS the slot and the rule is applied once below, rather than being written
        // out per arm — three copies of one condition is the shape this workspace keeps finding
        // wrong in exactly one of the copies.
        let slot: Option<(Ustr, &mut Option<Ustr>)> = match event {
            Event::AccountState(a) => Some((a.venue, &mut a.route_key)),
            Event::Funding(f) => Some((f.venue, &mut f.route_key)),
            Event::PositionLiquidated(p) => Some((p.venue, &mut p.route_key)),
            _ => None,
        };
        if let Some((venue, slot)) = slot
            && key != venue
            && slot.is_none()
        {
            *slot = Some(key);
        }
    }

    /// Async lossless send (venue WS tasks) — back-pressures the task, never drops.
    pub async fn send(&self, mut event: Event) -> Result<(), CoreGone> {
        self.stamp(&mut event);
        self.ingest.send(Ingest::Event(event)).await.map_err(|_| CoreGone)
    }

    /// Blocking lossless send (non-async producers, tests).
    pub fn blocking_send(&self, mut event: Event) -> Result<(), CoreGone> {
        self.stamp(&mut event);
        self.ingest.blocking_send(Ingest::Event(event)).map_err(|_| CoreGone)
    }
}

/// Test-support: a raw `EventSender` + ingest receiver with NO core thread behind it.
/// Venue-adapter integration tests use this to observe exactly what a client pushes
/// into the ingest lane (the core-thread twin is `CoreHandle::event_sender`).
pub fn event_channel(capacity: usize) -> (EventSender, mpsc::Receiver<Ingest>) {
    let (tx, rx) = mpsc::channel(capacity);
    (EventSender { ingest: tx, route_key: None }, rx)
}

/// Test-support: a `(BarSender, MarketSender)` pair + the ingest receiver behind them, with NO
/// core thread — the bar/market-lane twin of [`event_channel`]. A live market-data feed impl (or
/// its test) can push closed bars / forming updates / ticks and observe exactly what lands on the
/// ingest lane. Both senders share one ingest channel, mirroring how `spawn_core` hands the
/// core's single ingest to `bar_sender()`/`market_sender()`. Currently unused outside its own
/// definition (the retired `vike_exec::market_data` module was its last consumer; venue feeds now
/// implement `vike_data::DataClient` directly against the sink seam) — kept as the documented
/// test-support twin of [`event_channel`] for whenever a vike-exec-local market-data test needs it.
pub fn market_data_channel(capacity: usize) -> (BarSender, MarketSender, mpsc::Receiver<Ingest>) {
    let (tx, rx) = mpsc::channel(capacity);
    let market = MarketSender {
        inner: Arc::new(Conflated {
            state: Mutex::new(ConflatedState::default()),
            drops: AtomicU64::new(0),
        }),
        ingest: tx.clone(),
    };
    let bars = BarSender { ingest: tx };
    (bars, market, rx)
}

#[cfg(test)]
mod event_channel_tests {
    use super::*;
    use vike_model::events::{Event, OrderSubmitted};

    #[test]
    fn event_channel_delivers_events_without_a_core() {
        let (sender, mut rx) = event_channel(4);
        sender
            .blocking_send(Event::OrderSubmitted(OrderSubmitted {
                client_order_id: "c1".into(),
                ts: 7,
            }))
            .expect("receiver alive");
        match rx.blocking_recv() {
            Some(Ingest::Event(Event::OrderSubmitted(e))) => {
                assert_eq!(e.client_order_id, "c1");
                assert_eq!(e.ts, 7);
            }
            other => panic!("expected OrderSubmitted ingest, got {other:?}"),
        }
    }

    use vike_model::events::AccountState;

    fn account_state(venue: &str) -> Event {
        Event::AccountState(AccountState {
            venue: venue.into(),
            balances: vec![("USDT".to_string(), 100.0)],
            ts: 1,
            route_key: None,
        })
    }

    fn funding(venue: &str) -> Event {
        Event::Funding(vike_model::events::FundingEvent {
            venue: venue.into(),
            symbol: "BTCUSDT".into(),
            position_side: vike_model::events::PositionSide::Both,
            funding_rate: 0.0001,
            amount: -1.25,
            mark_price: None,
            ts: 1,
            route_key: None,
        })
    }

    fn liquidation(venue: &str) -> Event {
        Event::PositionLiquidated(vike_model::events::PositionLiquidated {
            venue: venue.into(),
            symbol: "BTCUSDT".into(),
            position_side: vike_model::events::PositionSide::Both,
            qty: 1.0,
            liq_price: 90.0,
            fee: 0.1,
            ts: 1,
            trade_id: "l1".into(),
            route_key: None,
        })
    }

    fn sent(sender: &EventSender, rx: &mut mpsc::Receiver<Ingest>, ev: Event) -> AccountState {
        sender.blocking_send(ev).expect("receiver alive");
        match rx.blocking_recv() {
            Some(Ingest::Event(Event::AccountState(a))) => a,
            other => panic!("expected an AccountState ingest, got {other:?}"),
        }
    }

    /// **The lane the mount hands a LABELLED account stamps its route key**, so `vike_core`'s
    /// router can fold the snapshot into that account's own engine instead of the venue's default.
    #[test]
    fn a_routed_lane_stamps_a_labelled_accounts_route_key() {
        let (plain, mut rx) = event_channel(4);
        let scoped = plain.routed("binance#alt");
        assert_eq!(
            sent(&scoped, &mut rx, account_state("binance")).route_key,
            Some("binance#alt".into())
        );
    }

    /// **…and the DEFAULT account's lane stamps NOTHING.** `vike_mount::account_route_key` renders
    /// the bare venue id for `AccountLabel::Default`, so this is the case every single-account box
    /// is in — and an emitted key would change its journal bytes. The mount calls `routed`
    /// unconditionally precisely because this case is inert.
    #[test]
    fn the_default_accounts_lane_stamps_nothing() {
        let (plain, mut rx) = event_channel(4);
        let scoped = plain.routed("binance");
        assert_eq!(
            sent(&scoped, &mut rx, account_state("binance")).route_key,
            None,
            "a key equal to the payload's own venue says nothing and must not reach the wire"
        );
    }

    /// An UNSCOPED lane — the one `CoreHandle::event_sender` hands out, and the one every test and
    /// in-process producer holds — is byte-identical to its pre-feature self.
    #[test]
    fn an_unscoped_lane_is_inert() {
        let (plain, mut rx) = event_channel(4);
        assert_eq!(sent(&plain, &mut rx, account_state("binance")).route_key, None);
    }

    /// A COID-LESS venue-tagged payload is stamped, and there are THREE of them — this is the
    /// funding half.
    ///
    /// It was missed when the stamp was written, on the argument that "every other venue-tagged
    /// payload names a symbol and the symbol is exact". The symbol stopped being an account key
    /// when two accounts of one venue were allowed onto one instrument, and a funding payment
    /// carries no client-order-id to fall back on — so an unstamped one folded into the venue's
    /// DEFAULT engine and debited the wrong account's balance.
    #[test]
    fn a_routed_lane_stamps_a_funding_payment() {
        let (plain, mut rx) = event_channel(4);
        let scoped = plain.routed("binance#alt");
        scoped.blocking_send(funding("binance")).expect("receiver alive");
        match rx.blocking_recv() {
            Some(Ingest::Event(Event::Funding(f))) => {
                assert_eq!(f.route_key, Some("binance#alt".into()));
            }
            other => panic!("expected a Funding ingest, got {other:?}"),
        }
        // …and the DEFAULT account's lane still stamps nothing, so a single-account box's bytes
        // are unchanged on this payload too.
        let (plain, mut rx) = event_channel(4);
        let default_lane = plain.routed("binance");
        default_lane.blocking_send(funding("binance")).expect("receiver alive");
        match rx.blocking_recv() {
            Some(Ingest::Event(Event::Funding(f))) => assert_eq!(f.route_key, None),
            other => panic!("expected a Funding ingest, got {other:?}"),
        }
    }

    /// …and the LIQUIDATION half, the more damaging of the two: `Account::apply_liquidation`
    /// CLOSES the position and books the realized PnL, so an unstamped frame flattened the default
    /// account's book on the strength of a labelled account's liquidation.
    #[test]
    fn a_routed_lane_stamps_a_liquidation() {
        let (plain, mut rx) = event_channel(4);
        let scoped = plain.routed("binance#alt");
        scoped.blocking_send(liquidation("binance")).expect("receiver alive");
        match rx.blocking_recv() {
            Some(Ingest::Event(Event::PositionLiquidated(p))) => {
                assert_eq!(p.route_key, Some("binance#alt".into()));
            }
            other => panic!("expected a PositionLiquidated ingest, got {other:?}"),
        }
        let (plain, mut rx) = event_channel(4);
        let default_lane = plain.routed("binance");
        default_lane.blocking_send(liquidation("binance")).expect("receiver alive");
        match rx.blocking_recv() {
            Some(Ingest::Event(Event::PositionLiquidated(p))) => assert_eq!(p.route_key, None),
            other => panic!("expected a PositionLiquidated ingest, got {other:?}"),
        }
    }

    /// The three stamped payloads are the ones with NO client-order-id, and `Event::Fill` is
    /// deliberately outside that set: `vike_core`'s `route_event` resolves it through the
    /// submit-time `coid_venue` map, exactly, for every order this process placed — so a key here
    /// would be redundant AND would touch the one wire shape the frozen parity fixtures pin.
    #[test]
    fn a_routed_lane_leaves_every_other_event_untouched() {
        let (plain, mut rx) = event_channel(4);
        let scoped = plain.routed("binance#alt");
        let ev = Event::OrderSubmitted(OrderSubmitted { client_order_id: "c9".into(), ts: 3 });
        scoped.blocking_send(ev.clone()).expect("receiver alive");
        match rx.blocking_recv() {
            Some(Ingest::Event(got)) => assert_eq!(got, ev),
            other => panic!("expected the event verbatim, got {other:?}"),
        }
    }

    /// A payload that ALREADY names an account keeps its own key: the stamp is an answer to
    /// "nobody said", never an override of somebody who did.
    #[test]
    fn a_lane_does_not_overwrite_a_key_the_payload_already_carries() {
        let (plain, mut rx) = event_channel(4);
        let scoped = plain.routed("binance#alt");
        let mut ev = account_state("binance");
        if let Event::AccountState(a) = &mut ev {
            a.route_key = Some("binance#other".into());
        }
        assert_eq!(sent(&scoped, &mut rx, ev).route_key, Some("binance#other".into()));
    }
}

#[cfg(test)]
mod serde_tests {
    use super::*;
    use crate::order::{ManagedOrder, OrderStatus};

    /// The journal serializes Ingest verbatim; every journaled Ingest variant AND every
    /// journaled Command variant (Command rides inside `Ingest::Command`) must round-trip
    /// byte-for-byte through JSON. The loop below constructs one instance of every `Ingest`
    /// variant and every `Command` variant — including a populated `ManagedOrder` inside
    /// `ApplySnapshot`'s `ReconcileSnapshot` and a non-empty `L2Book` (≥1 bid, ≥1 ask) inside
    /// `Book`, so the compiler-forced integer-keyed `BTreeMap` wire form is actually exercised.
    #[test]
    fn ingest_round_trips_through_json() {
        let req: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
            "client_order_id": "j1", "venue": "sim", "symbol": "BTCUSDT",
            "side": 1, "qty": 1.0, "order_type": "limit", "price": 100.0
        }))
        .unwrap();

        // A populated ManagedOrder — not just ManagedOrder::new's zeroed fields — so the
        // Option<String>/Option<i64> Some(..) arms round-trip too, not only the None arms.
        let mut snapshot_order = ManagedOrder::new(req.clone());
        snapshot_order.status = OrderStatus::Accepted;
        snapshot_order.venue_order_id = Some("v-1".to_string());
        snapshot_order.filled_qty = 0.5;
        snapshot_order.avg_fill_px = 100.25;
        snapshot_order.created_ms = Some(12_345);

        let snapshot = ReconcileSnapshot {
            positions: vec![("BTCUSDT".into(), 1.0)],
            open_orders: vec![snapshot_order],
            position_avg_px: vec![("BTCUSDT".into(), 100.0)],
            position_mark_px: vec![("BTCUSDT".into(), 100.5)],
            position_sides: vec![("BTCUSDT".into(), "BOTH".into())],
            balance: 10_000.0,
            // Populated (not empty) so the margin-mode carrier's Some arm round-trips too.
            position_margin: vec![("BTCUSDT".into(), vike_model::MarginMode::Isolated, Some(50.0))],
        };

        // Non-empty L2Book (>=1 bid, >=1 ask) — the empty book would trivially round-trip
        // even if the integer-keyed BTreeMap wire form were broken.
        let mut book = L2Book::new(0.5);
        book.apply_snapshot(1, &[(100.0, 1.5)], &[(100.5, 2.0)]);
        let book = Arc::new(book);

        for msg in [
            Ingest::Command(Command::Order(OrderIntent::Submit(Box::new(req.clone())))),
            Ingest::Command(Command::Order(OrderIntent::Cancel("j1".into()))),
            Ingest::Command(Command::Order(OrderIntent::Modify {
                client_order_id: "j1".into(),
                new_qty: Some(2.0),
                new_price: Some(101.0),
            })),
            Ingest::Command(Command::Order(OrderIntent::SubmitBatch(vec![req.clone()]))),
            Ingest::Command(Command::Order(OrderIntent::CancelBatch(vec!["j1".into()]))),
            Ingest::Command(Command::Order(OrderIntent::Confirm("j1".into()))),
            Ingest::Command(Command::UpdateParams(Box::new(ParamsUpdate {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                interval: "1m".into(),
                params: StrategyParams::SpreadMaker(vike_model::SpreadMakerParams {
                    qty: 1.0,
                    half_spread: 0.5,
                    target_inventory: 0.0,
                    max_inventory: 1.0,
                    skew: 0.25,
                    fill_window_ms: 1_000,
                    net_fill_threshold: 2.5,
                    suppress_cooldown_ms: 5_000,
                    style: vike_model::QuoteStyle::Join,
                    depth_levels: 2,
                    tick_size: 0.5,
                    filter_own: true,
                    avellaneda_stoikov: None,
                    refresh_tolerance: None,
                    ladder: None,
                    reward: None,
                    toxicity: None,
                }),
            }))),
            Ingest::Command(Command::Order(OrderIntent::MassCancel { venue: None, symbol: None })),
            Ingest::Command(Command::SetTradingState(TradingState::Active)),
            Ingest::Command(Command::SetMargin(Box::new(MarginUpdate {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                im_requirement: 0.1,
            }))),
            Ingest::Command(Command::ApplySnapshot(Box::new(snapshot))),
            // ReconcileReports with a NON-empty `hybrid` policy so the enum-keyed
            // `BTreeMap<DivergenceKind, ReconMode>` (JSON string keys) actually round-trips.
            Ingest::Command(Command::ReconcileReports(Box::new(crate::ReconcileReports {
                venue: "binance".into(),
                since: 42,
                orders: Vec::new(),
                fills: vec![vike_model::FillReport {
                    venue: "binance".into(),
                    symbol: "BTCUSDT".into(),
                    trade_id: "t1".into(),
                    venue_order_id: "v9".into(),
                    client_order_id: None,
                    side: 1,
                    last_qty: 1.0,
                    last_px: 100.0,
                    commission: 0.0,
                    commission_asset: "USDT".into(),
                    liquidity_side: vike_model::events::LiquiditySide::Taker,
                    ts: 5,
                }],
                positions: Vec::new(),
                policy: crate::recon::ReconPolicy::hybrid(),
                balance: Some(9999.0),
                generate_missing_orders: false,
                reconcile_balance: false,
                balance_tol: crate::recon::BalanceTol::default(),
                // Spelled `Some` HERE only so the field is exercised on the wire; every producer
                // in this tree sends `None`. The pre-field-journal shape is gated separately by
                // `a_pre_route_key_journal_replays_routing_to_its_venue`.
                route_key: Some("binance-sub2".into()),
            }))),
            Ingest::Command(Command::ConfirmRecon(7)),
            Ingest::Command(Command::Shutdown),
            Ingest::Event(vike_model::events::Event::OrderSubmitted(
                vike_model::events::OrderSubmitted { client_order_id: "j1".into(), ts: 7 },
            )),
            Ingest::Watchdog,
            Ingest::Market,
            Ingest::BarSeed(Box::new(BarSeed {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                interval: "1m".into(),
                bars: vec![Bar {
                    ts: 1,
                    open: 100.0,
                    high: 101.0,
                    low: 99.0,
                    close: 100.5,
                    volume: 10.0,
                    funding: Some(0.0001),
                    bid: Some(100.4),
                    ask: Some(100.6),
                    symbol: Some("BTCUSDT".into()),
                }],
            })),
            Ingest::BarClose(Box::new(BarUpdate {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                interval: "1m".into(),
                bar: Bar {
                    ts: 2,
                    open: 100.5,
                    high: 101.5,
                    low: 100.0,
                    close: 101.0,
                    volume: 5.0,
                    funding: None,
                    bid: None,
                    ask: None,
                    symbol: None,
                },
            })),
            Ingest::Quote(Box::new(QuoteUpdate {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                quote: QuoteTick {
                    ts: 3,
                    local_ts: 0,
                    bid: 100.4,
                    ask: 100.6,
                    bid_size: 1.0,
                    ask_size: 1.2,
                    symbol: "BTCUSDT".into(),
                },
            })),
            Ingest::Trade(Box::new(TradeUpdate {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                trade: TradeTick {
                    ts: 4,
                    local_ts: 0,
                    price: 100.5,
                    size: 0.25,
                    is_buyer_maker: true,
                    symbol: "BTCUSDT".into(),
                },
            })),
            Ingest::Book(Box::new(BookUpdate {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                book,
            })),
            Ingest::StreamStatus(Box::new(StreamStatusUpdate {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                stream: "quotes".into(),
                status: FeedStatus::Disconnected,
            })),
            Ingest::Flow(Box::new(FlowUpdate {
                venue: "polymarket".into(),
                symbol: "TOK".into(),
                flow: FlowToxicity { bid: 0.25, ask: 0.75, ts: 9 },
            })),
        ] {
            let json = serde_json::to_string(&msg).unwrap();
            let back: Ingest = serde_json::from_str(&json).unwrap();
            assert_eq!(format!("{msg:?}"), format!("{back:?}"), "lossy round-trip");
        }
    }

    /// PIN (perf audit finding #1, the `Arc<L2Book>` lane): moving `BookUpdate.book` behind an
    /// `Arc` must NOT change one wire byte. The `#[serde(serialize_with/deserialize_with)]` pair
    /// forwards to `L2Book`'s own impls, so the field is written exactly as the bare book was —
    /// this compares the real `BookUpdate` JSON against a hand-built object carrying the SAME book
    /// serialized directly, and asserts string equality (not just a successful round-trip, which
    /// a differently-shaped-but-self-consistent encoding would also pass).
    #[test]
    fn arc_book_field_serializes_exactly_like_a_bare_book() {
        let mut book = L2Book::new(0.5);
        book.apply_snapshot(7, &[(100.0, 1.5), (99.5, 2.0)], &[(100.5, 2.0)]);

        let with_arc = serde_json::to_string(&BookUpdate {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            book: Arc::new(book.clone()),
        })
        .unwrap();
        // The pre-Arc encoding, reconstructed independently: the same three fields in declaration
        // order, the book serialized straight off `L2Book`'s derive.
        let bare = format!(
            r#"{{"venue":"binance","symbol":"BTCUSDT","book":{}}}"#,
            serde_json::to_string(&book).unwrap()
        );
        assert_eq!(with_arc, bare, "Arc<L2Book> must be wire-transparent");

        // …and back, into a FRESH Arc (identity is not preserved; value is).
        let back: BookUpdate = serde_json::from_str(&with_arc).unwrap();
        assert_eq!(format!("{:?}", *back.book), format!("{book:?}"));
    }

    #[test]
    fn reconcile_reports_deserializes_pre_generate_missing_orders_journals() {
        // Back-compat pin: a journal written BEFORE `generate_missing_orders` existed carries no
        // such key — `#[serde(default)]` must fill in `false` (the inert value), not error.
        let json = serde_json::json!({
            "venue": "binance",
            "since": 42,
            "orders": [],
            "fills": [],
            "positions": [],
            "policy": { "default": "Synthesize", "per_kind": {} },
            "balance": null
        });
        let reports: crate::ReconcileReports = serde_json::from_value(json).unwrap();
        assert!(!reports.generate_missing_orders, "absent field defaults to the inert false");
        assert!(!reports.reconcile_balance, "absent reconcile_balance defaults to the inert false");
        assert_eq!(
            reports.balance_tol,
            crate::recon::BalanceTol::default(),
            "absent balance_tol defaults to the conservative constant"
        );
    }

    /// The routing half of the same back-compat pin, kept SEPARATE because it is not a flag: a
    /// pre-`route_key` journal carries no key, and the replay must route to the same engine the
    /// live pass did — the venue's sole account — rather than to the empty string (which resolves
    /// no engine at all, so the whole replayed pass would be dropped with a "no engine for venue"
    /// note).
    #[test]
    fn a_pre_route_key_journal_replays_routing_to_its_venue() {
        let json = serde_json::json!({
            "venue": "binance",
            "since": 42,
            "orders": [],
            "fills": [],
            "positions": [],
            "policy": { "default": "Synthesize", "per_kind": {} },
            "balance": null
        });
        let reports: crate::ReconcileReports = serde_json::from_value(json).unwrap();
        assert_eq!(reports.route_key, None, "absent field is the sole-account default");
        assert_eq!(
            reports.route().as_str(),
            "binance",
            "…and it routes to the venue, exactly as the one-field payload did"
        );
    }

    /// …and a payload that DOES carry one routes by it, not by the venue — the two-account shape
    /// this field exists for. The negative control for the test above: without it, a `route()`
    /// hard-wired to `self.venue` would pass that one and this pass/fail pair is what separates
    /// "defaults correctly" from "ignores the field".
    #[test]
    fn a_carried_route_key_routes_by_itself_not_by_the_venue() {
        let json = serde_json::json!({
            "venue": "binance",
            "since": 42,
            "orders": [],
            "fills": [],
            "positions": [],
            "policy": { "default": "Synthesize", "per_kind": {} },
            "balance": null,
            "route_key": "binance-sub2"
        });
        let reports: crate::ReconcileReports = serde_json::from_value(json).unwrap();
        assert_eq!(reports.route().as_str(), "binance-sub2");
        assert_ne!(reports.route().as_str(), reports.venue, "the two facts are not the same fact");
    }
}

#[cfg(test)]
mod order_intent_tests {
    use super::*;
    use vike_model::OrderRequest;

    #[test]
    fn order_intent_serde_roundtrips_each_variant() {
        let req = || {
            Box::new(OrderRequest {
                client_order_id: "c".into(),
                venue: "sim".into(),
                symbol: "BTCUSDT".into(),
                side: 1,
                qty: 1.0,
                order_type: "market".into(),
                ..Default::default()
            })
        };
        let variants = vec![
            OrderIntent::Submit(req()),
            OrderIntent::Cancel("c".into()),
            OrderIntent::Modify {
                client_order_id: "c".into(),
                new_qty: Some(2.0),
                new_price: None,
            },
            OrderIntent::Confirm("c".into()),
            OrderIntent::MassCancel { venue: None, symbol: None },
            OrderIntent::Flatten { venue: "sim".into(), symbol: "BTCUSDT".into() },
            OrderIntent::ArmConditional(ConditionalIntent {
                venue: "sim".into(),
                symbol: "BTCUSDT".into(),
                side: -1,
                qty: 1.0,
                price: Some(90.0),
                trail: None,
                trigger_by: None,
            }),
            OrderIntent::DisarmConditional { arm_id: "cafef00da0".into() },
            OrderIntent::SubmitBatch(vec![*req()]),
            OrderIntent::CancelBatch(vec!["c".into()]),
            OrderIntent::Bracket(Box::new(vike_model::BracketSpec {
                venue: "sim".into(),
                symbol: "BTCUSDT".into(),
                side: 1,
                qty: 2.0,
                entry_price: Some(100.0),
                stop_loss: 95.0,
                take_profit: 110.0,
            })),
            OrderIntent::Combo(Box::new(vike_model::ComboSpec {
                venue: "deribit".into(),
                side: 1,
                qty: 2.0,
                legs: vec![
                    vike_model::ComboLeg { symbol: "BTC-27MAR26-100000-C".into(), ratio: 1 },
                    vike_model::ComboLeg { symbol: "BTC-27MAR26-120000-C".into(), ratio: -1 },
                ],
                // a CREDIT combo: the signed net limit is NEGATIVE and must survive the trip
                net_limit: Some(-0.0125),
                time_in_force: vike_model::TimeInForce::Gtc,
            })),
        ];
        for v in variants {
            let js = serde_json::to_string(&v).unwrap();
            let back: OrderIntent = serde_json::from_str(&js).unwrap();
            assert_eq!(format!("{v:?}"), format!("{back:?}"));
        }
    }

    #[test]
    fn command_order_wraps_intent() {
        let c = Command::Order(OrderIntent::Cancel("c".into()));
        let js = serde_json::to_string(&c).unwrap();
        assert!(js.contains("Order"), "externally-tagged: {js}");
        let back: Command = serde_json::from_str(&js).unwrap();
        assert_eq!(format!("{c:?}"), format!("{back:?}"));
    }
}

#[cfg(test)]
mod conflation_tests {
    use super::*;

    fn tick(px: f64, ts: i64) -> MarketTick {
        MarketTick { venue: "sim".into(), symbol: "BTCUSDT".into(), px, ts }
    }

    /// Mark-slot semantics (W2-T4): real-mark (`publish`) and candle-close (`publish_bar_close`)
    /// ticks conflate INDEPENDENTLY — a 1s-cadence real-mark stream can never overwrite a kline
    /// feed's bar-close tick for the same (venue, symbol), and only a SAME-lane overwrite counts
    /// as a conflation drop.
    #[test]
    fn bar_close_and_mark_ticks_conflate_in_separate_slots() {
        let (_bars, market, _rx) = market_data_channel(8);
        market.publish(tick(101.0, 1));
        market.publish_bar_close(tick(100.0, 2));
        market.publish_bar_close(tick(99.0, 3)); // conflates the previous BAR-CLOSE tick only
        {
            let st = market.inner.state.lock().unwrap();
            assert_eq!(st.slots.len(), 1, "one real-mark slot");
            assert_eq!(st.slots[0].px, 101.0, "the real mark survives bar-close publishes");
            assert_eq!(st.bar_close_slots.len(), 1, "one bar-close slot");
            assert_eq!(st.bar_close_slots[0].px, 99.0, "bar-close latest-wins in its own slot");
        }
        assert_eq!(
            market.inner.drops.load(Ordering::Relaxed),
            1,
            "only the same-lane overwrite counts as a conflation drop"
        );
    }
}
