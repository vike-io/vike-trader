//! ConditionalBook — core-side emulated conditional orders (stop / trailing) for venues
//! (or strategies) without native support. Port of `exec/conditionals.py::ConditionalBook`
//! (registration bypasses the gate; the FIRE goes through the ONE live path as a plain
//! market — `live_portfolio_engine.py::check_conditionals` fires BEFORE `strategy.on_bar`
//! each closed bar). The trigger predicate is the r1-parity oracle
//! `vike_model::order_fill_price` — trailing extremes ratchet IN PLACE on no-fire, exactly
//! like the Python book (and the backtest engine).
//!
//! Nautilus-informed upgrade (Rust-native, `CoreConfig::conditionals_on_ticks`): the same
//! book can be checked against tick prices via a degenerate OHLC bar — intra-bar
//! triggering Python explicitly lacks (`conditionals.py` names the bar-close fidelity gap).
//! One book per core (the mount's single (venue, symbol) series today; per-symbol books
//! arrive with the multi-mount runtime).
//!
//! Trigger-source law (w2 `trigger_by`): each arm carries the [`TriggerBy`] source it was
//! requested with ([`ArmedConditional`]), and every check names the [`PriceLane`] its price came
//! from — `None`/`Last` arms evaluate on trade/bar prices (`check_bar`/`check_price`, today's
//! law, byte-identical), `Mark` arms ONLY on the mark lane (`check_mark`, fed from the conflated
//! mark drain — ALWAYS on when a Mark arm rests, not gated by `conditionals_on_ticks`, because
//! the mark lane has no bar-close equivalent). This is what lets the emulator reproduce a
//! mark-triggering venue's timing (hyperliquid) instead of firing everything off last.

use indexmap::IndexMap;
use vike_model::{Bar, OrderKind, TriggerBy, WorkingOrder, one_price_bar, order_fill_price};

/// A fired conditional: submit as a plain reduce-agnostic MARKET through the gate.
#[derive(Debug, Clone, PartialEq)]
pub struct FiredConditional {
    /// the runtime-minted id of the arm that fired — the journal's
    /// [`vike_journal::JournalRecord::ConditionalFire`] key, so a replayed fire is
    /// attributable to the arm it consumed (emulator-journal PR-1).
    pub arm_id: String,
    pub side: i32,
    pub qty: f64,
    /// the oracle's trigger fill price (diagnostic only — the live fill is the venue's)
    pub trigger_px: f64,
}

/// The price LANE a book check is fed from — which price series produced the price under test.
/// [`TriggerBy`] names what an ARM requests; this names what a CHECK carries; the two meet in
/// [`ArmedConditional::evaluates_on`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriceLane {
    /// trade/bar prices — closed bars ([`ConditionalBook::check_bar`]) and the quote/trade/book
    /// tick path ([`ConditionalBook::check_price`]); today's only lane before `trigger_by`
    Last,
    /// the core's mark lane ([`ConditionalBook::check_mark`] — `Ingest::Market` mark ticks)
    Mark,
}

/// One resting arm: the oracle's [`WorkingOrder`] plus the requested trigger source.
/// `trigger_by` `None`/`Some(Last)` evaluates on the [`PriceLane::Last`] lane (today's law,
/// byte-identical); `Some(Mark)` on the mark lane ONLY (no bar/tick ever fires it, and its
/// trailing extreme ratchets on mark prices only). `Some(Index)` matches NO lane — the core has
/// no index lane; the runtime refuses such an arm at apply time, and one smuggled in anyway
/// (e.g. a hand-edited journal) rests INERT rather than firing off the wrong series.
#[derive(Debug, Clone, PartialEq)]
pub struct ArmedConditional {
    pub order: WorkingOrder,
    pub trigger_by: Option<TriggerBy>,
}

impl ArmedConditional {
    /// Does this arm evaluate on `lane`? The one place the request-source → lane law lives.
    fn evaluates_on(&self, lane: PriceLane) -> bool {
        match self.trigger_by {
            None | Some(TriggerBy::Last) => lane == PriceLane::Last,
            Some(TriggerBy::Mark) => lane == PriceLane::Mark,
            Some(TriggerBy::Index) => false,
        }
    }
}

/// Resting conditional orders for one (venue, symbol), keyed by the runtime-minted `arm_id` —
/// uniqueness is STRUCTURAL (emulator PR-2). `IndexMap` (the repo's insertion-order convention)
/// rather than a `Vec`, because `arm_id` became the disarm key: a plain list let two arms carry
/// the same id (`add_*` blindly pushed) and `disarm` retain-dropped EVERY match — an ambiguous id
/// would disarm the wrong arm. The map makes a duplicate id unrepresentable; check/fire iteration
/// stays insertion order, byte-identical to the old `Vec` walk.
#[derive(Default)]
pub struct ConditionalBook {
    orders: IndexMap<String, ArmedConditional>,
    /// count of resting `Some(Mark)` arms — the mark-lane fast-path guard
    /// ([`Self::has_mark_arms`]): the mark lane is conflated-tick cadence, so the runtime must
    /// be able to skip the book walk with one integer compare when nobody asked for Mark.
    mark_arms: usize,
}

impl ConditionalBook {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.orders.is_empty()
    }

    pub fn len(&self) -> usize {
        self.orders.len()
    }

    /// Whether an arm with this id is resting in the book (the disarm lowering's routing probe).
    pub fn contains(&self, arm_id: &str) -> bool {
        self.orders.contains_key(arm_id)
    }

    /// Any resting `Some(Mark)` arms? The runtime's mark-lane fast-path guard: one integer
    /// compare per conflated mark tick when nobody asked for Mark (the default), so the hot
    /// drain never walks a book (nor allocates a key) for nothing.
    pub fn has_mark_arms(&self) -> bool {
        self.mark_arms > 0
    }

    /// Walk every resting arm in INSERTION (= fire) order — the `Snap` capture surface
    /// (emulator PR-3): `write_snap` reads each arm's CURRENT terms (a trailing arm's ratcheted
    /// extreme included, its `trigger_by` too) into the journal's `Snap.conditionals`, so a
    /// restart can re-arm the book exactly as it rested. Read-only; snapshot cadence, never the
    /// per-tick fold.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &ArmedConditional)> {
        self.orders.iter().map(|(id, a)| (id.as_str(), a))
    }

    /// Insert one armed order under `arm_id`, REFUSING a duplicate id: `true` = armed, `false` =
    /// an arm with this id already rests and the book is UNCHANGED (the existing arm is never
    /// silently replaced — `arm_id` is the disarm key, so it must name exactly one arm). Live ids
    /// are minted by a monotone counter and cannot collide; this is the structural backstop.
    fn insert_unique(
        &mut self,
        arm_id: String,
        order: WorkingOrder,
        trigger_by: Option<TriggerBy>,
    ) -> bool {
        match self.orders.entry(arm_id) {
            indexmap::map::Entry::Occupied(_) => false,
            indexmap::map::Entry::Vacant(v) => {
                if trigger_by == Some(TriggerBy::Mark) {
                    self.mark_arms += 1;
                }
                v.insert(ArmedConditional { order, trigger_by });
                true
            }
        }
    }

    /// Arm a fixed stop: fires when the bar crosses `price` (gap-open adverse fill law) on the
    /// lane its `trigger_by` names (see [`ArmedConditional`]; `None` = Last, today's law).
    /// `arm_id` is the runtime-minted name this arm carries into the journal. Returns `false`
    /// (book unchanged) on a duplicate id — see [`Self::insert_unique`].
    #[must_use]
    pub fn add_stop(
        &mut self,
        arm_id: impl Into<String>,
        side: i32,
        qty: f64,
        price: f64,
        trigger_by: Option<TriggerBy>,
    ) -> bool {
        let mut o = WorkingOrder::new(OrderKind::Stop, side, qty);
        o.price = Some(price);
        self.insert_unique(arm_id.into(), o, trigger_by)
    }

    /// Arm a trailing stop with the extreme seeded from the current mark (the caller
    /// refuses mark <= 0, mirroring `live_portfolio_engine.py::submit_trailing`). Evaluates —
    /// AND ratchets — on the lane its `trigger_by` names. Returns `false` (book unchanged) on a
    /// duplicate id — see [`Self::insert_unique`].
    #[must_use]
    pub fn add_trailing(
        &mut self,
        arm_id: impl Into<String>,
        side: i32,
        qty: f64,
        trail: f64,
        extreme: f64,
        trigger_by: Option<TriggerBy>,
    ) -> bool {
        let mut o = WorkingOrder::new(OrderKind::Trailing, side, qty);
        o.trail = Some(trail);
        o.extreme = Some(extreme);
        self.insert_unique(arm_id.into(), o, trigger_by)
    }

    /// Drop THE one arm named by its minted id; `true` when it was present. The individual-cancel
    /// primitive behind [`vike_exec::OrderIntent::DisarmConditional`] (`MassCancel` stays the
    /// coarse verb). Exactly-one is BY CONSTRUCTION now (the book is keyed by `arm_id`);
    /// `shift_remove` (not `swap_remove`) keeps the survivors' insertion order, so fire order
    /// across a disarm is unchanged. Unknown id = `false`, never a panic (stale-click tolerance).
    pub fn disarm(&mut self, arm_id: &str) -> bool {
        match self.orders.shift_remove(arm_id) {
            Some(a) => {
                if a.trigger_by == Some(TriggerBy::Mark) {
                    self.mark_arms -= 1;
                }
                true
            }
            None => false,
        }
    }

    /// Check the arms resting on `lane` against a bar: fired orders are REMOVED and returned;
    /// survivors keep their (possibly ratcheted) trailing extremes — the oracle's partition
    /// semantics (`conditionals.py::check`). Arms on the OTHER lane are untouched: not fired,
    /// not ratcheted — their price series simply did not tick. Walk + fire order is the book's
    /// insertion order (`IndexMap::retain` preserves it), exactly as the old `Vec` walk did.
    fn check_lane(&mut self, bar: &Bar, lane: PriceLane) -> Vec<FiredConditional> {
        let mut fired = Vec::new();
        let mark_arms = &mut self.mark_arms;
        self.orders.retain(|arm_id, armed| {
            if !armed.evaluates_on(lane) {
                return true;
            }
            match order_fill_price(&mut armed.order, bar) {
                Some(px) => {
                    if armed.trigger_by == Some(TriggerBy::Mark) {
                        *mark_arms -= 1;
                    }
                    fired.push(FiredConditional {
                        arm_id: arm_id.clone(),
                        side: armed.order.side,
                        qty: armed.order.size,
                        trigger_px: px,
                    });
                    false
                }
                None => true,
            }
        });
        fired
    }

    /// Check every Last-lane conditional against a closed bar — today's law, byte-identical for
    /// every arm without a requested source. Mark arms never fire here (see [`Self::check_mark`]).
    pub fn check_bar(&mut self, bar: &Bar) -> Vec<FiredConditional> {
        self.check_lane(bar, PriceLane::Last)
    }

    /// Tick-price check (Rust-native upgrade): a degenerate OHLC bar at `px` runs the same
    /// oracle predicate — a Last-lane stop crossed by this tick fires now, not at bar close.
    pub fn check_price(&mut self, px: f64, ts: i64) -> Vec<FiredConditional> {
        self.check_lane(&one_price_bar(ts, px), PriceLane::Last)
    }

    /// MARK-lane twin of [`Self::check_price`]: the same degenerate-bar oracle predicate, fed a
    /// mark tick, evaluating ONLY the `Some(Mark)` arms (their trailing extremes ratchet here
    /// too). The runtime calls this off the conflated mark drain, guarded by
    /// [`Self::has_mark_arms`].
    pub fn check_mark(&mut self, px: f64, ts: i64) -> Vec<FiredConditional> {
        self.check_lane(&one_price_bar(ts, px), PriceLane::Mark)
    }

    /// Drop every resting conditional (`cancel_all` clears the book — oracle semantics).
    pub fn clear(&mut self) {
        self.orders.clear();
        self.mark_arms = 0;
    }
}

#[path = "emulator_tests.rs"]
#[cfg(test)]
mod emulator_tests;
