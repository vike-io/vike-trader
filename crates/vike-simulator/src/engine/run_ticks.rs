//! The tick-path run loop (`StrategyEngine::run_ticks`) and its per-tick fill pass.

use vike_marketdata::{Bar, L2Book, apply_book_event};
use vike_model::{OrderDrop, OrderKind, OrderResting, Strategy};

use super::{
    EquitySampling, OrderBrokerSimulator, OrderEmulationMarketHook, OrderGate, StrategyEngine,
    TickEvent, tick_symbol,
};
use vike_analytics::{TradingRecord, TradingRecordSimulator, TradingRecordSource};

impl<S: Strategy<OrderBrokerSimulator>> StrategyEngine<S> {
    /// One HEARD event (a `TickEvent::Reference`, `Mark`, `Flow` or `FeedStatus`): the clock moves to
    /// when it is heard ([`TickEvent::heard_at`]) as a book event moves it (the inputs, the conversion
    /// rates, the in-flight orders and fills due by then), the order ends go first, and the strategy
    /// hears it on its hook. No fill, no equity sample, no warmup index: the live core hears these four
    /// off its market lanes, never warmup-gated (`vike_core`'s `drive_strategy_reference_quote`,
    /// `drive_strategy_mark`, `drive_strategy_flow`, `drive_strategy_feed_status`).
    fn hear(&mut self, tick: &TickEvent) {
        let ts = tick.heard_at();
        self.core.set_now(ts);
        self.core.inputs.advance_to(ts);
        self.core.advance_quote_rates(ts);
        self.deliver_due_orders(ts);
        self.deliver_due_fills(ts);
        self.deliver_order_ends();
        match tick {
            TickEvent::Reference { venue, quote, .. } => {
                self.strategy.on_reference_quote(&mut self.core, venue, quote)
            }
            TickEvent::Mark { mark, .. } => self.strategy.on_mark(&mut self.core, mark),
            TickEvent::Flow(flow) => self.strategy.on_flow(&mut self.core, *flow),
            TickEvent::FeedStatus { status, .. } => {
                self.strategy.on_feed_status(&mut self.core, *status)
            }
            TickEvent::Quote(_) | TickEvent::Trade(_) | TickEvent::Book(_) => {
                unreachable!("a market event is not heard: run_ticks routes it to its symbol")
            }
        }
    }

    /// Per-tick multi-symbol run: k-way merge by [`TickEvent::heard_at`] (ties: stream order —
    /// heapq.merge semantics), route each tick to its symbol; on_bar does NOT fire. A heard event
    /// ([`TickEvent::Reference`], `Mark`, `Flow`, `FeedStatus`) reaches its hook and fills nothing, and a
    /// traded leg on another venue (`StrategyEngineConfig::reference_legs`) fills on its own events and
    /// reaches no hook: its touch is heard as the `Reference` events its caller hands beside it.
    ///
    /// # Panics
    ///
    /// When `StrategyEngineConfig::shortfall_cover` is set: that margin call is judged at a bar's extreme and
    /// has no tick walk (`ShortfallCover`'s doc). On a portfolio run (`StrategyEngine::new_mounted`).
    ///
    /// A stream is any slice of events (`T: AsRef<[TickEvent]>`): a `Vec` the caller owns, or a BORROWED range of
    /// a longer tape (a tick walk-forward's window, the owner's yes of 2026-11-20). The fold is the same either way.
    pub fn run_ticks<T: AsRef<[TickEvent]>>(
        &mut self,
        ticks_by_symbol: &[(String, T)],
    ) -> TradingRecord {
        // the last line (the harness refuses a tick lane beside `[[mounts]]` in words first): the tick
        // hooks are not routed to mounts (`crate::mounted`)
        assert!(!self.core.is_mounted(), "a portfolio run (`new_mounted`) has no tick lane");
        assert!(
            self.core.shortfall_cover.is_none(),
            "StrategyEngineConfig::shortfall_cover is a bar-path margin call; run_ticks refuses it"
        );
        let n_symbol = self.core.symbols.len();
        // The curve's room made ONCE, never by doubling: at most one sample a priced event (a quote or a trade),
        // the stride's share of them under `EveryN`, plus its closing sample (see `EquitySampling`). A curve kept
        // by time is a close a step, a few kB a day: it grows.
        let priced = ticks_by_symbol
            .iter()
            .flat_map(|(_, t)| t.as_ref())
            .filter(|t| matches!(t, TickEvent::Quote(_) | TickEvent::Trade(_)))
            .count();
        let room = match self.equity_sampling {
            EquitySampling::EveryTick
            | EquitySampling::EveryN(0 | 1)
            | EquitySampling::ByTime(..=0) => priced,
            EquitySampling::EveryN(n) => priced / n + 2,
            EquitySampling::ByTime(_) | EquitySampling::Off => 0,
        };
        let mut by_time = match self.equity_sampling {
            EquitySampling::ByTime(interval_ms) if interval_ms > 0 => {
                Some(super::equity_by_time::EquityByTime::new(interval_ms))
            }
            _ => None,
        };
        let mut equity_curve: Vec<f64> = Vec::with_capacity(room);
        let mut equity_ts: Vec<i64> = Vec::with_capacity(room);
        // ARM the opt-in order-latency gate for the duration of this replay ONLY (see
        // `StrategyEngine::latency`). `None` (default) installs nothing at all, so every line
        // below stays on the frozen zero-latency path.
        //
        // The gate is armed WITH the per-symbol venue-hold table (`OrderBrokerSimulator::taker_hold_table_ns`),
        // resolved once at the run start from the PIT properties seam. That table is EMPTY unless a
        // properties source is configured AND some symbol declares a hold, and an empty table makes
        // the gate compute exactly the delivery stamp it computed before the table existed.
        let armed_gate = self.latency.as_ref().map(|kind| {
            let run_start = ticks_by_symbol
                .iter()
                .filter_map(|(_, t)| t.as_ref().first())
                .map(TickEvent::timestamp)
                .min()
                .unwrap_or(0);
            crate::latency::LatencyGate::with_holds(kind, self.core.taker_hold_table_ns(run_start))
        });
        self.core.latency = armed_gate;
        // ...and with it the strategy-visible SHADOW position (the response leg's other half; see
        // `OrderBrokerSimulator::shadow_pos`), seeded from the true positions so a pre-seeded book starts
        // agreeing with the exchange. Stays EMPTY — and every read stays the frozen line — when
        // no latency model is configured.
        self.core.shadow_pos = if self.core.latency.is_some() {
            (0..n_symbol).map(|si| self.core.symbol[si].pos.size).collect()
        } else {
            Vec::new()
        };
        // The run's last event: where each input's whole series ends (`OrderBroker::input_whole`; a
        // no-op unless the run serves it).
        let last_event =
            ticks_by_symbol.iter().filter_map(|(_, t)| t.as_ref().last()).map(TickEvent::heard_at);
        if let Some(end) = last_event.max() {
            self.core.inputs.end_at(end);
        }
        self.strategy.on_start(&mut self.core);

        // stable k-way merge by (when the event is heard, stream index)
        let streams: Vec<&[TickEvent]> =
            ticks_by_symbol.iter().map(|(_, t)| t.as_ref()).filter(|t| !t.is_empty()).collect();
        let mut cursors = vec![0usize; streams.len()];
        // Tick-lane equity-curve density, copied out of `self` once so the fold below reads a
        // `Copy` local instead of re-borrowing `self` between the `&mut self` engine calls.
        // `EveryTick` (the default) takes the frozen push-every-tick arm.
        let equity_sampling = self.equity_sampling;
        // The run's LAST PRICED tick ts, tracked ONLY under a thinning `EveryN` so the subsample
        // can be closed with a final sample after the loop (see there). Stays `None` — and the
        // block that reads it is dead — on the default and `Off` paths.
        let mut last_priced_timestamp: Option<i64> = None;
        // Replay book state per symbol: None until the first Snapshot anchors it, dropped on
        // any integrity break (seq discontinuity or a recorded §B gap/stale marker) until the
        // next Snapshot — the gap-sentinel rule, replayed.
        //
        // Held on the BROKER (rather than as a local, as it was before the L2 fill tier) so the
        // `FillModelKind::L2Book` price law and the `OrderBroker::quote_vwap`/`depth_within_price`
        // strategy reads see the same book this loop maintains. Sized here and cleared at the end
        // of the replay, exactly like `shadow_pos`, so no other engine path ever carries one.
        self.core.books = vec![None; n_symbol];
        // ARM the tick lane's market context for the opt-in impact model, for the duration of this
        // replay only — the same discipline as `latency` / `shadow_pos` / `books` above, and for
        // the same reason: a window left behind would let a later bar `run` or a caller inspecting
        // the broker be answered off a stale tape.
        //
        // Sized ONLY when a model is configured, so a default replay allocates nothing and
        // `OrderBrokerSimulator::impact_context`'s `Vec::get` misses exactly as it does on the bar path.
        // Without this the tick lane was inert rather than absent: `run_ticks` is routinely driven
        // with an EMPTY per-symbol bar series, so the bar-window read measured `None` and an
        // operator who configured the `market_impact` slippage got a silent zero.
        self.core.impact_ticks = if self.core.impact.is_some() {
            vec![
                crate::impact::ImpactModelTickWindow::with_capacity(self.core.impact_window);
                n_symbol
            ]
        } else {
            Vec::new()
        };
        // Last event ts seen per symbol — the settlement stamp for the end-of-run resolution sweep
        // when no explicit `resolution_end_timestamp` is given. `0` means "this symbol saw no ticks".
        let mut last_timestamp: Vec<i64> = vec![0; n_symbol];
        let mut i = 0usize;
        loop {
            let mut best: Option<(i64, usize)> = None;
            for (k, s) in streams.iter().enumerate() {
                if cursors[k] < s.len() {
                    let ts = s[cursors[k]].heard_at();
                    if best.is_none() || ts < best.unwrap().0 {
                        best = Some((ts, k));
                    }
                }
            }
            let Some((_, k)) = best else { break };
            // BORROWED, not cloned. `streams[k]` is a `&[TickEvent]` reborrowed out of the
            // `ticks_by_symbol` PARAMETER, so `tick`'s lifetime comes from the caller's slice and
            // is independent of `self` — which is what lets it stay live across the `&mut self`
            // engine calls below. The clone this replaces cost one `String` allocation per
            // quote/trade and three (symbol + both `Vec<L2BookLevel>`) per book event, on every tick of
            // the tape, purely to hand the fold a value it only ever reads.
            let stream: &[TickEvent] = streams[k];
            let tick: &TickEvent = &stream[cursors[k]];
            cursors[k] += 1;

            // A heard event fills nothing and names no slot: its hook, and on to the next event.
            if matches!(
                tick,
                TickEvent::Reference { .. }
                    | TickEvent::Mark { .. }
                    | TickEvent::Flow(_)
                    | TickEvent::FeedStatus { .. }
            ) {
                self.hear(tick);
                continue;
            }
            let symbol = tick_symbol(tick);
            let Some(si) = self.core.symbols.iter().position(|s| s.as_str() == symbol) else {
                i += 1;
                continue; // unknown symbol — skipped (Python logs a warning)
            };
            if let TickEvent::Book(b) = tick {
                self.core.set_now(b.timestamp);
                // The inputs as of this event (decision 0271; no-op with none).
                self.core.inputs.advance_to(b.timestamp);
                self.core.advance_quote_rates(b.timestamp);
                last_timestamp[si] = b.timestamp;
                // Latency gate (opt-in, no-ops when unarmed): book events advance the clock, so
                // an in-flight order/cancel whose delivery stamp has arrived lands HERE too —
                // otherwise a book-only stretch of tape would freeze the exchange's view.
                self.deliver_due_orders(b.timestamp);
                self.deliver_due_fills(b.timestamp);
                // book events advance time too: settle a just-resolved symbol before delivering
                // the book (no-op when no resolution source is configured). Runs BEFORE the
                // queue-depth read so a resolution-driven cancel is already reflected in the
                // tracked set the `pre`/`apply` pair folds over.
                self.check_resolution_at(si, b.timestamp);
                // every configured option, not only this event's symbol (no-op unconfigured)
                self.check_option_expiries_at(b.timestamp);
                // Queue-model depth hook (opt-in; `pre` is None when no tracker exists): read
                // the tracked levels' resting qty BEFORE the event mutates the book, apply,
                // then fold the old→new change into every tracked queue state.
                let pre =
                    self.queue.as_ref().map(|t| t.pre_depth(si, self.core.books[si].as_deref()));
                // The recorded chain's one integrity rule (`vike_marketdata`'s `apply_book_event`), on the
                // event's own grid. `Rc::make_mut` is O(1) whenever this is the only handle, which it is by
                // the time the next event arrives: the `on_order_book` delivery clone is dropped at the end
                // of that callback. It clones (once) only if a strategy has deliberately retained one.
                let applied = apply_book_event(
                    &mut self.core.books[si],
                    b,
                    b.tick_size,
                    std::rc::Rc::make_mut,
                );
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
                        // the emulated orders' laws on this book first (no-op while none works)
                        self.core.drive_emulated(si, &OrderEmulationMarketHook::Book(&book));
                        // book events don't advance the warmup index `i` (they carry no price
                        // semantics); deliver once a valid book exists, the order ends first. A
                        // foreign leg's book reaches no hook: its top is heard as a `Reference`.
                        if !self.leg_venues.get(si).is_some_and(Option::is_some) {
                            self.deliver_order_ends();
                            self.strategy.on_order_book(&mut self.core, &book);
                        }
                    }
                }
                continue; // no fill/equity/i bump — quotes/trades own price semantics (v1)
            }
            // Queue-model seed fallback (opt-in): remember the freshest L1 quote so a bookless
            // replay can still seed a new resting order's front from the matching quote size.
            if let (Some(tracker), TickEvent::Quote(q)) = (self.queue.as_mut(), tick) {
                tracker.note_quote(si, q);
            }
            let event = match tick {
                TickEvent::Quote(q) => vike_marketdata::quote_tick_to_bar(q),
                TickEvent::Trade(t) => vike_marketdata::trade_tick_to_bar(t),
                // `TickEvent::Book` and the heard events are intercepted above and `continue`d —
                // they never reach the price path. Rust match exhaustiveness is a static check
                // (blind to the `continue`), so this arm is required; it fabricates no price semantics.
                TickEvent::Book(_)
                | TickEvent::Reference { .. }
                | TickEvent::Mark { .. }
                | TickEvent::Flow(_)
                | TickEvent::FeedStatus { .. } => {
                    unreachable!(
                        "book ticks and heard events are intercepted before the price path"
                    )
                }
            };
            self.core.set_now(event.timestamp);
            self.core.inputs.advance_to(event.timestamp);
            // the rate of each converted symbol as of this event (decision 0243; no-op with none)
            self.core.advance_quote_rates(event.timestamp);
            last_timestamp[si] = event.timestamp;
            // The scheduled funding due by this tick, before anything else this tick does: on the
            // position held at the instant, at the venue's mark or else this tick's price
            // (`StrategyEngineConfig::funding_schedule`). Empty schedule: no-op.
            self.core.charge_funding_schedule(si, event.timestamp, event.close);
            // Latency gate (opt-in, no-ops when unarmed) — BEFORE this tick's fill phase, so an
            // order submitted `entry()` ago is resting in time to be matched, and a cancel
            // submitted too recently is NOT yet applied and therefore cannot save its order.
            self.deliver_due_orders(event.timestamp);
            // ...and the response leg: fills whose visibility stamp has arrived reach the
            // strategy now, late, exactly as an ack does live.
            self.deliver_due_fills(event.timestamp);
            // opt-in binary-resolution settlement BEFORE this tick's stop/fill phase (no-op
            // when unconfigured): cancels the symbol's resting orders and closes any open
            // position at the payout, so nothing fills past resolution.
            self.check_resolution_at(si, event.timestamp);
            // opt-in option-expiry settlement, likewise BEFORE the fill phase (no-op unconfigured):
            // EVERY configured option is probed, since an option's own tape stops before its expiry
            self.check_option_expiries_at(event.timestamp);
            // Freshness bookkeeping before the fill phase (no-op unless configured). A
            // `TickEvent::Trade` is a TRANSACTION and therefore a print regardless of its reported
            // size — some venues emit zero-size/index prints, which `trade_tick_to_bar` projects
            // to `volume == 0.0` with no quote. Deciding freshness from the tick KIND (not only
            // from the projected bar) is what makes the tick lane inert BY CONSTRUCTION rather
            // than by convention of the projection.
            let fresh_print = matches!(tick, TickEvent::Trade(_))
                || vike_fills::staleness::is_fresh_print(&event);
            self.core.note_print_at(si, event.timestamp, fresh_print);
            // `Rc` clone (O(1)) so the fill pass can take `&mut self` while still seeing the
            // book — the same reason the `on_order_book` delivery above clones.
            let book_now = self.core.books.get(si).and_then(Clone::clone);
            self.fill_pending_tick(si, &event, tick, book_now.as_deref());
            // the resting combos, on this tick and the legs' last marks (no-op while none rests)
            self.core.note_combo_mark(si, &event);
            self.sweep_combos();
            self.core.symbol[si].price = event.close;
            // Opt-in variation-margin settlement (no-op when unconfigured), the tick path's twin of
            // `run`'s. The cadence bucket is one global clock, so the FIRST tick to cross a boundary
            // settles EVERY open symbol at its own last-seen price — the other symbols need no tick
            // of their own to be marked. Placed after this tick's price update and before the
            // liquidation check, mirroring the bar path's ordering.
            self.core.settle_variation_margin(event.timestamp);
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
            // Quotes are deliberately not recorded (see `crate::impact::ImpactModelTickWindow`): they carry
            // no traded volume, so admitting them would push `average_volume` toward zero and the
            // participation rate toward infinity on a quote-heavy tape. EMPTY vector = no model
            // configured, so the default replay pays one `is_empty` branch per tick.
            if !self.core.impact_ticks.is_empty()
                && let TickEvent::Trade(t) = tick
            {
                self.core.impact_ticks[si].push(t.price, t.size.abs());
            }
            // TWO DELIBERATE equity samples, NOT a redundant recompute (a perf audit flagged the
            // second call; it must stay). The peak is stamped with the PRE-dispatch equity because
            // an entry the strategy callback below submits is sized off a drawdown that divides by
            // `equity_peak` (`size_entry`) — the peak has to already include this tick's mark. The curve is
            // stamped with the POST-dispatch equity. The two agree for every strategy that only
            // uses the documented `OrderBroker` verbs (they touch `pending` alone; `apply_fill` and the
            // settlement sweeps are `pub(crate)` and unreachable from a callback), but `OrderBrokerSimulator`
            // exposes `cash` and `symbol` as PUBLIC fields, so a strategy CAN move equity in-callback
            // and the two samples are then genuinely different values. Collapsing them would
            // silently change which one lands where. (The bar lane `run` samples once, AFTER its
            // dispatch, and is a different — also deliberate — ordering.)
            let eq = self.core.equity_now();
            self.core.equity_peak = self.core.equity_peak.max(eq);
            // The emulated orders' laws on this event, after its fills and before the strategy's
            // hook (no-op while none works).
            let hook = match tick {
                TickEvent::Quote(q) => OrderEmulationMarketHook::Quote(q),
                TickEvent::Trade(t) => OrderEmulationMarketHook::Trade(t),
                TickEvent::Book(_)
                | TickEvent::Reference { .. }
                | TickEvent::Mark { .. }
                | TickEvent::Flow(_)
                | TickEvent::FeedStatus { .. } => {
                    unreachable!("book ticks and heard events are intercepted before the drive")
                }
            };
            self.core.drive_emulated(si, &hook);
            // A traded leg on another venue (`StrategyEngineConfig::reference_legs`) reaches no hook: its
            // touch is heard as the `Reference` events beside it, when this box hears them.
            if i >= self.warmup && !self.leg_venues.get(si).is_some_and(Option::is_some) {
                self.core.index = i;
                // the ends of the seam orders, this event's fills' included, before the hook, as
                // the bar lane sends them before `on_bar` and a venue sends them live
                self.deliver_order_ends();
                match tick {
                    TickEvent::Quote(q) => self.strategy.on_quote_tick(&mut self.core, q),
                    TickEvent::Trade(t) => self.strategy.on_trade_tick(&mut self.core, t),
                    TickEvent::Book(_)
                    | TickEvent::Reference { .. }
                    | TickEvent::Mark { .. }
                    | TickEvent::Flow(_)
                    | TickEvent::FeedStatus { .. } => {
                        unreachable!("book ticks and heard events are intercepted before dispatch")
                    }
                }
            }
            // Equity-curve record (see `EquitySampling`). `EveryTick` — the default — is the frozen
            // unconditional push; the other arms exist only because two `Vec`s growing 16 bytes per
            // tick is 1.6 GB on a 100M-tick tape, paid by every point of a sweep.
            match equity_sampling {
                // `EveryN(0 | 1)` names the same sampling as `EveryTick` (documented on the
                // variant), so it joins the frozen arm rather than dividing by a degenerate
                // stride — which also leaves `n >= 2` guaranteed below. So does a step of `0` or less.
                EquitySampling::EveryTick
                | EquitySampling::EveryN(0 | 1)
                | EquitySampling::ByTime(..=0) => {
                    equity_curve.push(self.core.equity_now());
                    equity_ts.push(event.timestamp);
                }
                // A positive step, `by_time`'s (made for it above): one close a step, every tick folded.
                EquitySampling::ByTime(_) => {
                    if let Some(by_time) = by_time.as_mut() {
                        let equity = self.core.equity_now();
                        by_time.record(equity, event.timestamp, &mut equity_curve, &mut equity_ts);
                    }
                }
                EquitySampling::EveryN(n) => {
                    if i.is_multiple_of(n) {
                        equity_curve.push(self.core.equity_now());
                        equity_ts.push(event.timestamp);
                    }
                    last_priced_timestamp = Some(event.timestamp);
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
        // `last_priced_timestamp` is `None` on the default (`EveryTick`) and `Off` paths, so this whole
        // block is dead there — byte-identical.
        if let Some(ts) = last_priced_timestamp
            && equity_ts.last() != Some(&ts)
        {
            equity_curve.push(self.core.equity_now());
            equity_ts.push(ts);
        }
        // A curve kept by time ends on the run's last priced tick too, so the sweeps below patch that row.
        if let Some(by_time) = &by_time {
            by_time.close(&mut equity_curve, &mut equity_ts);
        }
        // END-OF-RUN binary-resolution sweep — the tick path's twin of `run`'s (see
        // `final_resolution_sweep`): a recorded tape normally stops at the trading halt, before
        // the resolution posts. No-op when unconfigured.
        if self.core.resolution.is_some() {
            // a symbol that saw no ticks at all settles at the run's last known ts
            let now = self.core.now;
            for t in last_timestamp.iter_mut() {
                if *t == 0 {
                    *t = now;
                }
            }
            if self.final_resolution_sweep(&last_timestamp)
                && let Some(last) = equity_curve.last_mut()
            {
                *last = self.core.equity_now();
            }
        }
        // END-OF-RUN option-expiry sweep — the tick path's twin of `run`'s (see
        // `final_option_expiry_sweep`). No-op when unconfigured. The `last_timestamp` 0→now fixup is
        // repeated (idempotent) so this arm is correct even when no `resolution` source ran it.
        if self.core.option_specs.is_some() {
            let now = self.core.now;
            for t in last_timestamp.iter_mut() {
                if *t == 0 {
                    *t = now;
                }
            }
            if self.final_option_expiry_sweep(&last_timestamp)
                && let Some(last) = equity_curve.last_mut()
            {
                *last = self.core.equity_now();
            }
        }
        // The path's drawdown and first zero, its last tick at the value the sweeps left on the curve.
        let sampling = by_time.map(|by_time| by_time.finish(&equity_curve));
        // Response-leg flush: every still-held fill reaches the strategy before `on_stop`, so a
        // strategy's own accounting cannot end the run disagreeing with `TradingRecord`.
        // In-flight ORDER actions are deliberately NOT flushed — an action still in flight when
        // the tape ends never reached the venue, so it must not retro-actively apply.
        self.flush_deferred_fills();
        // DISARM: the gate exists only for the duration of a tick replay (armed at the top of
        // `run_ticks`), which is what keeps the bar path from ever seeing one. The shadow goes
        // with it — every fill has now been delivered, so from `on_stop` onward the strategy reads
        // exchange truth again and cannot end the run disagreeing with `TradingRecord`.
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
        self.core.note_kept_ticks();
        self.core.stamp_drops();
        let per_symbol_pnl: Vec<(String, f64)> = (0..n_symbol)
            .map(|si| (self.core.symbols[si].clone(), self.core.symbol_pnl(si)))
            .collect();
        TradingRecord {
            n_trades: self.core.trades.len(),
            trades: self.core.trades.clone(),
            equity_curve,
            final_equity: self.core.equity_now(),
            per_symbol_pnl,
            per_symbol_curves: Vec::new(),
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
            sampling,
            mounts: None,
            // What the strategy's hooks failed with, read once, after `on_stop`.
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

    /// Per-tick fill for one symbol (`_fill_pending_tick`). `tick` is the RAW replayed tick
    /// (`event` is its Bar-shaped projection) and `book` the symbol's current replay book —
    /// both consulted ONLY by the opt-in queue-model branch; the `None` (default) path below
    /// is the frozen pre-queue code, byte-identical.
    fn fill_pending_tick(
        &mut self,
        si: usize,
        event: &Bar,
        tick: &TickEvent,
        book: Option<&L2Book>,
    ) {
        // protective stop on this tick (a breach OCO-cancels the resting exit siblings, same
        // law as every other lane — trigger-law wave 2)
        self.check_stop(si, event.open, event.low, event.high, event.timestamp);
        // held orders whose release has come join `pending` first, on both tick twins
        self.core.release_held(si, event.timestamp);
        // A closing auction is its session's LAST bar, and a tape cannot name its last tick until
        // the next one has come: ended in words, never filled at a tick it cannot prove.
        if self.core.symbol[si].pending.iter().any(|o| o.kind == OrderKind::AuctionClose) {
            let core = &mut self.core;
            core.symbol[si].pending.retain(|o| {
                let auction = o.kind == OrderKind::AuctionClose;
                if auction {
                    let why = "auction_close_needs_bars".into();
                    core.dropped.push(OrderDrop {
                        symbol: core.symbols[si].clone(),
                        reason: why,
                        size: o.size,
                        weight: o.weight,
                    });
                }
                !auction
            });
        }
        if self.queue.is_some() {
            self.fill_pending_tick_queued(si, event, tick, book);
            return;
        }
        // resting/market orders
        let pending = std::mem::take(&mut self.core.symbol[si].pending);
        let mut triggered: Vec<(OrderResting, f64)> = Vec::new();
        let mut still: Vec<OrderResting> = Vec::new();
        for mut o in pending {
            // a quote (`OrderBrokerSimulatorSymbolState::quotes`) takes its own pass after this one
            if self.core.is_quote(si, o.qid) {
                still.push(o);
                continue;
            }
            // THE gate sequence (see `gate_pending`). Its staleness half is INERT on this lane by
            // construction — `event` is the symbol's own just-arrived quote/trade tick, hence a
            // fresh print with age 0 (see `vike_fills::staleness`) — and is wired anyway so the policy
            // has ONE definition, not two.
            match self.gate_pending(si, &mut o, event) {
                OrderGate::Rest => still.push(o),
                OrderGate::Trigger(fp) => triggered.push((o, fp)),
                OrderGate::End => {}
            }
        }
        self.core.symbol[si].pending = still;
        self.core.open_group_pass(si, &triggered);
        let resolved = self.resolve_triggered(si, triggered, event);
        for (o, fp) in resolved {
            if o.size <= 1e-12 {
                continue;
            }
            self.dispatch_fill(si, &o, fp, event.volume, event.timestamp);
        }
        self.core.close_group_pass();
        // The maker law's pass: cross this tick against the resting quotes (a maker is driven by
        // on_quote_tick / on_order_book, which only fire on this tick path). A no-op while no quote
        // rests (`StrategyEngineConfig::optimistic_quotes`), so every other tick replay is unaffected.
        self.fill_tagged(si, event);
    }
}
