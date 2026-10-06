//! Every type a snapshot publishes (rows, venue blocks, `CoreSnapshot`) and the reads they answer.

use vike_exec::{BalanceMode, OrderStatus, PriceSource, TradingState};

use crate::portfolio::Portfolio;
use crate::runtime::MountBudget;

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
    /// effective leverage for this position's symbol (`1/im`). 0.0 means UNKNOWN, not "none": the
    /// margin gate is off, OR the symbol has no explicit rate under per-symbol margin. Such a leg is
    /// still priced into the venue's `margin_used` and the cross pool at the fallback rate, a
    /// conservative stand-in for risk accounting rather than a rate the venue declared, so no leverage
    /// or liquidation badge is shown for it.
    pub leverage: f64,
    /// estimated liquidation mark from the ONE scope-parameterized law at the ONE maintenance
    /// rate: Isolated → closed-form `vike_model::liquidation_price`; Cross →
    /// `vike_model::cross_liquidation_price_est` (the shared-pool line with every other position
    /// frozen — advisory, the venue-UI shape); Cash → 0.0 (never liquidates). 0.0 when the
    /// margin gate is off, the position is flat/unmarked, it has no explicit rate (see `leverage`),
    /// or it is not liquidatable by price. "Unmarked" means no ACCOUNT mark (`Account::mark_of`),
    /// even when the price board has a price for the symbol: this badge reads the account slot while
    /// `margin_used` and equity read the board, so a board-priced cross leg without an account mark
    /// shows 0.0 here AND drops out of the other legs' pool, which makes their badges optimistic.
    /// A known gap in what is shown, not in the arithmetic; the crate page records it.
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
    pub account: Option<vike_model::accounts::account_keys::AccountLabel>,
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
    /// Filled at every publish in `runtime::timers`' `mount_views` — per event on a sporadic feed,
    /// so NOT off the gated hop (`mount_views`' comment on this field argues why the gated
    /// harnesses never reach it). `None` on the [`MountRowKind::Residual`] row, which describes no
    /// mount.
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

/// GUI-facing projection of one held (Quarantine / Hybrid-quarantined) `vike_exec::recon::ReconAlert`
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
    /// `vike_model::accounts::account_keys::AccountLabel` states for every carrier of that type.
    ///
    /// [`CoreSnapshot::build`] derives it from the engine's own `route_key` through
    /// `vike_model::accounts::account_keys::label_of_route_key`, so on a block built there it cannot disagree
    /// with [`Self::route_key`] about which account this block is.
    ///
    /// ⚠ That is a property of the BUILDER, not of the type. A block the desktop's observe bridge
    /// rebuilds from the wire reads the label and the route key as two separate wire fields, so a
    /// skewed node could make them disagree; a reader that needs the ROUTING answer derives it from
    /// `route_key` through the same inverse and never trusts this field for it.
    ///
    /// `None` on every single-account process — which is every process with no `[accounts]` table —
    /// so a consumer that ignores it reads what it read before the field existed.
    pub account: Option<vike_model::accounts::account_keys::AccountLabel>,
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
    /// `vike_model::accounts::account_keys::AccountRef::route_key`.
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
    /// free buying power (`vike_model::free_buying_power`); equals `equity` (floored at 0.0) when the
    /// gate is off
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
    /// pointer-copy (`Arc::clone` — no allocation, no deep clone, on a publish path that runs per
    /// event on a sporadic feed). Read through [`VenueBlock::multiplier_of`], never directly: a
    /// symbol ABSENT from this map resolves to `multiplier_default`, not to nothing.
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
    /// cloned at every publish (per event on a sporadic feed — a no-op clone while it is empty).
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
