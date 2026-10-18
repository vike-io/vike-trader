//! Fill application: the PIT grid and the fee, the shared fold, and the two settlement lanes.

use vike_model::{Fill, FillKind, Trade, TradeFold};

use vike_fills::broker_sim::{adverse_fill_price, fee as fee_fn};

#[cfg(doc)]
use super::params::EngineParams;
use super::state::{MirrorFill, SettlementFill, SimBroker, VariationSettlement};

impl SimBroker {
    // --- fill application (called by the engine dispatchers) ---

    /// Apply one fill; returns `None` when the PIT grid gates the fill (an opening/increasing
    /// fill below `min_qty`/`min_notional`, or a dust size after step-rounding) — the caller
    /// must treat `None` as "no fill happened" and skip `fire_on_fill`.
    pub(crate) fn apply_fill(
        &mut self,
        si: usize,
        side_sign: i32,
        size: f64,
        price: f64,
        ts: i64,
        is_maker: bool,
    ) -> Option<Fill> {
        // Bound to a local so the saturation can be COUNTED without pricing the fill twice; the
        // value handed to `adverse_fill_price` is the same `f64` the one-liner passed before, so
        // the fill price is byte-identical on every input that did not already invert.
        let (slip, impact_skipped) = self.slippage_for(si, size, ts, is_maker);
        if vike_fills::broker_sim::adverse_move_saturates(side_sign, slip) {
            self.slippage_saturations += 1;
        }
        // Counted HERE and nowhere else, for the reason both counters' docs give: the cash
        // pre-check in `StrategyEngine` prices this same fill a second time.
        if impact_skipped {
            self.impact_unpriced += 1;
        }
        let price = adverse_fill_price(price, side_sign, slip);
        let (size, price) = match self.grid_for(si, ts) {
            None => (size, price),
            Some(f) => {
                let price = vike_model::round_to(price, vike_model::nz_step(f.tick_size));
                let rounded = vike_model::round_to(size, vike_model::nz_step(f.step_size));
                // Reject only OPENING/increasing fills — a closing fill must always execute so a
                // position is never stranded below-min.
                //
                // NOTE the deliberate asymmetry vs `vike_exec::RiskGate`: this predicate is
                // DIRECTION-ONLY (`vike_model::is_reducing_direction`), while the gate's floor
                // bypass is `is_covered_reduce` (direction AND `|position| >= |qty|`). A below-min
                // REVERSAL therefore executes whole here but is denied by the gate. That
                // divergence is pre-existing and documented on both sides; unifying it changes
                // FILL DECISIONS, so it is NOT folded into this hoist — see the `risk.rs`
                // "KNOWN RESIDUAL DIVERGENCE" comment for the intended SimBroker-side fix (split
                // a flip and floor-gate only its opening half).
                let opening = !vike_model::is_reducing_direction(side_sign, self.sym[si].pos.size);
                // DELIBERATELY hand-rolled, NOT `vike_model::order_notional` — the ALLOWLIST
                // entry for this file in `vike-ops/tests/duplicate_shape_gate.rs` carries the
                // full reason. Short version: the helper takes the magnitude of all THREE
                // factors, this floor takes the SIGNED product, and the two verdicts genuinely
                // disagree whenever an odd number of factors is negative. Adopting the helper
                // would be a FILL-DECISION change, not a byte-identical hoist.
                //
                // ⚠ Hoisted ABOVE the opening/closing split so the floor verdict is spelled ONCE
                // and both consumers below read the same two booleans — the opening branch's
                // refusal, and the closing branch's divergence counter. Pure arithmetic with no
                // side effects, so computing it on the closing path too is byte-identical; the
                // alternative (recomputing it under the counter) is a second spelling of the very
                // shape `notional_is_never_respelled` exists to prevent, and that gate caught it.
                let notional = rounded * price * self.multiplier_of_idx(si);
                let below_qty = f.min_qty > 0.0 && rounded < f.min_qty;
                let below_not = f.min_notional > 0.0 && notional < f.min_notional;
                // ⚠ MEASUREMENT ONLY — counts the known live-vs-backtest divergence, changes
                // nothing. See `SimBroker::below_min_reversals`.
                //
                // The condition is exactly "live denies, backtest fills": the order OPPOSES the
                // position (so this branch calls it closing and waves it through), it EXCEEDS the
                // position (a flip, so `is_covered_reduce` is false and the live gate's floor bypass
                // does NOT apply), and it is BELOW a venue floor (so the live gate refuses it).
                if !opening
                    && self.sym[si].pos.size != 0.0
                    && rounded.abs() > self.sym[si].pos.size.abs()
                    && (below_qty || below_not)
                {
                    self.below_min_reversals += 1;
                }
                if opening {
                    if below_qty || below_not {
                        let reason = if below_qty { "min_qty" } else { "min_notional" };
                        self.dropped.push((
                            self.symbols[si].clone(),
                            reason.into(),
                            rounded,
                            price,
                        ));
                        return None;
                    }
                    (rounded, price)
                } else {
                    // Closing/reducing fill must ALWAYS execute. If step-rounding zeroed a small
                    // remainder — reachable because the PIT `step_size` can WIDEN over a position's
                    // life (day-1 open on a fine grid, later close on a coarser one) — fall back to
                    // the raw size so the position is never stranded (also guards the engine's own
                    // force-close paths: stops, liquidation, close-inactive).
                    (if rounded > 0.0 { rounded } else { size }, price)
                }
            }
        };
        if size <= 0.0 {
            return None; // step-rounding zeroed a dust size
        }
        // NOTE (fee-model caveat, see `vike_model::money::fees`): `is_maker` reaches here decided by ORDER
        // KIND at the dispatch sites, not by crossing aggressiveness — a marketable limit books as
        // a maker fill. Harmless for the flat maker/taker rates used here; it flips the fee SIGN
        // under a rebate-bearing `FeeSchedule::ProbabilityScaled`.
        let rate = if is_maker { self.maker_fee } else { self.taker_fee };
        let mult = self.multiplier_of_idx(si);
        // `fee_curve` is `Some` for every schedule shape but the two `StrategyEngine::new` names
        // as flattenable (`PercentMakerTaker`, `Free`) — see `EngineParams::fee_schedule`. An
        // unset schedule, and those two, take the frozen `size * price * rate * mult` fold below,
        // byte-identical. ⚠ The `* mult` is right for a rate-shaped or notional-capped term and
        // wrong for a CURRENCY-shaped one (`PerShareWithFloor`'s per-order minimum is a dollar
        // amount, not a dollar per contract); it is exact at the default `multiplier = 1.0`.
        let fee = match self.fee_curve {
            Some(schedule) => schedule.commission(is_maker, size, price) * mult,
            None => fee_fn(size, price, rate, mult),
        };
        Some(self.fold_fill(si, side_sign, size, price, ts, is_maker, fee, false))
    }

    /// The post-cost fold shared by [`Self::apply_fill`] (which computes slippage / PIT-grid /
    /// fee first) and [`Self::settle_at_payout`] (fee `0.0`, no slippage, no grid — a venue
    /// settlement, not an order fill): cash moves, the `TradeFold` cost-basis step,
    /// excursion/stop bookkeeping, and the trade + mirror records. Extracted VERBATIM from
    /// `apply_fill` — the f64 op order is byte-identical, so every parity gate is unaffected.
    fn fold_fill(
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
        let mult = self.multiplier_of_idx(si);
        let delta = side_sign as f64 * size;
        // The REALISED mix and the realised total, recorded beside the charge itself (see
        // `maker_fills`). Two `u64` increments and one add on the fill path, after every gate has
        // already passed — this is the backtest engine, not the live fold the latency gate
        // measures. Settlements are excluded: they are venue events with a zero fee, and a
        // `settlement` folded into the taker count would describe a fill nobody placed.
        if !settlement {
            if is_maker {
                self.maker_fills += 1;
            } else {
                self.taker_fills += 1;
            }
            self.fees_paid += fee;
        }
        self.cash -= fee; // transaction cost
        self.cash -= vike_model::signed_notional(delta, price, mult); // signed notional moves cash
        let st = &mut self.sym[si];
        // Drive the shared trade fold over this symbol's position state (copy in, fold, write
        // back). The excursion (`hi_since`/`lo_since`) and protective-`stop` bookkeeping stays
        // engine-local — the fold owns only the cost-basis/fee-apportionment math.
        let mut fold = TradeFold {
            size: st.pos.size,
            avg_px: st.pos.avg_price,
            entry_fee: st.entry_fee,
            entry_ts: st.entry_ts,
        };
        let step = fold.apply(side_sign, size, price, fee, ts, mult);
        st.pos.size = fold.size;
        st.pos.avg_price = fold.avg_px;
        st.entry_fee = fold.entry_fee;
        st.entry_ts = fold.entry_ts;
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
                    entry_ts: c.entry_ts,
                    exit_ts: c.exit_ts,
                    symbol: self.symbols[si].clone(),
                    mae,
                    mfe,
                    is_long: c.is_long,
                });
            }
        }
        if let Some(sink) = &mut self.mirror_fills {
            sink.push(MirrorFill {
                symbol: self.symbols[si].clone(),
                side: side_sign,
                size,
                price,
                fee,
                ts,
                is_maker,
                is_settlement: settlement,
            });
        }
        Fill { side: side_sign, size, price, fee, ts, is_maker, symbol: self.symbols[si].clone() }
    }

    /// Binary-resolution settlement (pm-economics lane): close the WHOLE open position at the
    /// terminal `payout` price. Fee-free, slippage-free and PIT-grid-free — a settlement is a
    /// venue event, not an order fill, so none of the order-fill costs/gates apply (and a closing
    /// fill must never be gated anyway — the anti-stranding rule). Records a distinct
    /// [`SettlementFill`] in [`Self::settlements`] and returns the (fee `0.0`) close `Fill` for
    /// `on_fill`; `None` when already flat. The caller (the engine's resolution sweep) latches the
    /// symbol resolved and cancels its resting orders BEFORE calling this, so the `on_fill` this
    /// fill triggers cannot re-open the market. When the ledger mirror is enabled the settlement
    /// is pushed as a [`MirrorFill`] flagged `is_settlement` — a payout, not a trade.
    pub(crate) fn settle_at_payout(&mut self, si: usize, payout: f64, ts: i64) -> Option<Fill> {
        let pos = self.sym[si].pos.size;
        if pos == 0.0 {
            return None;
        }
        let side = vike_model::closing_side(pos);
        let qty = pos.abs();
        self.settlements.push(SettlementFill {
            symbol: self.symbols[si].clone(),
            payout,
            qty,
            side,
            ts,
        });
        Some(self.fold_fill(si, side, qty, payout, ts, false, 0.0, true))
    }

    /// Variation-margin settlement sweep for one step (the opt-in
    /// [`EngineParams::settlement_period_ms`] cadence — see that field for the full contract).
    ///
    /// Returns IMMEDIATELY when the cadence is unset or non-positive, which is what keeps every
    /// default run byte-identical: no bucket is ever computed, no symbol is ever visited. When the
    /// step's bucket (`ts.div_euclid(period)`) still matches the stored one this is also a no-op;
    /// on the first step of a run the bucket is merely ANCHORED (there is no prior mark to settle
    /// against).
    ///
    /// On a boundary, for each symbol with an open position: `dpl = size * (price - avg_price) *
    /// mult` is added to `realized` and to `settled_profit`, and `avg_price` is reset to `price`.
    /// `equity_now()` is unchanged by construction (it never reads `avg_price`), and the per-symbol
    /// curve `realized + size·(price − avg_price)·mult` is likewise invariant — the two terms move
    /// by `+dpl` and `−dpl`. Symbols are visited in `symbols` order (the engine's deterministic
    /// insertion order), so the sequence of settlements is reproducible; no cross-symbol f64 sum is
    /// formed here, so there is no `py_sum` site to preserve.
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
            let mult = self.multiplier_of_idx(si);
            let st = &self.sym[si];
            if st.pos.size == 0.0 {
                continue;
            }
            let mark = st.price;
            let dpl = st.pos.size * (mark - st.pos.avg_price) * mult;
            if dpl == 0.0 {
                continue; // flat mark: nothing crosses, and the basis is already `mark`
            }
            let st = &mut self.sym[si];
            st.pos.avg_price = mark;
            st.realized += dpl;
            st.settled_profit += dpl;
            let settled_profit = st.settled_profit;
            self.variation_settlements.push(VariationSettlement {
                symbol: self.symbols[si].clone(),
                ts,
                mark,
                amount: dpl,
                settled_profit,
            });
        }
    }
}
