//! Strategy-driving cluster — the bar/tick/feed-status/params dispatch onto the mounted
//! strategies, the [`LiveBroker`] drain, and conditional-order firing — split out of the runtime
//! fold module (behavior byte-identical; the block moved verbatim). `use super::*` re-exports the
//! parent runtime module's full import set + items, so nothing about resolution changes.
//!
//! ⚠ **Every [`LiveBroker`] built in this file takes its `equity` from
//! [`vike_exec::ExecutionEngine::sizing_equity`], never from `resolved_equity`** — the ceiling
//! `vike_config::Policy::max_sizing_equity` arms, applied at the ONE resolver rather than at each
//! construction site. This is a strategy-facing number: `LiveBroker::order_target_percent` feeds it
//! straight to `vike_model::units_from_percent`, and `vike-script` hands it to a Rhai strategy as
//! `ctx.equity()`, so it is a SPENDING figure and a lower one can only buy less. Unarmed
//! (`None`, every deployment with no `policy.max_sizing_equity` row) it is bit-identical to
//! `resolved_equity`.
//!
//! There is one more strategy-facing equity and it is NOT in this file: the per-fill
//! `AppliedFill::equity_after` that `drive_applied_fills` copies into `ctx.equity` comes off
//! `vike_exec::ExecutionEngine::fold`, computed from `Account::equity_all`. It is capped at its own
//! site through `ExecutionEngine::cap_sizing_equity` — the shared `min` — because a grep for the
//! resolver's name does not find it, which is how three review rounds on the abandoned predecessor
//! each missed a different consumer. `sizing_equity`'s doc is the enumeration.

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
    /// The [`vike_model::Broker::bars`] table — and, because it is the only one of the three that is
    /// COMPLETE for the declared set, the membership oracle `LiveBroker::carries` reads. See
    /// [`CoreThread::push_view`].
    bar_views: Vec<(String, Arc<Vec<Bar>>)>,
}

impl<C: ExecutionClient> CoreThread<C> {
    /// The R7 strategy step for one CLOSED bar of the mounted series — the live twin of
    /// the engine's `step`: (1) the client fills its resting book against this bar
    /// (next-open discipline) and the fills fold through the bus; (2) the mark moves to
    /// the close; (3) EVERY mount subscribed to this series decides on the full closed history,
    /// in mount order, and its orders run the ONE live path (mint → RiskGate → client).
    pub(crate) fn drive_strategy(&mut self, key: &SeriesKey, bar: &Bar) {
        // (1)+(2) fire for EVERY closed bar of any symbol any engine accepts — manual
        // ticket orders paper-fill on live bars even without a strategy mount
        let Some(eidx) = self.engine_idx_for_route_key(RouteKey::sole_account_of(&key.0)) else {
            return;
        };
        if !self.eng(eidx).accepts_symbol(&key.1) {
            return;
        }
        // (1) happened in the BarClose arm: paper fills + their `on_fill` deliveries fold
        // BEFORE the bar joins the cache (backtest ordering: fill → on_fill → index/price
        // advance → on_bar)
        // (2) mark to the close (the engine sets price=close after fills). THIS IS THE LOSSLESS
        // closed-bar lane, and it fires for exactly the symbols an engine holds positions in —
        // so before the law moved into `Account::set_mark_from` this was the highest-frequency
        // stomp of a fresh venue mark, ten lines above the margin-call sweep that reads it.
        let now = self.eng(eidx).now_ms;
        self.eng_mut(eidx).account.set_mark_from(
            &key.0,
            &key.1,
            bar.close,
            MarkSource::BarClose,
            now,
        );
        self.eng_mut(eidx).price_board.set_bar_close(&key.0, &key.1, bar.close, bar.ts);
        // …and onto the venue's OTHER accounts: a bar close is a fact about the exchange, and a
        // second account of it must be able to price its own positions. Inert (one bool read) for
        // every single-account process.
        self.mirror_venue_price(&key.0, &key.1, bar.close, MarkSource::BarClose, now, Some(bar.ts));
        self.dirty = true;
        // Phase B margin-call sweep (opt-in): LEAN's cadence is a 5-min timer; the vike
        // twin sweeps per closed bar of the engine's own series — marks are fresh here and
        // this is the per-bar path, never the event fold (latency gate untouched).
        if let Some(cfg) = self.config.margin_call {
            self.sweep_margin_call(&cfg, bar.ts);
        }
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
            self.sweep_mount_budgets(bar.ts);
        }
        // Phase C: conditional-order check BEFORE the strategy step (oracle firing order —
        // `check_conditionals(sym, bar)` runs before `strategy.on_bar` in LivePump)
        self.fire_conditionals_bar(&key.0.clone(), &key.1.clone(), bar);
        // (3) the strategy step for EVERY mount that hears this closed bar (`Audience::Bar`: who
        // that is, and why EVERY and not the first, is `mount_hears`'s). Mount order; each mount's
        // orders drain before the next mount steps, as on the feed-status lane.
        //
        // An index loop rather than an iterator or a collected `Vec`: each step needs `&mut self`,
        // and collecting would allocate per bar. A step takes and restores only its own slot,
        // restoring it before its drain, so later slots read as they did before it ran.
        for idx in 0..self.mounts.len() {
            if self.mount_hears(idx, &key.0, &key.1, Audience::Bar { interval: &key.2 }) {
                self.step_mount_on_bar(idx, key, bar);
            }
        }
    }

    /// Step (3) of [`Self::drive_strategy`] for ONE subscribed mount: its [`LiveBroker`] over the
    /// mount's OWN engine, `on_bar` past warmup, then its orders drained through the one live path.
    fn step_mount_on_bar(&mut self, idx: usize, key: &SeriesKey, bar: &Bar) {
        // the strategy sees the full closed series (no look-ahead: only <= this bar)
        let bars_arc = Arc::clone(&self.bars.get(key).expect("series just appended").closed);
        let index = bars_arc.len() - 1;
        // ⚠ THE MOUNT'S OWN ENGINE for everything the STRATEGY reads. The engine the MARKET message
        // belongs to (the venue's default account — marks and the price board are per-exchange
        // facts) is `drive_strategy`'s `eidx`; a mount that named an account reads its position,
        // equity, multiplier and lot size from THAT account's book. The two were one index until a
        // mount could name an account, and keeping them one would have left a labelled mount sizing
        // against the default account's position while trading its own — a worse defect than the
        // routing one, and one no order test would catch.
        let meidx = self.mount_eng(idx);
        // BEFORE the take: `declared_views` reads the mount's own (venue, symbol, interval) out of `self.mounts`.
        let views = self.declared_views(idx, None, &key.0);
        // take/replace so the strategy call can't alias the rest of the core state
        let mut mount = self.mounts[idx].take().expect("just found");
        let mut ctx = LiveBroker {
            positions: views.positions,
            prices: views.prices,
            bar_views: views.bar_views,
            position: self.eng(meidx).position_size_of(&key.1, "BOTH"),
            price: bar.close,
            equity: self.eng(meidx).sizing_equity(self.seed_of(meidx), &self.config.price_cfg),
            bars: bars_arc,
            index,
            now: bar.ts,
            multiplier: self.eng(meidx).account.multiplier_of(&key.1),
            lot_size: self.eng(meidx).gate.limits.lot_size.unwrap_or(0.0),
            submissions: Vec::new(),
            modifications: Vec::new(),
            cancels: Vec::new(),
            brackets: Vec::new(),
            conditionals: Vec::new(),
            mass_cancel: false,
        };
        if index >= mount.strategy.warmup() {
            mount.strategy.on_bar(&mut ctx, bar);
        }
        self.mounts[idx] = Some(mount);
        self.drain_broker(ctx, &key.0, &key.1, bar.ts, idx);
    }
}
