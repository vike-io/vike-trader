//! The core's configuration surface: `CoreConfig` (+ its `Default`), `JournalConfig` and the hook type aliases.

use super::*;

/// Opt-in write-ahead command journal (spec 2026-07-10-journal-replay-and-data-freshness §A).
/// `None` (the [`CoreConfig`] default) = today's behavior, ZERO overhead — no journal is opened
/// and the fold path is byte-identical. `Some` opens a [`vike_journal::CommandJournal`] at
/// construction and write-ahead-journals every exec-lane message before it folds (see
/// [`CoreThread::dispatch`]).
#[derive(Debug, Clone)]
pub struct JournalConfig {
    /// directory the segment files live in (created if absent).
    pub dir: std::path::PathBuf,
    /// segment sizing + flush cadence of the underlying journal.
    pub file: vike_journal::JournalFileConfig,
    /// write a full-state `Snap` record every N appended records (plus always on Shutdown / the
    /// senders-dropped exit). `0` would snapshot on every record.
    pub snapshot_every: u64,
}

impl JournalConfig {
    /// A journal at `dir` with the standard segment/flush/snapshot cadence — the same defaults the
    /// `[sinks.journal]` profile section uses (64 MiB segments, flush every 256 records, snapshot
    /// every 1024). The one-liner a binary reaches for when enabling the journal from a single
    /// directory knob (e.g. `VIKE_JOURNAL_DIR`) rather than a full [`crate::RunProfile`].
    pub fn at(dir: impl Into<std::path::PathBuf>) -> Self {
        JournalConfig {
            dir: dir.into(),
            file: vike_journal::JournalFileConfig {
                segment_bytes: 64 * 1024 * 1024,
                flush_every: 256,
            },
            snapshot_every: 1024,
        }
    }
}

/// Runtime configuration. Defaults follow the plan; the two hooks are for tests/harnesses.
pub struct CoreConfig {
    pub seed_cash: f64,
    /// coalesced snapshot publish interval while busy (idle always publishes)
    pub snapshot_interval: Duration,
    pub recent_events_cap: usize,
    pub ingest_capacity: usize,
    /// max messages folded per wakeup before a publish check (keeps latency bounded)
    pub batch_max: usize,
    /// injected [`Clock`] for `submit_order` now_ms + persistence timestamps —
    /// `LiveClock` (wall time) by default; tests inject `TestClock` or a bare closure
    /// (any `Fn() -> i64` is a `Clock`)
    pub clock: Box<dyn Clock + Send>,
    /// GUI wake (egui `request_repaint`); called on every publish — coalesced while busy, but also
    /// on every idle transition, so per event on a sporadic feed
    pub repaint: Option<Box<dyn Fn() + Send>>,
    /// FAULT-INJECTION TEST HOOK: panic when a bare FillEvent with this trade_id arrives
    pub panic_on_trade_id: Option<String>,
    /// LATENCY-HARNESS HOOK: called when a message is dequeued, immediately BEFORE
    /// dispatch — measures the pure send→core hop; dispatch cost is measured separately
    pub on_dequeued: Option<DequeuedHook>,
    /// R7: a strategy mounted on one bar series (paper or live — same path)
    pub strategy: Option<StrategyMount>,
    /// Opt-in stuck-order watchdog (audit C3), stage 1 of the confirm-grace ladder: an order with
    /// no venue ack within this is flagged un-acked — the sweep SOFT-WARNS (it does NOT reject here).
    /// `None` = disabled (default; behavior unchanged — no watchdog thread is spawned, no
    /// `Ingest::Watchdog` is ever enqueued). Set a CONSERVATIVE value (e.g. 30s) for reconcile-less
    /// venues; a too-short value merely starts the ladder early — the hard reject is still gated on
    /// [`Self::submit_ack_confirm_grace`] elapsing afterward, so a slow-but-real venue ack (or the
    /// adapter's own post-submit REST confirm) still wins.
    /// Set this to at LEAST the adapter's own submit + ambiguous-requery budget
    /// (`vike_bridge_core::resolve_ambiguous_submit`): the timeout backstops the adapter's REST
    /// confirm, it does not race it — a too-short value that fires while the adapter's re-query is
    /// still in flight defeats the ladder's whole point (the grace can absorb some of this, but the
    /// timeout itself should not undercut the adapter's known worst-case confirm latency).
    ///
    /// HARD LOWER BOUND (b): `submit_ack_timeout` MUST exceed the venue's worst-case
    /// order-visibility latency — the gap between the venue accepting an order and that order
    /// becoming queryable/streamed. STAGE 1 issues an ACTIVE confirm (a status re-query) the instant
    /// this elapses; if the venue has accepted the order but not yet indexed it, that first confirm
    /// can come back empty and, on a reconcile-less venue, be mistaken for "never landed". The grace +
    /// the in-flight-confirm guard below absorb this, but keeping the timeout above the visibility
    /// latency is the first line of defence against a phantom reject of a just-accepted order.
    pub submit_ack_timeout: Option<Duration>,
    /// Confirm-grace window (audit C3 hardening), stage 2 of the ladder: the ADDITIONAL time after
    /// `submit_ack_timeout` before the watchdog's LAST-RESORT synthesized `OrderRejected`. The order
    /// is terminalized only if it is STILL pre-ack (`Initialized`/`Submitted`) at the reject deadline
    /// — i.e. no authoritative `OrderAccepted`/`OrderFilled` (from a slow WS ack OR the adapter's own
    /// REST re-query, `vike_bridge_core::resolve_ambiguous_submit`) folded meanwhile. This is what
    /// stops the backstop from racing a real venue ack into a phantom reject that would strand a live
    /// position. Ignored when `submit_ack_timeout` is `None`.
    ///
    /// IN-FLIGHT-CONFIRM GUARD (the tuning-independent hardening): stage 1 does not merely WAIT — it
    /// issues an active `confirm` when the order enters the grace. If that confirm is still in flight
    /// when the plain reject deadline (`created + submit_ack_timeout + grace`) passes, the backstop is
    /// DEFERRED one ADDITIONAL grace window, to `created + submit_ack_timeout + 2·grace`. So a confirm
    /// gets up to `2·grace` to land (a real `Accepted`/`Filled` at any point moves the order out of
    /// pre-ack and cancels the reject entirely). This converts "the reject must not fire before the
    /// confirm returns" from a knife-edge into a guarded window — see [`CoreThread::sweep_stuck_orders`].
    ///
    /// HARD LOWER BOUND (a): keep `2·submit_ack_confirm_grace > submit_ack_timeout/2 + N·requery_timeout`
    /// (equivalently `submit_ack_confirm_grace` alone should exceed `submit_ack_timeout/2 +
    /// N·requery_timeout` for margin), where `requery_timeout` is the adapter's per-re-query REST
    /// timeout (`~5s`, `vike_bridge_core::http::blocking_agent_with_timeout`) and `N` is the number of
    /// sequential re-queries the confirm makes: N=2 for Bybit (realtime→history, worst case ~10s), N=1
    /// for OKX. The `submit_ack_timeout/2` term is the watchdog tick jitter (the timer fires every
    /// `submit_ack_timeout/2`, so the active confirm can be issued up to one tick late). Under-sizing
    /// the grace lets the reject deadline fall inside the confirm's own round-trip; the guard's
    /// `2·grace` window is the slack that keeps a slow-but-real confirm from being clobbered.
    ///
    /// A NON-TRIVIAL grace IS the safety margin: `Duration::ZERO` collapses the ladder back to
    /// immediate-reject-at-`submit_ack_timeout` (both the grace AND the guard become no-ops) and
    /// reintroduces exactly the phantom-reject risk this window exists to remove. Keep it comfortably
    /// above the venue's ack-jitter + the adapter's re-query RTT; the default (15s) already clears
    /// Bybit's ~10s two-hop confirm with the `2·grace` guard.
    pub submit_ack_confirm_grace: Duration,
    /// Opt-in margin-call watchdog (accounting-upgrade Phase B; LEAN DefaultMarginCallModel
    /// semantics). Swept once per CLOSED bar of the engine's own series (marks fresh, off
    /// the event fold). Warning → recent-events ring; liquidation submits reduce-only
    /// market orders through the NORMAL gate path. `None` (default) = disabled.
    pub margin_call: Option<vike_exec::MarginCallConfig>,
    /// Opt-in drawdown latch (audit exec#4): the max tolerated fractional drawdown from the
    /// high-water-mark of the DAEMON'S OWN equity curve before the core LATCHES into
    /// liquidate-only (`trading_state = Reducing`, so the existing RiskGate then denies
    /// risk-increasing orders and permits only reduce-only). Folded on the SAME per-CLOSED-bar
    /// cadence and through the SAME resolver as the margin-call watchdog (marks fresh, off the
    /// event fold). E.g. `Some(0.20)` = latch at 20% drawdown. `None` (default) — and any value
    /// `<= 0.0` — DISABLES the latch: no HWM is folded, behavior is byte-identical to today. LATCH
    /// semantics: once tripped it STAYS Reducing even if the curve later recovers (never
    /// auto-un-latched — un-latching is a deliberate manual `Command::SetTradingState`).
    ///
    /// ⚠ **A FRACTION OF WHAT.** Of `Σ seed_of(engine) + Σ resolved_own_pnl(engine)` at its peak —
    /// the operator's CONFIGURED capital plus the daemon's own realized+unrealized P&L, and
    /// deliberately NOT the observed account equity, which on an `Authoritative` block is a
    /// venue wallet for an account this daemon may not own alone. [`CoreThread::sweep_drawdown_latch`]
    /// is the authority and carries the the CI box measurement that forced it; on an all-`Delta` core
    /// the two are the same number. A positive `seed_cash` is therefore load-bearing whenever this
    /// is `Some` — `RunProfile::validate` requires it.
    pub max_drawdown: Option<f64>,
    /// Phase C (opt-in, Rust-native): also check the ConditionalBook on every tick of the
    /// engine's series (quote mid / trade px) — intra-bar stop/trailing triggering the
    /// Python oracle's bar-close book lacks. Default false = oracle-faithful bar-close only.
    pub conditionals_on_ticks: bool,
    /// Phase D (opt-in): ADDITIONAL strategy mounts beyond `strategy` — one engine, N
    /// (venue, symbol, interval) mounts over the one multi-symbol Account. Symbols beyond
    /// the engine's primary must be listed in `ExecutionEngine::extra_symbols` so their
    /// venue events fold. Empty (default) = single-mount behavior, unchanged.
    pub extra_mounts: Vec<StrategyMount>,
    /// Opt-in write-ahead command journal (spec §A). `None` (default) = no journal opened,
    /// zero overhead — the fold path is byte-identical to today.
    pub journal: Option<JournalConfig>,
    /// Resume the [`vike_exec::ClientOrderIdGenerator`] from a persisted `(session, seq)` on
    /// restart (so replayed/new orders keep unique, non-colliding ids across a restart). `None`
    /// (default) = a fresh random session, today's behavior.
    pub coid_session: Option<(String, u64)>,
    /// This instance's declared origin claim (`config.instance_origin`), stamped into the coid
    /// SESSION of a FRESH generator so every id this core mints names the deployment that placed
    /// it. `None` (default) = today's ids exactly — `vike_model::instance_origin`'s module doc
    /// argues the format and what it buys.
    ///
    /// ⚠ **Consulted only when [`Self::coid_session`] is `None`.** A restart RESUMES the persisted
    /// session verbatim, because that session is what the journal replays and the determinism
    /// fence compares — re-stamping it from configuration would make a replay's ids depend on the
    /// box replaying them. The consequence an operator has to know: turning the origin on (or
    /// changing it) does NOT reach the wire until the core starts a fresh session.
    /// [`assemble_core`] logs a WARN naming both tags when a resumed session disagrees with the
    /// configured one, so the gap is visible rather than inferred, and
    /// `docs/ops/double-live-instances.md` says what to do about it.
    pub instance_origin: Option<vike_model::InstanceOrigin>,
    /// Resume the emulated-conditional arm-id counter ([`CoreThread::mint_arm_id`]) on restart,
    /// the arm-side twin of [`Self::coid_session`]. `0` (default) = a fresh session's counter.
    ///
    /// This matters BECAUSE `coid_session` is resumed: an arm id is `{coid_session}a{arm_seq}`, so
    /// a restart that restores the session but restarts the counter at 0 would re-emit ids the
    /// pre-restart run already used — and resume APPENDS to the same journal directory, so both
    /// live in ONE log. And since emulator PR-2 the arm id is the DISARM key
    /// ([`vike_exec::OrderIntent::DisarmConditional`]), so a duplicate would let a disarm target
    /// the wrong arm. [`crate::replay::RestoredState::arm_seq`] supplies the resume value (the
    /// latest `Snap`'s stamped counter, prune-safe — see that field for the pre-v7 fallback); a
    /// caller that does not resume keeps today's behavior.
    pub arm_seq: u64,
    /// Re-arm the emulated-conditional books on restart (emulator PR-3, re-arm-on-restore): each
    /// entry is one armed conditional — its minted `arm_id` and the terms the book held at the
    /// restore base's `Snap` (a TRAILING arm's ratcheted extreme included) — seeded into
    /// `conditional_books` ONCE at [`assemble_core`], in order (fire order is insertion order).
    /// [`crate::replay::RestoredState::conditionals`] supplies the value on a crash restart, next
    /// to `coid_session`/`arm_seq`; empty (default) = today's behavior, a core that starts with
    /// empty books (and a pre-v8 journal's Snap carries no books, so it restores empty — never an
    /// error). Zero hot-fold cost: consumed entirely at assembly.
    ///
    /// ⚠ **DECLARED RESIDUAL — a RESTORED arm loses its ACCOUNT.** `SnapConditional` carries the
    /// arm's `(venue, symbol)` terms and no route key, so a seeded arm gets no
    /// [`CoreThread::cond_engine`] entry, and its fire takes the historical payload route with no
    /// account on the request. On a venue with ONE engine, that route reaches the one book, which
    /// is correct. On a venue with SEVERAL accounts, the Submit arm's ambiguity gate refuses it
    /// (a payload naming the exchange and no account). So a protective stop armed before a restart
    /// and triggered after it reaches NO book: the refusal lands in the recent-events ring naming
    /// the candidate accounts, and the position the stop was armed to close stays open and
    /// unprotected. The account-scoped mass-cancel reads the same fact
    /// ([`CoreThread::armed_engine`]'s `None`) and leaves such an arm under every label. Closing
    /// the residual means putting the route key on `SnapConditional`, which is a durable
    /// journal-shape change; the live arm is exact today.
    ///
    /// (This said the fire "fires onto the default account" until 2026-09-26. That was true before
    /// the ambiguity gate and has not been true since. `mount_account_tests`'
    /// `a_labelled_mass_cancel_leaves_a_restored_arm_it_cannot_attribute_to_the_named_account`
    /// asserts the refused fire.)
    pub conditionals: Vec<vike_journal::SnapConditional>,
    /// Re-seed the live-runtime OTO/OCO contingency book on restart (live-runtime OCO/OTO), the
    /// contingency twin of [`Self::conditionals`]: each entry is one resting linked leg — its
    /// linkage (`parent`/`linked`/`active`) and, for a HELD exit, the resolved order the runtime is
    /// keeping OFF the venue until its parent fills. Seeded into `contingency`/`held_orders` ONCE at
    /// [`assemble_core`], in the captured insertion order (arm/cancel iteration order). Supplied on a
    /// crash restart by [`crate::replay::RestoredState::contingencies`]; empty (default) = today's
    /// behavior (a core that starts with no contingency state; a pre-v11 journal's Snap carries none,
    /// so it restores empty — never an error). Zero hot-fold cost: consumed entirely at assembly.
    pub contingencies: Vec<vike_journal::SnapContingency>,
    /// Re-seed the per-mount fill-ATTRIBUTION ledgers on restart (multi-mount durability, gap D):
    /// one row per mount, resolved onto its slot by `mount_id`
    /// ([`crate::strategy_state::mount_id_with`]) at [`assemble_core`], so a re-ordered mount list
    /// still restores onto the right mounts and a row naming no mounted slot is simply ignored
    /// (inert, never an error). Supplied on a crash/clean restart by
    /// [`crate::replay::RestoredState::mount_attr`] (the latest `Snap`'s captured ledgers).
    ///
    /// Empty (default) = today's behavior: every mount starts with a zeroed ledger. Zero hot-fold
    /// cost — consumed entirely at assembly.
    pub mount_attr: Vec<vike_journal::SnapMountAttr>,
    /// Re-seed the coid -> mount ATTRIBUTION MAP on restart (multi-mount durability, gap D): each
    /// entry is `(client_order_id, mount_id)` for an order some mount minted before the restart, so
    /// a resting order that fills AFTER it still books into its originating mount's ledger (and is
    /// still cancellable scoped-to-that-mount by the budget latch). Resolved onto mount slots by
    /// `mount_id` at [`assemble_core`], exactly like [`Self::mount_attr`]; an entry naming no
    /// mounted slot is ignored.
    ///
    /// Supplied by [`crate::replay::RestoredState::coid_mounts`], which folds the journal's own
    /// `StrategySubmit` provenance records — the origin has always survived to the journal, nothing
    /// read it back. Empty (default) = today's behavior: post-restart fills on pre-restart orders
    /// are unattributed (they land in the residual row).
    pub coid_mounts: Vec<(String, String)>,
    /// Opt-in mmap counter mirror (audit co9): a file the key runtime health counters
    /// (`conflated_market_drops`, `rejected_commands`, `exec_db_*`, `stranded_terminal_drops`, …)
    /// are copied into at every snapshot publish, so an external `vike_stat` process can read
    /// them without attaching to a headless trader. `None` (default) = no file is opened and the
    /// publish path is byte-identical to today. `Some` adds the copy to every publish — never to
    /// `handle()` itself, but publish also runs on every idle transition, so on a sporadic feed the
    /// copy is per event and lands inside the gated core hop. A binary may source the path from an
    /// env var (env reads stay in binaries — see [`crate::counters`]).
    pub counters_path: Option<std::path::PathBuf>,
    /// Opt-in strategy-state persistence sidecar (portfolio-observer PR-4 T2): when set, each
    /// mount's deterministic id ([`crate::strategy_state::mount_id_of`]) resolves to
    /// `<state_dir>/<mount_id>.json`. `assemble_core` loads that sidecar (if present) into the
    /// mount's strategy (`Strategy::load_state`) before it ever runs; a clean shutdown saves the
    /// strategy's current durable state back out (`Strategy::save_state`), best-effort. `None`
    /// (default) is zero overhead: no path is ever built, no file is ever touched, and mount
    /// assembly/teardown stay byte-identical to today.
    pub state_dir: Option<std::path::PathBuf>,
    /// Timer-armed PERIODIC strategy-state save (portfolio-observer PR-4 T3), gated behind
    /// `Some` AND [`Self::state_dir`] also being `Some` (see
    /// [`CoreThread::maintain_state_save_timer`]): while at least one mount exists, on this
    /// cadence, every live mount's durable state is re-saved
    /// ([`CoreThread::save_all_strategy_state`]) — the same best-effort
    /// `Strategy::save_state` -> `write_json_atomic` sidecar write [`Self::state_dir`]'s doc
    /// describes for the clean-shutdown save, just on a running cadence instead of (in addition
    /// to) shutdown only. Mirrors [`Self::equity_sample`]'s timer machinery almost exactly; the
    /// one difference is the arm condition does NOT check open positions — a strategy's durable
    /// state (a breaker's trip count, an A-S accumulator, ...) matters whether the book is flat
    /// or not. Entirely COLD-path (drain-loop boundary arm/disarm + a self-rescheduling
    /// [`DeadlineTimerWheel`] entry — never the per-message fold). `None` (default), OR a `Some`
    /// with [`Self::state_dir`] left `None`, is zero overhead: no timer is ever armed and the
    /// fold path is byte-identical to a save-timer-free runtime.
    pub state_save: Option<Duration>,
    /// Timer-armed equity sampler (portfolio-observer PR-3), gated behind `Some`: while ANY
    /// position is open (see [`CoreThread::any_position_open`]), on this wall-clock cadence,
    /// resolve per-venue + cross-venue equity (`ExecutionEngine::resolve_equity`) and hand the
    /// batch to [`Self::on_equity_sample`]. Entirely COLD-path — the drain-loop boundary
    /// arm/disarm plus a self-rescheduling [`DeadlineTimerWheel`] entry — NEVER the per-message
    /// fold. `None` (default) = zero overhead: the boundary check is a single
    /// `Option::is_some()` read, no timer is ever armed, and the fold path is byte-identical to
    /// a sampler-free runtime.
    pub equity_sample: Option<Duration>,
    /// The equity sampler's sink: the CALLER decides where a batch goes — persistence, display, or
    /// nowhere — through this closure. Fired with the full batch — one row per engine (primary +
    /// extras, registration order) plus a `"TOTAL"` cross-venue row (`py_sum` laws, matching
    /// `CoreSnapshot::equity_total`) — each time `equity_sample` fires. `None` (default) = no
    /// sink; the sampler still arms/fires when `equity_sample` is `Some`, but the finished batch is
    /// simply not delivered anywhere (set both together in practice).
    ///
    /// ⚠ This opened "Option B: vike-core takes NO vike-data dependency" until 2026-09-28, and the
    /// crate declares one: `vike-data` with DEFAULT features, the trait-only half
    /// ([`crate::CoreLaneSink`] implements its `LiveDataSink`). What keeps the sink a closure is
    /// the half that stayed true — the core holds no store and cannot open one. The concrete store
    /// and its record writer sit behind vike-data's `hist-datafusion` feature, which this crate's
    /// edge does not enable, and `docs/decisions/0084-only-the-datahub-touches-the-store.md` gives
    /// the store one reader, the datahub, and two writers, the backfill collectors and the recorder
    /// inside the datahub. Persisting a sample is the caller's to arrange.
    pub on_equity_sample: Option<EquitySampleHook>,
    /// Journal cross-check (#3): builds a [`vike_exec::recon::JournalView`] for a venue from the
    /// materialized Tier-2 exec log (unified-journaling #2). `Some` unlocks the three-way
    /// (local-vs-venue-vs-journal) reconcile so a persisted fill the live `Account` lost surfaces as
    /// a `JournalDivergence` alert. Called on the fold thread at reconcile time only (an OCCASIONAL
    /// command — a bounded lookback query, off the p99 hot path). `None` (default) = two-way
    /// reconcile, byte-identical — and `None` is what every live mount passes today: no root builds
    /// one (`crates/vike-core/src/journal_view.rs` carries why the last one went).
    ///
    /// ⚠ This ended "the store read lives in `vike-app-core` (which deps both vike-core and
    /// vike-data); vike-core cannot read the store itself" until 2026-09-28, and neither half
    /// holds. The READ is this crate's: [`crate::journal_view_from_store`] walks any
    /// `vike_data::HistStore` it is handed, and it is the call a hook here would make. What the
    /// core cannot do is OPEN a store — none is constructible from its tree, since its `vike-data`
    /// edge is DEFAULT-features, trait-only — and per
    /// `docs/decisions/0084-only-the-datahub-touches-the-store.md` every reader but the datahub
    /// reaches the store over the wire, so the store a hook reads is one its host constructed and
    /// handed in. Nor does the core choose the clock or the lookback the walk is scoped by. Those
    /// are why this is a closure a root builds.
    pub journal_view_provider: Option<JournalViewHook>,
    /// Resolver knobs the equity sampler AND `CoreSnapshot::build` both resolve prices through
    /// (threading deferred from PR-2's `ExecutionEngine::resolve_equity`). Permissive default
    /// (`PriceCfg::default()`: mark enabled, no freshness windows) is behavior-preserving for
    /// every caller that never touches this.
    pub price_cfg: PriceCfg,
    /// How long a REAL venue mark keeps ownership of the account mark slot (ms).
    ///
    /// The rule itself is NOT here and NOT in any lane: it lives in `Account::set_mark_from`,
    /// keyed by [`vike_exec::MarkSource`], which is the only door into the (private) `marks` map.
    /// This field is only the WINDOW, pushed onto every engine's `Account` at spawn — a knob, not
    /// a policy. See [`vike_exec::MarkSource`] for the law and its consequences.
    ///
    /// Default 10s = ~10 missed ticks of a 1s venue mark stream. `0` disables ownership entirely
    /// (pure last-write-wins, the pre-law behavior).
    pub mark_staleness_ms: i64,
    /// How long a RECONCILE mark ([`vike_exec::MarkSource::ReconcileMark`]) keeps ownership of the
    /// account mark slot (ms) — the per-source twin of [`Self::mark_staleness_ms`] for the
    /// reconcile cadence, which samples the venue's mark far less often than a 1s stream.
    ///
    /// MUST exceed the deployment's `VIKE_RECONCILE_INTERVAL_MS` (default 60s): sized so a
    /// reconciled stream-less venue's slot stays owned by the reconcile mark CONTINUOUSLY between
    /// passes instead of expiring each minute and alternating with candle closes. Default 150s
    /// (2.5× the default reconcile interval); closes reclaim the slot only if reconcile STOPS for
    /// longer than this. Pushed onto every engine's `Account` at spawn (a knob, not a policy —
    /// the law lives in `Account::set_mark_from`). `0` makes reconcile-mark ownership expire
    /// immediately.
    pub reconcile_mark_staleness_ms: i64,
    /// Opt-in per-mount READINESS GATE (portfolio-observer PR-4 T5): when `true`, a freshly
    /// mounted strategy starts `Pending` (see [`MountState`]) and stays that way until its
    /// symbol actually prices through the [`vike_exec::price_board::PriceBoard`] (checked at
    /// the drain-loop boundary, [`CoreThread::maintain_mount_readiness`], using [`Self::price_cfg`]
    /// — the SAME resolver knobs the equity sampler and `CoreSnapshot::build` already price
    /// through, so "priced" means one consistent thing everywhere). A `Pending` mount still
    /// RECEIVES every hook call (bars/quotes/trades/fills/...) — its estimator/warmup proceeds
    /// exactly as if the gate were off — but any order intents it buffers are DISCARDED at
    /// [`CoreThread::drain_broker`] instead of reaching the engine, so nothing can hit a venue
    /// before its own symbol has a real price. `false` (the default) is INERT: every mount is
    /// seeded `Ready` at [`assemble_core`] and never re-checked, so the boundary probe never
    /// runs and `drain_broker`'s new check is a compare against a constant — BYTE-IDENTICAL to
    /// today's behavior.
    pub readiness_gate: bool,
    /// Opt-in DEAD-MAN'S SWITCH (auto cancel-on-disconnect; trading-hardening) — the AUTOMATIC
    /// counterpart to the manual HALT sentinel. When `Some`, a cold-path timer (a sibling of the
    /// stuck-order watchdog, driven by the same [`DeadlineTimerWheel`] at the drain-loop boundary,
    /// NEVER the fold) watches how long market data / venue events have been silent; if that
    /// exceeds [`DeadManConfig::timeout`] it TRIPS once — cancelling every resting order through the
    /// one order-write path and, for [`DeadManAction::CancelAllAndHalt`], engaging HALT (in-process
    /// `trading_state = Halted` + the cross-process HALT sentinel file). It re-arms on recovery.
    /// `None` (default) = DISABLED: no [`deadman::DeadMan`] is built, no timer/waker is armed for
    /// it, and the per-message fold never touches it — byte-identical to today. See
    /// [`deadman`] for the full contract.
    pub deadman: Option<DeadManConfig>,
    /// The CONNECTION-state DEAD-MAN (M13) — the SUCCESSOR to [`Self::deadman`], not a variant of
    /// it. When `Some`, a cold-path timer watches the per-`(venue, symbol)` [`FeedStatus`] the
    /// bridges disclose: a `Disconnected` on an armed venue opens a grace window, a `Live` closes
    /// it, `Stale` is ignored entirely, and a link still down when
    /// [`LinkDeadManConfig::grace`] expires cancels THAT VENUE's resting orders (plus, for
    /// [`DeadManAction::CancelAllAndHalt`], the process-wide HALT). `None` (default) = DISABLED:
    /// no latch is built, no timer or waker is armed for it, and the fold never touches it —
    /// byte-identical to a runtime that never heard of the feature.
    ///
    /// ⚠ Independent of [`Self::deadman`] in both directions: either, both or neither may be
    /// `Some`. They observe DIFFERENT signals (silence vs. the disclosed link state) and only
    /// share [`DeadManAction`]. [`link_deadman`]'s module doc carries the re-ruling that made this
    /// one the default and the other one opt-in.
    pub link_deadman: Option<LinkDeadManConfig>,
    /// Opt-in MANAGED GTD EXPIRY sweep cadence (core-ergonomics): how often the core re-checks
    /// resting orders for a passed `gtd_expiry` and cancels them ([`CoreThread::sweep_gtd_expiry`]).
    /// Runs on the EXISTING [`DeadlineTimerWheel`] at the drain-loop boundary — no new thread, and
    /// nothing at all on the per-message fold. `None` (default) = DISABLED: no timer is armed, the
    /// sweep never runs, and a venue's own native GTD handling (or the order simply resting) is
    /// unchanged — byte-identical to today. The order terms themselves are NOT new
    /// (`OrderRequest::time_in_force` / `gtd_expiry` already exist); this only adds the local
    /// enforcement for venues that do not expire GTD themselves. Pick a cadence coarse relative to
    /// the expiries you set: an order can rest up to one tick past its deadline before the sweep
    /// sees it.
    ///
    /// **REPLAY: SUPPORTED as of emulator PR-5** (it was a loud `Unsupported` refusal before).
    /// The sweep journals every expiry it decides write-ahead as
    /// [`vike_journal::JournalRecord::GtdExpire`], and its only local action — `cancel_order` —
    /// publishes nothing and moves no fenced state; the authoritative `OrderCanceled` comes back
    /// from the venue as its own journaled `Ingest::Event` and replays independently. So a
    /// `gtd_sweep` session now replays and crash-restores like any other, and the record is
    /// replay-NEUTRAL (never re-applied — re-issuing the cancel would double it). Combining this
    /// with [`Self::submit_ack_timeout`] or [`Self::deadman`] still forfeits replay, for THOSE
    /// features' own wall-clock reasons ([`crate::replay::ReplayError::Unsupported`]).
    pub gtd_sweep: Option<Duration>,
    /// Opt-in PERIODIC PORTFOLIO SNAPSHOT cadence (core-ergonomics): on this cadence the core
    /// appends ONE compact [`vike_journal::PortfolioSample`] record (per-venue equity/balance/
    /// realized + every open position) to the write-ahead journal — a low-rate equity/positions
    /// time series for downstream reporting, without the GUI having to poll a running process.
    ///
    /// WHY THE JOURNAL (the "least-invasive existing off-fold path" choice): the SQLite `exec_db`
    /// writer is retired (see [`crate::counters`]'s reserved slots), so the journal + its
    /// materializer is the only surviving durable off-fold sink the core already owns — it is
    /// already opened, already framed/checksummed/segmented, already pruned, and already the log
    /// `vike_journal::materialize`'s `JournalMaterializer` tail-follows. Writing here therefore adds
    /// no file handle, no thread and no new dependency edge. The record is REPLAY-NEUTRAL
    /// (`replay.rs`'s tail extraction ignores it, exactly like `MintedSubmit`): it is an
    /// observation, never a command to re-apply.
    ///
    /// ⚠ This said "already read by `vike-app-core`'s materializer" until 2026-09-28. The
    /// materializer is `crates/vike-journal/src/materialize.rs` since 2026-09-25, no production
    /// root has spawned one since the daemon's `materialize` feature was deleted (#2093,
    /// 2026-09-22), and it passes over these records (its `PortfolioSnap` arm does nothing) — so
    /// nothing in the tree reads a `PortfolioSample` back today.
    ///
    /// Gated on [`Self::journal`] ALSO being `Some` — with no journal there is nowhere to write, so
    /// the timer is simply never armed. `None` (default) = DISABLED, byte-identical.
    ///
    /// REPLAY/RESTORE STAYS INTACT. The cadence needs the boundary waker to reach an idle core, and
    /// the waker's `Ingest::Watchdog` message is what normally forces `replay_from` to refuse a
    /// session. That refusal exists for the WALL-CLOCK sweeps that mutate state, so the waker
    /// record is written only when one of those is enabled (`CoreThread::journal_waker_records`) —
    /// a core running this cadence alone journals no waker records and still replays/crash-restores
    /// exactly as before. Combining it with `submit_ack_timeout`/`deadman` does forfeit replay, but
    /// for those features' own reasons; `gtd_sweep` no longer forfeits it (emulator PR-5).
    ///
    /// DISK GROWTH (it is cheap per record, not free per session): these records deliberately do
    /// NOT bump `journaled_since_snap` — that counter paces the REPLAY-BASE `Snap` cadence, which
    /// stays a function of folded commands. Consequence: an otherwise IDLE journaling core (no
    /// exec-lane traffic at all) never reaches the `snapshot_every` threshold, so no `Snap` is
    /// written, so `prune_before_latest_snap` has no floor to prune to and segments accumulate for
    /// the life of the session (the shutdown `Snap` is the first prunable point). A 1s cadence on a
    /// silent weekend is ~86k unprunable records/day. Size the cadence for the session length —
    /// seconds-to-minutes for a reporting series, not sub-second.
    pub portfolio_snapshot_interval: Option<Duration>,
    /// Opt-in FAST IN-FLIGHT CONFIRM cadence + age threshold (recon path-to-superset, F1-A; grafted
    /// from Nautilus's ~2s `check_inflight_orders`). On this cadence the core issues a REJECT-FREE
    /// venue re-query ([`vike_exec::ExecutionEngine::confirm_order`]) for every order stuck
    /// SUBMITTED-but-unacked (`Initialized`/`Submitted`, `created_ms == Some`) whose age is in the
    /// band `[inflight_confirm, submit_ack_timeout)` — the early window the reject-capable
    /// stuck-order ladder ([`CoreThread::sweep_stuck_orders`], gated on [`Self::submit_ack_timeout`])
    /// deliberately ignores. It catches a wedged adapter in ~2s instead of waiting the conservative
    /// `submit_ack_timeout` (at LEAST 30s) for the ladder's first confirm.
    ///
    /// It NEVER synthesizes a terminal and NEVER fights the reject ladder: it calls only the
    /// `is_live`-guarded, publish-nothing `confirm_order`; it is BAND-LIMITED to
    /// `age < submit_ack_timeout` so once an order crosses that timeout this rung stops touching it
    /// and the late (reject-capable) rung owns it — the two never act on the same order at the same
    /// age, and this rung never touches the ladder's `confirm_issued_ms`; and it keeps its OWN dedup
    /// map (`inflight_confirm_last_ms`) so a still-stuck order is re-confirmed at most once per
    /// `inflight_confirm` window, not every tick. When `submit_ack_timeout` is `None` (watchdog off)
    /// the band is `[inflight_confirm, +inf)` — this becomes the ONLY stuck-order signal, still
    /// reject-free (without a configured reject timeout we must never invent a terminal).
    ///
    /// `None` (default) = DISABLED, byte-identical: the [`TimerKind::InflightConfirm`] timer is
    /// never armed, the waker spawn/tick is unchanged, `sweep_inflight_confirms` is never reachable,
    /// and no dedup state is ever written. Suggested live value ~2s
    /// (`VIKE_RECONCILE_INFLIGHT_MS=2000`). REPLAY STAYS INTACT: the sweep's only action is
    /// `confirm_order`, which publishes nothing and moves no fenced state (like the `gtd_sweep`
    /// cancel), so a core running this cadence alone journals no waker record and replays/crash-
    /// restores exactly as before.
    pub inflight_confirm: Option<Duration>,
    /// Opt-in OCO SIBLING-CANCEL ON A DEAD PROTECTIVE EXIT (live-runtime OCO/OTO). Governs what
    /// [`CoreThread::drive_contingency_on_terminal`] does when a RELEASED bracket exit leg (a stop-
    /// loss / take-profit already armed by its entry's fill) reaches a terminal-UNFILLED state — a
    /// venue reject / cancel / expire:
    ///
    /// - `false` (default) — KEEP the surviving OCO sibling. The dead leg's own stale book entry is
    ///   cleaned and the death is surfaced, but the sibling is left resting: a position that just
    ///   lost one protective leg retains whatever protection it still has (a naked position is the
    ///   worse default). Byte-identical to the feature as first shipped — only a FILL cancels a
    ///   sibling.
    /// - `true` — ALSO cancel the surviving OCO sibling (and clean its book entry), through the SAME
    ///   mechanics the fill-driven OCO-cancel uses: a still-held sibling is dropped, a resting one is
    ///   canceled at the venue. A deployment that prefers a fully flat book over keeping partial
    ///   protection opts into this. Zero effect on any run without brackets (the contingency book
    ///   stays empty, so the terminal drive is a no-op regardless).
    ///
    /// A composition root sources this from its settings (the core reads no environment):
    /// `vike-tradehub` sets it from `vike_config::Flags::oco_cancel_sibling_on_dead_exit` — the
    /// `flags.oco_cancel_sibling_on_dead_exit` setting, which `VIKE_OCO_CANCEL_SIBLING_ON_DEAD_EXIT`
    /// still overrides inside `vike_config`'s env layer — on its live mount, and hands the same flag
    /// to its paper arm through `vike_mount::PaperMountOpts`. ⚠ This said "`vike-app` reads
    /// `VIKE_OCO_CANCEL_SIBLING_ON_DEAD_EXIT=1`" until 2026-09-28; the desktop builds no
    /// `CoreConfig` since its local core was deleted (#1727, 2026-09-09).
    pub oco_cancel_sibling_on_dead_exit: bool,
    /// Opt-in per-mount loss/notional BUDGET (steal/core-per-mount-budget), keyed by the MOUNT ID —
    /// `StrategyMount::controller_id` when the mount carries one, else the legacy
    /// `{venue}__{symbol}__{interval}` derivation ([`crate::strategy_state::mount_id_with`], the ONE
    /// mount-identity law). A mount whose key is present (and whose [`MountBudget`] has an active
    /// arm) is watched on the SAME per-closed-bar cadence as [`Self::max_drawdown`]; on breach it
    /// latches liquidate-only (cancel its resting orders + optional flatten) while every other mount
    /// keeps trading. Empty (default) = no per-mount latch, byte-identical to today (the sweep is
    /// never even armed — see `CoreThread::any_mount_budget`).
    ///
    /// KEYED HERE, not as a field on [`StrategyMount`]: the id resolves each budget onto its mount at
    /// [`assemble_core`], and a key naming no mounted id is simply never matched (inert), never an
    /// error.
    ///
    /// **Why the MOUNT ID and not the `(venue, symbol, interval)` triple** (which is what this map
    /// used to be keyed by): the triple is COARSER than mount identity. Since
    /// [`StrategyMount::controller_id`] landed, two mounts may legitimately share one triple — that
    /// is the exact configuration a controller id exists to legitimize — and under the old key they
    /// resolved to the SAME budget entry. Each latched on its OWN attributed loss, but against a cap
    /// neither could be given independently: two mounts, one budget, no way to express two. The
    /// mount id is the unique identity `assemble_core` already asserts on (a duplicate is a loud
    /// mount-time panic), so keying on it makes "one budget per mount" true by construction. A
    /// controller-id-free mount's key is the SAME `{venue}__{symbol}__{interval}` string as before,
    /// only spelled as one segment instead of a tuple.
    ///
    /// ⚠ The key is the SANITIZED id, not the raw controller id: `mount_id_with` replaces every
    /// non-alphanumeric char with `_`, so a mount whose `controller_id` is `"maker-a"` keys as
    /// `"maker_a"`. Build the key with `mount_id_with`/`mount_id_of` rather than by hand.
    pub mount_budgets: std::collections::HashMap<String, MountBudget>,
    /// Opt-in per-mount WALL-CLOCK SCHEDULE (steal/core-live-scheduler), keyed by the MOUNT ID —
    /// the same key [`Self::mount_budgets`] uses, for the same reason: a same-triple mount PAIR
    /// (which [`StrategyMount::controller_id`] permits) used to share ONE schedule and could not be
    /// given two.
    /// Each [`crate::schedule::LiveSchedule`] registers wall-clock [`crate::schedule::TimeRule`]s
    /// (daily-at-HH:MM in a fixed-offset tz, market-open/close offsets) that fire
    /// `Strategy::on_schedule(broker, tag)` at the right instant — the LIVE twin of the backtest
    /// bar-boundary `Schedule`, with identical rule semantics. Checked at EVERY drain-loop boundary
    /// pass (NEVER per message; the `p99 < 10µs` fold is untouched) — no timer-wheel entry, the
    /// readiness-gate pattern: `check_due` is a few latch compares per rule, and the boundary waker
    /// (whose cadence [`Self::schedule_poll`] feeds) guarantees an idle core reaches the boundary on
    /// cadence. Driving off the CLOCK VALUE at the boundary rather than wheel deadlines is what
    /// keeps an injected/deterministic clock (replay, tests) working: a wheel deadline computed
    /// from a frozen clock is unreachable until the clock moves, which starves the poll.
    ///
    /// Empty (default) = byte-identical: `any_mount_schedule` is false (one bool read per boundary),
    /// no waker cadence is added, and `drive_schedule` never runs. A key naming no mounted mount id
    /// is simply never matched (inert), never an error.
    pub mount_schedules: std::collections::HashMap<String, LiveSchedule>,
    /// WAKER cadence contribution for the wall-clock schedule — how often an otherwise-IDLE core is
    /// woken to reach the drain-loop boundary where [`Self::mount_schedules`] is checked. Only in
    /// effect when some mount has a non-empty schedule; `None` (default) ⇒ 1s. A rule fires at the
    /// first boundary pass AT or AFTER its scheduled instant, so on an idle core this bounds the
    /// firing lateness (the waker tick is `max(schedule_poll/2, 50ms)`); a busy core fires at the
    /// next drain boundary regardless — set it finer for sub-second idle precision, coarser to
    /// reduce idle wakeups.
    pub schedule_poll: Option<Duration>,
    /// SHUTDOWN POLICY (opt-in): cancel every resting order during the core's own teardown, before
    /// the client is detached. `false` (the default) is BYTE-IDENTICAL to the behavior that shipped
    /// before this field existed — teardown detaches and exits, and **resting orders stay live at
    /// the venue with nothing left running to manage them**.
    ///
    /// That default is a deliberate operator decision, not an accident: a book left resting through
    /// a deploy is what many operators want, and cancelling on the way out would be a silent
    /// behavior change for every existing deployment. An operator who would rather leave nothing
    /// behind flips one value — `flags.cancel_orders_on_shutdown`
    /// (`vike-cli config set flags.cancel_orders_on_shutdown true`; `vike-tradehub` is the reader).
    ///
    /// ⚠ IT CANCELS; IT DOES NOT FLATTEN. Positions are untouched — this is "leave no resting
    /// orders", never "get me out" ([`vike_exec::OrderIntent::MarketExit`] is that verb, and it is an
    /// operator action, not something a stop may decide on its own). It is BEST-EFFORT like every
    /// other cancel path here: the cancels are queued to each venue's `ExecActor` before the detach
    /// and drained by that actor's own teardown, but no venue `OrderCanceled` is waited for. The
    /// whole teardown is hard-capped by the caller's deadline
    /// (`vike_ops::shutdown::run_with_deadline`), which abandons whatever is still in flight, so a
    /// wedged venue can never stop the process exiting.
    ///
    /// ⚠ IT CANNOT RUN ON A PATH THAT NEVER REACHES TEARDOWN — but as of the graceful-stop change
    /// `systemctl stop` IS such a path. This comment previously said the opposite, in capitals: that
    /// no signal handler existed, so SIGTERM ended the process where it stood and this flag was inert
    /// under a service. That was true until `vike_ops::stop`'s handler landed and
    /// `crates/vike-tradehub/src/tradehub_cli.rs` began waiting on the flag instead of parking.
    ///
    /// ⚠ SO READ THE CHANGE IN BEHAVIOUR BEFORE ENABLING THIS: on a build carrying that change, the
    /// FIRST `systemctl stop` after deploying will sweep the resting book. That is the intended
    /// behaviour and the reason the flag exists — but anyone who set it while the old comment was
    /// true set it believing it could not fire as a service.
    ///
    /// Still effective on the interactive paths (`shutdown`/`quit`/Ctrl-D), and still subject to the
    /// hard cap above: what the deadline abandons is not cancelled. `docs/ops/kill-switches.md` and
    /// `docs/ops/graceful-stop.md` carry the operator-facing versions.
    pub cancel_orders_on_shutdown: bool,
    /// RUNTIME strategy-mount resolver (split-plane B5) — what turns a
    /// [`vike_exec::MountSpec`]'s `name`/`rhai`+`params` vocabulary into a live
    /// `Box<dyn Strategy<LiveBroker>>` when a [`Command::MountStrategy`] arrives. Injected by the
    /// COMPOSITION ROOT (vike-core sits below the strategy crates — vike-strategy/vike-script are
    /// dev-deps here — so the core cannot resolve a name itself; the root closes over its own
    /// registry/profile machinery, e.g. vike-tradehub's `mount_factory`). `None` (the default, and
    /// every config that predates the field) REFUSES every runtime mount with a recent-events note
    /// — byte-identical otherwise: no arm of the per-event fold reads this field.
    ///
    /// The factory runs ON the fold thread, inside the `Command::MountStrategy` arm, under the same
    /// `catch_unwind` discipline as every other user-code call site — acceptable because a mount is
    /// an OCCASIONAL operator verb (the `UpdateParams` precedent: strategy hooks already run
    /// there), never per-message work. A slow resolve (a Rhai compile) briefly delays the fold like
    /// a reconcile pass does; it never touches the p99 event path of a core nobody is mounting
    /// into.
    pub strategy_factory: Option<StrategyFactory>,
}

impl Default for CoreConfig {
    fn default() -> Self {
        CoreConfig {
            seed_cash: 0.0,
            snapshot_interval: Duration::from_millis(16),
            recent_events_cap: 64,
            ingest_capacity: 8192,
            batch_max: 1024,
            clock: Box::new(LiveClock),
            repaint: None,
            panic_on_trade_id: None,
            on_dequeued: None,
            strategy: None,
            submit_ack_timeout: None,
            // Conservative default (bumped 5s → 15s per the confirm-race hardening): a single grace
            // of 15s already exceeds the worst-case adapter confirm (Bybit's realtime→history re-query
            // ≈ 2×5s = 10s), and the in-flight-confirm guard gives a still-pending confirm up to
            // 2×15s = 30s to land before the last-resort reject — so even a much slower venue confirm
            // is never raced into a phantom reject. Only in effect once `submit_ack_timeout` is opted
            // in. See the `submit_ack_confirm_grace` field docs for the hard lower bound.
            submit_ack_confirm_grace: Duration::from_secs(15),
            margin_call: None,
            max_drawdown: None,
            conditionals_on_ticks: false,
            extra_mounts: Vec::new(),
            journal: None,
            coid_session: None,
            instance_origin: None,
            arm_seq: 0,
            conditionals: Vec::new(),
            contingencies: Vec::new(),
            mount_attr: Vec::new(),
            coid_mounts: Vec::new(),
            counters_path: None,
            state_dir: None,
            state_save: None,
            equity_sample: None,
            on_equity_sample: None,
            journal_view_provider: None,
            price_cfg: PriceCfg::default(),
            mark_staleness_ms: 10_000,
            reconcile_mark_staleness_ms: 150_000,
            readiness_gate: false,
            deadman: None,
            link_deadman: None,
            gtd_sweep: None,
            portfolio_snapshot_interval: None,
            inflight_confirm: None,
            oco_cancel_sibling_on_dead_exit: false,
            mount_budgets: std::collections::HashMap::new(),
            mount_schedules: std::collections::HashMap::new(),
            schedule_poll: None,
            // OFF = today's behavior, byte-identical: teardown detaches and exits, resting orders
            // stay live at the venue. See the field doc for why that is the default.
            cancel_orders_on_shutdown: false,
            // No resolver = every runtime MountStrategy refuses (split-plane B5); byte-identical
            // for every config that predates the field.
            strategy_factory: None,
        }
    }
}

/// The [`CoreConfig::strategy_factory`] shape: resolve one runtime [`vike_exec::MountSpec`] into
/// the strategy object to mount, or a human-readable refusal (surfaced verbatim in the core's
/// recent-events ring). `FnMut` so a root's factory may keep state (e.g. a compile cache).
pub type StrategyFactory = Box<
    dyn FnMut(&vike_exec::MountSpec) -> Result<Box<dyn Strategy<LiveBroker> + Send>, String> + Send,
>;

/// Dequeue observer (latency harness / diagnostics). Runs under the core's panic guard.
pub type DequeuedHook = Box<dyn FnMut(&Ingest) + Send>;

/// The equity sampler's sink (portfolio-observer PR-3) — see [`CoreConfig::on_equity_sample`].
pub type EquitySampleHook = Box<dyn FnMut(&[EquitySample]) + Send>;

/// Builds a per-venue [`vike_exec::recon::JournalView`] from the materialized exec log — see
/// [`CoreConfig::journal_view_provider`]. No production root supplies one today, so that field is
/// `None` at every live mount; the only hooks built are this crate's tests' (such as
/// `crates/vike-core/tests/recon/recon_journal_crosscheck.rs`'s `journal_has_t1`). A root that
/// wires one would build it over [`crate::journal_view_from_store`], handed a store it constructed.
///
/// ⚠ This said "Supplied by `vike-app-core` (which can read the store)" until 2026-09-28. The one
/// production hook was built in the GUI shell's `main.rs` and was deleted with that binary's local
/// core on 2026-09-09 (#1727); `crates/vike-core/src/journal_view.rs` carries the rest.
pub type JournalViewHook = Box<dyn Fn(&str) -> vike_exec::recon::JournalView + Send>;
