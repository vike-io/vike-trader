//! ConditionalArms — core-side emulated conditional orders (stop / trailing, and the three that
//! release a limit, below) for venues (or strategies) without native support. Port of
//! `exec/conditionals.py::ConditionalArms` (registration bypasses the gate; the FIRE goes through
//! the ONE live path as a plain market — `live_portfolio_engine.py::check_conditionals` fires
//! BEFORE `strategy.on_bar` each closed bar). The trigger predicate is the r1-parity oracle
//! `vike_model::order_fill_price` — trailing extremes ratchet IN PLACE on no-fire, exactly
//! like the Python book (and the backtest engine).
//!
//! Tick-driven upgrade (Rust-native, `CoreConfig::conditionals_on_ticks`): the same
//! book can be checked against tick prices via a degenerate OHLC bar — intra-bar
//! triggering Python explicitly lacks (`conditionals.py` names the bar-close fidelity gap).
//! One book per core (the mount's single (venue, symbol) series today; per-symbol books
//! arrive with the multi-mount runtime).
//!
//! Trigger-source law (w2 `trigger_by`): each arm carries the [`TriggerBy`] source it was
//! requested with ([`OrderEmulationArm`]), and every check names the [`OrderEmulationPriceLane`] its price came
//! from — `None`/`Last` arms evaluate on trade/bar prices (`check_bar`/`check_price`, today's
//! law, byte-identical), `Mark` arms ONLY on the mark lane (`check_mark`, fed from the conflated
//! mark drain — ALWAYS on when a Mark arm rests, not gated by `conditionals_on_ticks`, because
//! the mark lane has no bar-close equivalent). This is what lets the emulator reproduce a
//! mark-triggering venue's timing (hyperliquid) instead of firing everything off last.
//!
//! **Five kinds, two releases.** A `Stop`, a `Trailing` stop and a market-if-touched `TakeProfit` (no
//! limit) fire a MARKET; a `StopLimit`, a limit-if-touched `TakeProfit` and a `TrailingLimit` fire a
//! LIMIT at [`OrderEmulationFire::limit`] (the stop-limit's limit, the trailing-limit's stop level less its
//! offset for a sell, plus it for a buy). The book decides only the TRIGGER: whether the limit then
//! fills is the venue's, so an arm whose trigger the oracle crossed fires whether or not the bar reached
//! its limit (the oracle turns such an order into its `Limit` in place; that copy is dropped with the
//! arm). A trailing arm with an [`OrderResting::activation`] trails nothing until a price reaches the
//! level (a sell's from below, a buy's from above): the bar that reaches it seeds the extreme with its
//! best price and fires nothing, the conservative half of the simulator's path walk, which a bar's close
//! cannot replay.
//!
//! **The attributes wave's three forms** (decision 0267, held only as an operator's or a mount's
//! core-held reduce-only exit): a `TrailingTakeProfit` (the if-touched mirror of the trailing stop, a
//! market or, with an offset, a limit off its trigger), a trailing kind whose distance is a PERCENT of
//! its extreme ([`OrderResting::trail_percent`]), and a `Stop` or `StopLimit` with an ADJUSTMENT
//! ([`OrderResting::adjust`]) that turns it, in place and once, into its target. All three are the
//! oracle's laws (`vike_model::order_fill_price`); the book only holds them and reads their level.

use indexmap::IndexMap;
use vike_execution::emulated::OrderEmulationLaw;
use vike_marketdata::Bar;
use vike_model::{
    Fill, OrderEvent, OrderKind, OrderResting, OrderType, TriggerBy, one_price_bar,
    order_fill_price,
};
use vike_venues::OrderEmulationKind;

/// A fired conditional: submit through the gate as a MARKET, or as a LIMIT at [`Self::limit`].
#[derive(Debug, Clone, PartialEq)]
pub struct OrderEmulationFire {
    /// the runtime-minted id of the arm that fired — the journal's
    /// [`vike_journal::JournalRecord::OrderEmulationFire`] key, so a replayed fire is
    /// attributable to the arm it consumed (emulator-journal PR-1).
    pub arm_id: String,
    pub side: i32,
    pub qty: f64,
    /// the oracle's trigger fill price (diagnostic only — the live fill is the venue's); for a limit
    /// the bar did not reach, the bar's close (on a tick, the price the order came alive at)
    pub trigger_price: f64,
    /// The LIMIT this fire releases; `None` releases a market (a stop, a trailing stop, a
    /// market-if-touched take-profit).
    pub limit: Option<f64>,
    /// The arm as it rested, copied at the fire (a trailing arm's extreme as it stood). A core-held
    /// reduce-only arm that is held back, or re-armed after a refused close, is put back with exactly
    /// these terms (`trigger_price` is the fill price, which can be a gap's, not the arm's).
    pub armed: OrderEmulationArm,
}

/// The price LANE a book check is fed from — which price series produced the price under test.
/// [`TriggerBy`] names what an ARM requests; this names what a CHECK carries; the two meet in
/// [`OrderEmulationArm::evaluates_on`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderEmulationPriceLane {
    /// trade/bar prices — closed bars ([`ConditionalArms::check_bar`]) and the quote/trade/book
    /// tick path ([`ConditionalArms::check_price`]); today's only lane before `trigger_by`
    Last,
    /// the core's mark lane ([`ConditionalArms::check_mark`] — `Ingest::Market` mark ticks)
    Mark,
}

/// One resting arm: the oracle's [`OrderResting`] plus the requested trigger source.
/// `trigger_by` `None`/`Some(Last)` evaluates on the [`OrderEmulationPriceLane::Last`] lane (today's law,
/// byte-identical); `Some(Mark)` on the mark lane ONLY (no bar/tick ever fires it, and its
/// trailing extreme ratchets on mark prices only). `Some(Index)` matches NO lane — the core has
/// no index lane; the runtime refuses such an arm at apply time, and one smuggled in anyway
/// (e.g. a hand-edited journal) rests INERT rather than firing off the wrong series.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderEmulationArm {
    pub order: OrderResting,
    pub trigger_by: Option<TriggerBy>,
}

impl OrderEmulationArm {
    /// Does this arm evaluate on `lane`? The one place the request-source → lane law lives.
    fn evaluates_on(&self, lane: OrderEmulationPriceLane) -> bool {
        match self.trigger_by {
            None | Some(TriggerBy::Last) => lane == OrderEmulationPriceLane::Last,
            Some(TriggerBy::Mark) => lane == OrderEmulationPriceLane::Mark,
            Some(TriggerBy::Index) => false,
        }
    }

    /// `order` as an arm on `trigger_by`'s lane, or `None` when its terms make none this book can hold
    /// (a field its kind's law reads is missing, or a kind the book does not arm).
    pub fn of(order: OrderResting, trigger_by: Option<TriggerBy>) -> Option<Self> {
        let armed = Self { order, trigger_by };
        armed.is_holdable().then_some(armed)
    }

    /// The arm a journaled [`vike_journal::OrderEmulationRecord`] (an arm's resolved terms, or a `Snapshot`
    /// entry's) describes, or `None` when its terms make none this book can hold: its
    /// [`vike_execution::ConditionalKindTerms`] when it carries them, else today's legacy read (a trailing
    /// stop from `trail` and `extreme`, else a stop from `price`), byte for byte.
    pub fn from_record(r: &vike_journal::OrderEmulationRecord) -> Option<Self> {
        let order = match (&r.kind_terms, r.trail, r.extreme, r.price) {
            (Some(terms), ..) => terms.working_order(r.side, r.qty)?,
            (None, Some(trail), Some(extreme), _) => {
                let mut o = OrderResting::new(OrderKind::Trailing, r.side, r.qty);
                o.trail = Some(trail);
                o.extreme = Some(extreme);
                o
            }
            (None, _, _, Some(px)) => {
                let mut o = OrderResting::new(OrderKind::Stop, r.side, r.qty);
                o.price = Some(px);
                o
            }
            (None, ..) => return None,
        };
        Self::of(order, r.trigger_by)
    }

    /// The intent that arms this arm's order (an operator's core-held arm, a re-arm): the legacy
    /// `price`/`trail` for a plain stop or a trailing stop with no activation, else its
    /// [`vike_execution::ConditionalKindTerms`] with those two `None`, the intent twin of
    /// `vike_journal::OrderEmulationRecord::of_arm` (never a legacy field beside the terms).
    pub fn intent(&self, venue: String, symbol: String) -> vike_execution::ConditionalIntent {
        let o = &self.order;
        let kind_terms = vike_execution::ConditionalKindTerms::of(o);
        let legacy = kind_terms.is_none();
        vike_execution::ConditionalIntent {
            venue,
            symbol,
            side: o.side,
            qty: o.size,
            price: o.price.filter(|_| legacy),
            trail: o.trail.filter(|_| legacy),
            trigger_by: self.trigger_by,
            kind_terms: kind_terms.map(Box::new),
        }
    }

    /// This arm armed again for `qty` (a core-held arm whose close failed): the same terms, except that a
    /// trailing arm that had REACHED its activation (it has an extreme) trails again from the mark at once,
    /// and one that had not keeps its activation level and waits for it, never trailing early.
    pub fn rearmed(&self, qty: f64) -> Self {
        let mut again = self.clone();
        again.order.size = qty;
        if again.order.extreme.is_some() {
            again.order.extreme = None;
            again.order.activation = None;
        }
        again
    }

    /// The level this arm fires at now: its trigger, a trailing arm's stop (its extreme less the trail
    /// for a sell, plus it for a buy; a trailing take-profit's mirrors it), or the activation level a
    /// trailing arm still waits for. A percent trail is its distance at the extreme it reads.
    pub fn trigger_level(&self) -> f64 {
        let o = &self.order;
        match o.extreme {
            Some(x) if trails(o) => {
                let d = o.trail_distance(x);
                // a sell stop sits below its extreme, a sell trailing take-profit above its low
                if (o.side < 0) == (o.kind != OrderKind::TrailingTakeProfit) {
                    x - d
                } else {
                    x + d
                }
            }
            None if trails(o) => o.activation.unwrap_or(0.0),
            _ => o.price.unwrap_or(0.0),
        }
    }

    /// Whether the oracle can judge this order without a field it `expect`s: one of the six kinds,
    /// with its trigger (and a stop-limit's limit), or a trail (a price or a percent) with an extreme or
    /// an activation to seed one (and a trailing-limit's offset). The book never holds anything else,
    /// so the fold never panics.
    fn is_holdable(&self) -> bool {
        let o = &self.order;
        let trails = trails(o) && (o.extreme.is_some() || o.activation.is_some());
        match o.kind {
            OrderKind::Stop | OrderKind::TakeProfit => o.price.is_some(),
            OrderKind::StopLimit => o.price.is_some() && o.limit.is_some(),
            OrderKind::Trailing | OrderKind::TrailingTakeProfit => trails,
            OrderKind::TrailingLimit => trails && o.limit_offset.is_some(),
            _ => false,
        }
    }

    /// Whether `bar` fires this arm: the fire, with the arm's terms as it rested, or `None` (a trailing
    /// extreme ratcheted, or an activation reached, in place).
    #[inline]
    fn fire(&mut self, arm_id: &str, bar: &Bar) -> Option<OrderEmulationFire> {
        let (trigger_px, limit, armed) = match self.order.kind {
            // The oracle fires these where they rest and changes nothing it fires, so the arm is copied
            // only at a fire: today's stop and trailing stop, and a market-if-touched take-profit.
            OrderKind::Stop => (order_fill_price(&mut self.order, bar)?, None, self.clone()),
            OrderKind::Trailing if self.order.extreme.is_some() => {
                (order_fill_price(&mut self.order, bar)?, None, self.clone())
            }
            OrderKind::TakeProfit if self.order.limit.is_none() => {
                (order_fill_price(&mut self.order, bar)?, None, self.clone())
            }
            _ if self.order.extreme.is_none() && trails(&self.order) => {
                self.activate(bar);
                return None;
            }
            // A stop-limit, a limit-if-touched take-profit, a trailing-limit and a trailing take-profit (its
            // offset sets a limit at the fire): the oracle turns a fired one into its `Limit` in place when
            // the bar does not reach the limit, so a copy keeps the terms. (A stop-limit's adjustment, like a
            // stop's above, changes it in place only on an event that does not fire it.)
            _ => {
                let rested = self.clone();
                match order_fill_price(&mut self.order, bar) {
                    Some(px) => (px, self.order.limit, rested),
                    None if self.order.kind == OrderKind::Limit => {
                        (bar.close, self.order.price, rested)
                    }
                    None => return None,
                }
            }
        };
        Some(OrderEmulationFire {
            arm_id: arm_id.to_string(),
            side: armed.order.side,
            qty: armed.order.size,
            trigger_price: trigger_px,
            limit,
            armed,
        })
    }

    /// A trailing arm waiting for its activation level: seed the extreme with `bar`'s best price (a
    /// sell's high, a buy's low) when that reaches the level.
    #[cold]
    fn activate(&mut self, bar: &Bar) {
        let o = &mut self.order;
        let Some(level) = o.activation else { return };
        let (best, reached) =
            if o.side < 0 { (bar.high, bar.high >= level) } else { (bar.low, bar.low <= level) };
        if reached {
            o.extreme = Some(best);
        }
    }
}

/// Whether `o` trails: a price trail or a percent one ([`OrderResting::trail_distance`] reads either).
#[inline]
fn trails(o: &OrderResting) -> bool {
    o.trail.is_some() || o.trail_percent.is_some()
}

/// Resting conditional orders for one (venue, symbol), keyed by the runtime-minted `arm_id` —
/// uniqueness is STRUCTURAL (emulator PR-2). `IndexMap` (the repo's insertion-order convention)
/// rather than a `Vec`, because `arm_id` became the disarm key: a plain list let two arms carry
/// the same id (`add_*` blindly pushed) and `disarm` retain-dropped EVERY match — an ambiguous id
/// would disarm the wrong arm. The map makes a duplicate id unrepresentable; check/fire iteration
/// stays insertion order, byte-identical to the old `Vec` walk.
#[derive(Default)]
pub struct ConditionalArms {
    orders: IndexMap<String, OrderEmulationArm>,
    /// count of resting `Some(Mark)` arms — the mark-lane fast-path guard
    /// ([`Self::has_mark_arms`]): the mark lane is conflated-tick cadence, so the runtime must
    /// be able to skip the book walk with one integer compare when nobody asked for Mark.
    mark_arms: usize,
}

impl ConditionalArms {
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

    /// Walk every resting arm in INSERTION (= fire) order — the `Snapshot` capture surface
    /// (emulator PR-3): `write_snap` reads each arm's CURRENT terms (a trailing arm's ratcheted
    /// extreme included, its `trigger_by` too) into the journal's `Snapshot.conditionals`, so a
    /// restart can re-arm the book exactly as it rested. Read-only; snapshot cadence, never the
    /// per-tick fold.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &OrderEmulationArm)> {
        self.orders.iter().map(|(id, a)| (id.as_str(), a))
    }

    /// Insert one armed order under `arm_id`, REFUSING a duplicate id: `true` = armed, `false` =
    /// an arm with this id already rests and the book is UNCHANGED (the existing arm is never
    /// silently replaced — `arm_id` is the disarm key, so it must name exactly one arm). Live ids
    /// are minted by a monotone counter and cannot collide; this is the structural backstop.
    fn insert_unique(
        &mut self,
        arm_id: String,
        order: OrderResting,
        trigger_by: Option<TriggerBy>,
    ) -> bool {
        match self.orders.entry(arm_id) {
            indexmap::map::Entry::Occupied(_) => false,
            indexmap::map::Entry::Vacant(v) => {
                if trigger_by == Some(TriggerBy::Mark) {
                    self.mark_arms += 1;
                }
                v.insert(OrderEmulationArm { order, trigger_by });
                true
            }
        }
    }

    /// Arm `armed` under `arm_id`: any of the six kinds, built by [`OrderEmulationArm::from_record`] or
    /// carried by a [`OrderEmulationFire`] that is put back. Returns `false` (book unchanged) on a
    /// duplicate id — see [`Self::insert_unique`] — and on terms the oracle could not judge.
    #[must_use]
    pub fn add(&mut self, arm_id: impl Into<String>, armed: OrderEmulationArm) -> bool {
        armed.is_holdable() && self.insert_unique(arm_id.into(), armed.order, armed.trigger_by)
    }

    /// Arm a fixed stop: fires when the bar crosses `price` (gap-open adverse fill law) on the
    /// lane its `trigger_by` names (see [`OrderEmulationArm`]; `None` = Last, today's law).
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
        let mut o = OrderResting::new(OrderKind::Stop, side, qty);
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
        let mut o = OrderResting::new(OrderKind::Trailing, side, qty);
        o.trail = Some(trail);
        o.extreme = Some(extreme);
        self.insert_unique(arm_id.into(), o, trigger_by)
    }

    /// Drop THE one arm named by its minted id; `true` when it was present. The individual-cancel
    /// primitive behind [`vike_execution::OrderIntent::DisarmConditional`] (`MassCancel` stays the
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
    fn check_lane(&mut self, bar: &Bar, lane: OrderEmulationPriceLane) -> Vec<OrderEmulationFire> {
        let mut fired = Vec::new();
        let mark_arms = &mut self.mark_arms;
        self.orders.retain(|arm_id, armed| {
            if !armed.evaluates_on(lane) {
                return true;
            }
            let Some(f) = armed.fire(arm_id, bar) else {
                return true;
            };
            if armed.trigger_by == Some(TriggerBy::Mark) {
                *mark_arms -= 1;
            }
            fired.push(f);
            false
        });
        fired
    }

    /// Check every Last-lane conditional against a closed bar — today's law, byte-identical for
    /// every arm without a requested source. Mark arms never fire here (see [`Self::check_mark`]).
    pub fn check_bar(&mut self, bar: &Bar) -> Vec<OrderEmulationFire> {
        self.check_lane(bar, OrderEmulationPriceLane::Last)
    }

    /// Tick-price check (Rust-native upgrade): a degenerate OHLC bar at `px` runs the same
    /// oracle predicate — a Last-lane stop crossed by this tick fires now, not at bar close.
    pub fn check_price(&mut self, px: f64, ts: i64) -> Vec<OrderEmulationFire> {
        self.check_lane(&one_price_bar(ts, px), OrderEmulationPriceLane::Last)
    }

    /// MARK-lane twin of [`Self::check_price`]: the same degenerate-bar oracle predicate, fed a
    /// mark tick, evaluating ONLY the `Some(Mark)` arms (their trailing extremes ratchet here
    /// too). The runtime calls this off the conflated mark drain, guarded by
    /// [`Self::has_mark_arms`].
    pub fn check_mark(&mut self, px: f64, ts: i64) -> Vec<OrderEmulationFire> {
        self.check_lane(&one_price_bar(ts, px), OrderEmulationPriceLane::Mark)
    }

    /// Drop every resting conditional (`cancel_all` clears the book — oracle semantics).
    pub fn clear(&mut self) {
        self.orders.clear();
        self.mark_arms = 0;
    }
}

/// What [`OrderEmulationBook::cond_engine`] remembers about one armed conditional.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OrderEmulationArmRoute {
    /// The routing index of the engine the arm will fire onto.
    pub(crate) engine: usize,
    /// The mount whose strategy armed it; `None` for an operator's or any non-mount arm.
    pub(crate) mount: Option<usize>,
    /// An operator's CORE-HELD reduce-only stop (`CoreThread::arm_conditional_as`): its fire is capped
    /// to the live position of `engine` and sent `reduce_only`, or dropped when that position is gone
    /// (`CoreThread::submit_fired`). It lives HERE and not on `OrderEmulationArm` because the book is
    /// the latency-gated crate's hot structure and the fire's checks (`check_lane`) stay untouched.
    /// ⚠ Restore must re-insert it (`CoreThread::restore_arm_routes`): a single-account box has no
    /// route key and no mount to resolve an engine from, so without that insert the flag would be lost.
    pub(crate) reduce_only: bool,
    /// Core-held stops only: the node has SEEN a position on the side this stop reduces (at the arm, or at
    /// a sweep). A stop with `covered` set whose position later goes flat or flips is CLEARED by
    /// `CoreThread::sweep_core_held_stops`; one armed on a flat book that no sweep has found a position
    /// for is left alone (an operator may arm the stop before the entry fills).
    pub(crate) covered: bool,
    /// Core-held stops only: how many times this stop was RE-ARMED after its close was refused or only
    /// partly filled (`0` = as the operator armed it). Not journaled: a restart counts again from zero.
    pub(crate) rearms: u8,
}

/// What a child told the book since the last pass, fed to the law at the next one.
pub(crate) enum OrderEmulationChildReport {
    Fill(Fill),
    Event(OrderEvent),
}

/// One WORKING emulated order (a chase, an iceberg): its request as accepted plus the law's own working state
/// (the slot of its live child, its progress) and what the book keeps beside it. No price, size or side is
/// copied out of the request. A TRIGGER kind's order is its [`OrderEmulationArm`] (a `OrderResting` and its
/// trigger source), kept by kind in [`ConditionalArms`]; both sit in [`OrderEmulationBook`].
pub(crate) struct OrderEmulation {
    /// The engine it works on, by route key.
    pub(crate) route_key: String,
    /// The owning mount's id; `None` for an operator's order.
    pub(crate) mount_id: Option<String>,
    /// The parent as accepted.
    pub(crate) request: vike_model::OrderRequest,
    /// `None` only while a pass has it out.
    pub(crate) law: Option<OrderEmulationLaw>,
    /// The live child; `None` between two children.
    pub(crate) child: Option<String>,
    /// Every child still mapped to this parent, the live one included.
    pub(crate) children: Vec<String>,
    pub(crate) filled: f64,
    pub(crate) average_price: f64,
    /// The live child's size as placed and how much of it the book counted filled: its completion.
    pub(crate) child_qty: f64,
    pub(crate) child_filled: f64,
    /// Heard since the last pass (a live core only).
    pub(crate) heard: Vec<OrderEmulationChildReport>,
    /// The newest market stamp the law was handed.
    pub(crate) seen_ms: i64,
    /// Why it places nothing now, in words.
    pub(crate) waiting: Option<String>,
    /// Restored with a live child the venue has not confirmed yet.
    pub(crate) unconfirmed: bool,
}

/// **Every EMULATED order the core holds, in ONE entity** (decision 0275): the TRIGGER kinds that fire
/// once (stop, trailing, take-profit, stop-limit, trailing-limit and the attribute forms, in their
/// per-`(venue, symbol)` [`ConditionalArms`]s, keyed by arm id) and the WORKING kinds that keep one child
/// resting (a chase, an iceberg, keyed by the parent's order client id). One id space, one cancel, one end of scope,
/// one list (`CoreSnapshot::emulated_orders`). The per-kind law stays per kind: the trigger books are
/// checked on the fold as they always were (`crate::emulator`'s oracle, `CoreThread::tick_conditionals`),
/// the working laws at the drain-loop boundary. The trigger journal records and `Snapshot.conditionals` are
/// unchanged byte for byte; the working steps ADD `Command::Emulated` and `Snapshot.emulated` to the family.
#[derive(Default)]
pub(crate) struct OrderEmulationBook {
    /// Phase C/D: emulated conditional orders per (venue, symbol) — checked per closed
    /// bar BEFORE `on_bar` (oracle firing order), optionally per tick.
    pub(crate) order_emulation_arms: IndexMap<(String, String), ConditionalArms>,
    /// **arm id → who armed it, written when the conditional is ARMED** — `CoreThread::order_client_id_venue`'s
    /// twin for the one order the core mints with no request behind it. The value is an
    /// [`OrderEmulationArmRoute`]: the routing index (`engine`) AND the mount that armed it (`mount`), so one map
    /// carries both and has one bound.
    ///
    /// `conditional_books` is keyed by `(venue, symbol)`, which is a fact about the EXCHANGE, so
    /// two accounts of one venue arm into ONE book and a fired arm carries no account. The FIRE
    /// (`CoreThread::submit_fired`) used to lower through `apply_intent`, i.e. by the payload's
    /// venue, which resolves the venue's DEFAULT engine: a labelled mount's `LiveBroker::submit_stop`
    /// armed against its own book and, on trigger, sold into somebody else's — the protective exit
    /// silently opening a naked position on the default account while the position it was armed to
    /// close stayed open. The arm's engine is known at ARM time (`apply/conditional.rs`'s
    /// `arm_conditional` already resolves it to read the seeding mark), so it is recorded here
    /// and spent at the fire.
    ///
    /// Bounded by the book: an entry is written when an arm enters a `ConditionalArms` and removed
    /// when it leaves one (fired, disarmed, or cleared by a mass-cancel).
    ///
    /// Byte-identical on every single-account core, and NOT because it is skipped there: the
    /// recorded index is `route_of(route, venue)`, which on a core with one engine per venue IS
    /// what the fire's payload lookup resolved.
    ///
    /// The MOUNT is what keeps the fired order a strategy's own: `submit_fired` writes it into
    /// `CoreThread::order_client_id_mount` beside the minted order client id, so the stop's fill books into the arming mount's
    /// ledger and reaches that mount's `on_fill`, rather than the first mount on the pair. `None`
    /// for an operator's arm.
    pub(crate) cond_engine: std::collections::HashMap<String, OrderEmulationArmRoute>,
    /// The WORKING parents by order client id, in accepted order.
    pub(crate) working: indexmap::IndexMap<String, OrderEmulation>,
    /// Every working child back to its parent.
    pub(crate) child_of: std::collections::HashMap<String, String>,
}

impl OrderEmulationBook {
    /// The entity a restart hands back: the trigger books as `assemble_core` rebuilt them
    /// (`restore_conditional_books`, unchanged) and the working parents a `Snapshot` carried, each law rebuilt
    /// from its saved state (`load_state`, which marks its slot restored). Cold: once per boot.
    pub(crate) fn restore(
        conditionals: IndexMap<(String, String), ConditionalArms>,
        saved: Vec<vike_journal::JournalSnapshotOrderEmulation>,
    ) -> Self {
        let mut book = Self { order_emulation_arms: conditionals, ..Self::default() };
        for s in saved {
            let route = if s.request.order_type == OrderType::Pegged {
                OrderEmulationKind::Chase
            } else {
                OrderEmulationKind::Iceberg
            };
            // A child the book knows of is awaited with no deadline; with none, the slot gives up at
            // its first hook (a no-op cancel: the book names no child) and the next one places.
            let deadline = if s.child.is_some() { i64::MAX } else { 0 };
            let mut law = OrderEmulationLaw::of(&s.request, route, deadline);
            law.hooks::<crate::runtime::LiveBroker>().load_state(&s.law);
            let order_client_id = s.request.order_client_id.clone();
            for c in &s.children {
                book.child_of.insert(c.clone(), order_client_id.clone());
            }
            let waiting = s.child.as_ref().map(|c| {
                format!("waiting for the venue to confirm child {c} (a restart never cancels it)")
            });
            book.working.insert(
                order_client_id,
                OrderEmulation {
                    route_key: s.route_key,
                    mount_id: s.mount_id,
                    request: s.request,
                    law: Some(law),
                    unconfirmed: s.child.is_some(),
                    child: s.child,
                    children: s.children,
                    filled: s.filled,
                    average_price: s.average_price,
                    child_qty: s.child_qty,
                    child_filled: s.child_filled,
                    heard: Vec::new(),
                    seen_ms: i64::MIN,
                    waiting,
                },
            );
        }
        book
    }
}

#[path = "emulator_tests.rs"]
#[cfg(test)]
mod emulator_tests;
