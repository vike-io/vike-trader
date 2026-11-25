//! The bar-path run loop (`StrategyEngine::run`).

use std::rc::Rc;

use vike_model::Strategy;

use super::{
    LedgerEventKind, MirrorFunding, OrderBrokerSimulator, OrderEmulationMarketHook, StrategyEngine,
};
use vike_analytics::{TradingRecord, TradingRecordSimulator, TradingRecordSource};

impl<S: Strategy<OrderBrokerSimulator>> StrategyEngine<S> {
    pub fn run(&mut self) -> TradingRecord {
        let n_symbol = self.core.symbols.len();
        let mut equity_curve: Vec<f64> = Vec::with_capacity(self.core.n);
        let mut equity_ts: Vec<i64> = Vec::with_capacity(self.core.n);
        let mut per_symbol_curve: Vec<Vec<f64>> = vec![Vec::with_capacity(self.core.n); n_symbol];
        // Reused across bars: the per-symbol "has sub-bars this step" flags. Refilled in place each
        // bar (clear + extend keeps the capacity), so there's no per-bar Vec malloc/free.
        let mut granular: Vec<bool> = Vec::with_capacity(n_symbol);
        // THE SHARED-CASH ADMISSION PHASE is reached by either door, and this is the ONE place
        // the disjunction is spelled: `StrategyEngineConfig::decide = Simultaneous` IMPLIES it, because
        // `fill_step_gated` is the only lane in this engine where a cross-section exists to be
        // ordered at all — the per-symbol `fill_pending` loop consumes the open-position caps in
        // list order inside `dispatch_fill` and offers no site where a ranking could apply. So a
        // "simultaneous" mode on the non-gated lane would be a name with nothing behind it; see
        // `DecideMode::Simultaneous`.
        let gated = self.core.cash_gate || self.core.decide.is_simultaneous();
        // The run's last decision, the last bar's close: where each input's whole series ends
        // (`OrderBroker::input_whole`; a no-op unless the run serves it).
        if n_symbol > 0 && self.core.n > 0 {
            let end = self.core.inputs.close_of(self.core.bars[0][self.core.n - 1].timestamp);
            self.core.inputs.end_at(end);
        }
        // The test build logs EVERY bar run's cash and position events and rebuilds its curve from them at the
        // end, bit for bit (`ledger_events.rs`): a new mutator that does not log reddens this crate's suites.
        #[cfg(any(test, feature = "test-support"))]
        let ledger_start = self.core.start_ledger_log();
        self.strategy.on_start(&mut self.core);
        for i in 0..self.core.n {
            self.core.step = i;
            self.core.note_bar_start();
            self.core.set_now(if n_symbol > 0 { self.core.bars[0][i].timestamp } else { 0 });
            // The inputs as of this bar's OPEN for its fills (decision 0271; no-op with none).
            self.core.inputs.advance_to(self.core.now);
            // ...and the rate of each converted symbol as of this bar's OPEN, for everything the step
            // books, marks and sizes (decision 0243; no-op with no conversion).
            self.core.advance_quote_rates(self.core.now);
            // The scheduled funding due by this bar, FIRST: on the position held at the funding
            // instant, which is the one from before this step's stops and fills, at the venue's mark
            // or else this bar's open (`StrategyEngineConfig::funding_schedule`). Empty schedule: no-op.
            if !self.core.funding_schedule.is_empty() {
                for si in 0..n_symbol {
                    let (ts, open) = {
                        let b = &self.core.bars[si][i];
                        (b.timestamp, b.open)
                    };
                    self.core.charge_funding_schedule(si, ts, open);
                }
            }
            // symbols with sub-bars THIS step (non-gated path only)
            granular.clear();
            granular
                .extend((0..n_symbol).map(|si| !gated && !self.core.symbol[si].sub[i].is_empty()));
            // opt-in binary-resolution settlement, BEFORE stops/fills (no-op when unconfigured)
            self.check_resolutions(i);
            // opt-in option-expiry settlement, likewise BEFORE stops/fills (no-op when unconfigured)
            self.check_option_expiries(i);
            // opt-in stale-price freshness bookkeeping, BEFORE the fill phase so a bar that IS a
            // fresh print has age 0 and fills on the spot (see `vike_fills::staleness`). Guarded so
            // the default path is a single branch per step, not per symbol.
            if self.core.max_price_staleness_ms.is_some() {
                for si in 0..n_symbol {
                    // scalars only — no Bar clone, and no borrow of core held across the write
                    let (ts, fresh) = {
                        let b = &self.core.bars[si][i];
                        (b.timestamp, vike_fills::staleness::is_fresh_print(b))
                    };
                    self.core.note_print_at(si, ts, fresh);
                }
            }
            self.check_protective_stops(i, &granular);
            if gated {
                // `InBarRules::fill_at_close`: what the strategy left on the bar before meets that bar's
                // close first, as the bar lane walks it at the head of this step (no-op with the rule off)
                if self.core.in_bar.rules.fill_at_close && i > 0 {
                    self.fill_step_gated_at_close(i - 1);
                }
                self.fill_step_gated(i);
            } else {
                for si in 0..n_symbol {
                    if granular[si] {
                        self.fill_pending_granular(si, i);
                    } else {
                        self.fill_pending(si, i);
                    }
                }
            }
            self.core.close_points_passed();
            // the resting combos, on this step's bars (no-op while none rests)
            self.core.note_combo_step(i);
            self.sweep_combos();
            self.core.index = i;
            for si in 0..n_symbol {
                // ABLATION: read Copy scalars instead of cloning the Bar (no String alloc)
                let (bclose, bfunding, bhigh, blow, bts) = {
                    let b = &self.core.bars[si][i];
                    (b.close, b.funding, b.high, b.low, b.timestamp)
                };
                self.core.symbol[si].price = bclose;
                // Funding rate for this step: a recorded `Bar.funding` is the on-the-bar truth and
                // WINS; otherwise the opt-in `funding_source` seam (venue funding history) is
                // consulted — `None` (no source / no venue / no funding event at this ts) leaves
                // the path byte-identical to the pre-seam engine. See `StrategyEngineConfig::funding_source`.
                // A symbol on a funding schedule was charged at the top of the step; its
                // `Bar.funding` is the strategy's to read, not a second charge.
                let funding_rate = match bfunding {
                    _ if self.core.has_funding_schedule(si) => None,
                    Some(f) => Some(f),
                    None => self.core.funding_rate_for(si, bts),
                };
                if let Some(funding) = funding_rate
                    && self.core.symbol[si].pos.size != 0.0
                {
                    let value = self.core.value_of_index(si);
                    let fc = value.funding(self.core.symbol[si].pos.size, bclose, funding);
                    self.core.cash -= fc;
                    self.core.log_ledger(si, LedgerEventKind::Funding { amount: fc });
                    // Observability twin of `Account.funding_paid`: net funding cashflow =
                    // Σ(-fc) = Σ MirrorFunding.amount (R4-gated). Does NOT touch cash/equity.
                    self.core.funding_paid -= fc;
                    if fc != 0.0
                        && let Some(sink) = &mut self.core.mirror_funding
                    {
                        sink.push(MirrorFunding {
                            symbol: self.core.symbols[si].clone(),
                            amount: -fc,
                            timestamp: bts,
                        });
                    }
                }
                // MAE/MFE extremes for open positions (this bar's high/low; no look-ahead)
                if self.core.symbol[si].pos.size != 0.0 {
                    self.core.symbol[si].hi_since = self.core.symbol[si].hi_since.max(bhigh);
                    self.core.symbol[si].lo_since = self.core.symbol[si].lo_since.min(blow);
                }
            }
            // Opt-in variation-margin settlement (no-op when unconfigured). Placed AFTER the
            // price/funding loop above — the mark must be this step's close and the funding charge
            // for the step must already have been taken — and BEFORE close_inactive/liquidation, so
            // a forced close in this same step exits against the freshly settled cost basis.
            self.core.settle_variation_margin(self.core.now);
            self.close_inactive(i);
            self.check_liquidation(i);
            // a portfolio run's mount budgets at this close, after the margin call and before any
            // strategy steps, as the live core sweeps them (no-op on a plain run)
            self.core.sweep_mount_budgets();
            let ts = if n_symbol > 0 { self.core.bars[0][i].timestamp } else { 0 };
            // ...and as of its CLOSE for the decision: an input bar closing with this bar is seen.
            let close = self.core.inputs.close_of(ts);
            self.core.inputs.advance_to(close);
            // The emulated orders' laws on this bar, after its fills and before the strategy's hook.
            for si in 0..n_symbol {
                if !self.core.symbol[si].emulated.is_empty() {
                    let series = Rc::clone(&self.core.bars[si]);
                    self.core.drive_emulated(si, &OrderEmulationMarketHook::Bar(&series[i]));
                }
            }
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
                // handle, so &series[i] doesn't borrow core -> &mut core coexists, and bars_for
                // still reads the live self.core.bars.
                // The ends of the orders the strategy placed through the order seam, as live sends
                // them (no-op, nothing allocated, for a run that placed none).
                self.deliver_order_ends();
                let published = self.core.forecasts.len();
                for k in 0..n_symbol {
                    let si = self.core.cross_section()[k];
                    // a portfolio run hands the bar to the mounts that trade this slot (`crate::mounted`)
                    self.core.dispatching_slot = si;
                    let series = Rc::clone(&self.core.bars[si]);
                    self.strategy.on_bar(&mut self.core, &series[i]);
                }
                // The forecasts of this bar step were DECIDED at its close (decision 0271), as live stamps
                // them (`vike_core`'s `journal_bar_forecasts`); `now`, which its orders are judged by, stays
                // the open. A run that states no bar length stamps the open, as before.
                for row in &mut self.core.forecasts[published..] {
                    row.timestamp = close;
                }
                // the in-bar rules on the book the strategy left and the next walk (no-op if off)
                self.core.after_decide(i);
                let due = self.core.schedule.check_due(ts, i);
                for tag in due {
                    self.strategy.on_schedule(&mut self.core, &tag);
                }
            }
            self.core.mark_closed_trades(i, ts);
            let eq = self.core.equity_now();
            self.core.equity_peak = self.core.equity_peak.max(eq);
            equity_curve.push(eq);
            equity_ts.push(ts);
            for si in 0..n_symbol {
                per_symbol_curve[si].push(self.core.symbol_pnl(si));
            }
            // a portfolio run's mounts' PnL beside it (no-op on a plain run)
            self.core.sample_mount_curves(false);
        }
        // END-OF-RUN binary-resolution sweep (no-op when unconfigured): the tape may stop before
        // the market resolves, which is the normal prediction-market shape. Settle at each
        // symbol's LAST bar ts; when anything settled, re-point the curves' final sample at the
        // post-settlement equity so `equity_curve.last()` still agrees with `final_equity` (the
        // sample COUNT never changes — no extra point, no ts shift).
        if self.core.resolution.is_some() {
            let last_timestamp: Vec<i64> = (0..n_symbol)
                .map(|si| {
                    if self.core.n > 0 { self.core.bars[si][self.core.n - 1].timestamp } else { 0 }
                })
                .collect();
            if self.final_resolution_sweep(&last_timestamp) {
                if let Some(last) = equity_curve.last_mut() {
                    *last = self.core.equity_now();
                }
                for si in 0..n_symbol {
                    if let Some(last) = per_symbol_curve[si].last_mut() {
                        *last = self.core.symbol_pnl(si);
                    }
                }
                self.core.sample_mount_curves(true);
            }
        }
        // END-OF-RUN option-expiry sweep (no-op when unconfigured): the tape may stop before an
        // option's expiry_ms (see `final_option_expiry_sweep`). Same curve re-point as the binary
        // sweep above so `equity_curve.last()` stays equal to `final_equity` (sample COUNT
        // unchanged) whenever something settled.
        if self.core.option_specs.is_some() {
            let last_timestamp: Vec<i64> = (0..n_symbol)
                .map(|si| {
                    if self.core.n > 0 { self.core.bars[si][self.core.n - 1].timestamp } else { 0 }
                })
                .collect();
            if self.final_option_expiry_sweep(&last_timestamp) {
                if let Some(last) = equity_curve.last_mut() {
                    *last = self.core.equity_now();
                }
                for si in 0..n_symbol {
                    if let Some(last) = per_symbol_curve[si].last_mut() {
                        *last = self.core.symbol_pnl(si);
                    }
                }
                self.core.sample_mount_curves(true);
            }
        }
        self.strategy.on_stop(&mut self.core);
        self.core.note_kept_ticks();
        self.core.stamp_drops();
        #[cfg(any(test, feature = "test-support"))]
        self.core.assert_ledger_rebuilds(&ledger_start, &equity_curve);
        let per_symbol_pnl: Vec<(String, f64)> = (0..n_symbol)
            .map(|si| (self.core.symbols[si].clone(), self.core.symbol_pnl(si)))
            .collect();
        TradingRecord {
            n_trades: self.core.trades.len(),
            trades: self.core.trades.clone(),
            equity_curve,
            final_equity: self.core.equity_now(),
            per_symbol_pnl,
            per_symbol_curves: self.core.symbols.iter().cloned().zip(per_symbol_curve).collect(),
            equity_timestamp: equity_ts,
            funding_paid: self.core.funding_paid,
            // The commission total, mirrored off the engine with the counters below — a stamp on a
            // ranked answer cannot be read off the equity curve, and this is the run that charged it.
            fees_paid: self.core.fees_paid,
            settle_price: self.core.settle_price(),
            // The engine's own findings (an option it could not settle); the harness appends the store's.
            data_warnings: self.core.data_warnings.clone(),
            closed_trades: self.core.closed_trade_metrics(),
            forecasts: self.core.forecasts.clone(),
            // One point a bar: the curve is the path.
            sampling: None,
            // A portfolio's mount rows are the harness's to fill (they name the profile's files).
            mounts: None,
            // What the strategy's hooks failed with, read once, after `on_stop` (a portfolio's mounts
            // together).
            strategy_failures: self.strategy.failures(),
            source: TradingRecordSource::Backtest(TradingRecordSimulator {
                intrabar_both_hit: self.core.intrabar_both_hit,
                stale_deferrals: self.core.stale_deferrals,
                impact_unpriced: self.core.impact_unpriced,
                session_deferrals: self.core.session_deferrals,
                // Already-accumulated diagnostics mirrored out for the zero-trade analyzer (a clone
                // after the fold loop, not new hot-path counting — see TradingRecordSimulator's docs).
                dropped: self.core.dropped.clone(),
                dropped_at: self.core.dropped_at.clone(),
                below_min_reversals: self.core.below_min_reversals,
                warmup: self.warmup,
                // The realised maker/taker mix, mirrored the same way.
                maker_fills: self.core.maker_fills,
                taker_fills: self.core.taker_fills,
            }),
        }
    }
}
