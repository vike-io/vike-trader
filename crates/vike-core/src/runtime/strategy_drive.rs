//! Strategy-driving cluster — the bar/tick/feed-status/params dispatch onto the mounted
//! strategies, the [`LiveBroker`] drain, and conditional-order firing — split out of the runtime
//! fold module (behavior byte-identical; the block moved verbatim). `use super::*` re-exports the
//! parent runtime module's full import set + items, so nothing about resolution changes.
//!
//! ⚠ **Every [`LiveBroker`] built in this file takes its `equity` from
//! [`vike_execution::Engine::sizing_equity`], never from `resolved_equity`** — the ceiling
//! `vike_config::Policy::max_sizing_equity` arms, applied at the ONE resolver rather than at each
//! construction site. This is a strategy-facing number: `LiveBroker::order_target_percent` feeds it
//! straight to `vike_model::ContractValue::units_worth`, and `vike-rhai` hands it to a Rhai strategy as
//! `ctx.equity()`, so it is a SPENDING figure and a lower one can only buy less. Unarmed
//! (`None`, every deployment with no `policy.max_sizing_equity` row) it is bit-identical to
//! `resolved_equity`.
//!
//! There is one more strategy-facing equity and it is NOT in this file: the per-fill
//! `AppliedFill::equity_after` that `drive_applied_fills` copies into `ctx.equity` comes off
//! `vike_execution::Engine::fold`, computed from `Engine::resolved_equity` (side-aware since the owner's
//! ruling of 2026-11-15; it read the account slot's mid through `Account::equity_all` before). It is
//! capped at its own site through `Engine::equity_after_fill` (`Engine::sizing_equity` at the engine's own
//! seed and config: the coin-seed cap and the shared `min`), because a grep in this file does not find it,
//! which is how three review rounds on the abandoned predecessor each missed a different consumer.
//! `sizing_equity`'s doc is the enumeration.

mod broker_drain;
mod conditionals;
mod hooks;
mod order_events;
// `pub(super)`: the white-box audience tests (`crates/vike-core/src/runtime/tests/audience.rs`) name
// `Audience`, and they are children of `runtime`, not of this module.
pub(super) mod subscriptions;
mod views;

use subscriptions::Audience;

use super::*;

/// The owned per-symbol read tables a DECLARED-multi mount's [`LiveBroker`] carries
/// (`Default` = all three empty, which is every single-symbol mount and costs nothing).
#[derive(Default)]
pub(crate) struct DeclaredViews {
    positions: Vec<(String, f64)>,
    prices: Vec<(String, f64)>,
    /// The [`vike_model::OrderBroker::bars`] table — and, because it is the only one of the three that is
    /// COMPLETE for the declared set, the membership oracle `LiveBroker::carries` reads. See
    /// [`CoreThread::push_view`].
    bar_views: Vec<(String, Arc<Vec<Bar>>)>,
    /// [`CoreThread::mount_leg_values`]' row for the mount, lent by `Arc::clone` (no lookup, no
    /// allocation per dispatch).
    leg_values: Option<Arc<[crate::runtime::broker::LegFacts]>>,
}

impl<C: ExecutionClient> CoreThread<C> {
    /// The R7 strategy step for one CLOSED bar of the mounted series — the live twin of
    /// the engine's `step`: (1) the client fills its resting book against this bar
    /// (next-open discipline) and the fills fold through the bus; (2) the mark moves to
    /// the close; (3) EVERY mount subscribed to this series decides on the full closed history,
    /// in mount order, and its orders run the ONE live path (mint → RiskGate → client). `sides` is the bar's QUOTE
    /// SIDES where quotes came (`BarUpdate::sides`): the core-held stops are judged on them.
    pub(crate) fn drive_strategy(
        &mut self,
        key: &SeriesKey,
        bar: &Bar,
        sides: Option<&vike_marketdata::BarSides>,
    ) {
        // (1)+(2) fire for EVERY closed bar of any symbol any engine accepts — manual
        // ticket orders paper-fill on live bars even without a strategy mount
        let Some(engine_index) = self.engine_index_for_route_key(RouteKey::sole_account_of(&key.0))
        else {
            return;
        };
        if !self.engine(engine_index).accepts_symbol(&key.1) {
            return;
        }
        // (1) happened in the BarClose arm: paper fills + their `on_fill` deliveries fold
        // BEFORE the bar joins the cache (backtest ordering: fill → on_fill → index/price
        // advance → on_bar)
        // (2) mark to the close (the engine sets price=close after fills). THIS IS THE LOSSLESS
        // closed-bar lane, and it fires for exactly the symbols an engine holds positions in —
        // so before the law moved into `Account::set_mark_from` this was the highest-frequency
        // stomp of a fresh venue mark, ten lines above the margin-call sweep that reads it.
        let now = self.engine(engine_index).now_ms;
        self.engine_mut(engine_index).account.set_mark_from(
            &key.0,
            &key.1,
            bar.close,
            MarkSource::BarClose,
            now,
        );
        self.engine_mut(engine_index).price_board.set_bar_close(
            &key.0,
            &key.1,
            bar.close,
            bar.timestamp,
        );
        // …and onto the venue's OTHER accounts: a bar close is a fact about the exchange, and a
        // second account of it must be able to price its own positions. Inert (one bool read) for
        // every single-account process.
        self.mirror_venue_price(
            &key.0,
            &key.1,
            bar.close,
            MarkSource::BarClose,
            now,
            Some(bar.timestamp),
        );
        self.dirty = true;
        // Phase B margin-call sweep (opt-in): LEAN's cadence is a 5-min timer; the vike
        // twin sweeps per closed bar of the engine's own series — marks are fresh here and
        // this is the per-bar path, never the event fold (latency gate untouched). A bar may be
        // hours apart, so the CLOCK sweeps every engine too (`sweep_risk_clock`, owner ruling
        // 2026-10-25); this bar pass is the one that prices a mounted engine permissively.
        if let Some(config) = self.config.margin_call {
            self.sweep_margin_call(&config, bar.timestamp);
        }
        // A coin ledger's seed is held in coin from its first priced pass, latch or not: the sizing
        // cap reads it (`hold_coin_seeds`, decision 0258).
        self.hold_coin_seeds();
        // Equity-drawdown latch (audit exec#4): same per-closed-bar cadence + equity source as
        // the margin-call sweep above (marks fresh, off the event fold). Disabled unless opted in.
        if let Some(threshold) = self.config.max_drawdown {
            self.sweep_drawdown_latch(threshold);
        }
        // Per-mount BUDGET latch (steal/core-per-mount-budget): the SCOPED sibling of the drawdown
        // latch above — same per-closed-bar cadence + resolver-priced source, but it latches ONE
        // mount at a time (cancel its orders + optional flatten) instead of the whole account, so
        // other mounts keep trading. Gated to a single bool read when no mount has an active budget
        // (the default), byte-identical to a budget-free runtime. Runs BEFORE the strategy step
        // below so a mount latched this bar has its own post-latch intents discarded at drain.
        if self.any_mount_budget {
            self.sweep_mount_budgets(bar.timestamp);
        }
        // Phase C: conditional-order check BEFORE the strategy step (oracle firing order —
        // `check_conditionals(symbol, bar)` runs before `strategy.on_bar` in LivePump)
        self.fire_conditionals_on_bar(&key.0.clone(), &key.1.clone(), bar, sides);
        // (3) the strategy step for EVERY mount that hears this closed bar (`Audience::Bar`: who
        // that is, and why EVERY and not the first, is `mount_hears`'s). Mount order; each mount's
        // orders drain before the next mount steps, as on the feed-status lane.
        //
        // An index loop rather than an iterator or a collected `Vec`: each step needs `&mut self`,
        // and collecting would allocate per bar. A step takes and restores only its own slot,
        // restoring it before its drain, so later slots read as they did before it ran.
        for index in 0..self.mounts.len() {
            if self.mount_hears(index, &key.0, &key.1, Audience::Bar { interval: &key.2 }) {
                // A step of this mount still held at an earlier shared close goes first (another series of
                // it closed meanwhile). One `is_empty()` while nothing is held.
                if !self.shared_closes.is_empty()
                    && self.shared_closes.iter().any(|p| p.mount == index)
                {
                    self.flush_shared_closes_of_mount(index);
                }
                // A SHARED CLOSE (decision 0271, `shared_close.rs`): an input's bar closing at this same
                // instant has not arrived, so this mount's step waits for it (or a short grace). One
                // `Option` read for a mount that reads no input.
                if let Some((close, _)) = self.shared_close_input(index, bar.timestamp) {
                    self.park_shared_close(index, key, bar, close);
                    continue;
                }
                self.step_mount_on_bar(index, key, bar);
            }
        }
    }

    /// Step (3) of [`Self::drive_strategy`] for ONE subscribed mount: its [`LiveBroker`] over the
    /// mount's OWN engine, `on_bar` past warmup, then its orders drained through the one live path.
    fn step_mount_on_bar(&mut self, index: usize, key: &SeriesKey, bar: &Bar) {
        // the strategy sees the full closed series (no look-ahead: only <= this bar)
        let bars_arc = Arc::clone(&self.bars.get(key).expect("series just appended").closed);
        self.step_mount_on_bar_over(index, key, bar, bars_arc);
    }

    /// [`Self::step_mount_on_bar`] over `bars_arc`, the mount's closed series with `bar` LAST: the series as it
    /// stands, or as it stood at a shared close that held the step (`shared_close.rs`).
    pub(crate) fn step_mount_on_bar_over(
        &mut self,
        mount_slot: usize,
        key: &SeriesKey,
        bar: &Bar,
        bars_arc: Arc<Vec<Bar>>,
    ) {
        let index = bars_arc.len() - 1;
        // ⚠ THE MOUNT'S OWN ENGINE for everything the STRATEGY reads. The engine the MARKET message
        // belongs to (the venue's default account — marks and the price board are per-exchange
        // facts) is `drive_strategy`'s `engine_index`; a mount that named an account reads its position,
        // equity, multiplier and lot size from THAT account's book. The two were one index until a
        // mount could name an account, and keeping them one would have left a labelled mount sizing
        // against the default account's position while trading its own — a worse defect than the
        // routing one, and one no order test would catch.
        let meidx = self.mount_engine(mount_slot);
        // BEFORE the take: `declared_views` reads the mount's own (venue, symbol, interval) out of `self.mounts`.
        let views = self.declared_views(mount_slot, None, &key.0);
        // take/replace so the strategy call can't alias the rest of the core state
        let mut mount = self.mounts[mount_slot].take().expect("just found");
        let (position, average_price_value) = self
            .engine(meidx)
            .position_and_average_of(&key.1, vike_model::events::PositionSide::Both);
        let mut ctx = LiveBroker {
            data_delay: self.mount_delays[mount_slot],
            // a bar step decides at its CLOSE, where the backtest's does (decision 0271)
            inputs: self.inputs_at_close(mount_slot, bar.timestamp),
            closed_trades: self.mount_closed[mount_slot].clone(),
            positions: views.positions,
            prices: views.prices,
            bar_views: views.bar_views,
            leg_values: views.leg_values,
            position,
            average_price: average_price_value,
            price: bar.close,
            equity: self
                .engine(meidx)
                .sizing_equity(self.seed_of(meidx), &self.config.price_config),
            bars: bars_arc,
            index,
            now: bar.timestamp,
            contract_value: self.engine(meidx).account.base_value_of(&key.1),
            quote_rate: self.engine(meidx).account.quote_rate_of(&key.1),
            lot_size: self.engine(meidx).gate.limits.lot_size.unwrap_or(0.0),
            tick_size: self.engine(meidx).gate.limits.tick_size.unwrap_or(0.0),
            min_qty: self.engine(meidx).gate.limits.min_qty.unwrap_or(0.0),
            min_notional: self.engine(meidx).gate.limits.min_notional.unwrap_or(0.0),
            submissions: Vec::new(),
            modifications: Vec::new(),
            cancels: Vec::new(),
            brackets: Vec::new(),
            order_lists: Vec::new(),
            combos: Vec::new(),
            order_emulation_arms: Vec::new(),
            mass_cancel: false,
            forecasts: Vec::new(),
        };
        if index >= mount.strategy.warmup() {
            mount.strategy.on_bar(&mut ctx, bar);
        }
        self.mounts[mount_slot] = Some(mount);
        // A `fill = "close"` mount on a paper engine: what this step places fills at this bar's close.
        self.hold_paper_close(mount_slot);
        // Its forecasts at the bar's CLOSE, where it decided; its orders keep the bar's time below.
        self.journal_bar_forecasts(mount_slot, &key.0, &key.1, &key.2, &mut ctx);
        self.drain_broker(ctx, &key.0, &key.1, bar.timestamp, mount_slot);
        self.fill_paper_at_close(mount_slot);
    }
}
