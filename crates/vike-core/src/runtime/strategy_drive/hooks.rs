//! The non-bar strategy hooks: tick, feed status, flow, mark, reference quote, params, schedule.

use super::*;

impl<C: ExecutionClient> CoreThread<C> {
    /// The tick-lane twin of [`drive_strategy`] for a SUB-BAR update (quote/trade/book). Marks to
    /// `price`, then — for EVERY mount on this (venue, symbol), in mount order — runs `dispatch`
    /// over that mount's own `LiveBroker` snapshot and drains its submissions through the one live
    /// path before the next mount steps. No bar is appended and the OMS event-fold path is
    /// untouched, so this is parity-neutral (net-new Rust surface, no Python twin). Mirrors
    /// `MseCore`'s tick dispatch on the backtest side.
    pub(crate) fn drive_strategy_tick(
        &mut self,
        venue: &str,
        symbol: &str,
        price: f64,
        now: i64,
        mut dispatch: impl FnMut(&mut Box<dyn Strategy<LiveBroker> + Send>, &mut LiveBroker),
    ) {
        // marks fire for any symbol any engine accepts even without a strategy mount
        if let Some(eidx) = self.engine_idx_for_route_key(RouteKey::sole_account_of(venue))
            && self.eng(eidx).accepts_symbol(symbol)
        {
            // A sub-bar print, not the venue mark — filed as such so a fresh venue mark
            // keeps the account slot (the board's own trade/quote slots are unaffected).
            // CLOCK HYGIENE: age the account mark on the CORE clock, NOT `now`. On the
            // quote/trade lanes `now` is the VENUE event time — it is the strategy-facing
            // event ts (`ctx.now`, conditional firing, the broker drain) and must stay that
            // — but the ownership window in `set_mark_from` is core-clock on BOTH sides
            // (account.rs): a venue whose clock runs ahead must not let this print look
            // "fresh" enough to displace a genuine venue mark. `self.eng(eidx).now_ms` IS
            // that core clock (every engine is stamped with `clock.now_ms()` at the top of
            // `dispatch`), matching the closed-bar lane's mark write in `drive_strategy`.
            let mark_now = self.eng(eidx).now_ms;
            self.eng_mut(eidx).account.set_mark_from(
                venue,
                symbol,
                price,
                MarkSource::TradeTick,
                mark_now,
            );
            // …and onto the venue's OTHER accounts, on the same core clock. Inert (one bool
            // read) for every single-account process; see `mirror_venue_price`.
            self.mirror_venue_price(venue, symbol, price, MarkSource::TradeTick, mark_now, None);
            self.dirty = true;
        }
        // Phase C (opt-in): intra-bar conditional triggering off the tick lanes —
        // the fidelity upgrade Python's bar-close book lacks (conditionals.py). `tick_conditionals`
        // is the profile's `guards.conditionals_on_ticks` OR an operator's core-held stop resting
        // (a stop checked per closed bar is no protection): one bool load, as the config read was.
        // ⚠ OUTSIDE the block above on purpose: that block is the venue's DEFAULT engine taking the
        // price (it must accept the symbol), but the books are keyed `(venue, symbol)` and an arm
        // fires onto ITS OWN account (`cond_engine`), so a stop armed onto a labelled account for a
        // symbol the default engine does not hold (an admitted one, decision 0174) must still see
        // the venue's tick. Inside it such a stop never fired, silently.
        if self.tick_conditionals {
            self.fire_conditionals_at_price(venue, symbol, price, now);
        }
        // EVERY mount that hears this tick (`Audience::Tick` says who and why), in mount order, not
        // the first: two mounts may share one (venue, symbol), and a first-match lookup here ran only
        // the first mount's tick hook (bug D in
        // `crates/vike-core/src/runtime/tests/multi_mount.rs`).
        //
        // The hearers come from the tick table (`tick_audience_of`), which `rebuild_tick_audience`
        // builds from that same rule, rather than from asking the rule of every slot: this lane
        // runs per market message on the fold thread, and the lookup costs the same at any mount
        // count while a scan pays for every mount that does NOT hear the tick (decision 0110's
        // measurement). No `p99 < 10µs` gate harness reaches this lane (the gated harnesses feed
        // only fills, with nothing mounted); `crates/vike-core/tests/runtime_latency.rs`'s
        // `book_lane_hop_mounted` and `book_lane_burst_n` are its ungated instruments. It stays
        // cheap by construction: two hash lookups, a refcount bump (`.cloned()` on the `Arc`, so
        // the loop can step mounts through `&mut self` while it holds the list), no allocation.
        //
        // ⚠ The `is_some()` re-check is LOAD-BEARING, not redundant. A strategy that panics inside
        // `step_mount_on_tick` unwinds between `mounts[idx].take()` and the restore;
        // `run_loop.rs`'s `handle` catches it, enters safe-state, and the core keeps dispatching with
        // that slot `None` and NO table rebuild, so the table lists a slot the rule rejects. Without
        // the re-check the next tick on the pair would panic at `expect("just found")` inside the
        // step, abort this loop, and every mount listed after the empty slot would go deaf (with
        // safe-state re-entered on every tick). Do not remove it
        // (`the_tick_lane_skips_a_slot_left_empty_by_a_hook_panic` pins it).
        let Some(audience) = self.tick_audience_of(venue, symbol) else {
            return;
        };
        for &idx in audience.iter() {
            if self.mounts[idx].is_some() {
                self.step_mount_on_tick(idx, venue, symbol, price, now, &mut dispatch);
            }
        }
    }

    /// [`Self::drive_strategy_tick`]'s step for ONE subscribed mount: its [`LiveBroker`] over the
    /// closed-bar history `on_bar` would see for this (venue, symbol) at the mount's interval,
    /// `dispatch` past warmup, then its orders drained.
    fn step_mount_on_tick(
        &mut self,
        idx: usize,
        venue: &str,
        symbol: &str,
        price: f64,
        now: i64,
        dispatch: &mut impl FnMut(&mut Box<dyn Strategy<LiveBroker> + Send>, &mut LiveBroker),
    ) {
        self.step_mount_hook(idx, venue, symbol, Some(price), now, |_, mount, ctx| {
            if ctx.index >= mount.strategy.warmup() {
                dispatch(&mut mount.strategy, ctx);
            }
        });
    }

    /// ONE mount's step on a non-bar lane, the body every hook in this file shares: its
    /// [`LiveBroker`] over the mount's own engine (`EngineRoute::Mount`, else the primary; per
    /// MOUNT, because two mounts on one pair may name two accounts) and the closed-bar history
    /// `on_bar` would see for `(venue, symbol)` at the mount's interval, `hook` over the TAKEN
    /// mount, the slot restored, then its orders drained on `(venue, symbol)` at `now`. `price:
    /// None` is that engine's standing mark of `(venue, symbol)`, else 0.0 (no price rides a status
    /// change, a toxicity reading, a mark of another symbol, a re-tune or a schedule fire).
    ///
    /// `#[inline(always)]`: the tick lane runs it per market message, and every caller held this
    /// body inline before it was shared (generic over `hook`, so each caller keeps its own copy).
    #[inline(always)]
    fn step_mount_hook(
        &mut self,
        idx: usize,
        venue: &str,
        symbol: &str,
        price: Option<f64>,
        now: i64,
        hook: impl FnOnce(&mut Self, &mut StrategyMount, &mut LiveBroker),
    ) {
        let eidx = self.route_of(EngineRoute::Mount(idx), venue).unwrap_or(0);
        // the step sees the SAME closed-bar history on_bar would (the mount's series)
        let interval = self.mounts[idx].as_ref().expect("just found").interval.clone();
        let key = (venue.to_string(), symbol.to_string(), interval);
        let bars_arc = self.bars.get(&key).map(|s| Arc::clone(&s.closed)).unwrap_or_default();
        let index = bars_arc.len().saturating_sub(1);
        let price =
            price.unwrap_or_else(|| self.eng(eidx).account.mark_of(venue, symbol).unwrap_or(0.0));
        // BEFORE the take: `declared_views` reads the mount's own (venue, symbol, interval) out of
        // `self.mounts` (empty tables for a single-symbol mount, hence byte-identical).
        let views = self.declared_views(idx, None, venue);
        // take/replace so the strategy call can't alias the rest of the core state
        let mut mount = self.mounts[idx].take().expect("just found");
        let mut ctx = LiveBroker {
            data_delay: self.mount_delays[idx],
            positions: views.positions,
            prices: views.prices,
            bar_views: views.bar_views,
            position: self
                .eng(eidx)
                .position_size_of(symbol, vike_model::events::PositionSide::Both),
            price,
            equity: self.eng(eidx).sizing_equity(self.seed_of(eidx), &self.config.price_cfg),
            bars: bars_arc,
            index,
            now,
            contract_value: self.eng(eidx).account.contract_value_of(symbol),
            lot_size: self.eng(eidx).gate.limits.lot_size.unwrap_or(0.0),
            submissions: Vec::new(),
            modifications: Vec::new(),
            cancels: Vec::new(),
            brackets: Vec::new(),
            conditionals: Vec::new(),
            mass_cancel: false,
        };
        hook(self, &mut mount, &mut ctx);
        self.mounts[idx] = Some(mount);
        self.drain_broker(ctx, venue, symbol, now, idx);
    }

    /// Dispatch a feed-health status CHANGE to EVERY strategy mounted on `(venue, symbol)` — the
    /// occasional control-event twin of [`Self::drive_strategy_tick`]. The producer fires a
    /// [`Ingest::StreamStatus`] only on a `StreamStatus` transition (feed up↔down), so this path
    /// is NOT per-market-message: the p99<10µs event fold is untouched (a status change touches no
    /// OMS fold — only the mounted strategy's `on_feed_status` hook + any orders it buffers,
    /// drained through the one live path). No mark is set (a status change carries no price); the
    /// broker's `price` is the standing mark if one exists, else 0.0. NOT warmup-gated — a
    /// disconnect is a safety signal a strategy must always hear (mirroring `on_fill`, which is
    /// likewise unconditional). Phase D: iterates ALL mounts on the pair, so e.g. a 1m and a 5m
    /// mount on the same symbol both hear the same feed-death signal. No-op when nothing is mounted
    /// on the pair.
    pub(crate) fn drive_strategy_feed_status(
        &mut self,
        venue: &str,
        symbol: &str,
        status: FeedStatus,
    ) {
        let idxs: Vec<usize> = (0..self.mounts.len())
            .filter(|&i| self.mount_hears(i, venue, symbol, Audience::OwnPair))
            .collect();
        if idxs.is_empty() {
            return;
        }
        let now = self.engine.now_ms; // stamped at the top of dispatch()
        for idx in idxs {
            // This lane dispatches the MOUNT's own pair, so the declared legs get their own rows,
            // which matters because a feed death is precisely when a two-leg strategy wants to know
            // what inventory it is holding on the OTHER leg.
            self.step_mount_hook(idx, venue, symbol, None, now, |_, mount, ctx| {
                mount.strategy.on_feed_status(ctx, status);
            });
        }
    }

    /// Dispatch a per-side FLOW-TOXICITY reading to EVERY strategy mounted on `(venue, symbol)` — the
    /// occasional control-event twin of [`Self::drive_strategy_feed_status`] (RTDS wallet-toxicity
    /// guard). The producer fires an [`Ingest::Flow`] at toxicity cadence (a toxicity update, NOT per
    /// market message), so this path is NOT per-market-message: the p99<10µs event fold is untouched (a
    /// toxicity reading touches no OMS fold — only the mounted strategy's `on_flow` hook + any orders it
    /// buffers, drained through the one live path). Routes by the mount's OWN `(venue, symbol)` — the
    /// toxic tape's asset IS the mounted token — exactly the key the feed-status twin routes by, NOT the
    /// `underlying_symbol` indirection [`Self::drive_strategy_mark`] uses. No mark is set (a toxicity
    /// reading carries no price); the broker's `price` is the standing mark if one exists, else 0.0. NOT
    /// warmup-gated — toxic flow is a market fact regardless of bar count (mirroring `on_feed_status`/
    /// `on_mark`, likewise unconditional). Phase D: iterates ALL mounts on the pair, so e.g. a 1m and a
    /// 5m mount on the same symbol both hear the same reading. No-op when nothing is mounted on the pair.
    pub(crate) fn drive_strategy_flow(&mut self, venue: &str, symbol: &str, flow: FlowToxicity) {
        let idxs: Vec<usize> = (0..self.mounts.len())
            .filter(|&i| self.mount_hears(i, venue, symbol, Audience::OwnPair))
            .collect();
        if idxs.is_empty() {
            return;
        }
        let now = self.engine.now_ms; // stamped at the top of dispatch()
        for idx in idxs {
            self.step_mount_hook(idx, venue, symbol, None, now, |_, mount, ctx| {
                mount.strategy.on_flow(ctx, flow);
            });
        }
    }

    /// Route an UNDERLYING/reference mark to EVERY mount that WATCHES this `(venue, symbol)` as its
    /// cross-symbol `underlying_symbol` ("Option B") — a DIFFERENT symbol than the mount TRADES on.
    /// The occasional mark-drain twin of [`Self::drive_strategy_feed_status`]: it fires only from
    /// `drain_market`'s venue-mark loop, never the per-market-message hot fold, so the p99<10µs event
    /// fold is untouched (a mark touches no OMS fold — only the matching strategy's `on_mark` hook +
    /// any orders it buffers, drained through the one live path). Distinct from the mark write in
    /// `drain_market` that precedes it: that files the mark for the (venue, symbol) it names; THIS
    /// delivers the SAME mark to any strategy that anchors on it while trading a different token.
    ///
    /// The routing ([`Audience::Underlying`]) is the only real difference from the feed-status twin:
    /// it matches a mount's `underlying_symbol`, not its own `symbol`, and the broker ctx is built on
    /// the matching mount's OWN `(venue, symbol)` — the series it trades — with `now = ts` (the
    /// mark's event time). The underlying rides ONLY in the `MarkTick`. NOT warmup-gated (an underlying
    /// observation is a market fact regardless of bar count, mirroring `on_feed_status`). EARLY-RETURN
    /// when no mount declares this underlying — a run with no underlying-anchored mount never allocates
    /// here, so it is byte-identical.
    pub(crate) fn drive_strategy_mark(&mut self, venue: &str, symbol: &str, price: f64, ts: i64) {
        let idxs: Vec<usize> = (0..self.mounts.len())
            .filter(|&i| self.mount_hears(i, venue, symbol, Audience::Underlying))
            .collect();
        if idxs.is_empty() {
            return;
        }
        let mark = MarkTick { symbol: symbol.to_string(), price, ts };
        // `venue` already == every matched mount's venue (`Audience::Underlying` requires it), so it
        // is used verbatim below; only the mount's OWN symbol/interval differ from the underlying.
        for idx in idxs {
            // the mount's OWN series (venue, symbol) — the token it trades, so its standing mark is
            // the broker's price — NOT the underlying it merely watches, which rides only in `mark`
            // and is not a declared leg at all (it is `StrategyMount::underlying_symbol`).
            let m_symbol = self.mounts[idx].as_ref().expect("just found").symbol.clone();
            self.step_mount_hook(idx, venue, &m_symbol, None, ts, |_, mount, ctx| {
                mount.strategy.on_mark(ctx, &mark);
            });
        }
    }

    /// Route a CROSS-VENUE L1 touch to every mount that DECLARED this exact `(venue, symbol)` as a
    /// leg on a venue OTHER than its own — the xEMM reference-price lane, and the ONLY inbound path
    /// by which a strategy can see a second exchange's book.
    ///
    /// ## What it exists to fix
    ///
    /// #997 routed cross-venue ORDERS: `resolve_intent_venue` reads `MountLeg::venue` and
    /// `apply_intent` delivers the request to that venue's engine. But `MountLeg::venue` is read at
    /// that ONE site, on the OUTBOUND path. Every inbound predicate — the bar lane
    /// ([`Self::drive_strategy`]), the tick lane ([`Self::drive_strategy_tick`]), feed status
    /// ([`Self::drive_strategy_feed_status`]) and the underlying mark
    /// ([`Self::drive_strategy_mark`]) — asks with the mount's own venue (`Audience::Bar`,
    /// `Audience::Tick`, `Audience::OwnPair`, `Audience::Underlying`), so a message of a FOREIGN
    /// venue never reaches the mount, whatever leg it declares. A leg declared
    /// `MountLeg::at("BTC-USDT-SWAP", "okx")` on a hyperliquid mount therefore had its orders routed
    /// to okx and NEVER received okx's quotes. A cross-exchange maker prices its resting quotes off
    /// the other venue's touch, so without this lane the strategy has no input at all.
    ///
    /// ## The rule, and why it is disjoint from the tick lane rather than a widening of it
    ///
    /// [`Audience::Reference`] is the rule (a mount on ANOTHER venue that declared this exact
    /// `(symbol, venue)` as a leg). It is DISJOINT from [`Audience::Tick`], so one tick can never
    /// reach one mount through both and nothing is dispatched twice. Widening the tick lane instead
    /// — the obvious alternative — was rejected because it would make a foreign-venue tick an
    /// EMISSION surface keyed on the FOREIGN series (see the drain note below), which silently
    /// misroutes the maker's own quotes.
    ///
    /// ## ⚠ The ctx and the drain are the MOUNT'S OWN — this is the safety property
    ///
    /// The [`LiveBroker`] is built on the mount's OWN engine/series and
    /// [`Self::drain_broker`] is called with the mount's own `(venue, symbol)`, NEVER the tick's.
    /// That is what makes it safe for a strategy to place, re-price and pull its maker quotes from
    /// this hook:
    ///
    /// - a tagged submit carries `symbol: None` by `HftBroker` contract, so
    ///   `resolve_intent_symbol` yields the DRAIN's symbol and `resolve_intent_venue` yields the
    ///   mount's own venue (the mount's own symbol is not a declared leg). Drained on the tick's
    ///   series instead, that same buffered quote would resolve to the REFERENCE venue and be
    ///   placed there — the maker's own quote landing on the hedge venue;
    /// - the tag registry keys on `{mount_idx}|{venue}|{symbol}|{tag}` off the drain args, so a
    ///   quote placed from THIS lane is looked up under the identical key an own-venue bar/tick
    ///   dispatch writes — `cancel_tagged`/`modify_tagged` work across the two lanes.
    ///
    /// A symbol-carrying `Broker::submit_market(leg_symbol, …)` still routes to the leg's venue
    /// through the existing `resolve_intent_venue`, which is how the hedge leg reaches venue B.
    ///
    /// ## Cost
    ///
    /// Gated by `any_mount_ref` at BOTH call sites (`Ingest::Quote`, `Ingest::Book`), which is
    /// `false` for every runtime that has no cross-venue mount — so this is one bool load on the
    /// per-market-message fold the `p99 < 10µs` gate measures, and the `QuoteTick` below is built
    /// only AFTER a mount matched. NOT warmup-gated (a reference touch is a market fact regardless
    /// of the mount's own bar count, mirroring [`Self::drive_strategy_mark`]).
    ///
    /// No mark, no price-board write and no OMS fold happen here: the reference venue's own engine
    /// (if one is mounted) receives its marks through the ordinary lanes in the `Ingest` arms above
    /// this call. This lane is purely strategy delivery.
    pub(crate) fn drive_strategy_reference_quote(
        &mut self,
        venue: &str,
        symbol: &str,
        bid: f64,
        ask: f64,
        bid_size: f64,
        ask_size: f64,
        ts: i64,
    ) {
        let idxs: Vec<usize> = (0..self.mounts.len())
            .filter(|&i| self.mount_hears(i, venue, symbol, Audience::Reference))
            .collect();
        if idxs.is_empty() {
            return;
        }
        // Built AFTER the match so a runtime with a cross-venue mount still allocates nothing for
        // the (far more numerous) ticks of venues/symbols no mount declared.
        let q = vike_marketdata::QuoteTick {
            ts,
            local_ts: 0,
            bid,
            ask,
            bid_size,
            ask_size,
            symbol: symbol.to_string(),
        };
        for idx in idxs {
            // The mount's OWN (venue, symbol, interval) — the series it RESTS on, and so its engine,
            // its standing mark (the reference touch is a FOREIGN venue's price, never this mount's
            // mark) and its drain. The reference venue rides only in the `venue` argument handed
            // to the hook.
            let (m_venue, m_symbol) = {
                let m = self.mounts[idx].as_ref().expect("just found");
                (m.venue.clone(), m.symbol.clone())
            };
            // ⚠ THE MOUNT'S OWN SERIES, not the tick's — see this method's doc. Passing `(venue,
            // symbol)` here would place the maker's own quotes on the reference venue and split
            // the tag registry across two keys.
            self.step_mount_hook(idx, &m_venue, &m_symbol, None, ts, |_, mount, ctx| {
                mount.strategy.on_reference_quote(ctx, venue, &q);
            });
        }
    }

    /// Route a live-parameter update ([`Command::UpdateParams`]) to ONE mount and run its
    /// `on_params_updated` hook (the live-parameter plane, audit co8). The occasional-control-event
    /// twin of [`Self::drive_strategy_feed_status`], with a single addressee: a RARE re-tune command,
    /// never a market message, so the p99<10µs event fold is untouched and NO OMS state is folded —
    /// only the mounted strategy hot-swaps its tunables, plus any orders that hook buffers, drained
    /// through the one live path (the maker buffers none: it re-prices its resting orders IN PLACE on
    /// the next tick, so queue position is preserved). No mark rides a param update; `price` is the
    /// standing mark if one exists, else 0.0. NOT warmup-gated (a re-tune is a control fact, like a
    /// status change).
    ///
    /// WHICH mount is [`Self::params_target`]'s rule, and every case it does not deliver is a
    /// recent-events note with nothing retuned: a re-tune that vanished silently, or reached a
    /// mount the operator did not mean, is worse than one that said no (the same contract as
    /// `MountStrategy` / `UnmountStrategy`).
    pub(crate) fn drive_strategy_params(&mut self, update: &ParamsUpdate) {
        let ParamsUpdate { venue, symbol, params, .. } = update;
        let Some(idx) = self.params_target(update) else {
            return;
        };
        let now = self.engine.now_ms; // stamped at the top of dispatch()
        // `params_target` delivers only to a mount whose own (venue, symbol, interval) IS the
        // payload's (`Audience::OwnSeries`), so the mount's interval keys the same bar series.
        self.step_mount_hook(idx, venue, symbol, None, now, |_, mount, ctx| {
            mount.strategy.on_params_updated(ctx, params);
        });
    }

    /// The slot [`Self::drive_strategy_params`] delivers `update` to, or `None` after saying why in
    /// recent-events (a COLD path: it runs only when the command arrives, so it allocates freely).
    ///
    /// - `mount_id: Some(id)`: `id` is sanitized the way ids are STORED (`maker-a` finds `maker_a`,
    ///   the `unmount_strategy_runtime` identity law) and must name a LIVE slot whose own
    ///   `(venue, symbol, interval)` is the payload's. An empty id, an unknown or unmounted id and
    ///   a triple that disagrees with the mount (a client acting on a stale view of what the id is
    ///   mounted on) each refuse; none falls back to the triple.
    /// - `mount_id: None`: the pre-field shape. Exactly one live mount on the triple
    ///   ([`Audience::OwnSeries`]) is delivered to as before; none is a note; TWO OR MORE are a note
    ///   naming them, because the first of them is not a safe guess for a re-tune (decision 0110
    ///   recorded the first-match lane as a known gap, this is its fix).
    fn params_target(&mut self, update: &ParamsUpdate) -> Option<usize> {
        let ParamsUpdate { venue, symbol, interval, mount_id, .. } = update;
        let own = Audience::OwnSeries { interval };
        let Some(raw) = mount_id.as_deref() else {
            let hearers: Vec<usize> = (0..self.mounts.len())
                .filter(|&i| self.mount_hears(i, venue, symbol, own))
                .collect();
            return match hearers[..] {
                [only] => Some(only),
                [] => {
                    self.note(format!(
                        "PARAMS REFUSED: no live strategy mount on {venue}/{symbol} @ {interval}"
                    ));
                    None
                }
                _ => {
                    let ids: Vec<String> =
                        hearers.iter().map(|&i| format!("`{}`", self.mount_ids[i])).collect();
                    self.note(format!(
                        "PARAMS REFUSED: {} mounts share {venue}/{symbol} @ {interval} ({}) — \
                         address one by mount id",
                        hearers.len(),
                        ids.join(", ")
                    ));
                    None
                }
            };
        };
        if raw.trim().is_empty() {
            self.note("PARAMS REFUSED: empty mount id".to_string());
            return None;
        }
        let want = crate::strategy_state::mount_id_with(Some(raw), "", "", "");
        let Some(i) =
            (0..self.mounts.len()).find(|&i| self.mounts[i].is_some() && self.mount_ids[i] == want)
        else {
            self.note(format!("PARAMS REFUSED: no live strategy mount with id `{want}`"));
            return None;
        };
        if !self.mount_hears(i, venue, symbol, own) {
            let m = self.mounts[i].as_ref().expect("a live slot was just found");
            let (mv, ms, mi) = (m.venue.clone(), m.symbol.clone(), m.interval.clone());
            self.note(format!(
                "PARAMS REFUSED: mount `{want}` runs {mv}/{ms} @ {mi}, not {venue}/{symbol} @ \
                 {interval} (the sender's view of it is stale)"
            ));
            return None;
        }
        Some(i)
    }

    /// Fire every mount's WALL-CLOCK schedule that crossed an instant at this boundary pass
    /// (steal/core-live-scheduler) — the live twin of the backtest engine's per-bar
    /// `Schedule::check_due` → `Strategy::on_schedule` loop, with IDENTICAL rule semantics
    /// ([`vike_model::time::schedule::TimeRule`] is the one shared crossing law). Called ONLY from
    /// the drain-loop boundary (gated on `any_mount_schedule` in `run`'s boundary block — no timer-wheel
    /// entry, see that block's comment for why the clock VALUE drives this rather than a wheel
    /// deadline), NEVER per market message, so the p99 fold is untouched. A mount with an empty
    /// [`crate::schedule::LiveSchedule`] contributes nothing (its `check_due` returns empty), so a
    /// schedule-free mount costs one empty-Vec iteration even while another mount's schedule is live.
    ///
    /// Firing order: each due `tag` is journaled write-ahead as a replay-neutral
    /// [`vike_journal::JournalRecord::ScheduleFire`] audit marker, then `on_schedule` runs (guarded
    /// like every other boundary strategy-hook call — it is arbitrary user code fired OUTSIDE
    /// `dispatch`'s panic guard). The orders it buffers drain through the ONE live path
    /// ([`Self::drain_broker`] → `apply_strategy_intent`), journaled on their own as `StrategySubmit`
    /// records that replay re-applies — so the fire needs no replayable command of its own and the
    /// schedule never forfeits replay (see `ScheduleFire`'s doc).
    pub(crate) fn drive_schedule(&mut self, now_ms: i64) {
        for idx in 0..self.mounts.len() {
            // wall-clock rules due at this poll (mutably latches each fired instant per rule)
            let due = self.mount_schedule[idx].check_due(now_ms);
            if due.is_empty() {
                continue;
            }
            // resolve the mount's routing; skip a transiently-taken slot (mid another hook dispatch —
            // never actually reachable from this boundary, defensive like the other drivers)
            let (venue, symbol) = match self.mounts[idx].as_ref() {
                Some(m) => (m.venue.clone(), m.symbol.clone()),
                None => continue,
            };
            // A schedule fires on the mount's OWN series, so only the legs get declared rows — and
            // a scheduled two-leg rebalance is exactly the hook that wants them.
            self.step_mount_hook(idx, &venue, &symbol, None, now_ms, |core, mount, ctx| {
                for tag in &due {
                    // WRITE-AHEAD: journal the fire DECISION before the on_schedule it drives. A
                    // replay-neutral audit marker (the on_schedule ORDERS journal on their own as
                    // StrategySubmit at the drain); off-fold + journal-gated so the no-journal path
                    // stays byte-identical.
                    if let Some(journal) = core.journal.as_mut() {
                        let mid = core.mount_ids[idx].clone();
                        journal.append_schedule_fire(now_ms, &mid, tag).expect("journal append");
                        core.journaled_since_snap += 1;
                    }
                    // on_schedule is arbitrary user code fired from the boundary (NOT inside
                    // `dispatch`'s catch_unwind), so guard it per tag like `save_all_strategy_state`
                    // does — a panic in one tag must neither kill the "vt-core" thread nor skip the
                    // mounts after it; `ctx` keeps whatever orders an earlier tag already buffered.
                    if let Err(payload) = catch_unwind(AssertUnwindSafe(|| {
                        mount.strategy.on_schedule(&mut *ctx, tag)
                    })) {
                        tracing::warn!(
                            target: "vike_core::schedule",
                            tag = %tag,
                            reason = %panic_text(payload),
                            "strategy on_schedule panicked (best-effort, continuing)"
                        );
                    }
                }
            });
        }
    }
}
