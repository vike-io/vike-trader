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
    /// [`crate::journal::JournalRecord::ConditionalFire`] key, so a replayed fire is
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

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(open: f64, high: f64, low: f64, close: f64) -> Bar {
        Bar {
            ts: 0,
            open,
            high,
            low,
            close,
            volume: 0.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        }
    }

    #[test]
    fn stop_fires_on_cross_and_leaves_book() {
        let mut book = ConditionalBook::new();
        assert!(book.add_stop("a1", -1, 2.0, 95.0, None)); // protect a long: sell-stop below
        assert!(book.check_bar(&bar(100.0, 101.0, 99.0, 100.5)).is_empty());
        assert_eq!(book.len(), 1);
        let fired = book.check_bar(&bar(97.0, 98.0, 94.0, 94.5));
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].side, -1);
        assert_eq!(fired[0].qty, 2.0);
        assert!(book.is_empty());
    }

    #[test]
    fn gap_open_fills_adverse() {
        let mut book = ConditionalBook::new();
        assert!(book.add_stop("a1", -1, 1.0, 95.0, None));
        // gaps DOWN through the stop: fill at the (worse) open, not the trigger
        let fired = book.check_bar(&bar(92.0, 93.0, 91.0, 92.5));
        assert_eq!(fired[0].trigger_px, 92.0);
    }

    #[test]
    fn trailing_ratchets_in_place_then_fires() {
        let mut book = ConditionalBook::new();
        assert!(book.add_trailing("a1", -1, 1.0, 5.0, 100.0, None)); // long protection, extreme 100 -> trigger 95
        // new high 110 ratchets the extreme; low 101 stays above the OLD trigger 95
        assert!(book.check_bar(&bar(105.0, 110.0, 101.0, 108.0)).is_empty());
        // trigger is now 105: a dip to 104 fires
        let fired = book.check_bar(&bar(106.0, 107.0, 104.0, 104.5));
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].trigger_px, 105.0);
    }

    #[test]
    fn new_high_bar_cannot_stop_itself_out() {
        // the oracle checks the PRIOR extreme's trigger before ratcheting (orders.rs law)
        let mut book = ConditionalBook::new();
        assert!(book.add_trailing("a1", -1, 1.0, 5.0, 100.0, None));
        // this bar's high 120 would imply trigger 115 — but its own low 103 must be
        // compared against the PRIOR trigger 95: no fire
        assert!(book.check_bar(&bar(110.0, 120.0, 103.0, 118.0)).is_empty());
    }

    #[test]
    fn disarm_removes_one_arm_by_id_and_tolerates_unknown() {
        let mut book = ConditionalBook::new();
        assert!(book.add_stop("a1", -1, 1.0, 95.0, None));
        assert!(book.add_stop("a2", -1, 1.0, 90.0, None));
        assert!(book.contains("a1") && book.contains("a2"));
        assert!(!book.disarm("nope"), "unknown id is a no-op, not a panic");
        assert_eq!(book.len(), 2);
        assert!(book.disarm("a1"));
        assert!(!book.contains("a1"));
        assert_eq!(book.len(), 1);
        // the survivor is a2: only the 90 stop is left, so a dip to 94 no longer fires
        assert!(book.check_price(94.0, 1).is_empty());
        let fired = book.check_price(89.0, 2);
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].arm_id, "a2");
    }

    /// Emulator PR-2 (structural uniqueness): a duplicate arm id is REFUSED at add — the resting
    /// arm keeps its terms, the book length is unchanged, and `disarm` therefore targets exactly
    /// one arm by construction (the pre-PR-2 `Vec` book blindly pushed both and `disarm`
    /// retain-dropped every match).
    #[test]
    fn duplicate_arm_id_is_refused_and_never_replaces_the_resting_arm() {
        let mut book = ConditionalBook::new();
        assert!(book.add_stop("a1", -1, 1.0, 95.0, None));
        assert!(!book.add_stop("a1", -1, 3.0, 80.0, None), "same id again: refused");
        assert!(!book.add_trailing("a1", -1, 1.0, 5.0, 100.0, None), "refused across kinds too");
        assert_eq!(book.len(), 1, "the book still holds exactly the FIRST arm");
        // the resting arm's ORIGINAL terms survive: 94 crosses the 95 stop (qty 1), which the
        // refused 80-stop replacement would not have
        let fired = book.check_price(94.0, 1);
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].qty, 1.0, "the original arm's terms, not the duplicate's");
        // and the fire consumed THE one arm — nothing lingers under the refused duplicate id
        assert!(book.is_empty());
        assert!(!book.disarm("a1"), "no ghost arm left behind for the id");
    }

    /// Fire order across a multi-arm book is the book's INSERTION order, and a disarm in the
    /// middle does not reorder the survivors (`shift_remove`, never `swap_remove`).
    #[test]
    fn fire_order_is_insertion_order_and_survives_a_disarm() {
        let mut book = ConditionalBook::new();
        assert!(book.add_stop("first", -1, 1.0, 95.0, None));
        assert!(book.add_stop("second", -1, 1.0, 96.0, None));
        assert!(book.add_stop("third", -1, 1.0, 97.0, None));
        assert!(book.disarm("second"));
        // one bar crosses ALL survivors: they fire in insertion order (first, third)
        let fired = book.check_bar(&bar(100.0, 100.0, 90.0, 92.0));
        let ids: Vec<&str> = fired.iter().map(|f| f.arm_id.as_str()).collect();
        assert_eq!(ids, vec!["first", "third"]);
    }

    #[test]
    fn fired_conditional_carries_its_arm_id() {
        let mut book = ConditionalBook::new();
        assert!(book.add_stop("armed-7", -1, 1.0, 95.0, None));
        let fired = book.check_price(94.0, 1);
        assert_eq!(fired[0].arm_id, "armed-7");
    }

    #[test]
    fn tick_check_uses_degenerate_bar() {
        let mut book = ConditionalBook::new();
        assert!(book.add_stop("a1", -1, 1.0, 95.0, None));
        assert!(book.check_price(96.0, 1).is_empty());
        let fired = book.check_price(94.0, 2);
        assert_eq!(fired.len(), 1);
    }

    #[test]
    fn clear_empties_the_book() {
        let mut book = ConditionalBook::new();
        assert!(book.add_stop("a1", 1, 1.0, 100.0, None));
        assert!(book.add_trailing("a2", -1, 1.0, 5.0, 100.0, None));
        book.clear();
        assert!(book.is_empty());
    }

    // ---- trigger-source lanes (w2 trigger_by) ----

    /// A Mark arm never fires off the Last lane (bars/ticks) — its price series simply did not
    /// tick there — and DOES fire off the mark lane; a Last (and a None) arm is the mirror image.
    #[test]
    fn mark_arm_fires_only_on_the_mark_lane_and_last_only_on_last() {
        let mut book = ConditionalBook::new();
        assert!(book.add_stop("m1", -1, 1.0, 95.0, Some(TriggerBy::Mark)));
        assert!(book.add_stop("l1", -1, 1.0, 95.0, Some(TriggerBy::Last)));
        assert!(book.add_stop("d1", -1, 1.0, 95.0, None)); // None = today's Last law
        assert!(book.has_mark_arms());

        // a crossing LAST price (bar and tick) fires l1+d1 but leaves m1 resting
        let fired = book.check_bar(&bar(94.0, 94.0, 94.0, 94.0));
        let ids: Vec<&str> = fired.iter().map(|f| f.arm_id.as_str()).collect();
        assert_eq!(ids, vec!["l1", "d1"], "insertion order, Mark arm skipped");
        assert!(book.contains("m1"), "the Mark arm rests through a crossing LAST price");
        assert!(book.has_mark_arms());

        // a crossing MARK price fires m1 (last=99 mark=94 SL=95: the HL-timing scenario)
        let fired = book.check_mark(94.0, 2);
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].arm_id, "m1");
        assert_eq!(fired[0].trigger_px, 94.0);
        assert!(book.is_empty());
        assert!(!book.has_mark_arms(), "the fire decremented the mark-arm count");
    }

    /// The reverse guard: a crossing mark price never fires a Last/None arm.
    #[test]
    fn last_arms_never_fire_on_the_mark_lane() {
        let mut book = ConditionalBook::new();
        assert!(book.add_stop("l1", -1, 1.0, 95.0, None));
        assert!(!book.has_mark_arms());
        assert!(book.check_mark(90.0, 1).is_empty(), "mark lane skips Last arms");
        assert!(book.contains("l1"));
        assert_eq!(book.check_price(94.0, 2).len(), 1, "…the Last lane still fires it");
    }

    /// A Mark TRAILING arm ratchets its extreme on mark prices ONLY: a higher Last print must
    /// neither ratchet nor fire it.
    #[test]
    fn mark_trailing_ratchets_only_on_the_mark_lane() {
        let mut book = ConditionalBook::new();
        assert!(book.add_trailing("mt", -1, 1.0, 5.0, 100.0, Some(TriggerBy::Mark)));
        // a LAST spike to 120 would ratchet the trigger to 115 — but this arm ignores Last
        assert!(book.check_bar(&bar(110.0, 120.0, 103.0, 118.0)).is_empty());
        // mark ratchets to 110 (trigger 105) without firing…
        assert!(book.check_mark(110.0, 1).is_empty());
        // …then a mark touch of exactly 105 fires at the RATCHETED trigger 105 — proof the mark
        // spike moved the extreme (an UN-ratcheted trigger of 95 would not fire on a 105 touch),
        // and the Last spike to 120 never did (a 115 trigger would have fired at 110 already)
        let fired = book.check_mark(105.0, 2);
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].trigger_px, 105.0, "extreme ratcheted by mark, not by last");
    }

    /// An Index arm matches NO lane (the core has no index lane): inert on both, never a
    /// misfire off the wrong series. The runtime refuses such arms at apply time; this is the
    /// structural backstop.
    #[test]
    fn index_arm_is_inert_on_both_lanes() {
        let mut book = ConditionalBook::new();
        assert!(book.add_stop("i1", -1, 1.0, 95.0, Some(TriggerBy::Index)));
        assert!(!book.has_mark_arms());
        assert!(book.check_bar(&bar(90.0, 90.0, 90.0, 90.0)).is_empty());
        assert!(book.check_price(90.0, 1).is_empty());
        assert!(book.check_mark(90.0, 2).is_empty());
        assert!(book.contains("i1"), "rests inert — disarmable, never misfired");
    }

    /// The mark-arm count survives disarms and clear (the runtime's fast-path guard must never
    /// go stale in either direction).
    #[test]
    fn mark_arm_count_tracks_disarm_and_clear() {
        let mut book = ConditionalBook::new();
        assert!(book.add_stop("m1", -1, 1.0, 95.0, Some(TriggerBy::Mark)));
        assert!(book.add_stop("m2", -1, 1.0, 90.0, Some(TriggerBy::Mark)));
        assert!(book.add_stop("l1", -1, 1.0, 85.0, None));
        assert!(book.has_mark_arms());
        assert!(book.disarm("m1"));
        assert!(book.has_mark_arms(), "one Mark arm still rests");
        assert!(book.disarm("m2"));
        assert!(!book.has_mark_arms(), "no Mark arms left — the guard must clear");
        assert!(book.add_stop("m3", -1, 1.0, 80.0, Some(TriggerBy::Mark)));
        book.clear();
        assert!(!book.has_mark_arms());
        assert!(book.is_empty());
    }

    /// `iter` exposes each arm's trigger source (the Snap capture surface carries it through a
    /// restart).
    #[test]
    fn iter_carries_the_trigger_source() {
        let mut book = ConditionalBook::new();
        assert!(book.add_stop("m1", -1, 1.0, 95.0, Some(TriggerBy::Mark)));
        assert!(book.add_stop("l1", -1, 1.0, 90.0, None));
        let got: Vec<(&str, Option<TriggerBy>)> =
            book.iter().map(|(id, a)| (id, a.trigger_by)).collect();
        assert_eq!(got, vec![("m1", Some(TriggerBy::Mark)), ("l1", None)]);
    }
}
