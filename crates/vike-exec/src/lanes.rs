//! The producer-side ingest lanes — the types venue adapters hold (fire-and-forget senders +
//! payloads). The consumer (the single-writer core thread) lives in vike-core; this split is
//! why bridges depend on vike-exec but never on vike-core. A `#[doc(hidden)]` `pub` item here is
//! pub for vike-core (the lane consumer), not venue API.
//!
//! **A journaled payload is a persisted schema.** A field added later is `#[serde(default)]` so an
//! old journal still replays, and an optional one also `skip_serializing_if` so a payload that does
//! not use it keeps its pre-field bytes. A NEW variant replays old journals unchanged, but a
//! journal containing it needs the new binary.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use ustr::Ustr;

use vike_marketdata::{Bar, L2Book, QuoteTick, TradeTick};
use vike_model::events::Event;
use vike_model::{
    DataDelay, FeedStatus, FillReport, FlowToxicity, OrderRequest, OrderStatusReport,
    PositionStatusReport, StrategyParams,
};

use crate::execution_engine::{ExecutionClient, ExecutionEngine, ReconcileSnapshot};
use crate::recon::{BalanceTol, ReconPolicy};
use vike_model::TradingState;

/// The core thread has exited (shutdown or channel closed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreGone;

/// Every ORDER-SCOPED instruction the core accepts — the ONE write contract. Both the external
/// Command path and the LiveBroker strategy drain lower into the core's single `apply_intent`
/// site, so mint + RiskGate are STRUCTURAL on every order path. Serde: journaled inside
/// `Ingest::Command`. Payloads boxed like `Command` variants.
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
    /// None/None = ALL engines + clear ALL conditional books; Some(v)/Some(s) = every account of
    /// that venue + clear that (venue,symbol) book; Some(v)/None = every account of that venue +
    /// clear that venue's books; None/Some = ignored (surfaced to recent-events).
    MassCancel {
        venue: Option<String>,
        symbol: Option<String>,
        /// **WHICH ACCOUNT of `venue` this cancel is for** — the risk-REDUCING verbs' half of the
        /// account-routing seam
        /// (`docs/superpowers/specs/2026-09-22-the-order-payload-names-its-account-design.md`
        /// §4.4); [`Self::Flatten`] and [`Self::MarketExit`] carry the same field, and this is
        /// the one statement of its three states:
        ///
        /// * `None` — the verb keeps its reach EXACTLY: every account of `venue`, or every engine
        ///   when `venue` is `None` too (§4.5 of
        ///   `docs/superpowers/specs/2026-09-13-the-wire-names-an-account-from-the-misroute.md`:
        ///   the sender meant all of them). Every internal producer (the dead-man, the shutdown
        ///   sweep, a strategy's `mass_cancel`, the panic button's own legs) passes it.
        /// * `Some(account)` of a venue — THAT account's book only. `vike_core` resolves it through
        ///   `route_key_of` and refuses by name when no engine carries it, never widening back to
        ///   the venue: a narrowed verb that silently fanned out would reduce books nobody named.
        /// * `Some(account)` with NO venue — refused: a label names one book OF a venue, so on its
        ///   own it resolves to nothing.
        ///
        /// ⚠ **Serde goes through `vike_model::accounts::account_keys::wire_account_option`**, as
        /// `vike_model::OrderRequest::account` does: the wire admits `DEFAULT`, and
        /// `AccountLabel`'s own `Serialize` refuses `AccountLabel::Default`, so the plain impls
        /// would panic the fold thread's journal writer on that spelling.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            with = "vike_model::accounts::account_keys::wire_account_option"
        )]
        account: Option<vike_model::accounts::account_keys::AccountLabel>,
    },
    /// Close the (venue, symbol) net position: reduce-only market for |position|, resolved at
    /// apply time. No-op when flat. An account-less flatten closes that position on EVERY account
    /// of `venue`; `account` narrows it to one (the three states: [`Self::MassCancel`]'s field).
    Flatten {
        venue: String,
        symbol: String,
        /// See [`Self::MassCancel`]'s `account`. `venue` is never absent here, so the no-venue
        /// refusal cannot arise.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            with = "vike_model::accounts::account_keys::wire_account_option"
        )]
        account: Option<vike_model::accounts::account_keys::AccountLabel>,
    },
    /// PANIC BUTTON — "get me out": cancel every live order, then flatten every open position.
    ///
    /// A COMPOUND verb: no state of its own, no orders minted directly. The runtime EXPANDS it at
    /// apply time into one [`Self::MassCancel`] applied FIRST, then one [`Self::Flatten`] per
    /// non-flat position (derived from the POST-cancel account, in the `Account`'s own (venue,
    /// symbol, position_side) insertion order), each lowered through the SAME `apply_intent` site,
    /// so mint → RiskGate → client stays structural.
    ///
    /// **USABLE FROM A HALTED CORE — no precondition.** `RiskGate` admits a POSITION-COVERED
    /// reduce under `Halted` (`vike_model::is_covered_reduce`), which is exactly the shape a
    /// flatten leg mints (side opposite the position, qty `|position|`), so the exit runs under
    /// `Active`, `Reducing` and `Halted` alike; under a halt the runtime logs a warning + a
    /// `recent` note saying it is PROCEEDING. A kill switch must stop opening risk, never trap
    /// you in it.
    ///
    /// **BEST-EFFORT, NOT ATOMIC.** Against a real venue the mass-cancel is fire-and-forget over
    /// the adapter's actor thread: its ACKs may land after the flatten legs are already on the
    /// wire, and a resting order can still fill after a flatten leg. Re-issue the exit if the
    /// board is not flat afterwards.
    ///
    /// `venue: None` = every engine (global exit); `Some(v)` = every account of that venue;
    /// `Some(v)` + `account` = that ONE account of it. A flat, order-free book is a no-op beyond
    /// the mass-cancel. ⚠ **The unscoped panic button — `venue: None, account: None` — reaches
    /// every engine and no gate of the account family may ever refuse it**: naming an account is
    /// an OPT-IN narrowing of an exit, never a precondition for one.
    MarketExit {
        venue: Option<String>,
        /// See [`Self::MassCancel`]'s `account`, including the refusal of an account named with no
        /// venue (which here would otherwise read as the global exit).
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            with = "vike_model::accounts::account_keys::wire_account_option"
        )]
        account: Option<vike_model::accounts::account_keys::AccountLabel>,
    },
    /// Entry + protective SL + TP as one contingent batch (runtime mints 3 coids, wires OTO/OCO).
    Bracket(Box<vike_model::BracketSpec>),
    /// Arm an emulated stop/trailing in the core-owned ConditionalBook (ARM bypasses the gate;
    /// only the FIRE crosses it).
    ArmConditional(ConditionalIntent),
    /// Disarm ONE emulated conditional by the `arm_id` the runtime minted when it was armed —
    /// the individual-cancel primitive (`MassCancel` stays the coarse verb). The ConditionalBook
    /// is keyed by `arm_id`, so exactly-one is structural; a `ConditionalDisarmed` record is
    /// journaled write-ahead of the book mutation. An unknown/stale id is a LOUD no-op (surfaced
    /// to recent-events), never a panic and never silent. Like ARM, a disarm bypasses the
    /// RiskGate — removing a resting emulated arm is not an order reaching a venue.
    DisarmConditional {
        arm_id: String,
    },
    /// An atomic multi-leg order at a SIGNED net price — ONE venue order, ONE coid, ONE
    /// `ManagedOrder` (the venue is the group manager; venue-native only). The runtime mints the
    /// single coid and lowers this through [`vike_model::build_combo`], exactly as `Bracket`
    /// lowers through `build_bracket`; risk is [`crate::RiskGate::check_combo`] across every leg,
    /// never `check` on the net (a credit combo's `ComboSpec::net_limit` is negative).
    /// `vike_core`'s `lower_combo` is the lowering and carries its full contract.
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
    /// `None`/`Some(Last)` = trade/bar prices, `Some(Mark)` = the core's mark lane, `Some(Index)`
    /// = REFUSED at apply time (the core has no index lane — surfaced to recent-events, never
    /// silently evaluated off a different series). `None` keeps a `Cmd`/`StrategySubmit` record's
    /// pre-field bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_by: Option<vike_model::TriggerBy>,
}

/// GUI/session commands. Everything an outside thread may ask of the core.
/// Payloads are boxed so the queued message stays small (clippy large_enum_variant).
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub enum Command {
    /// The order-write contract — every order-scoped verb (see [`OrderIntent`]).
    Order(OrderIntent),
    /// LIVE PARAMETER update for a mounted strategy (RUST-NATIVE). Routes typed
    /// [`StrategyParams`] to ONE mount (the one [`ParamsUpdate::mount_id`] names, or the sole mount
    /// on the payload's `(venue, symbol, interval)` when it names none; an ambiguous update is
    /// refused with a recent-events note, never delivered to the first mount) and calls
    /// its `Strategy::on_params_updated` hook, so a running strategy is re-tuned WITHOUT
    /// unmounting (which would lose queue position). INTERNAL lane plumbing (like
    /// `Ingest::StreamStatus`), NOT the serde `Event` wire union; an occasional session verb, off
    /// the p99<10µs event fold, touching NO OMS state.
    UpdateParams(Box<ParamsUpdate>),
    SetTradingState(TradingState),
    /// Set the trading state of ONE account's engine, addressed by `route_key`, and touch no other
    /// engine: `SetTradingState` is node-wide (it also loosens every other engine a latch or an
    /// operator halt holds), this is its per-account twin.
    ///
    /// **Precedence: the STRICTER state wins, and only a node-wide `SetTradingState(Active)` clears what
    /// the node holds.** The core keeps the strictest state any node-wide rule asked for (the drawdown
    /// latch's `Reducing`, a node-wide `halt`/`reduce`, the silence dead-man's `Halted`, a faulted core)
    /// as the node's state (`CoreThread::node_floor`, rebuilt at a restart as the minimum across the
    /// restored engines), and an account-scoped state looser than it, or than a [`Command::HoldAccount`]
    /// on the engine, lands AT the floor, said in recent-events. A tighter state always lands. An engine
    /// the link dead-man holds `Halted` stays so (a tighter state is remembered for when its venue is
    /// released; a looser one is skipped). An unknown route key, or a STOPPED account, is a refusal note,
    /// never a panic, and a command that would change nothing is not journaled.
    ///
    /// Journaled (a replay core re-applies it through the same dispatch), and the FIRST such record
    /// raises the journal segment to format 21: an older build refuses the directory from then on.
    /// Occasional, off the p99<10µs event fold. The wire door is `WireCommand::SetAccountTradingState`.
    SetAccountTradingState {
        route_key: String,
        state: TradingState,
    },
    /// **HOLD ONE account's engine `Reducing`** (`hold: true`), or release the hold (`false`): the account
    /// watcher's door for a lane twin it keeps up only to be CLOSED (its row is gone, or its tier restart
    /// waits for it to be flat), so a strategy MOUNTED on it cannot open exposure. Unlike
    /// [`Command::SetAccountTradingState`] it is a FLOOR on the engine that a node-wide `SetTradingState`
    /// cannot loosen (the node-wide word sets the state the engine returns to), and it only ever RAISES: an
    /// engine already `Halted` stays `Halted`. A release lifts only what the hold set: an engine that is
    /// not at the hold's own `Reducing` any more (an operator's `Halted`), or that was `Reducing` before
    /// the hold, is left alone, and an engine with no hold (a restarted account is a new engine) is not
    /// touched at all. The hold is keyed to the engine, not the route key.
    ///
    /// Journaled like its twin, and not journaled when it would change nothing (a release with no hold, a
    /// hold already held, an unknown or stopped account); the first record raises the segment to format 21.
    /// Occasional, off the p99<10µs event fold. No wire door: the node raises it, an operator does not.
    HoldAccount {
        route_key: String,
        hold: bool,
    },
    /// The operator's `resume-venue` (decision 0180): release the LINK DEAD-MAN's halt of ONE venue
    /// and nothing else. The core puts each engine of `venue` back to the state it held before the
    /// halt (the latch remembers it; after a restart it does not, and the core reads the other
    /// venues' state instead), and leaves every other venue, the drawdown latch and an operator's
    /// node-wide halt alone, which `SetTradingState` cannot do. It REFUSES while one of the venue's
    /// links is still down unless `force`, and a forced resume re-opens the venue's windows so a link
    /// that stays down trips again after a fresh grace. A refusal is a note and a WARN, never a panic.
    /// Occasional, off the p99<10µs event fold; the wire door is `WireCommand::ResumeVenue`.
    ///
    /// ⚠ Deliberately NOT journaled (the `journaled` match in `vike_core::runtime`'s `dispatch`): the
    /// halt it releases is an overlay the journal never held, so a replay core cannot re-apply it.
    ResumeVenue {
        venue: String,
        force: bool,
    },
    /// LIVE per-symbol leverage / margin change (RUST-NATIVE): writes `RiskLimits.im_by_symbol` on
    /// the target venue's engine, so a running engine is re-leveraged without a restart. An
    /// occasional control command (like `SetTradingState`), off the p99 event fold.
    SetMargin(Box<MarginUpdate>),
    /// ADMIT one more symbol into ONE account's engine (decision 0173): an account trades any symbol
    /// of its exchange, added the first time it is traded. The command carries the symbol's OWN
    /// facts (grid, contract value), resolved by the caller off the fold thread, so the fold needs
    /// no venue and a journal replay reproduces the engine without one. Sent BEFORE the first order
    /// on the symbol, on the same lane. Idempotent: a repeat refreshes the grid (the node re-sends
    /// it after a restart, which restores positions but not the symbol set). A refusal is a note in
    /// recent-events, never a panic. Occasional, off the p99<10µs event fold.
    AdmitSymbol(Box<SymbolAdmission>),
    ApplySnapshot(Box<ReconcileSnapshot>),
    /// Fetched venue reconciliation reports awaiting fold-thread diff/resolve — the write verb of
    /// `vike_tradehub_core::ReconManager`, whose thread does ONLY the blocking REST fetch. The
    /// single-writer fold thread reads its OWN engine's `local_view()`, runs the PURE
    /// `recon::diff` + `recon::resolve`, and folds the result through the SAME `on_event` path real
    /// venue events use, so there is NO cross-thread local-state read and NO staleness window.
    /// Unlike [`Command::ApplySnapshot`] (an already-diffed net snapshot) this carries raw REPORTS.
    /// Occasional (startup and the interval cadence), never the per-message hot fold.
    ReconcileReports(Box<ReconcileReports>),
    /// Operator approval of one held reconcile alert: the fold thread folds its `proposed_events`
    /// through the SAME `on_event` path, then removes it from the held store — the write half of
    /// `CoreSnapshot.recon.alerts`. The `u64` is a `ReconAlertView.id`, assigned by
    /// `reconcile_reports` when the alert was first held: the digest of the divergence's identity
    /// (venue, account, kind, instrument), so the same divergence has the same id after a restart
    /// and a retried id can only reach it. The held store itself restarts empty (a pass rebuilds
    /// it), and a pass that no longer raises a divergence removes its row, so an id the store does
    /// not hold - confirmed, cleared, or from before a restart - hits the unknown-id no-op a
    /// duplicate GUI click would (surfaced to recent-events, never a panic).
    ///
    /// The operator's production constructor is `WireCommand::ReconConfirm` (`vike-cli trade recon
    /// confirm`). An `OrphanLocalPosition` alert proposes no events; its confirm DROPS the local
    /// position through `ReconAlert::orphan_position` (`vike_core`'s `confirm_recon`).
    ConfirmRecon(u64),
    /// The operator's `recon cancel <id>` (decision 0196, rule (b)): send ONE cancel, through the
    /// account's own `ExecutionClient`, for the venue order the held `UnknownOrder` row `id` names
    /// by the client id the row carries. Operator only (`WireCommand::ReconCancel`; no strategy,
    /// GUI or MCP constructor) and `demo` tier only: the core refuses any other row, tier or kind
    /// with a ring note and a WARN. It folds nothing and removes no row: the row clears at the next
    /// reconcile pass if the venue really cancelled the order. The `u64` is the same
    /// `ReconAlertView.id` [`Command::ConfirmRecon`] takes.
    ///
    /// ⚠ Deliberately NOT journaled (the `journaled` match in `vike_core::runtime`'s `dispatch`): it
    /// changes no folded state, the held store it reads is not rebuilt by a replay, and a journaled
    /// new variant would stamp the segment format for a command a replay core could only refuse.
    /// Occasional, off the p99<10µs event fold.
    CancelReconOrder(u64),
    /// RUNTIME strategy MOUNT: add one mount to the running core WITHOUT a restart. The payload is
    /// a serializable SPEC; the core resolves it through the composition root's injected factory
    /// (`vike_core::CoreConfig::strategy_factory`; vike-exec sits below the strategy crates and
    /// can name no `Strategy` type). Occasional, off the p99<10µs event fold. Every failure
    /// (duplicate mount id, unknown venue, no factory, resolve error) is a REFUSAL note in
    /// recent-events, never a panic: unlike the spawn-time duplicate PANIC (a configuration fault
    /// at assembly), the process must keep trading.
    ///
    /// ⚠ Deliberately NOT journaled (the `journaled` match in `vike_core::runtime`'s `dispatch`):
    /// mount lifecycle is session topology, and the mount-less replay core could only replay it
    /// as a refusal (`StrategySubmit` provenance still names the mount id). Restart survival is
    /// DAEMON-LEVEL instead: the core records the spec in the `vike_core::mount_topology` sidecar
    /// (gated on `CoreConfig::state_dir`; unmount removes it), and the composition root re-sends
    /// this command at startup (vike-tradehub's `mount_factory`'s `resurrect_runtime_mounts`).
    MountStrategy(Box<MountSpec>),
    /// RUNTIME strategy UNMOUNT: remove the mount whose MOUNT ID matches `controller_id` (the
    /// explicit [`MountSpec::controller_id`], else the derived `{venue}__{symbol}__{interval}` id —
    /// `vike_core::strategy_state`'s `mount_id_with` is the identity law). Same discipline as
    /// [`Command::MountStrategy`]. Cancelling the mount's attributed live orders BEFORE removal is
    /// the core arm's contract, documented there.
    UnmountStrategy {
        controller_id: String,
    },
    Shutdown,
}

/// The [`Command::MountStrategy`] payload: WHERE the mount sits (`venue`/`symbol`/`interval` — the
/// addressing triple every strategy verb uses), WHO it is (`controller_id`, required when a mount
/// on the same triple already exists, as in spawn-time config), and WHAT to run — the profile
/// `[strategy]` vocabulary verbatim: a registry `name` XOR a `rhai` script path
/// (docs/decisions/0024-rhai-strategies-live.md), plus the free-form `params` table.
/// RESOLUTION (name → `Box<dyn Strategy>`) happens in the core via the injected factory.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MountSpec {
    /// Target venue — must name an engine the core already runs (a mount cannot conjure one).
    pub venue: String,
    /// The mount's own symbol.
    pub symbol: String,
    /// The mount's bar-series interval (e.g. `"1m"`).
    pub interval: String,
    /// **WHICH ACCOUNT of [`Self::venue`] this mount trades on.** `None` (every pre-field spec,
    /// file or wire frame) is the venue's DEFAULT account: the same engine, the same route key,
    /// byte-identical. `Some(label)` names a second account, and `vike_core`'s runtime mount
    /// REFUSES it by name when this core runs no engine for it rather than falling through to the
    /// default: a strategy executing on an account its author did not name is silent and
    /// unrecoverable.
    ///
    /// ⚠ **Serde goes through `vike_model::accounts::account_keys::wire_account_option`, not
    /// `AccountLabel`'s own impls.** `crates/vike-tradehub/src/server/control.rs`'s
    /// `WireCommand::MountStrategy` parses the field with `parse_wire_account`, which ADMITS
    /// `DEFAULT` (the wire's "the unlabelled account, deliberately"), and `AccountLabel`'s
    /// `Serialize` refuses it, so the `vike_core::mount_topology` sidecar (this field's ONE
    /// writer; the journal excludes `MountStrategy`) failed QUIETLY and the mount was not
    /// resurrected after a restart. The cure is the representation, deliberately NOT a refusal at
    /// the mount edge: the order plane (`vike_model::OrderRequest::account`) admits `DEFAULT`
    /// through the same parser, and the two planes must not disagree about one spelling. A `Named`
    /// label's bytes are unchanged.
    /// `vike_core::mount_topology`'s `a_mount_naming_the_default_account_is_recorded_and_reads_back`
    /// is the gate.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "vike_model::accounts::account_keys::wire_account_option"
    )]
    pub account: Option<vike_model::accounts::account_keys::AccountLabel>,
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
    /// The `[strategy.params]` table (default empty). The factory applies a profile load's
    /// refusals (unknown/mistyped keys refuse, never mount at a compiled default).
    ///
    /// A TABLE by type (decision 0145): its JSON is the object `serde_json::Value` wrote before
    /// this field was [`vike_model::params::Params`], byte for byte, so a `vike_core::mount_topology`
    /// sidecar written then reads back unchanged. What a parameter table cannot hold (a non-object
    /// such as `5`, `[1]` or `null`; a `null` inside; an integer above `i64::MAX`; a duplicate key)
    /// is refused when the spec is DESERIALIZED.
    #[serde(default)]
    pub params: vike_model::params::Params,
}

/// The [`Command::ReconcileReports`] payload: the three venue report kinds a reconcile pass
/// fetched, plus the `venue` this pass READ (the canonical exchange id — the health-probe key, the
/// label on every note and log line, and the identity half of a held alert's dedup row), the
/// [`Self::route_key`] that selects the target ENGINE, the `since` lower bound the reports were
/// pulled with, and the `policy` that decides fold-vs-quarantine per divergence kind. Built on the
/// reconcile-manager thread; consumed on the fold thread via `recon::diff`/`recon::resolve`.
///
/// ⚠ **`venue` and `route_key` are two facts.** With one field, a second account of one exchange
/// breaks either way: spelled canonically, its reports diff against the FIRST account's local
/// view and fold into the first account's book (under `hybrid`, `PositionDrift` rewrites position
/// size onto the other account's number); spelled as the route key, the manager's per-venue health
/// gate (`ReconManager::should_reconcile`) reads an unknown venue as `Healthy`, silently dropping
/// the degraded-feed suppression for that account.
///
/// `balance` is the venue's authoritative cash ([`crate::recon::ReconClient::fetch_balance`]),
/// fetched off-fold alongside the reports. `None` (the default trait impl, or a fetch error) means
/// "not reported": the fold thread leaves `Account` balance/mode untouched.
///
/// `generate_missing_orders`, `reconcile_balance` and `balance_tol` mirror the matching
/// `vike_tradehub_core::ReconConfig` fields for this one pass: they ride ALONGSIDE `policy` because the
/// fold thread has no other view of the driver's `ReconConfig` (built on a different thread).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReconcileReports {
    pub venue: String,
    pub since: i64,
    pub orders: Vec<OrderStatusReport>,
    pub fills: Vec<FillReport>,
    pub positions: Vec<PositionStatusReport>,
    pub policy: ReconPolicy,
    pub balance: Option<f64>,
    /// `false` (the default, inert): `recon::resolve` never synthesizes `UnknownOrder` adoption
    /// events.
    #[serde(default)]
    pub generate_missing_orders: bool,
    /// Cash reconcile (`VIKE_RECONCILE_BALANCE`). `false` (the default) ⇒ the fold thread takes
    /// the silent authoritative balance seed; `true` ⇒ venue cash is DIFFED against the
    /// realized-PnL-corrected local balance and any drift routed through `policy` (quarantined by
    /// default — a surprise cash move is never auto-folded).
    #[serde(default)]
    pub reconcile_balance: bool,
    /// The money tolerance `diff_balance` compares against, read only when `reconcile_balance` is
    /// `true`. Default [`BalanceTol::default`], the conservative constant.
    #[serde(default)]
    pub balance_tol: BalanceTol,
    /// Which ENGINE this pass's divergences fold into — `None` meaning "[`Self::venue`]'s sole
    /// account in this process", which is also what every pre-field journal meant, so a replayed
    /// pass routes exactly where it routed live.
    ///
    /// Read through [`Self::route`], never directly: that accessor makes `None` mean the venue
    /// rather than the empty string, and hands back a [`crate::RouteKey`], the only thing the
    /// router accepts.
    ///
    /// `Some` is produced: `vike_tradehub_core::ReconLeg` keys the reconcile manager per ACCOUNT, so a
    /// labelled account enqueues a payload carrying the canonical venue AND its own route key.
    ///
    /// ⚠ `None` means exactly one thing, ENFORCED at both ends: every account of a venue this
    /// process runs several ENGINES of is stamped at assembly
    /// (`vike_tradehub_core::ReconLeg::name_accounts_of_shared_venues`, the DEFAULT account's leg included),
    /// and `vike_core`'s `CoreThread::reconcile_reports` REFUSES a `None` payload on a venue it
    /// runs several engines of rather than folding it against the default book. ⚠ BOTH ends must
    /// count ENGINES, or a venue with two engines and one reconcile leg has that leg's own pass
    /// refused every interval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_key: Option<String>,
    /// The ADMITTED symbols (decision 0173) the client that fetched these reports had been told about
    /// when it read them (`crate::recon::ReconClient::admitted_symbols`, the claim of
    /// `crate::recon::admitted_throughout`). `None`, the default: the client keeps no list, and the
    /// restore step judges every symbol the engine serves, exactly as it did before this field.
    /// `Some`: an on-demand symbol of the engine that is NOT listed was not fetched, so the venue's
    /// silence about an order on it says nothing and the restore step HOLDS that order
    /// (`crates/vike-core/src/runtime/restore.rs`'s module doc); a declared leg and the mounted
    /// symbol are judged either way. `Some(vec![])` is a claim ("told nothing"), not an absence.
    ///
    /// Journaled with the pass, so a replay takes the restore verdicts the live pass took; `serde`
    /// reads every journal written before the field as `None`, and a `None` writes no key, so a box
    /// whose client keeps no list keeps its bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admitted_symbols: Option<Vec<String>>,
    /// The KIND twin of [`Self::admitted_symbols`]: the client that fetched these reports CLAIMS its
    /// open-order read lists the venue's trigger orders (`crate::recon::ReconClient::lists_trigger_orders`,
    /// read by the manager beside the fetch). `false`, the default and every client's answer unless
    /// it proves otherwise: the restore step does not declare a restored order of a trigger kind
    /// gone on the venue's silence, it holds it as unproven
    /// (`crates/vike-core/src/runtime/restore.rs`'s module doc, "A KIND the client does not list").
    ///
    /// Journaled with the pass, so a replay takes the restore verdicts the live pass took; `serde`
    /// reads every journal written before the field as `false` (the safe side: hold), and a `false`
    /// writes no key, so a box whose client makes no claim keeps its bytes.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub lists_trigger_orders: bool,
}

impl ReconcileReports {
    /// The engine this pass routes to. `None` ⇒ [`Self::venue`]'s sole account — what a venue with
    /// ONE account produces; a labelled account's pass carries `Some` (see `route_key`).
    ///
    /// ⚠ Route with THIS, never with `&self.venue`: the two are equal for a venue with one account,
    /// which is why the wrong one cannot be spotted by reading. `vike_core`'s `confirm_recon`
    /// re-resolves the same engine from a HELD alert minutes later, so the choice outlives the
    /// pass.
    #[inline]
    pub fn route(&self) -> crate::RouteKey<'_> {
        match &self.route_key {
            Some(k) => crate::RouteKey::declared(k),
            None => crate::RouteKey::sole_account_of(&self.venue),
        }
    }
}

/// The payload of a [`Command::UpdateParams`] live-parameter update. `(venue, symbol, interval)`
/// names the target series EXACTLY (the same key the bar path resolves a mount by), `mount_id`
/// names WHICH mount on it, and `params` is the typed [`StrategyParams`] the mount's
/// `on_params_updated` hook consumes.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ParamsUpdate {
    pub venue: String,
    pub symbol: String,
    pub interval: String,
    /// The mount to retune, by the id [`crate::MountView::mount_id`] publishes (a
    /// `controller_id` is sanitized the way the core stores it, so `maker-a` finds `maker_a`).
    ///
    /// `None` is the pre-field shape and means "the ONE mount on the triple": it is delivered when
    /// exactly one live mount sits on `(venue, symbol, interval)` and REFUSED (a recent-events note,
    /// nothing retuned) when two or more share it, because the core never picks the first. `Some`
    /// must also agree with the triple, or it is refused as a stale view. `Some("")` is refused.
    ///
    /// `serde(default)` reads every journal and wire frame written before the field existed, and
    /// `None` serializes with no key, so those bytes are unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mount_id: Option<String>,
    pub params: StrategyParams,
}

/// The payload of a [`Command::SetMargin`] live leverage change: set `symbol`'s initial-margin
/// fraction (`1/leverage`) on `venue`'s engine (`RiskLimits.im_by_symbol`). Other engines and
/// symbols are untouched.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MarginUpdate {
    pub venue: String,
    pub symbol: String,
    pub im_requirement: f64,
}

/// The payload of a [`Command::AdmitSymbol`]: WHICH account's engine (`route_key`, the engine's own
/// `ExecutionEngine::route_key`), WHICH symbol, and the facts the engine needs to judge an order on
/// it. `ExecutionEngine::admit_symbol` applies it.
///
/// `grid` must carry a tick and a lot: a `None` field of a [`vike_model::SymbolGrid`] INHERITS the
/// mounted symbol's scalar, which is the very defect admission closes, so a grid without them is
/// refused. `on_demand` is set by the engine, whatever the caller wrote. `contract_value` is `None`
/// for the default (`Linear`, 1.0), which is every first-wave venue's.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SymbolAdmission {
    pub route_key: String,
    pub symbol: String,
    pub grid: vike_model::SymbolGrid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract_value: Option<vike_model::ContractValue>,
    /// A price the caller observed for the symbol, seeded into the engine's price board when the
    /// admission folds (decision 0174). The engine only sees quotes for symbols it accepts, so without
    /// it the first order on a new symbol is judged with no mark: no collar, no exposure cap. It rides
    /// the journaled command, so a replay seeds the same mark. A non-finite or non-positive value is
    /// ignored, and a mark the engine already holds is never overwritten.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_mark: Option<f64>,
    /// The currency the symbol is QUOTED in ("JPY" for USDJPY), set ONLY for a symbol quoted in a
    /// currency other than the account's; `None` is a symbol quoted in the account's own currency,
    /// which is every first-wave venue's, and writes no key (so such an admission keeps its bytes and
    /// its journal version). `Some` makes the money of the symbol the account's through
    /// [`Self::quote_rate`]: the engine multiplies the symbol's contract value by it
    /// (`vike_model::ContractValue::in_account`), so notional, margin and both PnLs come out in the
    /// account's currency. A quote currency the caller could not name is NOT `None`: the caller
    /// refuses the symbol, because an absent currency is never read as the account's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote_ccy: Option<String>,
    /// Account-currency units per ONE unit of [`Self::quote_ccy`] (USDJPY on a USD account is about
    /// `1/150`, never `150`), observed at [`Self::rate_as_of_ms`]. Required with `quote_ccy`: an
    /// admission that names a foreign currency and carries no usable rate is refused, never valued
    /// at par. Journaled with the command, so a replay prices every fill with the rate that preceded
    /// it and needs no venue. A repeat admission of the symbol is how the rate is refreshed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote_rate: Option<f64>,
    /// When [`Self::quote_rate`] was observed, in the node's clock (the clock the engine's `now_ms`
    /// is stamped from); the engine judges the rate stale once it is older than
    /// `vike_exec::account::DEFAULT_QUOTE_RATE_TTL_MS`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_as_of_ms: Option<i64>,
    /// This symbol's INITIAL-MARGIN rate (`1 / leverage`: 0.25 is 4x, 1.0 is no leverage), when the
    /// venue holds a leverage for the symbol that differs from the engine's. `None` writes nothing:
    /// the symbol is margined at what it always was, the engine-wide `RiskLimits::im_requirement`
    /// (or by an operator's `Command::SetMargin` row). `Some(x)` for `x` in `(0, 1]` writes the
    /// symbol's `RiskLimits::im_by_symbol` row, which `RiskLimits::im_for` already puts ahead of the
    /// engine-wide rate in every reader (the gate's buying power, the margin fold, the published
    /// block), and a repeat admission REPLACES it. The admission is the COMPLETE statement of an
    /// ADMITTED symbol's row: a repeat admission with `None` REMOVES the row an earlier one wrote
    /// (the node re-sends the figure with every refresh, so a missing figure means "the venue states
    /// none", and the old one must not keep margining the symbol). Any other value (not finite, or
    /// outside `(0, 1]`: no leverage anybody holds) is IGNORED, exactly as `None` (decision 0183
    /// item 11) and never refuses the admission, which also carries the quote-currency rate refresh.
    /// Journaled with the command, so a replay margins the symbol as the live run did (a NaN is
    /// journaled as `null`, which reads back as `None`: ignored, the same verdict).
    ///
    /// Stated for the mounted symbol too, where it is the only field an admission writes (the
    /// outcome stays `Mounted`): a spot symbol mounted on an engine whose engine-wide rate is a
    /// perp's leverage says `1.0` here. For the mounted symbol `None` writes nothing (its row may be
    /// the operator's `Command::SetMargin`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub im_requirement: Option<f64>,
}

impl SymbolAdmission {
    /// Whether the admission states a conversion rate that is not a finite number. `serde_json` writes
    /// a non-finite `f64` as `null`, so a journaled `Some(NaN)` reads back as `None`: the live engine
    /// would refuse it (`no-quote-rate`, or `quote-ccy-missing` for an admission that names no
    /// currency) and a replay would not see it at all and admit the symbol at par. The core refuses
    /// such a command BEFORE it can be journaled (`vike_core`'s `dispatch`), so the two cannot differ.
    /// (`im_requirement` needs no twin: a figure that is not a number is ignored, which is also what
    /// `null` reads back as.)
    pub fn states_non_finite_rate(&self) -> bool {
        self.quote_rate.is_some_and(|r| !r.is_finite())
    }
}

/// One ingest message. `Market` is a marker — the payload rides the conflation slot.
/// Bar lanes: `BarSeed`/`BarClose` are LOSSLESS (a missed close = a hole in the series);
/// forming-bar updates conflate with the ticks (latest-wins per series key).
// large_enum_variant: Event IS the dominant hot-path variant; boxing it would trade a
// 168-byte move (measured, and pinned below this enum) for a per-event heap allocation + pointer
// chase on exactly the path the p99<10µs gate protects. Commands (rare) are already boxed.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub enum Ingest {
    Event(Event),
    /// A venue event that arrived over a gateway's TRADE LINK, with the position it held in that
    /// gateway's per-account event ring ([`GatewayEvent`]). Journaled INSTEAD of
    /// [`Ingest::Event`], never beside it: the event and its cursor are ONE record, so a crash can
    /// never leave the event folded and its cursor unrecorded (or the reverse), and an event that
    /// is not idempotent (a funding payment has no id and `apply_funding` is not a set) can never
    /// be folded twice across a restart. Folded exactly as an [`Ingest::Event`] is
    /// (`vike_core`'s dispatch calls one shared helper for both); the cursor only rides along.
    /// Boxed: `Event` is the one unboxed hot variant (see the size guard below).
    ///
    /// ⚠ Every consumer of the journal that reads an `Event` out of an `Ingest` must read this
    /// variant too, or the event vanishes there without a sound. Ask [`Ingest::venue_event`] instead
    /// of matching `Ingest::Event` by hand.
    GatewayEvent(Box<GatewayEvent>),
    /// The ACCOUNT's trade link to its venue gateway went down or came back ([`ExecLinkUpdate`]):
    /// the remote execution client's disclosure that its orders cannot be answered right now. An
    /// OCCASIONAL control event, fired only on a TRANSITION, so it adds no steady-state traffic and
    /// touches NO OMS fold. The core files it per account so the stuck-order watchdog stops counting
    /// while the link is down and resumes counting from the link-up instant. Boxed; NOT journaled (a
    /// transport fact about this process's connection, like [`Ingest::StreamStatus`]: a restart
    /// begins with the link presumed up until its client discloses otherwise, which is why the
    /// client owes its INITIAL state at attach: see [`ExecLinkUpdate`]).
    ///
    /// Not a venue event: [`Ingest::venue_event`] answers `None` for it, and it never advances the
    /// silence dead-man's freshness.
    ExecLink(Box<ExecLinkUpdate>),
    Command(Command),
    Market,
    /// REST warmup backfill: replace the whole series (boxed — rare, large)
    BarSeed(Box<BarSeed>),
    /// a bar CLOSED — append to the series (lossless lane)
    BarClose(Box<BarUpdate>),
    /// L2/tick lanes — lossless, per-update strategy dispatch. Boxed so the hot `Event`/`Market`
    /// path keeps a small enum. They touch NO parity-gated OMS fold path, only the mounted
    /// strategy's tick handlers.
    Quote(Box<QuoteUpdate>),
    Trade(Box<TradeUpdate>),
    Book(Box<BookUpdate>),
    /// Opt-in stuck-order watchdog tick: sweep orders stuck pre-ack past `submit_ack_timeout`.
    /// No payload; only enqueued when the watchdog is enabled.
    Watchdog,
    /// Feed-health status CHANGE — the disconnect/stale/recover signal a mounted strategy reacts
    /// to (`Strategy::on_feed_status`). Fired ONLY on a `StreamStatus` transition (a feed's
    /// stream-health machine → its `LiveDataSink` boundary), so it adds no per-market-message
    /// traffic and touches NO OMS fold path. Boxed; NOT journaled (advisory telemetry).
    StreamStatus(Box<StreamStatusUpdate>),
    /// Per-side FLOW-TOXICITY update (RTDS wallet-toxicity guard) — the widen/cut signal a mounted
    /// market maker reacts to (`Strategy::on_flow`). Fired at toxicity cadence by an aggregator
    /// over the wallet-attributed activity tape, NOT per market message, and touches NO OMS fold
    /// path. Boxed; NOT journaled. The twin of [`Ingest::StreamStatus`].
    Flow(Box<FlowUpdate>),
    /// A stream's DELAY LABEL changed (decision 0140): real-time, delayed or frozen, as the venue
    /// said for the data it sent. Fired only when the label is first known and on each CHANGE, never
    /// per market message, and it touches NO OMS fold: the core files it on every mount of that
    /// `(venue, symbol)` and in the table the submit path stamps an order from. The label is
    /// information, never a gate. Boxed; NOT journaled. The twin of [`Ingest::StreamStatus`].
    DataDelay(Box<DataDelayUpdate>),
    /// A change to the SET of accounts the RUNNING core holds (decision 0169, slice S1: an engine is
    /// APPENDED; slice S2: one is STOPPED, never removed). A cold control message, sent once per account start by the tradehub's account
    /// watcher, never on the market path. Boxed so `Ingest` does not grow (the size guard below).
    ///
    /// ⚠ **Never serialized, never journaled, and LAST on purpose**: it carries a live engine, not
    /// data (`#[serde(skip)]`: serializing it is an error that cannot happen, because the core's
    /// `dispatch` journals nothing for it), and a variant added at the end moves no existing
    /// variant's index. A replay or a refold skips it. [`Ingest::venue_event`] answers `None`.
    #[serde(skip)]
    Accounts(Box<AccountChange>),
}

/// A change to the set of engines a running core holds ([`Ingest::Accounts`]).
///
/// The engine travels as `Box<dyn Any + Send>` because `Ingest` is not generic over the core's client
/// type: the core downcasts it to the `ExecutionEngine<C>` it was spawned over (production: `C =
/// Box<dyn ExecutionClient + Send>`) and REFUSES, with a note, a parcel of any other type. The
/// engine's index in the core is its attribution key for ever, so a change only ever APPENDS.
pub enum AccountChange {
    /// Append `engine` (built by the same code as a startup engine) with equity seed `seed`.
    AddEngine {
        /// The equity seed the core folds this engine against (`RuntimeConfig::seed_cash`).
        seed: f64,
        /// An `ExecutionEngine<C>`, type-erased; see the type's doc.
        engine: Box<dyn std::any::Any + Send>,
    },
    /// STOP the account `not_running` names (decision 0169, slice S2), phase one: cancel its resting
    /// orders (and ONLY its own), tombstone its engine and list it as NOT RUNNING with `not_running`'s
    /// reason. Its client stays attached, so the venue's answers to those cancels still reach the
    /// engine; [`AccountChange::DetachEngine`] ends the session once they have. The engine is NOT removed:
    /// the list only grows, so every index stays the attribution key of what it wrote; a later start of
    /// the same account appends a new one.
    StopEngine {
        /// The account that stops: venue, label, route key and the reason it is listed with.
        not_running: crate::read_model::NotRunningAccount,
    },
    /// STOP, phase two: begin the detach of the client of the account `route_key` whose engine
    /// [`AccountChange::StopEngine`] tombstoned (the lease closes on the link's own thread; nothing joins
    /// on the fold). Sent by the caller once the account's orders are terminal, or a bound has passed.
    DetachEngine {
        /// The route key of the stopped account.
        route_key: String,
    },
}

impl AccountChange {
    /// An [`AccountChange::AddEngine`] over `engine`, whatever its client type.
    #[must_use]
    pub fn add_engine<C: ExecutionClient + Send + 'static>(
        seed: f64,
        engine: ExecutionEngine<C>,
    ) -> AccountChange {
        AccountChange::AddEngine { seed, engine: Box::new(engine) }
    }
}

impl std::fmt::Debug for AccountChange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AccountChange::AddEngine { seed, .. } => {
                f.debug_struct("AddEngine").field("seed", seed).finish_non_exhaustive()
            }
            AccountChange::StopEngine { not_running } => {
                f.debug_struct("StopEngine").field("not_running", not_running).finish()
            }
            AccountChange::DetachEngine { route_key } => {
                f.debug_struct("DetachEngine").field("route_key", route_key).finish()
            }
        }
    }
}

/// Where an event stood in a gateway's per-account event ring: the resume point a restarted
/// consumer presents so the gateway can replay what it missed.
///
/// `gw_epoch` is a RANDOM id the gateway mints per process start (it is not ordered), `seq` is the
/// per-account, per-epoch sequence number the gateway assigned when it emitted the event, and
/// `ts_ms` the event's own timestamp (the floor a gateway restart's history replay would start
/// from). A cursor is only comparable with another of the SAME epoch; [`Self::after`] is the one
/// rule that folds a newer observation into a held one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GatewayCursor {
    pub gw_epoch: u64,
    pub seq: u64,
    pub ts_ms: i64,
}

impl GatewayCursor {
    /// The cursor to HOLD once `seen` has been observed after `self`, in the order the observations
    /// were folded (journal order). Inside one epoch the larger `seq` wins, so a straggler that a
    /// replay re-delivers (a lower `seq`) can never move the resume point backwards; a different
    /// epoch means the gateway restarted and its ring began again, so the later observation wins
    /// whatever its `seq` (an epoch is random: there is no "larger" one).
    ///
    /// This is the ONE rule: the core's live fold, the boot restore's fold over the journal tail
    /// and the `Snap` all use it, so a cursor means the same thing in each.
    #[inline]
    pub fn after(self, seen: GatewayCursor) -> GatewayCursor {
        if seen.gw_epoch == self.gw_epoch && seen.seq < self.seq { self } else { seen }
    }
}

/// One venue event that came over a gateway's trade link, and the ring position it held there
/// ([`Ingest::GatewayEvent`]).
///
/// `route_key` names the ACCOUNT whose ring the cursor belongs to, spelled as the core spells an
/// account's engine (`vike_tradehub_core::account_route_key`: the bare venue id for a venue's
/// default account). It rides the record because the sender's own route key is `None` for exactly
/// that default account, and because the engine an event folds into is NOT a safe stand-in for the
/// ring it came from (an event the router cannot attribute still advanced its ring). The
/// producer, which owns the link, knows it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GatewayEvent {
    pub event: Event,
    pub route_key: Ustr,
    pub cursor: GatewayCursor,
}

/// One transition of an account's TRADE LINK ([`Ingest::ExecLink`]).
///
/// `route_key` names the account exactly as [`GatewayEvent::route_key`] does (the bare venue id for
/// a venue's default account), and is what the core matches against each engine's own route key.
/// `up == false` is the link going down, `up == true` the link re-established; the producer fires
/// one message per transition and never repeats a state. `at_ts_ms` is the producer's wall clock at
/// the transition, for the operator's log; the core counts the watchdog's resumed interval on its
/// own dispatch clock, the clock every order's `created_ms` was stamped with.
///
/// ⚠ **A producer owes its INITIAL link state, not only its transitions.** `ExecLink` is not
/// journaled, so a core that starts or restores holds an EMPTY link map and presumes every link
/// up. A client that connects (or first dials) while the link is down must therefore send
/// `up: false` at attach, before the core's first stuck-order sweep can see an order of the
/// restored snapshot (its `created_ms` is old, so the ladder would confirm and then reject an
/// order that may rest at the venue, with the link still down). "One message per transition"
/// counts the attach as the transition from "unknown" to the client's current state. The same
/// debt exists for the dead-man's trade-link `StreamStatus`: a source that starts down and
/// discloses nothing opens no window.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExecLinkUpdate {
    pub route_key: Ustr,
    pub venue: Ustr,
    pub up: bool,
    pub at_ts_ms: i64,
}

impl Ingest {
    /// The venue [`Event`] this message carries, if it carries one: [`Ingest::Event`] and
    /// [`Ingest::GatewayEvent`] alike.
    ///
    /// The ONE place that answers "is this message a venue event?" for the readers of a journal
    /// (the tearsheet's fills, the boot restore's crash-tail facts, the materializer). It is
    /// EXHAUSTIVE on purpose: a variant added to [`Ingest`] must be classified here, where a `_ =>`
    /// would have dropped it from every reader without a sound.
    pub fn venue_event(&self) -> Option<&Event> {
        match self {
            Ingest::Event(e) => Some(e),
            Ingest::GatewayEvent(g) => Some(&g.event),
            Ingest::ExecLink(_)
            | Ingest::Command(_)
            | Ingest::Market
            | Ingest::BarSeed(_)
            | Ingest::BarClose(_)
            | Ingest::Quote(_)
            | Ingest::Trade(_)
            | Ingest::Book(_)
            | Ingest::Watchdog
            | Ingest::StreamStatus(_)
            | Ingest::Flow(_)
            | Ingest::DataDelay(_)
            | Ingest::Accounts(_) => None,
        }
    }
}

// ⚠ SIZE GUARDS (plan idea I-20): `Ingest::Event(Event)` is the dominant hot-path variant and is
// deliberately NOT boxed, so a move of an `Ingest` is a move of an `Event`. MEASURED 2026-10-10
// on the latency box (x86_64 Linux, rustc 1.97.1): `size_of::<Ingest>() == size_of::<Event>() == 168`. The
// comment above this enum used to say "240-byte move" and nobody noticed it was stale for as long
// as it was, which is what these two lines are for: a field added to `Event` or to any `Ingest`
// variant that grows the move past the bound stops the build HERE, where the person who grew it
// has to either shrink it (box the new field) or raise the bound and say what the p99 gate
// (`crates/vike-core/tests/runtime_latency.rs`) made of it. The bound is the measured size, not a
// guess, and not a ceiling with headroom: headroom is how a 168-byte move becomes a 240-byte one.
// 64-bit only: the sizes are not claimed for other pointer widths.
#[cfg(target_pointer_width = "64")]
const _: () =
    assert!(std::mem::size_of::<Ingest>() <= 168, "Ingest grew past its measured 168 bytes");
#[cfg(target_pointer_width = "64")]
const _: () =
    assert!(std::mem::size_of::<Event>() <= 168, "Event grew past its measured 168 bytes");

/// A per-side FLOW-TOXICITY update on the control lane. Routed by EXACT `(venue, symbol)` like
/// [`StreamStatusUpdate`] (the toxic tape's `asset` IS the mounted token), carrying the
/// [`FlowToxicity`] the `on_flow` hook receives.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FlowUpdate {
    pub venue: String,
    pub symbol: String,
    pub flow: FlowToxicity,
}

/// A feed-health status CHANGE on the control lane, routed to the strategy mounted on
/// `(venue, symbol)`, carrying the [`FeedStatus`] its `on_feed_status` hook receives. `stream`
/// names the lane as the data verbs do (`"1m"` for a bar stream; for tick lanes one of
/// `vike_data::StreamLabel`'s four words, `"quotes"`/`"trades"`/`"book"`/`"depth"`; the CEX tick
/// pumps' own link disclosure stamps `"ticks"`, which names their socket, not a lane). Dispatch routes
/// the status to the strategy by `(venue, symbol)`, but ⚠ **`stream` is LOAD-BEARING for the core's
/// connection-state dead-man**: it names the SOURCE of the disclosure
/// (`vike_venues::link_deadman::LinkSource::of`), and the latch keeps one window per source, so a
/// `Live` closes only the window of the source whose word it carries. Two words are RESERVED for the
/// hub links (`vike_venues::link_deadman::TRADE_LINK_STREAM`, `STRAT_LINK_STREAM`); every other word
/// is the venue's own socket. A producer owes a closing `Live` under the SAME word as each
/// `Disconnected`, and nothing between producer and core may rewrite, normalize or coalesce the field.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StreamStatusUpdate {
    pub venue: String,
    pub symbol: String,
    pub stream: String,
    pub status: FeedStatus,
}

/// A stream's delay-label CHANGE on the control lane (decision 0140), routed by EXACT
/// `(venue, symbol)` like [`StreamStatusUpdate`]. `stream` names the lane as the data verbs do
/// (`"quotes"`, `"1m"`, …) — diagnostics only; the core files the label per `(venue, symbol)`,
/// because an order is priced off its symbol, not off one lane.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DataDelayUpdate {
    pub venue: String,
    pub symbol: String,
    pub stream: String,
    pub delay: DataDelay,
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
/// **The book rides behind an [`Arc`], NOT by value.** A venue pump keeps ONE standing book and
/// folds each depth delta into it; by value, every delta deep-cloned two whole `BTreeMap`s (up to
/// a 1000-level binance seed) and paid the matching drop on the core thread the `p99 < 10µs` gate
/// measures. With an `Arc` the send is a refcount bump; a pump mutates its own copy through
/// [`Arc::make_mut`], so it deep-clones ONLY while a consumer still holds the previous handle, on
/// the PUMP thread. The backtest's twin is `crates/vike-sim/src/engine.rs`'s `apply_book_event`.
///
/// SERDE (the `Ingest` derive is all-or-nothing, though `Ingest::Book` is not journaled): `book`
/// goes through the `book_serde::{ser_book, de_book}` pair, which call `L2Book`'s OWN impls, so
/// the bytes equal a bare book's and no `serde/rc` feature is needed (the workspace does not
/// enable it). Pinned by `arc_book_field_serializes_exactly_like_a_bare_book`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BookUpdate {
    pub venue: String,
    pub symbol: String,
    #[serde(serialize_with = "book_serde::ser_book", deserialize_with = "book_serde::de_book")]
    pub book: Arc<L2Book>,
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

/// Latest price tick for (venue, symbol) — the live-feed seam. Which SLOT it fills downstream
/// depends on the publish verb: [`MarketSender::publish`] carries the venue's REAL mark price
/// (the `PriceBoard` MARK slot), [`MarketSender::publish_bar_close`] a kline feed's candle-close
/// snapshot (the BAR-CLOSE slot).
#[derive(Debug, Clone)]
pub struct MarketTick {
    pub venue: String,
    pub symbol: String,
    pub px: f64,
    pub ts: i64,
}

/// Per-symbol latest-wins slots + the shared marker state. `marker_in_flight` lives under the
/// same lock as the slots so the slot-write/marker-send decision is atomic.
#[doc(hidden)]
pub struct Conflated {
    #[doc(hidden)]
    pub state: Mutex<ConflatedState>,
    #[doc(hidden)]
    pub drops: AtomicU64,
}

#[derive(Default)]
#[doc(hidden)]
pub struct ConflatedState {
    /// (venue, symbol) -> freshest REAL-mark tick; insertion-ordered drain
    #[doc(hidden)]
    pub slots: indexmap::IndexMap<(String, String), MarketTick>,
    /// (venue, symbol) -> freshest candle-close tick — a SEPARATE slot map from `slots` so a
    /// 1s-cadence real-mark stream can never conflate away a kline feed's bar-close tick for the
    /// same symbol (the two feed different `PriceBoard` slots downstream)
    #[doc(hidden)]
    pub bar_close_slots: indexmap::IndexMap<(String, String), MarketTick>,
    /// (venue, symbol, interval) -> freshest FORMING bar (intrabar updates conflate;
    /// closes ride the lossless BarClose lane)
    #[doc(hidden)]
    pub forming: indexmap::IndexMap<SeriesKey, Bar>,
    /// at most ONE marker is ever queued; the core clears it when it drains the slots
    #[doc(hidden)]
    pub marker_in_flight: bool,
}

/// Producer half of the market-data conflation lane (Clone per feed thread).
#[derive(Clone)]
pub struct MarketSender {
    #[doc(hidden)]
    pub inner: Arc<Conflated>,
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
    /// bar). Same wait-free marker protocol as [`MarketSender::publish`], but a separate slot map,
    /// so bar-close and real-mark ticks for one (venue, symbol) never conflate each other away
    /// (they fill different `PriceBoard` slots downstream).
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

/// Lossless L2/tick lane. Unlike [`MarketSender`] (latest-wins mark conflation),
/// each quote/trade/book update is a DISTINCT event a strategy may act on, so these back-pressure
/// the feed rather than conflate — mirroring [`BarSender`]'s lossless discipline.
#[derive(Clone)]
pub struct TickSender {
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

    /// Feed-health status CHANGE, sent ONLY on a `StreamStatus` transition (disconnect/stale/
    /// recover), so a lossless send costs nothing steady-state. Delivered to the mounted
    /// strategy's `on_feed_status` hook.
    pub fn stream_status(&self, update: StreamStatusUpdate) -> Result<(), CoreGone> {
        self.ingest.blocking_send(Ingest::StreamStatus(Box::new(update))).map_err(|_| CoreGone)
    }

    /// Per-side FLOW-TOXICITY update, sent at toxicity cadence, NOT per market message. Delivered
    /// to the mounted strategy's `on_flow` hook. The twin of [`TickSender::stream_status`].
    pub fn flow(&self, update: FlowUpdate) -> Result<(), CoreGone> {
        self.ingest.blocking_send(Ingest::Flow(Box::new(update))).map_err(|_| CoreGone)
    }

    /// A stream's delay-label CHANGE (decision 0140), sent only when the label is first known and
    /// when it changes, so a lossless send costs nothing steady-state. The core files it per mount
    /// and in the submit path's stamp table. The twin of [`TickSender::stream_status`].
    pub fn data_delay(&self, update: DataDelayUpdate) -> Result<(), CoreGone> {
        self.ingest.blocking_send(Ingest::DataDelay(Box::new(update))).map_err(|_| CoreGone)
    }
}

/// Lossless exec-event lane for venue tasks (async or blocking contexts).
#[derive(Clone)]
pub struct EventSender {
    #[doc(hidden)]
    pub ingest: mpsc::Sender<Ingest>,
    /// **WHICH ACCOUNT the producer behind this handle speaks for** — set by
    /// [`EventSender::routed`] at the MOUNT, `None` on every handle the core hands out. A venue
    /// adapter cannot answer that (it holds one credential set and stamps the canonical venue id),
    /// so without it `vike_core`'s router cannot tell two accounts' account-wide payloads apart;
    /// the mount knows, so the answer is attached there rather than taught to every bridge.
    #[doc(hidden)]
    pub route_key: Option<Ustr>,
}

impl EventSender {
    /// This lane, scoped to ONE venue ACCOUNT: the returned handle stamps `route_key` onto every
    /// account-wide payload it carries (see `stamp`), so `vike_core`'s `route_event` can
    /// fold it into that account's own engine.
    ///
    /// ⚠ **A key equal to the payload's own venue stamps NOTHING** — the byte-identity argument,
    /// not an optimisation. `vike_tradehub_core::account_route_key` renders the bare venue id for
    /// `AccountLabel::Default`, so the default account (every account on a single-account box)
    /// produces `route_key: None`, which `skip_serializing_if` omits: a box with no `[accounts]`
    /// table writes the journal bytes it always did, and the mount can call this unconditionally.
    /// The comparison is per-EVENT, against the payload's venue (one mount's lane carries one
    /// venue's frames, and the payload cannot be wrong about its own venue).
    ///
    /// Off the fold thread: this runs on the venue adapter's thread/task, on the way IN to the
    /// ingest channel.
    pub fn routed(&self, route_key: &str) -> EventSender {
        EventSender { ingest: self.ingest.clone(), route_key: Some(Ustr::from(route_key)) }
    }

    /// Stamp [`Self::route_key`] onto an account-wide payload that carries no other routing handle.
    ///
    /// **THE THREE venue-tagged payloads with no client-order-id**: [`Event::AccountState`],
    /// [`Event::Funding`] and [`Event::PositionLiquidated`]. [`Event::Fill`] is deliberately NOT
    /// here — it names its order, which `vike_core`'s `route_event` resolves exactly through its
    /// submit-time `coid_venue` map; stamping it would add a redundant key to the one wire shape
    /// the frozen parity fixtures pin.
    ///
    /// ⚠ **The symbol does NOT disambiguate two accounts**: two accounts on one instrument is an
    /// ordinary SPREAD (the point of the mount `account` field). Funding and liquidation carry no
    /// coid, so unstamped on a shared symbol they fall to the venue's DEFAULT engine: a labelled
    /// account's funding debit lands on the default account's balance, and its liquidation CLOSES
    /// the default account's position.
    ///
    /// ⚠ **`&mut`, not by-value, for the LATENCY GATE.** `blocking_send` sits inside the measured
    /// core hop (`crates/vike-core/tests/runtime_latency.rs` feeds `Ingest::Event(Event::Fill)`
    /// through exactly this call); a by-value signature would put a move of the largest enum
    /// variant on that path. With `&mut`, an unstamped payload costs one discriminant test.
    #[inline]
    fn stamp(&self, event: &mut Event) {
        // ONE `Option` test guards all three arms (every payload on a single-account box has
        // `route_key: None`).
        let Some(key) = self.route_key else { return };
        // A default account's key IS its venue — nothing to say (see [`Self::routed`]).
        // `is_none`: the stamp answers "nobody said", never overrides a producer that named an
        // account, and is idempotent under a re-send. The match SELECTS the slot and the rule is
        // applied once below, not copied per arm.
        let slot: Option<(Ustr, &mut Option<Ustr>)> = match event {
            Event::AccountState(a) => Some((a.venue, &mut a.route_key)),
            Event::Funding(f) => Some((f.venue, &mut f.route_key)),
            Event::PositionLiquidated(p) => Some((p.venue, &mut p.route_key)),
            Event::Fill(_)
            | Event::OrderSubmitted(_)
            | Event::OrderAccepted(_)
            | Event::OrderRejected(_)
            | Event::OrderDenied(_)
            | Event::OrderTriggered(_)
            | Event::OrderPartiallyFilled(_)
            | Event::OrderFilled(_)
            | Event::OrderCanceled(_)
            | Event::OrderExpired(_)
            | Event::OrderLiquidated(_)
            | Event::OrderModified(_)
            | Event::PositionOpened(_)
            | Event::PositionChanged(_)
            | Event::PositionClosed(_)
            | Event::OrderCancelRejected(_)
            | Event::OrderModifyRejected(_) => None,
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

    /// Blocking lossless send of an event that arrived over a gateway's trade link, with its ring
    /// position ([`Ingest::GatewayEvent`]): the twin of [`Self::blocking_send`] for the remote
    /// execution client's reader (and the forwarder that relays it to the core). The same
    /// [`Self::route_key`] stamp applies to the payload, so a labelled account's account-wide
    /// event reaches its own engine whichever door it came through; `gw.route_key` is the ring's
    /// account and is NOT touched here (see [`GatewayEvent::route_key`]).
    pub fn blocking_send_gateway(&self, mut gw: GatewayEvent) -> Result<(), CoreGone> {
        self.stamp(&mut gw.event);
        self.ingest.blocking_send(Ingest::GatewayEvent(Box::new(gw))).map_err(|_| CoreGone)
    }

    /// Blocking lossless disclosure that an account's trade link went down or came back
    /// ([`Ingest::ExecLink`]). Sent ONLY on a transition, so it costs nothing steady-state. The
    /// payload names its account itself ([`ExecLinkUpdate::route_key`]); this handle's own
    /// [`Self::route_key`] is not consulted.
    pub fn exec_link(&self, update: ExecLinkUpdate) -> Result<(), CoreGone> {
        self.ingest.blocking_send(Ingest::ExecLink(Box::new(update))).map_err(|_| CoreGone)
    }

    /// Blocking lossless hand-over of a started account's ENGINE to the running core
    /// ([`Ingest::Accounts`], decision 0169). `Ok` means the message is QUEUED: every message sent
    /// on this lane after it folds after it (one FIFO), which is the order a caller relies on to
    /// release the new account's own event lane only after this returns. Cold path, once per account.
    pub fn add_account_engine<C: ExecutionClient + Send + 'static>(
        &self,
        seed: f64,
        engine: ExecutionEngine<C>,
    ) -> Result<(), CoreGone> {
        self.ingest
            .blocking_send(Ingest::Accounts(Box::new(AccountChange::add_engine(seed, engine))))
            .map_err(|_| CoreGone)
    }

    /// Blocking lossless request that the running core STOP an account, phase one
    /// ([`AccountChange::StopEngine`], decision 0169, slice S2). `Ok` means the message is QUEUED behind
    /// everything already sent on this lane. Cold path, once per stop.
    pub fn stop_account_engine(
        &self,
        not_running: crate::read_model::NotRunningAccount,
    ) -> Result<(), CoreGone> {
        self.ingest
            .blocking_send(Ingest::Accounts(Box::new(AccountChange::StopEngine { not_running })))
            .map_err(|_| CoreGone)
    }

    /// Blocking lossless [`Command::HoldAccount`] on this lane: hold ONE account's engine `Reducing`, or
    /// release it. `Ok` means the message is QUEUED behind everything already sent on this lane, journaled
    /// like any command. The account watcher's door (a lane twin kept up only to be closed, so a mount on
    /// it cannot open exposure). Cold path, once per edge.
    pub fn hold_account(&self, route_key: &str, hold: bool) -> Result<(), CoreGone> {
        self.ingest
            .blocking_send(Ingest::Command(Command::HoldAccount {
                route_key: route_key.to_string(),
                hold,
            }))
            .map_err(|_| CoreGone)
    }

    /// Blocking lossless request that the running core begin the detach of a stopped account's client, phase
    /// two of the stop ([`AccountChange::DetachEngine`]). Cold path, once per stop.
    pub fn detach_account_engine(&self, route_key: &str) -> Result<(), CoreGone> {
        self.ingest
            .blocking_send(Ingest::Accounts(Box::new(AccountChange::DetachEngine {
                route_key: route_key.to_string(),
            })))
            .map_err(|_| CoreGone)
    }
}

/// A raw `EventSender` + ingest receiver with NO core thread behind it (the core-thread twin is
/// `CoreHandle::event_sender`).
///
/// Two users. Venue-adapter integration tests use it to observe exactly what a client pushes into
/// the ingest lane. And a process that HOSTS venue adapters but runs no core — the gateway daemon,
/// whose bridges emit into the receiver this returns and whose trade sessions relay each `Ingest`
/// over the wire to the one process that does run a core — builds its senders here: that is a
/// production use of the function, not only a test fixture.
pub fn event_channel(capacity: usize) -> (EventSender, mpsc::Receiver<Ingest>) {
    let (tx, rx) = mpsc::channel(capacity);
    (EventSender { ingest: tx, route_key: None }, rx)
}

/// A `(BarSender, MarketSender)` pair + the ingest receiver behind them, with NO core thread — the
/// bar/market-lane twin of [`event_channel`]. A live market-data feed impl (or its test) can push
/// closed bars / forming updates / ticks and observe exactly what lands on the ingest lane. Both
/// senders share one ingest channel, as `spawn_core` hands the core's single ingest to
/// `bar_sender()`/`market_sender()`. Today's venue feeds implement `vike_data::DataClient` against
/// the sink seam and call this nowhere; the gateway daemon's credentialed data sessions (which host
/// feeds but no core) are the production caller it is kept for, with [`event_channel`] as its
/// twin.
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

mod book_serde;

#[cfg(test)]
mod event_channel_tests;

#[cfg(test)]
mod serde_tests;

#[cfg(test)]
mod order_intent_tests;

#[cfg(test)]
mod conflation_tests;
