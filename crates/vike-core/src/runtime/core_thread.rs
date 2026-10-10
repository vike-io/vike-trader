//! The single-writer `CoreThread` state, and the held reconcile-alert record it keeps.

use super::*;

/// What [`CoreThread::cond_engine`] remembers about one armed conditional.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ArmedBy {
    /// The routing index of the engine the arm will fire onto.
    pub(crate) engine: usize,
    /// The mount whose strategy armed it; `None` for an operator's or any non-mount arm.
    pub(crate) mount: Option<usize>,
}

/// What [`CoreThread::pending_owners`] holds for one mount id: the coids it owned and its ledger.
#[derive(Debug, Default)]
pub(crate) struct PendingOwner {
    pub(crate) coids: Vec<String>,
    pub(crate) ledger: Option<MountAttribution>,
}

pub(crate) struct CoreThread<C: ExecutionClient> {
    pub(crate) engine: ExecutionEngine<C>,
    pub(crate) bus: EventBus,
    pub(crate) config: CoreConfig,
    pub(crate) market: Arc<Conflated>,
    pub(crate) snapshot: Arc<ArcSwap<CoreSnapshot>>,
    pub(crate) rejected: Arc<AtomicU64>,
    /// The bounded recent-events ring, held RENDERED: each line is formatted ON the per-message
    /// fold at push (`crates/vike-core/src/runtime/journaling.rs`'s `note_event`) and stored as an
    /// `Arc<str>`, so `snapshot::build` clones one refcount per entry and formats nothing. Perf
    /// audit finding #2 (#887) had held it un-rendered for a render at publish; #896 reversed that,
    /// because publish also runs on every idle transition — per event on a sporadic feed (see
    /// [`crate::recent`]).
    pub(crate) recent: VecDeque<std::sync::Arc<str>>,
    pub(crate) fault: Option<String>,
    pub(crate) seq: u64,
    pub(crate) dirty: bool,
    /// the core-owned bar cache (plan §1 "Cache write BEFORE dispatch"); snapshots share
    /// the closed series by Arc — R7 strategies read this same cache deterministically
    pub(crate) bars: indexmap::IndexMap<SeriesKey, BarSeries>,
    /// client-order-id generator for the mounted strategy's orders
    pub(crate) coid_gen: vike_model::orders::client_order_id::ClientOrderIdGenerator,
    /// HFT modify surface: strategy-chosen tag → the current coid it minted for, so a strategy can
    /// `modify(tag, ..)` a resting order it never saw the coid of. A re-submit under a tag overwrites;
    /// stale entries (terminal orders) are harmless (modify_order gates on status).
    ///
    /// KEYED `{mount_idx}|{venue}|{symbol}|{tag}` (multi-mount correctness, bug A). The mount index
    /// leads because tags are strategy-LOCAL names, not global ones: `vike-mm`'s maker emits the
    /// literal tags `"bid"`/`"ask"`, so two makers on ONE (venue, symbol) both wrote
    /// `binance|BTCUSDT|bid` — mount B's submit OVERWROTE mount A's coid, and A's next
    /// `cancel_tagged("bid")`/`modify_tagged("bid")` then operated on B's RESTING ORDER (a real
    /// cross-mount money bug, not a display one). The index is unique per slot by construction and
    /// cheaper than the string mount id. Single-mount runs are unaffected in behavior (every key
    /// simply gains the constant `0|` prefix; the map is runtime-only and never persisted or
    /// published, so nothing on the wire or in the journal changes).
    pub(crate) strategy_tags: indexmap::IndexMap<String, String>,
    /// Phase D: every strategy mount (config.strategy + extra_mounts); Option per slot
    /// for the take/replace dance around strategy calls
    pub(crate) mounts: Vec<Option<StrategyMount>>,
    /// Portfolio-observer PR-4 T2: each mount's deterministic [`crate::strategy_state::mount_id_of`],
    /// same length and index alignment as `mounts`, computed once in `assemble_core`. Read by the
    /// `run()` teardown's save-on-stop (gated on [`CoreConfig::state_dir`]) so it never recomputes
    /// the id from a possibly-taken (`None`) mount slot.
    pub(crate) mount_ids: Vec<String>,
    /// Portfolio-observer PR-4 T5: each mount's [`MountState`], same length and index alignment
    /// as `mounts` (computed once in `assemble_core`, never resized after). All `Ready` forever
    /// when [`CoreConfig::readiness_gate`] is off (the default); seeded `Pending` when it is on,
    /// flipped to `Ready` one-way by [`Self::maintain_mount_readiness`]. Read by
    /// [`Self::drain_broker`] (the discard gate) and [`Self::mount_views`] (the snapshot view).
    pub(crate) mount_states: Vec<MountState>,
    /// Phase C/D: emulated conditional orders per (venue, symbol) — checked per closed
    /// bar BEFORE `on_bar` (oracle firing order), optionally per tick.
    pub(crate) conditional_books: indexmap::IndexMap<(String, String), ConditionalBook>,
    /// Live-runtime OTO/OCO: the ONE shared [`vike_exec::ContingencyBook`] resolver, keyed by coid
    /// (globally unique per core). Records a leg's linkage the instant a linked/bracket order is
    /// submitted; on a leg's FILL it arms held OTO children (released via `held_orders`) and returns
    /// the OCO siblings to cancel. EMPTY for every non-bracket run — plain orders are never inserted,
    /// so the whole feature is a single `is_empty()` check away on the byte-identical path.
    pub(crate) contingency: ContingencyBook,
    /// The resolved requests of OTO children the runtime is HOLDING back from the venue until their
    /// parent fills (live-runtime OCO/OTO), keyed by coid. A held exit lives ONLY here — it is NOT
    /// in any engine registry and NOT at the venue — until its parent's fill arms it, at which point
    /// it is drained here and submitted (`release_held_child`). Also the durable half a `Snap`
    /// captures (`SnapContingency.held_request`). `IndexMap`: release/drop order is the map's own
    /// insertion order, deterministic. Empty on every non-bracket run.
    pub(crate) held_orders: indexmap::IndexMap<String, OrderRequest>,
    /// Monotone counter behind [`Self::mint_arm_id`] — the emulated conditionals' id sequence
    /// (emulator-journal PR-1). Deliberately SEPARATE from the `ClientOrderIdGenerator`: an arm
    /// is not an order, and drawing arm ids from the coid generator would shift every subsequent
    /// order's coid the moment a conditional is armed.
    ///
    /// Seeded from [`CoreConfig::arm_seq`] — `0` for a fresh session, and on a restart that
    /// resumes `coid_session` the value [`crate::replay::RestoredState::arm_seq`] carries, so the
    /// `{coid_session}a{arm_seq}` ids minted after a restart cannot collide with the ones the
    /// pre-restart run already wrote into the SAME (appended-to) journal. Stamped into every
    /// `Snap` next to `coid_seq` (emulator PR-2) so that resume value survives segment pruning.
    /// Since emulator PR-3 the arms themselves survive a restart too: the books ride the same
    /// `Snap` (`Snap.conditionals`) and re-seed via [`CoreConfig::conditionals`], so the resumed
    /// counter and the re-armed ids come from the ONE restore base together.
    pub(crate) arm_seq: u64,
    /// Monotone counter behind [`CoreThread::mint_refusal_id`] — the id sequence for an intent the
    /// drain REFUSED before it could become an order (today: a declared-multi mount naming an
    /// UNDECLARED symbol, see `CoreThread::resolve_intent_symbol`).
    ///
    /// SEPARATE from the [`vike_model::orders::client_order_id::ClientOrderIdGenerator`] for the
    /// same reason [`Self::arm_seq`] is, only more sharply: a refusal is NOT an order — it
    /// journals nothing and submits nothing — so drawing its id from the coid generator would burn
    /// a client-order-id that never appears in the journal. Every later order's coid would then sit
    /// one step ahead of the `coid_seq` the `Snap` records, and a restart that resumes from that
    /// journal would re-mint an id the live session had already spent: a misconfigured session
    /// would become un-restorable.
    ///
    /// Not stamped into a `Snap` and not resumed (unlike `arm_seq`): a refusal id is never a lookup
    /// key for anything durable — no journal record carries it, no disarm targets it — so a restart
    /// restarting the sequence at 0 can collide with nothing. It is purely a handle for the
    /// operator/strategy-visible denial the refusal emits.
    pub(crate) refusal_seq: u64,
    /// Cross-venue: additional (seed_cash, engine) pairs, one per extra venue. Empty
    /// (spawn_core default) = single-venue behavior, byte-identical. Index 0 in routing
    /// terms is the primary `engine`; extra i is routing index i+1.
    pub(crate) extra_engines: Vec<(f64, ExecutionEngine<C>)>,
    /// **Do two engines in this process share one exchange?** — i.e. does this process mount more
    /// than one ACCOUNT of some venue. Computed once at construction (`assemble_core`) from the
    /// engines' own `venue` fields; `false` on every process with one account per venue, which is
    /// every process with no `policy.accounts.*` rows.
    ///
    /// It exists for exactly one branch, in [`Self::route_event`], and the whole reason it is a
    /// precomputed `bool` rather than a scan is that the branch sits on the fold: a `false` costs
    /// one predictable compare per venue-tagged event and reaches the identical `return` the
    /// routing has always taken, so the single-account hot path is byte-identical.
    pub(crate) multi_account: bool,
    /// coid → routing index, written at submit time — order-LIFECYCLE events carry no
    /// venue field (wire schema), so replies route back through this map (miss = primary,
    /// today's behavior).
    pub(crate) coid_venue: std::collections::HashMap<String, usize>,
    /// **arm id → who armed it, written when the conditional is ARMED** — [`Self::coid_venue`]'s
    /// twin for the one order the core mints with no request behind it. The value is an
    /// [`ArmedBy`]: the routing index (`engine`) AND the mount that armed it (`mount`), so one map
    /// carries both and has one bound.
    ///
    /// `conditional_books` is keyed by `(venue, symbol)`, which is a fact about the EXCHANGE, so
    /// two accounts of one venue arm into ONE book and a fired arm carries no account. The FIRE
    /// (`CoreThread::submit_fired`) used to lower through `apply_intent`, i.e. by the payload's
    /// venue, which resolves the venue's DEFAULT engine: a labelled mount's `Broker::submit_stop`
    /// armed against its own book and, on trigger, sold into somebody else's — the protective exit
    /// silently opening a naked position on the default account while the position it was armed to
    /// close stayed open. The arm's engine is known at ARM time (`apply/conditional.rs`'s
    /// `arm_conditional` already resolves it to read the seeding mark), so it is recorded here
    /// and spent at the fire.
    ///
    /// Bounded by the book: an entry is written when an arm enters a `ConditionalBook` and removed
    /// when it leaves one (fired, disarmed, or cleared by a mass-cancel).
    ///
    /// Byte-identical on every single-account core, and NOT because it is skipped there: the
    /// recorded index is `route_of(route, venue)`, which on a core with one engine per venue IS
    /// what the fire's payload lookup resolved.
    ///
    /// The MOUNT is what keeps the fired order a strategy's own: `submit_fired` writes it into
    /// [`Self::coid_mount`] beside the minted coid, so the stop's fill books into the arming mount's
    /// ledger and reaches that mount's `on_fill`, rather than the first mount on the pair. `None`
    /// for an operator's arm.
    pub(crate) cond_engine: std::collections::HashMap<String, ArmedBy>,
    /// Opt-in write-ahead journal (spec §A). `None` = journaling off (default) — the whole
    /// hot-path hook is skipped and the fold is byte-identical to today. Opened once at
    /// construction from [`CoreConfig::journal`].
    pub(crate) journal: Option<vike_journal::CommandJournal>,
    /// Records appended since the last `Snap` — drives the [`JournalConfig::snapshot_every`]
    /// cadence. Reset to 0 by [`CoreThread::write_snap`].
    pub(crate) journaled_since_snap: u64,
    /// Whether an `Ingest::Watchdog` waker record is written to the journal — `true` iff a
    /// wall-clock sweep that MUTATES state is enabled (stuck-order watchdog / dead-man's switch /
    /// GTD expiry sweep). See the construction site in `assemble_core` for the full rationale; the
    /// short version is that the record exists to make such a session refuse to replay, and a
    /// core running only replay-neutral boundary observers must not inherit that refusal.
    pub(crate) journal_waker_records: bool,
    /// Opt-in mmap counter mirror (audit co9). `None` (default) = disabled — [`Self::publish`]
    /// skips the mirror and the publish path is byte-identical. `Some` is opened once at
    /// construction from [`CoreConfig::counters_path`]; each publish copies the current counters in.
    /// Written ONLY at publish, never from `handle()` — but publish also runs on every idle
    /// transition, so with `Some` on a sporadic feed the copy is per event and lands inside the
    /// gated core hop (the gated `runtime_latency` harnesses run with `None`).
    pub(crate) counters: Option<crate::counters::CountersFile>,
    /// Stuck-order watchdog (audit C3) confirm-grace bookkeeping, hardened into the in-flight-confirm
    /// guard: `coid → the now_ms at which stage 1 ISSUED its active confirm for that order`. Two jobs:
    /// (a) stage 1 warns + confirms each coid exactly ONCE per stuck episode (present ⇒ already
    /// confirmed, skip); (b) stage 2's guard — a reject candidate WITH an entry has a confirm in
    /// flight, so the last-resort reject is DEFERRED to `created + submit_ack_timeout +
    /// 2·submit_ack_confirm_grace` (one extra grace window) to let a slow confirm (Bybit's
    /// realtime→history re-query can exceed a single grace) land before terminalizing. Whenever a coid
    /// is here the order is STILL pre-ack (any authoritative fold moves it out), so an entry means
    /// "confirm issued, nothing authoritative folded since". Core-local, NOT serialized (the watchdog
    /// path is out of replay scope); self-cleaning — pruned each sweep to only still-pre-ack coids, so
    /// it stays bounded. Empty unless the watchdog is enabled.
    pub(crate) confirm_issued_ms: std::collections::HashMap<String, i64>,
    /// Drawdown latch (audit exec#4) high-water-mark, on the DAEMON'S OWN equity curve
    /// (`Σ seed_of(e) + Σ ExecutionEngine::resolved_own_pnl(e)` — never an account balance level;
    /// see [`CoreThread::sweep_drawdown_latch`] for why the wallet had to leave this number).
    /// Folded on the per-CLOSED-bar sweep, only when [`CoreConfig::max_drawdown`] is enabled.
    /// Core-local, NOT serialized (never journaled/snapshotted); `None` until the first sweep (or
    /// forever, when the latch is disabled). RESTART: the LATCH itself
    /// (`trading_state = Reducing`) IS restored via the engine snapshot, and the CURVE resumes
    /// underwater because `Account`'s PnL terms are in `AccountSnapshot` — so this HWM re-seeds to
    /// `max(capital_base, curve)`, which forgives no pre-restart loss below configured capital.
    pub(crate) pnl_curve_peak: Option<f64>,
    /// One-shot guard for the drawdown latch's DISARMED warning — see
    /// [`CoreThread::sweep_drawdown_latch`]. The condition (a non-positive capital base under an
    /// armed `max_drawdown`) holds on every subsequent sweep, so without this the per-bar ring
    /// would carry nothing else. Core-local, NOT serialized, like the HWM above.
    pub(crate) pnl_curve_disarmed_noted: bool,
    /// Deadline timer wheel (audit co6), advanced ONCE per drain-loop boundary — NEVER per message,
    /// so the p99<10µs fold is untouched. Empty by default (a single `is_empty` gate keeps the
    /// wheel-free path byte-identical); armed with a self-rescheduling [`TimerKind::StuckSweep`] only
    /// when the watchdog (`submit_ack_timeout`) is enabled. See [`CoreThread::drive_due_timers`].
    pub(crate) timers: DeadlineTimerWheel<TimerKind>,
    /// Reusable expiry buffer for [`Self::drive_due_timers`] — cleared and refilled each boundary so
    /// the timer path allocates nothing on the steady state.
    pub(crate) due_timers: Vec<TimerKind>,
    /// The stuck-order sweep cadence (ms) the [`TimerKind::StuckSweep`] timer re-arms itself at —
    /// `submit_ack_timeout / 2` clamped to `>= 50ms`, mirroring the OS-thread waker's tick. 0 when
    /// the watchdog is disabled.
    pub(crate) watchdog_tick_ms: i64,
    /// The armed [`TimerKind::EquitySample`] timer (portfolio-observer PR-3), `None` while flat
    /// (or whenever [`CoreConfig::equity_sample`] is disabled — never armed in that case at
    /// all). Set by [`CoreThread::maintain_equity_timer`]'s arm branch and the re-arm in
    /// [`CoreThread::drive_due_timers`]; cleared by its disarm branch and by a fire that finds
    /// the book gone flat.
    pub(crate) equity_timer: Option<TimerId>,
    /// Reusable batch buffer for [`Self::sample_equity`] — cleared and refilled each fire (one
    /// row per engine plus the `"TOTAL"` row) so steady-state sampling allocates no `Vec` of its
    /// own, mirroring [`Self::due_timers`].
    pub(crate) equity_rows: Vec<EquitySample>,
    /// The armed [`TimerKind::StateSave`] timer (portfolio-observer PR-4 T3), `None` whenever
    /// [`CoreConfig::state_save`] or [`CoreConfig::state_dir`] is disabled, or (transiently)
    /// while no mount exists — never armed at all in the first two cases. Set by
    /// [`CoreThread::maintain_state_save_timer`]'s arm branch and the re-arm in
    /// [`CoreThread::drive_due_timers`]; cleared by its disarm branch and by a fire that finds
    /// every mount gone.
    pub(crate) state_save_timer: Option<TimerId>,
    /// Task 17: held Quarantine/Hybrid-quarantined reconcile alerts awaiting an operator
    /// `Command::ConfirmRecon`, keyed by the monotonic id `reconcile_reports` assigns when the
    /// alert is first surfaced. Fold-thread-only state (single-writer invariant — never mutated
    /// off this thread); the GUI-facing projection is `CoreSnapshot.recon.alerts`
    /// (`ReconAlertView`, built by `Self::recon_block`), which carries kind/detail/count but
    /// never the raw `Event`s. NOT journaled: ids reset with the process (see
    /// `vike_exec::Command::ConfirmRecon`'s doc for why a replayed confirm against a stale id is
    /// a harmless no-op, not a correctness gap).
    pub(crate) recon_alerts: indexmap::IndexMap<u64, HeldReconAlert>,
    /// Which held divergences have already been ANNOUNCED to the log, per venue — the state that
    /// turns a per-pass repetition into a per-transition line. Purely a logging concern: it never
    /// decides what folds, what holds or what `recon_alerts` stores, and it is touched ONLY from
    /// `reconcile_reports` (interval cadence) and `confirm_recon` (an operator command), never the
    /// per-message fold. Empty and allocation-free on a box with no held divergences. See
    /// `crate::runtime::recon_held`.
    pub(crate) recon_announce: recon_held::HeldAnnouncer,
    /// Next id `reconcile_reports` assigns to a newly-held alert. Monotonic within this process
    /// run only (see `recon_alerts` doc).
    pub(crate) recon_next_alert_id: u64,
    /// wall-clock ms of the most recently completed reconcile pass (`engine.now_ms` at the time
    /// `reconcile_reports` ran); 0 before the first pass. Surfaced verbatim as
    /// `CoreSnapshot.recon.last_pass_ts`.
    pub(crate) recon_last_pass_ts: i64,
    /// Latest venue-REPORTED per-position coin delta from each reconcile pass, keyed `(venue,
    /// symbol)` (Wave 5d). Upserted from every `PositionStatusReport` that carries a `delta`
    /// (Deribit `get_positions.delta` today); empty and inert for every other venue. Cloned into
    /// `CoreSnapshot::recon_coin_deltas` at every publish so the greeks tool can fold a
    /// Deribit perp/future leg via `coin_delta × spot` — `PositionView` is fills-derived and has
    /// no venue delta of its own.
    pub(crate) recon_coin_deltas: indexmap::IndexMap<(String, String), f64>,
    /// Dead-man's-switch (auto cancel-on-disconnect) pure trip-logic state machine — `Some` ONLY
    /// when [`CoreConfig::deadman`] is set, else `None` (default) and the whole feature is inert.
    /// [`Self::dispatch`] feeds it the freshest data/event ts ([`DeadMan::observe`], the ONE cheap
    /// per-message store, gated on `is_some`); [`Self::sweep_deadman`] evaluates it at the drain-loop
    /// boundary on the [`TimerKind::DeadManSweep`] cadence — NEVER the per-message fold.
    pub(crate) deadman: Option<DeadMan>,
    /// The dead-man sweep cadence (ms) the [`TimerKind::DeadManSweep`] timer re-arms itself at —
    /// `deadman.timeout / 2` clamped `>= 50ms`, mirroring [`Self::watchdog_tick_ms`]. 0 when the
    /// switch is disabled (never armed).
    pub(crate) deadman_tick_ms: i64,
    /// CONNECTION-state dead-man latch (M13) — `Some` ONLY when [`CoreConfig::link_deadman`] is
    /// set, else `None` (default) and the whole feature is inert. [`Self::dispatch`] feeds it the
    /// `Ingest::StreamStatus` transitions ([`LinkDeadMan::observe`] — an OCCASIONAL control event,
    /// never the per-message fold); [`Self::sweep_link_deadman`] evaluates it at the drain-loop
    /// boundary on the [`TimerKind::LinkDeadManSweep`] cadence.
    pub(crate) link_deadman: Option<LinkDeadMan>,
    /// The link-dead-man sweep cadence (ms) the [`TimerKind::LinkDeadManSweep`] timer re-arms
    /// itself at — `link_deadman.grace / 2` clamped `>= 50ms`, the [`Self::deadman_tick_ms`]
    /// shape. 0 when the switch is disabled (never armed).
    pub(crate) link_deadman_tick_ms: i64,
    /// The MANAGED-GTD sweep cadence (ms) the [`TimerKind::GtdSweep`] timer re-arms itself at —
    /// [`CoreConfig::gtd_sweep`], clamped `>= 1ms`. 0 when the feature is disabled (never armed).
    pub(crate) gtd_tick_ms: i64,
    /// Coids whose GTD expiry already triggered a cancel this episode (core-ergonomics) — the
    /// FIRE-ONCE guard for [`Self::sweep_gtd_expiry`], so a venue slow to report the terminal is
    /// not spammed with duplicate cancels. Core-local, NOT serialized (like `confirm_issued_ms`);
    /// self-cleaning — pruned each sweep to coids still live in some registry, so it stays bounded.
    /// Empty forever unless [`CoreConfig::gtd_sweep`] is enabled.
    pub(crate) gtd_canceled: std::collections::HashSet<String>,
    /// The periodic-portfolio-snapshot cadence (ms) the [`TimerKind::PortfolioSnap`] timer re-arms
    /// itself at — [`CoreConfig::portfolio_snapshot_interval`], clamped `>= 1ms`. 0 when disabled.
    pub(crate) portfolio_snap_tick_ms: i64,
    /// The FAST in-flight confirm cadence (ms) the [`TimerKind::InflightConfirm`] timer re-arms
    /// itself at — [`CoreConfig::inflight_confirm`], clamped `>= 1ms`. 0 when the feature is
    /// disabled (never armed). See [`Self::sweep_inflight_confirms`].
    pub(crate) inflight_confirm_tick_ms: i64,
    /// FAST in-flight confirm dedup (recon path-to-superset, F1-A): `coid -> the now_ms at which
    /// this rung last issued a reject-free `confirm_order` re-query for that order`. Deliberately
    /// SEPARATE from the reject ladder's `confirm_issued_ms` so the early (reject-free) rung and the
    /// late (reject-capable) rung never share bookkeeping — the band guard keeps them on disjoint
    /// age windows, and each owns its own map. A still-stuck order is re-confirmed at most once per
    /// [`CoreConfig::inflight_confirm`] window (present + gap-not-elapsed ⇒ skip). Core-local, NOT
    /// serialized (like `confirm_issued_ms`); self-cleaning — pruned each sweep to only still-pre-ack
    /// coids, so it stays bounded. Empty forever unless the feature is enabled.
    pub(crate) inflight_confirm_last_ms: std::collections::HashMap<String, i64>,
    /// Per-mount fill ATTRIBUTION ledger (steal/core-per-mount-budget), parallel to `mounts` (same
    /// index), folded from attributed fills off the hot fold — see [`MountAttribution`]. All-zero
    /// for a mount that has never filled.
    pub(crate) mount_attr: Vec<MountAttribution>,
    /// coid -> mount index, written through [`Self::own_coid`] for every order a mount's strategy
    /// mints (a submit, a fired stop, a budget-latch flatten) so a mount's fills AND resting orders
    /// are attributable back to it — cross-venue safe (coids are globally unique per core). Read by
    /// the fill-attribution fold ([`Self::dispatch_applied_fills`]) and the budget latch's scoped
    /// cancel ([`Self::latch_mount`]). Not fenced state and not journaled; it survives a restart
    /// through [`Self::order_owners`], keyed by mount ID. Empty when no strategy is mounted;
    /// BOUNDED by [`Self::coid_terminal`], which retires an entry once its order has been terminal
    /// for [`COID_PRUNE_LINGER_MS`].
    pub(crate) coid_mount: std::collections::HashMap<String, usize>,
    /// The PRUNE QUEUE that bounds [`Self::coid_mount`]: `(coid, the ms at which its order was first
    /// observed TERMINAL)` in observation order, drained from the FRONT by
    /// [`Self::prune_terminal_coids`] once an entry has aged past [`COID_PRUNE_LINGER_MS`].
    ///
    /// `coid_mount` is written once per mount-minted order and used to be erased NEVER, so a
    /// long-lived maker session grew it without bound — and a restart made it WORSE, because
    /// [`crate::replay`]'s `fold_coid_mounts` rebuilds order provenance across the WHOLE readable
    /// record set rather than just the tail. It is a pure routing/attribution index, so a long-dead
    /// order's entry is dead weight.
    ///
    /// **Why a LINGER queue and not an immediate remove** — see [`Self::note_terminal_coid`].
    /// Core-local, never serialized, and empty on a core whose orders never terminalize.
    pub(crate) coid_terminal: VecDeque<(String, i64)>,
    /// The ORDER-OWNERSHIP file ([`CoreConfig::order_owners`], taken at assemble and started there).
    /// Written through [`Self::record_owner`] only: an `own` per order a mount mints
    /// ([`Self::own_coid`]), a `forget` per entry [`Self::prune_terminal_coids`] retires, a `ledger`
    /// per attributed fill of a live mount, an `unmount` per runtime unmount. `None` = nothing is
    /// recorded, byte-identical to a core without the file.
    pub(crate) order_owners: Option<crate::order_owners::OrderOwnerLog>,
    /// Restored ownership waiting for its MOUNT: entries the file named for a mount id no live slot
    /// carried at assemble (a profile mount removed, or a runtime mount not yet resurrected), keyed
    /// by that id. [`Self::mount_strategy_runtime`] binds an entry to the new slot when a mount of
    /// that id lands. Never written after assemble, so it only shrinks; the file keeps the entries
    /// (until their time-to-live) whether or not a mount ever claims them.
    pub(crate) pending_owners: std::collections::HashMap<String, PendingOwner>,
    /// Per-mount resolved [`MountBudget`] (steal/core-per-mount-budget), parallel to `mounts`, copied
    /// from [`CoreConfig::mount_budgets`] at assemble. `None` for a mount with no budget.
    pub(crate) mount_budget: Vec<Option<MountBudget>>,
    /// Per-mount `(venue, symbol)` (steal/core-per-mount-budget), parallel to `mounts`, captured at
    /// assemble so the budget sweep + the snapshot view price a mount's ledger without depending on a
    /// transiently-taken (`None`) mount slot.
    pub(crate) mount_vs: Vec<(String, String)>,
    /// **Per-mount ENGINE INDEX**, parallel to `mounts` — the mount's declared account resolved to
    /// one of this core's engines, ONCE, at assemble ([`mount_engine_idx`]).
    ///
    /// It is the ONE answer to "which book does this mount trade", and every strategy lane reads it
    /// through [`Self::engine_of_mount_venue`] rather than re-deriving one from a venue string.
    /// That is the structural half of the account seam: a venue-keyed lookup can only ever resolve
    /// a venue's DEFAULT account, so as long as the lanes asked that question a second account was
    /// unaddressable no matter what a mount declared, and no runtime test could have said so.
    ///
    /// Identical to the old `engine_idx_for_route_key(sole_account_of(mount.venue)).unwrap_or(0)`
    /// for every account-less mount, which is every mount that existed before the field.
    pub(crate) mount_engine: Vec<usize>,
    /// Per-mount budget LATCH (steal/core-per-mount-budget), parallel to `mounts`: `true` once that
    /// mount breached its budget and was latched liquidate-only. Fire-once — a re-breach on a
    /// still-open residual never re-cancels/re-flattens; read by [`Self::drain_broker`] (discards a
    /// latched mount's intents, the liquidate-only enforcement) and [`Self::sweep_mount_budgets`].
    /// Core-local, NOT serialized (like `equity_peak`). All `false` when no budget is set — the
    /// byte-identical path.
    pub(crate) mount_latched: Vec<bool>,
    /// Per-mount DECLARED extra symbols (`StrategyMount::symbols`), parallel to `mounts`. Empty
    /// for every single-symbol mount, which is what makes the drain byte-identical.
    pub(crate) mount_symbols: Vec<Vec<MountLeg>>,
    /// Whether ANY mount declared extra symbols. `false` (default) ⇒ the drain's symbol
    /// resolution is one bool read and the per-symbol read tables are never built.
    pub(crate) any_mount_multi: bool,
    /// Whether ANY mount declared a leg on a venue OTHER than its own (the xEMM shape). `false`
    /// (default, and true of every same-venue declared mount too) ⇒ the two per-market-message
    /// reference-quote call sites in the `Ingest::Quote`/`Ingest::Book` arms are ONE bool load and
    /// [`Self::drive_strategy_reference_quote`] never runs — the byte-identical gate, mirroring
    /// `any_mount_multi`/`any_mount_budget`.
    pub(crate) any_mount_ref: bool,
    /// Whether ANY mount has an ACTIVE budget (steal/core-per-mount-budget). `false` (default) makes
    /// [`Self::drive_strategy`] skip the per-closed-bar budget sweep with a single bool read — the
    /// byte-identical gate, mirroring `config.max_drawdown`'s `Option` gate for the drawdown latch.
    pub(crate) any_mount_budget: bool,
    /// Per-mount wall-clock [`crate::schedule::LiveSchedule`] (steal/core-live-scheduler), parallel
    /// to `mounts` (same indices), resolved from [`CoreConfig::mount_schedules`] at assemble. Empty
    /// for a mount with no schedule. Checked at EVERY drain-loop boundary pass
    /// ([`Self::drive_schedule`], the readiness-gate pattern — no timer-wheel entry), NEVER per
    /// message.
    pub(crate) mount_schedule: Vec<LiveSchedule>,
    /// Whether ANY mount has a non-empty schedule (steal/core-live-scheduler). `false` (default) ⇒
    /// the boundary check is a single bool read, no clock read happens, no waker cadence is added,
    /// and the boundary is byte-identical — the gate mirrors `any_mount_budget`.
    pub(crate) any_mount_schedule: bool,
    /// **The tick lane's subscription table**: venue → symbol → the LIVE mount slots that hear a
    /// tick (quote / trade / book) about that pair, in mount order — exactly the slots
    /// [`Self::mount_hears`] answers `true` for with `Audience::Tick`, except a slot a strategy-hook
    /// PANIC left `None` (no rebuild runs then): the tick lane's `is_some()` re-check covers it.
    ///
    /// Built ONLY by [`Self::rebuild_tick_audience`] (at assemble, and after every runtime mount
    /// and unmount) and read ONLY through [`Self::tick_audience_of`] by
    /// [`Self::drive_strategy_tick`]; never edited elsewhere. A write of `mounts` or
    /// `mount_symbols` that does not end in that rebuild leaves the table disagreeing with the
    /// rule, and a mount silently deaf to its ticks. The module doc of
    /// `crates/vike-core/src/runtime/strategy_drive/subscriptions.rs` says where the rebuild runs.
    pub(crate) tick_audience:
        std::collections::HashMap<String, std::collections::HashMap<String, Arc<[usize]>>>,
    /// The mount-row list the last publish handed the snapshot, kept so the next publish can hand
    /// the SAME list over when no row's numbers moved ([`Self::published_mount_rows`]). Written ONLY
    /// there; read only there and by the tests.
    pub(crate) mount_rows: MountRowCache,
    /// Bumped by [`Self::recompute_mount_gates`] — the one place a runtime mount or unmount ends —
    /// so the cache above can tell a CHANGED LIST (rows added, removed, renamed) from changed
    /// numbers. A cache built under another epoch is never reused. Starts at `0` beside an EMPTY
    /// cache: a core assembled with mounts has live slots that list cannot match, so its first
    /// publish builds the real one.
    pub(crate) mount_epoch: u64,
}

/// Fold-thread-only record of one held reconcile alert (Task 17): the full proposed events
/// (folded verbatim on confirm) plus the metadata `Self::recon_block` needs to build its
/// `ReconAlertView` projection, and the `route_key` needed to re-resolve the routing engine index
/// at confirm time (indices are NOT stored directly — cheap to recompute via
/// `engine_idx_for_route_key`, and robust if that ever became less than perfectly stable).
///
/// ⚠ **`venue` and `route_key` are stored SEPARATELY, and this record is why.** An operator confirm
/// arrives minutes to hours after the pass that raised the alert, and `Self::confirm_recon` folds
/// the held `proposed_events` into whichever engine this record resolves — so a single string here
/// would be a ROUTING decision made at pass time, kept, and re-applied later under the name of a
/// canonical venue. `venue` keeps the jobs that are genuinely about the EXCHANGE (the
/// operator-facing label, the capability row), so collapsing the two would force one of them to be
/// wrong for a second account of one exchange: route by `venue` and the approved events fold into
/// the FIRST account's book, or label by `route_key` and the operator-facing venue becomes a string
/// `vike_model::VENUES` does not contain.
///
/// ⚠ **The DEDUP IDENTITY below is NOT one of `venue`'s jobs, and this doc said it was.** It
/// carries BOTH strings (`recon_held::HeldId`), because a `dedup_key` names an instrument and a
/// side and never a book: fifty accounts of one exchange holding `position:BTCUSDT:Both` produced
/// fifty EQUAL identities, so the first account's row absorbed all fifty — refreshed with whichever
/// leg ran last while its `route_key` still named the first account — and one confirm folded
/// another account's fills into the wrong book. Identity is per ACCOUNT; labelling is per venue.
pub(crate) struct HeldReconAlert {
    /// The CANONICAL exchange id (`vike_model::VENUES`) — the operator-facing label. NEVER the
    /// routing input, and (since the identity below learned to carry the account) never the whole
    /// of this row's dedup identity either.
    pub(crate) venue: String,
    /// The ENGINE this alert's `proposed_events` fold into on confirm — carried verbatim from the
    /// pass that raised it (`ReconcileReports::route`), so the confirm lands where the pass would
    /// have. Equal to `venue` for a venue's SOLE account — which is every alert a box with no
    /// `[accounts]` table can raise — and the labelled account's own key otherwise.
    pub(crate) route_key: String,
    pub(crate) kind: DivergenceKind,
    pub(crate) detail: String,
    pub(crate) proposed_events: Vec<Event>,
    /// Order-loss recovery payload (recon `JournalDivergence`): venue orders to RE-REGISTER into
    /// local state on confirm, via `ExecutionEngine::reregister_orders` (insert-only). Empty for
    /// every other alert.
    pub(crate) recover_orders: Vec<vike_model::OrderStatusReport>,
    /// This row's DEDUP IDENTITY — a later pass re-raising the same divergence REFRESHES this row
    /// in place (same confirm id, freshened payload) instead of appending a new one.
    ///
    /// ⚠ This used to be `vike_exec::recon::ReconAlert::dedup_key`, an `Option` that only SOME
    /// kinds set, and an alert without one appended a row EVERY PASS with a fresh id. Nothing caps,
    /// prunes or retains this store — `shift_remove` fires only on an operator confirm — so an
    /// un-keyed divergence that nothing heals grew it without bound: measured on the CI box, alert id
    /// 337 at 00:00 → 815 at 06:59 on 2026-08-25, ~65 rows/hour ≈ 1,560/day, every one of them the
    /// same two divergences, each holding its own `proposed_events` and each projected into every
    /// published `CoreSnapshot` by `Self::recon_block`. `recon_held::HeldId` is the stable name
    /// those alerts never carried; where a `dedup_key` DOES exist it still decides alone, so this
    /// is a strict generalization of the old match rather than a different rule.
    pub(crate) identity: recon_held::HeldId,
}
