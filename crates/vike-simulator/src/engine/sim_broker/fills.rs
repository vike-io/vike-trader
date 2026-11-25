//! Fill application: the PIT grid and the fee, the shared fold, and the two settlement lanes.

use vike_model::{ContractValue, Fill, FillKind, OrderDrop, OrderFees, Trade, TradeFold};

use vike_fills::broker_sim::{adverse_fill_price, commission, commission_on_underlying};

use super::ledger_events::LedgerEventKind;
#[cfg(doc)]
use super::params::StrategyEngineConfig;
use super::state::{
    OrderBrokerSimulator, OrderBrokerSimulatorSymbolState, SettlementFill, SettlementVariation,
};

/// The cash a fill leaves: the fee out (transaction cost), then the signed notional (a coin leg on an
/// inverse contract, [`ContractValue::cash_leg`]). THE fill's cash step, shared by
/// [`OrderBrokerSimulator::fold_fill`] and `rebuild_equity_curve` (`ledger_events.rs`).
#[inline]
pub(crate) fn fill_cash(cash: f64, value: ContractValue, delta: f64, price: f64, fee: f64) -> f64 {
    (cash - fee) - value.cash_leg(delta, price)
}

/// One fill's final terms ([`OrderBrokerSimulator::fill_terms`]). `size` is `0.0` when the grid refuses it whole.
pub(crate) struct FillTerms {
    pub(crate) size: f64,
    pub(crate) price: f64,
    pub(crate) fee: f64,
    /// What the grid refused of it: the floor it failed (the word in [`OrderBrokerSimulator::dropped`]) and the
    /// size refused.
    pub(crate) refused: Option<(&'static str, f64)>,
    /// Whether [`StrategyEngineConfig::slippage_amount`] hit its floor ([`vike_fills::broker_sim::adverse_fill_amount`]):
    /// counted by [`OrderBrokerSimulator::apply_fill`] alone, as the fraction's floor is.
    pub(crate) amount_floored: bool,
    /// The order's fees once this fill books ([`OrderBrokerSimulator::apply_order_fill`]).
    pub(crate) order: OrderFees,
}

impl OrderBrokerSimulator {
    // --- fill application (called by the engine dispatchers) ---

    /// Apply one fill; returns `None` when the PIT grid gates the fill (an opening/increasing
    /// fill below `min_qty`/`min_notional`, or a dust size after step-rounding) — the caller
    /// must treat `None` as "no fill happened" and skip `fire_on_fill`. `limit_bounded` is the
    /// order's [`vike_model::OrderResting::is_limit_bounded`] (`false` for a forced close).
    /// The fill is its order's only one: a forced close, or a lane whose order leaves the book whole.
    /// Its order carries no sizer weight (a forced close, a tagged quote, a combo leg), so a grid
    /// refusal records `0.0` there, as every weightless push into [`OrderBrokerSimulator::dropped`] does.
    pub(crate) fn apply_fill(
        &mut self,
        si: usize,
        side_sign: i32,
        size: f64,
        price: f64,
        ts: i64,
        is_maker: bool,
        limit_bounded: bool,
    ) -> Option<Fill> {
        let fresh = &mut OrderFees::default();
        self.apply_order_fill(si, side_sign, size, price, 0.0, ts, is_maker, limit_bounded, fresh)
    }

    /// The next engine-local order identity ([`vike_model::OrderResting::qid`]), never handed out
    /// twice in a run: the queue model's stamp and a rested remainder's ([`Self::order_fees`]). In a
    /// portfolio run it carries the current mount (`mounts.rs`; `0` adds nothing).
    pub(crate) fn next_queue_id(&mut self) -> u64 {
        let id = self.queue_id_sequence | self.mount_bits();
        self.queue_id_sequence += 1;
        id
    }

    /// What the order `qid` has been charged so far ([`Self::order_fees`]); nothing for an order
    /// with no fill yet, or no `qid`.
    pub(crate) fn order_fees_of(&self, qid: u64) -> OrderFees {
        self.order_fees.get(&qid).map_or_else(OrderFees::default, |e| e.1)
    }

    /// After a fill of the order `qid` on `si`: its `fees` are kept while an order of that `qid`
    /// still rests there, and dropped once none does (filled whole, its remainder dropped). Keeping
    /// one drops the entries of every order that left the book since (a cancel, an expiry), so the
    /// table never holds more than the resting orders that filled in part. `qid` 0 has no entry.
    pub(crate) fn note_order_fees(&mut self, si: usize, qid: u64, fees: OrderFees) {
        if qid == 0 {
            return;
        }
        let rests = |st: &OrderBrokerSimulatorSymbolState, qid: u64| {
            st.pending.iter().any(|o| o.qid == qid)
        };
        if rests(&self.symbol[si], qid) {
            let symbol = &self.symbol;
            self.order_fees.retain(|q, (s, _)| rests(&symbol[*s], *q));
            self.order_fees.insert(qid, (si, fees));
        } else {
            self.order_fees.swap_remove(&qid);
        }
    }

    /// [`Self::apply_fill`] of a fill whose order may fill in pieces: `order` is what the order
    /// filled and was charged before it ([`Self::order_fees_of`]), advanced when the fill books, so
    /// a per-order fee minimum is charged once (`vike_fills::broker_sim::commission`). `weight` is
    /// the order's [`vike_model::OrderResting::weight`], what a grid refusal records in
    /// [`OrderBrokerSimulator::dropped`].
    pub(crate) fn apply_order_fill(
        &mut self,
        si: usize,
        side_sign: i32,
        size: f64,
        price: f64,
        weight: f64,
        ts: i64,
        is_maker: bool,
        limit_bounded: bool,
        order: &mut OrderFees,
    ) -> Option<Fill> {
        // Bound to a local so the saturation can be COUNTED without pricing the fill twice; the
        // value handed to `adverse_fill_price` is the same `f64` the one-liner passed before, so
        // the fill price is byte-identical on every input that did not already invert.
        let (slip, impact_skipped) = self.slippage_for(si, size, price, ts, is_maker);
        if vike_fills::broker_sim::adverse_move_saturates(side_sign, slip) {
            self.slippage_saturations += 1;
        }
        // Counted HERE and nowhere else, for the reason both counters' docs give: the cash
        // pre-check in `StrategyEngine` prices this same fill a second time.
        if impact_skipped {
            self.impact_unpriced += 1;
        }
        let t =
            self.fill_terms(si, side_sign, size, price, slip, ts, is_maker, limit_bounded, *order);
        if t.amount_floored {
            self.slippage_saturations += 1;
        }
        if let Some((reason, refused)) = t.refused {
            self.dropped.push(OrderDrop {
                symbol: self.symbols[si].clone(),
                reason: reason.into(),
                size: refused,
                weight,
            });
            // A refusal with a size left is a split flip. See `OrderBrokerSimulator::below_min_reversals`.
            if t.size > 0.0 {
                self.below_min_reversals += 1;
            }
        }
        if t.size <= 0.0 {
            return None; // refused whole, or step-rounding zeroed a dust size
        }
        if self.shortfall_cover.is_some()
            && vike_model::is_reducing_direction(side_sign, self.symbol[si].pos.size)
        {
            self.shortfall_reduced[si] = Some((self.step, side_sign));
        }
        *order = t.order;
        Some(self.fold_fill(si, side_sign, t.size, t.price, ts, is_maker, t.fee, false))
    }

    /// What a fill of `size` at the raw `price` finally IS: slippage `slip`, then the fixed
    /// [`StrategyEngineConfig::slippage_amount`], then the PIT grid, then the fee. PURE: [`Self::apply_fill`]
    /// books the answer and keeps the counters, and the cash pre-check of
    /// `StrategyEngine::fill_step_gated` judges the same answer, so a gate cannot pass what the fill
    /// then charges more for. The fixed amount is keyed on `limit_bounded`, never on `is_maker`: a
    /// stop-limit filling on its trigger event is a taker whose limit still bounds it. `order` is the
    /// order's fees before this fill ([`Self::apply_order_fill`]).
    pub(crate) fn fill_terms(
        &self,
        si: usize,
        side_sign: i32,
        size: f64,
        price: f64,
        slip: f64,
        ts: i64,
        is_maker: bool,
        limit_bounded: bool,
        mut order: OrderFees,
    ) -> FillTerms {
        let price = adverse_fill_price(price, side_sign, slip);
        let (price, amount_floored) = vike_fills::broker_sim::adverse_fill_amount(
            price,
            side_sign,
            self.slippage_amount,
            limit_bounded,
        );
        let (size, price, refused) = match self.grid_for(si, ts) {
            None => (size, price, None),
            Some(f) => {
                let price = f.round_price(price);
                let rounded = vike_model::round_to(size, vike_model::nz_step(f.step_size));
                let pos = self.symbol[si].pos.size;
                // Reject only OPENING/increasing fills — a closing fill must always execute so a
                // position is never stranded below-min.
                let opening = !vike_model::is_reducing_direction(side_sign, pos);
                // The SIGNED product (`ContractValue::signed_notional`: linear `rounded·price·u`,
                // inverse `rounded·u`, the price never entering), NOT `order_notional`, which takes
                // the magnitude of all three factors: on a negative-priced instrument the signed
                // product is negative and the opening fill is dropped where the magnitude could
                // clear the floor, so the magnitude form would be a FILL-DECISION change.
                //
                // ⚠ Hoisted ABOVE the opening/closing split so the floor verdict is spelled ONCE
                // and both branches below read it. Pure arithmetic with no side effects, so
                // computing it on the closing path too is byte-identical.
                //
                // In the QUOTE currency, the one the venue states its floor in: a converted symbol's
                // ledger value would compare a ledger amount with the venue's number (decision 0183, item 3).
                let notional = self.quote_value_of_index(si).signed_notional(rounded, price);
                let floor = if f.min_qty > 0.0 && rounded < f.min_qty {
                    Some("min_qty")
                } else if f.min_notional > 0.0 && notional < f.min_notional {
                    Some("min_notional")
                } else {
                    None
                };
                if opening {
                    match floor {
                        Some(reason) => (0.0, price, Some((reason, rounded))),
                        None => (rounded, price, None),
                    }
                } else if let Some(reason) = floor
                    && rounded.abs() > pos.abs()
                {
                    // A below-min REVERSAL: it EXCEEDS the position, so it flips through flat and
                    // is no `is_covered_reduce`, and it is below a floor, so the live gate denies
                    // it. Its closing half executes and its opening half is refused. A flip whose
                    // WHOLE size clears the floor is one order the venue takes, and falls through.
                    // shortcut: the impact model in `slip` priced the whole order, not the closing
                    // half; upgrade if a run with an impact model reads a non-zero count.
                    (pos.abs(), price, Some((reason, rounded - pos.abs())))
                } else {
                    // Closing/reducing fill must ALWAYS execute. If step-rounding zeroed a small
                    // remainder — reachable because the PIT `step_size` can WIDEN over a position's
                    // life (day-1 open on a fine grid, later close on a coarser one) — fall back to
                    // the raw size so the position is never stranded (also guards the engine's own
                    // force-close paths: stops, liquidation, close-inactive).
                    (if rounded > 0.0 { rounded } else { size }, price, None)
                }
            }
        };
        // Opt-in `StrategyEngineConfig::shortfall_cover`: a fill the account cannot margin is refused WHOLE,
        // a flip's closing half with it, as its reference refuses the order
        // (`OrderBrokerSimulator::shortfall_refuses`).
        let (size, refused) = if size > 0.0 && self.shortfall_refuses(si, side_sign, size, price) {
            (0.0, Some((vike_model::OrderDenial::InsufficientMargin.template(), size)))
        } else {
            (size, refused)
        };
        // NOTE (fee-model caveat, see `vike_model::money::fees`): `is_maker` reaches here decided by ORDER
        // KIND at the dispatch sites, not by crossing aggressiveness — a marketable limit books as
        // a maker fill. Harmless for the flat maker/taker rates used here; it flips the fee SIGN
        // under a rebate-bearing `FeeSchedule::ProbabilityScaled`.
        //
        // `fee_curve` is `Some` for every schedule shape but the two `StrategyEngine::new` names
        // as flattenable (`PercentMakerTaker`, `Free`) — see `StrategyEngineConfig::fee_schedule`. An
        // unset schedule, and those two, take the frozen `size * price * rate * mult` fold,
        // byte-identical.
        //
        // The fee is booked in the ledger's money: the paper book's own inverse fee (dollars) in
        // coin at the fill price, as `vike_execution::Account` books it; a linear amount is its own.
        // It is priced in the symbol's QUOTE currency, as the paper book and a venue state it, and a
        // converted symbol's is then booked at the rate in force (`quote_rate_of`, the live engine's
        // `fill.commission * rate`): the fill law never sees a rate (decision 0243, item 2).
        let value = self.quote_value_of_index(si);
        // The symbol's own schedule when the run lists one (`StrategyEngineConfig::fee_schedules`), charged
        // exactly; else the run-wide path, unchanged.
        let schedule = self.fee_by_si.get(si).copied().flatten().or(self.fee_curve);
        // An option whose underlying the run lists is charged on it (`StrategyEngineConfig::fee_underlyings`),
        // at the underlying's mark: the previous step's close on the bar lane, never this one's. On a
        // linear contract only (`vike_fills::broker_sim::commission_on_underlying`): an inverse one is
        // charged as any other fill.
        let underlying = self
            .fee_underlying_by_si
            .get(si)
            .copied()
            .flatten()
            .map(|u| self.symbol[u].price)
            .filter(|u| *u > 0.0);
        let (maker, taker) = (self.maker_fee, self.taker_fee);
        let quoted = commission(
            schedule, maker, taker, is_maker, size, price, value, underlying, &mut order,
        );
        let fee = self.booked_fee(si, value, quoted, price);
        // A spot (cash) symbol never buys for more than the FREE cash the account holds (`Self::free_cash`: the
        // margin its derivatives hold taken out), its fee in it, as a spot venue refuses the order: refused WHOLE
        // (`StrategyEngineConfig::cash_symbols`; owner 2026-11-16). A symbol on margin and a reducing buy are untouched.
        let overbuys = size > 0.0
            && side_sign > 0
            && self.cash_by_si[si]
            && !vike_model::is_reducing_direction(side_sign, self.symbol[si].pos.size)
            && self.value_of_index(si).cash_leg(size, price) + fee > self.free_cash() + 1e-9;
        if overbuys {
            let why = vike_model::OrderDenial::CashOverbuy.template();
            return FillTerms {
                size: 0.0,
                price,
                fee,
                refused: Some((why, size)),
                amount_floored,
                order,
            };
        }
        FillTerms { size, price, fee, refused, amount_floored, order }
    }

    /// A fee `quoted` in `symbols[si]`'s QUOTE currency (its `value`, filled at `price`) as the ledger
    /// books it: the paper book's own inverse fee (dollars) in coin at the price, as
    /// `vike_execution::Account` books it, a linear amount as it is, and a converted symbol's then at the
    /// rate in force (`quote_rate_of`). An order fill's fee and a delivery fee are both booked here.
    fn booked_fee(&self, si: usize, value: ContractValue, quoted: f64, price: f64) -> f64 {
        let fee = value.in_settle(quoted, price);
        match self.quote_rate_of(si) {
            Some(rate) => rate.to_account(fee),
            None => fee,
        }
    }

    /// The DELIVERY fee an expiry settlement of `qty` of `symbols[si]` at `payout` pays
    /// ([`StrategyEngineConfig::delivery_fees`]), a share of its underlying at `underlying`: on a linear
    /// contract, as an option's trading fee ([`Self::fill_terms`]); `0.0` for a symbol not listed and on an
    /// inverse contract.
    fn delivery_fee(&self, si: usize, qty: f64, payout: f64, underlying: f64) -> f64 {
        let Some(schedule) = self.delivery_fee_by_si.get(si).copied().flatten() else {
            return 0.0;
        };
        let value = self.quote_value_of_index(si);
        commission_on_underlying(schedule, qty, payout, underlying, value)
            .map_or(0.0, |quoted| self.booked_fee(si, value, quoted, payout))
    }

    /// The post-cost fold shared by [`Self::apply_fill`] (which computes slippage / PIT-grid /
    /// fee first) and [`Self::settle_at_payout`] (no slippage, no grid, fee `0.0` but an option's
    /// delivery fee — a venue settlement, not an order fill): cash moves, the `TradeFold` cost-basis step,
    /// excursion/stop bookkeeping, and the trade + mirror records. Extracted VERBATIM from
    /// `apply_fill` — the f64 op order is byte-identical, so every parity gate is unaffected. An
    /// option's exercise books through it too (`OrderBrokerSimulator::exercise`), as a settlement.
    pub(super) fn fold_fill(
        &mut self,
        si: usize,
        side_sign: i32,
        size: f64,
        price: f64,
        ts: i64,
        is_maker: bool,
        fee: f64,
        settlement: bool,
    ) -> Fill {
        let value = self.value_of_index(si);
        let delta = side_sign as f64 * size;
        // The REALISED mix and the realised total, recorded beside the charge itself (see
        // `maker_fills`). Two `u64` increments and one add on the fill path, after every gate has
        // already passed — this is the backtest engine, not the live fold the latency gate
        // measures. Settlements are excluded from the counts: they are venue events, and a
        // `settlement` folded into the taker count would describe a fill nobody placed. Their fee
        // (zero, or an option's delivery fee) is still a fee paid.
        if !settlement {
            if is_maker {
                self.maker_fills += 1;
            } else {
                self.taker_fills += 1;
            }
        }
        self.fees_paid += fee;
        self.cash = fill_cash(self.cash, value, delta, price, fee);
        self.log_ledger(si, LedgerEventKind::Fill { side: side_sign as i8, size, price, fee });
        let st = &mut self.symbol[si];
        // Drive the shared trade fold over this symbol's position state (copy in, fold, write
        // back). The excursion (`hi_since`/`lo_since`) and protective-`stop` bookkeeping stays
        // engine-local — the fold owns only the cost-basis/fee-apportionment math.
        let mut fold = TradeFold {
            size: st.pos.size,
            average_price: st.pos.average_price,
            entry_fee: st.entry_fee,
            entry_timestamp: st.entry_timestamp,
        };
        let step = fold.apply_value(side_sign, size, price, fee, ts, value);
        st.pos.size = fold.size;
        st.pos.average_price = fold.average_price;
        st.entry_fee = fold.entry_fee;
        st.entry_timestamp = fold.entry_timestamp;
        match step.kind {
            FillKind::Open => {
                st.hi_since = price; // reset excursion extremes to entry
                st.lo_since = price;
            }
            FillKind::Add => {}
            _ => {
                // reduce / close / flip
                let c = step.closed.expect("reduce/close/flip yields a closed trade");
                st.realized += c.pnl;
                let entry = c.entry_price;
                // NB: `c.is_long` is the PRIOR sign — the closed portion's direction, exactly as
                // in Python (the fold captured it before updating `size`).
                let (mfe, mae) = if entry != 0.0 {
                    if c.is_long {
                        ((st.hi_since - entry) / entry, (st.lo_since - entry) / entry)
                    } else {
                        ((entry - st.lo_since) / entry, (entry - st.hi_since) / entry)
                    }
                } else {
                    (0.0, 0.0)
                };
                match step.kind {
                    FillKind::Flip => {
                        st.hi_since = price;
                        st.lo_since = price;
                        st.stop = None; // old stop belonged to the closed position
                    }
                    FillKind::Close => st.stop = None,
                    _ => {} // Reduce: keep the stop and the running extremes
                }
                self.trades.push(Trade {
                    entry_price: c.entry_price,
                    exit_price: c.exit_price,
                    size: c.size,
                    pnl: c.pnl,
                    fees: c.fees,
                    entry_timestamp: c.entry_timestamp,
                    exit_timestamp: c.exit_timestamp,
                    symbol: self.symbols[si].clone(),
                    mae,
                    mfe,
                    is_long: c.is_long,
                });
            }
        }
        let fill = Fill {
            side: side_sign,
            size,
            price,
            fee,
            timestamp: ts,
            is_maker,
            symbol: self.symbols[si].clone(),
            tag: None,
        };
        if let Some(sink) = &mut self.mirror_fills {
            sink.push(fill.clone());
        }
        // The bar the fill is stamped in, which a fill booked after its bar's step (a close) keeps; a
        // tick run with no bars books every row at bar 0.
        let bar = self.bars[si].partition_point(|b| b.timestamp <= ts).saturating_sub(1);
        self.closed_trades.apply(&fill, "", bar, value, 1.0);
        fill
    }

    /// Binary-resolution settlement (pm-economics lane): close the WHOLE open position at the
    /// terminal `payout` price. Fee-free, slippage-free and PIT-grid-free — a settlement is a
    /// venue event, not an order fill, so none of the order-fill costs/gates apply (and a closing
    /// fill must never be gated anyway — the anti-stranding rule). The one charge is an option
    /// EXPIRY's delivery fee: with `underlying` (the mark its intrinsic was read against) an option
    /// [`StrategyEngineConfig::delivery_fees`] lists pays [`Self::delivery_fee`]. Records a distinct
    /// [`SettlementFill`] in [`Self::settlements`] and returns the close `Fill` (fee `0.0`, or that
    /// delivery fee) for `on_fill`; `None` when already flat. The caller (the engine's resolution sweep) latches the
    /// symbol resolved and cancels its resting orders BEFORE calling this, so the `on_fill` this
    /// fill triggers cannot re-open the market. When the ledger mirror is enabled the settlement's
    /// `Fill` reaches [`Self::mirror_fills`] too, right after its [`SettlementFill`] in
    /// [`Self::settlements`], which is how a consumer tells the payout from a trade.
    pub(crate) fn settle_at_payout(
        &mut self,
        si: usize,
        payout: f64,
        ts: i64,
        underlying: Option<f64>,
    ) -> Option<Fill> {
        let pos = self.symbol[si].pos.size;
        if pos == 0.0 {
            return None;
        }
        let side = vike_model::closing_side(pos);
        let qty = pos.abs();
        let fee = underlying.map_or(0.0, |u| self.delivery_fee(si, qty, payout, u));
        self.settlements.push(SettlementFill {
            symbol: self.symbols[si].clone(),
            payout,
            qty,
            side,
            timestamp: ts,
        });
        Some(self.fold_fill(si, side, qty, payout, ts, false, fee, true))
    }

    /// Variation-margin settlement sweep for one step (the opt-in
    /// [`StrategyEngineConfig::settlement_period_ms`] cadence — see that field for the full contract).
    ///
    /// Returns IMMEDIATELY when the cadence is unset or non-positive, which is what keeps every
    /// default run byte-identical: no bucket is ever computed, no symbol is ever visited. When the
    /// step's bucket (`ts.div_euclid(period)`) still matches the stored one this is also a no-op;
    /// on the first step of a run the bucket is merely ANCHORED (there is no prior mark to settle
    /// against).
    ///
    /// On a boundary, for each symbol with an open position: `dpl = size * (price - average_price) *
    /// mult` is added to `realized` and to `settled_profit`, and `average_price` is reset to `price`.
    /// `equity_now()` is unchanged by construction (it never reads `average_price`), and the per-symbol
    /// curve `realized + size·(price − average_price)·mult` is likewise invariant — the two terms move
    /// by `+dpl` and `−dpl`. Symbols are visited in `symbols` order (the engine's deterministic
    /// insertion order), so the sequence of settlements is reproducible; no cross-symbol f64 sum is
    /// formed here, so there is no `compensated_sum` site to preserve.
    pub(crate) fn settle_variation_margin(&mut self, ts: i64) {
        let period = match self.settlement_period_ms {
            Some(p) if p > 0 => p,
            _ => return,
        };
        let bucket = ts.div_euclid(period);
        match self.settle_bucket {
            Some(b) if b == bucket => return,
            Some(_) => self.settle_bucket = Some(bucket),
            None => {
                // first step: anchor only — nothing has been marked yet
                self.settle_bucket = Some(bucket);
                return;
            }
        }
        for si in 0..self.symbols.len() {
            let value = self.value_of_index(si);
            let st = &self.symbol[si];
            if st.pos.size == 0.0 {
                continue;
            }
            let mark = st.price;
            // `size * (mark - average_price) * mult` on a linear symbol; the coin PnL on an inverse one,
            // whose cash leg is as additive across marks, so equity is unchanged here either way.
            let dpl = super::lookups::settle_unrealized_pnl(value, &st.pos, mark);
            if dpl == 0.0 {
                continue; // flat mark: nothing crosses, and the basis is already `mark`
            }
            let st = &mut self.symbol[si];
            st.pos.average_price = mark;
            st.realized += dpl;
            st.settled_profit += dpl;
            let settled_profit = st.settled_profit;
            self.variation_settlements.push(SettlementVariation {
                symbol: self.symbols[si].clone(),
                timestamp: ts,
                mark,
                amount: dpl,
                settled_profit,
            });
        }
    }
}
