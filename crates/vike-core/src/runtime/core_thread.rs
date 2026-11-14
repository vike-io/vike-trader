//! The single-writer `CoreThread` state, and the held reconcile-alert record it keeps.

use super::*;

/// One mount's declared legs as its published row names them (`vike_execution::MountView::legs`): `(venue,
/// symbol)`, `None` for a mount that declares none. Named for clippy's `type_complexity`.
pub(crate) type MountLegRow = Option<Arc<[(String, String)]>>;

/// A RESTORED core-held stop waiting for its account's first reconcile pass (`CoreThread::stop_holds`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OrderEmulationStopWait {
    /// The engine whose first pass releases it.
    pub(crate) engine: usize,
    /// The core clock (ms) at which the hold LAPSES, set lazily at the first crossing it holds back
    /// (`None` until then): the first pass is not coming for an account with no reconcile leg.
    pub(crate) until: Option<i64>,
}

/// The close a core-held stop sent when it fired, kept until the cold sweep has judged how it ended
/// (`CoreThread::fired_stops`): the terms the stop is re-armed with when the close was refused or filled
/// only in part.
#[derive(Clone, Debug)]
pub(crate) struct OrderEmulationStopClose {
    pub(crate) arm_id: String,
    pub(crate) venue: String,
    pub(crate) symbol: String,
    pub(crate) side: i32,
    /// The ARMED quantity (not the capped quantity the fire sent).
    pub(crate) qty: f64,
    /// The arm's terms as it rested (its kind, trigger, limit, trail): what a re-arm arms again.
    pub(crate) armed: crate::emulator::OrderEmulationArm,
    pub(crate) engine: usize,
    /// The mount whose strategy armed the stop (`OrderEmulationArmRoute::mount`), so a re-arm stays that strategy's.
    pub(crate) mount: Option<usize>,
    /// The order client ids of the close (empty when it was refused before it was registered).
    pub(crate) order_client_ids: Vec<String>,
    pub(crate) rearms: u8,
    /// The engine clock (ms) of the fire.
    pub(crate) at_ms: i64,
}

/// What [`CoreThread::account_holds`] remembers about one held engine (`Command::HoldAccount`): the state
/// the engine would hold WITHOUT the hold, so releasing it lifts exactly what it set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AccountHold {
    /// The engine's state absent the hold: its state when the hold was set, then whatever a node-wide
    /// `SetTradingState` or an account-scoped `SetAccountTradingState` asked since.
    pub(crate) free: TradingState,
}

/// What [`CoreThread::pending_owners`] holds for one mount id: the orders it owned and its ledger.
#[derive(Debug, Default)]
pub(crate) struct OrderOwnerPending {
    pub(crate) order_client_ids: Vec<crate::order_owners::OrderOwnerEntry>,
    pub(crate) ledger: Option<MountAttribution>,
}

/// What [`CoreThread::restore_unproven`] remembers of one UNPROVEN restored order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RestoreUnprovenOrder {
    /// The core clock (ms) of the first pass that found the order absent. A later pass counts as
    /// QUIET only if its fill window opened at or after it (`ReconcileReports::since`), so the
    /// lookback in which a fill of the order could have shown has passed.
    pub(crate) first_ms: i64,
    /// The quiet passes counted so far (`vike_execution::recon::RESTORE_QUIET_PASSES` make it gone).
    pub(crate) quiet: u32,
}

/// The core-level counters of order RESTORE (the last four slots of `crate::counters::COUNTER_NAMES`),
/// summed into the mmap mirror by `collect_counters`. Plain `u64`s on the fold thread; nothing here
/// is an engine fold, so nothing resets them but a new process.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RestoreCounters {
    /// Restored orders the first successful reconcile pass found still resting and adopted back
    /// into the engine registry (incremented by the reconcile adoption step, not by assemble).
    pub(crate) restored_adopted: u64,
    /// Restored orders that pass found no longer at the venue and no fill for.
    pub(crate) restored_gone: u64,
    /// Restored orders that pass found filled while the process was down.
    pub(crate) restored_filled_while_down: u64,
    /// A tagged submit re-pointed a strategy tag whose previous order was STILL LIVE in the
    /// registry (`drain_broker`): that older order keeps resting with no tag naming it.
    pub(crate) tag_overwrite_orphaned: u64,
}

/// The drawdown latch's state for ONE currency group other than the primary's (the primary's is
/// [`CoreThread::pnl_curve_peak`] and [`CoreThread::pnl_curve_disarmed_noted`], untouched). A curve adds
/// figures of one currency, so every group has its own high-water mark and trips on its own drop
/// (`CoreThread::sweep_drawdown_latch`). Created at the group's first sweep; core-local, NOT serialized.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ForeignCurve {
    /// The group's key (`Engine::ledger_currency_key`). EMPTY when this is the dollar group of a node
    /// whose PRIMARY engine is in another currency: "foreign" here means "not the primary's".
    pub(crate) currency: vike_execution::Interned,
    /// The group's high-water mark, as `pnl_curve_peak` is the primary's.
    pub(crate) peak: Option<f64>,
    /// The one-shot guard of this group's DISARMED warning, as `pnl_curve_disarmed_noted` is the primary's.
    pub(crate) disarmed_noted: bool,
    /// The group's curve (`capital base + own PnL`) at the last sweep that measured it: what the publish
    /// site hands a push alert beside `peak` ([`vike_execution::Portfolio::group_curves`]).
    pub(crate) curve: f64,
    /// The group's capital base at that same sweep: the figure the curve was built on, published beside it.
    pub(crate) base: f64,
}

/// How long the latch leaves a non-dollar group unjudged while a demo or live engine of it has not reported its
/// first wallet ([`CoreThread::group_awaits_wallet`]), in event-clock ms. Measured on the CI box (2026-10-27): a hot
/// activation of FXCM's EUR demo had its first balance 70 to 90 s later; the first reconcile pass is at most
/// `MAX_RECONCILE_STARTUP_DELAY_MS` (60 s) after the start and a failed pass is retried one interval (60 s by
/// default) on, so 300 s covers three passes. Past it the engine is read as one that never gets a balance (a venue
/// with no reconcile) and its group is judged on the configured seed, as it was before the wait existed.
pub(crate) const ADOPT_WAIT_MS: i64 = 300_000;

/// One engine's wait for its first wallet balance ([`CoreThread::adopt_wait`]): a demo or live engine in a
/// non-dollar group whose seed slot still holds the configured seed, which the latch will not judge its group
/// against ([`CoreThread::group_awaits_wallet`]). Created at the first sweep that finds the engine so; core-local,
/// NOT serialized.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AdoptWait {
    /// The event clock (`Engine::now_ms`) of the sweep that first found the engine waiting.
    pub(crate) since_ms: i64,
    /// The wait ran out and the ring said so: from then on the group is judged on the seed, as it always was.
    pub(crate) gave_up: bool,
}

/// The downcast `Ingest::Accounts` is opened with: a parcel of the core's own engine type, or nothing.
pub(crate) type EngineDoor<C> = fn(Box<dyn std::any::Any + Send>) -> Option<Engine<C>>;

pub(crate) struct CoreThread<C: ExecutionClient> {
    pub(crate) engine: Engine<C>,
    pub(crate) bus: EventBus,
    pub(crate) config: CoreConfig,
    pub(crate) market: Arc<MarketConflation>,
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
    pub(crate) sequence: u64,
    pub(crate) dirty: bool,
    /// the core-owned bar cache (plan §1 "Cache write BEFORE dispatch"); snapshots share
    /// the closed series by Arc — R7 strategies read this same cache deterministically
    pub(crate) bars: indexmap::IndexMap<SeriesKey, BarSeries>,
    /// order-client-id generator for the mounted strategy's orders
    pub(crate) order_client_id_gen: vike_model::orders::client_id::OrderClientIdGenerator,
    /// **One order client id MINTING PLAN per engine** (decision 0270), at the routing index (`engine` is 0,
    /// `extra_engines[i]` is `i + 1`): the shape and account code that engine's ids carry, chosen ONCE when the
    /// core is assembled (`assemble_core`) or an engine appended (`account_change.rs`'s `add_engine_hot`), so a
    /// mint reads no venue (the HFT rule). One generator ([`Self::order_client_id_gen`]) serves every plan: one session, one
    /// sequence, only the render differs. Core-local, never snapped: a replay core rebuilds it from its engines.
    pub(crate) order_client_id_plans: Vec<vike_model::orders::client_id::OrderClientIdPlan>,
    /// The LATE-RETRY memory this boot was handed (`CoreConfig::recent_order_client_ids`, decision 0270 D7): `(client order
    /// id, created_ms)` of the ids seen before the restart. Carried into every `Snapshot` (`Snapshot.recent_order_client_ids`).
    pub(crate) recent_order_client_ids: super::recent_order_client_ids::RecentOrderClientIds,
    /// The PENDING SUCCESSOR of a cancel-then-new amend (decision 0270's `R<n>`): set by `apply/amend.rs`'s
    /// `place_replacement` for the one submit it lowers, taken by `lower_submit`'s mint in place of a fresh id (so
    /// the replacement is journaled as a `MintedSubmit` and asks no explicit-id refusal), and cleared after that
    /// submit whatever it did. Transient, never snapped: a replay re-derives it from the replayed registry.
    pub(crate) order_client_id_successor: Option<String>,
    /// The MOUNT a core-made order belongs to when it is not lowered on the mount's own route (decision 0270's
    /// `S`): a strategy's conditional fire, core-held stop included, lowers on its arm's engine. Set by
    /// `strategy_drive/conditionals.rs`'s `release_fired` around its one submit, read by `order_client_id_labels.rs`'s
    /// `order_client_id_who`, cleared after. Transient, never snapped.
    pub(crate) order_client_id_owner: Option<usize>,
    /// HFT modify surface: strategy-chosen tag → the current order client id it minted for, so a strategy can
    /// `modify(tag, ..)` a resting order it never saw the order client id of. A re-submit under a tag whose order
    /// still works is refused (P1, `drain_broker`'s `tag_busy`), one after its cancel overwrites;
    /// stale entries (terminal orders) are harmless (modify_order gates on status).
    ///
    /// KEYED `{mount_index}|{venue}|{symbol}|{tag}` (multi-mount correctness, bug A). The mount index
    /// leads because tags are strategy-LOCAL names, not global ones: the spread maker emits the
    /// literal tags `"bid"`/`"ask"`, so two makers on ONE (venue, symbol) both wrote
    /// `binance|BTCUSDT|bid` — mount B's submit OVERWROTE mount A's order client id, and A's next
    /// `cancel("bid")`/`amend("bid", ..)` then operated on B's RESTING ORDER (a real
    /// cross-mount money bug, not a display one). The index is unique per slot by construction and
    /// cheaper than the string mount id. Single-mount runs are unaffected in behavior (every key
    /// simply gains the constant `0|` prefix; the map is runtime-only and never persisted or
    /// published, so nothing on the wire or in the journal changes).
    pub(crate) strategy_tags: indexmap::IndexMap<String, String>,
    /// The order client ids a mount's cancel BY TAG was sent for, so their tag is free (P1, `drain_broker`'s
    /// `tag_busy`): the engine never marks its own cancel on the order (`PendingCancel` is only ever
    /// a venue's word, seeded by a reconcile report), so this is the one record that a cancel went.
    /// An entry goes when its order ends (`note_terminal_order_client_id`), its tag names a newer order, or the
    /// venue refuses the cancel (`drain_delivered`: the order works on, so its tag is busy again).
    pub(crate) tag_cancels_sent: std::collections::HashSet<String>,
    /// Phase D: every strategy mount (config.strategy + extra_mounts); Option per slot
    /// for the take/replace dance around strategy calls
    pub(crate) mounts: Vec<Option<Mount>>,
    /// Portfolio-observer PR-4 T2: each mount's deterministic id
    /// ([`crate::strategy_state::mount_id_with`]), same length and index alignment as `mounts`,
    /// computed once in `assemble_core`. Read by the `run()` teardown's save-on-stop (gated on
    /// [`CoreConfig::state_dir`]) so it never recomputes the id from a possibly-taken (`None`)
    /// mount slot.
    pub(crate) mount_ids: Vec<String>,
    /// Portfolio-observer PR-4 T5: each mount's [`MountState`], same length and index alignment
    /// as `mounts` (computed once in `assemble_core`, never resized after). All `Ready` forever
    /// when [`CoreConfig::readiness_gate`] is off (the default); seeded `Pending` when it is on,
    /// flipped to `Ready` one-way by [`Self::maintain_mount_readiness`]. Read by
    /// [`Self::drain_broker`] (the discard gate) and [`Self::mount_views`] (the snapshot view).
    pub(crate) mount_states: Vec<MountState>,
    /// Live-runtime OTO/OCO: the ONE shared [`vike_execution::ContingencyBook`] resolver, keyed by order client id
    /// (globally unique per core). Records a leg's linkage the instant a linked/bracket order is
    /// submitted; on a leg's FILL it arms held OTO children (released via `held_orders`) and returns
    /// the OCO siblings to cancel. EMPTY for every non-bracket run — plain orders are never inserted,
    /// so the whole feature is a single `is_empty()` check away on the byte-identical path.
    pub(crate) contingency: ContingencyBook,
    /// The resolved requests of OTO children the runtime is HOLDING back from the venue until their
    /// parent fills (live-runtime OCO/OTO), keyed by order client id. A held exit lives ONLY here — it is NOT
    /// in any engine registry and NOT at the venue — until its parent's fill arms it, at which point
    /// it is drained here and submitted (`release_held_child`). Also the durable half a `Snapshot`
    /// captures (`JournalSnapshotContingency.held_request`). `IndexMap`: release/drop order is the map's own
    /// insertion order, deterministic. Empty on every non-bracket run.
    pub(crate) held_orders: indexmap::IndexMap<String, OrderRequest>,
    /// The orders the core HOLDS until their release (`vike_model::OrderRelease`), keyed by order client id, in the
    /// order they were held: accepted and preflighted, in no engine registry, not at the venue. The
    /// contract is `held_release.rs`'s module doc. Also the durable half a `Snapshot` captures
    /// (`Snapshot.held_releases`). Empty on every core that was never sent an order with a release.
    pub(crate) held_releases: indexmap::IndexMap<String, vike_journal::OrderHeldRelease>,
    /// EVERY emulated order the core holds, in one entity (decision 0275): the trigger kinds' books
    /// and arm rows, and the working parents with their laws and live children. The contract is
    /// `emulated.rs`'s module doc; the trigger law is `crate::emulator`'s.
    pub(crate) emulated: crate::emulator::OrderEmulationBook,
    /// When the armed price poll of [`Self::held_releases`] falls due (`None`: none armed), so a
    /// conditional order waiting for its price keeps ONE poll timer, not one per release that fired.
    /// Core-local, never snapped.
    pub(crate) release_poll_due: Option<i64>,
    /// Why a held order of [`Self::held_releases`] whose time came still waits (its price condition read no
    /// FRESH price), keyed by order client id; set by `release_due` when the words change, published as
    /// `OrderHeldView::waiting`. Core-local, never snapped: after a restart the first poll says it again.
    pub(crate) release_waiting: std::collections::HashMap<String, String>,
    /// Set by `apply_routed_order` for the one `lower_submit` call that IS a held order's release, read
    /// and cleared there. Transient, never snapped.
    pub(crate) releasing: bool,
    /// Monotone counter behind [`Self::mint_arm_id`] — the emulated conditionals' id sequence
    /// (emulator-journal PR-1). Deliberately SEPARATE from the `OrderClientIdGenerator`: an arm
    /// is not an order, and drawing arm ids from the order client id generator would shift every subsequent
    /// order's order client id the moment a conditional is armed.
    ///
    /// Seeded from [`CoreConfig::arm_sequence`] — `0` for a fresh session, and on a restart that
    /// resumes `order_client_id_session` the value [`crate::replay::RestoredState::arm_sequence`] carries, so the
    /// `{order_client_id_session}a{arm_sequence}` ids minted after a restart cannot collide with the ones the
    /// pre-restart run already wrote into the SAME (appended-to) journal. Stamped into every
    /// `Snapshot` next to `order_client_id_sequence` (emulator PR-2) so that resume value survives segment pruning.
    /// Since emulator PR-3 the arms themselves survive a restart too: the books ride the same
    /// `Snapshot` (`Snapshot.order_emulation_arms`) and re-seed via [`CoreConfig::order_emulation_arms`], so the resumed
    /// counter and the re-armed ids come from the ONE restore base together.
    pub(crate) arm_sequence: u64,
    /// Monotone counter behind [`CoreThread::mint_refusal_id`] — the id sequence for an intent the
    /// drain REFUSED before it could become an order (today: a declared-multi mount naming an
    /// UNDECLARED symbol, see `CoreThread::resolve_intent_symbol`).
    ///
    /// SEPARATE from the [`vike_model::orders::client_id::OrderClientIdGenerator`] for the
    /// same reason [`Self::arm_sequence`] is, only more sharply: a refusal is NOT an order — it
    /// journals nothing and submits nothing — so drawing its id from the order client id generator would burn
    /// an order-client-id that never appears in the journal. Every later order's order client id would then sit
    /// one step ahead of the `order_client_id_sequence` the `Snapshot` records, and a restart that resumes from that
    /// journal would re-mint an id the live session had already spent: a misconfigured session
    /// would become un-restorable.
    ///
    /// Not stamped into a `Snapshot` and not resumed (unlike `arm_sequence`): a refusal id is never a lookup
    /// key for anything durable — no journal record carries it, no disarm targets it — so a restart
    /// restarting the sequence at 0 can collide with nothing. It is purely a handle for the
    /// operator/strategy-visible denial the refusal emits.
    pub(crate) refusal_sequence: u64,
    /// Cross-venue: additional (seed_cash, engine) pairs, one per extra venue. Empty
    /// (spawn_core default) = single-venue behavior, byte-identical. Index 0 in routing
    /// terms is the primary `engine`; extra i is routing index i+1.
    pub(crate) extra_engines: Vec<(f64, Engine<C>)>,
    /// **Do two engines in this process share one exchange?** — i.e. does this process mount more
    /// than one ACCOUNT of some venue. Computed once at construction (`assemble_core`) from the
    /// engines' own `venue` fields; `false` on every process with one account per venue, which is
    /// every process whose account table arms no labelled account.
    ///
    /// It exists for exactly one branch, in [`Self::route_event`], and the whole reason it is a
    /// precomputed `bool` rather than a scan is that the branch sits on the fold: a `false` costs
    /// one predictable compare per venue-tagged event and reaches the identical `return` the
    /// routing has always taken, so the single-account hot path is byte-identical.
    pub(crate) multi_account: bool,
    /// How a parcel on `Ingest::Accounts` becomes an `Engine<C>`: the downcast, stored here by
    /// `spawn_core_multi` (the one place `C: 'static` is known) so no `impl` block needs the bound.
    /// `None` (the synchronous test core) refuses every parcel. Read only by the cold arm in
    /// `account_change.rs`.
    pub(crate) engine_door: Option<EngineDoor<C>>,
    /// The indices of the engines a STOP left in the list (decision 0169, slice S2): tombstones. EMPTY on a core
    /// that never stopped an account, which is one `is_empty` test on the lookups below. An index is never
    /// reused or removed from here, and the engine stays where it is: it is the attribution key of
    /// everything it wrote. Read by the route-key lookup (a live engine of one key wins over a
    /// stopped one), the venue counts and the margin sweep; written only by `account_change.rs`.
    pub(crate) retired_engines: Vec<usize>,
    /// **Does a running engine of this core state the `paper` tier?** Set where the engine set changes
    /// (assembly, an account added or stopped: [`Self::refresh_paper_engines`]). The funding-rate lane reads it
    /// first, so a node with no paper engine (every live node) does no work per funding observation.
    pub(crate) any_paper_engine: bool,
    /// The bound on the UNATTRIBUTED log (`refuse_unattributed`): per-kind full-line allowance and the
    /// counts waiting for the next summary. `None` until the first event is refused, so a node that
    /// never refuses one pays a pointer. Core-local: not serialized, not state-hashed, not published.
    pub(crate) unattributed: Option<Box<dispatch::UnattributedLog>>,
    /// **The LABELLED engines that hold a symbol `Command::AdmitSymbol` installed**, by routing index
    /// (see [`Self::rebuild_admitted_fanout`]): the engines, beyond the one a venue's default account
    /// is, that the quote, trade and book arms of `dispatch` must also write. Empty on every process
    /// that admitted nothing into a labelled account (every process before decision 0173), so the arms
    /// pay one `is_empty()` and nothing else. Rebuilt only by the cold paths, never on the fold.
    pub(crate) admitted_fanout: Vec<usize>,
    /// order client id → routing index, written at submit time — order-LIFECYCLE events carry no
    /// venue field (wire schema), so replies route back through this map (miss = primary,
    /// today's behavior).
    pub(crate) order_client_id_venue: std::collections::HashMap<String, usize>,
    /// **Whether the tick lane checks the conditional books** (`drive_strategy_tick`): the run
    /// profile's `guards.conditionals_on_ticks` ([`CoreConfig::conditionals_on_ticks`]), OR an
    /// operator's core-held reduce-only stop rests (an [`OrderEmulationArmRoute::reduce_only`] row in
    /// [`crate::emulator::OrderEmulationBook::cond_engine`]). The second term is why this is a field of its own: a stop checked per
    /// CLOSED BAR is no protection (FXCM and Polymarket may run no bar for the symbol at all), and
    /// the arm is journaled, so it cannot be refused on a setting the replay core does not carry.
    /// `drive_strategy_tick` reads this bool where it used to read the config bool: the same one load.
    /// Written only by `Self::refresh_tick_conditionals`, at assembly and wherever a core-held arm
    /// enters or leaves `cond_engine` (command cadence).
    pub(crate) tick_conditionals: bool,
    /// **Restored core-held stops WAITING for the first reconcile pass of their account** (arm id ->
    /// [`OrderEmulationStopWait`]), filled by `restore_arm_routes`, emptied by `reconcile_reports` (the pass for the
    /// engine) and by a lapse. A held stop stays in its book (the Snapshot still writes it, a cancel still
    /// disarms it); `submit_fired` puts a held one back instead of firing it. Empty on every node that
    /// restored no core-held stop, so the fire path pays one `is_empty` test. Not state: never hashed,
    /// snapped or journaled.
    pub(crate) stop_holds: std::collections::HashMap<String, OrderEmulationStopWait>,
    /// The closes of core-held stops that FIRED, until the cold sweep has judged how each ended
    /// (`CoreThread::sweep_core_held_stops`). Bounded by `FIRED_STOPS_MAX`. Not state: never hashed,
    /// snapped or journaled; empty at boot.
    pub(crate) fired_stops: Vec<OrderEmulationStopClose>,
    /// `(engine, order client id)` of each reduce-only LIMIT a conditional fire released (a stop-limit's, a
    /// limit-if-touched's, a trailing-limit's close), which a core-held fire nets whether or not it crosses
    /// (`CoreThread::core_held_closes_in_flight`). Pruned to the live ones at each insert, so bounded by the
    /// live orders. Not state: never hashed, snapped or journaled; empty at boot.
    pub(crate) fired_limit_closes: Vec<(usize, String)>,
    /// Opt-in write-ahead journal (spec §A). `None` = journaling off (default) — the whole
    /// hot-path hook is skipped and the fold is byte-identical to today. Opened once at
    /// construction from [`CoreConfig::journal`].
    pub(crate) journal: Option<vike_journal::Journal>,
    /// Records appended since the last `Snapshot` — drives the [`JournalConfig::snapshot_every`]
    /// cadence. Reset to 0 by [`CoreThread::write_snap`].
    pub(crate) journaled_since_snap: u64,
    /// WHERE each gateway's per-account event ring stood at the last [`Ingest::GatewayEvent`] this
    /// core folded (journal v18): `(route key, cursor)`, one entry per account that ever delivered
    /// one, seeded at assembly from [`CoreConfig::gateway_cursors`] (the restored `Snapshot`'s and
    /// tail's) so an account that stays quiet keeps its cursor through the next `Snapshot`. Written only
    /// by the `GatewayEvent` arm, through [`vike_model::EventRingCursor::after`]; read only by
    /// [`CoreThread::write_snap`], which stamps it on the `Snapshot`. A linear scan: one entry per
    /// gateway account, and the arm that scans it is the trade link's, not the market-data lane's.
    /// NOT fenced state (outside `state_hash`) and empty forever on a box with no gateway.
    pub(crate) gateway_cursors: Vec<(ustr::Ustr, vike_model::EventRingCursor)>,
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
    /// guard: `order_client_id → the now_ms at which stage 1 ISSUED its active confirm for that order`. Two jobs:
    /// (a) stage 1 warns + confirms each order client id exactly ONCE per stuck episode (present ⇒ already
    /// confirmed, skip); (b) stage 2's guard — a reject candidate WITH an entry has a confirm in
    /// flight, so the last-resort reject is DEFERRED to `created + submit_ack_timeout +
    /// 2·submit_ack_confirm_grace` (one extra grace window) to let a slow confirm (Bybit's
    /// realtime→history re-query can exceed a single grace) land before terminalizing. Whenever an order client id
    /// is here the order is STILL pre-ack (any authoritative fold moves it out), so an entry means
    /// "confirm issued, nothing authoritative folded since". Core-local, NOT serialized (the watchdog
    /// path is out of replay scope); self-cleaning — pruned each sweep to only still-pre-ack order client ids, so
    /// it stays bounded. Empty unless the watchdog is enabled.
    pub(crate) confirm_issued_ms: std::collections::HashMap<String, i64>,
    /// Per-account TRADE-LINK state filed by [`Ingest::TradeLink`] (route key → [`execution_link::TradeLinkGate`]),
    /// read by the stuck-order ladder through [`CoreThread::execution_link_resumes`]: an account whose link
    /// is down has its pre-ack orders skipped, and one whose link came back counts from the link-up
    /// instant. EMPTY unless some link has been disclosed down (a box with no gateway never fills it).
    /// Core-local, NOT serialized (a transport fact, not order state); keyed by `String` so a sweep
    /// looks an engine's `route_key` up without allocating.
    pub(crate) trade_link: std::collections::BTreeMap<String, execution_link::TradeLinkGate>,
    /// Drawdown latch (audit exec#4) high-water-mark, on the DAEMON'S OWN equity curve
    /// (`Σ seed_of(e) + Σ Engine::resolved_own_pnl(e)` — never an account balance level;
    /// see [`CoreThread::sweep_drawdown_latch`] for why the wallet had to leave this number).
    /// Folded on the per-CLOSED-bar sweep, only when [`CoreConfig::max_drawdown`] is enabled.
    /// Core-local, NOT serialized (never journaled/snapshotted); `None` until the first sweep (or
    /// forever, when the latch is disabled). RESTART: the LATCH itself
    /// (`trading_state = Reducing`) IS restored via the engine snapshot, and the CURVE resumes
    /// underwater because `Account`'s PnL terms are in `EngineSnapshotAccount` — so this HWM re-seeds to
    /// `max(capital_base, curve)`, which forgives no pre-restart loss below configured capital.
    pub(crate) pnl_curve_peak: Option<f64>,
    /// One-shot guard for the drawdown latch's DISARMED warning — see
    /// [`CoreThread::sweep_drawdown_latch`]. The condition (a non-positive capital base under an
    /// armed `max_drawdown`) holds on every subsequent sweep, so without this the per-bar ring
    /// would carry nothing else. Core-local, NOT serialized, like the HWM above.
    pub(crate) pnl_curve_disarmed_noted: bool,
    /// The same state for every currency group but the primary's (an engine whose account is in
    /// another currency than the primary engine's: [`Engine::ledger_currency_key`]). EMPTY, and
    /// never allocated, on a node whose engines share one currency, which is every node today.
    pub(crate) foreign_curves: Vec<ForeignCurve>,
    /// The first engine's group's curve at the last sweep (the counterpart of `foreign_curves[k].curve`
    /// for the slot that keeps [`Self::pnl_curve_peak`]). `0.0` until the latch has swept.
    pub(crate) pnl_curve_last: f64,
    /// The first engine's group's capital base at that same sweep (the counterpart of `foreign_curves[k].base`).
    pub(crate) pnl_curve_base: f64,
    /// The engines (by routing index) whose capital base the latch has already moved from the configured seed
    /// to the wallet they adopted ([`CoreThread::adopt_wallet_bases`]): a NON-dollar engine does it once, at the
    /// first sweep that finds it `Authoritative`. EMPTY, and never allocated, on a node of dollar engines.
    /// Core-local, NOT serialized, like the high-water marks beside it. **"Adopted" means exactly membership
    /// here**: the engine's seed slot holds the wallet its account reported, no longer the configured seed.
    pub(crate) base_adopted: std::collections::BTreeSet<usize>,
    /// The configured capital of each COIN-LEDGER engine (by routing index), in its settle coin: the seed it
    /// had at the first sweep that could price the coin, divided by that price ([`CoreThread::revalue_coin_seeds`],
    /// decision 0258). EMPTY, and never allocated, on a node with no engine mounted on an inverse contract.
    /// Core-local, NOT serialized, like the state beside it: a restart converts the seed again.
    pub(crate) coin_seeds: std::collections::BTreeMap<usize, f64>,
    /// The engines (by routing index) the latch found waiting for that first wallet and since when
    /// ([`CoreThread::group_awaits_wallet`]). EMPTY, and never allocated, on a node whose non-dollar engines
    /// are paper or already adopted, which is every node but one with a demo or live account in another
    /// currency that has not reconciled yet. Core-local, NOT serialized, like the state beside it.
    pub(crate) adopt_wait: std::collections::BTreeMap<usize, AdoptWait>,
    /// Whether any two engines this core holds, stopped ones included, are in different currency
    /// groups (`Engine::ledger_currency_key`). Engines never leave and a group is fixed when an engine is
    /// built (`set_account_currency` runs at assembly), so it is set at assembly and by a hot add and never
    /// cleared. It lets the per-publish paths (the mount residual row, the group view) pay one bool on a
    /// node of one currency, which is every node today.
    pub(crate) ledger_mixed: bool,
    /// **THE NODE'S TRADING STATE** — the one node-level fact, as opposed to any engine's own state
    /// (engine 0's included: an account-scoped state on the primary is not a node fact). The strictest
    /// state a NODE-WIDE rule has asked of every engine, and the floor an account-scoped
    /// `Command::SetAccountTradingState` cannot go below: the drawdown latch's `Reducing`
    /// ([`CoreThread::raise_trading_state`]), the operator's node-wide `SetTradingState`
    /// ([`CoreThread::set_trading_state`], which REPLACES it, so a node-wide `Active` clears it), the
    /// silence dead-man's `Halted`, the panic safe state's `Halted`. Every reader of "the node's state"
    /// reads THIS: the drawdown latch's "already latched?" (`sweep_drawdown_latch`), a hot-started
    /// account's inherited state (`add_engine_hot`), `resume-venue`'s fallback. `Active` (nothing held) for
    /// a core whose engines start `Active`.
    ///
    /// Core-local, NOT serialized and NOT state-hashed, but REBUILT at assembly: the minimum strictness
    /// across the engines the boot restore put back (`restored_node_floor`), so a restored uniform
    /// halt or latch keeps its floor and a lone halted engine (a refused snapshot, an account-scoped halt)
    /// does not read as the node's.
    pub(crate) node_floor: TradingState,
    /// The ACCOUNT-level holds (`Command::HoldAccount`): engine index -> the hold. A held engine is never
    /// below `Reducing`, whatever a node-wide `SetTradingState` says (the node-wide word sets the state
    /// the engine returns to, [`AccountHold::free`]). Keyed by engine INDEX, not route key: a restarted
    /// account is a new engine and carries no hold. Empty on every core that never ran one. Core-local,
    /// NOT serialized: a restart keeps the engine's restored state (so it fails closed) and forgets the hold.
    pub(crate) account_holds: std::collections::BTreeMap<usize, AccountHold>,
    /// Deadline timer wheel (audit co6), advanced ONCE per drain-loop boundary — NEVER per message,
    /// so the p99<10µs fold is untouched. Empty by default (a single `is_empty` gate keeps the
    /// wheel-free path byte-identical); armed with a self-rescheduling [`TimerKind::StuckSweep`] only
    /// when the watchdog (`submit_ack_timeout`) is enabled. See [`CoreThread::drive_due_timers`].
    pub(crate) timers: TimerWheel<TimerKind>,
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
    /// `Command::ConfirmReconcile`, keyed by the id `reconcile_reports` assigns when the alert is
    /// first surfaced: the digest of the row's identity (`recon_held::ReconcileHeldId::confirm_id`), so
    /// the same divergence has the same id after a restart. Fold-thread-only state (single-writer
    /// invariant — never mutated off this thread); the GUI-facing projection is
    /// `CoreSnapshot.recon.alerts` (`ReconcileAlertView`, built by `Self::reconcile_block`), which carries
    /// kind/detail/count but never the raw `Event`s. NOT journaled: the store is rebuilt by the
    /// next pass, and an id it does not hold is a refused no-op (see
    /// `vike_execution::Command::ConfirmReconcile`'s doc). A row a later pass contradicts is removed
    /// (`reconcile_pass`).
    pub(crate) reconcile_alerts: indexmap::IndexMap<u64, ReconcileHeldAlert>,
    /// Which held divergences have already been ANNOUNCED to the log, per venue — the state that
    /// turns a per-pass repetition into a per-transition line. Purely a logging concern: it never
    /// decides what folds, what holds or what `reconcile_alerts` stores, and it is touched ONLY from
    /// `reconcile_reports` (interval cadence) and `confirm_reconcile` (an operator command), never the
    /// per-message fold. Empty and allocation-free on a box with no held divergences. See
    /// `crate::runtime::recon_held`.
    pub(crate) reconcile_announce: recon_held::ReconcileHeldAnnouncer,
    /// The latest delivered reconcile pass of each ACCOUNT (route key): its sequence number and what
    /// it could say about each kind. What makes a held row stale
    /// (`crate::runtime::reconcile_pass`). Fold-thread-only; empty until a pass is delivered.
    pub(crate) reconcile_pass: std::collections::HashMap<String, reconcile_pass::ReconcilePassMark>,
    /// wall-clock ms of the most recently completed reconcile pass (`engine.now_ms` at the time
    /// `reconcile_reports` ran); 0 before the first pass. Surfaced verbatim as
    /// `CoreSnapshot.recon.last_pass_timestamp`.
    pub(crate) reconcile_last_pass_timestamp: i64,
    /// Latest venue-REPORTED per-position coin delta from each reconcile pass, keyed `(venue,
    /// symbol)` (Wave 5d). Upserted from every `PositionStatusReport` that carries a `delta`
    /// (Deribit `get_positions.delta` today); empty and inert for every other venue. Cloned into
    /// `CoreSnapshot::reconcile_coin_deltas` at every publish so the greeks tool can fold a
    /// Deribit perp/future leg via `coin_delta × spot` — `PositionView` is fills-derived and has
    /// no venue delta of its own.
    pub(crate) reconcile_coin_deltas: indexmap::IndexMap<(String, String), f64>,
    /// Dead-man's-switch (auto cancel-on-disconnect) pure trip-logic state machine — `Some` ONLY
    /// when [`CoreConfig::deadman`] is set, else `None` (default) and the whole feature is inert.
    /// [`Self::dispatch`] feeds it the freshest data/event ts ([`DeadManSilence::observe`], the ONE cheap
    /// per-message store, gated on `is_some`); [`Self::sweep_deadman`] evaluates it at the drain-loop
    /// boundary on the [`TimerKind::DeadManSweep`] cadence — NEVER the per-message fold.
    pub(crate) deadman: Option<DeadManSilence>,
    /// The dead-man sweep cadence (ms) the [`TimerKind::DeadManSweep`] timer re-arms itself at —
    /// `deadman.timeout / 2` clamped `>= 50ms`, mirroring [`Self::watchdog_tick_ms`]. 0 when the
    /// switch is disabled (never armed).
    pub(crate) deadman_tick_ms: i64,
    /// CONNECTION-state dead-man latch (M13) — `Some` ONLY when [`CoreConfig::link_deadman`] is
    /// set, else `None` (default) and the whole feature is inert. [`Self::dispatch`] feeds it the
    /// `Ingest::StreamStatus` transitions ([`DeadManLink::observe`] — an OCCASIONAL control event,
    /// never the per-message fold); [`Self::sweep_link_deadman`] evaluates it at the drain-loop
    /// boundary on the [`TimerKind::LinkDeadManSweep`] cadence.
    pub(crate) link_deadman: Option<DeadManLink>,
    /// The link-dead-man sweep cadence (ms) the [`TimerKind::LinkDeadManSweep`] timer re-arms
    /// itself at — `link_deadman.grace / 2` clamped `>= 50ms`, the [`Self::deadman_tick_ms`]
    /// shape. 0 when the switch is disabled (never armed).
    pub(crate) link_deadman_tick_ms: i64,
    /// The instant (ms) each fixed-cadence boundary timer is SCHEDULED for, indexed by `TimerKind as usize`
    /// ([`Self::arm_periodic`] writes it, [`Self::rearm_periodic`] reads it): the next firing is the scheduled
    /// instant plus the cadence, not the instant the boundary happened to reach the timer, so the cadence does
    /// not drift by the wheel's 50 ms quantum and a late boundary does not push every later sweep out with it.
    /// 0 for a kind never armed. Core-local, never serialized.
    pub(crate) periodic_due_ms: [i64; TIMER_KINDS],
    /// The risk-clock cadence (ms) the [`TimerKind::RiskClockSweep`] timer re-arms itself at —
    /// [`CoreConfig::price_only`]'s interval, clamped `>= 1ms`. 0 when the feature is disabled (never armed).
    pub(crate) price_only_tick_ms: i64,
    /// The engines the clock's risk sweeps are SKIPPING right now, by engine index, with the reason
    /// said (`CoreThread::price_only_basis`): the note is written when an engine enters (or changes) a
    /// reason and the entry is dropped when its marks are usable again, so a dead pump is said once per
    /// episode, not at every sweep. Empty, and never touched, without [`CoreConfig::price_only`].
    pub(crate) price_only_skipped: std::collections::BTreeMap<usize, &'static str>,
    /// The HOLD-DOWN of that episode, by engine index: the engine-clock instant (ms) of the first sweep that
    /// found a skipped engine fresh again. The episode in [`Self::price_only_skipped`] ends only when the engine
    /// has stayed fresh for `RECOVERY_SWEEPS` (`watchdog/risk.rs`) clock intervals, so a feed that gaps around the age bound is ONE
    /// episode and not a WARN and a ring line at every other sweep; a sweep that finds it stale again drops the
    /// entry (the clock starts over). Only the SAYING is held: the margin call and the budgets act on a fresh
    /// engine at once. Empty, and never touched, without [`CoreConfig::price_only`].
    pub(crate) price_only_recovering: std::collections::BTreeMap<usize, i64>,
    /// Per engine index, the engine-clock instant (ms) before which its margin call stays quiet because it
    /// released a liquidation less than one [`Self::price_only_tick_ms`] ago: the close has not folded
    /// back, and the other pass (the closed bar, or the clock) would read the same breach and release a
    /// second one (`CoreThread::sweep_margin_call_engine`). Written only while the clock is armed, so
    /// empty without [`CoreConfig::price_only`]. Core-local, NOT serialized and not replayed: a replay runs
    /// no sweep (the journaled release is re-applied, never re-decided).
    pub(crate) margin_call_quiet_until: std::collections::BTreeMap<usize, i64>,
    /// The engines whose margin-call WARNING is being said right now (`CoreThread::sweep_margin_call_engine`):
    /// the clock says a band that holds ONCE per episode, until a sweep finds the engine out of it, so the
    /// 64-line recent ring it shares with fills and rejects is not crowded by one repeated line; a closed
    /// bar repeats per bar as it always did. Core-local, not serialized; empty without a margin call.
    pub(crate) margin_warning_held: std::collections::BTreeSet<usize>,
    /// The `(stage, engine)` pairs of the risk clock whose panic has been handled
    /// (`CoreThread::clock_stage`; `None` = a node-wide stage): each stage of each engine runs under its OWN
    /// `catch_unwind`, so a panicking engine does not end the sweep for the engines below it, and a stage that
    /// panics at every sweep is logged and cancel-swept once, not every 30 s. Core-local, not serialized.
    pub(crate) risk_clock_faulted: std::collections::BTreeSet<(&'static str, Option<usize>)>,
    /// The MANAGED-GTD sweep cadence (ms) the [`TimerKind::GtdSweep`] timer re-arms itself at —
    /// [`CoreConfig::gtd_sweep`], clamped `>= 1ms`. 0 when the feature is disabled (never armed).
    pub(crate) gtd_tick_ms: i64,
    /// Order client ids whose GTD expiry already triggered a cancel this episode (core-ergonomics) — the
    /// FIRE-ONCE guard for [`Self::sweep_gtd_expiry`], so a venue slow to report the terminal is
    /// not spammed with duplicate cancels. Core-local, NOT serialized (like `confirm_issued_ms`);
    /// self-cleaning — pruned each sweep to order client ids still live in some registry, so it stays bounded.
    /// Empty forever unless [`CoreConfig::gtd_sweep`] is enabled.
    pub(crate) gtd_canceled: std::collections::HashSet<String>,
    /// The periodic-portfolio-snapshot cadence (ms) the [`TimerKind::PortfolioSnapshot`] timer re-arms
    /// itself at — [`CoreConfig::portfolio_snapshot_interval`], clamped `>= 1ms`. 0 when disabled.
    pub(crate) portfolio_snap_tick_ms: i64,
    /// The FAST in-flight confirm cadence (ms) the [`TimerKind::InflightConfirm`] timer re-arms
    /// itself at — [`CoreConfig::inflight_confirm`], clamped `>= 1ms`. 0 when the feature is
    /// disabled (never armed). See [`Self::sweep_inflight_confirms`].
    pub(crate) inflight_confirm_tick_ms: i64,
    /// Whether the [`TimerKind::ExposurePair`] timer is armed (it re-arms itself for the core's life
    /// once it is), so the hot add of a second lane-twin engine does not arm a second one. `false`
    /// on a node with no engine that names an `Engine::exposure_partner`.
    pub(crate) exposure_pair_armed: bool,
    /// FAST in-flight confirm dedup (recon path-to-superset, F1-A): `order_client_id -> the now_ms at which
    /// this rung last issued a reject-free `confirm_order` re-query for that order`. Deliberately
    /// SEPARATE from the reject ladder's `confirm_issued_ms` so the early (reject-free) rung and the
    /// late (reject-capable) rung never share bookkeeping — the band guard keeps them on disjoint
    /// age windows, and each owns its own map. A still-stuck order is re-confirmed at most once per
    /// [`CoreConfig::inflight_confirm`] window (present + gap-not-elapsed ⇒ skip). Core-local, NOT
    /// serialized (like `confirm_issued_ms`); self-cleaning — pruned each sweep to only still-pre-ack
    /// order client ids, so it stays bounded. Empty forever unless the feature is enabled.
    pub(crate) inflight_confirm_last_ms: std::collections::HashMap<String, i64>,
    /// Per-mount fill ATTRIBUTION ledger (steal/core-per-mount-budget), parallel to `mounts` (same
    /// index), folded from attributed fills off the hot fold — see [`MountAttribution`]. All-zero
    /// for a mount that has never filled.
    pub(crate) mount_attribution: Vec<MountAttribution>,
    /// order client id -> mount index, written through [`Self::own_order_client_id`] for every order a mount's strategy
    /// mints (a submit, a fired stop, a budget-latch flatten) so a mount's fills AND resting orders
    /// are attributable back to it — cross-venue safe (order client ids are globally unique per core). Read by
    /// the fill-attribution fold ([`Self::dispatch_applied_fills`]) and the budget latch's scoped
    /// cancel ([`Self::latch_mount`]). Not fenced state and not journaled; it survives a restart
    /// through [`Self::order_owners`], keyed by mount ID. Empty when no strategy is mounted;
    /// BOUNDED by [`Self::order_client_id_terminal`], which retires an entry once its order has been terminal
    /// for [`ORDER_CLIENT_ID_PRUNE_LINGER_MS`].
    pub(crate) order_client_id_mount: std::collections::HashMap<String, usize>,
    /// The PRUNE QUEUE that bounds [`Self::order_client_id_mount`]: `(order client id, the ms at which its order was first
    /// observed TERMINAL)` in observation order, drained from the FRONT by
    /// [`Self::prune_terminal_order_client_ids`] once an entry has aged past [`ORDER_CLIENT_ID_PRUNE_LINGER_MS`].
    ///
    /// `order_client_id_mount` is written once per mount-minted order and used to be erased NEVER, so a
    /// long-lived maker session grew it without bound — and a restart made it WORSE, because
    /// [`crate::replay`]'s `fold_order_client_id_mounts` rebuilds order provenance across the WHOLE readable
    /// record set rather than just the tail. It is a pure routing/attribution index, so a long-dead
    /// order's entry is dead weight.
    ///
    /// **Why a LINGER queue and not an immediate remove** — see [`Self::note_terminal_order_client_id`].
    /// Core-local, never serialized, and empty on a core whose orders never terminalize.
    pub(crate) order_client_id_terminal: VecDeque<(String, i64)>,
    /// The ORDER-OWNERSHIP file ([`CoreConfig::order_owners`], taken at assemble and started there).
    /// Written through [`Self::record_owner`] only: an `own` per order a mount mints
    /// ([`Self::own_order_client_id`]), a `forget` per entry [`Self::prune_terminal_order_client_ids`] retires, a `ledger`
    /// per attributed fill of a live mount, an `unmount` per runtime unmount. `None` = nothing is
    /// recorded, byte-identical to a core without the file.
    pub(crate) order_owners: Option<crate::order_owners::OrderOwnerLog>,
    /// Restored ownership waiting for its MOUNT: entries the file named for a mount id no live slot
    /// carried at assemble (a profile mount removed, or a runtime mount not yet resurrected), keyed
    /// by that id. [`Self::mount_strategy_runtime`] binds an entry to the new slot when a mount of
    /// that id lands. Never written after assemble, so it only shrinks; the file keeps the entries
    /// (until their time-to-live) whether or not a mount ever claims them.
    pub(crate) pending_owners: std::collections::HashMap<String, OrderOwnerPending>,
    /// The mounts [`CoreConfig::mounts_awaiting_strategy`] parked at assemble, each on its placeholder
    /// strategy, until `AccountChange::AddMounts` brings its strategy (`add_mounts_hot`). Only shrinks.
    pub(crate) awaiting_strategy: Vec<Mount>,
    /// Mounts whose strategy arrived while their ACCOUNT did not run (listed NOT RUNNING: its venue not
    /// connected): each mounts when its account's engine joins (`add_engine_hot`).
    pub(crate) awaiting_account: Vec<Mount>,
    /// The orders a PREVIOUS session left resting, restored with their facts and bound to a live
    /// mount slot, keyed by order client id: the input of the reconcile pass's restore hook
    /// ([`Self::restore_orders_at_pass`], the ONE reader; its module doc has the verdicts). A pass
    /// SPENDS an entry (adopted, gone or filled while down), so the collection only ever holds
    /// orders no successful pass has judged yet. Filled by [`Self::restore_owned_order`] at assemble and
    /// when a runtime mount takes over a [`Self::pending_owners`] entry; shrunk by
    /// [`Self::prune_terminal_order_client_ids`] (one `forget`) and by a runtime unmount (that mount's rows).
    /// Empty when [`CoreConfig::restore_orders_off`] is on, when the journal restore answered
    /// instead of the file, and on a first boot. Entries with `None` facts are here too (an old
    /// line): the consumer decides what an entry without a venue can be matched by.
    ///
    /// A row says what the file REMEMBERS, not what is live: `tag` is the tag the order was
    /// submitted under, whether or not the tag registry still names it (a later order may have
    /// re-pointed it; [`Self::tag_for_order_client_id`] answers that).
    pub(crate) restored_orders: indexmap::IndexMap<String, crate::order_owners::OrderOwnerEntry>,
    /// The restore counters; see [`RestoreCounters`].
    pub(crate) restore_counters: RestoreCounters,
    /// The fail-open deadline (event-clock ms) of the restored-tag refusal in `drain_broker`: set at
    /// the FIRST submit refused because a restored order still holds its tag
    /// (`CoreThread::restored_tag_holds`), after which no refusal outlives it. `None` until then.
    /// A reconcile pass that never comes (reconcile off, fetch failing, an entry no pass can speak
    /// for) must not keep a maker from quoting for the life of the process.
    pub(crate) restored_tag_deny_until: Option<i64>,
    /// The restored order client ids the last pass HELD because the reconcile client was not told the symbol
    /// they rest on (`restore_orders_at_pass`): what keeps the hold a once-per-order edge and not a
    /// note per pass. Not state: never hashed, snapped or journaled; empty at boot.
    pub(crate) restore_held: std::collections::HashSet<String>,
    /// The restored order client ids the passes found UNPROVEN: absent from the report, where silence alone
    /// proves nothing (`crates/vike-core/src/runtime/restore.rs`'s module doc). Doubles as the
    /// once-per-order edge of [`Self::restore_held`]'s kind (an entry is announced when it is first
    /// inserted) and as the count of quiet passes the planner reads; an order client id a pass resolves (adopted,
    /// gone, filled) leaves it. Not state: never hashed, snapped or journaled; empty at boot, so a
    /// restart starts the count again (the ownership file still holds the entry).
    pub(crate) restore_unproven: std::collections::HashMap<String, RestoreUnprovenOrder>,
    /// The order client ids of operator's orders (a protective stop or a limit) a pass ADOPTED silently (the
    /// pseudo mount's entries, `restore.rs`'s `spend_operator_stop`): no mount owns them, so
    /// `order_client_id_mount`'s terminal prune never sees them, and this set is what lets their cancel forget
    /// the ownership file's line (`forget_ended_operator_stops`). Not state: never hashed, snapped or
    /// journaled; empty at boot.
    pub(crate) operator_stops: std::collections::HashSet<String>,
    /// The cancel-then-new amends in flight (`apply/amend.rs`): empty unless a venue that cannot change a
    /// resting order in place is being asked to. Driven at the drain-loop boundary (one `is_empty` read per
    /// drained batch, never per message). Not state: never hashed, snapped or journaled; empty at boot.
    pub(crate) amends: Vec<apply::amend::AmendInFlight>,
    /// The live legs a contingency fill sent a cancel to, watched for a DOUBLE FILL
    /// (`apply/contingency.rs`): empty unless a group resolved. Read at the drain-loop boundary (one
    /// `is_empty` per drained batch, never per message). Not state: never hashed, snapped or
    /// journaled; empty at boot.
    pub(crate) contingency_races: Vec<apply::contingency::ContingencyRace>,
    /// The OUO legs' wanted sizes and driven fills between fills (`apply/contingency.rs`'s
    /// `ContingencyOuoLegs`): empty unless a ladder or another OUO group rests. Not state: never hashed,
    /// snapped or journaled; at boot only the restored legs' fills are counted driven.
    pub(crate) ouo: apply::contingency::ContingencyOuoLegs,
    /// The `(venue, kind)` pairs a PAPER engine was let through where the venue's own row would have
    /// refused the reduce-only kind (`CoreThread::note_paper_admit`): what keeps the note that says so
    /// a once-per-pair line and not one per order. Bounded by the capability table (at most the
    /// roster's venues times five kinds, and only a row that lists fewer reduce-only kinds than it
    /// accepts ever adds one: two today). Not state: never hashed, snapped or journaled; empty at boot.
    pub(crate) paper_noted: std::collections::HashSet<(String, String)>,
    /// Per-mount resolved [`MountBudget`] (steal/core-per-mount-budget), parallel to `mounts`, copied
    /// from [`CoreConfig::mount_budgets`] at assemble. `None` for a mount with no budget.
    pub(crate) mount_budget: Vec<Option<MountBudget>>,
    /// Per-mount `(venue, symbol)` (steal/core-per-mount-budget), parallel to `mounts`, captured at
    /// assemble so the budget sweep + the snapshot view price a mount's ledger without depending on a
    /// transiently-taken (`None`) mount slot.
    pub(crate) mount_vs: Vec<(String, String)>,
    /// Per-mount DELAY LABEL of its own `(venue, symbol)` stream (decision 0140), parallel to
    /// `mounts`. Written ONLY by the `Ingest::DataDelay` arm (and seeded from
    /// [`Self::data_delays`] when a mount arrives at runtime); each strategy dispatch COPIES it into
    /// `LiveBroker::data_delay`, an index into this vector and nothing more, so no per-tick lookup.
    /// `None` = no label stated. Information, never a gate.
    pub(crate) mount_delays: Vec<Option<vike_model::StreamDelay>>,
    /// Per-mount INPUTS (decision 0271's live half, `runtime/inputs.rs`), parallel to `mounts`: the series
    /// its strategy reads by name and their closed bars, opened at mount. `None` for a mount that reads
    /// none (every mount before that record) and for a tombstone. Each strategy dispatch lends the set to
    /// `LiveBroker::inputs` by `Arc::clone`, an index into this vector and nothing more.
    pub(crate) mount_inputs: Vec<Option<MountInputs>>,
    /// Per-mount CLOSED-TRADE ledger (`runtime/closed_trades.rs`), parallel to `mounts`: every fill delivered
    /// to the mount, booked by the simulator's ledger before the strategy hears it. `None` before the mount's
    /// first fill (an empty ledger) and for a tombstone. Each strategy dispatch lends it to
    /// `LiveBroker::closed_trades` by `Arc::clone`; a broker lives one dispatch, so the next booking's
    /// `Arc::make_mut` copies nothing.
    pub(crate) mount_closed: Vec<Option<Arc<vike_analytics::closed_trades::TradeClosedLedger>>>,
    /// The bar clock each mount's [`Self::mount_closed`] was resumed with (`runtime/closed_trades.rs`'s "A
    /// restart"), parallel to `mounts`: what a `Snapshot` carries for a mount whose series holds no bar yet (the
    /// datahub's history has not reached it), so a `Snapshot` written before it keeps the clock the tail counts from.
    pub(crate) mount_closed_clock: Vec<Option<vike_journal::JournalSnapshotBarClock>>,
    /// What the closed-trade archive holds of each mount's [`Self::mount_closed`] rows, parallel to `mounts`
    /// (`runtime/closed_trades.rs`'s "A restart"): a `Snapshot` carries only the rows after it.
    pub(crate) mount_archive: Vec<super::closed_trades::TradeClosedMountArchive>,
    /// The closed-trade ARCHIVE's writer beside the journal (`vike_journal::TradeClosedArchive`), opened with the journal;
    /// `None` on a core with no journal, and on one whose archive did not open (an ERROR: its rows then ride every
    /// `Snapshot`). Dropped with the core, which lets its thread finish what is queued.
    pub(crate) closed_archive: Option<vike_journal::TradeClosedArchive>,
    /// The journal's closed-trade ledgers (`CoreConfig::closed_trades`, the tail booked on them) for a mount id
    /// no slot has taken yet: each slot takes its own at assembly and a runtime mount at its door, and every
    /// `Snapshot` carries what is left. Ordered, so a `Snapshot`'s bytes do not depend on a hash.
    pub(crate) pending_closed:
        std::collections::BTreeMap<String, super::closed_trades::TradeClosedPendingLedger>,
    /// The order client ids of the account-wide margin call's closes (`watchdog/risk.rs`): a LIQUIDATION of the
    /// positions the mounts hold, which the mounts' closed-trade ledgers book (`closed_trades.rs`'s "What is
    /// booked"). Core-local, never snapped; an entry leaves when its order ends.
    pub(crate) liquidation_order_client_ids: std::collections::HashSet<String>,
    /// The mount bar steps a SHARED CLOSE holds (decision 0271, `runtime/shared_close.rs`), oldest first.
    /// Empty on every core whose mounts read no input: one `is_empty()` per bar message and per drained batch.
    pub(crate) shared_closes: Vec<super::shared_close::ParkedClose>,
    /// A sender to this core's own ingest lane that does not keep it open: a held shared close asks for a wake
    /// at its grace's end through it, so an idle core still releases it on time. `None` on a core built
    /// without `spawn_core` (a white-box rig), whose next message reaches the boundary instead.
    pub(crate) self_wake: Option<mpsc::WeakSender<Ingest>>,
    /// Every series key a mount input was opened on, in this process, never shrunk: the bar lane files a
    /// bar of one into the inputs that read it, and one no mount hears goes nowhere else
    /// (`CoreThread::input_only`), even after its mount left, because the host keeps the series open to
    /// the teardown. EMPTY on every core whose mounts read no input: one `is_empty()` per bar message.
    pub(crate) input_keys: Vec<SeriesKey>,
    /// The delay label each `(venue, symbol)` last reported, `venue -> symbol -> label` (two levels
    /// so a lookup takes borrowed `&str`s and allocates nothing). Written only by the
    /// `Ingest::DataDelay` arm; read once per submit, where `lower_submit` stamps
    /// `OrderRequest::data_delay` (one `is_empty` branch on a core whose feeds label nothing, which
    /// is every core but an IBKR one today), and when a runtime mount arrives. ⚠ The stream is NOT a
    /// key: ONE labelling stream per symbol is assumed (`runtime/data_delay.rs`'s module doc, "One
    /// labelling stream per symbol"); a second one on a symbol would be last-writer-wins.
    pub(crate) data_delays: std::collections::HashMap<
        String,
        std::collections::HashMap<String, vike_model::StreamDelay>,
    >,
    /// **Per-mount ENGINE INDEX**, parallel to `mounts` — the mount's declared account resolved to
    /// one of this core's engines, ONCE, at assemble ([`assemble_core`]'s `resolve_mount_engines`).
    ///
    /// It is the ONE answer to "which book does this mount trade", and every strategy lane reads it
    /// through [`Self::engine_of_mount_venue`] rather than re-deriving one from a venue string.
    /// That is the structural half of the account seam: a venue-keyed lookup can only ever resolve
    /// a venue's DEFAULT account, so as long as the lanes asked that question a second account was
    /// unaddressable no matter what a mount declared, and no runtime test could have said so.
    ///
    /// Identical to the old `engine_index_for_route_key(sole_account_of(mount.venue)).unwrap_or(0)`
    /// for every account-less mount, which is every mount that existed before the field.
    pub(crate) mount_engine: Vec<usize>,
    /// Per-mount budget LATCH (steal/core-per-mount-budget), parallel to `mounts`: `true` once that
    /// mount breached its budget and was latched liquidate-only. Fire-once — a re-breach on a
    /// still-open residual never re-cancels or re-judges the budget; read by [`Self::drain_broker`] (discards a
    /// latched mount's intents, the liquidate-only enforcement) and [`Self::sweep_mount_budgets`], which
    /// RE-OFFERS the flatten of a latched mount that is still open and has no close in flight
    /// ([`Self::reoffer_mount_flatten`]). Journaled on every `Snapshot` (`JournalSnapshotMountAttribution::latched`) and put
    /// back by the boot restore for a mount that still has an active budget. All `false` when no budget
    /// is set — the byte-identical path.
    pub(crate) mount_latched: Vec<bool>,
    /// Per-mount LOSS-BUDGET BASE (decision 0239), parallel to `mounts`: the net realized PnL (realized less fees)
    /// the operator's un-latch (`Command::UnlatchMount`, [`Self::unlatch_mount`]) found the mount at. The loss arm
    /// of the budget judges `-(realized net - base + unrealized)`, so a mount that was un-latched has the whole of
    /// its `max_loss` to spend again instead of re-latching on the history that latched it. `0.0` for every
    /// mount that was never un-latched (the judgement every mount always had). Read only by
    /// [`Self::sweep_mount_budgets`] and written only by the un-latch and the boot restore; journaled on every
    /// `Snapshot` (`JournalSnapshotMountAttribution::loss_base`, absent while zero).
    pub(crate) mount_loss_base: Vec<f64>,
    /// The latched mounts whose re-offered flatten has been said (`CoreThread::reoffer_mount_flatten`):
    /// what makes the line once per episode, not one per 30 s sweep. An entry ends when the mount is
    /// flat. Core-local, never journaled or hashed; empty without a latched mount.
    pub(crate) mount_reoffer_said: std::collections::BTreeSet<usize>,
    /// Per-mount DECLARED extra symbols (`Mount::symbols`), parallel to `mounts`. Empty
    /// for every single-symbol mount, which is what makes the drain byte-identical.
    pub(crate) mount_symbols: Vec<Vec<MountLeg>>,
    /// Per-mount contract values of a DECLARED mount's symbols, parallel to `mounts`: its own symbol
    /// and every leg, each with the value the leg's account books it by and the grid its engine's gate
    /// holds for it (`None` without a row of its own). Empty for a mount that declared nothing.
    /// Written only by [`Self::rebuild_leg_values`] (cold); every `LiveBroker` of a declared mount
    /// borrows its row by `Arc::clone`.
    pub(crate) mount_leg_values: Vec<Arc<[crate::runtime::broker::LegFacts]>>,
    /// Per-mount DECLARED legs as the published mount row names them (`MountView::legs`): `(venue,
    /// symbol)`, the venue resolved by [`Self::leg_venue`], parallel to `mounts`; `None` for a mount that
    /// declared nothing. Written only by [`Self::rebuild_leg_values`] (cold); a row build clones the `Arc`.
    pub(crate) mount_leg_rows: Vec<MountLegRow>,
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
    /// Per-mount wall-clock [`crate::schedule::MountSchedule`] (steal/core-live-scheduler), parallel
    /// to `mounts` (same indices), resolved from [`CoreConfig::mount_schedules`] at assemble. Empty
    /// for a mount with no schedule. Checked at EVERY drain-loop boundary pass
    /// ([`Self::drive_schedule`], the readiness-gate pattern — no timer-wheel entry), NEVER per
    /// message.
    pub(crate) mount_schedule: Vec<MountSchedule>,
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
    pub(crate) mount_rows: MountViewCache,
    /// Bumped by [`Self::recompute_mount_gates`] — the one place a runtime mount or unmount ends —
    /// so the cache above can tell a CHANGED LIST (rows added, removed, renamed) from changed
    /// numbers. A cache built under another epoch is never reused. Starts at `0` beside an EMPTY
    /// cache: a core assembled with mounts has live slots that list cannot match, so its first
    /// publish builds the real one.
    pub(crate) mount_epoch: u64,
}

/// Fold-thread-only record of one held reconcile alert (Task 17): the full proposed events
/// (folded verbatim on confirm) plus the metadata `Self::reconcile_block` needs to build its
/// `ReconcileAlertView` projection, and the `route_key` needed to re-resolve the routing engine index
/// at confirm time (indices are NOT stored directly — cheap to recompute via
/// `engine_index_for_route_key`, and robust if that ever became less than perfectly stable).
///
/// ⚠ **`venue` and `route_key` are stored SEPARATELY, and this record is why.** An operator confirm
/// arrives minutes to hours after the pass that raised the alert, and `Self::confirm_reconcile` folds
/// the held `proposed_events` into whichever engine this record resolves — so a single string here
/// would be a ROUTING decision made at pass time, kept, and re-applied later under the name of a
/// canonical venue. `venue` keeps the jobs that are genuinely about the EXCHANGE (the
/// operator-facing label, the capability row), so collapsing the two would force one of them to be
/// wrong for a second account of one exchange: route by `venue` and the approved events fold into
/// the FIRST account's book, or label by `route_key` and the operator-facing venue becomes a string
/// `vike_model::VENUES` does not contain.
///
/// ⚠ **The DEDUP IDENTITY below is NOT one of `venue`'s jobs, and this doc said it was.** It
/// carries BOTH strings (`recon_held::ReconcileHeldId`), because a `dedup_key` names an instrument and a
/// side and never a book: fifty accounts of one exchange holding `position:BTCUSDT:Both` produced
/// fifty EQUAL identities, so the first account's row absorbed all fifty — refreshed with whichever
/// leg ran last while its `route_key` still named the first account — and one confirm folded
/// another account's fills into the wrong book. Identity is per ACCOUNT; labelling is per venue.
pub(crate) struct ReconcileHeldAlert {
    /// The CANONICAL exchange id (`vike_model::VENUES`) — the operator-facing label. NEVER the
    /// routing input, and (since the identity below learned to carry the account) never the whole
    /// of this row's dedup identity either.
    pub(crate) venue: String,
    /// The ENGINE this alert's `proposed_events` fold into on confirm — carried verbatim from the
    /// pass that raised it (`ReconcileReports::route`), so the confirm lands where the pass would
    /// have. Equal to `venue` for a venue's SOLE account — which is every alert a box with no
    /// `[accounts]` table can raise — and the labelled account's own key otherwise.
    pub(crate) route_key: String,
    pub(crate) kind: ReconcileDivergenceKind,
    pub(crate) detail: String,
    pub(crate) proposed_events: Vec<Event>,
    /// Order-loss recovery payload (recon `JournalDivergence`): venue orders to RE-REGISTER into
    /// local state on confirm, via `Engine::reregister_orders` (insert-only). Empty for
    /// every other alert.
    pub(crate) recover_orders: Vec<vike_model::OrderStatusReport>,
    /// `OrphanLocalPosition` only: the LOCAL position the venue never reported, which a confirm
    /// drops at the position's own average price (`CoreThread::confirm_reconcile`). `None` for every
    /// other kind. Refreshed with the row, because the local qty may have moved between passes.
    pub(crate) orphan_position: Option<vike_execution::recon::ReconcileOrphanPosition>,
    /// A `PositionDrift` / `PositionOnlyExternal` held because its venue row has no usable price and
    /// is not flat: no confirm may book it (`CoreThread::confirm_reconcile` REFUSES). Refreshed with the row.
    pub(crate) confirm_refused: bool,
    /// This row's DEDUP IDENTITY — a later pass re-raising the same divergence REFRESHES this row
    /// in place (same confirm id, freshened payload) instead of appending a new one.
    ///
    /// ⚠ This used to be `vike_execution::recon::ReconAlert::dedup_key`, an `Option` that only SOME
    /// kinds set, and an alert without one appended a row EVERY PASS with a fresh id. Nothing caps,
    /// prunes or retains this store — `shift_remove` fires only on an operator confirm — so an
    /// un-keyed divergence that nothing heals grew it without bound: measured on the CI box, alert id
    /// 337 at 00:00 → 815 at 06:59 on 2026-08-25, ~65 rows/hour ≈ 1,560/day, every one of them the
    /// same two divergences, each holding its own `proposed_events` and each projected into every
    /// published `CoreSnapshot` by `Self::reconcile_block`. `recon_held::ReconcileHeldId` is the stable name
    /// those alerts never carried; where a `dedup_key` DOES exist it still decides alone, so this
    /// is a strict generalization of the old match rather than a different rule.
    pub(crate) identity: recon_held::ReconcileHeldId,
    /// The sequence (`reconcile_pass::ReconcilePassMark::seq`) of the last pass of this row's account that raised
    /// it. A row whose account has moved on to a later pass that COVERED its kind and did not raise
    /// it is stale: removed by `CoreThread::prune_stale_reconcile`, refused by `confirm_reconcile`.
    pub(crate) raised_sequence: u64,
}
