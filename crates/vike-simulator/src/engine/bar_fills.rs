//! Bar-path fill passes: coarse, granular sub-bar and shared-cash gated.

use std::rc::Rc;

use vike_marketdata::Bar;
use vike_model::{OrderDrop, OrderKind, OrderResting, Strategy};

use super::{OrderBrokerSimulator, OrderGate, StrategyEngine, kind_str, unfilled_limit};

impl<S: Strategy<OrderBrokerSimulator>> StrategyEngine<S> {
    /// Coarse per-symbol fill phase (`_fill_pending`).
    pub(super) fn fill_pending(&mut self, si: usize, current_index: usize) {
        // Most bars have no pending orders. Python `_fill_pending` is a no-op on empty pending
        // (empty loop, reassign empty list), so returning early is behavior-identical — and it
        // skips the per-symbol Bar clone that otherwise runs every single bar. The maker law's
        // quotes rest in `pending` too and take their own pass first; a book of quotes alone ends
        // there, as an empty book does.
        let st = &self.core.symbol[si];
        if st.pending.is_empty() && st.held.is_empty() {
            return;
        }
        let series = Rc::clone(&self.core.bars[si]); // refcount bump, not a Bar/String clone
        let bar = &series[current_index];
        // held orders whose release has come join `pending` first, so this bar is their first event
        self.core.release_held(si, bar.timestamp);
        self.fill_tagged(si, bar);
        if !self.core.rests_beside_quotes(si) {
            return;
        }
        let mut triggered: Vec<(OrderResting, f64)> = Vec::new();
        let mut still: Vec<OrderResting> = Vec::new();
        let pending = std::mem::take(&mut self.core.symbol[si].pending);
        for mut o in pending {
            // a quote (`OrderBrokerSimulatorSymbolState::quotes`) took its own pass above
            if self.core.is_quote(si, o.qid) {
                still.push(o);
                continue;
            }
            // THE gate sequence, shared with every other lane (see `gate_pending`). A deferred /
            // stop-released / untriggered order keeps resting at this bar's own ts.
            match self.gate_pending(si, &mut o, bar) {
                OrderGate::Rest => still.push(o),
                OrderGate::Trigger(fp) => triggered.push((o, fp)),
                OrderGate::End => {}
            }
        }
        self.core.symbol[si].pending = still;
        self.core.open_group_pass(si, &triggered);
        let resolved = self.resolve_triggered(si, triggered, bar);
        for (o, fp) in resolved {
            if o.size <= 1e-12 {
                continue;
            }
            self.dispatch_fill(si, &o, fp, bar.volume, bar.timestamp);
        }
        self.core.close_group_pass();
    }

    /// Granular sub-bar fill phase (`_fill_pending_granular`) — and, since
    /// `crates/vike-backtest/src/harness/run.rs`'s `load_profile_detail_bars` gave it a profile
    /// door (`data.detail_interval`), the engine's answer to the ONE thing a coarse bar cannot
    /// say.
    ///
    /// # What this lane resolves that [`Self::fill_pending`] can only guess
    ///
    /// A bar carries open/high/low/close and no PATH, so when a stop and a target both sit inside
    /// it, OHLC cannot say which was touched first. [`Self::fill_pending`] hands the triggered
    /// pair to `vike_fills::resolve_intrabar_fills`, which orders them ADVERSE-FIRST and caps the
    /// total reduction to the position — the pessimistic bound, not the truth — and counts the bar
    /// in `OrderBrokerSimulator::intrabar_both_hit`. This lane instead walks the step's sub-bars IN TIME
    /// ORDER and fills whichever level the finer series actually reaches first, so the sequence is
    /// OBSERVED. It never calls `resolve_intrabar_fills` at all.
    ///
    /// ⚠ **So `intrabar_both_hit` does not increment for a symbol that has a detail series, no
    /// matter how ambiguous its bars were.** Read that counter as "bars whose order the engine had
    /// to guess", never "ambiguous bars": with a detail tape on, a `0` says the guessing stopped,
    /// not that the ambiguity was absent. Comparing the two configurations is a PnL comparison.
    ///
    /// ⚠ **It is a finer PRINT tier, not only a finer FILL tier**, so turning the tape on can move
    /// a run that never had an ambiguous bracket at all: `note_print` is called per sub-bar (the
    /// opt-in staleness discipline therefore measures age against the sub-bar, and a fully-printing
    /// step never defers where a quiet stretch WITHIN a step does), [`Self::check_stop`] is
    /// evaluated once per sub-bar, and the coarse lane's `size <= 1e-12` dust guard is deliberately
    /// absent here — preserved from the frozen engine rather than chosen.
    ///
    /// ⚠ **KNOWN HOLE, stated because a profile key can now reach this lane: the MAKER LAW's pass
    /// is not run here.** [`Self::fill_pending`] calls [`Self::fill_tagged`] before its pending
    /// loop and this function has no equivalent call, so a resting quote (`OrderBrokerSimulatorSymbolState::quotes`:
    /// a tagged plain limit under `StrategyEngineConfig::optimistic_quotes`, an emulated order's child) on a
    /// symbol that HAS a detail tape never fills at all — it is not filled late, it is not filled on
    /// the coarse bar either, because the coarse lane is skipped for that symbol on that step. That
    /// predates the tape (the lane was unreachable from any profile), but the tape is what makes it
    /// reachable. So: `data.detail_interval` is for STOP/TARGET realism on a directional strategy,
    /// and a maker backtest belongs on the tick lane's `engine.queue_model` (whose
    /// `fill_tagged_queued` is the queue-gated twin of the maker pass). Closing it means giving this
    /// lane a `fill_tagged` call per sub-bar, which is a fill-behaviour change to a frozen path and
    /// needs its own proof.
    ///
    /// **Orders and cancels from `on_fill` take effect here**, because `pending` is written back
    /// before a sub-bar's first fill, as [`Self::fill_pending`] does: an order submitted from
    /// `on_fill` rests from the NEXT sub-bar, and a cancel removes what still rests. An order that
    /// already triggered on the same sub-bar is past a cancel's reach, on this lane as on that one.
    pub(super) fn fill_pending_granular(&mut self, si: usize, i: usize) {
        let sub_bars: Vec<Bar> = self.core.symbol[si].sub[i].clone();
        for (k, sub) in sub_bars.iter().enumerate() {
            let last_sub = k + 1 == sub_bars.len();
            // 0) Wait discipline (opt-in): a sub-bar is a FIRST-CLASS print, not merely intrabar
            // detail of the step's coarse verdict. Granular data is the finest tier available on
            // this lane, so its own volume/quote evidence is the best print evidence there is —
            // and the age below is measured against `sub.ts`, so recording only the coarse ts
            // would make every later sub-bar of a fully-fresh step spuriously stale. No-op unless
            // configured (see `vike_fills::staleness`).
            self.core.note_print(si, sub);
            // 1) protective stop first within this sub-bar (breach cancels the OCO sibling exit)
            self.check_stop(si, sub.open, sub.low, sub.high, sub.timestamp);
            // held orders whose release has come join `pending` at the first sub-bar at or after it
            self.core.release_held(si, sub.timestamp);
            // 2) resting/market orders that trigger on THIS sub-bar, in pending order.
            // NOTE: no dust guard here (unlike the coarse/tick lanes) — preserved from the original.
            let pending = std::mem::take(&mut self.core.symbol[si].pending);
            let mut still: Vec<OrderResting> = Vec::new();
            let mut triggered: Vec<(OrderResting, f64)> = Vec::new();
            for mut o in pending {
                // A closing auction is the coarse bar's close: its LAST sub-bar's, never an
                // earlier one's (`vike_model::order_fill_price_granular`'s rule). An order that does
                // not meet the bar before's close (`InBarRules::fill_at_close`) rests through it. A
                // quote (`OrderBrokerSimulatorSymbolState::quotes`) never fills on this lane: the KNOWN HOLE above.
                if self.core.is_quote(si, o.qid)
                    || (o.kind == OrderKind::AuctionClose && !last_sub)
                    || !self.core.meets_close_point(si, k, &o)
                {
                    still.push(o);
                    continue;
                }
                // THE gate sequence (see `gate_pending`), run at `sub.ts`: the staleness age is
                // measured against the last fresh print, which the `note_print` above may have
                // just set to THIS sub-bar (age 0). A fully-printing granular step therefore
                // never defers; a quiet stretch WITHIN a step does, which is the whole point of
                // having the finer tier. The stop-release arm likewise fills the NEXT sub-bar.
                match self.gate_pending(si, &mut o, sub) {
                    OrderGate::Rest => still.push(o),
                    OrderGate::Trigger(fp) => triggered.push((o, fp)),
                    OrderGate::End => {}
                }
            }
            // Before the first fill: a write-back after it would overwrite what `on_fill` did. The
            // gate reads no position, cash or `pending`, so the fills are the same ones in the same
            // order as when each one was dispatched inside the loop above.
            self.core.symbol[si].pending = still;
            self.core.open_group_pass(si, &triggered);
            self.order_group_entries(&mut triggered, sub);
            for (o, fp) in triggered {
                self.dispatch_fill(si, &o, fp, sub.volume, sub.timestamp);
            }
            self.core.close_group_pass();
            // opt-in `StrategyEngineConfig::shortfall_cover`: the sub-bar that just filled ended at its close, a margin checkpoint
            if self.core.shortfall_cover.is_some() {
                self.cover_at_node(si, sub.close, sub.timestamp);
            }
        }
    }

    /// Shared-cash gated fill phase (`_fill_step_gated`): reductions first, opens by
    /// weight desc, unfundable dropped.
    ///
    /// **This is the engine's one cross-sectional allocator** — it collects the whole step's
    /// triggered orders across every symbol before admitting any of them, which is what makes a
    /// joint decision possible here and nowhere else. The TIE-BREAK is therefore the decision,
    /// and [`StrategyEngineConfig::decide`] chooses it: trigger order (the frozen answer, which is the
    /// order the symbols were listed) or the cross-section rank (the symbol's own name). See
    /// [`DecideMode`] for why the tie-break is not a detail — no strategy in this tree sets
    /// `weight`, so every order ties and the tie-break decides everything.
    pub(super) fn fill_step_gated(&mut self, current_index: usize) {
        let mut opens: Vec<(usize, OrderResting, f64, usize)> = Vec::new();
        let mut frees: Vec<(usize, OrderResting, f64, usize)> = Vec::new();
        let mut seq = 0usize;
        for si in 0..self.core.symbols.len() {
            let bar = self.core.bars[si][current_index].clone();
            self.core.release_held(si, bar.timestamp);
            let pending = std::mem::take(&mut self.core.symbol[si].pending);
            let mut still: Vec<OrderResting> = Vec::new();
            let mut triggered: Vec<(OrderResting, f64)> = Vec::new();
            for mut o in pending {
                // a quote (`OrderBrokerSimulatorSymbolState::quotes`) never fills on this lane, as the granular lane's
                if self.core.is_quote(si, o.qid) {
                    still.push(o);
                    continue;
                }
                // THE gate sequence (see `gate_pending`), run BEFORE the cash gate — an order the
                // disciplines deferred, or the emulator released as a stop child, was never
                // triggered, so it must not consume an open slot or cash budget this step.
                match self.gate_pending(si, &mut o, &bar) {
                    OrderGate::Trigger(fp) => triggered.push((o, fp)),
                    OrderGate::Rest => still.push(o),
                    OrderGate::End => {}
                }
            }
            self.core.open_group_pass(si, &triggered);
            self.order_group_entries(&mut triggered, &bar);
            for (o, fp) in triggered {
                let pos = self.core.symbol[si].pos;
                // A reduce-only order is never an open: it takes the reductions' path, where the
                // fill caps it to the position (or drops it) at its turn.
                let increasing =
                    !o.reduce_only && !vike_model::is_reducing_direction(o.side, pos.size);
                if increasing && !self.core.is_active_index(si) {
                    continue; // inactive member: dropped before the cash gate
                }
                if increasing {
                    opens.push((si, o, fp, seq));
                } else {
                    frees.push((si, o, fp, seq));
                }
                seq += 1;
            }
            self.core.symbol[si].pending = still;
        }
        // THE CROSS-SECTION RANK — `symbols` position -> position in `OrderBrokerSimulator::cross_section`,
        // built once for this step and used as the tie-break of BOTH phases below.
        //
        // Empty (and every read of it skipped) under `DecideMode::Sequential`, so neither phase
        // allocates and both keep their frozen collection-order sequence.
        let ranked = self.core.decide.is_simultaneous();
        let rank: Vec<usize> = if ranked {
            let cs = self.core.cross_section();
            // `cs` is a permutation of `0..n`, so every symbol index is assigned exactly once
            // and no entry is left at the fill value.
            let mut r = vec![0usize; cs.len()];
            for (pos, &si) in cs.iter().enumerate() {
                r[si] = pos;
            }
            r
        } else {
            Vec::new()
        };
        // Reductions/closes first — free cash, never gated.
        //
        // ⚠ Their ORDER matters even though none of them can be refused, and this is the subtler
        // half of the list-order defect. Each `apply_fill` moves the shared `cash` and pushes a
        // `Trade`, so the sequence decides (a) the f64 accumulation order of the cash the opens
        // are then judged against — addition is not associative, and the cash gate below is an
        // inequality against that exact value, so an ULP can flip an admission — and (b) the
        // order of `trades`, `mirror_fills` and every `Strategy::on_fill` callback, all of which a
        // report and a fixture compare positionally. Under `Simultaneous` the sequence is the
        // cross-section's; under the default it is the collection order, unchanged.
        if ranked {
            frees.sort_by(|a, b| rank[a.0].cmp(&rank[b.0]).then(a.3.cmp(&b.3)));
        }
        for (si, o, fp, _) in frees {
            // an order-list leg its siblings' fills this step cancelled or reduced
            let Some(size) = self.core.group_leg_size(si, &o) else {
                continue;
            };
            let bar_timestamp = self.core.bars[si][current_index].timestamp;
            let bar_vol = self.core.bars[si][current_index].volume;
            let size = if o.reduce_only {
                let room = self.core.reduce_only_size(si, o.side, size);
                if room <= 0.0 {
                    continue;
                }
                room
            } else {
                size
            };
            let fill_size = self.volume_clamp(si, &o, size, bar_vol, fp);
            if fill_size > 0.0 {
                let (is_maker, bounded) = (o.books_as_maker(), o.is_limit_bounded());
                let mut fees = self.core.order_fees_of(o.qid);
                if let Some(mut fill) = self.core.apply_order_fill(
                    si,
                    o.side,
                    fill_size,
                    fp,
                    o.weight,
                    bar_timestamp,
                    is_maker,
                    bounded,
                    &mut fees,
                ) {
                    let qid = self.rest_remainder(si, &o, size - fill_size, Some(fill.price));
                    self.core.note_order_fees(si, qid, fees);
                    self.core.resolve_group_fill(si, &o, fill.size, fill.price);
                    fill.tag = self.core.tag_of(o.qid);
                    self.fire_on_fill(fill);
                }
            } else {
                self.rest_remainder(si, &o, size, unfilled_limit(&o, fp));
            }
        }
        // Opens/adds: highest weight first. THE TIE-BREAK IS THE CROSS-SECTION DECISION, and
        // under the default mode it is the run's own hidden parameter.
        //
        // `weight` is `OrderResting::weight`, the declared cross-symbol fill priority — and
        // NOTHING in this tree sets it: an untagged `vike_model::OrderBroker::place` passes
        // `0.0`, and so does `OrderBrokerSimulator::strategy_order_target_percent`. So on every strategy
        // that exists here every order ties at `0.0`, and the whole verdict of this allocator —
        // which symbol gets the last of the shared cash and the last open slot — falls to the
        // tie-break.
        //
        // * `DecideMode::Sequential` breaks it on `seq`, the collection order, which is the order
        //   `symbols` was handed to `StrategyEngine::new`, which is the order the operator typed
        //   `data.symbols`. Kept verbatim (Python `sorted` = stable) because it is the frozen
        //   answer every parity fixture pins.
        // * `DecideMode::Simultaneous` breaks it on the CROSS-SECTION RANK — each order's
        //   position in `OrderBrokerSimulator::cross_section`, i.e. its symbol's own name — so permuting
        //   `data.symbols` permutes nothing here. The rank is looked up rather than compared by
        //   name so that this comparator agrees with every fold by construction: both read the
        //   one canonical order, and a future ordering rule changes `cross_section_order` alone.
        opens.sort_by(|a, b| {
            let tie =
                if ranked { rank[a.0].cmp(&rank[b.0]).then(a.3.cmp(&b.3)) } else { a.3.cmp(&b.3) };
            b.1.weight.partial_cmp(&a.1.weight).unwrap_or(std::cmp::Ordering::Equal).then(tie)
        });
        for (si, o, fp, _) in opens {
            // an order-list leg its siblings' fills this step cancelled or reduced
            let Some(size) = self.core.group_leg_size(si, &o) else {
                continue;
            };
            let o = OrderResting { size, ..o };
            let symbol_name = self.core.symbols[si].clone();
            if self.core.symbol[si].pos.size == 0.0 && self.core.at_open_cap() {
                self.core.dropped.push(OrderDrop {
                    symbol: symbol_name,
                    reason: kind_str(o.kind).into(),
                    size: o.size,
                    weight: o.weight,
                });
                continue;
            }
            if self.core.symbol[si].pos.size == 0.0 && o.side > 0 && self.core.at_long_cap() {
                self.core.dropped.push(OrderDrop {
                    symbol: symbol_name,
                    reason: kind_str(o.kind).into(),
                    size: o.size,
                    weight: o.weight,
                });
                continue;
            }
            if self.core.symbol[si].pos.size == 0.0 && o.side < 0 && self.core.at_short_cap() {
                self.core.dropped.push(OrderDrop {
                    symbol: symbol_name,
                    reason: kind_str(o.kind).into(),
                    size: o.size,
                    weight: o.weight,
                });
                continue;
            }
            let bar_timestamp = self.core.bars[si][current_index].timestamp;
            let bar_vol = self.core.bars[si][current_index].volume;
            let fill_size = self.volume_clamp(si, &o, o.size, bar_vol, fp);
            if fill_size <= 0.0 {
                self.rest_remainder(si, &o, o.size, unfilled_limit(&o, fp));
                continue;
            }
            // Maker/taker classification is hoisted ABOVE the cash pre-check because
            // `slippage_for` needs it: the impact addend is taker-only, so the gate must ask the
            // same question `apply_fill` will. Same rule as `dispatch_fill` — by ORDER KIND, not
            // by crossing aggressiveness.
            let (is_maker, bounded) = (o.books_as_maker(), o.is_limit_bounded());
            // Pre-check only: the cash gate reads `OrderBrokerSimulator::fill_terms`, what `apply_fill` is
            // about to book, so it charges the SAME price, size and fee. Otherwise an impact-priced
            // fill, a fill the grid rounds up or a fee schedule could pass and overdraw. Same
            // expressions in the same order with no impact model, no grid and no schedule.
            // `.0` — the skip flag is deliberately DROPPED here. This is the cash pre-check, which
            // prices the same fill `apply_fill` will price again; counting in both would double
            // every entry, exactly as `OrderBrokerSimulator::slippage_saturations`' doc records for its twin.
            let slip = self.core.slippage_for(si, fill_size, fp, bar_timestamp, is_maker).0;
            let mut fees = self.core.order_fees_of(o.qid);
            let t = self.core.fill_terms(
                si,
                o.side,
                fill_size,
                fp,
                slip,
                bar_timestamp,
                is_maker,
                bounded,
                fees,
            );
            // The ledger's own cash leg (`fold_fill`'s): the signed notional, a coin leg on inverse.
            let leg = self.core.value_of_index(si).cash_leg(o.side as f64 * t.size, t.price);
            let cash_impact = -leg - t.fee;
            // A fill the grid refuses moves no cash: `apply_fill` below drops it in its own words.
            if t.size > 0.0 && self.core.cash + cash_impact < 0.0 {
                self.core.dropped.push(OrderDrop {
                    symbol: symbol_name,
                    reason: kind_str(o.kind).into(),
                    size: fill_size,
                    weight: o.weight,
                });
                continue;
            }
            if let Some(mut fill) = self.core.apply_order_fill(
                si,
                o.side,
                fill_size,
                fp,
                o.weight,
                bar_timestamp,
                is_maker,
                bounded,
                &mut fees,
            ) {
                if o.stop.is_some() && self.core.symbol[si].pos.size != 0.0 {
                    self.core.symbol[si].stop = o.stop;
                }
                let qid = self.rest_remainder(si, &o, o.size - fill_size, Some(fill.price));
                self.core.note_order_fees(si, qid, fees);
                self.core.resolve_group_fill(si, &o, fill.size, fill.price);
                fill.tag = self.core.tag_of(o.qid);
                self.fire_on_fill(fill);
            }
        }
        self.core.close_group_pass();
    }
}
