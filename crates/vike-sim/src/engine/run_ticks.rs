//! The tick-path run loop (`StrategyEngine::run_ticks`) and its per-tick fill pass.

use vike_model::{Bar, L2Book, Strategy, WorkingOrder};

use super::{
    EquitySampling, OrderGate, SimBroker, StrategyEngine, Tick, apply_book_event, tick_symbol,
};
use vike_analytics::BacktestResult;
use vike_fills::fill_resolution::resolve_intrabar_fills;

impl<S: Strategy<SimBroker>> StrategyEngine<S> {
    /// Per-tick multi-symbol run: k-way merge by ts (ties: stream order — heapq.merge
    /// semantics), route each tick to its symbol; on_bar does NOT fire.
    pub fn run_ticks(&mut self, ticks_by_symbol: &[(String, Vec<Tick>)]) -> BacktestResult {
        let n_sym = self.core.symbols.len();
        let mut equity_curve: Vec<f64> = Vec::new();
        let mut equity_ts: Vec<i64> = Vec::new();
        // ARM the opt-in order-latency gate for the duration of this replay ONLY (see
        // `StrategyEngine::latency`). `None` (default) installs nothing at all, so every line
        // below stays on the frozen zero-latency path.
        //
        // The gate is armed WITH the per-symbol venue-hold table (`SimBroker::taker_hold_table_ns`),
        // resolved once at the run start from the PIT properties seam. That table is EMPTY unless a
        // properties source is configured AND some symbol declares a hold, and an empty table makes
        // the gate compute exactly the delivery stamp it computed before the table existed.
        let armed_gate = self.latency.as_ref().map(|kind| {
            let run_start = ticks_by_symbol
                .iter()
                .filter_map(|(_, t)| t.first())
                .map(|t| match t {
                    Tick::Quote(q) => q.ts,
                    Tick::Trade(tr) => tr.ts,
                    Tick::Book(b) => b.ts,
                })
                .min()
                .unwrap_or(0);
            crate::latency::LatencyGate::with_holds(kind, self.core.taker_hold_table_ns(run_start))
        });
        self.core.latency = armed_gate;
        // ...and with it the strategy-visible SHADOW position (the response leg's other half; see
        // `SimBroker::shadow_pos`), seeded from the true positions so a pre-seeded book starts
        // agreeing with the exchange. Stays EMPTY — and every read stays the frozen line — when
        // no latency model is configured.
        self.core.shadow_pos = if self.core.latency.is_some() {
            (0..n_sym).map(|si| self.core.sym[si].pos.size).collect()
        } else {
            Vec::new()
        };
        self.strategy.on_start(&mut self.core);

        // stable k-way merge by (ts, stream index)
        let streams: Vec<&Vec<Tick>> =
            ticks_by_symbol.iter().filter(|(_, t)| !t.is_empty()).map(|(_, t)| t).collect();
        let mut cursors = vec![0usize; streams.len()];
        let tick_ts = |t: &Tick| match t {
            Tick::Quote(q) => q.ts,
            Tick::Trade(tr) => tr.ts,
            Tick::Book(b) => b.ts,
        };
        // Tick-lane equity-curve density, copied out of `self` once so the fold below reads a
        // `Copy` local instead of re-borrowing `self` between the `&mut self` engine calls.
        // `EveryTick` (the default) takes the frozen push-every-tick arm.
        let equity_sampling = self.equity_sampling;
        // The run's LAST PRICED tick ts, tracked ONLY under a thinning `EveryN` so the subsample
        // can be closed with a final sample after the loop (see there). Stays `None` — and the
        // block that reads it is dead — on the default and `Off` paths.
        let mut last_priced_ts: Option<i64> = None;
        // Replay book state per symbol: None until the first Snapshot anchors it, dropped on
        // any integrity break (seq discontinuity or a recorded §B gap/stale marker) until the
        // next Snapshot — the gap-sentinel rule, replayed.
        //
        // Held on the BROKER (rather than as a local, as it was before the L2 fill tier) so the
        // `FillModelKind::L2Book` price law and the `Broker::quote_vwap`/`depth_within_price`
        // strategy reads see the same book this loop maintains. Sized here and cleared at the end
        // of the replay, exactly like `shadow_pos`, so no other engine path ever carries one.
        self.core.books = vec![None; n_sym];
        // ARM the tick lane's market context for the opt-in impact model, for the duration of this
        // replay only — the same discipline as `latency` / `shadow_pos` / `books` above, and for
        // the same reason: a window left behind would let a later bar `run` or a caller inspecting
        // the broker be answered off a stale tape.
        //
        // Sized ONLY when a model is configured, so a default replay allocates nothing and
        // `SimBroker::impact_context`'s `Vec::get` misses exactly as it does on the bar path.
        // Without this the tick lane was inert rather than absent: `run_ticks` is routinely driven
        // with an EMPTY per-symbol bar series, so the bar-window read measured `None` and an
        // operator who configured `[engine.impact]` got a silent zero.
        self.core.impact_ticks = if self.core.impact.is_some() {
            vec![crate::impact::TickWindow::with_capacity(self.core.impact_window); n_sym]
        } else {
            Vec::new()
        };
        // Last event ts seen per symbol — the settlement stamp for the end-of-run resolution sweep
        // when no explicit `resolution_end_ts` is given. `0` means "this symbol saw no ticks".
        let mut last_ts: Vec<i64> = vec![0; n_sym];
        let mut i = 0usize;
        loop {
            let mut best: Option<(i64, usize)> = None;
            for (k, s) in streams.iter().enumerate() {
                if cursors[k] < s.len() {
                    let ts = tick_ts(&s[cursors[k]]);
                    if best.is_none() || ts < best.unwrap().0 {
                        best = Some((ts, k));
                    }
                }
            }
            let Some((_, k)) = best else { break };
            // BORROWED, not cloned. `streams[k]` is a `&Vec<Tick>` reborrowed out of the
            // `ticks_by_symbol` PARAMETER, so `tick`'s lifetime comes from the caller's slice and
            // is independent of `self` — which is what lets it stay live across the `&mut self`
            // engine calls below. The clone this replaces cost one `String` allocation per
            // quote/trade and three (symbol + both `Vec<BookLevel>`) per book event, on every tick of
            // the tape, purely to hand the fold a value it only ever reads.
            let stream: &[Tick] = streams[k];
            let tick: &Tick = &stream[cursors[k]];
            cursors[k] += 1;

            let sym = tick_symbol(tick);
            let Some(si) = self.core.symbols.iter().position(|s| s.as_str() == sym) else {
                i += 1;
                continue; // unknown symbol — skipped (Python logs a warning)
            };
            if let Tick::Book(b) = tick {
                self.core.now = b.ts;
                last_ts[si] = b.ts;
                // Latency gate (opt-in, no-ops when unarmed): book events advance the clock, so
                // an in-flight order/cancel whose delivery stamp has arrived lands HERE too —
                // otherwise a book-only stretch of tape would freeze the exchange's view.
                self.deliver_due_orders(b.ts);
                self.deliver_due_fills(b.ts);
                // book events advance time too: settle a just-resolved symbol before delivering
                // the book (no-op when no resolution source is configured). Runs BEFORE the
                // queue-depth read so a resolution-driven cancel is already reflected in the
                // tracked set the `pre`/`apply` pair folds over.
                self.check_resolution_at(si, b.ts);
                self.check_option_expiry_at(si, b.ts);
                // Queue-model depth hook (opt-in; `pre` is None when no tracker exists): read
                // the tracked levels' resting qty BEFORE the event mutates the book, apply,
                // then fold the old→new change into every tracked queue state.
                let pre =
                    self.queue.as_ref().map(|t| t.pre_depth(si, self.core.books[si].as_deref()));
                let applied = apply_book_event(&mut self.core.books[si], b);
                if let Some(pre) = pre
                    && let Some(tracker) = self.queue.as_mut()
                {
                    tracker.apply_depth(si, &pre, self.core.books[si].as_deref());
                }
                if applied {
                    // An `Rc` clone (O(1)) rather than a borrow, so the book stays IN the broker
                    // for the duration of the callback: a strategy handling `on_order_book` can
                    // call `quote_vwap`/`depth_within_price` on its own symbol and get the book it
                    // was just handed, instead of a hole where the book used to be.
                    if let Some(book) = self.core.books[si].clone() {
                        // book events don't advance the warmup index `i` (they carry no price
                        // semantics); deliver once a valid book exists.
                        self.strategy.on_order_book(&mut self.core, &book);
                    }
                }
                continue; // no fill/equity/i bump — quotes/trades own price semantics (v1)
            }
            // Queue-model seed fallback (opt-in): remember the freshest L1 quote so a bookless
            // replay can still seed a new resting order's front from the matching quote size.
            if let (Some(tracker), Tick::Quote(q)) = (self.queue.as_mut(), tick) {
                tracker.note_quote(si, q);
            }
            let event = match tick {
                Tick::Quote(q) => vike_model::quote_tick_to_bar(q),
                Tick::Trade(t) => vike_model::trade_tick_to_bar(t),
                // `Tick::Book` is intercepted above and `continue`d — it never reaches the
                // price path. Rust match exhaustiveness is a static check (blind to the
                // `continue`), so this arm is required; it fabricates no price semantics.
                Tick::Book(_) => unreachable!("book ticks are intercepted before the price path"),
            };
            self.core.now = event.ts;
            last_ts[si] = event.ts;
            // Latency gate (opt-in, no-ops when unarmed) — BEFORE this tick's fill phase, so an
            // order submitted `entry()` ago is resting in time to be matched, and a cancel
            // submitted too recently is NOT yet applied and therefore cannot save its order.
            self.deliver_due_orders(event.ts);
            // ...and the response leg: fills whose visibility stamp has arrived reach the
            // strategy now, late, exactly as an ack does live.
            self.deliver_due_fills(event.ts);
            // opt-in binary-resolution settlement BEFORE this tick's stop/fill phase (no-op
            // when unconfigured): cancels the symbol's resting orders and closes any open
            // position at the payout, so nothing fills past resolution.
            self.check_resolution_at(si, event.ts);
            // opt-in option-expiry settlement, likewise BEFORE the fill phase (no-op unconfigured)
            self.check_option_expiry_at(si, event.ts);
            // Freshness bookkeeping before the fill phase (no-op unless configured). A
            // `Tick::Trade` is a TRANSACTION and therefore a print regardless of its reported
            // size — some venues emit zero-size/index prints, which `trade_tick_to_bar` projects
            // to `volume == 0.0` with no quote. Deciding freshness from the tick KIND (not only
            // from the projected bar) is what makes the tick lane inert BY CONSTRUCTION rather
            // than by convention of the projection.
            let fresh_print =
                matches!(tick, Tick::Trade(_)) || vike_fills::staleness::is_fresh_print(&event);
            self.core.note_print_at(si, event.ts, fresh_print);
            // `Rc` clone (O(1)) so the fill pass can take `&mut self` while still seeing the
            // book — the same reason the `on_order_book` delivery above clones.
            let book_now = self.core.books.get(si).and_then(Clone::clone);
            self.fill_pending_tick(si, &event, tick, book_now.as_deref());
            self.core.sym[si].price = event.close;
            // Opt-in variation-margin settlement (no-op when unconfigured), the tick path's twin of
            // `run`'s. The cadence bucket is one global clock, so the FIRST tick to cross a boundary
            // settles EVERY open symbol at its own last-seen price — the other symbols need no tick
            // of their own to be marked. Placed after this tick's price update and before the
            // liquidation check, mirroring the bar path's ordering.
            self.core.settle_variation_margin(event.ts);
            self.check_liquidation_tick(si, &event);
            // Record this TRADE PRINT into the impact model's tick-lane market context — AFTER
            // EVERY FILL SITE THIS TICK REACHES, never before one. That ordering IS the
            // no-lookahead rule on this lane: an order pricing on this tick must not be charged
            // against a print that includes its own execution, exactly as the bar lane excludes
            // the bar its `ts` falls inside.
            // ⚠ It sits below `check_liquidation_tick`, not merely below `fill_pending_tick`,
            // and that is the CORRECTION of where it first landed. A forced close is a fill like
            // any other and is priced through the same `slippage_for`, so recording the print
            // between the two fill sites left liquidations — and only liquidations — charged
            // against a window containing their own tick, while the module claimed the exclusion
            // unconditionally. The strategy callback below may only SUBMIT; its orders fill on a
            // later tick, so nothing after this line prices anything against this print.
            // Quotes are deliberately not recorded (see `crate::impact::TickWindow`): they carry
            // no traded volume, so admitting them would push `avg_volume` toward zero and the
            // participation rate toward infinity on a quote-heavy tape. EMPTY vector = no model
            // configured, so the default replay pays one `is_empty` branch per tick.
            if !self.core.impact_ticks.is_empty()
                && let Tick::Trade(t) = tick
            {
                self.core.impact_ticks[si].push(t.price, t.size.abs());
            }
            // TWO DELIBERATE equity samples, NOT a redundant recompute (a perf audit flagged the
            // second call; it must stay). The peak is stamped with the PRE-dispatch equity because
            // the strategy callback below can read `drawdown_now()`, which divides by
            // `equity_peak` — the peak has to already include this tick's mark. The curve is
            // stamped with the POST-dispatch equity. The two agree for every strategy that only
            // uses the documented `Broker` verbs (they touch `pending` alone; `apply_fill` and the
            // settlement sweeps are `pub(crate)` and unreachable from a callback), but `SimBroker`
            // exposes `cash` and `sym` as PUBLIC fields, so a strategy CAN move equity in-callback
            // and the two samples are then genuinely different values. Collapsing them would
            // silently change which one lands where. (The bar lane `run` samples once, AFTER its
            // dispatch, and is a different — also deliberate — ordering.)
            let eq = self.core.equity_now();
            self.core.equity_peak = self.core.equity_peak.max(eq);
            if i >= self.warmup {
                self.core.index = i;
                match tick {
                    Tick::Quote(q) => self.strategy.on_quote_tick(&mut self.core, q),
                    Tick::Trade(t) => self.strategy.on_trade_tick(&mut self.core, t),
                    Tick::Book(_) => unreachable!("book ticks are intercepted before dispatch"),
                }
            }
            // Equity-curve record (see `EquitySampling`). `EveryTick` — the default — is the frozen
            // unconditional push; the other arms exist only because two `Vec`s growing 16 bytes per
            // tick is 1.6 GB on a 100M-tick tape, paid by every point of a sweep.
            match equity_sampling {
                // `EveryN(0 | 1)` names the same sampling as `EveryTick` (documented on the
                // variant), so it joins the frozen arm rather than dividing by a degenerate
                // stride — which also leaves `n >= 2` guaranteed below.
                EquitySampling::EveryTick | EquitySampling::EveryN(0 | 1) => {
                    equity_curve.push(self.core.equity_now());
                    equity_ts.push(event.ts);
                }
                EquitySampling::EveryN(n) => {
                    if i.is_multiple_of(n) {
                        equity_curve.push(self.core.equity_now());
                        equity_ts.push(event.ts);
                    }
                    last_priced_ts = Some(event.ts);
                }
                EquitySampling::Off => {}
            }
            i += 1;
        }
        // Close an `EquitySampling::EveryN` subsample with the run's LAST priced tick when the
        // stride did not already land on it. Nothing has moved equity since that tick (the merge
        // loop just ended), so `equity_now()` here IS that tick's equity. This keeps the two
        // invariants the settlement sweeps below rely on: `equity_curve.last()` agrees with
        // `final_equity`, and `last_mut()` patches the FINAL row rather than a stale mid-run one.
        // `last_priced_ts` is `None` on the default (`EveryTick`) and `Off` paths, so this whole
        // block is dead there — byte-identical.
        if let Some(ts) = last_priced_ts
            && equity_ts.last() != Some(&ts)
        {
            equity_curve.push(self.core.equity_now());
            equity_ts.push(ts);
        }
        // END-OF-RUN binary-resolution sweep — the tick path's twin of `run`'s (see
        // `final_resolution_sweep`): a recorded tape normally stops at the trading halt, before
        // the resolution posts. No-op when unconfigured.
        if self.core.resolution.is_some() {
            // a symbol that saw no ticks at all settles at the run's last known ts
            let now = self.core.now;
            for t in last_ts.iter_mut() {
                if *t == 0 {
                    *t = now;
                }
            }
            if self.final_resolution_sweep(&last_ts)
                && let Some(last) = equity_curve.last_mut()
            {
                *last = self.core.equity_now();
            }
        }
        // END-OF-RUN option-expiry sweep — the tick path's twin of `run`'s (see
        // `final_option_expiry_sweep`). No-op when unconfigured. The `last_ts` 0→now fixup is
        // repeated (idempotent) so this arm is correct even when no `resolution` source ran it.
        if self.core.option_specs.is_some() {
            let now = self.core.now;
            for t in last_ts.iter_mut() {
                if *t == 0 {
                    *t = now;
                }
            }
            if self.final_option_expiry_sweep(&last_ts)
                && let Some(last) = equity_curve.last_mut()
            {
                *last = self.core.equity_now();
            }
        }
        // Response-leg flush: every still-held fill reaches the strategy before `on_stop`, so a
        // strategy's own accounting cannot end the run disagreeing with `BacktestResult`.
        // In-flight ORDER actions are deliberately NOT flushed — an action still in flight when
        // the tape ends never reached the venue, so it must not retro-actively apply.
        self.flush_deferred_fills();
        // DISARM: the gate exists only for the duration of a tick replay (armed at the top of
        // `run_ticks`), which is what keeps the bar path from ever seeing one. The shadow goes
        // with it — every fill has now been delivered, so from `on_stop` onward the strategy reads
        // exchange truth again and cannot end the run disagreeing with `BacktestResult`.
        self.core.latency = None;
        self.core.shadow_pos = Vec::new();
        // The replay books go with them: they are tick-replay state, so no later read (`on_stop`,
        // a caller inspecting the broker, a subsequent bar `run`) can be answered off a stale book.
        // The impact model's tick window is the same kind of state and is dropped for the same
        // reason — leaving it behind would make a later BAR run price its fills off this tape's
        // prints instead of its own bars, which is the one way this extension could reach a lane
        // it was never wired into.
        self.core.books = Vec::new();
        self.core.impact_ticks = Vec::new();
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
            per_symbol_curves: Vec::new(),
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

    /// Per-tick fill for one symbol (`_fill_pending_tick`). `tick` is the RAW replayed tick
    /// (`event` is its Bar-shaped projection) and `book` the symbol's current replay book —
    /// both consulted ONLY by the opt-in queue-model branch; the `None` (default) path below
    /// is the frozen pre-queue code, byte-identical.
    fn fill_pending_tick(&mut self, si: usize, event: &Bar, tick: &Tick, book: Option<&L2Book>) {
        // protective stop on this tick (a breach OCO-cancels the resting exit siblings, same
        // law as every other lane — trigger-law wave 2)
        self.check_stop(si, event.open, event.low, event.high, event.ts);
        if self.queue.is_some() {
            self.fill_pending_tick_queued(si, event, tick, book);
            return;
        }
        // resting/market orders
        let pending = std::mem::take(&mut self.core.sym[si].pending);
        let mut triggered: Vec<(WorkingOrder, f64)> = Vec::new();
        let mut still: Vec<WorkingOrder> = Vec::new();
        for mut o in pending {
            // THE gate sequence (see `gate_pending`). Its staleness half is INERT on this lane by
            // construction — `event` is the symbol's own just-arrived quote/trade tick, hence a
            // fresh print with age 0 (see `vike_fills::staleness`) — and is wired anyway so the policy
            // has ONE definition, not two.
            match self.gate_pending(si, &mut o, event) {
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
            self.dispatch_fill(si, &o, fp, event.volume, event.ts);
        }
        // The HFT maker lane: cross this tick against the resting tagged quotes (the maker is driven
        // by on_quote_tick / on_order_book, which only fire on this tick path). A no-op unless an
        // `HftBroker` strategy is mounted, so every existing tick-replay test is unaffected.
        self.fill_tagged(si, event);
    }
}
