//! Bar-path fill passes: coarse, granular sub-bar and shared-cash gated.

use std::rc::Rc;

use vike_model::{Bar, OrderKind, Strategy, WorkingOrder};

use super::{OrderGate, SimBroker, StrategyEngine, kind_str};
use vike_fills::broker_sim::{adverse_fill_price, fee as fee_fn};
use vike_fills::fill_resolution::resolve_intrabar_fills;

impl<S: Strategy<SimBroker>> StrategyEngine<S> {
    /// Coarse per-symbol fill phase (`_fill_pending`).
    pub(super) fn fill_pending(&mut self, si: usize, cur_idx: usize) {
        // Most bars have no pending orders. Python `_fill_pending` is a no-op on empty pending
        // (empty loop, reassign empty list), so returning early is behavior-identical — and it
        // skips the per-symbol Bar clone that otherwise runs every single bar. The tagged (HFT
        // maker) lane is checked in the SAME breath so the fast path stays byte-identical for every
        // non-HFT strategy (tagged is always empty there → `has_tagged` false → same early return).
        let has_tagged = !self.core.sym[si].tagged.is_empty();
        if self.core.sym[si].pending.is_empty() && !has_tagged {
            return;
        }
        let series = Rc::clone(&self.core.bars[si]); // refcount bump, not a Bar/String clone
        let bar = &series[cur_idx];
        if has_tagged {
            self.fill_tagged(si, bar);
        }
        if self.core.sym[si].pending.is_empty() {
            return;
        }
        let mut triggered: Vec<(WorkingOrder, f64)> = Vec::new();
        let mut still: Vec<WorkingOrder> = Vec::new();
        let pending = std::mem::take(&mut self.core.sym[si].pending);
        for mut o in pending {
            // THE gate sequence, shared with every other lane (see `gate_pending`). A deferred /
            // stop-released / untriggered order keeps resting at this bar's own ts.
            match self.gate_pending(si, &mut o, bar) {
                OrderGate::Rest => still.push(o),
                OrderGate::Trigger(fp) => triggered.push((o, fp)),
            }
        }
        self.core.sym[si].pending = still;
        let resolved = if triggered.len() > 1 {
            let (r, both) = resolve_intrabar_fills(triggered, self.core.sym[si].pos.size);
            self.core.intrabar_both_hit += both;
            r
        } else {
            triggered
        };
        for (o, fp) in resolved {
            if o.size <= 1e-12 {
                continue;
            }
            self.dispatch_fill(si, &o, fp, bar.volume, bar.ts);
        }
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
    /// in `SimBroker::intrabar_both_hit`. This lane instead walks the step's sub-bars IN TIME
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
    /// ⚠ **KNOWN HOLE, stated because a profile key can now reach this lane: the TAGGED maker lane
    /// is not run here.** [`Self::fill_pending`] calls [`Self::fill_tagged`] before its pending
    /// loop and this function has no equivalent call, so a resting TAGGED quote
    /// (`vike_model::HftBroker`) on a symbol that HAS a detail tape never fills at all — it is not
    /// filled late, it is not filled on the coarse bar either, because the coarse lane is skipped
    /// for that symbol on that step. That predates the tape (the lane was unreachable from any
    /// profile), but the tape is what makes it reachable, and it cannot be refused from a profile
    /// because nothing in a profile says whether the strategy quotes with tags. So:
    /// `data.detail_interval` is for STOP/TARGET realism on a directional strategy, and a maker
    /// backtest belongs on the tick lane's `engine.queue_model` (whose `fill_tagged_queued` is the
    /// queue-gated twin of the tagged lane). Closing it means giving this lane a `fill_tagged` call
    /// per sub-bar, which is a fill-behaviour change to a frozen path and needs its own proof.
    pub(super) fn fill_pending_granular(&mut self, si: usize, i: usize) {
        let sub_bars: Vec<Bar> = self.core.sym[si].sub[i].clone();
        for sub in &sub_bars {
            // 0) Wait discipline (opt-in): a sub-bar is a FIRST-CLASS print, not merely intrabar
            // detail of the step's coarse verdict. Granular data is the finest tier available on
            // this lane, so its own volume/quote evidence is the best print evidence there is —
            // and the age below is measured against `sub.ts`, so recording only the coarse ts
            // would make every later sub-bar of a fully-fresh step spuriously stale. No-op unless
            // configured (see `vike_fills::staleness`).
            self.core.note_print(si, sub);
            // 1) protective stop first within this sub-bar (breach cancels the OCO sibling exit)
            self.check_stop(si, sub.open, sub.low, sub.high, sub.ts);
            // 2) resting/market orders that trigger on THIS sub-bar, in pending order.
            // NOTE: no dust guard here (unlike the coarse/tick lanes) — preserved from the original.
            let pending = std::mem::take(&mut self.core.sym[si].pending);
            let mut still: Vec<WorkingOrder> = Vec::new();
            for mut o in pending {
                // THE gate sequence (see `gate_pending`), run at `sub.ts`: the staleness age is
                // measured against the last fresh print, which the `note_print` above may have
                // just set to THIS sub-bar (age 0). A fully-printing granular step therefore
                // never defers; a quiet stretch WITHIN a step does, which is the whole point of
                // having the finer tier. The stop-release arm likewise fills the NEXT sub-bar.
                match self.gate_pending(si, &mut o, sub) {
                    OrderGate::Rest => still.push(o),
                    OrderGate::Trigger(fp) => {
                        self.dispatch_fill(si, &o, fp, sub.volume, sub.ts);
                    }
                }
            }
            self.core.sym[si].pending = still;
        }
    }

    /// Shared-cash gated fill phase (`_fill_step_gated`): reductions first, opens by
    /// weight desc, unfundable dropped.
    ///
    /// **This is the engine's one cross-sectional allocator** — it collects the whole step's
    /// triggered orders across every symbol before admitting any of them, which is what makes a
    /// joint decision possible here and nowhere else. The TIE-BREAK is therefore the decision,
    /// and [`EngineParams::decide`] chooses it: trigger order (the frozen answer, which is the
    /// order the symbols were listed) or the cross-section rank (the symbol's own name). See
    /// [`DecideMode`] for why the tie-break is not a detail — no strategy in this tree sets
    /// `weight`, so every order ties and the tie-break decides everything.
    pub(super) fn fill_step_gated(&mut self, cur_idx: usize) {
        let mut opens: Vec<(usize, WorkingOrder, f64, usize)> = Vec::new();
        let mut frees: Vec<(usize, WorkingOrder, f64, usize)> = Vec::new();
        let mut seq = 0usize;
        for si in 0..self.core.symbols.len() {
            let bar = self.core.bars[si][cur_idx].clone();
            let pending = std::mem::take(&mut self.core.sym[si].pending);
            let mut still: Vec<WorkingOrder> = Vec::new();
            for mut o in pending {
                // THE gate sequence (see `gate_pending`), run BEFORE the cash gate — an order the
                // disciplines deferred, or the emulator released as a stop child, was never
                // triggered, so it must not consume an open slot or cash budget this step.
                let OrderGate::Trigger(fp) = self.gate_pending(si, &mut o, &bar) else {
                    still.push(o);
                    continue;
                };
                let pos = self.core.sym[si].pos;
                let increasing = pos.size == 0.0 || (pos.size > 0.0) == (o.side > 0);
                if increasing && !self.core.is_active_idx(si) {
                    continue; // inactive member: dropped before the cash gate
                }
                if increasing {
                    opens.push((si, o, fp, seq));
                } else {
                    frees.push((si, o, fp, seq));
                }
                seq += 1;
            }
            self.core.sym[si].pending = still;
        }
        // THE CROSS-SECTION RANK — `symbols` position -> position in `SimBroker::cross_section`,
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
        // order of `trades`, `MirrorFill` and every `Strategy::on_fill` callback, all of which a
        // report and a fixture compare positionally. Under `Simultaneous` the sequence is the
        // cross-section's; under the default it is the collection order, unchanged.
        if ranked {
            frees.sort_by(|a, b| rank[a.0].cmp(&rank[b.0]).then(a.3.cmp(&b.3)));
        }
        for (si, o, fp, _) in frees {
            let bar_ts = self.core.bars[si][cur_idx].ts;
            let bar_vol = self.core.bars[si][cur_idx].volume;
            let fill_size = self.volume_clamp(si, o.size, bar_vol);
            if fill_size > 0.0 {
                let is_maker = o.kind == OrderKind::Limit;
                if let Some(fill) =
                    self.core.apply_fill(si, o.side, fill_size, fp, bar_ts, is_maker)
                {
                    self.fire_on_fill(fill);
                }
            }
        }
        // Opens/adds: highest weight first. THE TIE-BREAK IS THE CROSS-SECTION DECISION, and
        // under the default mode it is the run's own hidden parameter.
        //
        // `weight` is `WorkingOrder::weight`, the declared cross-symbol fill priority — and
        // NOTHING in this tree sets it: `vike_model::Broker::submit_market`/`submit_limit` pass
        // `0.0`, and so does `SimBroker::strategy_order_target_percent`. So on every strategy
        // that exists here every order ties at `0.0`, and the whole verdict of this allocator —
        // which symbol gets the last of the shared cash and the last open slot — falls to the
        // tie-break.
        //
        // * `DecideMode::Sequential` breaks it on `seq`, the collection order, which is the order
        //   `symbols` was handed to `StrategyEngine::new`, which is the order the operator typed
        //   `data.symbols`. Kept verbatim (Python `sorted` = stable) because it is the frozen
        //   answer every parity fixture pins.
        // * `DecideMode::Simultaneous` breaks it on the CROSS-SECTION RANK — each order's
        //   position in `SimBroker::cross_section`, i.e. its symbol's own name — so permuting
        //   `data.symbols` permutes nothing here. The rank is looked up rather than compared by
        //   name so that this comparator agrees with every fold by construction: both read the
        //   one canonical order, and a future ordering rule changes `cross_section_order` alone.
        opens.sort_by(|a, b| {
            let tie =
                if ranked { rank[a.0].cmp(&rank[b.0]).then(a.3.cmp(&b.3)) } else { a.3.cmp(&b.3) };
            b.1.weight.partial_cmp(&a.1.weight).unwrap_or(std::cmp::Ordering::Equal).then(tie)
        });
        for (si, o, fp, _) in opens {
            let sym = self.core.symbols[si].clone();
            if self.core.sym[si].pos.size == 0.0 && self.core.at_open_cap() {
                self.core.dropped.push((sym, kind_str(o.kind).into(), o.size, o.weight));
                continue;
            }
            if self.core.sym[si].pos.size == 0.0 && o.side > 0 && self.core.at_long_cap() {
                self.core.dropped.push((sym, kind_str(o.kind).into(), o.size, o.weight));
                continue;
            }
            if self.core.sym[si].pos.size == 0.0 && o.side < 0 && self.core.at_short_cap() {
                self.core.dropped.push((sym, kind_str(o.kind).into(), o.size, o.weight));
                continue;
            }
            let bar_ts = self.core.bars[si][cur_idx].ts;
            let bar_vol = self.core.bars[si][cur_idx].volume;
            let fill_size = self.volume_clamp(si, o.size, bar_vol);
            if fill_size <= 0.0 {
                continue;
            }
            // Maker/taker classification is hoisted ABOVE the cash pre-check because
            // `slippage_for` needs it: the impact addend is taker-only, so the gate must ask the
            // same question `apply_fill` will. Same rule as `dispatch_fill` — by ORDER KIND, not
            // by crossing aggressiveness.
            let is_maker = o.kind == OrderKind::Limit;
            // Pre-check only (cash gate + fee estimate). Uses `slippage_for` so it charges the
            // SAME price `apply_fill` is about to charge — otherwise an impact-priced fill could
            // pass a gate computed on the flat slippage and overdraw. Byte-identical without an
            // impact model (see `SimBroker::slippage_for`).
            // `.0` — the skip flag is deliberately DROPPED here. This is the cash pre-check, which
            // prices the same fill `apply_fill` will price again; counting in both would double
            // every entry, exactly as `SimBroker::slippage_saturations`' doc records for its twin.
            let slipped = adverse_fill_price(
                fp,
                o.side,
                self.core.slippage_for(si, fill_size, bar_ts, is_maker).0,
            );
            let rate = if is_maker { self.core.maker_fee } else { self.core.taker_fee };
            let mult = self.core.multiplier_of_idx(si);
            let fee = fee_fn(fill_size, slipped, rate, mult);
            let cash_impact = -(o.side as f64 * fill_size) * slipped * mult - fee;
            if self.core.cash + cash_impact < 0.0 {
                self.core.dropped.push((sym, kind_str(o.kind).into(), fill_size, o.weight));
                continue;
            }
            if let Some(fill) = self.core.apply_fill(si, o.side, fill_size, fp, bar_ts, is_maker) {
                if o.stop.is_some() && self.core.sym[si].pos.size != 0.0 {
                    self.core.sym[si].stop = o.stop;
                }
                self.fire_on_fill(fill);
            }
        }
    }
}
