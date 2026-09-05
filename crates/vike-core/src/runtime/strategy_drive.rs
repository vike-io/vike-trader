//! Strategy-driving cluster — the bar/tick/feed-status/params dispatch onto the mounted
//! strategies, the [`LiveBroker`] drain, and conditional-order firing — split out of the runtime
//! fold module (behavior byte-identical; the block moved verbatim). `use super::*` re-exports the
//! parent runtime module's full import set + items, so nothing about resolution changes.

use super::*;

/// The owned per-symbol read tables a DECLARED-multi mount's [`LiveBroker`] carries
/// (`Default` = all three empty, which is every single-symbol mount and costs nothing).
#[derive(Default)]
struct DeclaredViews {
    positions: Vec<(String, f64)>,
    prices: Vec<(String, f64)>,
    /// The [`vike_model::Broker::bars`] table — and, because it is the only one of the three that is
    /// COMPLETE for the declared set, the membership oracle `LiveBroker::carries` reads. See
    /// [`CoreThread::push_view`].
    bar_views: Vec<(String, Arc<Vec<Bar>>)>,
}

impl<C: ExecutionClient> CoreThread<C> {
    /// The R7 strategy step for one CLOSED bar of the mounted series — the live twin of
    /// the engine's `step`: (1) the client fills its resting book against this bar
    /// (next-open discipline) and the fills fold through the bus; (2) the mark moves to
    /// the close; (3) the strategy decides on the full closed history and its orders run
    /// the ONE live path (mint → RiskGate → client).
    pub(crate) fn drive_strategy(&mut self, key: &SeriesKey, bar: &Bar) {
        // (1)+(2) fire for EVERY closed bar of any symbol any engine accepts — manual
        // ticket orders paper-fill on live bars even without a strategy mount
        let Some(eidx) = self.engine_idx_for_route_key(RouteKey::sole_account_of(&key.0)) else {
            return;
        };
        if !self.eng(eidx).accepts_symbol(&key.1) {
            return;
        }
        // (1) happened in the BarClose arm: paper fills + their `on_fill` deliveries fold
        // BEFORE the bar joins the cache (backtest ordering: fill → on_fill → index/price
        // advance → on_bar)
        // (2) mark to the close (the engine sets price=close after fills). THIS IS THE LOSSLESS
        // closed-bar lane, and it fires for exactly the symbols an engine holds positions in —
        // so before the law moved into `Account::set_mark_from` this was the highest-frequency
        // stomp of a fresh venue mark, ten lines above the margin-call sweep that reads it.
        let now = self.eng(eidx).now_ms;
        self.eng_mut(eidx).account.set_mark_from(
            &key.0,
            &key.1,
            bar.close,
            MarkSource::BarClose,
            now,
        );
        self.eng_mut(eidx).price_board.set_bar_close(&key.0, &key.1, bar.close, bar.ts);
        // …and onto the venue's OTHER accounts: a bar close is a fact about the exchange, and a
        // second account of it must be able to price its own positions. Inert (one bool read) for
        // every single-account process.
        self.mirror_venue_price(&key.0, &key.1, bar.close, MarkSource::BarClose, now, Some(bar.ts));
        self.dirty = true;
        // Phase B margin-call sweep (opt-in): LEAN's cadence is a 5-min timer; the vike
        // twin sweeps per closed bar of the engine's own series — marks are fresh here and
        // this is the per-bar path, never the event fold (latency gate untouched).
        if let Some(cfg) = self.config.margin_call {
            self.sweep_margin_call(&cfg, bar.ts);
        }
        // Equity-drawdown latch (audit exec#4): same per-closed-bar cadence + equity source as
        // the margin-call sweep above (marks fresh, off the event fold). Disabled unless opted in.
        if let Some(threshold) = self.config.max_drawdown {
            self.sweep_drawdown_latch(threshold);
        }
        // Per-mount BUDGET latch (steal/core-per-mount-budget): the SCOPED sibling of the drawdown
        // latch above — same per-closed-bar cadence + resolver-priced source, but it latches ONE
        // mount at a time (cancel its orders + optional flatten) instead of the whole account, so
        // other mounts keep trading. Gated to a single bool read when no mount has an active budget
        // (the default), byte-identical to a budget-free runtime. Runs BEFORE the strategy step
        // below so a mount latched this bar has its own post-latch intents discarded at drain.
        if self.any_mount_budget {
            self.sweep_mount_budgets(bar.ts);
        }
        // Phase C: conditional-order check BEFORE the strategy step (oracle firing order —
        // `check_conditionals(sym, bar)` runs before `strategy.on_bar` in LivePump)
        self.fire_conditionals_bar(&key.0.clone(), &key.1.clone(), bar);
        // (3) the strategy step for the mount on this (venue, interval) whose symbol is either its
        // OWN or one it DECLARED — the bar twin of the tick-lane predicate below.
        //
        // Together with the series-symbol stamp in `mod.rs`'s `BarClose` arm, this is what lets a
        // two-leg strategy work at all: it now RECEIVES both legs' bars and can tell them apart by
        // `bar.symbol`. Widening the routing without the stamp would have been useless (two
        // indistinguishable `symbol: None` bars), and the stamp without the widening would have
        // left the second leg's bars never arriving.
        //
        // `any_mount_multi` is false for every runtime today, so this stays the same tuple compare
        // it was — the declared arm is not even evaluated.
        let any_multi = self.any_mount_multi;
        let Some(idx) = self.mounts.iter().enumerate().position(|(i, m)| {
            m.as_ref().is_some_and(|m| {
                m.venue == key.0
                    && m.interval == key.2
                    && (m.symbol == key.1
                        || (any_multi && self.mount_symbols[i].iter().any(|l| l.symbol == key.1)))
            })
        }) else {
            return;
        };
        // (3) the strategy sees the full closed series (no look-ahead: only <= this bar)
        let bars_arc = Arc::clone(&self.bars.get(key).expect("series just appended").closed);
        let index = bars_arc.len() - 1;
        // ⚠ THE MOUNT'S OWN ENGINE for everything the STRATEGY reads. `eidx` above is the engine the
        // MARKET message belongs to (the venue's default account — marks and the price board are
        // per-exchange facts); a mount that named an account reads its position, equity, multiplier
        // and lot size from THAT account's book. The two were one index until a mount could name an
        // account, and keeping them one would have left a labelled mount sizing against the default
        // account's position while trading its own — a worse defect than the routing one, and one no
        // order test would catch.
        let meidx = self.mount_eng(idx);
        // BEFORE the take: `declared_views` reads the mount's own (venue, symbol, interval) out of `self.mounts`.
        let views = self.declared_views(idx, None, &key.0);
        // take/replace so the strategy call can't alias the rest of the core state
        let mut mount = self.mounts[idx].take().expect("just found");
        let mut ctx = LiveBroker {
            positions: views.positions,
            prices: views.prices,
            bar_views: views.bar_views,
            position: self.eng(meidx).position_size_of(&key.1, "BOTH"),
            price: bar.close,
            equity: self.eng(meidx).resolved_equity(self.seed_of(meidx), &self.config.price_cfg),
            bars: bars_arc,
            index,
            now: bar.ts,
            multiplier: self.eng(meidx).account.multiplier_of(&key.1),
            lot_size: self.eng(meidx).gate.limits.lot_size.unwrap_or(0.0),
            submissions: Vec::new(),
            modifications: Vec::new(),
            cancels: Vec::new(),
            brackets: Vec::new(),
            conditionals: Vec::new(),
            mass_cancel: false,
        };
        if index >= mount.strategy.warmup() {
            mount.strategy.on_bar(&mut ctx, bar);
        }
        self.mounts[idx] = Some(mount);
        self.drain_broker(ctx, &key.0, &key.1, bar.ts, idx);
    }

    /// The per-symbol position/mark tables a DECLARED-multi mount's `Broker::position(sym)` /
    /// `price(sym)` read from.
    ///
    /// (This doc block used to carry [`Self::drain_broker`]'s own paragraphs as its lead — the two
    /// were one comment with no `fn` between them, so `drain_broker` rendered UNDOCUMENTED while its
    /// text advertised this method. They are separated here; no wording was dropped, it moved onto
    /// `drain_broker`.)
    ///
    /// ## ONE RULE: the table carries every declared instrument EXCEPT the DISPATCHING symbol
    ///
    /// `LiveBroker::declared_read` falls back to the `position`/`price` SCALARS when the requested
    /// symbol is absent from these tables, and those scalars describe the **dispatching** series
    /// (`key.1` on the bar lane, the tick's symbol on the tick lane, the FILL's symbol on the fill
    /// lane). So the two halves partition cleanly: the scalar owns the dispatching symbol, the table
    /// owns everything else the mount declared.
    ///
    /// Leaving the mount's OWN symbol out of the table was a silent misread, and it was reachable
    /// from shipped code: `vike_strategy::pairs::PairsZScore` evaluates from whichever leg's bar
    /// completes the pair, and its `flatten` reads `broker.position(&sym)` for BOTH legs before
    /// submitting `p.abs()` at `vike_model::closing_side(p)`. Reached from a leg-B dispatch it closed
    /// leg A with leg B's quantity — and, whenever the two legs' signs differ (the normal case for a
    /// beta-weighted spread), with the wrong SIDE. `SimBroker` indexes every read by symbol, so the
    /// same strategy backtested correctly. `crates/vike-backtest/tests/multi_symbol_read_parity.rs`
    /// pins the law in both engines.
    ///
    /// ⚠ **The dispatching symbol must NOT get a row, and that is load-bearing rather than an
    /// optimization.** On the fill lane the scalar is `AppliedFill::position_after` — the position
    /// after THIS fill, which is why a multi-fill batch shows each handler its own state rather than
    /// the batch's. A table row would answer from `position_size_of`, i.e. the account AFTER the
    /// whole batch folded, silently undoing that guarantee. On the bar/tick lanes the scalar is the
    /// fresher number too (this bar's close / this tick's price, versus the resolver's standing
    /// mark). The skip is by SYMBOL, not by (venue, symbol): `Broker::position` takes a symbol and
    /// nothing else, so a table keyed any finer could not be looked up, and a mount that declares
    /// its own symbol again on a foreign venue simply cannot address the two apart.
    ///
    /// ## Which ENGINE each row is read from — the same one the ORDER would be routed to
    ///
    /// A row resolves against `engine_idx_for_route_key(<that leg's venue>).unwrap_or(0)`, which is
    /// VERBATIM what `apply_intent` does with `OrderRequest::venue`, so a read and a write can never
    /// disagree about which book a leg lives in. Before this, every leg was read out of the
    /// DISPATCHING venue's engine, so a leg declared `MountLeg::at(sym, other_venue)` — the shape an
    /// xEMM hedge requires by construction — reported the MAKER venue's position and price for the
    /// hedge instrument. A same-venue leg (`MountLeg::same_venue`, and the mount's own symbol) still
    /// resolves the mount's own venue, which is the engine the old code used on every lane that
    /// built these tables, so that path is byte-identical.
    ///
    /// A leg naming a venue this runtime mounts no engine for falls back to engine 0 — again
    /// `apply_intent`'s rule, so its orders and its reads land in the same (wrong, misconfigured)
    /// place rather than in two different ones.
    ///
    /// Owned values, never a borrow of engine state — the strategy call must not be able to alias
    /// the rest of the core (the same rule the rest of the ctx follows). EMPTY (no allocation, no
    /// engine reads) for every single-symbol mount, which is what keeps the ordinary dispatch
    /// byte-identical; a mount that declared nothing gets `Default` even while a SIBLING mount is
    /// multi-symbol.
    /// ⚠ **RESIDUAL, stated rather than glossed: a declared row is POST-BATCH state, and only the
    /// dispatching symbol is per-fill.**
    ///
    /// Every row here is `position_size_of(sym)`, read when the views are built — which on the fill
    /// lane is AFTER the whole `applied_fills` batch has folded into the account. The dispatching
    /// symbol is protected (it has no row, so `LiveBroker::position` answers from
    /// `AppliedFill::position_after`), but a NON-dispatching declared leg is not: during
    /// `on_fill(fill_A)` a two-leg mount can read leg B's position reflecting a fill it has not yet
    /// been told about.
    ///
    /// That is look-ahead inside one batch, and a live-vs-backtest divergence — `SimBroker` folds
    /// and fires one fill at a time, so the same strategy sees per-fill state there and post-batch
    /// state here. Multi-fill batches are not hypothetical: a bar-close paper fill of two resting
    /// orders, or a reconcile fold, produces one.
    ///
    /// It is left as a residual rather than fixed because closing it means rebuilding each declared
    /// row from AppliedFill-era account state, which is a change to the fill fold rather than to
    /// this read — and the alternative shipped today (EMPTY tables, so a leg read answers with the
    /// DISPATCH's scalar, i.e. the wrong instrument entirely) is strictly worse than post-batch
    /// state for the right one. Naming it is the point: an earlier draft of this method claimed the
    /// per-fill guarantee was preserved for every leg, and it is not.
    fn declared_views(
        &self,
        mount_idx: usize,
        skip_symbol: Option<&str>,
        dispatch_venue: &str,
    ) -> DeclaredViews {
        if !self.any_mount_multi || self.mount_symbols[mount_idx].is_empty() {
            return DeclaredViews::default();
        }
        // A transiently-taken slot (mid another hook dispatch) leaves the mount's own venue unknown,
        // and guessing it would resolve every `venue: None` leg against the wrong engine. Not
        // reachable from any call site today — every one of them builds the views BEFORE its
        // `take()` — but silently answering from the wrong book is the whole defect this method
        // exists to remove, so it declines instead.
        // `interval` joins venue+symbol because the BAR table is keyed on the full `SeriesKey`
        // triple. A declared leg rides the MOUNT's interval by construction — the bar lane only
        // dispatches a leg whose `key.2` equals `m.interval` — so there is no second interval to
        // choose between.
        let Some((own_venue, own_symbol, interval)) = self.mounts[mount_idx]
            .as_ref()
            .map(|m| (m.venue.clone(), m.symbol.clone(), m.interval.clone()))
        else {
            return DeclaredViews::default();
        };
        let mut views = DeclaredViews::default();
        // The mount's OWN symbol goes in FIRST, so that a mount which also declares its own symbol
        // as a leg on a FOREIGN venue still reads its own book under that name: `declared_read`
        // takes the first match, and the mount's own instrument is the less surprising answer for
        // the mount's own symbol.
        self.push_view(mount_idx, &own_venue, &own_symbol, &interval, skip_symbol, &mut views);
        for leg in &self.mount_symbols[mount_idx] {
            // ⚠ `dispatch_venue`, NOT `own_venue`. This is the read half of [`Self::leg_venue`]'s
            // one rule: the write half resolves a `venue: None` leg against the venue `drain_broker`
            // was called with, so reading it against the MOUNT's venue instead would answer from a
            // different book than the very next `submit` writes to. See `leg_venue`'s doc for the
            // concrete cross-venue fill that makes them differ.
            let leg_venue = self.leg_venue(mount_idx, &leg.symbol, dispatch_venue);
            self.push_view(mount_idx, &leg_venue, &leg.symbol, &interval, skip_symbol, &mut views);
        }
        views
    }

    /// One symbol's row in [`Self::declared_views`]' tables, read out of `venue`'s own engine.
    ///
    /// Writes NOTHING to the position/price tables for the DISPATCHING symbol (the ctx scalars
    /// answer for it — see [`Self::declared_views`]) and nothing for a symbol already in a table, so
    /// `declared_read`'s first-match scan can never see two rows for one symbol.
    ///
    /// A price the resolver cannot answer (`Missing` — no mark, no quote, no trade, no bar for that
    /// cell) writes NO price row while the POSITION row still lands, which leaves `Broker::price` on
    /// its scalar fallback: there is no defensible number to invent, and a fabricated one would be
    /// indistinguishable from a real mark to the strategy reading it. (That fallback lands on the
    /// DISPATCHING series' price, which for a non-dispatching leg is the wrong instrument —
    /// `LiveBroker::price`'s doc carries the residual and why closing it is a separate change.)
    ///
    /// ## ⚠ The BAR row is written UNCONDITIONALLY — before the skip, for every declared symbol
    ///
    /// The two exclusions above are why neither table can answer "does this mount carry `sym`",
    /// and `LiveBroker` needs exactly that to tell an uncarried symbol (answer EMPTY) from the fill
    /// lane's dispatching one (answer from the scalar). `bar_views` is therefore COMPLETE by
    /// construction and is the oracle `LiveBroker::carries` reads:
    ///
    /// - no skip, because there is no per-fill freshness to protect — the `bars` scalar is the
    ///   MOUNT's own history on every lane but bar/tick, so a row for the dispatching symbol is
    ///   strictly better information, not a regression;
    /// - a series this runtime has no bars for yet still gets a row, holding an EMPTY `Arc`. Missing
    ///   history is "no bars", and dropping the row would say "not carried" — which would then feed
    ///   `position`/`price` a `0.0` for a leg the mount genuinely holds.
    ///
    /// Same `(venue, sym)` pair as the position row, hence the same `Self::leg_venue` rule: a leg's
    /// history comes from the venue its orders route to. A cross-venue mount reading `bars(leg)`
    /// during a bar of the SAME symbol on a different venue therefore gets the declared venue's
    /// series, matching what `position(leg)` already answers.
    fn push_view(
        &self,
        mount_idx: usize,
        venue: &str,
        sym: &str,
        interval: &str,
        skip_symbol: Option<&str>,
        out: &mut DeclaredViews,
    ) {
        // ⚠ `skip_symbol` is `Some` on the FILL lane ONLY, and that asymmetry is the point.
        //
        // On the fill lane the dispatching symbol MUST have no row: `LiveBroker::position` answers
        // it from `AppliedFill::position_after` — the state after THIS fill — which is the per-fill
        // snapshot guarantee `applied_fills` exists for. A table row would answer from
        // `position_size_of`, i.e. the account after the WHOLE batch folded, silently undoing it.
        //
        // On the bar / tick / reference lanes there is no such scalar to protect, and skipping
        // there would be a REGRESSION rather than a nicety: a declared leg read on its OWN bar used
        // to get a row, so `price(leg)` came from `resolved_position_price` — the `price_cfg`
        // resolver (mark > quote > trade > bar-close, with its staleness policy). Skipping drops it
        // to the raw `bar.close`, so a leg carrying a fresh venue mark would silently report the bar
        // close instead. That path was already wired and working; an earlier draft of this change
        // skipped unconditionally and altered it without saying so.
        if !out.bar_views.iter().any(|(s, _)| s == sym) {
            let key = (venue.to_string(), sym.to_string(), interval.to_string());
            let closed = self.bars.get(&key).map(|s| Arc::clone(&s.closed)).unwrap_or_default();
            out.bar_views.push((sym.to_string(), closed));
        }
        if skip_symbol == Some(sym) || out.positions.iter().any(|(s, _)| s == sym) {
            return;
        }
        // ⚠ The MOUNT's engine when this leg is on the mount's own venue, the venue's default
        // account otherwise (a cross-exchange declared leg). Same rule as the WRITE half
        // (`EngineRoute::Mount`), reached through the same function — a leg whose read came from the
        // default account while its order went to a labelled one is the read/write split
        // [`Self::leg_venue`]'s own doc records as having been measured once already.
        let eidx = self.route_of(EngineRoute::Mount(mount_idx), venue).unwrap_or(0);
        let pos = self.eng(eidx).position_size_of(sym, "BOTH");
        out.positions.push((sym.to_string(), pos));
        if let Some(px) =
            self.eng(eidx).resolved_position_price(venue, sym, pos, &self.config.price_cfg)
        {
            out.prices.push((sym.to_string(), px));
        }
    }

    /// Resolve ONE buffered intent's target symbol against the mount's declaration.
    ///
    /// - No mount declared extra symbols (`any_mount_multi == false`, every runtime today) ⇒ the
    ///   mount's own symbol, via a single bool read. Byte-identical.
    /// - This mount declared nothing ⇒ likewise the mount's own symbol, so a single-symbol mount
    ///   is unaffected even when a SIBLING mount is multi-symbol.
    /// - This mount DID declare, and the intent named its own symbol or a declared one ⇒ that
    ///   symbol. This is the whole point of the lane.
    /// - This mount declared, and the intent named something ELSE ⇒ `None`, the order is REFUSED.
    ///
    /// The refusal is deliberate. Silently rewriting an undeclared symbol to the mount's own is
    /// exactly the bug this program exists to remove (`multi_symbol_routing.rs`): it turns "trade
    /// X" into "trade Y" with no signal. Once a mount opts in, symbol correctness is ENFORCED for
    /// it; a mount that never opted in keeps the old permissive contract, which shipped strategies
    /// depend on (they pass `""`, and live bars carry no symbol at all).
    fn resolve_intent_symbol(
        &self,
        mount_idx: usize,
        requested: Option<&str>,
        mount_symbol: &str,
    ) -> Option<String> {
        if !self.any_mount_multi {
            return Some(mount_symbol.to_string());
        }
        let declared = &self.mount_symbols[mount_idx];
        if declared.is_empty() {
            return Some(mount_symbol.to_string());
        }
        match requested {
            None => Some(mount_symbol.to_string()),
            Some(r) if r == mount_symbol => Some(mount_symbol.to_string()),
            Some(r) if declared.iter().any(|l| l.symbol == r) => Some(r.to_string()),
            Some(r) => {
                tracing::warn!(
                    mount_idx,
                    requested = r,
                    mount_symbol,
                    "refusing an order for a symbol this mount did not declare"
                );
                None
            }
        }
    }

    /// The VENUE a resolved symbol trades on for this mount.
    ///
    /// A declared leg may name a DIFFERENT venue (`MountLeg::at`) — that is what makes a
    /// cross-exchange strategy possible: an xEMM maker rests on one venue and hedges on another,
    /// so its two legs cannot share a venue by construction. Anything not declared, or declared
    /// without a venue, uses the mount's own — so every single-venue mount is byte-identical and
    /// the whole path is one bool read when no mount declared anything.
    ///
    /// Downstream is ALREADY venue-aware: `apply_intent` routes to an engine by
    /// `engine_idx_for_route_key(&req.venue)` and every adapter forwards the request verbatim. The
    /// only reason a foreign venue could not be reached was that `drain_broker` stamped the
    /// mount's venue onto every intent — which is what this resolves.
    fn resolve_intent_venue(&self, mount_idx: usize, symbol: &str, mount_venue: &str) -> String {
        if !self.any_mount_multi {
            return mount_venue.to_string();
        }
        self.leg_venue(mount_idx, symbol, mount_venue)
    }

    /// **THE one rule for "which venue does this declared leg live on".**
    ///
    /// A `MountLeg::at(sym, venue)` names its venue; a `MountLeg::same_venue(sym)` does not, and
    /// falls back to `fallback`.
    ///
    /// ⚠ It exists because the READ side and the WRITE side each had their OWN copy of that
    /// fallback and the two copies DISAGREED. [`Self::declared_views`] fell back to the MOUNT's
    /// venue; [`Self::resolve_intent_venue`] falls back to whatever `drain_broker` passed it, which
    /// is the DRAIN's venue. On the bar and tick lanes those are the same string, so nothing showed
    /// — but on the order-event and fill lanes the drain venue is the EVENT's venue, which on a
    /// cross-venue mount is routinely foreign.
    ///
    /// The concrete failure that made this one function: a mount on binance/BTCUSDT declaring
    /// `MountLeg::at("ETHUSDT", "bybit")` and `MountLeg::same_venue("SOLUSDT")`. A hedge fill
    /// arrives on bybit/ETHUSDT; inside `on_fill` the strategy reads `position("SOLUSDT")` — served
    /// from BINANCE's engine — then submits SOLUSDT, which routes to BYBIT. Read and write land in
    /// different books, silently, with no error anywhere.
    ///
    /// Both callers pass the SAME `fallback`, so they cannot diverge again: there is one rule and
    /// one place it lives.
    ///
    /// ⚠ **But AGREEING is not the same as being RIGHT, and the first version of this rule agreed
    /// on the wrong answer.** It resolved a `venue: None` leg to `fallback` — the DRAIN's venue —
    /// on both sides. Run the scenario above through that: the hedge fill arrives on bybit, so the
    /// `same_venue("SOLUSDT")` leg resolves to BYBIT for the read *and* the write. They match, the
    /// stated rule holds, and both are wrong — `MountLeg::same_venue` is documented as "a leg on
    /// the mount's OWN venue", SOLUSDT's position is on binance, and the submitted order goes to an
    /// exchange the mount never named. Measured, not reasoned: `position(SOLUSDT)` read `0.0`
    /// against a seeded `11.0`, and the order landed on the hedge client.
    ///
    /// So a DECLARED leg with no venue of its own resolves to the MOUNT's venue, which is what the
    /// constructor means. `fallback` still answers for a symbol that is not a declared leg at all —
    /// the mount's own symbol, and an undeclared symbol the order path is about to refuse — where
    /// the dispatching series genuinely is the least surprising answer.
    ///
    /// Byte-identical on the bar and tick lanes, which is where every shipped mount lives today: a
    /// same-venue leg's bar arrives on the mount's own venue, so `fallback` already WAS that venue
    /// and the two spellings pick the same string. Only a dispatch whose venue is foreign — a
    /// cross-venue fill or order event — changes, and only to the venue the leg was declared on.
    ///
    /// `a_same_venue_leg_reads_and_writes_the_same_book` is the gate, and it asserts BOTH halves
    /// land on the mount's venue rather than merely landing together — agreement alone is what the
    /// previous rule already had.
    fn leg_venue(&self, mount_idx: usize, symbol: &str, fallback: &str) -> String {
        let Some(leg) = self.mount_symbols[mount_idx].iter().find(|l| l.symbol == symbol) else {
            // Not a declared leg: the dispatching series is the right default.
            return fallback.to_string();
        };
        if let Some(v) = &leg.venue {
            return v.clone();
        }
        // A declared `same_venue` leg: the MOUNT's own venue, by definition of the constructor.
        // A transiently-taken slot leaves that unknown, and guessing is what this method exists to
        // stop, so it declines to the caller's default — `declared_views` already returns empty in
        // that state, and no call site can reach it (each restores the mount before draining).
        self.mounts[mount_idx]
            .as_ref()
            .map(|m| m.venue.clone())
            .unwrap_or_else(|| fallback.to_string())
    }

    /// Make ONE refused intent OBSERVABLE — the denial [`Self::resolve_intent_symbol`]'s `None`
    /// stands for, emitted through the EXISTING `RiskGate`-veto mechanism rather than a second one
    /// of its own.
    ///
    /// The refusal itself is older than this method and unchanged: an opted-in mount that names a
    /// symbol it never declared gets no order. What was missing is that `continue` told NOBODY. The
    /// strategy received no callback, no event reached the bus, and nothing appeared in the
    /// operator's recent-events ring — a strategy asked for something and the platform quietly did
    /// nothing, which is the same silent-drop class the two-leg routing bug was.
    ///
    /// So this mirrors `ExecutionEngine::gate_and_register`'s veto path VERBATIM, both halves:
    ///
    /// 1. an [`vike_exec::execution_engine::OrderEventOut`] carrying
    ///    `OrderEventKind::Denied { reason }`, pushed onto the routing engine's buffer under the
    ///    same `collect_applied_fills` gate the gate's own veto capture uses (a strategy is
    ///    mounted ⇔ someone can hear it), delivered by [`Self::dispatch_order_events`];
    /// 2. an [`vike_model::events::Event::OrderDenied`] published to that engine, which the bus's
    ///    delivery log turns into a recent-events line at `drain_delivered` — the same way an
    ///    operator sees a `RiskGate` veto. Like a vetoed order, the refusal was never registered,
    ///    so the fold counts it into `dropped_unknown_coid` exactly as a denied order is.
    ///
    /// The id comes from [`Self::mint_refusal_id`], NOT the coid generator — see that method.
    /// `coid_mount` records it against the refusing mount purely so `dispatch_order_events` routes
    /// the denial back to the mount that asked, precisely, rather than falling back to the first
    /// mount on a `(venue, symbol)` pair (the requested symbol matches no mount by construction —
    /// it is the undeclared one). No fill can ever carry a refusal id: nothing was submitted.
    ///
    /// Cold path — a refusal happens at ORDER cadence at most, inside `drain_broker`, which is
    /// never on the per-message fold the `p99 < 10µs` gate measures.
    fn deny_undeclared_symbol(
        &mut self,
        mount_idx: usize,
        venue: &str,
        requested: Option<&str>,
        mount_symbol: &str,
        now: i64,
    ) {
        let requested = requested.unwrap_or_default().to_string();
        let refusal_id = self.mint_refusal_id();
        let declared = self.mount_symbols[mount_idx]
            .iter()
            .map(|l| l.symbol.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let reason = format!(
            "undeclared symbol `{requested}`: mount {mount_idx} on {venue}/{mount_symbol} declared \
             [{declared}]"
        );
        // No `tracing` call here on purpose: `resolve_intent_symbol` already warns on the refusal it
        // decided, and a second warn per refusal would only duplicate it. The channels this method
        // adds are the ones a WARN cannot reach — the strategy's own callback and the operator ring
        // — and the ring line carries the richer text (requested + mount + full declared set).
        // The REFUSING mount's own engine: this denial is that mount's order-event, so it must land
        // in the book that mount reads. The same `EngineRoute::Mount` rule the intent itself would
        // have taken had it not been refused.
        let eidx = self.route_of(EngineRoute::Mount(mount_idx), venue).unwrap_or(0);
        self.coid_mount.insert(refusal_id.clone(), mount_idx);
        {
            let eng = self.eng_mut(eidx);
            if eng.collect_applied_fills {
                eng.order_events.push(vike_exec::execution_engine::OrderEventOut {
                    venue: venue.to_string(),
                    symbol: requested,
                    event: vike_model::strategy::OrderLifecycle {
                        client_order_id: refusal_id.clone(),
                        // Stamped at DELIVERY (`dispatch_order_events`), never here — one spelling
                        // of the rule. This synthetic refusal id never reaches the tag registry
                        // (the refused intent minted no order), so it stays `None` in practice.
                        tag: None,
                        kind: vike_model::strategy::OrderEventKind::Denied {
                            reason: reason.clone(),
                        },
                    },
                });
            }
        }
        self.publish_to(
            eidx,
            Event::OrderDenied(vike_model::events::OrderDenied {
                client_order_id: refusal_id,
                reason: reason.into(),
                ts: now,
            }),
        );
        self.dirty = true;
    }

    /// Mint → RiskGate → client for each buffered submission, then resolve buffered modifications
    /// (tag → coid) and route them. Shared by the bar path ([`drive_strategy`]) and the tick path
    /// ([`drive_strategy_tick`]) so all order construction flows through the ONE hot-path literal
    /// (piece 1) and the ONE live path. Submits are processed before modifications so a submit-then-modify
    /// within the SAME handler call resolves against the tag registered this call.
    ///
    /// `venue`/`symbol` are the DRAIN SERIES — whichever series this dispatch was about. They are
    /// the default target of a symbol-less intent ([`Self::resolve_intent_symbol`]); they are NOT
    /// the tag-registry key, which is the MOUNT's own series ([`Self::tag_key`]).
    ///
    /// `mount_idx` is the buffering mount's index into `self.mounts` / `self.mount_states` — every
    /// call site already has it on hand (it is how the mount was found/taken in the first place).
    /// Portfolio-observer PR-4 T5: while that mount is [`MountState::Pending`] (only reachable when
    /// [`CoreConfig::readiness_gate`] is on — every mount is `Ready` immediately otherwise), the
    /// ENTIRE buffered `ctx` — submissions, brackets, modifications, cancels, conditionals,
    /// mass_cancel — is DISCARDED right here, before any of it reaches `apply_intent`. The strategy
    /// hook that filled `ctx` already ran at the call site (warmup/observation is unaffected); only
    /// the order OUTPUT of this one dispatch is dropped. This is a single choke point rather than a
    /// check duplicated at each of the six call sites, and it leaves NO residue by construction: `ctx`
    /// is a fresh, owned `LiveBroker` built new by every call site (never a persisted field), so an
    /// early return here simply drops its `Vec`s — there is no outbox to "clear" and nothing can
    /// accumulate across ticks regardless of how many consecutive dispatches stay Pending.
    fn drain_broker(
        &mut self,
        ctx: LiveBroker,
        venue: &str,
        symbol: &str,
        now: i64,
        mount_idx: usize,
    ) {
        // Discard this mount's buffered intents when it is either not yet priced (readiness gate,
        // portfolio-observer PR-4 T5) OR budget-latched liquidate-only (steal/core-per-mount-budget)
        // — in both cases the strategy hook already ran at the call site; only its ORDER OUTPUT is
        // dropped. `mount_latched` is all-false when no budget is active, so this stays byte-identical
        // to the readiness-only check for a budget-free runtime.
        if self.mount_states[mount_idx] == MountState::Pending || self.mount_latched[mount_idx] {
            return; // ctx (and every buffered intent in it) is dropped here — nothing reaches the engine
        }
        for s in ctx.submissions {
            let Some(sym) = self.resolve_intent_symbol(mount_idx, s.symbol.as_deref(), symbol)
            else {
                // refused: a declared-multi mount named an UNDECLARED symbol. The refusal is
                // unchanged (no order) — it is now VISIBLE: the strategy hears a Denied lifecycle
                // event and the operator sees an `OrderDenied` line. See `deny_undeclared_symbol`.
                self.deny_undeclared_symbol(mount_idx, venue, s.symbol.as_deref(), symbol, now);
                continue;
            };
            let ven = self.resolve_intent_venue(mount_idx, &sym, venue);
            let req = OrderRequest {
                client_order_id: String::new(),
                venue: ven.clone(),
                symbol: sym.clone(),
                side: s.side,
                qty: s.qty,
                order_type: s.order_type,
                price: s.price,
                reduce_only: s.reduce_only,
                ts: now,
                ..Default::default()
            };
            let coids =
                self.apply_strategy_intent(OrderIntent::Submit(Box::new(req)), now, mount_idx);
            if let (Some(tag), Some(coid)) = (s.tag, coids.first()) {
                // Keyed by the BUFFERING mount's index first — see `CoreThread::strategy_tags` for
                // why a mount-blind key let one mount's `cancel_tagged("bid")` hit another mount's
                // resting quote — and on the MOUNT's own series, never this drain's: see
                // `Self::tag_key`. A tagged submission carries no symbol (`HftBroker` contract, and
                // `broker.rs`'s producer-side `tagged_orders_never_carry_a_symbol` pins the closed
                // producer set), so it always resolves to the drain's `symbol`; the ORDER is
                // therefore on the drain's instrument while the KEY is the mount's, which is
                // exactly what makes one tag name one quote across every lane.
                let key = self.tag_key(mount_idx, venue, symbol, tag.as_str());
                self.strategy_tags.insert(key, coid.clone());
            }
        }
        for b in ctx.brackets {
            let Some(sym) = self.resolve_intent_symbol(mount_idx, b.symbol.as_deref(), symbol)
            else {
                self.deny_undeclared_symbol(mount_idx, venue, b.symbol.as_deref(), symbol, now);
                continue;
            };
            let ven = self.resolve_intent_venue(mount_idx, &sym, venue);
            let spec = BracketSpec {
                venue: ven,
                symbol: sym,
                side: b.side,
                qty: b.qty,
                entry_price: b.entry_price,
                stop_loss: b.stop_loss,
                take_profit: b.take_profit,
            };
            self.apply_strategy_intent(OrderIntent::Bracket(Box::new(spec)), now, mount_idx);
        }
        for a in ctx.modifications {
            let key = self.tag_key(mount_idx, venue, symbol, &a.tag);
            if let Some(coid) = self.strategy_tags.get(&key).cloned() {
                self.apply_strategy_intent(
                    OrderIntent::Modify {
                        client_order_id: coid,
                        new_qty: a.new_qty,
                        new_price: a.new_price,
                    },
                    now,
                    mount_idx,
                );
            }
        }
        for tag in ctx.cancels {
            let key = self.tag_key(mount_idx, venue, symbol, &tag);
            if let Some(coid) = self.strategy_tags.get(&key).cloned() {
                // ROUTINE, and this is the ONE site in the runtime where that is genuinely known:
                // `HftBroker::cancel_tagged` pulls ONE of a strategy's own tagged quotes, which is
                // ladder churn by construction — a maker suppressing a side, or the cancel half of
                // a requote it is about to re-place. A venue metering its cancels may hold this
                // back under pressure (it comes back as a NON-terminal cancel-reject and the
                // strategy re-offers on its next tick), which is exactly the churn a flatten
                // reserve is protected FROM.
                //
                // ⚠ `Broker::mass_cancel()` is NOT classified with it, further down: "pull ALL my
                // quotes" is the adverse-selection hammer, it is not retried on a shed, and it
                // stays at the flatten-safe default.
                self.apply_strategy_intent_with_cancel_intent(
                    OrderIntent::Cancel(coid),
                    now,
                    mount_idx,
                    CancelIntent::Routine,
                );
            }
        }
        for c in ctx.conditionals {
            let Some(cond_sym) = self.resolve_intent_symbol(mount_idx, c.symbol.as_deref(), symbol)
            else {
                self.deny_undeclared_symbol(mount_idx, venue, c.symbol.as_deref(), symbol, now);
                continue;
            };
            let cond_ven = self.resolve_intent_venue(mount_idx, &cond_sym, venue);
            self.apply_strategy_intent(
                OrderIntent::ArmConditional(ConditionalIntent {
                    venue: cond_ven,
                    symbol: cond_sym,
                    side: c.side,
                    qty: c.qty,
                    price: c.price,
                    trail: c.trail,
                    // strategies arm on the default (Last) lane today — the portable Broker
                    // surface exposes no trigger-source verb; None keeps this path byte-identical
                    trigger_by: None,
                }),
                now,
                mount_idx,
            );
        }
        if ctx.mass_cancel {
            // ⚠ The MOUNT's series, NEVER the drain's — the same rule `tag_key` states at length,
            // and the last verb that was still keyed the other way.
            //
            // `Broker::mass_cancel` is documented as "cancel ALL of THIS ENGINE's live orders", and
            // `apply.rs`'s `(Some(v), sym)` arm makes the VENUE load-bearing: it resolves that
            // venue's engine and calls `ExecutionEngine::mass_cancel` on the whole thing, using the
            // symbol only to scope held exits and conditional books. So the venue stamped here
            // decides which book gets pulled.
            //
            // `drain_broker`'s `venue`/`symbol` are the DRAIN SERIES, which its own doc says are not
            // the mount's: the fill and order-event lanes dispatch the FILL's `(venue, symbol)`. A
            // maker that calls `mass_cancel()` from inside `on_fill` — the canonical use, pulling
            // quotes on adverse selection — therefore pulled quotes on whichever venue the fill
            // arrived from. On a cross-venue mount that is routinely the HEDGE venue: its own quotes
            // stayed live at the maker venue while an unrelated book was cleared.
            //
            // Byte-identical for every single-symbol mount, whose drain series IS its own on every
            // lane (`tag_key`'s doc enumerates why). The `map_or` fallback covers a transiently-taken
            // mount slot, which no call site can reach.
            let (mc_venue, mc_symbol) = self.mounts[mount_idx]
                .as_ref()
                .map_or((venue, symbol), |m| (m.venue.as_str(), m.symbol.as_str()));
            self.apply_strategy_intent(
                OrderIntent::MassCancel {
                    venue: Some(mc_venue.to_string()),
                    symbol: Some(mc_symbol.to_string()),
                },
                now,
                mount_idx,
            );
        }
        self.dirty = true;
    }

    /// ONE tag's key into `strategy_tags` — `{mount_idx}|{mount_venue}|{mount_symbol}|{tag}`.
    ///
    /// ## ⚠ The MOUNT's series, NEVER the drain's — the insert and BOTH lookups, together
    ///
    /// [`Self::drain_broker`] runs on whichever series dispatched, and that is NOT always the
    /// mount's own: the bar and tick lanes dispatch a DECLARED LEG's series, and the fill and
    /// order-event lanes dispatch the FILL's / the ORDER's `(venue, symbol)`. The key used to be
    /// built from those drain ARGUMENTS, so a tagged quote placed from one lane was filed under a
    /// key a `cancel_tagged` from another lane never looks up. The failure mode is the one this
    /// program's own plan called worse than the netting bug: the lookup finds nothing, `drain_broker`
    /// simply does not act, and a REAL quote is left resting at the venue with no error, no event
    /// and no ring line. `vike_mm::xemm`'s `strategy_impl` module doc carries the per-lane table
    /// this was found through; its hedge-fill arm still emits no tagged SUBMIT, but for the
    /// EMISSION reason that survives here (a symbol-less submit still resolves to the drain's
    /// series) rather than for the registry reason, which this method removes.
    ///
    /// Keying on the mount removes the second dimension the strategy could not see anyway:
    /// `HftBroker::submit_limit_tagged`/`modify_tagged`/`cancel_tagged` name NO symbol, so from the
    /// strategy's side a tag has always been a MOUNT-scoped name. The drain-keyed registry gave it a
    /// hidden per-series dimension that no verb could address.
    ///
    /// The insert and both lookups go through this ONE method deliberately. Moving only the insert —
    /// the obvious half-fix — reproduces the silent no-op above exactly, which is why
    /// `crates/vike-core/tests/wiring/multi_symbol_reads.rs` drives the tag in BOTH directions (place on
    /// the mount's series and cancel from the leg's, and the reverse): each direction fails if only
    /// one side moved.
    ///
    /// ⚠ **One tag names ONE order per mount.** Re-using a tag before cancelling overwrites the
    /// entry and orphans the previous order (it keeps resting, and no tag can address it any more).
    /// That was already true for a repeat on the SAME series; keying on the mount widens it to a
    /// repeat across two LEGS. A mount quoting both legs must use per-leg tags — which is also the
    /// only shape the symbol-less `HftBroker` verbs can express.
    ///
    /// Byte-identical for every single-symbol mount: on every lane its drain series IS its own
    /// (`drive_strategy`/`drive_strategy_tick` match `m.symbol == symbol`; `drive_strategy_feed_status`/
    /// `_flow`/`_params` require the pair; `_mark`/`_reference_quote`/`drive_schedule` drain the
    /// mount's own by construction; and an undeclared mount's orders — hence its fills and order
    /// events — can only ever carry its own symbol, since `resolve_intent_symbol` returns
    /// `mount_symbol` for it unconditionally). Cold path: called only when a tagged verb was actually
    /// buffered, so the per-market-message dispatch that buffers nothing pays nothing and the
    /// `p99 < 10µs` fold is untouched. `drain_venue`/`drain_symbol` are the fallback for a
    /// transiently-taken mount slot, which no call site can reach (each restores the mount before
    /// draining).
    fn tag_key(
        &self,
        mount_idx: usize,
        drain_venue: &str,
        drain_symbol: &str,
        tag: &str,
    ) -> String {
        let (venue, symbol) = self.mounts[mount_idx]
            .as_ref()
            .map_or((drain_venue, drain_symbol), |m| (m.venue.as_str(), m.symbol.as_str()));
        format!("{mount_idx}|{venue}|{symbol}|{tag}")
    }

    /// The STRATEGY's own tag for `coid` under `mount_idx`, or `None` if that mount has no live tag
    /// entry naming this order — the reverse of [`Self::tag_key`], and the stamp that gives
    /// [`vike_model::strategy::OrderLifecycle::tag`] its value.
    ///
    /// ## Why the lookup is VALUE-matched, and why that is the whole safety argument
    ///
    /// `strategy_tags` maps `{mount_idx}|{venue}|{symbol}|{tag}` → coid, and a re-submit under the
    /// same tag OVERWRITES the entry (`drain_broker`'s insert). So the question "which tag names
    /// this order" is only answerable by matching the coid on the VALUE side. Doing it that way is
    /// not merely convenient — it is what makes a LATE terminal harmless: a maker that submits under
    /// `"bid"` again before the previous order's cancel-ack arrives has an entry pointing at the NEW
    /// coid, the old coid matches nothing, this returns `None`, and the strategy is told about an
    /// order it no longer tracks under a tag that now belongs to a LIVE quote. A key-side lookup
    /// (build the key, compare nothing) would instead hand the strategy its own live tag and make it
    /// cancel a quote it had just placed.
    ///
    /// The mount's own `(venue, symbol)` builds the prefix — the same rule [`Self::tag_key`] states
    /// at length — and the tag is the REMAINDER after it, taken with `strip_prefix` rather than by
    /// splitting on `|`, so a tag containing a `|` round-trips.
    ///
    /// Cost: a scan of `strategy_tags`, which holds one entry per LIVE tag per mount (two for a
    /// single-quote maker, `2 × levels` for a laddered one). Called only from the two OCCASIONAL
    /// order-outcome lanes at ORDER cadence — never from the per-market-message fold — so the
    /// `p99 < 10µs` hop is untouched.
    fn tag_for_coid(&self, mount_idx: usize, coid: &str) -> Option<String> {
        let m = self.mounts[mount_idx].as_ref()?;
        let prefix = format!("{mount_idx}|{}|{}|", m.venue, m.symbol);
        self.strategy_tags
            .iter()
            .find(|(k, v)| v.as_str() == coid && k.starts_with(&prefix))
            .and_then(|(k, _)| k.strip_prefix(&prefix).map(str::to_string))
    }

    /// Drop `mount_idx`'s tag entry for `coid` — called ONLY once that order is terminal, so the
    /// registry holds live orders and nothing else.
    ///
    /// Unlike [`Self::note_terminal_coid`], this erases IMMEDIATELY and needs no linger window: the
    /// linger exists so a late fill can still be ATTRIBUTED to a mount, and attribution reads
    /// `coid_mount`, never this map. What this map answers is "which order does the tag `"bid"` name
    /// right now", and after a terminal the honest answer is "none" — keeping the dead entry is what
    /// let `modify_tagged("bid")` resolve a terminal coid and be swallowed by
    /// `ExecutionEngine::modify_order`'s not-modifiable early return, with no error, no event and no
    /// ring line. The removal is value-guarded through [`Self::tag_for_coid`], so a late terminal
    /// can never retire an entry that has since been re-pointed at a live order.
    fn retire_tag_for_coid(&mut self, mount_idx: usize, coid: &str) -> Option<String> {
        let tag = self.tag_for_coid(mount_idx, coid)?;
        let key = self.tag_key(mount_idx, "", "", &tag);
        self.strategy_tags.shift_remove(&key);
        Some(tag)
    }

    /// Journal a mounted strategy's ALREADY-RESOLVED order intent write-ahead of `apply_intent`
    /// (portfolio-observer PR-5 T2) — the `drain_broker` boundary's twin of `dispatch()`'s `Cmd`
    /// gate (~:954-958): same idiom, journal BEFORE the fold that mints coids. `self.journal.is_some()`
    /// gates the whole hook so journaling off stays byte-identical to today (every existing
    /// drain_broker/readiness/journal_wiring test is unaffected). `intent` is journaled AFTER the
    /// tag→coid resolution `drain_broker`'s modify/cancel call sites already did, so replay never
    /// needs the `strategy_tags` registry — it re-applies the same resolved `OrderIntent` a
    /// `Command::Order` `Cmd` would carry. `journaled_since_snap` is bumped here too — the SAME
    /// field `dispatch()`'s tail cadence check (~:1165) reads — so a strategy-heavy session still
    /// feeds the `snapshot_every` cadence; the actual `write_snap` call fires at the next journaled
    /// (Event/Command/Watchdog) dispatch, since `drain_broker` itself runs from within a
    /// BarClose/Quote/Trade/Book fold that is not in that `journaled` match arm — the counter is
    /// not lost, just possibly observed one dispatch later. Cold path (`drain_broker` is off the
    /// per-message hot fold, so this never touches the `p99 < 10µs` gate): the mount_id is cloned
    /// rather than borrowed to sidestep holding `self.journal.as_mut()` (mutable) and
    /// `self.mount_ids[mount_idx]` (immutable) at once.
    /// **The mount that OWNS an unattributed `(venue, symbol)`** — the fallback both delivery lanes
    /// take when no mount minted the coid.
    ///
    /// ⚠ It must consider a mount's DECLARED LEGS, not only its own series, and both lanes used to
    /// consider only the latter (`m.venue == f.venue && m.symbol == f.symbol`). The comment above
    /// each fallback names exactly the cases that reach it — "an operator ticket, a liquidation, an
    /// adopted venue order" — and every one of them is a real possibility on a hedge venue. On a
    /// cross-venue mount none of them matched anything, so the `else { continue; }` dropped them:
    /// a LIQUIDATION on the hedge leg reached no strategy at all, in either lane, silently.
    ///
    /// ⚠ ONE function, called by both lanes, deliberately. The identical predicate was written out
    /// twice — once in `dispatch_applied_fills`, once in `dispatch_order_events` — and was wrong
    /// identically in both. That is the same shape as the three bugs fixed before it (the capture
    /// gate, `mass_cancel`'s venue, `leg_venue`'s fallback): one law, spelled more than once. It is
    /// spelled once now.
    ///
    /// A leg's venue comes from [`Self::leg_venue`], so a `MountLeg::same_venue` leg resolves to the
    /// MOUNT's venue — the same rule the read and write sides use, rather than a fourth spelling.
    /// First match wins, matching the historical behaviour for the mount's-own-series case.
    fn mount_owning(&self, venue: &str, symbol: &str) -> Option<usize> {
        (0..self.mounts.len()).find(|&i| {
            let Some(m) = self.mounts[i].as_ref() else {
                return false;
            };
            if m.venue == venue && m.symbol == symbol {
                return true;
            }
            self.mount_symbols[i]
                .iter()
                .any(|leg| leg.symbol == symbol && self.leg_venue(i, symbol, &m.venue) == venue)
        })
    }

    fn apply_strategy_intent(
        &mut self,
        intent: OrderIntent,
        now: i64,
        mount_idx: usize,
    ) -> Vec<String> {
        self.apply_strategy_intent_with_cancel_intent(
            intent,
            now,
            mount_idx,
            CancelIntent::Unspecified,
        )
    }

    /// [`Self::apply_strategy_intent`], classifying every cancel it lowers (see [`CancelIntent`]).
    /// The classification reaches the venue and is journaled NOWHERE — see
    /// [`Self::apply_intent_with_cancel_intent`] for why it rides beside the [`OrderIntent`] rather
    /// than inside it, and why that keeps the write-ahead record's shape unchanged.
    fn apply_strategy_intent_with_cancel_intent(
        &mut self,
        intent: OrderIntent,
        now: i64,
        mount_idx: usize,
        cancel_intent: CancelIntent,
    ) -> Vec<String> {
        if let Some(journal) = self.journal.as_mut() {
            let mid = self.mount_ids[mount_idx].clone();
            journal.append_strategy_submit(now, &mid, &intent).expect("journal append");
            self.journaled_since_snap += 1;
        }
        // ⚠ ROUTED BY THE MOUNT, not by the intent's venue string. This is the one choke point every
        // strategy-minted intent lowers through, so it is the one place the mount's declared
        // ACCOUNT can be turned into an engine — and turning it into one HERE is what makes a
        // misroute unwritable rather than merely untested: there is no venue string anywhere
        // downstream that could name the wrong account, because the account never becomes a string.
        // `EngineRoute::Mount` still defers to the payload for a leg declared on a FOREIGN venue;
        // see that enum.
        let coids =
            self.apply_intent_routed(intent, now, cancel_intent, EngineRoute::Mount(mount_idx));
        // steal/core-per-mount-budget ATTRIBUTION: tag every coid this mount's intent minted back to
        // the mount, so its fills fold into the mount's ledger and its resting orders can be canceled
        // scoped-to-this-mount on a budget breach. `apply_intent` returns the coids of orders it
        // SUBMITTED (empty for modify/cancel/arm), so nothing is over-tagged. Cold path (order
        // cadence, off the p99 fold); the map is written even with no budget (attribution is
        // always-on + read-only — it changes no fold decision, so behavior stays byte-identical).
        for c in &coids {
            self.coid_mount.insert(c.clone(), mount_idx);
        }
        coids
    }

    /// The mount that MINTED `coid` — the EXACT strategy-callback routing key for a fill or an
    /// order event (multi-mount correctness, bug B).
    ///
    /// `coid_mount` is written at [`Self::apply_strategy_intent`], i.e. the one choke point where a
    /// mount's buffered intent lowers into `apply_intent`, so it already records precisely which
    /// mount owns each server-minted coid. Routing by it replaces the previous FIRST-MATCH scan over
    /// `(venue, symbol)`, which was not a routing key at all once two mounts shared a symbol: only
    /// the first ever received `on_fill`/`on_order_event`, and the second silently never heard about
    /// its own orders. (The bar lane has always matched the full `(venue, symbol, interval)` triple;
    /// the fill/order-event lanes did not.)
    ///
    /// `None` means "no mount minted this coid" — a manual operator ticket, a margin-call or
    /// budget-latch liquidation, a reconcile-adopted venue order — or that the owning slot is
    /// transiently taken (mid another hook dispatch). Callers then fall back to the historical
    /// (venue, symbol) match, so an unattributed fill still reaches a strategy exactly as before.
    /// A pure map lookup at order/fill cadence; the per-message fold never calls it.
    fn mount_for_coid(&self, coid: &str) -> Option<usize> {
        let idx = *self.coid_mount.get(coid)?;
        self.mounts.get(idx).and_then(|slot| slot.as_ref()).map(|_| idx)
    }

    /// Check the ConditionalBook against a closed bar and submit fired orders as plain
    /// MARKETs through the ONE live path (mint → RiskGate → client) — oracle semantics:
    /// only the FIRE crosses the gate; a veto surfaces as OrderDenied.
    pub(crate) fn fire_conditionals_bar(&mut self, venue: &str, symbol: &str, bar: &Bar) {
        let fired = match self.conditional_books.get_mut(&(venue.to_string(), symbol.to_string())) {
            Some(book) if !book.is_empty() => book.check_bar(bar),
            _ => return,
        };
        self.submit_fired(venue, symbol, fired, bar.ts);
    }

    /// Tick-price twin (Rust-native upgrade, `CoreConfig::conditionals_on_ticks`): the
    /// same book checked against a degenerate bar at the tick price — the Last lane.
    fn fire_conditionals_at_price(&mut self, venue: &str, symbol: &str, px: f64, now: i64) {
        let fired = match self.conditional_books.get_mut(&(venue.to_string(), symbol.to_string())) {
            Some(book) if !book.is_empty() => book.check_price(px, now),
            _ => return,
        };
        self.submit_fired(venue, symbol, fired, now);
    }

    /// MARK-lane twin (w2 `trigger_by`): a mark tick evaluates ONLY the `Some(Mark)` arms of the
    /// book (their trailing extremes ratchet on this lane too). Called off the conflated mark
    /// drain for EVERY mark tick — deliberately NOT behind `conditionals_on_ticks` (that knob
    /// gates the Last lane's sub-bar upgrade over the oracle's bar-close law; the mark lane has
    /// no bar-close equivalent at all, so gating it would mean a Mark arm could never fire).
    ///
    /// Because it is ungated it sits on the mark drain, so the no-Mark-arm path must cost
    /// nothing: the `has_mark_arms` scan runs FIRST and the (venue, symbol) lookup key — two
    /// String allocations — is built ONLY once some book actually holds a Mark arm. The scan is
    /// O(books) over the (venue, symbol) pairs carrying conditionals (zero when none are armed,
    /// a handful otherwise), never a walk of the arms themselves.
    pub(crate) fn fire_conditionals_at_mark(
        &mut self,
        venue: &str,
        symbol: &str,
        px: f64,
        now: i64,
    ) {
        if !self.conditional_books.values().any(|b| b.has_mark_arms()) {
            return;
        }
        let fired = match self.conditional_books.get_mut(&(venue.to_string(), symbol.to_string())) {
            Some(book) if book.has_mark_arms() => book.check_mark(px, now),
            _ => return,
        };
        self.submit_fired(venue, symbol, fired, now);
    }

    /// Release each fired conditional as a plain MARKET through the ONE live path, WRITE-AHEAD
    /// journaled as a [`crate::journal::JournalRecord::ConditionalFire`] (emulator-journal PR-1).
    ///
    /// The write-ahead record is what makes an emulated trigger replayable at all. The FIRE is a
    /// runtime reaction to a bar/tick, and market data is deliberately never journaled — so before
    /// this record `replay.rs` re-folded a fired session WITHOUT its release and the determinism
    /// fence caught it as a `HashMismatch` (the residual its module doc named). Journaling the
    /// DECISION (not the tick behind it) is the same trick `apply_strategy_intent` plays for a
    /// mounted strategy's orders: replay re-applies the recorded request through `apply_intent`
    /// and never re-evaluates a trigger, so a fire can neither double nor vanish.
    ///
    /// The record goes down BEFORE `apply_intent` (write-ahead: a crash between the two loses the
    /// release, never hides it) and carries the request with its coid still EMPTY — the mint
    /// happens inside `apply_intent`, which then writes its own `MintedSubmit`, exactly as for any
    /// other server-minted submit. Gated on journaling being on, so the no-journal path is
    /// byte-identical; firing is order cadence, never the p99 per-message fold.
    fn submit_fired(
        &mut self,
        venue: &str,
        symbol: &str,
        fired: Vec<crate::emulator::FiredConditional>,
        now: i64,
    ) {
        for f in fired {
            let req = OrderRequest {
                client_order_id: String::new(),
                venue: venue.to_string(),
                symbol: symbol.to_string(),
                side: f.side,
                qty: f.qty,
                order_type: "market".to_string(),
                ts: now,
                ..Default::default()
            };
            if let Some(j) = self.journal.as_mut() {
                j.append_conditional_fire(now, &f.arm_id, f.trigger_px, &req)
                    .expect("journal append");
                self.journaled_since_snap += 1;
            }
            // ⚠ THE ARM'S OWN ACCOUNT, not the venue's default one. `conditional_books` is keyed by
            // `(venue, symbol)` — an EXCHANGE fact — so two accounts of one venue arm into one book
            // and a `FiredConditional` names no account; lowering this release through
            // `apply_intent` routed it by the payload's venue, which resolves the venue's DEFAULT
            // engine. A labelled mount's `Broker::submit_stop` therefore armed against its own book
            // and, on trigger, sold into the default account's: the protective exit OPENED a naked
            // position on an account that never asked for one while the position it was armed to
            // close stayed open — silently, both books wrong, and it is the exact shape the spread
            // configuration makes routine (long on one account, short on the other).
            //
            // `cond_engine` recorded the index at ARM time; a MISS keeps the historical payload
            // route, which is what a book seeded from a restored `Snap` still takes
            // (`CoreConfig::conditionals` carries `(venue, symbol)` and no route key — the declared
            // residual on that field).
            let route = match self.cond_engine.remove(&f.arm_id) {
                Some(eidx) => EngineRoute::Engine(eidx),
                None => EngineRoute::Payload,
            };
            self.apply_intent_routed(
                OrderIntent::Submit(Box::new(req)),
                now,
                CancelIntent::Unspecified,
                route,
            );
            self.dirty = true;
        }
    }

    /// Deliver buffered NON-FILL order-lifecycle transitions (accept / reject / deny / cancel /
    /// expire) to the mounted strategy that OWNS each order — the occasional-order-outcome twin of
    /// [`Self::dispatch_applied_fills`], and the delivery half of `Strategy::on_order_event`
    /// (position-executor stage 3). Each `vike_exec::OrderEventOut` was captured at the engine
    /// fold WITH its (venue, symbol) — from the just-advanced registry order, or the request for a
    /// `RiskGate` deny (a denied order never reaches the registry) — so this routes it to the mount
    /// that MINTED the order ([`Self::mount_for_coid`]), exactly as a fill is routed, builds the same
    /// `LiveBroker` snapshot, calls the hook, and drains any orders it buffers through the one live
    /// path. An order NO mount minted falls back to the first mount on its (venue, symbol).
    ///
    /// Fills are NOT here (they stay on [`Self::dispatch_applied_fills`], which is why
    /// `OrderLifecycle::from_event` returns `None` for them — no double-delivery). An order event
    /// folds at ORDER cadence (a submit reply / a cancel), never per quote or bar, so this touches
    /// the `p99 < 10µs` per-market-message fold NOT AT ALL. `now` is the dispatch wall-clock stamp
    /// (as in `on_feed_status`/`on_params_updated`), `price` the standing mark (else `0.0`). NOT
    /// warmup-gated (an order outcome is an account fact regardless of bar count, mirroring
    /// `on_fill`). No mount on the pair ⇒ the event is dropped (an order no live strategy owns). An
    /// empty buffer is a single length check + return — zero cost when nothing is pending or no
    /// strategy is mounted.
    fn dispatch_order_events(&mut self) {
        if self.engine.order_events.is_empty()
            && self.extra_engines.iter().all(|(_, e)| e.order_events.is_empty())
        {
            return;
        }
        if self.mounts.is_empty() {
            self.engine.order_events.clear();
            for (_, e) in self.extra_engines.iter_mut() {
                e.order_events.clear();
            }
            return;
        }
        let mut evs = std::mem::take(&mut self.engine.order_events);
        for (_, e) in self.extra_engines.iter_mut() {
            evs.append(&mut e.order_events);
        }
        let now = self.engine.now_ms; // stamped at the top of dispatch(), like feed_status/params
        for mut oe in evs {
            // A TERMINAL outcome makes this order's attribution entry retirable. Queued HERE, at the
            // top, BEFORE the routing `continue` below, so an event whose mount cannot be resolved
            // still bounds the map. Nothing is erased yet — see `note_terminal_coid`. The
            // classification is `OrderEventKind::is_terminal`, spelled once in vike-model, so a new
            // variant cannot default to "not terminal" here while a strategy treats it as a death.
            if oe.event.kind.is_terminal() {
                self.note_terminal_coid(&oe.event.client_order_id, now);
            }
            // Route to the mount that MINTED this order (bug B) — exactly the key
            // `dispatch_applied_fills` routes a fill by. An order NO mount minted (operator ticket,
            // liquidation, adopted venue order) falls back to the historical FIRST mount on
            // (venue, symbol); no mount at all = drop.
            let Some(idx) = self
                .mount_for_coid(&oe.event.client_order_id)
                .or_else(|| self.mount_owning(&oe.venue, &oe.symbol))
            else {
                continue;
            };
            let mount_key = {
                let m = self.mounts[idx].as_ref().expect("just found");
                (m.venue.clone(), m.symbol.clone(), m.interval.clone())
            };
            // Stamp the strategy's OWN name for this order — the tagged-order contract's missing
            // learning half (`OrderLifecycle::tag`). A TERMINAL transition retires the entry in the
            // same step, because after it the tag names nothing; a non-terminal `Accepted` only
            // reads, so the order stays addressable by tag exactly as before.
            oe.event.tag = if oe.event.kind.is_terminal() {
                self.retire_tag_for_coid(idx, &oe.event.client_order_id)
            } else {
                self.tag_for_coid(idx, &oe.event.client_order_id)
            };
            // The MOUNT's engine — this event is being delivered to `idx`, so its context must be
            // that mount's book. `EngineRoute::Mount` falls back to the payload venue for a
            // cross-venue order, which is what a declared foreign leg's event is.
            let eidx = self.route_of(EngineRoute::Mount(idx), &oe.venue).unwrap_or(0);
            // no price rides an order event — use the standing mark if one exists, else 0.0
            let mark = self.eng(eidx).account.mark_of(&oe.venue, &oe.symbol).unwrap_or(0.0);
            // the order-event step sees the SAME closed-bar history on_bar would (the mount's series)
            let bars_arc =
                self.bars.get(&mount_key).map(|s| Arc::clone(&s.closed)).unwrap_or_default();
            let index = bars_arc.len().saturating_sub(1);
            // BEFORE the take: the declared per-symbol read tables (empty for a single-symbol mount,
            // which is what keeps this byte-identical). The DISPATCHING symbol here is the ORDER's,
            // which for a declared-multi mount is routinely a leg's rather than the mount's own —
            // so without this an `on_order_event` reading `position(mount_symbol)` fell through to
            // the scalar below and answered about the order's instrument instead.
            let views = self.declared_views(idx, None, &oe.venue);
            // take/replace so the strategy call can't alias the rest of the core state
            let mut mount = self.mounts[idx].take().expect("just found");
            let mut ctx = LiveBroker {
                positions: views.positions,
                prices: views.prices,
                bar_views: views.bar_views,
                position: self.eng(eidx).position_size_of(&oe.symbol, "BOTH"),
                price: mark,
                equity: self.eng(eidx).resolved_equity(self.seed_of(eidx), &self.config.price_cfg),
                bars: bars_arc,
                index,
                now,
                multiplier: self.eng(eidx).account.multiplier_of(&oe.symbol),
                lot_size: self.eng(eidx).gate.limits.lot_size.unwrap_or(0.0),
                submissions: Vec::new(),
                modifications: Vec::new(),
                cancels: Vec::new(),
                brackets: Vec::new(),
                conditionals: Vec::new(),
                mass_cancel: false,
            };
            mount.strategy.on_order_event(&mut ctx, &oe.event);
            self.mounts[idx] = Some(mount);
            self.drain_broker(ctx, &oe.venue, &oe.symbol, now, idx);
        }
        self.dirty = true;
    }

    /// Deliver account-applied fills to the mounted strategy — the live twin of the backtest
    /// engine's synchronous `on_fill` firing point. Each fill was captured post-dedup at the
    /// ONE `Account::apply_fill` site ([`ExecutionEngine::applied_fills`]) WITH its per-fill
    /// position/equity snapshot, so a WS reconnect replay can never double-fire and a
    /// multi-fill batch shows each handler the state after ITS fill, not the batch's.
    /// `ctx.price` is the standing mark (in the paper bar path that is the PRIOR close —
    /// the backtest fill-phase price), falling back to the fill's own price pre-first-mark.
    /// NOT warmup-gated: the backtest `fire_on_fill` is unconditional, and live fills
    /// (manual tickets, reconcile-era executions) are account facts regardless of bar count.
    /// Fills folded WHILE a handler runs are delivered next round — bounded rounds here;
    /// leftovers drain at the next message, the idle branch, and teardown.
    pub(crate) fn dispatch_applied_fills(&mut self) {
        // Deliver NON-FILL order-lifecycle transitions FIRST (accept/reject/cancel/expire/deny), so
        // an accept-then-fill sequence reaches the strategy in order. Cheap when nothing is buffered
        // (a length check). Folded into this one method so ALL its call sites — per-message, idle,
        // and the two teardown drains — deliver order events too, with no new call sites.
        self.dispatch_order_events();
        for _ in 0..8 {
            // NB `break`, not `return`: the prune sweep at the tail must run on the COMMON path
            // (nothing buffered) too, or `coid_mount` would only ever be bounded on a dispatch that
            // happened to carry fills.
            if self.engine.applied_fills.is_empty()
                && self.extra_engines.iter().all(|(_, e)| e.applied_fills.is_empty())
            {
                break;
            }
            if self.mounts.is_empty() {
                self.engine.applied_fills.clear();
                for (_, e) in self.extra_engines.iter_mut() {
                    e.applied_fills.clear();
                }
                break;
            }
            let mut fills = std::mem::take(&mut self.engine.applied_fills);
            for (_, e) in self.extra_engines.iter_mut() {
                fills.append(&mut e.applied_fills);
            }
            for af in fills {
                let f = &af.fill;
                // steal/core-per-mount-budget ATTRIBUTION: fold this fill into its ORIGINATING
                // mount's ledger (coid -> mount), the weighted-average-cost fold `Account::fold`
                // itself runs (both call `vike_model::compute_fill`), so a mount that trades one
                // symbol tracks the same position the account does. Keyed by coid — the SAME key the
                // strategy routing below now uses, so ledger and callback can never disagree about
                // which mount owns a fill; a fill whose coid no mount minted (a manual ticket /
                // margin-call liquidation) is simply not attributed (see the residual view).
                // Off the p99 hot fold (fills are order cadence); runs even with no budget
                // (attribution is always-on + read-only).
                let attributed = self.coid_mount.get(f.client_order_id.as_str()).copied();
                if let Some(midx) = attributed
                    && midx < self.mount_attr.len()
                {
                    let aeidx = self.route_of(EngineRoute::Mount(midx), &f.venue).unwrap_or(0);
                    let mult = self.eng(aeidx).account.multiplier_of(&f.symbol);
                    let attr = &mut self.mount_attr[midx];
                    let out = vike_model::compute_fill(
                        attr.size,
                        attr.avg_px,
                        f.side,
                        f.last_qty,
                        f.last_px,
                        mult,
                    );
                    attr.size = out.new_size;
                    attr.avg_px = out.new_avg_px;
                    if out.closing_qty > 0.0 {
                        attr.realized_pnl += out.realized_pnl;
                    }
                    attr.fees_paid += f.commission;
                }
                // A FULLY-FILLED order is terminal too — but `OrderLifecycle::from_event` returns
                // `None` for fills, so it never appears on the order-event lane above. This is the
                // ONLY place a filled order's attribution entry can be made retirable, and for a
                // maker it is the MAJORITY of them. The registry's post-fold status is the authority
                // on whether this fill completed the order. Queued, never erased here: the linger
                // window is exactly what protects the NEXT fill on this coid if one still comes.
                // ⚠ THE ORDER's OWN ENGINE, from the submit-time `coid_venue` map, falling back to
                // the venue lookup for a coid this process never submitted. The registry read below
                // asks "did THIS order complete", and asking the wrong account's registry answers
                // about an order it has never heard of — which reads as "not completed" and leaves
                // the attribution entry unretirable.
                let reidx = self
                    .coid_venue
                    .get(f.client_order_id.as_str())
                    .copied()
                    .or_else(|| self.engine_idx_for_route_key(RouteKey::sole_account_of(&f.venue)))
                    .unwrap_or(0);
                let completed = self
                    .eng(reidx)
                    .registry
                    .get(f.client_order_id.as_str())
                    .is_some_and(|mo| mo.status.is_terminal());
                if completed {
                    let (coid, ts) = (f.client_order_id.to_string(), self.engine.now_ms);
                    self.note_terminal_coid(&coid, ts);
                }
                // Route each fill to the mount that MINTED its coid (bug B) — the same exact key the
                // attribution fold just used, so the ledger and the callback can never disagree
                // about which mount a fill belongs to. A fill no mount minted falls back to the
                // historical FIRST mount on (venue, symbol); no mount at all = drop.
                let Some(idx) = self
                    .mount_for_coid(f.client_order_id.as_str())
                    .or_else(|| self.mount_owning(&f.venue, &f.symbol))
                else {
                    continue;
                };
                let mount_key = {
                    let m = self.mounts[idx].as_ref().expect("just found");
                    (m.venue.clone(), m.symbol.clone(), m.interval.clone())
                };
                let fill = Fill {
                    side: f.side,
                    size: f.last_qty,
                    price: f.last_px,
                    fee: f.commission,
                    ts: f.ts,
                    is_maker: f.liquidity_side.is_maker(),
                    symbol: f.symbol.to_string(),
                };
                let feidx = self.route_of(EngineRoute::Mount(idx), &f.venue).unwrap_or(0);
                let mark = self.eng(feidx).account.mark_of(&f.venue, &f.symbol).unwrap_or(0.0);
                let bars_arc =
                    self.bars.get(&mount_key).map(|s| Arc::clone(&s.closed)).unwrap_or_default();
                // pre-seed the cache may be empty: index 0 with empty bars — the same
                // contract as the tick lane (strategies must not index blindly pre-bars)
                let index = bars_arc.len().saturating_sub(1);
                // BEFORE the take: the declared per-symbol read tables (empty for a single-symbol
                // mount, which is what keeps this byte-identical). ⚠ The FILL's symbol is the
                // dispatching one, so it gets NO row and `position(f.symbol)` keeps answering from
                // the `af.position_after` scalar below — the state after THIS fill rather than after
                // the whole batch, which is the guarantee `applied_fills`' per-fill snapshot exists
                // for. Every OTHER declared instrument now answers about itself instead of about the
                // filled one — a misread `vike_mm::xemm`'s `HedgeLedger` used to name as one of its
                // three reasons for folding its own fill stream rather than reading the `Broker`
                // seam. That doc is amended: two of the three were THIS defect and its cross-venue
                // sibling; what survives is intrinsic (a symbol-less `HftBroker::position`, and
                // in-flight hedge state no broker read of any shape can see).
                let views = self.declared_views(idx, Some(&f.symbol), &f.venue);
                // The TAG this completed order was resting under, retired in the same step (see
                // `retire_tag_for_coid`). Resolved BEFORE the mount is taken below, because the
                // lookup builds its key from the mount's own (venue, symbol). `None` for a partial
                // fill, an untagged order, or a coid whose tag has already been re-pointed at a
                // newer quote.
                let completed_tag = if completed {
                    self.retire_tag_for_coid(idx, &f.client_order_id)
                } else {
                    None
                };
                let mut mount = self.mounts[idx].take().expect("just found");
                let mut ctx = LiveBroker {
                    positions: views.positions,
                    prices: views.prices,
                    bar_views: views.bar_views,
                    position: af.position_after,
                    price: if mark > 0.0 { mark } else { fill.price },
                    equity: af.equity_after,
                    bars: bars_arc,
                    index,
                    now: fill.ts,
                    multiplier: self.eng(feidx).account.multiplier_of(&f.symbol),
                    lot_size: self.eng(feidx).gate.limits.lot_size.unwrap_or(0.0),
                    submissions: Vec::new(),
                    modifications: Vec::new(),
                    cancels: Vec::new(),
                    brackets: Vec::new(),
                    conditionals: Vec::new(),
                    mass_cancel: false,
                };
                mount.strategy.on_fill(&mut ctx, &fill);
                // ...and, when THIS fill completed the order, the order's DEATH — the third way a
                // resting order dies, and for a maker the most common. `on_fill` above cannot carry
                // it: `Fill` has no client-order-id, no tag and no remaining quantity, so a strategy
                // could only guess (and guesses wrong on any partial-fill sequence). Delivered
                // SECOND and on the SAME `LiveBroker` snapshot, so the money is folded before the
                // slot is freed and anything either hook buffers drains together through the one
                // live path below. `completed` is the post-fold registry status already read above —
                // the authority — so this adds no new judgement, only a delivery.
                if completed {
                    mount.strategy.on_order_event(
                        &mut ctx,
                        &vike_model::strategy::OrderLifecycle {
                            client_order_id: f.client_order_id.to_string(),
                            tag: completed_tag,
                            kind: vike_model::strategy::OrderEventKind::Filled,
                        },
                    );
                }
                self.mounts[idx] = Some(mount);
                let (venue, symbol, ts) = (f.venue, f.symbol, f.ts);
                self.drain_broker(ctx, &venue, &symbol, ts, idx);
            }
            self.dirty = true;
        }
        // AFTER every delivery round, never before: a fill buffered for THIS dispatch must have had
        // its chance to find its owner before any entry is retired.
        self.prune_terminal_coids();
    }

    /// Queue `coid`'s attribution entry for retirement, stamped `now` — called when its order is
    /// first observed TERMINAL (a non-fill lifecycle transition in [`Self::dispatch_order_events`],
    /// or a fill that COMPLETED it in [`Self::dispatch_applied_fills`]).
    ///
    /// **This ERASES NOTHING.** Removing `coid_mount`'s entry on the terminal itself is the obvious
    /// implementation and it is wrong. `Event::Fill` is a SEPARATE event from the FSM terminal that
    /// accompanies it, the two carry no ordering contract, and `Account::apply_fill` folds a fill
    /// regardless of FSM state (the terminal-drop guard is on the FSM lane only) — so a partial fill
    /// racing its own cancel ack, or a reconnect re-delivering executions, lands after the order is
    /// terminal. With the entry already gone that fill is UNATTRIBUTED, and an unattributed fill
    /// silently books into the residual row instead of the mount that traded it: the very defect the
    /// previous commit fixed for the budget-latch flatten, reintroduced through the back door. A
    /// bound on a map is not worth that. So the entry LINGERS for [`COID_PRUNE_LINGER_MS`] and
    /// [`Self::prune_terminal_coids`] retires it later.
    ///
    /// Only coids `coid_mount` actually holds are queued (an operator ticket or a reconcile-adopted
    /// venue order was never attributed and has nothing to retire). A repeat — an order seen
    /// terminal more than once — simply queues a second entry; the later `remove` is a no-op, which
    /// is cheaper than de-duplicating on the way in.
    fn note_terminal_coid(&mut self, coid: &str, now: i64) {
        if self.coid_mount.contains_key(coid) {
            self.coid_terminal.push_back((coid.to_string(), now));
        }
    }

    /// Retire every queued attribution entry that has been terminal for at least
    /// [`COID_PRUNE_LINGER_MS`] — the drain half of [`Self::note_terminal_coid`], and the bound on
    /// `coid_mount`'s growth.
    ///
    /// The queue is append-ordered on a monotone core clock, so the eligible entries are a PREFIX:
    /// pop from the front while the head has aged out, stop at the first that has not. An empty
    /// queue — and a queue whose head is still fresh, which is the steady state — costs one
    /// `front()` and one comparison, which is why this can sit on `dispatch_applied_fills`'s tail
    /// without touching the `p99 < 10µs` budget (that method is called per message, but every one of
    /// its bodies is already gated behind an emptiness check).
    fn prune_terminal_coids(&mut self) {
        let now = self.engine.now_ms;
        let aged = |q: &VecDeque<(String, i64)>| {
            q.front().is_some_and(|(_, ts)| now.saturating_sub(*ts) >= COID_PRUNE_LINGER_MS)
        };
        while aged(&self.coid_terminal) {
            if let Some((coid, _)) = self.coid_terminal.pop_front() {
                self.coid_mount.remove(&coid);
            }
        }
    }

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
            equity: self.eng(teidx).resolved_equity(self.seed_of(teidx), &self.config.price_cfg),
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
                equity: self.eng(eidx).resolved_equity(self.seed_of(eidx), &self.config.price_cfg),
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
                equity: self.eng(eidx).resolved_equity(self.seed_of(eidx), &self.config.price_cfg),
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
                equity: self.eng(eidx).resolved_equity(self.seed_of(eidx), &self.config.price_cfg),
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
                equity: self.eng(eidx).resolved_equity(self.seed_of(eidx), &self.config.price_cfg),
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
            equity: self.eng(eidx).resolved_equity(self.seed_of(eidx), &self.config.price_cfg),
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
    /// [`crate::journal::JournalRecord::ScheduleFire`] audit marker, then `on_schedule` runs (guarded
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
                equity: self.eng(eidx).resolved_equity(self.seed_of(eidx), &self.config.price_cfg),
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
