//! CoreSnapshot — the immutable state view the core publishes to the GUI via arc-swap.
//!
//! The GUI is a LOSSY DOWNSTREAM OBSERVER (plan §1): it reads the latest snapshot on
//! repaint and can never back-pressure the core. Snapshots are built COALESCED (dirty-flag
//! with a ≥16 ms interval or on queue-idle, see `runtime.rs`), never per-event — the build
//! cost is off the per-event hot path by construction and measured in the latency harness.

use crate::portfolio::Portfolio;
use crate::runtime::MountBudget;
use vike_exec::{
    BalanceMode, ExecutionClient, ExecutionEngine, OrderStatus, PriceCfg, PriceSource,
    ResolvedEquity, TradingState,
};

#[derive(Debug, Clone, PartialEq)]
pub struct PositionView {
    pub venue: String,
    pub symbol: String,
    pub position_side: String,
    pub size: f64,
    pub avg_px: f64,
    /// resolver-priced unrealized PnL (PR-1 `PriceBoard` chain via `resolve_equity`);
    /// 0.0 when `mark_source` is `None` (no priceable source — identical to the legacy
    /// marks-based silent-zero).
    pub unrealized: f64,
    /// which price source resolved this position's mark; `None` == unpriceable (Missing).
    pub mark_source: Option<PriceSource>,
    /// effective leverage for this position's symbol (`1/im`); 0.0 when the margin gate is off.
    pub leverage: f64,
    /// estimated liquidation mark from the ONE scope-parameterized law at the ONE maintenance
    /// rate: Isolated → closed-form `vike_model::liquidation_price`; Cross →
    /// `vike_model::cross_liquidation_price_est` (the shared-pool line with every other position
    /// frozen — advisory, the venue-UI shape); Cash → 0.0 (never liquidates). 0.0 when the
    /// margin gate is off, the position is flat/unmarked, or it is not liquidatable by price.
    pub liq_price: f64,
    /// this position's margin mode (`Cross` default = whole-account collateral). Partitions the
    /// liquidation-badge pool above; the venue-report read side writes it (`apply_snapshot`).
    pub margin_mode: vike_model::MarginMode,
    /// allocated isolated-margin wallet for this position (account currency); `None` for cross.
    /// The read-side twin of `PositionEntry::isolated_margin` — inert carrier, no math reads it.
    pub isolated_margin: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OrderView {
    pub client_order_id: String,
    /// owning engine's venue (cross-venue: snapshot.orders spans ALL engines)
    pub venue: String,
    /// **WHICH ACCOUNT's book this order rests in.** The holding engine's label, `None` for the
    /// default account, so a single-account node (every node today) allocates nothing for it.
    /// Derived from the engine's route key exactly as [`VenueBlock::account`] is.
    ///
    /// ⚠ It exists because `venue` cannot tell two accounts of one exchange apart: on a node that
    /// runs two, a reader filtering `orders` by venue and symbol would mix both accounts' orders,
    /// and a cancel-all built on that filter would pull the other account's orders too.
    pub account: Option<vike_model::account_keys::AccountLabel>,
    pub symbol: String,
    pub side: i32,
    pub qty: f64,
    pub order_type: String,
    pub price: Option<f64>,
    pub trigger_price: Option<f64>,
    pub status: OrderStatus,
    pub venue_order_id: Option<String>,
    pub filled_qty: f64,
    pub avg_fill_px: f64,
}

/// One PENDING bracket exit the live runtime is HOLDING off the venue until its OTO parent fills
/// (live-runtime OCO/OTO). These are NOT in `orders` — they have no registered order and no venue
/// order id yet — so the GUI would otherwise be blind to a resting bracket's protective legs. Its
/// `parent_order_id` names the entry whose fill will release it. Empty on every non-bracket run.
#[derive(Debug, Clone, PartialEq)]
pub struct HeldOrderView {
    pub client_order_id: String,
    pub venue: String,
    pub symbol: String,
    pub side: i32,
    pub qty: f64,
    pub order_type: String,
    pub price: Option<f64>,
    pub trigger_price: Option<f64>,
    /// the OTO entry whose fill releases this exit
    pub parent_order_id: Option<String>,
}

/// One live mount's readiness view (portfolio-observer PR-4 T5) — the snapshot-facing twin of
/// the runtime's private per-mount `MountState`. `ready == false` means the mount is still
/// PENDING (see `CoreConfig::readiness_gate`): it is RECEIVING data — its strategy hook still
/// runs every tick/bar, so any warmup/estimator proceeds normally — but its buffered order
/// intents are DISCARDED before they ever reach the venue, because its (venue, symbol) has not
/// yet resolved a price (either side) through the engine's `PriceBoard`. When the gate is off
/// (`CoreConfig::readiness_gate: false`, the default) every mount is immediately `ready: true`.
///
/// The vec this rides in ([`CoreSnapshot::mounts`]) carries ONE extra row when any mount exists: the
/// [`MountRowKind::Residual`] row — see that variant. Read `kind` before treating a row as a mount.
#[derive(Debug, Clone, PartialEq)]
pub struct MountView {
    /// whether this row describes a real mount or the account-vs-mounts RESIDUAL
    pub kind: MountRowKind,
    pub venue: String,
    pub symbol: String,
    pub interval: String,
    pub ready: bool,
    /// steal/core-per-mount-budget — the per-mount fill ATTRIBUTION + BUDGET view (read-only; never
    /// drives a fold decision). Signed net position attributed to this mount (0.0 until it fills).
    pub position: f64,
    /// cumulative realized PnL NET of fees attributed to this mount.
    pub realized_pnl: f64,
    /// resolver-marked unrealized PnL on this mount's attributed net position (0.0 flat/unpriceable).
    pub unrealized_pnl: f64,
    /// gross notional exposure (resolver-priced) of this mount's attributed net position.
    pub notional: f64,
    /// this mount's optional loss/notional budget (`None` = no per-mount latch).
    pub budget: Option<MountBudget>,
    /// `true` once this mount breached its budget and latched liquidate-only.
    pub latched: bool,
    /// This mount's own LIVE tunables, read straight off its strategy (`vike_model::Strategy`'s
    /// `params`), or `None` when that strategy publishes none — its default. The read-side twin of
    /// `Command::UpdateParams`: what comes back is the bag that command takes, so an operator
    /// surface can patch one knob and send the rest back unchanged instead of re-typing (and
    /// silently reverting) the whole object.
    ///
    /// It is the strategy's CURRENT state, not the boot config the mount was built from — after a
    /// re-tune the two disagree, and only this one is what the next tick prices from.
    ///
    /// Cold path only: filled at the coalesced publish in `runtime::timers`' `mount_views`, never on
    /// the per-message fold. `None` on the [`MountRowKind::Residual`] row, which describes no mount.
    pub params: Option<vike_model::StrategyParams>,
}

/// What one [`MountView`] row describes (multi-mount durability, gap E).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MountRowKind {
    /// A real strategy mount: every field describes that mount's own attributed ledger.
    #[default]
    Mount,
    /// The RESIDUAL row — `Account.realized_pnl (net of fees)` MINUS the sum of every mount row's
    /// `realized_pnl`, i.e. the realized PnL that belongs to NO mount: manual operator tickets,
    /// margin-call and budget-latch liquidations, reconcile-adopted venue orders, and (across a
    /// restart) any fill whose originating mount could not be reconstructed.
    ///
    /// It exists because that difference was previously invisible: attribution is keyed by coid, an
    /// un-minted coid is simply not attributed, and nothing summed the two sides — so
    /// `Σ MountView.realized_pnl ≠ Account.realized_pnl` systematically and silently. With the row,
    /// `Σ (mount rows) + residual == account realized (net)` is an INVARIANT anything can assert,
    /// and an unexpectedly large residual is a visible signal rather than a slow leak.
    ///
    /// Its `venue`/`symbol`/`interval` are EMPTY (it is account-wide, not a series), `position`/
    /// `unrealized_pnl`/`notional` are `0.0` (only realized PnL is reconcilable this way — an
    /// unattributed OPEN position has no basis in any ledger), `budget` is `None` and `latched`
    /// `false`. Appended LAST, and only when at least one mount exists — a mount-free core publishes
    /// an empty `mounts` vec exactly as before.
    Residual,
}

/// GUI-facing projection of one held (Quarantine / Hybrid-quarantined) `vike_exec::ReconAlert`
/// (Task 17) — kind/detail/count only, NEVER the raw proposed `Event`s (those stay fold-thread
/// state until an operator confirms; see `Command::ConfirmRecon`). `id` is the key an operator
/// confirms by (`CoreThread::reconcile_reports` assigns it, monotonic within this process run —
/// NOT stable across a journal replay, see the `Command::ConfirmRecon` doc).
#[derive(Debug, Clone, PartialEq)]
pub struct ReconAlertView {
    pub id: u64,
    /// `format!("{:?}", DivergenceKind)` — a fieldless enum, so this is stable/comparable text.
    pub kind: String,
    pub detail: String,
    /// how many events are held pending confirm (never the events themselves).
    pub proposed_event_count: usize,
    /// **WHICH BOOK this divergence is about** — the raising pass's route key
    /// (`vike_exec::ReconcileReports::route_key`), or the venue itself for a venue with one
    /// account, which is every venue on a box with no `[accounts]` table.
    ///
    /// ⚠ It exists because this projection is the whole of what an operator SEES, and it carried
    /// kind/detail/count and nothing else. At fifty accounts of one exchange that is fifty rows
    /// reading `PositionDrift` with no way to tell which account each belongs to — and confirming
    /// one of them moves one specific book. The confirm still routes on the fold thread's own
    /// stored key; this is the label that lets somebody decide WHICH id to confirm.
    pub account: String,
}

/// Reconciliation state block (Task 17): every currently-held alert awaiting an operator
/// `Command::ConfirmRecon`, plus the wall-clock ms of the most recently completed reconcile pass
/// (0 before the first pass ever runs). Empty/zero by default — a core that never reconciles, or
/// whose policy is fully `Synthesize` (today's default), publishes this byte-identically empty.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ReconBlock {
    pub alerts: Vec<ReconAlertView>,
    pub last_pass_ts: i64,
}

/// Per-venue ledger block (cross-venue runtime): one per engine, primary included, so the
/// GUI renders every venue uniformly while the scalar fields keep mirroring the primary.
#[derive(Debug, Clone, PartialEq)]
pub struct VenueBlock {
    pub venue: String,
    /// **WHICH ACCOUNT of `venue`** — `None` for the unlabelled one, the convention
    /// `vike_model::account_keys::AccountLabel` states for every carrier of that type.
    ///
    /// [`CoreSnapshot::build`] derives it from the engine's own `route_key` through
    /// `vike_model::account_keys::label_of_route_key`, so on a block built there it cannot disagree
    /// with [`Self::route_key`] about which account this block is.
    ///
    /// ⚠ That is a property of the BUILDER, not of the type. A block the desktop's observe bridge
    /// rebuilds from the wire reads the label and the route key as two separate wire fields, so a
    /// skewed node could make them disagree; a reader that needs the ROUTING answer derives it from
    /// `route_key` through the same inverse and never trusts this field for it.
    ///
    /// `None` on every single-account process — which is every process with no `[accounts]` table —
    /// so a consumer that ignores it reads what it read before the field existed.
    pub account: Option<vike_model::account_keys::AccountLabel>,
    /// **This engine's own ROUTING key** — `route_key_of(venue, account)`, i.e. the bare venue id
    /// for the default account and `venue#LABEL` for a labelled one.
    ///
    /// ⚠ **This is the field a gate must compare against, never [`Self::venue`]**, and that is the
    /// whole reason it is published. A two-account node emits two blocks whose `venue` is the same
    /// string, so `venues[].venue` is evidence that cannot tell two books apart — the defect
    /// `docs/decisions/0041-an-unmountable-venue-is-refused-at-the-preview.md`'s *What would reopen
    /// this* named in advance ("A second account of one exchange is mounted"). `venue` keeps doing
    /// the CAPABILITY job it has always done (`vike_model::caps_for` and every table keyed on
    /// `vike_model::VENUES`); this does the routing one.
    ///
    /// Equal to `venue` on a single-account process, by construction rather than by care — see
    /// `vike_model::account_keys::AccountRef::route_key`.
    pub route_key: String,
    /// **The symbol this engine was mounted for.** `vike_exec::ExecutionEngine::symbol`, the first
    /// half of the set `accepts_symbol` answers for. EMPTY only on a block rebuilt from a node that
    /// did not publish it (the desktop's observe bridge reading an older node), which a reader
    /// treats as "not said", never as "trades nothing".
    pub symbol: String,
    /// **The other symbols it trades** — the engine's `extra_symbols` verbatim. Empty for an engine
    /// `vike_mount::make_engine` builds (it sets none), which is every venue mount; **not empty
    /// everywhere**: the paper multi-strategy builder (`vike_mount`'s
    /// `build_paper_multi_strategy_core_with`) fills it, and runtime combo admission
    /// (`CoreThread::lower_combo`) appends every new leg symbol while nothing prunes the list. It
    /// also persists through `vike_exec::EngineSnapshot`. So wherever a combo producer exists the
    /// list only grows: the per-publish clone costs an allocation per distinct leg symbol ever
    /// traded, and the node's wire `symbols` list carries them all in every frame. No production
    /// code constructs `OrderIntent::Combo` today, so that is latent. A `Vec` because cloning an
    /// empty one allocates nothing, and `CoreSnapshot::build` runs per EVENT.
    pub extra_symbols: Vec<String>,
    /// **What stands behind this engine's orders** — `vike_exec::ExecutionEngine::mode`.
    /// `CoreSnapshot::build` always says. `None` only on a block rebuilt from a node that did not
    /// publish it.
    ///
    /// ⚠ **`Some(Paper)` is what an engine says until something says otherwise, so it is not proof
    /// that no real money is behind it.** The snapshot an engine is restored from does not carry its
    /// mode: `vike_exec::ExecutionEngine::from_snapshot` rebuilds through `ExecutionEngine::new`, so
    /// a restored engine reports `Paper` until its caller sets `mode`. Every engine `vike_mount`
    /// builds around a venue client is set explicitly; a caller that restores one around a real
    /// client must set it too, or this field understates what an order sent to it will do.
    pub mode: Option<vike_exec::EngineMode>,
    pub balance: f64,
    pub realized_pnl: f64,
    pub fees_paid: f64,
    pub funding_paid: f64,
    pub balance_mode: BalanceMode,
    /// mode-aware equity at THIS venue's seed (resolver-priced — see `resolve_equity`)
    pub equity: f64,
    /// resolver-priced unrealized total for this venue (`ResolvedEquity::unrealized_total`)
    pub unrealized: f64,
    /// open positions on this venue with no priceable source (`ResolvedEquity::missing`)
    pub missing_prices: u32,
    /// margin currently locked by open positions on this venue (0.0 when the gate is off)
    pub margin_used: f64,
    /// free buying power (`vike_model::free_buying_power`); equals `equity` when the gate is off
    pub free_bp: f64,
    /// `margin_used / equity` (0.0 when the gate is off or equity ≤ 0)
    pub margin_ratio: f64,
    /// the venue's effective per-order fee schedule (fee model follow-up 1) — the LIVE
    /// account-actual maker/taker the mount fetched when available, else the static published
    /// default; `None` for an engine the mount never tagged (GUI-only / test). Surfaced so the GUI
    /// can display real transaction costs; read-only, never drives the fold.
    pub fee_schedule: Option<vike_model::FeeSchedule>,
    pub trading_state: TradingState,
    /// This venue's per-symbol contract-multiplier grid, shared with the engine's `Account` by
    /// pointer-copy (`Arc::clone` — no allocation, no deep clone, on a publish path that is
    /// already coalesced). Read through [`VenueBlock::multiplier_of`], never directly: a symbol
    /// ABSENT from this map resolves to `multiplier_default`, not to nothing.
    ///
    /// Why the whole grid and not a field on `PositionView`: the GUI needs the multiplier for
    /// symbols it holds NO position in — the deribit options confirm-ticket prices a contract the
    /// account has never traded — so hanging it off a position would leave exactly the motivating
    /// case unanswerable.
    pub multipliers: std::sync::Arc<indexmap::IndexMap<String, f64>>,
    /// the fallback multiplier for any symbol absent from `multipliers` (`Account`'s legacy
    /// scalar; 1.0 for every engine `vike-mount` builds today).
    pub multiplier_default: f64,
    pub positions: Vec<PositionView>,
}

/// A FLAT, unnamed block: every scalar zero, no positions, and the two enums set to the same
/// values [`CoreSnapshot::empty`] publishes before the first real build (`BalanceMode::Delta`,
/// `TradingState::Active`). Exists so a consumer TEST can name only the two or three fields it is
/// about — `VenueBlock { venue: "bybit".into(), realized_pnl: -450.0, ..Default::default() }` —
/// instead of restating eighteen; `vike-alerting` in particular cannot spell `BalanceMode` at all
/// (it has no `vike-exec` edge), so without this it could not build one.
///
/// ⚠ NOT a valid published block: `venue` is EMPTY, which no real engine ever is. Never construct
/// a snapshot for a consumer to render from this — [`CoreSnapshot::build`] is the only producer.
///
/// ⚠ [`VenueBlock::route_key`] is therefore EMPTY here too, and that is the consistent answer
/// rather than a gap: the spec's *"a `route_key` equal to its venue"* is satisfied only vacuously
/// on a block whose venue is itself empty, and spelling anything else would make the default block
/// claim a routing identity no engine has. [`VenueBlock::account`] is `None`, the absent/default
/// shape a single-account process publishes.
///
/// ⚠ [`VenueBlock::symbol`] is EMPTY and [`VenueBlock::mode`] is `None` for the same reason: a block
/// that names no engine names nothing that engine trades and nothing that stands behind it. A reader
/// treats both as "not said" — [`VenueBlock::trades`] answers `false` for every symbol.
impl Default for VenueBlock {
    fn default() -> Self {
        VenueBlock {
            venue: String::new(),
            account: None,
            route_key: String::new(),
            symbol: String::new(),
            extra_symbols: Vec::new(),
            mode: None,
            balance: 0.0,
            realized_pnl: 0.0,
            fees_paid: 0.0,
            funding_paid: 0.0,
            balance_mode: BalanceMode::Delta,
            equity: 0.0,
            unrealized: 0.0,
            missing_prices: 0,
            margin_used: 0.0,
            free_bp: 0.0,
            margin_ratio: 0.0,
            fee_schedule: None,
            trading_state: TradingState::Active,
            multipliers: std::sync::Arc::new(indexmap::IndexMap::new()),
            multiplier_default: 1.0,
            positions: Vec::new(),
        }
    }
}

impl VenueBlock {
    /// This venue's contract multiplier for `symbol` — the exact `Account::multiplier_of`
    /// authority, resolvable for ANY symbol (position or not), falling back to
    /// `multiplier_default` for one absent from the grid.
    pub fn multiplier_of(&self, symbol: &str) -> f64 {
        self.multipliers.get(symbol).copied().unwrap_or(self.multiplier_default)
    }

    /// Whether this block's engine trades `symbol`: the engine's own
    /// `vike_exec::ExecutionEngine::accepts_symbol`, answered from what the block carries.
    ///
    /// ⚠ **`false` does NOT always mean "this account does not trade it".** A block that names no
    /// symbol ([`VenueBlock::symbol`] empty: a node that did not say, or a default block) answers
    /// `false` for EVERY `symbol`, while an empty `VenueBlock::symbol` means "not said" and never
    /// "trades nothing" — this method cannot tell the two apart. So every reader checks
    /// `block.symbol.is_empty()` FIRST and decides what "not said" allows; only a block that did say
    /// a symbol gives `false` its literal meaning. An empty `symbol` argument is `false` too.
    pub fn trades(&self, symbol: &str) -> bool {
        !symbol.is_empty()
            && (self.symbol == symbol || self.extra_symbols.iter().any(|s| s == symbol))
    }
}

/// One immutable core-state view. `seq` increments per publish (GUI change detection).
#[derive(Debug, Clone)]
pub struct CoreSnapshot {
    pub seq: u64,
    pub venue: String,
    pub symbol: String,
    pub trading_state: TradingState,
    pub balance: f64,
    pub balance_mode: BalanceMode,
    /// The named live cross-venue aggregator (F24 / portfolio read-model spec): the equity totals
    /// and per-venue `venues` rows that used to sit loose here, now grouped under one type. Purely
    /// a naming reshape — every value is computed exactly as before. GUI reads `snap.portfolio.*`.
    pub portfolio: Portfolio,
    pub positions: Vec<PositionView>,
    pub marks: Vec<(String, String, f64)>,
    /// registry in insertion order
    pub orders: Vec<OrderView>,
    /// pending bracket exits held off the venue until their OTO parent fills (live-runtime OCO/OTO);
    /// empty on every non-bracket run. NOT part of `orders` (they have no registered/venue order).
    pub held_exits: Vec<HeldOrderView>,
    /// the core-owned bar cache: (venue, symbol, interval) -> closed bars (Arc-shared,
    /// pointer-copy per snapshot) + the forming bar. The GUI converts to its render
    /// model at this boundary (R5c).
    pub bars: indexmap::IndexMap<vike_exec::SeriesKey, vike_exec::BarSeries>,
    /// one [`MountView`] per live strategy mount (portfolio-observer PR-4 T5), primary mount
    /// first then `extra_mounts` in registration order — empty only before the first build or
    /// when nothing is mounted. `ready: true` for every mount when
    /// `CoreConfig::readiness_gate` is off (the default).
    ///
    /// The LAST row is the [`MountRowKind::Residual`] one (gap E) whenever any mount exists, so
    /// `Σ realized_pnl` over this whole vec equals the account's realized PnL net of fees. Filter on
    /// `kind` before treating rows as mounts.
    pub mounts: Vec<MountView>,
    /// most recent delivered exec events (bounded journal feed for the GUI)
    pub recent_events: Vec<std::sync::Arc<str>>,
    /// set once a handler panicked — the core is HALTED in safe-state (see runtime.rs)
    pub fault: Option<String>,
    /// market-data conflation drops since start (latest-wins is WORKING, not a bug)
    pub conflated_market_drops: u64,
    /// GUI/command messages rejected because the ingest queue was full (never silent)
    pub rejected_commands: u64,
    /// Task 17: held Quarantine/Hybrid-quarantined reconcile alerts awaiting operator confirm,
    /// plus the last reconcile-pass timestamp. Empty/zero when nothing has ever been quarantined.
    pub recon: ReconBlock,
    /// Latest venue-REPORTED per-position coin delta from the reconcile path, keyed `(venue,
    /// symbol)` (Wave 5d). Populated ONLY for a venue whose `PositionStatusReport` carries a
    /// `delta` — Deribit today (`get_positions.delta`, correct for inverse contracts); every other
    /// venue leaves this empty and byte-identical. The greeks tool reads it (via
    /// [`Self::coin_delta`]) to fold a Deribit perp/future hedge leg into net portfolio greeks
    /// (`coin_delta × spot`) — `PositionView` is FILLS-derived and carries no venue delta, so the
    /// coin delta reaches the GUI through this side map instead. Refreshed each reconcile pass;
    /// cloned at the coalesced publish, never on the per-message fold.
    ///
    /// ⚠ **The key is per VENUE, not per ACCOUNT, and that is a declared residual rather than an
    /// oversight** — two accounts of one exchange on one instrument overwrite each other here.
    /// The argument for leaving it (the consumer has no account handle to look one up with, so a
    /// per-account key would un-price every leg instead of pricing it wrongly) and what would
    /// close it are at the write site, `CoreThread::reconcile_reports`.
    pub recon_coin_deltas: indexmap::IndexMap<(String, String), f64>,
    /// **WHICH SET OF ACCOUNTS this node is mounting** — a value that CHANGES whenever the mounted
    /// account set does, so a client holding an older view of this node can tell that it is stale
    /// (the account-routing spec's §6.3; the property kept from the opaque-handle candidate it
    /// refused, bought for one integer).
    ///
    /// `0` means *no fold has happened yet* — [`CoreSnapshot::empty`], the placeholder published
    /// before the first real build, where `venues` is empty and there is no account set to describe.
    /// A real build never publishes `0`.
    ///
    /// # ⚠ It is a DIGEST of the account set, not a counter, and the spec says "monotonically increasing"
    ///
    /// A declared deviation, with the argument, because the word matters at the other end. The
    /// account set of a `CoreThread` is FIXED at `assemble_core` and nothing pushes to
    /// `extra_engines` afterwards, so an in-process counter would never advance — its only real job
    /// is catching a RESTART that came back with a different set, and a counter starting at `0` on
    /// every boot publishes `0` for two different account sets, which is precisely the staleness the
    /// field exists to detect. A digest over the sorted route keys answers that question and a
    /// counter cannot.
    ///
    /// What it costs: a consumer must compare for INEQUALITY, never for ordering — "this is newer
    /// than that" is not a question this value can answer. §6.3's own use ("a confirm carrying a
    /// stale epoch is REFUSED") is an equality check, so nothing it was specified for is lost. The
    /// alternative — a counter plus a separate set digest — is two fields where one answers, and is
    /// the swap to make if an ordering consumer ever appears.
    ///
    /// The digest is FNV-1a, spelled out in [`CoreSnapshot::accounts_epoch_of`] rather than taken
    /// from `std::hash`: `DefaultHasher`'s algorithm is explicitly not guaranteed stable across
    /// Rust releases, and a value compared across a node upgrade must not change because the
    /// toolchain did.
    pub accounts_epoch: u64,
}

impl CoreSnapshot {
    /// Placeholder published before the first real build (GUI renders "connecting").
    pub fn empty(venue: &str, symbol: &str) -> Self {
        CoreSnapshot {
            seq: 0,
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            trading_state: TradingState::Active,
            balance: 0.0,
            balance_mode: BalanceMode::Delta,
            portfolio: Portfolio::default(),
            positions: Vec::new(),
            marks: Vec::new(),
            orders: Vec::new(),
            held_exits: Vec::new(),
            bars: indexmap::IndexMap::new(),
            mounts: Vec::new(),
            recent_events: Vec::new(),
            fault: None,
            conflated_market_drops: 0,
            rejected_commands: 0,
            recon: ReconBlock::default(),
            recon_coin_deltas: indexmap::IndexMap::new(),
            // ⚠ `0` = NO FOLD YET, and it must stay distinguishable from every real answer:
            // `CoreSnapshot::empty` is the window before a node's first fold, which
            // `docs/decisions/0041-an-unmountable-venue-is-refused-at-the-preview.md` measured as
            // a hole in its own gate ("the mounted set is empty, so the check answers 'no
            // evidence'"). A consumer must read this as "the node has not said yet", never as an
            // account set that happens to hash to zero — see `accounts_epoch_of`, which never
            // returns it.
            accounts_epoch: 0,
        }
    }

    /// [`CoreSnapshot::accounts_epoch`]'s derivation: an FNV-1a digest over this process's route
    /// keys, SORTED so the same set of accounts answers the same value whatever order the mount
    /// fan-out registered them in.
    ///
    /// Never `0` for a non-empty set — a digest that landed there is mapped to `1`, so `0` keeps
    /// meaning exactly "no fold yet" ([`CoreSnapshot::empty`]).
    #[must_use]
    pub fn accounts_epoch_of<'a>(route_keys: impl IntoIterator<Item = &'a str>) -> u64 {
        let mut keys: Vec<&str> = route_keys.into_iter().collect();
        if keys.is_empty() {
            return 0;
        }
        keys.sort_unstable();
        // FNV-1a, 64-bit, spelled out: `std::hash::DefaultHasher` is documented as not stable
        // across Rust releases, and this value is compared across a node RESTART — which is also a
        // node UPGRADE. A toolchain bump must not read as an account-set change.
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for key in keys {
            for b in key.as_bytes().iter().copied().chain(std::iter::once(0u8)) {
                h ^= u64::from(b);
                h = h.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        if h == 0 { 1 } else { h }
    }

    /// Build from the engine (called coalesced by the core loop, never per-event).
    #[allow(clippy::too_many_arguments)] // runtime-internal ctor: counters come from the loop
    pub fn build<C: ExecutionClient>(
        seq: u64,
        engine: &ExecutionEngine<C>,
        extra_engines: &[(f64, ExecutionEngine<C>)],
        seed_cash: f64,
        price_cfg: PriceCfg,
        // The ONE maintenance rate (the scope-parameterized liquidation law's rate source):
        // the operator's `MarginCallConfig::mm_requirement` when the watchdog is configured,
        // else that config's default — resolved by the caller (see `publish`). Feeds the
        // per-position liq-price badge; the retired `im * 0.5` hardcode is dead.
        mm_rate: f64,
        // Perf audit finding #2: the ring arrives UN-RENDERED. This coalesced publish (>=16ms)
        // is where each note becomes its line - the fold thread no longer `format!`s per event.
        recent_events: &std::collections::VecDeque<std::sync::Arc<str>>,
        bars: &indexmap::IndexMap<vike_exec::SeriesKey, vike_exec::BarSeries>,
        mounts: &[MountView],
        fault: &Option<String>,
        conflated_market_drops: u64,
        rejected_commands: u64,
        // Task 17: the GUI-facing reconcile block (held alerts + last pass ts), built by the
        // caller from its held-alert store (`CoreThread::recon_block`) — an owned value (not a
        // `&`) since the caller already builds a fresh one per publish.
        recon: ReconBlock,
        // Live-runtime OTO/OCO: pending bracket exits held off the venue, built by the caller from
        // `CoreThread::held_orders` (a `&[]` — empty on every non-bracket run).
        held_exits: &[HeldOrderView],
    ) -> Self {
        let acc = &engine.account;
        // Cold publish path only (this fn is coalesced, ≥16ms — never the per-message fold).
        // `price_cfg` is caller-tunable (`CoreConfig::price_cfg`, threaded through from PR-3's
        // equity sampler, which resolves prices through the SAME cfg); permissive default
        // (`PriceCfg::default()`: no freshness windows, mark enabled) unless the caller opts in.
        let cfg = price_cfg;
        // One resolve per engine, parallel to `account.positions` insertion order. Builds a
        // whole VenueBlock from a `ResolvedEquity` so `equity`/`unrealized`/`missing_prices`
        // and each PositionView's `unrealized`/`mark_source` all come from the SAME resolve
        // call — never a second, possibly-stale board read.
        let build_block = |venue: &str, e: &ExecutionEngine<C>, seed: f64| -> VenueBlock {
            let re: ResolvedEquity = e.resolve_equity(seed, &cfg);
            // Margin fields are computed ONLY when the gate is armed — this keeps the snapshot
            // publish (which is on the measured core-hop) zero-cost on the default/off path (the
            // latency gate runs with the gate off), and byte-identical to the pre-margin builder.
            let margin_on =
                e.gate.limits.im_requirement.is_some() || !e.gate.limits.im_by_symbol.is_empty();
            let mut margin_used = 0.0;
            if margin_on {
                // THE shared margin-in-use fold (`Account::margin_in_use`) — the SAME authority
                // the pre-trade gate uses, so the published number counts every position the gate
                // counts. DIVERGENCE FIX: the old snapshot fold used `im_for(s)` with NO fallback,
                // so a position whose symbol had no per-symbol override (`Command::SetMargin` writes
                // ONLY `im_by_symbol`, leaving the global `im_requirement` unset) was SKIPPED here
                // while the gate still counted it at the order symbol's rate — the published
                // margin_used UNDERSTATED what the gate enforced. (These snapshot fields are
                // GUI-only — the auto-liquidation watchdog computes its own margin from
                // `mm_requirement` via `check_margin_call` and never reads this number — so the
                // bug was a rosy GUI display, not a liquidation-safety issue.) RATE POLICY now
                // mirrors the gate: each position uses its own IM, falling back — for a
                // no-override position — to the account's MOST CONSERVATIVE armed initial-margin
                // rate (the max of `im_by_symbol` and any global `im_requirement`). That guarantees
                // such a position is VISIBLE and priced no lower than any single gate check would
                // price it, so the published free_bp is never rosier than the gate's admission
                // basis. When `im_requirement` IS set (global default), `im_for` never returns None
                // and this fallback is never consulted → byte-identical to the pre-fix number.
                //
                // KNOWN GUI ARTIFACT (cosmetic, deliberate): because the fallback is the max over
                // ALL armed rates, a no-override position's displayed margin shifts when an
                // UNRELATED symbol's rate is armed/disarmed via `Command::SetMargin`. It settles
                // once every held symbol has its own override or a global `im_requirement` is set.
                // Never dangerous (overstates, never understates, GUI-only).
                let fallback = e
                    .gate
                    .limits
                    .im_by_symbol
                    .values()
                    .copied()
                    .chain(e.gate.limits.im_requirement)
                    .fold(0.0_f64, f64::max);
                // POOL POLICY (the liquidation law's partition, mirroring the gate and
                // `check_margin_call`): only CROSS positions price into this shared number.
                // An Isolated position is backed by its own walled-off wallet (surfaced
                // per-position via `PositionView::isolated_margin`) and a Cash position is
                // fully funded — folding either into `margin_used` would double-charge a
                // mixed account's published free_bp against equity that never backs them.
                // All-cross accounts (every position today): the filter is a no-op →
                // byte-identical to the pre-filter number (the dedup-A1 pins still hold).
                // PRICE BASIS (risk-lane completion): the fold is resolver-priced
                // (`resolved_margin_in_use_by` under the SAME `cfg` as `resolve_equity` above),
                // so the published `margin_ratio`'s numerator and denominator — and the free_bp
                // crossing — share one price basis; a stale `Account.marks` scalar can no longer
                // overstate margin against a fresh-quote equity. Rate/pool policy unchanged.
                margin_used = e.resolved_margin_in_use_by(&cfg, |(_v, s, _side), p| {
                    p.margin_mode.is_cross().then(|| e.gate.limits.im_for(s).unwrap_or(fallback))
                });
            }
            // gate off ⇒ free_bp == equity (nothing locked), ratio 0.
            let free_bp = if margin_on {
                vike_model::free_buying_power(
                    re.equity,
                    margin_used,
                    0.0,
                    e.gate.limits.required_free_bp_pct,
                )
            } else {
                re.equity.max(0.0)
            };
            let margin_ratio =
                if margin_on && re.equity > 0.0 { margin_used / re.equity } else { 0.0 };
            // Liq-badge pool inputs (GUI-only, cold publish path) — the scope-parameterized
            // law's cross pool over THIS venue's account: total marked CROSS notional, and the
            // pool equity = resolved equity minus every isolated position's walled-off pool
            // (wallet + its own uPnL). With no isolated positions (today's default) the pool
            // equity IS `re.equity`.
            let mut cross_notional_total = 0.0;
            let mut pool_equity = re.equity;
            if margin_on {
                for (((v, s, _ps), p), rp) in e.account.positions.iter().zip(re.per_position.iter())
                {
                    if p.size == 0.0 {
                        continue;
                    }
                    if p.margin_mode.is_isolated() {
                        pool_equity -= p.isolated_margin.unwrap_or(0.0) + rp.unrealized;
                    } else if p.margin_mode.is_cross()
                        && let Some(mark) = e.account.mark_of(v, s)
                    {
                        cross_notional_total +=
                            vike_model::gross_notional(p.size, mark, e.account.multiplier_of(s));
                    }
                }
            }
            let positions = e
                .account
                .positions
                .iter()
                .zip(re.per_position.iter())
                .map(|(((v, s, ps), p), rp)| {
                    // Per-symbol leverage + the liq-price badge, from the ONE liquidation law
                    // (`vike_model::money::liquidation`) at the ONE maintenance rate (`mm_rate` — the
                    // `im * 0.5` hardcode is dead). 0.0 when the gate is off or the leg is flat;
                    // skips the per-position `im_for` lookup entirely on the off path. By mode:
                    // - Isolated → the closed-form `liquidation_price` at the REAL maint rate
                    //   (its own wallet is its pool, so a per-position price is exact in shape);
                    // - Cross → `cross_liquidation_price_est`: the mark at which the SHARED
                    //   pool first hits the law's line, others frozen (a per-position cross liq
                    //   price is inherently an estimate — the venue-UI shape, advisory only;
                    //   the watchdog acts on the account-level law, never on this number);
                    // - Cash → no badge (structurally cannot breach).
                    let (leverage, liq_price) = match margin_on.then(|| e.gate.limits.im_for(s)) {
                        Some(Some(im)) if im > 0.0 && p.size != 0.0 => {
                            let liq = if p.margin_mode.is_isolated() {
                                vike_model::liquidation_price(
                                    p.avg_px,
                                    p.size.signum() as i32,
                                    im,
                                    mm_rate,
                                )
                            } else if p.margin_mode.is_cross() {
                                match e.account.mark_of(v, s).as_ref() {
                                    Some(mark) => {
                                        let mult = e.account.multiplier_of(s);
                                        let own = vike_model::gross_notional(p.size, *mark, mult);
                                        vike_model::cross_liquidation_price_est(
                                            pool_equity,
                                            p.size,
                                            *mark,
                                            mult,
                                            cross_notional_total - own,
                                            mm_rate,
                                        )
                                    }
                                    None => 0.0, // unmarked → the pool can't be judged here
                                }
                            } else {
                                0.0 // Cash: never liquidates
                            };
                            (1.0 / im, liq)
                        }
                        _ => (0.0, 0.0),
                    };
                    PositionView {
                        venue: v.to_string(),
                        symbol: s.to_string(),
                        position_side: ps.to_string(),
                        size: p.size,
                        avg_px: p.avg_px,
                        unrealized: rp.unrealized,
                        mark_source: rp.mark_source,
                        leverage,
                        liq_price,
                        // Carried straight from the account's PositionEntry (Cross/None by
                        // default → byte-identical to the pre-field view).
                        margin_mode: p.margin_mode,
                        isolated_margin: p.isolated_margin,
                    }
                })
                .collect();
            VenueBlock {
                venue: venue.to_string(),
                // The ROUTING half of the canonical/routing split, published so a consumer can tell
                // two accounts of one exchange apart — `venue` above cannot, and on a two-account
                // node it is the same string in both blocks. The label is the INVERSE of the same
                // renderer the mount stamped the engine with, so the two cannot drift.
                // ⚠ `AccountLabel::Default` renders as an ABSENT field, never as `Some(Default)`:
                // that is the convention `AccountLabel`'s own doc states for every carrier of the
                // type (*"an account-less mount emits no key and a pre-change file still parses"*),
                // and it is what keeps a single-account node's published shape indistinguishable
                // from the one it published before this field existed.
                account: vike_model::account_keys::label_of_route_key(&e.venue, &e.route_key)
                    .filter(|l| !l.is_default()),
                route_key: e.route_key.clone(),
                // What the engine trades and what stands behind it (the Trade window design, §4.3).
                // One small `String` clone, the same kind of work as `route_key` above; the `Vec`
                // clone allocates nothing while it is empty, which is every venue mount.
                symbol: e.symbol.clone(),
                extra_symbols: e.extra_symbols.clone(),
                mode: Some(e.mode),
                balance: e.account.balance,
                realized_pnl: e.account.realized_pnl,
                fees_paid: e.account.fees_paid,
                funding_paid: e.account.funding_paid,
                balance_mode: e.account.balance_mode,
                equity: re.equity,
                unrealized: re.unrealized_total,
                missing_prices: re.missing,
                margin_used,
                free_bp,
                margin_ratio,
                fee_schedule: e.fee_schedule,
                trading_state: e.trading_state,
                // Pointer-copy of the engine's immutable multiplier grid — one `Arc` refcount
                // bump per venue per publish, no allocation (same cost profile as `bars`).
                multipliers: e.account.multiplier_grid(),
                multiplier_default: e.account.multiplier_default(),
                positions,
            }
        };
        // Compute the primary block first so the top-level scalar `equity` and `positions`
        // can be reused from it verbatim (they are documented mirrors of the primary venue —
        // see the struct docs above `equity`/`positions`).
        let primary = build_block(&engine.venue, engine, seed_cash);
        let top_equity = primary.equity;
        let top_positions = primary.positions.clone();
        let mut venues = Vec::with_capacity(1 + extra_engines.len());
        venues.push(primary);
        for (seed, e) in extra_engines {
            venues.push(build_block(&e.venue, e, *seed));
        }
        // CrossVenueDriver::aggregate_equity law: py_sum in venue registration order
        let equity_total = vike_model::py_sum(venues.iter().map(|v| v.equity));
        // The drawdown latch's frozen capital base — SAME fold law, SAME order (primary first,
        // then extras in registration order) as `CoreThread::sweep_drawdown_latch` computes it
        // from `seed_of`, so the published `Portfolio::drawdown_curve` is bit-identical to the
        // number the latch acted on. See `Portfolio::capital_base`.
        let capital_base = vike_model::py_sum(
            std::iter::once(seed_cash).chain(extra_engines.iter().map(|(s, _)| *s)),
        );
        let missing_prices_total: u32 = venues.iter().map(|v| v.missing_prices).sum();
        let margin_used_total: f64 = venues.iter().map(|v| v.margin_used).sum();
        let mut orders: Vec<OrderView> = Vec::new();
        // One (engine, block) pair per engine: `venues` was filled above from exactly these engines
        // in exactly this order, primary first. The block already carries the label
        // `label_of_route_key` derived from the engine's route key, so an order reads THAT instead of
        // parsing the key a second time per engine per publish — and `OrderView::account` cannot
        // disagree with `VenueBlock::account` about which account it is.
        //
        // The zip is only right while `venues` holds exactly one block per engine, so the invariant
        // is checked here. `debug_assert_eq!` is compiled out of release builds: nothing is logged
        // and nothing allocates on this per-event path (the formatting runs only on a failure).
        debug_assert_eq!(
            venues.len(),
            1 + extra_engines.len(),
            "one venue block per engine, primary first: the order loop below zips them"
        );
        for (e, block) in
            std::iter::once(engine).chain(extra_engines.iter().map(|(_, e)| e)).zip(&venues)
        {
            for (coid, mo) in e.registry.iter() {
                orders.push(OrderView {
                    client_order_id: coid.clone(),
                    venue: e.venue.clone(),
                    // `None` for the default account (every node today), which clones to nothing; a
                    // labelled account pays one small `String` per order.
                    account: block.account.clone(),
                    symbol: mo.request.symbol.clone(),
                    side: mo.request.side,
                    qty: mo.request.qty,
                    order_type: mo.request.order_type.clone(),
                    price: mo.request.price,
                    trigger_price: mo.request.trigger_price,
                    status: mo.status,
                    venue_order_id: mo.venue_order_id.clone(),
                    filled_qty: mo.filled_qty,
                    avg_fill_px: mo.avg_fill_px,
                });
            }
        }
        CoreSnapshot {
            seq,
            venue: engine.venue.clone(),
            symbol: engine.symbol.clone(),
            trading_state: engine.trading_state,
            balance: acc.balance,
            balance_mode: acc.balance_mode,
            portfolio: Portfolio {
                equity: top_equity,
                equity_total,
                realized_pnl: acc.realized_pnl,
                fees_paid: acc.fees_paid,
                funding_paid: acc.funding_paid,
                margin_used_total,
                capital_base,
                missing_prices_total,
                balances_by_asset: acc
                    .balances_by_asset
                    .iter()
                    .map(|(a, q)| (a.clone(), *q))
                    .collect(),
                venues,
            },
            positions: top_positions,
            marks: acc
                .marks_iter()
                .map(|((v, s), px)| (v.to_string(), s.to_string(), *px))
                .collect(),
            orders,
            held_exits: held_exits.to_vec(),
            bars: bars.clone(), // Arc clones for the closed series — cheap by design
            mounts: mounts.to_vec(),
            recent_events: recent_events.iter().cloned().collect(),
            fault: fault.clone(),
            conflated_market_drops,
            rejected_commands,
            recon,
            // The reconcile-path coin deltas are a side map the caller overwrites at the publish
            // site (`runtime::publish`) from `CoreThread::recon_coin_deltas` — kept out of this
            // ctor's arg list (already `too_many_arguments`) since it is retained fold-thread state,
            // not derived from the engine here. Empty on the test/direct-build path.
            recon_coin_deltas: indexmap::IndexMap::new(),
            // The account-set digest (§6.3), over the SAME engines whose blocks are published
            // above. Recomputed each build rather than cached because the set cannot change within
            // a process — so the value is constant per run and the cost is a few dozen bytes of
            // FNV on a coalesced (≥16ms) publish, never the per-message fold.
            accounts_epoch: Self::accounts_epoch_of(
                std::iter::once(engine.route_key.as_str())
                    .chain(extra_engines.iter().map(|(_, e)| e.route_key.as_str())),
            ),
        }
    }
}

/// Query surface — pure O(n) accessors over the published `Vec`s (no indexed Cache). This is the
/// READ half of the control boundary (`crate::control`); a CLI/MCP translates into these.
impl CoreSnapshot {
    /// The contract multiplier in force for `(venue, symbol)` — the GUI-side twin of
    /// `vike_exec::Account::multiplier_of`, which lives inside the core's per-venue engines and
    /// is otherwise unreachable from a lossy snapshot reader.
    ///
    /// Resolves for a symbol the account holds NO position in (that is the point — the deribit
    /// options confirm-ticket must price a contract it has never traded), because the whole
    /// per-venue grid is published, not just the held rows.
    ///
    /// **An UNKNOWN venue reads 1.0**, matching the multiplier-free arithmetic every caller uses
    /// today. That is the permissive answer, so a caller gating on notional (e.g. the order-entry
    /// `max_notional_per_order` policy cap) must not treat 1.0 as proof the instrument is unlevered —
    /// it means "this snapshot knows of no multiplier for that venue".
    pub fn multiplier_of(&self, venue: &str, symbol: &str) -> f64 {
        self.portfolio.venue(venue).map_or(1.0, |v| v.multiplier_of(symbol))
    }

    /// All orders for a symbol, registry insertion order (any status).
    pub fn orders_for<'a>(&'a self, symbol: &'a str) -> impl Iterator<Item = &'a OrderView> + 'a {
        self.orders.iter().filter(move |o| o.symbol == symbol)
    }

    /// One order by client-order-id (any status).
    pub fn order(&self, client_order_id: &str) -> Option<&OrderView> {
        self.orders.iter().find(|o| o.client_order_id == client_order_id)
    }

    /// One order by client-order-id, only while NON-TERMINAL.
    pub fn open_order(&self, client_order_id: &str) -> Option<&OrderView> {
        self.orders.iter().find(|o| o.client_order_id == client_order_id && !o.status.is_terminal())
    }

    /// Net position for (venue, symbol) — the `BOTH` leg (hedge-mode legs via `positions`).
    pub fn position(&self, venue: &str, symbol: &str) -> Option<&PositionView> {
        self.positions
            .iter()
            .find(|p| p.venue == venue && p.symbol == symbol && p.position_side == "BOTH")
    }

    /// Latest mark for (venue, symbol).
    pub fn last_mark(&self, venue: &str, symbol: &str) -> Option<f64> {
        self.marks.iter().find(|(v, s, _)| v == venue && s == symbol).map(|(_, _, px)| *px)
    }

    /// The latest venue-REPORTED coin delta for `(venue, symbol)` from the reconcile path (Wave
    /// 5d) — `Some` only for a venue that surfaces one (Deribit `get_positions.delta`), `None`
    /// otherwise. The greeks tool passes this straight into `PositionViewLite::coin_delta` so a
    /// Deribit perp/future hedge leg folds into net portfolio greeks; see `recon_coin_deltas`.
    pub fn coin_delta(&self, venue: &str, symbol: &str) -> Option<f64> {
        self.recon_coin_deltas.get(&(venue.to_string(), symbol.to_string())).copied()
    }

    /// Mode-aware total account equity — the cross-venue `equity_total` (py_sum law). FOOTGUN:
    /// this is the cross-venue TOTAL, so in multi-venue mode it can differ from the raw `equity`
    /// field (primary venue only); equal single-venue. Adapters use this accessor.
    #[allow(clippy::misnamed_getters)] // deliberately the cross-venue TOTAL, not the `equity` field
    pub fn equity(&self) -> f64 {
        self.portfolio.equity_total
    }

    /// Which OPEN positions have NO priceable mark (core-ergonomics) — `(venue, symbol)` pairs,
    /// delegating to [`Portfolio::missing_price_instruments`] (see it for the exact definition).
    /// The named counterpart to `portfolio.missing_prices_total`: the count says how many, this
    /// says which. Read-only, off the fold path, like every accessor here.
    pub fn missing_price_instruments(&self) -> Vec<(String, String)> {
        self.portfolio.missing_price_instruments()
    }

    /// Signed NET notional exposure for `(venue, symbol)`: Σ `size · mark · multiplier` over that
    /// venue's legs of the symbol (long +, short −), priced at the PUBLISHED account mark — the
    /// same `last_mark`/`marks` basis the GUI already shows per symbol. A leg with no published
    /// mark contributes 0. 0.0 for an unknown venue/symbol. Read-only, off the fold path.
    ///
    /// PRICE BASIS: this reads the account mark slot (`marks`), NOT the full `PriceBoard` resolver
    /// chain — a display readout consistent with the marks the snapshot already carries. The
    /// resolver-priced (one-price-law) exposure lives on `vike_exec::ExecutionEngine`
    /// (`net_exposure`/`gross_exposure`) inside the core, per venue.
    pub fn net_exposure(&self, venue: &str, symbol: &str) -> f64 {
        let Some(mark) = self.last_mark(venue, symbol) else { return 0.0 };
        let mult = self.multiplier_of(venue, symbol);
        self.portfolio.venue(venue).map_or(0.0, |v| {
            v.positions
                .iter()
                .filter(|p| p.symbol == symbol)
                .map(|p| vike_model::signed_notional(p.size, mark, mult))
                .sum()
        })
    }

    /// Cross-venue signed net notional for `symbol` — [`Self::net_exposure`] summed over every
    /// venue block (each venue priced at its own published mark and multiplier).
    pub fn net_exposure_total(&self, symbol: &str) -> f64 {
        self.portfolio.venues.iter().map(|v| self.net_exposure(&v.venue, symbol)).sum()
    }

    /// Total GROSS notional across every venue/position — Σ `|size| · mark · multiplier`, never
    /// netting a long against a short. The account-wide gross exposure a risk strip shows; an
    /// unmarked position contributes 0. Marks-basis, like [`Self::net_exposure`]. Read-only.
    pub fn gross_exposure(&self) -> f64 {
        self.portfolio
            .venues
            .iter()
            .flat_map(|v| v.positions.iter().map(move |p| (v, p)))
            .map(|(v, p)| match self.last_mark(&v.venue, &p.symbol) {
                Some(mark) => vike_model::gross_notional(p.size, mark, v.multiplier_of(&p.symbol)),
                None => 0.0,
            })
            .sum()
    }

    /// Primary-engine trading state (per-venue states via `venues`).
    pub fn trading_state(&self) -> vike_exec::TradingState {
        self.trading_state
    }
}

#[path = "query_tests.rs"]
#[cfg(test)]
mod query_tests;

#[path = "snapshot_tests.rs"]
#[cfg(test)]
mod snapshot_tests;

/// **STAGE 0 of the account-routing seam** — the additive `VenueBlock::{account, route_key}` and
/// `CoreSnapshot::accounts_epoch`
/// (`docs/superpowers/specs/2026-09-13-the-wire-names-an-account-from-the-misroute.md` §7.1, §6.3).
///
/// The claim these tests exist for is **"a single-account node is byte-identical"**, which is the
/// whole of what makes Stage 0 safe to stop at. It is asserted as a FROZEN BASELINE rather than as
/// a behavioural property, for the reason `runtime::mount_account_tests`'
/// `an_account_less_mount_is_the_single_account_core_unchanged` states: *"unchanged" is the claim,
/// and a behavioural test can only ever check the properties somebody thought to name.*
#[path = "account_fields_tests.rs"]
#[cfg(test)]
mod account_fields_tests;
