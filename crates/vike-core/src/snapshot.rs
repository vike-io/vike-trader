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
impl Default for VenueBlock {
    fn default() -> Self {
        VenueBlock {
            venue: String::new(),
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
    pub recon_coin_deltas: indexmap::IndexMap<(String, String), f64>,
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
        }
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
                    // (`vike_model::liquidation`) at the ONE maintenance rate (`mm_rate` — the
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
        for (venue, registry) in std::iter::once((&engine.venue, &engine.registry))
            .chain(extra_engines.iter().map(|(_, e)| (&e.venue, &e.registry)))
        {
            for (coid, mo) in registry.iter() {
                orders.push(OrderView {
                    client_order_id: coid.clone(),
                    venue: venue.clone(),
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

#[cfg(test)]
mod query_tests {
    use super::*;
    use vike_exec::{OrderStatus, TradingState};

    fn ov(coid: &str, symbol: &str, status: OrderStatus) -> OrderView {
        OrderView {
            client_order_id: coid.into(),
            venue: "sim".into(),
            symbol: symbol.into(),
            side: 1,
            qty: 1.0,
            order_type: "limit".into(),
            price: Some(1.0),
            trigger_price: None,
            status,
            venue_order_id: None,
            filled_qty: 0.0,
            avg_fill_px: 0.0,
        }
    }

    fn snap() -> CoreSnapshot {
        let mut s = CoreSnapshot::empty("sim", "BTCUSDT");
        s.orders = vec![
            ov("a", "BTCUSDT", OrderStatus::Accepted),
            ov("b", "ETHUSDT", OrderStatus::Filled),
        ];
        s.positions = vec![PositionView {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            position_side: "BOTH".into(),
            size: 2.0,
            avg_px: 100.0,
            unrealized: 0.0,
            mark_source: None,
            leverage: 0.0,
            liq_price: 0.0,
            margin_mode: vike_model::MarginMode::Cross,
            isolated_margin: None,
        }];
        s.marks = vec![("sim".into(), "BTCUSDT".into(), 101.0)];
        s.portfolio.equity_total = 1234.0;
        s.trading_state = TradingState::Halted;
        s
    }

    #[test]
    fn accessors_read_the_existing_vecs() {
        let s = snap();
        assert_eq!(s.orders_for("BTCUSDT").count(), 1);
        assert_eq!(s.order("b").unwrap().symbol, "ETHUSDT");
        assert!(s.open_order("a").is_some(), "Accepted is non-terminal");
        assert!(s.open_order("b").is_none(), "Filled is terminal");
        assert_eq!(s.position("sim", "BTCUSDT").unwrap().size, 2.0);
        assert!(s.position("sim", "NOPE").is_none());
        assert_eq!(s.last_mark("sim", "BTCUSDT"), Some(101.0));
        assert_eq!(s.last_mark("sim", "NOPE"), None);
        assert_eq!(s.equity(), 1234.0);
        assert_eq!(s.trading_state(), TradingState::Halted);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_exec::MarkSource;
    use vike_exec::testing::RecordingClient;
    use vike_exec::{Account, PositionEntry, RiskGate, RiskLimits};

    /// One-position engine: venue "binance", symbol "BTC", long 1 @ 100, Delta mode, no
    /// board prices/marks (mirrors vike-exec's `resolve_equity.rs` integration-test convention).
    fn engine_with_position() -> ExecutionEngine<RecordingClient> {
        let mut e = ExecutionEngine::new(
            Account::new(1.0, "binance", None, BalanceMode::Delta),
            RiskGate::new(RiskLimits::new()),
            RecordingClient::default(),
            "binance",
            "BTC",
        );
        e.account.positions.insert(
            ("binance".into(), "BTC".into(), "LONG".into()),
            PositionEntry { size: 1.0, avg_px: 100.0, ..Default::default() },
        );
        e
    }

    fn build(e: &ExecutionEngine<RecordingClient>, seed: f64) -> CoreSnapshot {
        CoreSnapshot::build(
            1,
            e,
            &[],
            seed,
            PriceCfg::default(),
            vike_exec::MarginCallConfig::default().mm_requirement,
            &std::collections::VecDeque::new(),
            &indexmap::IndexMap::new(),
            &[],
            &None,
            0,
            0,
            ReconBlock::default(),
            &[],
        )
    }

    fn build_snapshot_with_quote_only_position() -> CoreSnapshot {
        let mut e = engine_with_position();
        // bid/ask on the board, NO mark -> resolver falls through to the side-appropriate quote
        e.price_board.set_quote("binance", "BTC", 104.0, 106.0, 1);
        build(&e, 1_000.0)
    }

    fn build_snapshot_no_feed_and_legacy() -> (CoreSnapshot, f64) {
        let e = engine_with_position();
        let legacy_equity_total = e.account.equity_all(1_000.0);
        (build(&e, 1_000.0), legacy_equity_total)
    }

    /// ⚠ **The premise every cross-venue REPORT rests on, pinned against the real builder.**
    /// `build` binds `let acc = &engine.account` — the PRIMARY — into the scalar
    /// `Portfolio::realized_pnl`/`fees_paid` and into `CoreSnapshot::positions`. On the CI box the
    /// primary is the untraded binance paper engine (`vike_run::WIRED_MARKETS` lists it first) and
    /// the traded mount is a NON-primary engine, so those three scalars structurally cannot see the
    /// venue that moves — while `equity_total`, `orders` and the `*_total` folds can. This test is
    /// the machine-checked statement of that asymmetry, so `vike-tradehub`'s `summary_line` tests
    /// may hand-build the same shape without hand-waving that `build` really produces it.
    #[test]
    fn the_primary_mirroring_scalars_cannot_see_a_non_primary_engines_fills() {
        let primary = ExecutionEngine::new(
            Account::new(1.0, "binance", None, BalanceMode::Delta),
            RiskGate::new(RiskLimits::new()),
            RecordingClient::default(),
            "binance",
            "BTC",
        );
        let mut secondary = ExecutionEngine::new(
            Account::new(1.0, "bybit", None, BalanceMode::Delta),
            RiskGate::new(RiskLimits::new()),
            RecordingClient::default(),
            "bybit",
            "BTC",
        );
        secondary.account.realized_pnl = 12.5;
        secondary.account.fees_paid = 0.75;
        secondary.account.positions.insert(
            ("bybit".into(), "BTC".into(), "LONG".into()),
            PositionEntry { size: -0.25, avg_px: 100.0, ..Default::default() },
        );
        let snap = CoreSnapshot::build(
            1,
            &primary,
            &[(1_000.0, secondary)],
            1_000.0,
            PriceCfg::default(),
            vike_exec::MarginCallConfig::default().mm_requirement,
            &std::collections::VecDeque::new(),
            &indexmap::IndexMap::new(),
            &[],
            &None,
            0,
            0,
            ReconBlock::default(),
            &[],
        );
        // The primary-mirroring half: blind to the venue that traded.
        assert_eq!(snap.portfolio.realized_pnl, 0.0, "the scalar mirrors the PRIMARY engine");
        assert_eq!(snap.portfolio.fees_paid, 0.0, "ditto — binance paid no fees");
        assert!(snap.positions.is_empty(), "`positions` mirrors the PRIMARY venue's rows");
        // The cross-venue half: sees it.
        assert_eq!(snap.portfolio.realized_pnl_total(), 12.5);
        assert_eq!(snap.portfolio.fees_paid_total(), 0.75);
        assert_eq!(snap.portfolio.position_count(), 1);
        assert_eq!(snap.portfolio.net_position("BTC"), -0.25);
    }

    #[test]
    fn snapshot_prices_positions_through_resolver() {
        // build an engine with a position + a board bid/ask but NO mark;
        // snapshot equity should reflect the resolver-valued position, and the
        // PositionView should carry unrealized + a Some(mark_source).
        let snap = build_snapshot_with_quote_only_position();
        let vb = &snap.portfolio.venues[0];
        assert_eq!(vb.missing_prices, 0);
        assert!(vb.unrealized != 0.0);
        assert!(snap.positions[0].mark_source.is_some());
        assert!(snap.positions[0].unrealized != 0.0);
    }

    #[test]
    fn margin_fields_inert_when_gate_off() {
        // default RiskLimits (im_requirement None, empty im_by_symbol) → the margin machinery
        // contributes nothing: margin_used/ratio 0, free_bp == equity, no leverage/liq on legs.
        let snap = build_snapshot_with_quote_only_position();
        assert_eq!(snap.portfolio.margin_used_total, 0.0);
        let vb = &snap.portfolio.venues[0];
        assert_eq!(vb.margin_used, 0.0);
        assert_eq!(vb.margin_ratio, 0.0);
        assert_eq!(vb.free_bp.to_bits(), vb.equity.to_bits());
        assert!(snap.positions.iter().all(|p| p.leverage == 0.0 && p.liq_price == 0.0));
    }

    #[test]
    fn snapshot_margin_used_counts_no_override_position_matching_the_gate() {
        // DIVERGENCE FIX (dedup A1): per-symbol margin armed for BTC ONLY (SetMargin writes
        // im_by_symbol, leaves the global im_requirement unset). A second position (ETH) has no
        // per-symbol override. The OLD snapshot fold used `im_for(s)` with no fallback and SKIPPED
        // ETH, understating margin_used vs what the pre-trade gate enforces. The published number
        // must now include ETH at the account's max armed rate.
        let mut im_by_symbol: indexmap::IndexMap<String, f64> = indexmap::IndexMap::new();
        im_by_symbol.insert("BTC".to_string(), 0.2);
        let limits = RiskLimits { im_by_symbol, ..RiskLimits::new() }; // im_requirement stays None
        let mut e = ExecutionEngine::new(
            Account::new(1.0, "binance", None, BalanceMode::Delta),
            RiskGate::new(limits),
            RecordingClient::default(),
            "binance",
            "BTC",
        );
        // BTC long 1 @ 100, board-marked 100 → 1·100·1·0.2 = 20 (has its own rate). The
        // margin fold is resolver-priced now, so the tests feed the BOARD's mark slot (the
        // live write-sites store both `account.marks` and the board together).
        e.account.positions.insert(
            ("binance".into(), "BTC".into(), "LONG".into()),
            PositionEntry { size: 1.0, avg_px: 100.0, ..Default::default() },
        );
        e.price_board.set_mark("binance", "BTC", 100.0, 0);
        // ETH long 2 @ 50, board-marked 50, NO per-symbol override → falls back to max armed
        // (0.2): 2·50·1·0.2 = 20. Old fold skipped it (would have published 20 total).
        e.account.positions.insert(
            ("binance".into(), "ETH".into(), "LONG".into()),
            PositionEntry { size: 2.0, avg_px: 50.0, ..Default::default() },
        );
        e.price_board.set_mark("binance", "ETH", 50.0, 0);

        let snap = build(&e, 10_000.0);
        let vb = &snap.portfolio.venues[0];
        // 20 (BTC) + 20 (ETH now counted) = 40 — the gate's own basis, not the old skipped 20.
        assert_eq!(vb.margin_used.to_bits(), 40.0_f64.to_bits());
        // and the gate, checking a BTC order, computes the SAME 40 for the existing book
        // (the gate's own resolver-priced fold — the same authority the snapshot publishes):
        let gate_used = e.resolved_margin_in_use_by(&PriceCfg::default(), |(_v, s, _side), p| {
            p.margin_mode.is_cross().then(|| e.gate.limits.im_for(s).unwrap_or(0.2))
        });
        assert_eq!(vb.margin_used.to_bits(), gate_used.to_bits());
    }

    #[test]
    fn snapshot_margin_used_with_global_im_requirement_never_consults_the_fallback() {
        // The byte-identity guard for the dedup A1 fix: with a GLOBAL `im_requirement` set,
        // `im_for(s)` is Some for EVERY symbol, so the new max-armed-rate fallback is never
        // consulted and the published number is exactly what the pre-fix fold produced —
        // no drift from the extraction. (The fallback only ever engages in the per-symbol-only
        // arm proven above; this pins the far more common global-margin path as unchanged.)
        let limits = RiskLimits { im_requirement: Some(0.1), ..RiskLimits::new() };
        let mut e = ExecutionEngine::new(
            Account::new(1.0, "binance", None, BalanceMode::Delta),
            RiskGate::new(limits),
            RecordingClient::default(),
            "binance",
            "BTC",
        );
        // Two positions, neither with a per-symbol override → both price off im_requirement 0.1
        // (board-marked: the fold reads the resolver, not `account.marks`).
        e.account.positions.insert(
            ("binance".into(), "BTC".into(), "LONG".into()),
            PositionEntry { size: 1.0, avg_px: 100.0, ..Default::default() },
        );
        e.price_board.set_mark("binance", "BTC", 100.0, 0); // 1·100·1·0.1 = 10
        e.account.positions.insert(
            ("binance".into(), "ETH".into(), "LONG".into()),
            PositionEntry { size: 2.0, avg_px: 50.0, ..Default::default() },
        );
        e.price_board.set_mark("binance", "ETH", 50.0, 0); // 2·50·1·0.1 = 10
        let snap = build(&e, 10_000.0);
        // 10 + 10 = 20, folded off the global rate with the fallback untouched.
        assert_eq!(snap.portfolio.venues[0].margin_used.to_bits(), 20.0_f64.to_bits());
    }

    /// MAJOR-3 (the liquidation law's partition in the PUBLISHED margin number): only Cross
    /// positions price into `VenueBlock::margin_used` — an Isolated position is backed by its
    /// own wallet and a Cash position is fully funded, so folding them in overstated a mixed
    /// account's shared margin (and understated free_bp). All-cross accounts are unchanged
    /// (the dedup-A1 pins above still pass byte-identically — filter no-op).
    #[test]
    fn snapshot_margin_used_excludes_isolated_and_cash_positions() {
        use vike_model::MarginMode;
        let limits = RiskLimits { im_requirement: Some(0.1), ..RiskLimits::new() };
        let mut e = ExecutionEngine::new(
            Account::new(1.0, "binance", None, BalanceMode::Delta),
            RiskGate::new(limits),
            RecordingClient::default(),
            "binance",
            "BTC",
        );
        // cross BTC long 1 @ 100, board-marked → 1·100·1·0.1 = 10 (the only shared-pool row;
        // the fold is resolver-priced, so the board carries the prices)
        e.account.positions.insert(
            ("binance".into(), "BTC".into(), "LONG".into()),
            PositionEntry { size: 1.0, avg_px: 100.0, ..Default::default() },
        );
        e.price_board.set_mark("binance", "BTC", 100.0, 0);
        // isolated ETH long 2 @ 50, board-marked, wallet 10 → would add 2·50·1·0.1 = 10 if counted
        e.account.positions.insert(
            ("binance".into(), "ETH".into(), "LONG".into()),
            PositionEntry {
                size: 2.0,
                avg_px: 50.0,
                margin_mode: MarginMode::Isolated,
                isolated_margin: Some(10.0),
            },
        );
        e.price_board.set_mark("binance", "ETH", 50.0, 0);
        // cash SOL long 3 @ 10, board-marked → would add 3·10·1·0.1 = 3 if counted
        e.account.positions.insert(
            ("binance".into(), "SOL".into(), "LONG".into()),
            PositionEntry {
                size: 3.0,
                avg_px: 10.0,
                margin_mode: MarginMode::Cash,
                ..Default::default()
            },
        );
        e.price_board.set_mark("binance", "SOL", 10.0, 0);

        let snap = build(&e, 10_000.0);
        let vb = &snap.portfolio.venues[0];
        // ONLY the cross row: 10 — not the unfiltered 23 (10 + iso 10 + cash 3).
        assert_eq!(vb.margin_used.to_bits(), 10.0_f64.to_bits());
        // and it matches the admitting gate's own partitioned resolver-priced fold
        // (same authority, same law, same price basis):
        let gate_used = e.resolved_margin_in_use_by(&PriceCfg::default(), |(_v, s, _side), p| {
            p.margin_mode.is_cross().then(|| e.gate.limits.im_for(s).unwrap_or(0.1))
        });
        assert_eq!(vb.margin_used.to_bits(), gate_used.to_bits());
    }

    #[test]
    fn liq_badge_routes_by_margin_mode_at_the_one_rate() {
        // The scope-parameterized law's badge: Isolated → closed-form at the REAL maintenance
        // rate (the `im * 0.5` hardcode is dead), Cross → the shared-pool estimate, Cash → none.
        // im 0.2 on purpose: im·0.5 = 0.1 ≠ the maint rate 0.05, so the isolated assertion
        // below can only pass through the REAL-rate call, never the retired `im * 0.5` shape.
        let limits = RiskLimits { im_requirement: Some(0.2), ..RiskLimits::new() };
        let mut e = ExecutionEngine::new(
            Account::new(1.0, "binance", None, BalanceMode::Delta),
            RiskGate::new(limits),
            RecordingClient::default(),
            "binance",
            "BTC",
        );
        // cross BTC long 1 @ 100 (marked 100)
        e.account.positions.insert(
            ("binance".into(), "BTC".into(), "LONG".into()),
            PositionEntry { size: 1.0, avg_px: 100.0, ..Default::default() },
        );
        e.account.set_mark_from("binance", "BTC", 100.0, MarkSource::VenueMark, 0);
        // isolated ETH long 2 @ 50 (marked 50), wallet 10
        e.account.positions.insert(
            ("binance".into(), "ETH".into(), "LONG".into()),
            PositionEntry {
                size: 2.0,
                avg_px: 50.0,
                margin_mode: vike_model::MarginMode::Isolated,
                isolated_margin: Some(10.0),
            },
        );
        e.account.set_mark_from("binance", "ETH", 50.0, MarkSource::VenueMark, 0);
        // cash SOL long 3 @ 10 (marked 10)
        e.account.positions.insert(
            ("binance".into(), "SOL".into(), "LONG".into()),
            PositionEntry {
                size: 3.0,
                avg_px: 10.0,
                margin_mode: vike_model::MarginMode::Cash,
                ..Default::default()
            },
        );
        e.account.set_mark_from("binance", "SOL", 10.0, MarkSource::VenueMark, 0);

        // seed 60 → equity 60; the ONE rate in the test builder is the watchdog default 0.05
        let mm = vike_exec::MarginCallConfig::default().mm_requirement;
        let snap = build(&e, 60.0);
        let by_sym = |s: &str| snap.positions.iter().find(|p| p.symbol == s).unwrap();

        // Cross: pool equity = 60 − (wallet 10 + iso upnl 0) = 50; cross notional = BTC's 100
        // only (SOL is Cash — outside every pool); others = 0.
        let want_cross = vike_model::cross_liquidation_price_est(50.0, 1.0, 100.0, 1.0, 0.0, mm);
        assert!(want_cross > 0.0, "scenario sanity: the cross badge must be live");
        assert_eq!(by_sym("BTC").liq_price.to_bits(), want_cross.to_bits());
        // Isolated: closed-form at im 0.2 and the REAL maint rate 0.05 (im·0.5 would be 0.1):
        let want_iso = vike_model::liquidation_price(50.0, 1, 0.2, mm);
        assert_eq!(by_sym("ETH").liq_price.to_bits(), want_iso.to_bits());
        // Cash: never liquidates → no badge.
        assert_eq!(by_sym("SOL").liq_price, 0.0);
    }

    #[test]
    fn snapshot_surfaces_the_engines_fee_schedule() {
        // fee model follow-up 1: the mount tags each engine with its resolved fee schedule and the
        // snapshot must surface it read-only on the per-venue block for the GUI cost display.
        let mut e = engine_with_position();
        let sched = vike_model::FeeSchedule::PercentMakerTaker { maker_bps: 2.0, taker_bps: 5.5 };
        e.fee_schedule = Some(sched);
        let snap = build(&e, 1_000.0);
        assert_eq!(snap.portfolio.venues[0].fee_schedule, Some(sched));
        // an untagged engine surfaces None (default path unchanged).
        let untagged = engine_with_position();
        assert_eq!(build(&untagged, 1_000.0).portfolio.venues[0].fee_schedule, None);
    }

    /// The per-position `margin_mode`/`isolated_margin` carrier surfaces on `PositionView`
    /// (feat/margin-mode-field): a default position reads Cross/None; an isolated position flows
    /// its mode AND its allocated wallet straight from the account's `PositionEntry`.
    #[test]
    fn position_view_surfaces_the_margin_mode_carrier() {
        use vike_model::MarginMode;
        let mut e = engine_with_position(); // seeds a cross BTC long 1 @ 100
        e.account.positions.insert(
            ("binance".into(), "ETH".into(), "BOTH".into()),
            PositionEntry {
                size: 2.0,
                avg_px: 50.0,
                margin_mode: MarginMode::Isolated,
                isolated_margin: Some(40.0),
            },
        );
        let snap = build(&e, 1_000.0);
        let btc = snap.positions.iter().find(|p| p.symbol == "BTC").unwrap();
        assert_eq!(btc.margin_mode, MarginMode::Cross, "default position is cross");
        assert_eq!(btc.isolated_margin, None);
        let eth = snap.positions.iter().find(|p| p.symbol == "ETH").unwrap();
        assert_eq!(eth.margin_mode, MarginMode::Isolated);
        assert_eq!(eth.isolated_margin, Some(40.0));
    }

    #[test]
    fn snapshot_no_feed_equity_is_byte_identical() {
        // no board prices, no marks -> equity_total bits identical to the legacy law.
        let (snap, legacy_equity_total) = build_snapshot_no_feed_and_legacy();
        assert_eq!(snap.portfolio.equity_total.to_bits(), legacy_equity_total.to_bits());
        assert_eq!(snap.portfolio.missing_prices_total, snap.positions.len() as u32);
        assert!(snap.positions.iter().all(|p| p.mark_source.is_none()));
    }

    /// `CoreSnapshot::build` (portfolio-observer PR-4 T5) must thread the given `&[MountView]`
    /// straight into `snap.mounts`, preserving each mount's `ready` flag — the runtime's own
    /// readiness state (Pending vs. Ready) surfaced for the GUI/control-boundary read side.
    #[test]
    fn snapshot_exposes_mount_readiness() {
        let e = engine_with_position();
        let mounts = vec![
            MountView {
                kind: MountRowKind::Mount,
                venue: "sim".into(),
                symbol: "BTC".into(),
                interval: "1m".into(),
                ready: false,
                position: 0.0,
                realized_pnl: 0.0,
                unrealized_pnl: 0.0,
                notional: 0.0,
                budget: None,
                latched: false,
                params: None,
            },
            MountView {
                kind: MountRowKind::Mount,
                venue: "sim".into(),
                symbol: "ETH".into(),
                interval: "5m".into(),
                ready: true,
                position: 0.0,
                realized_pnl: 0.0,
                unrealized_pnl: 0.0,
                notional: 0.0,
                budget: None,
                latched: false,
                params: None,
            },
        ];
        let snap = CoreSnapshot::build(
            1,
            &e,
            &[],
            1_000.0,
            PriceCfg::default(),
            vike_exec::MarginCallConfig::default().mm_requirement,
            &std::collections::VecDeque::new(),
            &indexmap::IndexMap::new(),
            &mounts,
            &None,
            0,
            0,
            ReconBlock::default(),
            &[],
        );
        assert_eq!(snap.mounts, mounts, "build() must thread the mount views through verbatim");
        assert!(!snap.mounts[0].ready, "a Pending mount must surface ready: false");
        assert!(snap.mounts[1].ready, "a Ready mount must surface ready: true");
    }

    /// An engine whose account carries a bespoke multiplier grid (the deribit-options shape:
    /// contract size != 1), holding a position in exactly ONE of the graded symbols.
    fn engine_with_multipliers() -> ExecutionEngine<RecordingClient> {
        let mut mults: indexmap::IndexMap<String, f64> = indexmap::IndexMap::new();
        mults.insert("BTC-PERP".to_string(), 10.0);
        // deliberately NEVER traded — the options confirm-ticket case
        mults.insert("BTC-30AUG26-90000-C".to_string(), 0.1);
        let mut e = ExecutionEngine::new(
            Account::new(1.0, "deribit", Some(mults), BalanceMode::Delta),
            RiskGate::new(RiskLimits::new()),
            RecordingClient::default(),
            "deribit",
            "BTC-PERP",
        );
        e.account.positions.insert(
            ("deribit".into(), "BTC-PERP".into(), "LONG".into()),
            PositionEntry { size: 1.0, avg_px: 100.0, ..Default::default() },
        );
        e
    }

    #[test]
    fn snapshot_exposes_the_engine_contract_multiplier() {
        let e = engine_with_multipliers();
        let snap = build(&e, 1_000.0);
        // The snapshot must agree with the ONE authority, `Account::multiplier_of`.
        assert_eq!(snap.multiplier_of("deribit", "BTC-PERP"), 10.0);
        assert_eq!(snap.multiplier_of("deribit", "BTC-PERP"), e.account.multiplier_of("BTC-PERP"));
        assert_eq!(snap.portfolio.venues[0].multiplier_of("BTC-PERP"), 10.0);
    }

    #[test]
    fn snapshot_multiplier_resolves_for_a_symbol_with_no_position() {
        // THE motivating case: the deribit options confirm-ticket prices a contract the account
        // has never traded. If this ever regresses to "position-only", the pre-submit notional cap
        // silently under-measures options notional again (the #458/#468/#477 bug class).
        let e = engine_with_multipliers();
        let snap = build(&e, 1_000.0);
        assert!(
            snap.position("deribit", "BTC-30AUG26-90000-C").is_none(),
            "precondition: the account holds no position in this symbol"
        );
        assert_eq!(snap.multiplier_of("deribit", "BTC-30AUG26-90000-C"), 0.1);
    }

    #[test]
    fn snapshot_multiplier_defaults_to_one_for_ungraded_symbols_and_venues() {
        let e = engine_with_multipliers();
        let snap = build(&e, 1_000.0);
        // absent from the grid → the account's legacy scalar default (1.0 here)
        assert_eq!(snap.multiplier_of("deribit", "ETH-PERP"), 1.0);
        assert_eq!(snap.portfolio.venues[0].multiplier_default, 1.0);
        // unknown venue → 1.0 (documented permissive answer, NOT proof of an unlevered instrument)
        assert_eq!(snap.multiplier_of("nosuchvenue", "BTC-PERP"), 1.0);
        // an engine built with no grid at all publishes an empty map, every symbol 1.0
        let plain = build(&engine_with_position(), 1_000.0);
        assert!(plain.portfolio.venues[0].multipliers.is_empty());
        assert_eq!(plain.multiplier_of("binance", "BTC"), 1.0);
    }

    #[test]
    fn snapshot_multiplier_grid_is_shared_by_pointer_not_deep_cloned() {
        // The publish path is coalesced but still must not deep-clone per venue per tick: two
        // successive builds share ONE allocation with the engine's account.
        let e = engine_with_multipliers();
        let a = build(&e, 1_000.0);
        let b = build(&e, 1_000.0);
        assert!(std::sync::Arc::ptr_eq(
            &a.portfolio.venues[0].multipliers,
            &b.portfolio.venues[0].multipliers
        ));
        assert!(std::sync::Arc::ptr_eq(
            &a.portfolio.venues[0].multipliers,
            &e.account.multiplier_grid()
        ));
    }

    #[test]
    fn snapshot_exposure_helpers_price_off_the_published_marks() {
        let mut e = engine_with_position(); // binance BTC long 1 @ 100 (mult 1)
        e.account.positions.insert(
            ("binance".into(), "ETH".into(), "BOTH".into()),
            PositionEntry { size: -2.0, avg_px: 60.0, ..Default::default() },
        );
        e.account.set_mark_from("binance", "BTC", 105.0, MarkSource::VenueMark, 0);
        e.account.set_mark_from("binance", "ETH", 50.0, MarkSource::VenueMark, 0);
        let snap = build(&e, 1_000.0);
        // BTC 1·105 = +105 ; ETH -2·50 = -100.
        assert_eq!(snap.net_exposure("binance", "BTC").to_bits(), 105.0_f64.to_bits());
        assert_eq!(snap.net_exposure("binance", "ETH").to_bits(), (-100.0_f64).to_bits());
        // cross-venue total for BTC = 105 (only binance holds it).
        assert_eq!(snap.net_exposure_total("BTC").to_bits(), 105.0_f64.to_bits());
        // gross never nets long vs short: 105 + 100 = 205.
        assert_eq!(snap.gross_exposure().to_bits(), 205.0_f64.to_bits());
        // unknown venue / symbol → 0.0.
        assert_eq!(snap.net_exposure("okx", "BTC"), 0.0);
        assert_eq!(snap.net_exposure("binance", "NOPE"), 0.0);
    }

    #[test]
    fn snapshot_exposure_folds_the_contract_multiplier() {
        // deribit BTC-PERP has contract multiplier 10 → notional folds the multiplier.
        let mut e = engine_with_multipliers(); // deribit BTC-PERP long 1 @ 100, mult 10
        e.account.set_mark_from("deribit", "BTC-PERP", 100.0, MarkSource::VenueMark, 0);
        let snap = build(&e, 1_000.0);
        assert_eq!(snap.net_exposure("deribit", "BTC-PERP").to_bits(), 1_000.0_f64.to_bits());
        assert_eq!(snap.gross_exposure().to_bits(), 1_000.0_f64.to_bits());
    }
}
