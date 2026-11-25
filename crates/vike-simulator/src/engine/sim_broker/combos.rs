//! Combos and option exercise in the backtest: [`OrderBrokerSimulator::submit_combo`] with its resting book and
//! its pass ([`OrderBrokerSimulator::fill_combos`]), and [`OrderBrokerSimulator::exercise`].
//!
//! - **A combo** ([`vike_model::OrderCombo`]: legs on the run's symbols, a signed ratio each) is ONE
//!   order that fills every leg or none, the paper book's law (`vike-paper`'s `fill_combos`), on the
//!   first event at which every leg carries a FRESH mark: the leg's last event (its bar of the step,
//!   its quote or trade), no older than [`OrderBrokerSimulator::combo_mark_staleness_ms`] at the event's ts (the
//!   paper book's `combo_mark_staleness_ms`, default 0: every leg priced at the same ts). A leg with
//!   no fresh mark waits, and the whole combo with it. A leg the combo BUYS (`side × ratio > 0`)
//!   trades at its ask and one it sells at its bid, each at its close where the event carries no
//!   quote (the plan's assumption; LEAN takes a bar's low or high, more optimistic still).
//!   - A combo MARKET fills at those prices. One with a NET limit fills there when their net crosses
//!     it ([`vike_model::combo_net_cross`], before slippage, as the paper book). One priced by its
//!     LEGS fills when every leg crosses its own limit on the same event
//!     ([`vike_model::combo_legs_cross`], the one law the paper book shares), each leg at its own
//!     crossing price.
//!   - Each leg books through [`OrderBrokerSimulator::apply_fill`] (slippage, the fixed amount, the grid, the
//!     fee): a combo with a limit as a maker its limit bounds, a market one as a taker. Every leg's
//!     terms are judged before any leg books ([`OrderBrokerSimulator::fill_terms`]): a leg the grid refuses or
//!     resizes ENDS the combo, in words, in [`OrderBrokerSimulator::dropped`], with no leg filled.
//!   - Intake, whole or nothing (decision 0219's shape), in the model's words where the model has
//!     them: the spec's own rules ([`vike_model::OrderCombo::validate`]: among them each instrument
//!     named by one leg) and the books' lifetime rule, GTC only
//!     ([`vike_model::OrderCombo::book_lifetime_refusal`], the paper book's too); then no release,
//!     legs that are unsettled symbols of the run, the mount leverage cap on the combo as ONE unit, all
//!     its legs or none (`vike_execution::risk::judge_mount_leverage`, as live's drain judges it), and
//!     the pre-trade gate on each leg (`|ratio| × qty` at the leg's side, judged alone). The venue and
//!     account the spec names are the live door's to judge: this run is one account.
//!   - A resting combo commits each leg in the gate's folds (`OrderBrokerSimulator::committed_order_quantities`),
//!     [`OrderBrokerSimulator::cancel_all`] on any leg's symbol pulls it, and a leg that settles ends it.
//!   - It is NOT judged by the volume cap, the open-position caps or the active mask, which judge one
//!     symbol's order, nor by the paper book either; it is refused under the order-latency gate and
//!     under [`StrategyEngineConfig::shortfall_cover`], whose laws are one symbol's.
//! - **An exercise** of a held option ([`OrderBrokerSimulator::exercise`]) settles in CASH at its intrinsic
//!   value, the law its expiry settles by (the owner's ruling, 2026-11-09: there is no delivery of
//!   the underlying). Only an AMERICAN option ([`vike_model::ContractOption`]'s `style`, from
//!   [`StrategyEngineConfig::option_specs`]) whose underlying is a symbol of the run, and at most the long
//!   position; everything else is refused in words.

use vike_execution::risk::MountLeverageLeg;
use vike_marketdata::Bar;
use vike_model::{
    ContractOptionExerciseStyle, ExerciseSpec, Fill, OrderCombo, OrderComboLeg, OrderDrop,
    OrderFees, OrderType,
};

#[cfg(doc)]
use super::params::StrategyEngineConfig;
use super::state::{OrderBrokerSimulator, SettlementFill};

/// The combos of one run: those resting, and each symbol's last mark `(ts, bid, ask)` (a bar or tick
/// with no quote gives its close for both), indexed like `symbols`. The marks are kept only while a
/// combo rests and dropped when the book empties, so a combo never prices off a mark older than its
/// own life. Empty for every run that places none.
#[derive(Default)]
pub(crate) struct Combos {
    resting: Vec<OrderComboResting>,
    marks: Vec<Option<(i64, f64, f64)>>,
}

/// A resting combo and the symbol index of each leg, in leg order.
struct OrderComboResting {
    spec: OrderCombo,
    sis: Vec<usize>,
}

impl Combos {
    fn note(&mut self, n: usize, si: usize, b: &Bar) {
        if self.marks.len() != n {
            self.marks = vec![None; n];
        }
        self.marks[si] = Some((b.timestamp, b.bid.unwrap_or(b.close), b.ask.unwrap_or(b.close)));
    }

    fn forget_if_idle(&mut self) {
        if self.resting.is_empty() {
            self.marks = Vec::new();
        }
    }
}

/// A leg's side and size: selling the combo flips every leg, and a leg trades `|ratio| × qty`.
fn leg_terms(spec: &OrderCombo, leg: &OrderComboLeg) -> (i32, f64) {
    (spec.side * leg.ratio.signum(), f64::from(leg.ratio.unsigned_abs()) * spec.qty)
}

/// Whether the combo is priced by its legs ([`OrderComboLeg::limit`], every leg or none).
fn by_legs(spec: &OrderCombo) -> bool {
    spec.legs.iter().any(|l| l.limit.is_some())
}

/// Whether the combo carries a limit of either kind: its legs then book as makers the limit bounds.
fn is_priced(spec: &OrderCombo) -> bool {
    spec.net_limit.is_some() || by_legs(spec)
}

impl OrderBrokerSimulator {
    /// A COMBO: `spec`'s legs, filled all or none by this module's law. Returns whether it rests;
    /// refused whole, in words, in [`Self::dropped`] (under its first leg's symbol), otherwise. Its
    /// size is the strategy's own (no sizer).
    pub fn submit_combo(&mut self, spec: &OrderCombo) -> bool {
        match self.judge_combo(spec) {
            Ok(sis) => {
                self.combos.resting.push(OrderComboResting { spec: spec.clone(), sis });
                true
            }
            Err(why) => {
                if let Some(why) = why {
                    let symbol_name =
                        spec.legs.first().map_or_else(String::new, |l| l.symbol.clone());
                    self.dropped.push(OrderDrop {
                        symbol: symbol_name,
                        reason: why,
                        size: spec.qty,
                        weight: 0.0,
                    });
                }
                false
            }
        }
    }

    /// [`Self::submit_combo`]'s intake: each leg's symbol index, or the refusal (`None` when the
    /// pre-trade gate refused a leg and already said so in its words).
    fn judge_combo(&mut self, spec: &OrderCombo) -> Result<Vec<usize>, Option<String>> {
        spec.validate().map_err(|e| Some(e.to_string()))?;
        if let Some(e) = spec.book_lifetime_refusal() {
            return Err(Some(e.to_string()));
        }
        let knob = if self.releasing.is_some() {
            Some("a combo takes no release")
        } else if self.latency.is_some() {
            Some("a combo has no law under the order-latency model")
        } else if self.shortfall_cover.is_some() {
            Some("a combo has no law under the shortfall cover, which judges one symbol's fill")
        } else {
            None
        };
        if let Some(why) = knob {
            return Err(Some(why.to_string()));
        }
        let mut sis: Vec<usize> = Vec::with_capacity(spec.legs.len());
        for leg in &spec.legs {
            let symbol = &leg.symbol;
            let Some(si) = self.symbols.iter().position(|s| s == symbol) else {
                return Err(Some(format!("combo leg {symbol} is not a symbol of this run")));
            };
            if self.is_resolved_index(si) {
                return Err(Some(format!("combo leg {symbol} has settled")));
            }
            sis.push(si);
        }
        // The mount leverage cap judges the combo as ONE unit, all its legs or none, each on its own
        // slot, as live's drain does; `gate_order` then leaves the cap out for its legs.
        let legs: Vec<[MountLeverageLeg; 1]> = spec
            .legs
            .iter()
            .map(|leg| {
                let (side, quantity) = leg_terms(spec, leg);
                [MountLeverageLeg { side, quantity, price: leg.limit, ..Default::default() }]
            })
            .collect();
        let order: Vec<(usize, &[MountLeverageLeg])> =
            sis.iter().zip(&legs).map(|(&si, l)| (si, &l[..])).collect();
        if let Some(why) = self.leverage_refusal(&order) {
            return Err(Some(why));
        }
        self.leverage_judged = true;
        let gated = self.gate_combo_legs(spec, &sis);
        self.leverage_judged = false;
        gated.map(|()| sis)
    }

    /// [`Self::judge_combo`]'s pre-trade gate on each leg, the mount leverage cap left out.
    fn gate_combo_legs(&mut self, spec: &OrderCombo, sis: &[usize]) -> Result<(), Option<String>> {
        // shortcut: each leg is judged alone, its siblings' margin unseen; upgrade when the live
        // gate judges a combo as one order.
        for (leg, &si) in spec.legs.iter().zip(sis) {
            let (side, qty) = leg_terms(spec, leg);
            let ty = if leg.limit.is_some() { OrderType::Limit } else { OrderType::Market };
            let size = self.gate_order(si, side, qty, 0.0, ty, leg.limit, None);
            if size <= 0.0 {
                return Err(None);
            }
            if size < qty {
                return Err(Some(format!(
                    "a combo fills all its legs or none, and the leverage clamp takes only {size} \
                     of {qty} {}",
                    leg.symbol
                )));
            }
        }
        Ok(())
    }

    /// Note this STEP's bar of every symbol as its mark (the bar lane, before
    /// [`Self::fill_combos`]). One `is_empty` while no combo rests.
    pub(crate) fn note_combo_step(&mut self, i: usize) {
        if self.combos.resting.is_empty() {
            return;
        }
        let n = self.symbols.len();
        for si in 0..n {
            self.combos.note(n, si, &self.bars[si][i]);
        }
    }

    /// Note `event`, `symbols[si]`'s tick, as its mark (the tick lane, before [`Self::fill_combos`]).
    /// One `is_empty` while no combo rests.
    pub(crate) fn note_combo_mark(&mut self, si: usize, event: &Bar) {
        if !self.combos.resting.is_empty() {
            self.combos.note(self.symbols.len(), si, event);
        }
    }

    /// The combo pass at `self.now`, after the event's other fills: every resting combo whose legs
    /// all carry a fresh mark and whose limit, if any, crosses fills whole, and the fills are
    /// returned in leg order for the engine to hand `on_fill`; one that cannot fill ends in words.
    /// Nothing while no combo rests.
    pub(crate) fn fill_combos(&mut self) -> Vec<Fill> {
        if self.combos.resting.is_empty() {
            return Vec::new();
        }
        let ts = self.now;
        let mut fills = Vec::new();
        let mut still = Vec::new();
        for c in std::mem::take(&mut self.combos.resting) {
            match self.cross_combo(&c, ts) {
                Ok(None) => still.push(c),
                Ok(Some(prices)) => {
                    let priced = is_priced(&c.spec);
                    for ((leg, &si), &px) in c.spec.legs.iter().zip(&c.sis).zip(&prices) {
                        let (side, qty) = leg_terms(&c.spec, leg);
                        // `cross_combo` judged these terms: every leg books
                        fills.extend(self.apply_fill(si, side, qty, px, ts, priced, priced));
                    }
                }
                Err(why) => {
                    let symbol_name = c.spec.legs[0].symbol.clone();
                    self.dropped.push(OrderDrop {
                        symbol: symbol_name,
                        reason: why,
                        size: c.spec.qty,
                        weight: 0.0,
                    });
                }
            }
        }
        self.combos.resting = still;
        self.combos.forget_if_idle();
        fills
    }

    /// One combo at `ts`: `Ok(Some(the price of each leg))` when it fills now, `Ok(None)` when it
    /// waits (a leg with no fresh mark, a limit not crossed), `Err` when it ends (a settled leg, a
    /// leg the grid would refuse or resize).
    fn cross_combo(&self, c: &OrderComboResting, ts: i64) -> Result<Option<Vec<f64>>, String> {
        let spec = &c.spec;
        if let Some(k) = c.sis.iter().position(|&si| self.is_resolved_index(si)) {
            let symbol = &spec.legs[k].symbol;
            return Err(format!(
                "combo leg {symbol} has settled: a combo fills all its legs or none"
            ));
        }
        let mut marks = Vec::with_capacity(c.sis.len());
        for &si in &c.sis {
            match self.combos.marks.get(si).copied().flatten() {
                Some(m)
                    if !vike_fills::staleness::is_stale(
                        Some(m.0),
                        ts,
                        self.combo_mark_staleness_ms,
                    ) =>
                {
                    marks.push(m)
                }
                _ => return Ok(None),
            }
        }
        let quote = |k: usize, want_ask: bool| if want_ask { marks[k].2 } else { marks[k].1 };
        let prices = if by_legs(spec) {
            let at =
                |symbol: &str| spec.legs.iter().position(|l| l.symbol == symbol).expect("a leg");
            match vike_model::combo_legs_cross(spec.side, &spec.legs, |s, ask| quote(at(s), ask)) {
                Some(p) => p,
                None => return Ok(None),
            }
        } else {
            let quotes: Vec<(i32, f64)> = spec
                .legs
                .iter()
                .enumerate()
                .map(|(k, l)| (l.ratio, quote(k, spec.side * l.ratio.signum() > 0)))
                .collect();
            if let Some(limit) = spec.net_limit
                && vike_model::combo_net_cross(spec.side, limit, &quotes).is_none()
            {
                return Ok(None);
            }
            quotes.into_iter().map(|(_, px)| px).collect()
        };
        let priced = is_priced(spec);
        for ((leg, &si), &px) in spec.legs.iter().zip(&c.sis).zip(&prices) {
            let (side, qty) = leg_terms(spec, leg);
            let (slip, _) = self.slippage_for(si, qty, px, ts, priced);
            let fresh = OrderFees::default();
            let t = self.fill_terms(si, side, qty, px, slip, ts, priced, priced, fresh);
            if t.size != qty {
                let floor = t.refused.map_or("step_size", |r| r.0);
                return Err(format!(
                    "a combo fills all its legs or none, and the grid does not take {qty} {} \
                     ({floor})",
                    leg.symbol
                ));
            }
        }
        Ok(Some(prices))
    }

    /// [`Self::cancel_all`]'s share: every combo with a leg on `symbols[si]` goes at once.
    pub(super) fn drop_combos_on(&mut self, si: usize) {
        self.combos.resting.retain(|c| !c.sis.contains(&si));
        self.combos.forget_if_idle();
    }

    /// What each resting combo's legs on `symbols[si]` commit, by live's one rule
    /// ([`vike_execution::risk::committed_quantity`], `position` the symbol's): `OrderBrokerSimulator::committed_order_quantities`'s
    /// share. Nothing while no combo rests.
    pub(super) fn combo_committed(&self, si: usize, position: f64) -> impl Iterator<Item = f64> {
        let legs = self.combos.resting.iter().flat_map(move |c| {
            let on = c.sis.iter().zip(&c.spec.legs).filter(move |(s, _)| **s == si);
            on.map(move |(_, leg)| leg_terms(&c.spec, leg))
        });
        legs.filter_map(move |(side, qty)| {
            vike_execution::risk::committed_quantity(qty, side, position)
        })
    }

    /// EXERCISE `qty` contracts of the held option `symbol` now: closed at its intrinsic value per
    /// contract against its underlying's last price, IN CASH and fee-free (no expiry's delivery fee,
    /// `StrategyEngineConfig::delivery_fees`), at the intrinsic its expiry settles it at (a
    /// call `max(0, u − strike)`, a put `max(0, strike − u)`; out of the money that is 0). Recorded in
    /// [`Self::settlements`] and returned, the fill the caller hears of (it reaches no `on_fill`).
    ///
    /// Only an AMERICAN option of [`StrategyEngineConfig::option_specs`] whose underlying is a symbol of the
    /// run with a price, unsettled, held LONG: at most the long position is exercised. Every other
    /// ask is refused in words in [`Self::dropped`] (a European option exercises only by its expiry,
    /// an `Unknown` style is no proof of an American one, a short or flat book holds nothing to
    /// exercise). Cash only, the owner's ruling (2026-11-09): there is no delivery of the underlying.
    pub fn exercise(&mut self, symbol: &str, qty: f64) -> Option<Fill> {
        let si = self.index(symbol);
        match self.judge_exercise(si, qty) {
            Ok((qty, payout)) => {
                let (side, ts) = (-1, self.now);
                let settled =
                    SettlementFill { symbol: symbol.to_string(), payout, qty, side, timestamp: ts };
                self.settlements.push(settled);
                let fill = self.fold_fill(si, side, qty, payout, ts, false, 0.0, true);
                self.advance_shadow(&fill);
                Some(fill)
            }
            Err(why) => {
                self.dropped.push(OrderDrop {
                    symbol: symbol.to_string(),
                    reason: why,
                    size: qty,
                    weight: 0.0,
                });
                None
            }
        }
    }

    /// [`Self::exercise`]'s judgement: the contracts to exercise and the intrinsic value of each.
    fn judge_exercise(&self, si: usize, qty: f64) -> Result<(f64, f64), String> {
        let symbol_name = &self.symbols[si];
        let ask =
            ExerciseSpec { venue: String::new(), symbol: symbol_name.clone(), qty, account: None };
        ask.validate()?;
        if self.is_resolved_index(si) {
            return Err(format!("{symbol_name} has expired: nothing is left to exercise"));
        }
        let Some(spec) = self.option_specs.as_ref().and_then(|f| f(symbol_name)) else {
            return Err(format!("{symbol_name} is not an option this run's option specs name"));
        };
        match spec.style {
            ContractOptionExerciseStyle::American => {}
            ContractOptionExerciseStyle::European => {
                return Err(format!(
                    "{symbol_name} is a European option: only its expiry exercises it"
                ));
            }
            ContractOptionExerciseStyle::Unknown => {
                return Err(format!(
                    "{symbol_name}'s exercise style is unknown: only an American option is exercised \
                     before its expiry"
                ));
            }
        }
        let under = spec.underlying.as_deref();
        let Some(u) = under.and_then(|u| self.symbols.iter().position(|s| s == u)) else {
            return Err(format!(
                "{symbol_name}'s underlying is not a symbol of this run: there is no price to exercise \
                 it against"
            ));
        };
        let price = self.symbol[u].price;
        if !(price.is_finite() && price > 0.0) {
            return Err(format!("{symbol_name}'s underlying has no price yet"));
        }
        let long = self.symbol[si].pos.size;
        if long <= 0.0 {
            return Err(format!(
                "only a long option is exercised, and {symbol_name}'s position is {long}"
            ));
        }
        Ok((qty.min(long), vike_options::intrinsic(spec.right, spec.strike, price)))
    }
}
