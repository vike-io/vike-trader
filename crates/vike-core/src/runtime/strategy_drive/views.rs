//! The read side of a declared-multi mount: its per-symbol views and the venue a leg resolves to.

use super::*;

impl<C: ExecutionClient> CoreThread<C> {
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
    /// from shipped code: `vike_strategy::strategies::pairs::PairsZScore` evaluates from whichever leg's bar
    /// completes the pair, and its `flatten` reads `broker.position(&sym)` for BOTH legs before
    /// submitting `p.abs()` at `vike_model::closing_side(p)`. Reached from a leg-B dispatch it closed
    /// leg A with leg B's quantity — and, whenever the two legs' signs differ (the normal case for a
    /// beta-weighted spread), with the wrong SIDE. `SimBroker` indexes every read by symbol, so the
    /// same strategy backtested correctly. `crates/vike-sim/tests/multi_symbol_read_parity.rs`
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
    pub(crate) fn declared_views(
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
    pub(crate) fn leg_venue(&self, mount_idx: usize, symbol: &str, fallback: &str) -> String {
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
}
