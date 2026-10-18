//! The bar-path run loop (`StrategyEngine::run`).

use std::rc::Rc;

use vike_model::Strategy;

use super::{MirrorFunding, SimBroker, StrategyEngine};
use vike_analytics::BacktestResult;
use vike_fills::broker_sim::funding_charge;

impl<S: Strategy<SimBroker>> StrategyEngine<S> {
    pub fn run(&mut self) -> BacktestResult {
        let n_sym = self.core.symbols.len();
        let mut equity_curve: Vec<f64> = Vec::with_capacity(self.core.n);
        let mut equity_ts: Vec<i64> = Vec::with_capacity(self.core.n);
        let mut per_symbol_curve: Vec<Vec<f64>> = vec![Vec::with_capacity(self.core.n); n_sym];
        // Reused across bars: the per-symbol "has sub-bars this step" flags. Refilled in place each
        // bar (clear + extend keeps the capacity), so there's no per-bar Vec malloc/free.
        let mut granular: Vec<bool> = Vec::with_capacity(n_sym);
        // THE SHARED-CASH ADMISSION PHASE is reached by either door, and this is the ONE place
        // the disjunction is spelled: `EngineParams::decide = Simultaneous` IMPLIES it, because
        // `fill_step_gated` is the only lane in this engine where a cross-section exists to be
        // ordered at all — the per-symbol `fill_pending` loop consumes the open-position caps in
        // list order inside `dispatch_fill` and offers no site where a ranking could apply. So a
        // "simultaneous" mode on the non-gated lane would be a name with nothing behind it; see
        // `DecideMode::Simultaneous`.
        let gated = self.core.cash_gate || self.core.decide.is_simultaneous();
        self.strategy.on_start(&mut self.core);
        for i in 0..self.core.n {
            self.core.step = i;
            self.core.now = if n_sym > 0 { self.core.bars[0][i].ts } else { 0 };
            // symbols with sub-bars THIS step (non-gated path only)
            granular.clear();
            granular.extend((0..n_sym).map(|si| !gated && !self.core.sym[si].sub[i].is_empty()));
            // opt-in binary-resolution settlement, BEFORE stops/fills (no-op when unconfigured)
            self.check_resolutions(i);
            // opt-in option-expiry settlement, likewise BEFORE stops/fills (no-op when unconfigured)
            self.check_option_expiries(i);
            // opt-in stale-price freshness bookkeeping, BEFORE the fill phase so a bar that IS a
            // fresh print has age 0 and fills on the spot (see `vike_fills::staleness`). Guarded so
            // the default path is a single branch per step, not per symbol.
            if self.core.max_price_staleness_ms.is_some() {
                for si in 0..n_sym {
                    // scalars only — no Bar clone, and no borrow of core held across the write
                    let (ts, fresh) = {
                        let b = &self.core.bars[si][i];
                        (b.ts, vike_fills::staleness::is_fresh_print(b))
                    };
                    self.core.note_print_at(si, ts, fresh);
                }
            }
            self.check_protective_stops(i, &granular);
            if gated {
                self.fill_step_gated(i);
            } else {
                for si in 0..n_sym {
                    if granular[si] {
                        self.fill_pending_granular(si, i);
                    } else {
                        self.fill_pending(si, i);
                    }
                }
            }
            self.core.index = i;
            for si in 0..n_sym {
                // ABLATION: read Copy scalars instead of cloning the Bar (no String alloc)
                let (bclose, bfunding, bhigh, blow, bts) = {
                    let b = &self.core.bars[si][i];
                    (b.close, b.funding, b.high, b.low, b.ts)
                };
                self.core.sym[si].price = bclose;
                // Funding rate for this step: a recorded `Bar.funding` is the on-the-bar truth and
                // WINS; otherwise the opt-in `funding_source` seam (venue funding history) is
                // consulted — `None` (no source / no venue / no funding event at this ts) leaves
                // the path byte-identical to the pre-seam engine. See `EngineParams::funding_source`.
                let funding_rate = match bfunding {
                    Some(f) => Some(f),
                    None => self.core.funding_rate_for(si, bts),
                };
                if let Some(funding) = funding_rate
                    && self.core.sym[si].pos.size != 0.0
                {
                    let mult = self.core.multiplier_of_idx(si);
                    let fc = funding_charge(self.core.sym[si].pos.size, bclose, funding, mult);
                    self.core.cash -= fc;
                    // Observability twin of `Account.funding_paid`: net funding cashflow =
                    // Σ(-fc) = Σ MirrorFunding.amount (R4-gated). Does NOT touch cash/equity.
                    self.core.funding_paid -= fc;
                    if fc != 0.0
                        && let Some(sink) = &mut self.core.mirror_funding
                    {
                        sink.push(MirrorFunding {
                            symbol: self.core.symbols[si].clone(),
                            amount: -fc,
                            ts: bts,
                        });
                    }
                }
                // MAE/MFE extremes for open positions (this bar's high/low; no look-ahead)
                if self.core.sym[si].pos.size != 0.0 {
                    self.core.sym[si].hi_since = self.core.sym[si].hi_since.max(bhigh);
                    self.core.sym[si].lo_since = self.core.sym[si].lo_since.min(blow);
                }
            }
            // Opt-in variation-margin settlement (no-op when unconfigured). Placed AFTER the
            // price/funding loop above — the mark must be this step's close and the funding charge
            // for the step must already have been taken — and BEFORE close_inactive/liquidation, so
            // a forced close in this same step exits against the freshly settled cost basis.
            self.core.settle_variation_margin(self.core.now);
            self.close_inactive(i);
            self.check_liquidation(i);
            let ts = if n_sym > 0 { self.core.bars[0][i].ts } else { 0 };
            if i >= self.warmup {
                // Strategy._on_step: fan the bundle out per symbol, in CROSS-SECTION order —
                // `symbols` order under the default `DecideMode::Sequential`, so this loop is the
                // frozen one index for index.
                //
                // ⚠ The ENGINE's own reads do not move during this fan-out (no fill lands here,
                // so `cash`, `equity_now` and `open_risk` are the same for every symbol in the
                // step), which is why re-ordering it is not what closes the list-order defect —
                // the folds and the allocator are. It is re-ordered anyway because a STRATEGY may
                // carry cross-sectional state of its own (a rank, a budget it hands out, a "first
                // signal this bar wins" latch), and leaving the fan-out in list order would keep
                // a hidden argv dependency alive on the one side the engine cannot see. Under
                // `Simultaneous` the sequence a strategy observes is a property of the universe.
                // Rc::clone is a refcount bump (no Bar/String copy); `series` is a local owning
                // handle, so &series[i] doesn't borrow core -> &mut core coexists, and bars_for/
                // forming_for still read the live self.core.bars.
                for k in 0..n_sym {
                    let si = self.core.cross_section()[k];
                    let series = Rc::clone(&self.core.bars[si]);
                    self.strategy.on_bar(&mut self.core, &series[i]);
                }
                let due = self.core.schedule.check_due(ts, i);
                for tag in due {
                    self.strategy.on_schedule(&mut self.core, &tag);
                }
            }
            let eq = self.core.equity_now();
            self.core.equity_peak = self.core.equity_peak.max(eq);
            equity_curve.push(eq);
            equity_ts.push(ts);
            for si in 0..n_sym {
                let st = &self.core.sym[si];
                per_symbol_curve[si].push(
                    st.realized
                        + st.pos.size
                            * (st.price - st.pos.avg_price)
                            * self.core.multiplier_of_idx(si),
                );
            }
        }
        // END-OF-RUN binary-resolution sweep (no-op when unconfigured): the tape may stop before
        // the market resolves, which is the normal prediction-market shape. Settle at each
        // symbol's LAST bar ts; when anything settled, re-point the curves' final sample at the
        // post-settlement equity so `equity_curve.last()` still agrees with `final_equity` (the
        // sample COUNT never changes — no extra point, no ts shift).
        if self.core.resolution.is_some() {
            let last_ts: Vec<i64> = (0..n_sym)
                .map(|si| if self.core.n > 0 { self.core.bars[si][self.core.n - 1].ts } else { 0 })
                .collect();
            if self.final_resolution_sweep(&last_ts) {
                if let Some(last) = equity_curve.last_mut() {
                    *last = self.core.equity_now();
                }
                for si in 0..n_sym {
                    if let Some(last) = per_symbol_curve[si].last_mut() {
                        let st = &self.core.sym[si];
                        *last = st.realized
                            + st.pos.size
                                * (st.price - st.pos.avg_price)
                                * self.core.multiplier_of_idx(si);
                    }
                }
            }
        }
        // END-OF-RUN option-expiry sweep (no-op when unconfigured): the tape may stop before an
        // option's expiry_ms (see `final_option_expiry_sweep`). Same curve re-point as the binary
        // sweep above so `equity_curve.last()` stays equal to `final_equity` (sample COUNT
        // unchanged) whenever something settled.
        if self.core.option_specs.is_some() {
            let last_ts: Vec<i64> = (0..n_sym)
                .map(|si| if self.core.n > 0 { self.core.bars[si][self.core.n - 1].ts } else { 0 })
                .collect();
            if self.final_option_expiry_sweep(&last_ts) {
                if let Some(last) = equity_curve.last_mut() {
                    *last = self.core.equity_now();
                }
                for si in 0..n_sym {
                    if let Some(last) = per_symbol_curve[si].last_mut() {
                        let st = &self.core.sym[si];
                        *last = st.realized
                            + st.pos.size
                                * (st.price - st.pos.avg_price)
                                * self.core.multiplier_of_idx(si);
                    }
                }
            }
        }
        self.strategy.on_stop(&mut self.core);
        let per_symbol_pnl: Vec<(String, f64)> = (0..n_sym)
            .map(|si| {
                let st = &self.core.sym[si];
                (
                    self.core.symbols[si].clone(),
                    st.realized
                        + st.pos.size
                            * (st.price - st.pos.avg_price)
                            * self.core.multiplier_of_idx(si),
                )
            })
            .collect();
        BacktestResult {
            n_trades: self.core.trades.len(),
            trades: self.core.trades.clone(),
            equity_curve,
            final_equity: self.core.equity_now(),
            per_symbol_pnl,
            per_symbol_curves: self.core.symbols.iter().cloned().zip(per_symbol_curve).collect(),
            equity_ts,
            intrabar_both_hit: self.core.intrabar_both_hit,
            stale_deferrals: self.core.stale_deferrals,
            impact_unpriced: self.core.impact_unpriced,
            session_deferrals: self.core.session_deferrals,
            // Already-accumulated diagnostics mirrored out for the zero-trade analyzer (a clone
            // after the fold loop, not new hot-path counting — see BacktestResult's field docs).
            dropped: self.core.dropped.clone(),
            below_min_reversals: self.core.below_min_reversals,
            warmup: self.warmup,
            funding_paid: self.core.funding_paid,
            // The realised maker/taker mix and the commission total, mirrored off the engine with
            // the diagnostics above — a stamp on a ranked answer cannot be read off the equity
            // curve, and this is the run that actually charged it.
            maker_fills: self.core.maker_fills,
            taker_fills: self.core.taker_fills,
            fees_paid: self.core.fees_paid,
        }
    }
}
