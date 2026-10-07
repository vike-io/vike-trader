//! The non-bar strategy hooks: tick, feed status, flow, mark, reference quote, params, schedule.

use super::*;

impl<C: ExecutionClient> CoreThread<C> {
    /// The tick-lane twin of [`drive_strategy`] for a SUB-BAR update (quote/trade/book). Marks to
    /// `price`, then — only if a strategy is mounted on this (venue, symbol) — runs `dispatch`
    /// over a `LiveBroker` snapshot and drains its submissions through the one live path. No bar
    /// is appended and the OMS event-fold path is untouched, so this is parity-neutral (net-new
    /// Rust surface, no Python twin). Mirrors `MseCore`'s tick dispatch on the backtest side.
    pub(crate) fn drive_strategy_tick(
        &mut self,
        venue: &str,
        symbol: &str,
        price: f64,
        now: i64,
        dispatch: impl FnOnce(&mut Box<dyn Strategy<LiveBroker> + Send>, &mut LiveBroker),
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
            // Phase C (opt-in): intra-bar conditional triggering off the tick lanes —
            // the fidelity upgrade Python's bar-close book lacks (conditionals.py)
            if self.config.conditionals_on_ticks {
                self.fire_conditionals_at_price(venue, symbol, price, now);
            }
        }
        // A mount also receives the TICKS (quote/trade/book) of any symbol it DECLARED.
        // `any_mount_multi` is false for every runtime today, so this stays the single string
        // compare it was.
        //
        // ⚠ SAME-VENUE ONLY. The predicate below requires `m.venue == venue` BEFORE it consults a
        // declared leg, so a leg declared on a DIFFERENT venue (`MountLeg::at`) never arrives here.
        // That is deliberate, not an omission: draining a foreign venue's tick would hand the
        // strategy a broker whose drain series is the FOREIGN one, and a symbol-less tagged maker
        // quote buffered there would resolve to — and be placed on — the wrong venue. Cross-venue
        // touches ride the disjoint `drive_strategy_reference_quote` lane instead, which drains on
        // the mount's OWN series.
        //
        // (An earlier revision of this comment claimed the BAR lane still matches the full
        // `(venue, symbol, interval)` triple and that live bars carry no symbol. BOTH are stale:
        // `drive_strategy` gained the same declared arm, and the `Ingest::BarClose` arm stamps the
        // series symbol onto every live bar.)
        let any_multi = self.any_mount_multi;
        let Some(idx) = self.mounts.iter().enumerate().position(|(i, m)| {
            m.as_ref().is_some_and(|m| {
                m.venue == venue
                    && (m.symbol == symbol
                        || (any_multi && self.mount_symbols[i].iter().any(|l| l.symbol == symbol)))
            })
        }) else {
            return;
        };
        // the tick step sees the SAME closed-bar history on_bar would (the mount's series)
        let interval = self.mounts[idx].as_ref().expect("just found").interval.clone();
        let key = (venue.to_string(), symbol.to_string(), interval);
        let bars_arc = self.bars.get(&key).map(|s| Arc::clone(&s.closed)).unwrap_or_default();
        let index = bars_arc.len().saturating_sub(1);
        let teidx = self.route_of(EngineRoute::Mount(idx), venue).unwrap_or(0);
        // BEFORE the take: `declared_views` reads the mount's own (venue, symbol, interval) out of `self.mounts`.
        let views = self.declared_views(idx, None, venue);
        // take/replace so the strategy call can't alias the rest of the core state
        let mut mount = self.mounts[idx].take().expect("just found");
        let mut ctx = LiveBroker {
            positions: views.positions,
            prices: views.prices,
            bar_views: views.bar_views,
            position: self.eng(teidx).position_size_of(symbol, "BOTH"),
            price,
            equity: self.eng(teidx).sizing_equity(self.seed_of(teidx), &self.config.price_cfg),
            bars: bars_arc,
            index,
            now,
            multiplier: self.eng(teidx).account.multiplier_of(symbol),
            lot_size: self.eng(teidx).gate.limits.lot_size.unwrap_or(0.0),
            submissions: Vec::new(),
            modifications: Vec::new(),
            cancels: Vec::new(),
            brackets: Vec::new(),
            conditionals: Vec::new(),
            mass_cancel: false,
        };
        if index >= mount.strategy.warmup() {
            dispatch(&mut mount.strategy, &mut ctx);
        }
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
        let idxs: Vec<usize> = self
            .mounts
            .iter()
            .enumerate()
            .filter(|(_, m)| m.as_ref().is_some_and(|m| m.venue == venue && m.symbol == symbol))
            .map(|(i, _)| i)
            .collect();
        if idxs.is_empty() {
            return;
        }
        let now = self.engine.now_ms; // stamped at the top of dispatch()
        for idx in idxs {
            // …per MOUNT: two mounts on one (venue, symbol) may name two ACCOUNTS, and each must be
            // told about its own book. Hoisting this out of the loop is what made it a per-venue
            // answer, and there is nothing per-venue about it.
            let eidx = self.route_of(EngineRoute::Mount(idx), venue).unwrap_or(0);
            // the status step sees the SAME closed-bar history on_bar would (the mount's series)
            let interval = self.mounts[idx].as_ref().expect("just found").interval.clone();
            let key = (venue.to_string(), symbol.to_string(), interval);
            let bars_arc = self.bars.get(&key).map(|s| Arc::clone(&s.closed)).unwrap_or_default();
            let index = bars_arc.len().saturating_sub(1);
            // no price rides a status change — use the standing mark if one exists, else 0.0
            let mark = self.eng(eidx).account.mark_of(venue, symbol).unwrap_or(0.0);
            // BEFORE the take: the declared per-symbol read tables (empty for a single-symbol mount,
            // hence byte-identical). This lane dispatches the MOUNT's own pair, so the own-symbol row
            // is skipped and the scalars below answer for it; the legs get their own numbers, which
            // matters because a feed death is precisely when a two-leg strategy wants to know what
            // inventory it is holding on the OTHER leg.
            let views = self.declared_views(idx, None, venue);
            // take/replace so the strategy call can't alias the rest of the core state
            let mut mount = self.mounts[idx].take().expect("just found");
            let mut ctx = LiveBroker {
                positions: views.positions,
                prices: views.prices,
                bar_views: views.bar_views,
                position: self.eng(eidx).position_size_of(symbol, "BOTH"),
                price: mark,
                equity: self.eng(eidx).sizing_equity(self.seed_of(eidx), &self.config.price_cfg),
                bars: bars_arc,
                index,
                now,
                multiplier: self.eng(eidx).account.multiplier_of(symbol),
                lot_size: self.eng(eidx).gate.limits.lot_size.unwrap_or(0.0),
                submissions: Vec::new(),
                modifications: Vec::new(),
                cancels: Vec::new(),
                brackets: Vec::new(),
                conditionals: Vec::new(),
                mass_cancel: false,
            };
            mount.strategy.on_feed_status(&mut ctx, status);
            self.mounts[idx] = Some(mount);
            self.drain_broker(ctx, venue, symbol, now, idx);
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
        let idxs: Vec<usize> = self
            .mounts
            .iter()
            .enumerate()
            .filter(|(_, m)| m.as_ref().is_some_and(|m| m.venue == venue && m.symbol == symbol))
            .map(|(i, _)| i)
            .collect();
        if idxs.is_empty() {
            return;
        }
        let now = self.engine.now_ms; // stamped at the top of dispatch()
        for idx in idxs {
            // …per MOUNT — the feed-status twin's note applies verbatim.
            let eidx = self.route_of(EngineRoute::Mount(idx), venue).unwrap_or(0);
            // the flow step sees the SAME closed-bar history on_bar would (the mount's series)
            let interval = self.mounts[idx].as_ref().expect("just found").interval.clone();
            let key = (venue.to_string(), symbol.to_string(), interval);
            let bars_arc = self.bars.get(&key).map(|s| Arc::clone(&s.closed)).unwrap_or_default();
            let index = bars_arc.len().saturating_sub(1);
            // no price rides a toxicity reading — use the standing mark if one exists, else 0.0
            let mark = self.eng(eidx).account.mark_of(venue, symbol).unwrap_or(0.0);
            // BEFORE the take: the declared per-symbol read tables (empty for a single-symbol mount,
            // hence byte-identical). Same shape as the feed-status twin above — this lane dispatches
            // the MOUNT's own pair, so only the legs get rows.
            let views = self.declared_views(idx, None, venue);
            // take/replace so the strategy call can't alias the rest of the core state
            let mut mount = self.mounts[idx].take().expect("just found");
            let mut ctx = LiveBroker {
                positions: views.positions,
                prices: views.prices,
                bar_views: views.bar_views,
                position: self.eng(eidx).position_size_of(symbol, "BOTH"),
                price: mark,
                equity: self.eng(eidx).sizing_equity(self.seed_of(eidx), &self.config.price_cfg),
                bars: bars_arc,
                index,
                now,
                multiplier: self.eng(eidx).account.multiplier_of(symbol),
                lot_size: self.eng(eidx).gate.limits.lot_size.unwrap_or(0.0),
                submissions: Vec::new(),
                modifications: Vec::new(),
                cancels: Vec::new(),
                brackets: Vec::new(),
                conditionals: Vec::new(),
                mass_cancel: false,
            };
            mount.strategy.on_flow(&mut ctx, flow);
            self.mounts[idx] = Some(mount);
            self.drain_broker(ctx, venue, symbol, now, idx);
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
    /// The routing PREDICATE is the only real difference from the feed-status twin: it matches
    /// `underlying_symbol == Some(symbol)`, not `symbol` itself, and the broker ctx is built on the
    /// matching mount's OWN `(venue, symbol)` — the series it trades — with `now = ts` (the mark's
    /// event time). The underlying rides ONLY in the `MarkTick`. NOT warmup-gated (an underlying
    /// observation is a market fact regardless of bar count, mirroring `on_feed_status`). EARLY-RETURN
    /// when no mount declares this underlying — a run with no underlying-anchored mount never allocates
    /// here, so it is byte-identical.
    pub(crate) fn drive_strategy_mark(&mut self, venue: &str, symbol: &str, price: f64, ts: i64) {
        let idxs: Vec<usize> = self
            .mounts
            .iter()
            .enumerate()
            .filter(|(_, m)| {
                m.as_ref().is_some_and(|m| {
                    m.venue == venue && m.underlying_symbol.as_deref() == Some(symbol)
                })
            })
            .map(|(i, _)| i)
            .collect();
        if idxs.is_empty() {
            return;
        }
        let mark = MarkTick { symbol: symbol.to_string(), price, ts };
        // `venue` already == every matched mount's venue (the predicate requires it), so it is used
        // verbatim below; only the mount's OWN symbol/interval differ from the underlying.
        for idx in idxs {
            // …per MOUNT — the feed-status twin's note applies verbatim.
            let eidx = self.route_of(EngineRoute::Mount(idx), venue).unwrap_or(0);
            // the mount's OWN series (venue, symbol) — the token it trades — NOT the underlying it
            // merely watches (which rides only in `mark`).
            let (m_symbol, interval) = {
                let m = self.mounts[idx].as_ref().expect("just found");
                (m.symbol.clone(), m.interval.clone())
            };
            // the mark step sees the SAME closed-bar history on_bar would (the mount's OWN series)
            let key = (venue.to_string(), m_symbol.clone(), interval);
            let bars_arc = self.bars.get(&key).map(|s| Arc::clone(&s.closed)).unwrap_or_default();
            let index = bars_arc.len().saturating_sub(1);
            // the underlying mark carries no token price — use the mount symbol's standing mark, else 0.0
            let mark_px = self.eng(eidx).account.mark_of(venue, &m_symbol).unwrap_or(0.0);
            // BEFORE the take: the declared per-symbol read tables (empty for a single-symbol mount,
            // hence byte-identical). The ctx is built on the mount's OWN series, so THAT is the
            // dispatching symbol here — the UNDERLYING this lane is named for rides only in `mark`
            // and is not a declared leg at all (it is `StrategyMount::underlying_symbol`).
            let views = self.declared_views(idx, None, venue);
            // take/replace so the strategy call can't alias the rest of the core state
            let mut mount = self.mounts[idx].take().expect("just found");
            let mut ctx = LiveBroker {
                positions: views.positions,
                prices: views.prices,
                bar_views: views.bar_views,
                position: self.eng(eidx).position_size_of(&m_symbol, "BOTH"),
                price: mark_px,
                equity: self.eng(eidx).sizing_equity(self.seed_of(eidx), &self.config.price_cfg),
                bars: bars_arc,
                index,
                now: ts,
                multiplier: self.eng(eidx).account.multiplier_of(&m_symbol),
                lot_size: self.eng(eidx).gate.limits.lot_size.unwrap_or(0.0),
                submissions: Vec::new(),
                modifications: Vec::new(),
                cancels: Vec::new(),
                brackets: Vec::new(),
                conditionals: Vec::new(),
                mass_cancel: false,
            };
            mount.strategy.on_mark(&mut ctx, &mark);
            self.mounts[idx] = Some(mount);
            self.drain_broker(ctx, venue, &m_symbol, ts, idx);
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
    /// ([`Self::drive_strategy_mark`]) — gates on `m.venue == <the event's venue>` BEFORE it ever
    /// consults a declared leg. So a leg declared `MountLeg::at("BTC-USDT-SWAP", "okx")` on a
    /// hyperliquid mount had its orders routed to okx and NEVER received okx's quotes. A
    /// cross-exchange maker prices its resting quotes off the other venue's touch, so without this
    /// lane the strategy has no input at all.
    ///
    /// ## The predicate, and why it is `!=` rather than a widening
    ///
    /// `m.venue != venue && <some declared leg matches (symbol, venue) exactly>`. The `!=` makes
    /// this lane DISJOINT from [`Self::drive_strategy_tick`], whose predicate requires `==`: one
    /// tick can never reach one mount through both, so nothing is dispatched twice. Widening the
    /// tick lane instead — the obvious alternative — was rejected because it would make a
    /// foreign-venue tick an EMISSION surface keyed on the FOREIGN series (see the drain note
    /// below), which silently misroutes the maker's own quotes.
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
    #[allow(clippy::too_many_arguments)]
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
        let idxs: Vec<usize> = self
            .mounts
            .iter()
            .enumerate()
            .filter(|(i, m)| {
                m.as_ref().is_some_and(|m| {
                    m.venue != venue
                        && self.mount_symbols[*i]
                            .iter()
                            .any(|l| l.symbol == symbol && l.venue.as_deref() == Some(venue))
                })
            })
            .map(|(i, _)| i)
            .collect();
        if idxs.is_empty() {
            return;
        }
        // Built AFTER the match so a runtime with a cross-venue mount still allocates nothing for
        // the (far more numerous) ticks of venues/symbols no mount declared.
        let q = vike_model::QuoteTick {
            ts,
            local_ts: 0,
            bid,
            ask,
            bid_size,
            ask_size,
            symbol: symbol.to_string(),
        };
        for idx in idxs {
            // The mount's OWN (venue, symbol, interval) — the series it RESTS on. The reference
            // venue rides only in the `venue` argument handed to the hook.
            let (m_venue, m_symbol, interval) = {
                let m = self.mounts[idx].as_ref().expect("just found");
                (m.venue.clone(), m.symbol.clone(), m.interval.clone())
            };
            let eidx = self.route_of(EngineRoute::Mount(idx), &m_venue).unwrap_or(0);
            let key = (m_venue.clone(), m_symbol.clone(), interval);
            let bars_arc = self.bars.get(&key).map(|s| Arc::clone(&s.closed)).unwrap_or_default();
            let index = bars_arc.len().saturating_sub(1);
            // The reference touch is a FOREIGN venue's price — never this mount's mark. Use the
            // mount symbol's standing mark, else 0.0 (the `drive_strategy_mark` convention).
            let mark_px = self.eng(eidx).account.mark_of(&m_venue, &m_symbol).unwrap_or(0.0);
            // The mount's OWN series is what this lane dispatches on (see the doc above), so the
            // own-symbol row is skipped and the scalar below answers about it — byte-identical.
            let views = self.declared_views(idx, None, &m_venue);
            // take/replace so the strategy call can't alias the rest of the core state
            let mut mount = self.mounts[idx].take().expect("just found");
            let mut ctx = LiveBroker {
                positions: views.positions,
                prices: views.prices,
                bar_views: views.bar_views,
                position: self.eng(eidx).position_size_of(&m_symbol, "BOTH"),
                price: mark_px,
                equity: self.eng(eidx).sizing_equity(self.seed_of(eidx), &self.config.price_cfg),
                bars: bars_arc,
                index,
                now: ts,
                multiplier: self.eng(eidx).account.multiplier_of(&m_symbol),
                lot_size: self.eng(eidx).gate.limits.lot_size.unwrap_or(0.0),
                submissions: Vec::new(),
                modifications: Vec::new(),
                cancels: Vec::new(),
                brackets: Vec::new(),
                conditionals: Vec::new(),
                mass_cancel: false,
            };
            mount.strategy.on_reference_quote(&mut ctx, venue, &q);
            self.mounts[idx] = Some(mount);
            // ⚠ THE MOUNT'S OWN SERIES, not the tick's — see this method's doc. Changing these two
            // arguments to `(venue, symbol)` would place the maker's own quotes on the reference
            // venue and split the tag registry across two keys.
            self.drain_broker(ctx, &m_venue, &m_symbol, ts, idx);
        }
    }

    /// Route a live-parameter update ([`Command::UpdateParams`]) to the ONE mount on this EXACT
    /// (venue, symbol, interval) — the same key the bar path resolves a mount by — and run its
    /// `on_params_updated` hook (the live-parameter plane, audit co8). The occasional-control-event
    /// twin of [`Self::drive_strategy_feed_status`], single-mount: a RARE re-tune command, never a
    /// market message, so the p99<10µs event fold is untouched and NO OMS state is folded — only the
    /// mounted strategy hot-swaps its tunables, plus any orders that hook buffers, drained through
    /// the one live path (the maker buffers none: it re-prices its resting orders IN PLACE on the
    /// next tick, so queue position is preserved). No mark rides a param update; `price` is the
    /// standing mark if one exists, else 0.0. NOT warmup-gated (a re-tune is a control fact, like a
    /// status change). No mount on the key ⇒ no-op (silently ignored, like an unknown modify tag).
    pub(crate) fn drive_strategy_params(
        &mut self,
        venue: &str,
        symbol: &str,
        interval: &str,
        params: &StrategyParams,
    ) {
        let Some(idx) = self.mounts.iter().position(|m| {
            m.as_ref()
                .is_some_and(|m| m.venue == venue && m.symbol == symbol && m.interval == interval)
        }) else {
            return;
        };
        let eidx = self.route_of(EngineRoute::Mount(idx), venue).unwrap_or(0);
        let now = self.engine.now_ms; // stamped at the top of dispatch()
        // the update step sees the SAME closed-bar history on_bar would (the mount's series)
        let key = (venue.to_string(), symbol.to_string(), interval.to_string());
        let bars_arc = self.bars.get(&key).map(|s| Arc::clone(&s.closed)).unwrap_or_default();
        let index = bars_arc.len().saturating_sub(1);
        // no price rides a param update — use the standing mark if one exists, else 0.0
        let mark = self.eng(eidx).account.mark_of(venue, symbol).unwrap_or(0.0);
        // BEFORE the take: the declared per-symbol read tables (empty for a single-symbol mount,
        // hence byte-identical). This lane resolves the mount by its own exact
        // (venue, symbol, interval), so only the legs get rows.
        let views = self.declared_views(idx, None, venue);
        // take/replace so the strategy call can't alias the rest of the core state
        let mut mount = self.mounts[idx].take().expect("just found");
        let mut ctx = LiveBroker {
            positions: views.positions,
            prices: views.prices,
            bar_views: views.bar_views,
            position: self.eng(eidx).position_size_of(symbol, "BOTH"),
            price: mark,
            equity: self.eng(eidx).sizing_equity(self.seed_of(eidx), &self.config.price_cfg),
            bars: bars_arc,
            index,
            now,
            multiplier: self.eng(eidx).account.multiplier_of(symbol),
            lot_size: self.eng(eidx).gate.limits.lot_size.unwrap_or(0.0),
            submissions: Vec::new(),
            modifications: Vec::new(),
            cancels: Vec::new(),
            brackets: Vec::new(),
            conditionals: Vec::new(),
            mass_cancel: false,
        };
        mount.strategy.on_params_updated(&mut ctx, params);
        self.mounts[idx] = Some(mount);
        self.drain_broker(ctx, venue, symbol, now, idx);
    }

    /// Fire every mount's WALL-CLOCK schedule that crossed an instant at this boundary pass
    /// (steal/core-live-scheduler) — the live twin of the backtest engine's per-bar
    /// `Schedule::check_due` → `Strategy::on_schedule` loop, with IDENTICAL rule semantics
    /// ([`crate::schedule::TimeRule`] is the one shared crossing law). Called ONLY from the
    /// drain-loop boundary (gated on `any_mount_schedule` in `run`'s boundary block — no timer-wheel
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
            let (venue, symbol, interval) = match self.mounts[idx].as_ref() {
                Some(m) => (m.venue.clone(), m.symbol.clone(), m.interval.clone()),
                None => continue,
            };
            let eidx = self.route_of(EngineRoute::Mount(idx), &venue).unwrap_or(0);
            // the schedule step sees the SAME closed-bar history on_bar would (the mount's series)
            let key = (venue.clone(), symbol.clone(), interval);
            let bars_arc = self.bars.get(&key).map(|s| Arc::clone(&s.closed)).unwrap_or_default();
            let index = bars_arc.len().saturating_sub(1);
            // no price rides a schedule fire — use the standing mark if one exists, else 0.0
            let mark = self.eng(eidx).account.mark_of(&venue, &symbol).unwrap_or(0.0);
            // BEFORE the take: the declared per-symbol read tables (empty for a single-symbol mount,
            // hence byte-identical). A schedule fires on the mount's OWN series, so only the legs get
            // rows — and a scheduled two-leg rebalance is exactly the hook that wants them.
            let views = self.declared_views(idx, None, &venue);
            // take/replace so the strategy call can't alias the rest of the core state
            let mut mount = self.mounts[idx].take().expect("just matched Some");
            let mut ctx = LiveBroker {
                positions: views.positions,
                prices: views.prices,
                bar_views: views.bar_views,
                position: self.eng(eidx).position_size_of(&symbol, "BOTH"),
                price: mark,
                equity: self.eng(eidx).sizing_equity(self.seed_of(eidx), &self.config.price_cfg),
                bars: bars_arc,
                index,
                now: now_ms,
                multiplier: self.eng(eidx).account.multiplier_of(&symbol),
                lot_size: self.eng(eidx).gate.limits.lot_size.unwrap_or(0.0),
                submissions: Vec::new(),
                modifications: Vec::new(),
                cancels: Vec::new(),
                brackets: Vec::new(),
                conditionals: Vec::new(),
                mass_cancel: false,
            };
            for tag in &due {
                // WRITE-AHEAD: journal the fire DECISION before the on_schedule it drives. A
                // replay-neutral audit marker (the on_schedule ORDERS journal on their own as
                // StrategySubmit at the drain below); off-fold + journal-gated so the no-journal path
                // stays byte-identical.
                if let Some(journal) = self.journal.as_mut() {
                    let mid = self.mount_ids[idx].clone();
                    journal.append_schedule_fire(now_ms, &mid, tag).expect("journal append");
                    self.journaled_since_snap += 1;
                }
                // on_schedule is arbitrary user code fired from the boundary (NOT inside `dispatch`'s
                // catch_unwind), so guard it per tag like `save_all_strategy_state` does — a panic in
                // one tag must neither kill the "vt-core" thread nor skip the mounts after it; `ctx`
                // keeps whatever orders an earlier tag already buffered.
                if let Err(payload) =
                    catch_unwind(AssertUnwindSafe(|| mount.strategy.on_schedule(&mut ctx, tag)))
                {
                    tracing::warn!(
                        target: "vike_core::schedule",
                        tag = %tag,
                        reason = %panic_text(payload),
                        "strategy on_schedule panicked (best-effort, continuing)"
                    );
                }
            }
            self.mounts[idx] = Some(mount);
            self.drain_broker(ctx, &venue, &symbol, now_ms, idx);
        }
    }
}
