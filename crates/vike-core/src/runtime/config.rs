//! The core's configuration surface: `CoreConfig` (+ its `Default`), `JournalConfig` and the hook type aliases.

use super::*;

/// Opt-in write-ahead command journal (spec 2026-07-10-journal-replay-and-data-freshness §A).
/// `None` (the [`CoreConfig`] default) = today's behavior, ZERO overhead — no journal is opened
/// and the fold path is byte-identical. `Some` opens a [`vike_journal::Journal`] at
/// construction and write-ahead-journals every exec-lane message before it folds (see
/// [`CoreThread::dispatch`]).
#[derive(Debug, Clone)]
pub struct JournalConfig {
    /// directory the segment files live in (created if absent).
    pub dir: std::path::PathBuf,
    /// segment sizing + flush cadence of the underlying journal.
    pub file: vike_journal::JournalFileConfig,
    /// write a full-state `Snapshot` record every N appended records (plus always on Shutdown / the
    /// senders-dropped exit). `0` would snapshot on every record.
    pub snapshot_every: u64,
}

impl JournalConfig {
    /// A journal at `dir` with the standard segment/flush/snapshot cadence (64 MiB segments, flush
    /// every 256 records, snapshot every 8192) — the ONE place those defaults are written: a run
    /// profile's `[sinks.journal]` key left off keeps this value (`vike_tradehub_core::journal_config_from`
    /// starts from it, with `dir` the `config.journal_dir` row).
    ///
    /// `snapshot_every` was 1024 until 2026-11-19 (the owner's "do all", the research note's 1a): a `Snapshot` of
    /// the CI box's ~40 KB of state every ~5.7 s while the maker was busy. 8192 takes one 8x less often, and a restart
    /// after a crash refolds at most 8192 records after the latest one instead of 1024 (a clean stop writes an exit
    /// `Snapshot` and refolds none).
    pub fn at(dir: impl Into<std::path::PathBuf>) -> Self {
        JournalConfig {
            dir: dir.into(),
            file: vike_journal::JournalFileConfig {
                segment_bytes: 64 * 1024 * 1024,
                flush_every: 256,
            },
            snapshot_every: 8192,
        }
    }
}

/// **The oldest price this node acts on, 60 s: ONE constant, so nothing disagrees about what a stale price is.**
/// The control door's price tap answers "none yet" past it (`vike-tradehub`'s `PriceTap`), the live root sets
/// [`PriceOnlyConfig::max_price_age`] to it, and a held order's price condition is judged by it on a core that
/// configures no [`PriceOnlyConfig`] (`held_release.rs`'s `release_price`: the paper rehearsal cores).
pub const MAX_PRICE_AGE: Duration = Duration::from_secs(60);

/// **The risk clock, and the venues a MOUNT does not drive** (decision 0173's on-demand engines: `binance`,
/// `okx`, `aster`, `hyperliquid` priced by a price-only feed), folded by the composition root.
///
/// **The clock (owner ruling 2026-10-25).** The margin call and the drawdown latch run from
/// [`CoreThread::drive_strategy`]'s closed bar, and a mount on 4-hour bars is not looked at once per
/// 4 hours: [`Self::sweep_interval`] drives both off the drain-loop boundary for EVERY live engine, the ones
/// a mount drives and the ones none does ([`CoreThread::sweep_risk_clock`]; cold, never the fold), priced
/// and skipped by the same age bound ([`Self::max_price_age`]). The closed-bar pass keeps running beside
/// it. [`Self::venues`] may be EMPTY: a node whose every admitting venue is mounted still has a clock.
/// The per-mount budget latch is not on the clock: it latches a MOUNT, on a closed bar.
///
/// Two things are true of a PRICE-ONLY engine (a venue in [`Self::venues`]) and neither is true of a
/// mounted one. (1) No closed bar of its own series ever arrives, so the clock is the ONLY thing that runs
/// its margin call. (2) Its feed is a pump per
/// admitted symbol, opened on demand, so "silent" means nothing while nothing is admitted: the link
/// dead-man opens no window for these venues, and discards the open ones at every sweep, unless the venue
/// holds a live order or a non-flat position ([`CoreThread::venue_holds`]; a mere admitted symbol is not a
/// holding), and acts on the rest like any armed venue.
/// The venues must ALSO be in [`DeadManLinkConfig::venues`] to be heard at all.
///
/// (3) Its marks must be FRESH and from the engine's own network. The clock prices every engine
/// with every rung capped at [`Self::max_price_age`] and no stale fallback, and the margin call and the
/// mount budgets SKIP an engine with an open position past that bound (said once per episode, with the
/// age); an engine whose [`Self::price_source`] network is not its own tier's is kept out of the margin
/// call and the drawdown latch altogether. The drawdown latch NEVER skips a stale engine, mounted or
/// price-only (owner ruling 2026-10-25): it values it at its last known price, so a quiet pump cannot
/// blind the node's latch. A STOPPED engine is exempt from the bound (its board never moves again).
#[derive(Debug, Clone)]
pub struct PriceOnlyConfig {
    /// The price-only venues, as the engines name them; empty on a node that mounts every venue it trades.
    pub venues: std::collections::BTreeSet<String>,
    /// How often the margin call and the drawdown latch are evaluated off the clock, for every live engine.
    pub sweep_interval: Duration,
    /// The oldest price the clock's risk sweeps act on: the SAME bound the control door's price tap
    /// uses ([`MAX_PRICE_AGE`]), set by the composition root.
    pub max_price_age: Duration,
    /// Per venue, the account tier whose NETWORK the venue's price feed dials (`Live` = mainnet, `Demo` =
    /// testnet). An engine that runs on a real network (`tier != Paper`) at a different tier than its feed
    /// is priced off another network's book (a mainnet-vs-testnet basis is phantom PnL) and does not feed
    /// the margin call or the drawdown latch. A venue absent here is trusted.
    pub price_source: std::collections::BTreeMap<String, vike_model::AccountTier>,
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
    /// `ClockLive` (wall time) by default; tests inject `ClockTest` or a bare closure
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
    pub strategy: Option<Mount>,
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
    /// timeout (`~5s`, `vike_transport::http::blocking_agent_with_timeout`) and `N` is the number of
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
    /// semantics). Swept on every CLOSED bar (marks fresh, off the event fold) AND, with
    /// [`Self::price_only`] set, on its clock for every live engine ([`PriceOnlyConfig`]).
    /// Warning → recent-events ring; liquidation submits reduce-only
    /// market orders through the NORMAL gate path. `None` (default) = disabled.
    pub margin_call: Option<vike_model::MarginCallConfig>,
    /// Opt-in drawdown latch (audit exec#4): the max tolerated fractional drawdown from the
    /// high-water-mark of the DAEMON'S OWN equity curve before the core LATCHES into
    /// liquidate-only (`trading_state = Reducing`, so the existing RiskGate then denies
    /// risk-increasing orders and permits only reduce-only). Folded on the SAME cadences (every
    /// CLOSED bar, and the [`Self::price_only`] clock) and through the SAME resolver as the
    /// margin-call watchdog (off the event fold). E.g. `Some(0.20)` = latch at 20% drawdown. `None` (default) — and any value
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
    /// is `Some` — `ProfileRun::validate` requires it. The one exception is an engine in a NON-dollar
    /// currency group, whose capital is the venue wallet it adopted (`CoreThread::adopt_wallet_bases`).
    pub max_drawdown: Option<f64>,
    /// Phase C (opt-in, Rust-native): also check the ConditionalArms on every tick of the
    /// engine's series (quote mid / trade px) — intra-bar stop/trailing triggering the
    /// Python oracle's bar-close book lacks. Default false = oracle-faithful bar-close only.
    pub conditionals_on_ticks: bool,
    /// Phase D (opt-in): ADDITIONAL strategy mounts beyond `strategy` — one engine, N
    /// (venue, symbol, interval) mounts over the one multi-symbol Account. Symbols beyond
    /// the engine's primary must be listed in `Engine::extra_symbols` so their
    /// venue events fold. Empty (default) = single-mount behavior, unchanged.
    pub extra_mounts: Vec<Mount>,
    /// Opt-in write-ahead command journal (spec §A). `None` (default) = no journal opened,
    /// zero overhead — the fold path is byte-identical to today.
    pub journal: Option<JournalConfig>,
    /// Resume the [`vike_model::orders::client_id::OrderClientIdGenerator`] from a persisted
    /// `(session, seq)` on restart (so replayed/new orders keep unique, non-colliding ids across a
    /// restart). `None` (default) = a fresh random v2 session (decision 0270).
    pub order_client_id_session: Option<(String, u64)>,
    /// This instance's declared origin claim (`config.instance_origin`), stamped into the order client id
    /// SESSION of a FRESH generator so every id this core mints names the deployment that placed
    /// it. `None` (default) = today's ids exactly — `vike_model::orders::client_id::origin`'s module doc
    /// argues the format and what it buys.
    ///
    /// ⚠ **Consulted only for a fresh session: [`Self::order_client_id_session`] `None`, or the boot restore's one-time
    /// switch to a v2 session (decision 0270, [`assemble_core`]).** Otherwise a restart RESUMES the persisted
    /// session verbatim, because that session is what the journal replays and the determinism
    /// fence compares — re-stamping it from configuration would make a replay's ids depend on the
    /// box replaying them. The consequence an operator has to know: turning the origin on (or
    /// changing it) does NOT reach the wire until the core starts a fresh session.
    /// [`assemble_core`] logs a WARN naming both tags when a resumed session disagrees with the
    /// configured one, so the gap is visible rather than inferred, and
    /// `docs/ops/double-live-instances.md` says what to do about it.
    pub instance_origin: Option<vike_model::OrderClientIdOrigin>,
    /// Resume the emulated-conditional arm-id counter ([`CoreThread::mint_arm_id`]) on restart,
    /// the arm-side twin of [`Self::order_client_id_session`]. `0` (default) = a fresh session's counter.
    ///
    /// This matters BECAUSE `order_client_id_session` is resumed: an arm id is `{order_client_id_session}a{arm_sequence}`, so
    /// a restart that restores the session but restarts the counter at 0 would re-emit ids the
    /// pre-restart run already used — and resume APPENDS to the same journal directory, so both
    /// live in ONE log. And since emulator PR-2 the arm id is the DISARM key
    /// ([`vike_execution::OrderIntent::DisarmConditional`]), so a duplicate would let a disarm target
    /// the wrong arm. [`crate::replay::RestoredState::arm_sequence`] supplies the resume value (the
    /// latest `Snapshot`'s stamped counter, prune-safe — see that field for the pre-v7 fallback); a
    /// caller that does not resume keeps today's behavior.
    pub arm_sequence: u64,
    /// Re-arm the emulated-conditional books on restart (emulator PR-3, re-arm-on-restore): each
    /// entry is one armed conditional — its minted `arm_id` and the terms the book held at the
    /// restore base's `Snapshot` (a TRAILING arm's ratcheted extreme included) — seeded into
    /// `conditional_books` ONCE at [`assemble_core`], in order (fire order is insertion order).
    /// [`crate::replay::RestoredState::order_emulation_arms`] supplies the value on a crash restart, next
    /// to `order_client_id_session`/`arm_sequence`; empty (default) = today's behavior, a core that starts with
    /// empty books (and a pre-v8 journal's Snapshot carries no books, so it restores empty — never an
    /// error). Zero hot-fold cost: consumed entirely at assembly.
    ///
    /// ⚠ **A RESTORED arm's ACCOUNT comes from its `route_key` (journal v17) or its owning mount.** A
    /// v17 `Snapshot` of a core holding two engines of one exchange writes each arm's route key, and
    /// [`assemble_core`] gives such an arm its [`crate::emulator::OrderEmulationBook::cond_engine`] row back (on that
    /// engine, with its owning mount from [`Self::conditional_owners`]), so its fire reaches its own
    /// account. An arm WITHOUT a route key — every single-account core's, where the payload route
    /// reaches the one book anyway, every pre-v17 `Snapshot`'s, and a crash-tail arm the pure fold
    /// re-derives — gets a row only through its owning mount (`route_of` over the mount's engine),
    /// and otherwise none.
    ///
    /// ⚠ **DECLARED RESIDUAL — such an arm with neither loses its ACCOUNT.** It gets no
    /// [`crate::emulator::OrderEmulationBook::cond_engine`] entry, and its fire takes the historical payload route with no
    /// account on the request. On a venue with ONE engine, that route reaches the one book, which
    /// is correct. On a venue with SEVERAL accounts, the Submit arm's ambiguity gate refuses it
    /// (a payload naming the exchange and no account). So a protective stop armed before a restart
    /// and triggered after it reaches NO book: the refusal lands in the recent-events ring naming
    /// the candidate accounts, and the position the stop was armed to close stays open and
    /// unprotected. The account-scoped mass-cancel reads the same fact
    /// ([`CoreThread::armed_engine`]'s `None`) and leaves such an arm under every label. Since v17
    /// the residual is narrow: an operator's keyless arm on a multi-account core, from a pre-v17
    /// journal or from a crash tail restored by the pure folds; the live arm is exact.
    ///
    /// (This said the fire "fires onto the default account" until 2026-09-26. That was true before
    /// the ambiguity gate and has not been true since. `mount_account_tests`'
    /// `a_labelled_mass_cancel_leaves_a_restored_arm_it_cannot_attribute_to_the_named_account`
    /// asserts the refused fire.)
    pub order_emulation_arms: Vec<vike_journal::JournalSnapshotOrderEmulationArm>,
    /// Re-seed the live-runtime OTO/OCO contingency book on restart (live-runtime OCO/OTO), the
    /// contingency twin of [`Self::order_emulation_arms`]: each entry is one resting linked leg — its
    /// linkage (`parent`/`linked`/`active`) and, for a HELD exit, the resolved order the runtime is
    /// keeping OFF the venue until its parent fills. Seeded into `contingency`/`held_orders` ONCE at
    /// [`assemble_core`], in the captured insertion order (arm/cancel iteration order). Supplied on a
    /// crash restart by [`crate::replay::RestoredState::contingencies`]; empty (default) = today's
    /// behavior (a core that starts with no contingency state; a pre-v11 journal's Snapshot carries none,
    /// so it restores empty — never an error). Zero hot-fold cost: consumed entirely at assembly.
    pub contingencies: Vec<vike_journal::JournalSnapshotContingency>,
    /// Re-hold the orders the core was keeping off the venue until their release (`Snapshot.held_releases`,
    /// `held_release.rs`), in their held order. Supplied by [`crate::replay::RestoreReport::apply_to`]
    /// and [`crate::replay::RestoredState::held_releases`]; empty (default) = nothing held. Consumed
    /// entirely at assembly.
    pub held_releases: Vec<vike_journal::OrderHeldRelease>,
    /// Work again the EMULATED orders the core was working (`Snapshot.emulated`, decision 0275,
    /// `runtime/emulated.rs`), in their accepted order: each law is rebuilt from its saved state and
    /// AWAITS its child's confirmation, so a restart never places a second child and never cancels one.
    /// Supplied by [`crate::replay::RestoreReport::apply_to`] and
    /// [`crate::replay::RestoredState::emulated`]; empty (default) = nothing emulated. Consumed entirely
    /// at assembly.
    pub emulated: Vec<vike_journal::JournalSnapshotOrderEmulation>,
    /// Whether a held order's release is driven by the boundary timer (`TimerKind::Release`). `true`
    /// (default) on every core but the REPLAY core, whose releases are the journaled `Command` records it
    /// re-applies: a timer there would read a clock stamp per boundary and shift every stamp of the
    /// tail it refolds.
    pub release_timer: bool,
    /// `true` on the REPLAY core alone (`crates/vike-core/src/replay/refold.rs`'s `run_replay_core`): only it lowers a
    /// `Command::OrderOn` whose route key names a mount as that mount's order (decision 0270); every other core refuses it.
    /// It also never places an amend's replacement at its own drain-loop boundary, and reads no clock there: the journal's
    /// record of the replacement places it (`crates/vike-core/src/runtime/apply/amend.rs`'s `place_journaled_replacement`).
    pub replay_core: bool,
    /// `true` when the boot restore resumed [`Self::order_client_id_session`] (set by `RestoreReport::apply_to` alone): arms the
    /// one-time switch to a fresh v2 session at assembly (decision 0270's D1); a session a caller hands in is kept.
    pub order_client_id_switch: bool,
    /// This install's PAST order client id sessions, `(session, last sequence)` each (a v1 session before the v2
    /// switch, a v2 one past its roll, a journal moved aside): an id one of them minted below its last sequence is
    /// this install's (`vike_model::orders::client_id::order_client_id_minted_by`), so a late fill on it is OURS
    /// in its execution report, with its account and mount, never foreign. The current session is the live
    /// generator's. Read only by the report's ownership check, on a cold path. Empty (default) = no past session
    /// known: an id of one reads foreign unless the live registry holds it.
    pub order_client_id_past_sessions: Vec<(String, u64)>,
    /// The order client id ranges the journal reserved for COUNTED denials, `(session, from, to)` each (its
    /// `vike_journal::JournalRecord::DeniedRepeat` records): the generator of the session it resumes never mints inside
    /// one (`OrderClientIdGenerator::reserve`), so a replay core mints, past a range, the ids the live core minted.
    /// Set by the refold alone (`crates/vike-core/src/replay/refold.rs`'s `run_replay_core`); empty (default) on a live
    /// core, whose restored sequence is past every range already.
    pub order_client_id_reserved: Vec<(String, u64, u64)>,
    /// **Where the host records the order client id session this core mints under, BEFORE it mints** (the ownership
    /// check above): called once, on the spawning thread, right after assembly and before the core thread starts,
    /// with the session and the sequence it starts at. The tradehub writes its `vike.db` row there, so an id this
    /// session mints is this install's at every later boot, whatever becomes of the journal (a journal moved aside
    /// names its sessions nowhere a boot can read). `None` (default): nothing is recorded (a replay core, a test).
    /// The core names no store: the host builds the closure.
    pub order_client_id_session_recorder: Option<OrderClientIdSessionRecorder>,
    /// The per-mount fill-ATTRIBUTION ledgers the JOURNAL carries (multi-mount durability, gap D):
    /// one row per mount, resolved onto its slot by `mount_id`
    /// ([`crate::strategy_state::mount_id_with`]) at [`assemble_core`]. Supplied by
    /// [`crate::replay::RestoreReport::apply_to`] (the latest `Snapshot`'s rows) or
    /// [`crate::replay::RestoredState::mount_attribution`].
    ///
    /// ⚠ **The ownership file wins** (decision 0127, amending 0113's verdict 4): a row is applied
    /// only for a mount id [`Self::order_owners`] holds NO ledger for. The file records a ledger on
    /// every attributed fill, the `Snapshot` only at its cadence, so on a crash the file is the fresher
    /// witness. A row naming no mounted slot waits in the pending-owner table like a file ledger.
    /// Empty (default) = every mount the file does not know starts with a zeroed ledger. Consumed
    /// entirely at assembly.
    pub mount_attribution: Vec<vike_journal::JournalSnapshotMountAttribution>,
    /// Where each gateway ring stood when the journal last said so (journal v18): one
    /// `(route key, cursor)` per account, supplied by [`crate::replay::RestoreReport::apply_to`] (the
    /// latest `Snapshot`'s cursors, advanced over its tail's `GatewayEvent` records). The core only
    /// CARRIES them: seeded into `CoreThread::gateway_cursors` at [`assemble_core`] so the next
    /// `Snapshot` keeps the cursor of an account that has stayed quiet since; whoever resumes a gateway
    /// session reads the same list from [`crate::replay::RestoreReport::gateway_cursors`]. Empty
    /// (default) = no gateway, or a journal that never held one. Consumed entirely at assembly.
    pub gateway_cursors: Vec<(String, vike_model::EventRingCursor)>,
    /// The node's LATE-RETRY memory (decision 0270, D7): `(order client id, created_ms)` of every id it saw before
    /// this boot within the guard's window, as `vike_journal`'s `Snapshot.recent_order_client_ids` carries it. The core CARRIES
    /// it: moved into `CoreThread::recent_order_client_ids` at [`assemble_core`], and every `Snapshot` writes it again. Empty
    /// (default) = no memory, today's behaviour. Consumed entirely at assembly.
    pub recent_order_client_ids: Vec<(String, i64)>,
    /// Every mount's CLOSED-TRADE ledger the journal carries (decision 0271 item 8,
    /// `crates/vike-core/src/runtime/closed_trades.rs`'s "A restart"): the latest `Snapshot`'s `closed_trades`, per
    /// mount id, each with the closed-trade archive's rows in front of its own (`rows` is the whole list,
    /// `archived` how many of them the archive holds on disk), supplied by
    /// [`crate::replay::RestoreReport::apply_to`]. A mount of that id resumes it when it
    /// comes up, at either door, with [`Self::closed_tail`] booked on it. Empty (default) = no journal, or one
    /// written before the key: a mount then holds its restored position as one lot. Consumed at assembly (a
    /// ledger for a mount not mounted yet waits for its runtime mount).
    pub closed_trades: Vec<vike_journal::JournalSnapshotClosedTrades>,
    /// What those ledgers book from the journal's tail, the fills and liquidations after that `Snapshot`, in order
    /// ([`crate::replay::TradeClosedTail`]): booked at assembly through the call that booked them live. Empty
    /// (default) = nothing after the `Snapshot`, or a restore that did not fold the tail.
    pub closed_tail: Vec<crate::replay::TradeClosedTail>,
    /// The orders the JOURNAL says a mount left RESTING (decision 0127), as ownership candidates:
    /// [`crate::replay::RestoreReport::apply_to`] supplies them (`resting_orders` in the replay folds:
    /// non-terminal in a restored registry, plus held exits — never "every order client id ever minted").
    ///
    /// They SUPPLEMENT [`Self::order_owners`] and never override it: an order client id the file knows keeps
    /// the file's owner (a different journal owner is noted `RESTORE OWNER CONFLICT`) and gains only
    /// the facts the file lacks (venue, symbol, account, price); an order client id the file does not know is
    /// owned as the journal says, recorded into the file, and goes through the same restore as a
    /// file entry (`restore_owned_order`, or the pending-owner table for a mount that is not
    /// mounted). None of them enters an engine registry at boot: the reconcile pass adopts what the
    /// venue still holds (decision 0120). Empty (default) = the file alone, today's behaviour.
    pub journal_orders: Vec<crate::replay::OrderOwnerCandidate>,
    /// The engine snapshots the JOURNAL restores account state from (decision 0127): each is
    /// matched to a live engine by ROUTE KEY (`EngineSnapshot::route_key`, the venue when `None`)
    /// and handed to `Engine::restore_runtime_state` — positions, realized PnL, fees,
    /// balances, all three fill-dedup sets and the gate's rate window, atomically. Never the risk
    /// limits. Its trading state applies only when STRICTER than the engine's (`Active` <
    /// `Reducing` < `Halted`). A snapshot that matches no engine, or that cannot be told from a
    /// sibling (two snapshots of one venue without a route key, the v16 shape, or a keyless one
    /// whose symbol or quote asset differs from the live engine's), is NOT applied and is noted
    /// `RESTORE ENGINE SKIPPED`. Empty (default) = every engine starts from its mount's state.
    ///
    /// A snapshot that STATES a tier other than its engine's (`vike_execution::EngineSnapshot::tier`) is
    /// refused the same way (`boot_restore.rs`'s `refuse_other_tier`): ERROR and HALTED over an open
    /// position, WARN over a flat book, its trading state carried when stricter, its book saved under
    /// [`Self::state_dir`]'s `refused-books/`, and (a journal being on) one `Snapshot` written at once so
    /// the refusal does not repeat at a boot that follows a crash.
    pub restored_engines: Vec<vike_execution::EngineSnapshot>,
    /// **The composition root's own steps over a journaled engine before the core restores it**, run by the core
    /// wherever it restores one, so the boot and a late join are ONE path: at the assembly over
    /// [`Self::restored_engines`] against the engines it holds, and at an account's late join over the book held for
    /// it against the joining engine (`late_join.rs`). A `fn(&mut [EngineSnapshot], &[&Engine<C>])` over the core's
    /// client type, type-erased as `vike_execution::AccountChange::AddEngine`'s engine is (one of another type is
    /// refused with a note). `vike-tradehub-core`'s node sets its two snapshot steps; `None` (default) runs none, as a
    /// replay core does.
    pub restored_engine_steps: Option<Box<dyn std::any::Any + Send>>,
    /// `(order client id, route key)` rows for orders that PREDATE this core, written into
    /// `CoreThread::order_client_id_venue` at assembly (a row only for an engine that needs one:
    /// `order_client_id_row_needed`) so a replay core routes a lifecycle event, a fill or a held exit's release
    /// of an order its base `Snapshot` already holds to the engine that holds it, as the dead core's
    /// submit-time map did. The boot restore's refold is the only writer
    /// (`crate::replay`'s `refold_records`, for a base whose engines an order's venue cannot tell
    /// apart); a live core starts the map empty and fills it at submit, which is why this is not a
    /// restore feature: the reconcile pass adopts a restored order into its engine's registry and
    /// writes the row itself. A key no engine carries is skipped. Empty (default) = nothing seeded.
    pub order_client_id_routes: Vec<(String, String)>,
    /// `(order client id, mount id)` of orders that PREDATE this core, written into `CoreThread::order_client_id_mount` at
    /// assembly for a mount it holds (no other restore step), so an amend of such an order is placed as its
    /// mount's, as the dead core placed it. The REPLAY core's alone (`crates/vike-core/src/replay/refold.rs`'s
    /// `run_replay_core`); a live core restores ownership through [`Self::journal_orders`] and the file.
    /// Empty (default) = nothing seeded.
    pub order_client_id_owners: Vec<(String, String)>,
    /// The journal `Snapshot` the boot restore took [`Self::restored_engines`] from (its `seq`), written
    /// by `RestoreReport::apply_to`; `None` when nothing was restored. It only NAMES the checkpoint in
    /// the note and the file of a refused book. Never read by the fold.
    pub restored_snap_sequence: Option<u64>,
    /// WHILE THE NODE WAS DOWN, in ms: `(the clock of the LAST record of the journal the boot restore folded, the
    /// boot restore's own clock)`, written by `RestoreReport::apply_to` only when the restore folded the journal
    /// through that record (a full restore, or a torn tail it refolded). The reconcile pass books a fill of our own
    /// restored order stamped strictly after the first and strictly before the FIRST venue link start in this process
    /// that reaches the account's book (`vike_execution::Engine::venue_link_started_ms`, after the boot), or before the
    /// boot when no link start is known (`crates/vike-core/src/runtime/reconcile.rs`'s
    /// `book_own_fills_filled_while_down`): a stamp a pass took when it READ the row (IBKR's socket client) is after
    /// both on one clock, so it never is (that function's doc declares the two-host residual). `None` (the default: no
    /// journal, a fresh or moved-aside one, a snap-only restore, every replay core) books none.
    pub restored_downtime_ms: Option<(i64, i64)>,
    /// `(arm_id, mount_id)` of every restored conditional a MOUNT armed (the journal's
    /// `OrderEmulationArmed` provenance): [`assemble_core`] gives the restored arm its
    /// `OrderEmulationBook::cond_engine` row back, so its fire books into the mount that armed it and
    /// lowers onto that mount's engine. Empty (default) = restored arms fire unattributed.
    pub conditional_owners: Vec<(String, String)>,
    /// The HELD exits a broken journal lost (decision 0127): each one's owning mount (per the
    /// ownership file) hears a synthetic terminal `OrderRejected`
    /// ([`crate::replay::RESTORE_LOST_REASON`]) at boot, so no protective order vanishes in
    /// silence. Empty (default) = nothing to tell.
    pub lost_held_exits: Vec<crate::replay::RestoreLostHeldExit>,
    /// What the boot restore owes the execution stream (package 2's C.1b): the execution reports a crash cut off
    /// that its refold regenerated, and the window it could not regenerate. [`assemble_core`] journals them right
    /// after the journal opens, before the boot `Snapshot` and the first dispatch. Set by `RestoreReport::apply_to`
    /// alone. Empty (default) = nothing owed.
    pub owed_execution_reports: crate::replay::OwedExecutionReports,
    /// Whether, and on what basis, [`assemble_core`] journals the boot's POSITION SNAPSHOT (every account's open
    /// positions, or why they are unknown: `vike_journal::JournalRecord::PositionSnapshot`), right after
    /// [`Self::owed_execution_reports`]. Set by `RestoreReport::apply_to` alone, from how deep the restore went.
    /// `None` (default) = none is journaled.
    pub position_snapshot: Option<crate::replay::PositionSnapshotBasis>,
    /// Lines the boot restore puts on the recent-events ring at assembly, in order (its startup
    /// line, a torn or broken journal, PROTECTION LOST). Empty (default) = none.
    pub restore_notes: Vec<String>,
    /// Whether a restored order whose engine is a PAPER book (`Engine::tier` is
    /// `AccountTier::Paper`) is reported GONE at boot (decision 0127): the paper client's book lives
    /// in memory, so a restart emptied it, and no reconcile pass will ever judge it — without this
    /// the entry waited for ever. Its mount hears a synthetic `Canceled`
    /// ([`crate::replay::RESTORE_PAPER_BOOK_LOST_REASON`]) and the ownership file forgets it. A FACT
    /// about the clients the composition root built, so the root states it: `vike-tradehub`'s live
    /// mount and `vike-tradehub-core`'s paper builders set it `true`. `false` (default; tests whose doubles
    /// report `Paper` while standing in for a venue) leaves such entries to the pass.
    pub restored_paper_orders_gone: bool,
    /// **The accounts the composition root knows and does NOT run, each with why** (decision 0156:
    /// an account runs at the tier its active row states, or not at all — never a paper engine in
    /// its place). No engine stands behind any of them, so a command addressed to one is refused as
    /// for any account this process runs no engine for, and the refusal NAMES THIS REASON
    /// (`crate::account_ambiguity::unheld_account_refusal`). Read only on that refusal path, an
    /// order-lowering path, never the fold. `vike-tradehub-core`'s `assemble_runtime` fills it from the mount's
    /// own answer; empty (default) names no reason, exactly as before the field.
    pub not_running: Vec<vike_execution::ArmingNotRunningView>,
    /// ORDER OWNERSHIP across a restart (`docs/decisions/0113-order-ownership-survives-a-restart-in-a-state-dir-file.md`):
    /// the file that remembers which mount owns each resting order, and each mount's ledger.
    /// [`assemble_core`] starts it: it loads the file and ALWAYS restores its entries onto the mounts
    /// by MOUNT ID (an entry whose mount is not mounted waits for a runtime mount of that id) — with
    /// a journal restore too, where the file is the authority on who owns an order and on every
    /// ledger and the journal ([`Self::journal_orders`], [`Self::mount_attribution`]) only fills what the
    /// file does not know (decision 0127; until then a journal restore skipped the file outright,
    /// which switched off decision 0120's adoption with it). From then on the core records every order a mount mints, every
    /// prune, every attributed fill's ledger and every runtime unmount — one channel send each, at
    /// order or fill cadence, never per market message. [`crate::order_owners`]' module doc is the
    /// contract.
    ///
    /// `None` (default — replay, the latency harnesses, every caller that predates the field) reads
    /// and writes nothing: byte-identical to a core without it. The daemon passes
    /// `OrderOwnerLog::in_state_dir` over the same directory it hands [`Self::state_dir`].
    pub order_owners: Option<crate::order_owners::OrderOwnerLog>,
    /// The operator off switch of ORDER RESTORE (the settings row `flags.reconcile_restore_off`, wired
    /// into this field by the composition root). `true` = [`assemble_core`] and a runtime mount that
    /// takes over pending ownership restore OWNERSHIP and ledgers exactly as before and nothing
    /// else: no strategy tag is put back into the tag registry, no `order_client_id_venue` row is written, and
    /// `CoreThread::restored_orders` stays empty (so nothing is adopted into the registry). `false`
    /// (default; replay, the latency harnesses, every caller that predates the field) restores them,
    /// for the entries the file carries facts for. Has no effect without [`Self::order_owners`].
    pub restore_orders_off: bool,
    /// The per-VENUE gate of order restore: the reconcile pass of a venue for which this answers
    /// `false` adopts nothing and tells no mount anything (the restored entries just wait).
    ///
    /// The default is the declared table, `vike_venues::venue_restore::restores_orders`,
    /// which says `true` only for a venue whose row a demo smoke has flipped. It is a plain `fn`
    /// pointer, not a setting and not an environment read, so no operator can raise it: a test (or
    /// a harness on a venue whose row is still off) substitutes `|_| true`, which is the only
    /// reason the field exists. It sits beside [`Self::restore_orders_off`], which is the operator's
    /// switch and the one a deployment turns.
    ///
    /// The one other writer LOWERS it: the live composition root closes it for every venue when
    /// the operator refused reconcile (`crates/vike-tradehub/src/reconcile_config.rs`'s
    /// `restore_venue_gate`), because no pass will ever come to adopt anything. A closed venue is
    /// the state this crate already handles: the restored entry waits, the tag refusal never
    /// applies to it (`CoreThread::restored_tag_holds`), and one boot note per venue says so.
    pub restore_venue_gate: fn(&str) -> bool,
    /// Opt-in strategy-state persistence sidecar (portfolio-observer PR-4 T2): when set, each
    /// mount's deterministic id ([`crate::strategy_state::mount_id_with`]) resolves to
    /// `<state_dir>/<mount_id>.msgpack` (an older `<mount_id>.json` is still read:
    /// [`crate::strategy_state::read_state`]). `assemble_core` loads that sidecar (if present) into the
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
    /// `Strategy::save_state` -> `write_state_atomic` sidecar write [`Self::state_dir`]'s doc
    /// describes for the clean-shutdown save, just on a running cadence instead of (in addition
    /// to) shutdown only. Mirrors [`Self::equity_sample`]'s timer machinery almost exactly; the
    /// one difference is the arm condition does NOT check open positions — a strategy's durable
    /// state (a breaker's trip count, an A-S accumulator, ...) matters whether the book is flat
    /// or not. Entirely COLD-path (drain-loop boundary arm/disarm + a self-rescheduling
    /// [`TimerWheel`] entry — never the per-message fold). `None` (default), OR a `Some`
    /// with [`Self::state_dir`] left `None`, is zero overhead: no timer is ever armed and the
    /// fold path is byte-identical to a save-timer-free runtime.
    pub state_save: Option<Duration>,
    /// Timer-armed equity sampler (portfolio-observer PR-3), gated behind `Some`: while ANY
    /// position is open (see [`CoreThread::any_position_open`]), on this wall-clock cadence,
    /// resolve per-venue + cross-venue equity (`Engine::resolve_equity`) and hand the
    /// batch to [`Self::on_equity_sample`]. Entirely COLD-path — the drain-loop boundary
    /// arm/disarm plus a self-rescheduling [`TimerWheel`] entry — NEVER the per-message
    /// fold. `None` (default) = zero overhead: the boundary check is a single
    /// `Option::is_some()` read, no timer is ever armed, and the fold path is byte-identical to
    /// a sampler-free runtime.
    pub equity_sample: Option<Duration>,
    /// The equity sampler's sink: the CALLER decides where a batch goes — persistence, display, or
    /// nowhere — through this closure. Fired with the full batch — one row per engine (primary +
    /// extras, registration order) plus a `"TOTAL"` cross-venue row (`compensated_sum` laws, matching
    /// `CoreSnapshot::equity_total`) — each time `equity_sample` fires. `None` (default) = no
    /// sink; the sampler still arms/fires when `equity_sample` is `Some`, but the finished batch is
    /// simply not delivered anywhere (set both together in practice).
    ///
    /// ⚠ This opened "Option B: vike-core takes NO vike-data dependency" until 2026-09-28, and the
    /// crate declared one until `CoreLaneSink` left for `vike-tradehub-core`: `vike-data` with DEFAULT
    /// features, the trait-only half
    /// (`vike_tradehub_core::CoreLaneSink` implements its `DataClientSink`). What keeps the sink a closure is
    /// the half that stayed true — the core holds no store and cannot open one. The concrete store
    /// and its record writer live in `vike-store`, which this crate must not name, and
    /// `docs/decisions/0084-only-the-datahub-touches-the-store.md` gives the store one reader, the
    /// datahub, and two writers, the backfill collectors and the recorder inside the datahub.
    /// Persisting a sample is the caller's to arrange.
    pub on_equity_sample: Option<EquitySampleHook>,
    /// Journal cross-check (#3): builds a [`vike_execution::recon::ReconcileJournalView`] for a venue from the
    /// materialized Tier-2 exec log (unified-journaling #2). `Some` unlocks the three-way
    /// (local-vs-venue-vs-journal) reconcile so a persisted fill the live `Account` lost surfaces as
    /// a `JournalDivergence` alert. Called on the fold thread at reconcile time only (an OCCASIONAL
    /// command — a bounded lookback query, off the p99 hot path). `None` (default) = two-way
    /// reconcile, byte-identical — and `None` is what every live mount passes today: no root builds
    /// one (`crates/vike-tradehub-core/src/journal_view.rs` carries why the last one went).
    ///
    /// ⚠ This ended "the store read lives in `vike-app-core` (which deps both vike-core and
    /// vike-data); vike-core cannot read the store itself" until 2026-09-28, and neither half
    /// holds. The READ is `vike-tradehub-core`'s: `vike_tradehub_core::journal_view_from_store` walks any
    /// `vike_data::HistoryStore` it is handed, and it is the call a hook here would make. What the
    /// core cannot do is OPEN a store — none is constructible from its tree, since it has no
    /// `vike-data` edge at all — and per
    /// `docs/decisions/0084-only-the-datahub-touches-the-store.md` every reader but the datahub
    /// reaches the store over the wire, so the store a hook reads is one its host constructed and
    /// handed in. Nor does the core choose the clock or the lookback the walk is scoped by. Those
    /// are why this is a closure a root builds.
    pub journal_view_provider: Option<ReconcileJournalViewHook>,
    /// Resolver knobs the equity sampler AND `snapshot::build` both resolve prices through
    /// (threading deferred from PR-2's `Engine::resolve_equity`). Permissive default
    /// (`PriceBoardConfig::default()`: mark enabled, no freshness windows) is behavior-preserving for
    /// every caller that never touches this.
    pub price_config: PriceBoardConfig,
    /// How long a REAL venue mark keeps ownership of the account mark slot (ms).
    ///
    /// The rule itself is NOT here and NOT in any lane: it lives in `Account::set_mark_from`,
    /// keyed by [`vike_execution::MarkSource`], which is the only door into the (private) `marks` map.
    /// This field is only the WINDOW, pushed onto every engine's `Account` at spawn — a knob, not
    /// a policy. See [`vike_execution::MarkSource`] for the law and its consequences.
    ///
    /// Default 10s = ~10 missed ticks of a 1s venue mark stream. `0` disables ownership entirely
    /// (pure last-write-wins, the pre-law behavior).
    pub mark_staleness_ms: i64,
    /// How long a RECONCILE mark ([`vike_execution::MarkSource::ReconcileMark`]) keeps ownership of the
    /// account mark slot (ms) — the per-source twin of [`Self::mark_staleness_ms`] for the
    /// reconcile cadence, which samples the venue's mark far less often than a 1s stream.
    ///
    /// Sized to exceed the default reconcile interval (`config.reconcile_interval_ms`, 60s), so a
    /// reconciled stream-less venue's slot stays owned by the reconcile mark CONTINUOUSLY between
    /// passes instead of expiring each minute and alternating with candle closes. Default 150s
    /// (2.5× the default reconcile interval); closes reclaim the slot only if reconcile STOPS for
    /// longer than this. Pushed onto every engine's `Account` at spawn (a knob, not a policy —
    /// the law lives in `Account::set_mark_from`). `0` makes reconcile-mark ownership expire
    /// immediately.
    ///
    /// ⚠ **The window matters only while something writes a reconcile mark, and nothing in a
    /// shipped binary does.** The one writer is `Engine::apply_snapshot` (the snapshot's
    /// `position_mark_price`), which no runtime path calls (tests and the frozen `fixtures/r5/` replay
    /// only), while the live reconcile driver sends `Command::ReconcileReports`, which files no
    /// reconcile mark. So this
    /// is NOT a gate on any reconcile pass: an interval longer than the window skips, delays and
    /// blocks nothing, it would only let a reconcile mark's slot lapse between passes once a writer
    /// is wired. A different interval hazard is real, though: a `config.reconcile_interval_ms`
    /// above `config.reconcile_lookback_ms` leaves a gap in what reconcile re-reads (the live fill
    /// stream still covers it; reconcile is the backstop).
    pub reconcile_mark_staleness_ms: i64,
    /// How far back this node's reconcile pass asks a venue for fills, ms (`config.reconcile_lookback_ms`, which the
    /// root resolves): each engine's journal `Snapshot` keeps that much of its two trade-id sets, plus a margin
    /// (`vike_execution::Engine::size_fill_window`), imposed at spawn like the windows above. `None` (the default)
    /// leaves each engine's own: the widest a read-back can be, or the one the `Snapshot` a replay core was rebuilt
    /// from carried.
    pub reconcile_lookback_ms: Option<i64>,
    /// Opt-in per-mount READINESS GATE (portfolio-observer PR-4 T5): when `true`, a freshly
    /// mounted strategy starts `Pending` (see [`MountState`]) and stays that way until its
    /// symbol actually prices through the [`vike_execution::price_board::PriceBoard`] (checked at
    /// the drain-loop boundary, [`CoreThread::maintain_mount_readiness`], using [`Self::price_config`]
    /// — the SAME resolver knobs the equity sampler and `snapshot::build` already price
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
    /// stuck-order watchdog, driven by the same [`TimerWheel`] at the drain-loop boundary,
    /// NEVER the fold) watches how long market data / venue events have been silent; if that
    /// exceeds [`DeadManSilenceConfig::timeout`] it TRIPS once — cancelling every resting order through the
    /// one order-write path and, for [`vike_model::DeadManAction::CancelAllAndHalt`], engaging HALT (in-process
    /// `trading_state = Halted` + the cross-process HALT sentinel file). It re-arms on recovery.
    /// `None` (default) = DISABLED: no [`deadman::DeadManSilence`] is built, no timer/waker is armed for
    /// it, and the per-message fold never touches it — byte-identical to today. See
    /// [`deadman`] for the full contract.
    pub deadman: Option<DeadManSilenceConfig>,
    /// The CONNECTION-state DEAD-MAN (M13) — the SUCCESSOR to [`Self::deadman`], not a variant of
    /// it. When `Some`, a cold-path timer watches the per-`(venue, symbol)` [`FeedStatus`] the
    /// bridges disclose: a `Disconnected` on an armed venue opens a grace window, a `Live` closes
    /// it, `Stale` is ignored entirely, and a link still down when
    /// [`DeadManLinkConfig::grace`] expires cancels THAT VENUE's resting orders (plus, for
    /// [`vike_model::DeadManAction::CancelAllAndHalt`], a HALT of THAT venue's engines alone, released when its
    /// links are Live again; no sentinel file). `None` (default) = DISABLED:
    /// no latch is built, no timer or waker is armed for it, and the fold never touches it —
    /// byte-identical to a runtime that never heard of the feature.
    ///
    /// ⚠ Independent of [`Self::deadman`] in both directions: either, both or neither may be
    /// `Some`. They observe DIFFERENT signals (silence vs. the disclosed link state) and only
    /// share [`vike_model::DeadManAction`]. [`link_deadman`]'s module doc carries the re-ruling that made this
    /// one the default and the other one opt-in.
    pub link_deadman: Option<DeadManLinkConfig>,
    /// The risk clock, and the venues whose engines NO MOUNT drives (price-only feeds: the control door's
    /// on-demand pumps), see [`PriceOnlyConfig`]. When `Some`: a cold-path timer sweeps EVERY live engine's
    /// margin call and the account-wide drawdown latch, mounted engines included (a mount's closed bar may
    /// be hours apart, and never reaches a price-only engine), and the link dead-man acts on the price-only
    /// venues only while they hold something (an admitted symbol or a live order). `None`
    /// (default) = no timer, no waker, nothing on the fold: byte-identical. Like [`Self::link_deadman`]
    /// it arms the waker record, because the latch it can flip is wall-clock driven.
    pub price_only: Option<PriceOnlyConfig>,
    /// Opt-in MANAGED GTD EXPIRY sweep cadence (core-ergonomics): how often the core re-checks
    /// resting orders for a passed `gtd_expiry` and cancels them ([`CoreThread::sweep_gtd_expiry`]).
    /// Runs on the EXISTING [`TimerWheel`] at the drain-loop boundary — no new thread, and
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
    /// features' own wall-clock reasons ([`crate::replay::JournalReplayError::Unsupported`]).
    pub gtd_sweep: Option<Duration>,
    /// Opt-in PERIODIC PORTFOLIO SNAPSHOT cadence (core-ergonomics): on this cadence the core
    /// appends ONE compact [`vike_journal::PortfolioSample`] record (per-venue equity/balance/
    /// realized + every open position) to the write-ahead journal — a low-rate equity/positions
    /// time series for downstream reporting, without the GUI having to poll a running process.
    ///
    /// WHY THE JOURNAL (the "least-invasive existing off-fold path" choice): the SQLite `exec_db`
    /// writer is retired, so the journal is the only
    /// surviving durable off-fold sink the core already owns — it is already opened, already
    /// framed/checksummed/segmented, already pruned, and already the log the tradehub's execution
    /// stream reads back (decision 0295). Writing here therefore adds
    /// no file handle, no thread and no new dependency edge. The record is REPLAY-NEUTRAL
    /// (`replay.rs`'s tail extraction ignores it, exactly like `MintedSubmit`): it is an
    /// observation, never a command to re-apply.
    ///
    /// ⚠ This said "already read by `vike-app-core`'s materializer" until 2026-09-28. The
    /// materializer (vike-journal's from 2026-09-25, the data daemon's from 2026-10-11) passed over
    /// these records, and the execution stream that replaced it on 2026-11-18 does not send them —
    /// so nothing in the tree reads a `PortfolioSample` back today.
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
    /// NOT bump `journaled_since_snap` — that counter paces the REPLAY-BASE `Snapshot` cadence, which
    /// stays a function of folded commands. Consequence: an otherwise IDLE journaling core (no
    /// exec-lane traffic at all) never reaches the `snapshot_every` threshold, so no `Snapshot` is
    /// written, so `prune_before_latest_snap` has no floor to prune to and segments accumulate in
    /// the live directory for the life of the session (the shutdown `Snapshot` is the first prunable point). A 1s cadence on a
    /// silent weekend is ~86k unprunable records/day. Size the cadence for the session length —
    /// seconds-to-minutes for a reporting series, not sub-second.
    pub portfolio_snapshot_interval: Option<Duration>,
    /// Opt-in FAST IN-FLIGHT CONFIRM cadence + age threshold (recon path-to-superset, F1-A; modelled
    /// on the reference engines' ~2s in-flight order check). On this cadence the core issues a REJECT-FREE
    /// venue re-query ([`vike_execution::Engine::confirm_order`]) for every order stuck
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
    /// and no dedup state is ever written. Suggested live value ~2s. ⚠ No root sets it today: the
    /// variable that was meant to was read by a helper nothing called, and both are deleted
    /// (decision 0111). REPLAY STAYS INTACT: the sweep's only action is
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
    /// to its paper arm through `vike_tradehub_core::StrategyCorePaperConfig`. ⚠ This said "`vike-app` reads
    /// `VIKE_OCO_CANCEL_SIBLING_ON_DEAD_EXIT=1`" until 2026-09-28; the desktop builds no
    /// `CoreConfig` since its local core was deleted (#1727, 2026-09-09).
    pub oco_cancel_sibling_on_dead_exit: bool,
    /// Opt-in per-mount loss/notional BUDGET (steal/core-per-mount-budget), keyed by the MOUNT ID —
    /// `Mount::controller_id` when the mount carries one, else the legacy
    /// `{venue}__{symbol}__{interval}` derivation ([`crate::strategy_state::mount_id_with`], the ONE
    /// mount-identity law). A mount whose key is present (and whose [`MountBudget`] has an active
    /// arm) is watched on the SAME per-closed-bar cadence as [`Self::max_drawdown`]; on breach it
    /// latches liquidate-only (cancel its resting orders + optional flatten) while every other mount
    /// keeps trading. Empty (default) = no per-mount latch, byte-identical to today (the sweep is
    /// never even armed — see `CoreThread::any_mount_budget`).
    ///
    /// KEYED HERE, not as a field on [`Mount`]: the id resolves each budget onto its mount at
    /// [`assemble_core`], and a key naming no mounted id is simply never matched (inert), never an
    /// error.
    ///
    /// **Why the MOUNT ID and not the `(venue, symbol, interval)` triple** (which is what this map
    /// used to be keyed by): the triple is COARSER than mount identity. Since
    /// [`Mount::controller_id`] landed, two mounts may legitimately share one triple — that
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
    /// `"maker_a"`. Build the key with `mount_id_with` rather than by hand.
    ///
    /// A budget's LEVERAGE arm ([`MountBudget::max_leverage`]) is no latch and no arm of the sweep:
    /// the drain judges it per OPENING order of the mount on the mount's own book
    /// (`CoreThread::leverage_refusal_on`; an order held for its release at the release), so a budget
    /// that states only it watches nothing on the clock.
    pub mount_budgets: std::collections::HashMap<String, MountBudget>,
    /// **The budget of every mount that [`Self::mount_budgets`] names no key for** — the SOURCE the
    /// map never had (decision 0249). `None` (default) = no default budget: a mount without its own
    /// key is watched by nothing, byte-identical to before this field. `Some(b)` gives each such
    /// mount `b` when it is assembled AND when it is mounted at run time (`Command::MountStrategy`:
    /// a ceiling that a later mount could slip under is no ceiling), and an inactive `b`
    /// ([`MountBudget::is_active`] false) is inert exactly like an inactive keyed entry. A key in
    /// [`Self::mount_budgets`] wins over it, so a mount given its own budget is never also held to
    /// this one.
    ///
    /// `vike-tradehub`'s live mount fills it from the `policy.max_mount_loss` / `policy.mount_loss_action`
    /// rows (`crates/vike-tradehub/src/venue_arming/mount_budget.rs`'s `mount_budget_default_from_policy`);
    /// nothing else does. Read ONCE per mount, when its slot is built: it is cold, judged by the
    /// existing per-mount budget sweep on the 30 s risk clock and on every closed bar (its leverage
    /// arm by the drain, per opening order, [`Self::mount_budgets`]' last paragraph), and adds
    /// nothing to the per-message fold.
    pub mount_budget_default: Option<MountBudget>,
    /// Opt-in per-mount WALL-CLOCK SCHEDULE (steal/core-live-scheduler), keyed by the MOUNT ID —
    /// the same key [`Self::mount_budgets`] uses, for the same reason: a same-triple mount PAIR
    /// (which [`Mount::controller_id`] permits) used to share ONE schedule and could not be
    /// given two.
    /// Each [`crate::schedule::MountSchedule`] registers wall-clock
    /// [`vike_model::time::schedule::TimeRule`]s (daily-at-HH:MM in a fixed-offset tz,
    /// market-open/close offsets) that fire
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
    pub mount_schedules: std::collections::HashMap<String, MountSchedule>,
    /// The strategy FILE each mount runs (its path and the sha256 of the bytes the composition root read),
    /// keyed by the MOUNT ID ([`Self::mount_budgets`]' key, built with `mount_id_with`). Journaled once per
    /// mount as it opens (`vike_journal::JournalRecord::StrategyFileMounted`: at assembly for a boot mount,
    /// in the runtime mount arm for one the [`StrategyFactory`] resolved) and carried on the mount's own row
    /// of every `Snapshot`, so "which version of the file traded" survives a restart. Empty (default) =
    /// byte-identical: a mount no file names (a test's, a replay core's) writes no record and no key. Cold:
    /// read when a mount opens and once per mount per `Snapshot`, never per message.
    pub mount_strategy_files: std::collections::HashMap<String, vike_journal::JournalStrategyFile>,
    /// **The mounts whose strategy file declares `fill = "close"`**, by MOUNT ID ([`Self::mount_strategy_files`]'
    /// key): on a PAPER engine, the orders such a mount places on a bar fill at that bar's close the moment its
    /// step drains (`ExecutionClient::fill_at_close`, decision 0285's amendment of 2026-11-16), as the backtest
    /// fills them; every other mount's wait for the next bar's first price. A demo or live engine sends them as
    /// it always did (a market order sent at the close meets it). A boot mount is named here by its host; a run-time
    /// mount by the core itself, from what its factory read ([`MountStrategyFile::fill_at_close`]). Empty (default) =
    /// byte-identical. Cold: read once per bar step of a mount on a paper engine.
    pub mount_fills_at_close: std::collections::HashSet<String>,
    /// **Whether the forecasts strategies publish are JOURNALED** (book W35; the run profile's `sinks.forecasts`,
    /// REVIEW-LIVE-MONEY M1), at most 512 a mount's step (`MAX_FORECASTS_PER_STEP`, M3: the rest are refused in
    /// words and counted). `false` (the default) =
    /// a forecast a step publishes is dropped at its drain: no `vike_journal::JournalRecord::Forecast` is
    /// written, so the journal never moves to the forecast format. ⚠ `true` raises the node's journal segment to
    /// format 32 at its first forecast, and a build that reads only 31 then refuses the journal at cold start:
    /// a rollback past that point needs every account flat (the journal move-aside). Cold: one bool read per
    /// drain of a step that published something.
    pub journal_forecasts: bool,
    /// **The mounts whose STRATEGY arrives later**, by MOUNT ID: a `.rs` strategy file the host is still
    /// building when the core starts (the owner's ruling of 2026-11-14: the start never waits for a build).
    /// [`assemble_core`] PARKS each such mount instead of mounting it, so its placeholder strategy never runs,
    /// loads or saves anything, and the mount gets its slot when [`vike_execution::AccountChange::AddMounts`]
    /// brings a mount of that id: its strategy goes onto the parked mount's own fields (the account the
    /// assembly named for it included), and it mounts as a deactivated one does, its strategy file journaled
    /// then. A parked mount whose strategy never comes (a failed build) is never mounted. Empty (default) =
    /// byte-identical: nothing is parked. Cold: read once at assembly and per `AddMounts`.
    pub mounts_awaiting_strategy: Vec<String>,
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
    /// orders", never "get me out" ([`vike_execution::OrderIntent::MarketExit`] is that verb, and it is an
    /// operator action, not something a stop may decide on its own). It is BEST-EFFORT like every
    /// other cancel path here: the cancels are queued to each venue's `ExecutionClientActor` before the detach
    /// and drained by that actor's own teardown, but no venue `OrderCanceled` is waited for. The
    /// whole teardown is hard-capped by the caller's deadline
    /// (`vike_boot::shutdown::run_with_deadline`), which abandons whatever is still in flight, so a
    /// wedged venue can never stop the process exiting.
    ///
    /// ⚠ IT CANNOT RUN ON A PATH THAT NEVER REACHES TEARDOWN — but as of the graceful-stop change
    /// `systemctl stop` IS such a path. This comment previously said the opposite, in capitals: that
    /// no signal handler existed, so SIGTERM ended the process where it stood and this flag was inert
    /// under a service. That was true until `vike_boot::stop`'s handler landed and
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
    /// [`vike_execution::MountSpec`]'s `name`/`rhai`+`params` vocabulary into a live
    /// `Box<dyn Strategy<LiveBroker>>` when a [`Command::MountStrategy`] arrives. Injected by the
    /// COMPOSITION ROOT (vike-core sits below the strategy crates — vike-strategy/vike-rhai are
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
    /// **Charge a PAPER engine its venue's public funding rate**, by `vike_model::ContractValue::funding`
    /// (the one formula the simulator books). `true`, which the composition root sets for its paper
    /// engines, arms the funding-rate lane: each `Ingest::FundingRate` keeps a funding clock on every
    /// engine whose STATED tier is paper and that holds the symbol, and the settlement the venue rolls
    /// past is booked on that engine's own position as a journaled `Event::Funding`
    /// (`runtime/funding.rs`). `false` (the default: funding is off unless asked, every replay core and
    /// every config that predates the field) books nothing from the lane; a live engine never does either way.
    pub charge_paper_funding: bool,
    /// THE VENUE'S STORED FUNDING HISTORY (the datahub's `interval=funding` series), read by a RESTARTED
    /// paper engine to catch up the settlements it missed while the node was down: asked once per
    /// restored funding clock, at the first funding observation after the restart, ON THE FOLD THREAD
    /// (cold, like a runtime mount's resolve), and each missed settlement the series holds is booked at
    /// its stored rate on the restored position (`runtime/funding.rs`). `vike-tradehub` hands in the
    /// datahub's strategy-lane read (`StrategyFundingHistory`). `None` (the default, every replay core)
    /// keeps the lane's old restart: the first settlement the venue rolled past is booked at the rate it
    /// stated last, and the rest are not.
    pub funding_history: Option<FundingHistory>,
    /// **WHERE A MOUNT'S INPUTS ARE READ** (decision 0271's live half, `runtime/inputs.rs`): an input name a
    /// strategy declares (`vike_model::Strategy::inputs`) to the series it reads and its stored history up to
    /// now, asked once per name at each mount door (spawn and run time), ON THE FOLD THREAD (cold, like a
    /// runtime mount's resolve, so the host bounds it); the host then keeps the series fresh on the bar lane
    /// under the key it answered. `vike-tradehub` hands in the datahub's strategy-lane read
    /// (`StrategyInputBars`) on both of its arms, the paper one included. `None` (the default: every replay
    /// core) reads no input, and a mount of a strategy that declares one is refused by name.
    pub input_opener: Option<InputOpener>,
    /// **The KEPT rows a restart's closed trades read a market's class from**: the datahub's lists of the venues whose
    /// row can name a session class (`vike_catalog::session_venue`), which `vike_catalog::kept_row` finds a mount's
    /// symbol in, for the calendar a crash-tail fill's bar is counted by (`runtime/closed_trades.rs`). `vike-tradehub`
    /// hands in what it read at spawn, on both of its arms. Empty (the default: every replay core): every market's
    /// class is unknown, so every tail counts by the plain bar clock.
    pub session_rows: Vec<vike_model::Instrument>,
    /// **Who hears the LEGS of a combo an engine ACCEPTED** (`apply/combo.rs`'s `lower_combo`, after the risk
    /// gate, as it writes them into the engine's `extra_symbols`): the host's half of a symbol admission. The
    /// core admits a combo's legs itself, with no `Command::AdmitSymbol`, so the host that tells the venue's
    /// reconcile client what the engine trades hears of them only here: `vike-tradehub` commits each leg to
    /// the account's gatewayhub (`TradeLeaseRegistry::combo_leg_teller`), as the control door commits an admitted
    /// symbol. Called once per accepted combo with its id and every leg but the engine's own symbol, on the
    /// fold thread inside that one command (never per message); it must not block, and the host's closure
    /// hands the legs to a thread of its own. `None` (the default: every replay core and the paper rehearsal
    /// cores) tells nobody.
    pub on_combo_legs: Option<ComboLegsHook>,
}

/// The [`CoreConfig::funding_history`] shape: `(venue, symbol, after, before)` to that series' stored
/// settlements strictly between the two epoch-ms bounds, ascending, each `(ts, rate, mark)` (the rate
/// per the venue's interval, the mark the venue published beside it or `None`), or why it could not be
/// read.
pub type FundingHistory =
    Box<dyn FnMut(&str, &str, i64, i64) -> Result<Vec<(i64, f64, Option<f64>)>, String> + Send>;

/// The [`CoreConfig::on_combo_legs`] shape: `(route key, the combo's order client id, legs)` of one accepted
/// combo, the legs as its request names them. The id is what lets the host tell a leg a RESTING combo still
/// needs from one nothing needs any more.
pub type ComboLegsHook = Box<dyn Fn(&str, &str, &[String]) + Send>;

/// The [`CoreConfig::order_client_id_session_recorder`] shape: `(session, the sequence it starts at)`, called once
/// before the core mints. Named for clippy's `type_complexity`.
pub type OrderClientIdSessionRecorder = Box<dyn FnOnce(&str, u64) + Send>;

impl Default for CoreConfig {
    fn default() -> Self {
        CoreConfig {
            seed_cash: 0.0,
            snapshot_interval: Duration::from_millis(16),
            recent_events_cap: 64,
            ingest_capacity: 8192,
            batch_max: 1024,
            clock: Box::new(ClockLive),
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
            submit_ack_confirm_grace: vike_model::DEFAULT_SUBMIT_ACK_CONFIRM_GRACE,
            margin_call: None,
            max_drawdown: None,
            conditionals_on_ticks: false,
            extra_mounts: Vec::new(),
            journal: None,
            order_client_id_session: None,
            instance_origin: None,
            arm_sequence: 0,
            order_emulation_arms: Vec::new(),
            contingencies: Vec::new(),
            held_releases: Vec::new(),
            emulated: Vec::new(),
            release_timer: true,
            replay_core: false,
            order_client_id_past_sessions: Vec::new(),
            order_client_id_reserved: Vec::new(),
            order_client_id_session_recorder: None,
            order_client_id_switch: false,
            mount_attribution: Vec::new(),
            gateway_cursors: Vec::new(),
            recent_order_client_ids: Vec::new(),
            closed_trades: Vec::new(),
            closed_tail: Vec::new(),
            journal_orders: Vec::new(),
            restored_engines: Vec::new(),
            restored_engine_steps: None,
            order_client_id_routes: Vec::new(),
            order_client_id_owners: Vec::new(),
            restored_snap_sequence: None,
            restored_downtime_ms: None,
            conditional_owners: Vec::new(),
            lost_held_exits: Vec::new(),
            owed_execution_reports: crate::replay::OwedExecutionReports::default(),
            position_snapshot: None,
            restore_notes: Vec::new(),
            restored_paper_orders_gone: false,
            not_running: Vec::new(),
            order_owners: None,
            restore_orders_off: false,
            restore_venue_gate: vike_venues::venue_restore::restores_orders,
            state_dir: None,
            state_save: None,
            equity_sample: None,
            on_equity_sample: None,
            journal_view_provider: None,
            price_config: PriceBoardConfig::default(),
            mark_staleness_ms: 10_000,
            reconcile_mark_staleness_ms: 150_000,
            reconcile_lookback_ms: None,
            readiness_gate: false,
            deadman: None,
            link_deadman: None,
            price_only: None,
            gtd_sweep: None,
            portfolio_snapshot_interval: None,
            inflight_confirm: None,
            oco_cancel_sibling_on_dead_exit: false,
            mount_budgets: std::collections::HashMap::new(),
            mount_budget_default: None,
            mount_schedules: std::collections::HashMap::new(),
            mount_strategy_files: std::collections::HashMap::new(),
            mount_fills_at_close: std::collections::HashSet::new(),
            // OFF: no forecast is journaled, so the journal's format never moves for one.
            journal_forecasts: false,
            mounts_awaiting_strategy: Vec::new(),
            schedule_poll: None,
            // OFF = today's behavior, byte-identical: teardown detaches and exits, resting orders
            // stay live at the venue. See the field doc for why that is the default.
            cancel_orders_on_shutdown: false,
            // No resolver = every runtime MountStrategy refuses (split-plane B5); byte-identical
            // for every config that predates the field.
            strategy_factory: None,
            // OFF = the funding-rate lane books nothing (funding is off unless asked; a replay core never
            // re-derives funding: it re-applies the journaled events).
            charge_paper_funding: false,
            // No history = a restart books only the first missed settlement, at its stated rate.
            funding_history: None,
            input_opener: None,
            // No kept rows = every market's class unknown: a tail counts by the plain bar clock.
            session_rows: Vec::new(),
            // No listener = nobody outside the core hears a combo's legs (they reach the engine as ever).
            on_combo_legs: None,
        }
    }
}

/// The [`CoreConfig::strategy_factory`] shape: resolve one runtime [`vike_execution::MountSpec`] into
/// the strategy object to mount and the strategy FILE it was built from (`None` for a mount no file
/// names; [`MountStrategyFile`] says what the core does with it), or a human-readable
/// refusal (surfaced verbatim in the core's recent-events ring). `FnMut` so a root's factory may keep
/// state (e.g. a compile cache).
pub type StrategyFactory = Box<
    dyn FnMut(
            &vike_execution::MountSpec,
        )
            -> Result<(Box<dyn Strategy<LiveBroker> + Send>, Option<MountStrategyFile>), String>
        + Send,
>;

/// What a [`StrategyFactory`] read from a runtime mount's strategy FILE, beside the strategy it built: the file
/// itself (journaled as the mount opens and kept on its `Snapshot` rows, [`CoreConfig::mount_strategy_files`]) and
/// whether its header declares `fill = "close"` ([`CoreConfig::mount_fills_at_close`]), so a mount made while the
/// node runs fills its bar orders on a paper engine exactly as one present at boot does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountStrategyFile {
    pub file: vike_journal::JournalStrategyFile,
    pub fill_at_close: bool,
}

/// Dequeue observer (latency harness / diagnostics). Runs under the core's panic guard.
pub type DequeuedHook = Box<dyn FnMut(&Ingest) + Send>;

/// The equity sampler's sink (portfolio-observer PR-3) — see [`CoreConfig::on_equity_sample`].
pub type EquitySampleHook = Box<dyn FnMut(&[EquitySample]) + Send>;

/// Builds a per-venue [`vike_execution::recon::ReconcileJournalView`] from the materialized exec log — see
/// [`CoreConfig::journal_view_provider`]. No production root supplies one today, so that field is
/// `None` at every live mount; the only hooks built are this crate's tests' (such as
/// `crates/vike-core/tests/recon/reconcile_journal_crosscheck.rs`'s `journal_has_t1`). A root that
/// wires one would build it over `vike_tradehub_core::journal_view_from_store`, handed a store it constructed.
///
/// ⚠ This said "Supplied by `vike-app-core` (which can read the store)" until 2026-09-28. The one
/// production hook was built in the GUI shell's `main.rs` and was deleted with that binary's local
/// core on 2026-09-09 (#1727); `crates/vike-tradehub-core/src/journal_view.rs` carries the rest.
pub type ReconcileJournalViewHook =
    Box<dyn Fn(&str) -> vike_execution::recon::ReconcileJournalView + Send>;
