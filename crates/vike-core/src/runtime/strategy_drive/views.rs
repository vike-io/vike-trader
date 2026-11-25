//! The read side of a declared-multi mount: its per-symbol views and the venue a leg resolves to.

use super::*;

impl<C: ExecutionClient> CoreThread<C> {
    /// The per-symbol position/mark tables a DECLARED-multi mount's `OrderBroker::position(symbol)` /
    /// `price(symbol)` read from.
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
    /// from shipped code: the file `strategies/pairs/pairs_zscore.rs`'s `PairsZScore` evaluates from
    /// whichever leg's bar completes the pair, and its `flatten` reads `broker.position(&symbol)` for BOTH legs before
    /// submitting `p.abs()` at `vike_model::closing_side(p)`. Reached from a leg-B dispatch it closed
    /// leg A with leg B's quantity — and, whenever the two legs' signs differ (the normal case for a
    /// beta-weighted spread), with the wrong SIDE. `OrderBrokerSimulator` indexes every read by symbol, so the
    /// same strategy backtested correctly. `crates/vike-simulator/tests/parity/multi_symbol_read_parity.rs`
    /// pins the law in both engines.
    ///
    /// ⚠ **The dispatching symbol must NOT get a row, and that is load-bearing rather than an
    /// optimization.** On the fill lane the scalar is `AppliedFill::position_after` — the position
    /// after THIS fill, which is why a multi-fill batch shows each handler its own state rather than
    /// the batch's. A table row would answer from `position_size_of`, i.e. the account AFTER the
    /// whole batch folded, silently undoing that guarantee. On the bar/tick lanes the scalar is the
    /// fresher number too (this bar's close / this tick's price, versus the resolver's standing
    /// mark). The skip is by SYMBOL, not by (venue, symbol): `OrderBroker::position` takes a symbol and
    /// nothing else, so a table keyed any finer could not be looked up, and a mount that declares
    /// its own symbol again on a foreign venue simply cannot address the two apart.
    ///
    /// ## Which ENGINE each row is read from — the same one the ORDER would be routed to
    ///
    /// A row resolves against `engine_index_for_route_key(<that leg's venue>).unwrap_or(0)`, which is
    /// VERBATIM what `apply_intent` does with `OrderRequest::venue`, so a read and a write can never
    /// disagree about which book a leg lives in. Before this, every leg was read out of the
    /// DISPATCHING venue's engine, so a leg declared `MountLeg::at(symbol, other_venue)` — the shape an
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
    /// Every row here is `position_size_of(symbol)`, read when the views are built — which on the fill
    /// lane is AFTER the whole `applied_fills` batch has folded into the account. The dispatching
    /// symbol is protected (it has no row, so `LiveBroker::position` answers from
    /// `AppliedFill::position_after`), but a NON-dispatching declared leg is not: during
    /// `on_fill(fill_A)` a two-leg mount can read leg B's position reflecting a fill it has not yet
    /// been told about.
    ///
    /// That is look-ahead inside one batch, and a live-vs-backtest divergence — `OrderBrokerSimulator` folds
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
        mount_index: usize,
        skip_symbol: Option<&str>,
        dispatch_venue: &str,
    ) -> DeclaredViews {
        if !self.any_mount_multi || self.mount_symbols[mount_index].is_empty() {
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
        let Some((own_venue, own_symbol, interval)) = self.mounts[mount_index]
            .as_ref()
            .map(|m| (m.venue.clone(), m.symbol.clone(), m.interval.clone()))
        else {
            return DeclaredViews::default();
        };
        // The values were resolved cold (`rebuild_leg_values`): one refcount bump, no lookup.
        let mut views = DeclaredViews {
            leg_values: self.mount_leg_values.get(mount_index).map(Arc::clone),
            ..DeclaredViews::default()
        };
        // The mount's OWN symbol goes in FIRST, so that a mount which also declares its own symbol
        // as a leg on a FOREIGN venue still reads its own book under that name: `declared_read`
        // takes the first match, and the mount's own instrument is the less surprising answer for
        // the mount's own symbol.
        let own = (own_symbol.as_str(), own_symbol.as_str());
        self.push_view(mount_index, &own_venue, own, &interval, skip_symbol, &mut views);
        for leg in &self.mount_symbols[mount_index] {
            // ⚠ `dispatch_venue`, NOT `own_venue`. This is the read half of [`Self::leg_venue`]'s
            // one rule: the write half resolves a `venue: None` leg against the venue `drain_broker`
            // was called with, so reading it against the MOUNT's venue instead would answer from a
            // different book than the very next `submit` writes to. See `leg_venue`'s doc for the
            // concrete cross-venue fill that makes them differ.
            let leg_venue = self.leg_venue(mount_index, &leg.name, dispatch_venue);
            self.push_view(
                mount_index,
                &leg_venue,
                (&leg.name, &leg.symbol),
                &interval,
                skip_symbol,
                &mut views,
            );
        }
        views
    }

    /// One leg's row in [`Self::declared_views`]' tables, read out of `venue`'s own engine (the leg's
    /// account's, [`Self::leg_engine`]): `(name, symbol)` is the strategy's name of the leg, which keys
    /// the row, and the instrument's, which the engine is read by (the two differ for a leg named by its
    /// account, decision 0293).
    ///
    /// Writes NOTHING to the position/price tables for the DISPATCHING symbol (the ctx scalars
    /// answer for it — see [`Self::declared_views`]) and nothing for a symbol already in a table, so
    /// `declared_read`'s first-match scan can never see two rows for one symbol.
    ///
    /// A price the resolver cannot answer (`Missing` — no mark, no quote, no trade, no bar for that
    /// cell) writes NO price row while the POSITION row still lands, which leaves `OrderBroker::price` on
    /// its scalar fallback: there is no defensible number to invent, and a fabricated one would be
    /// indistinguishable from a real mark to the strategy reading it. (That fallback lands on the
    /// DISPATCHING series' price, which for a non-dispatching leg is the wrong instrument —
    /// `LiveBroker::price`'s doc carries the residual and why closing it is a separate change.)
    ///
    /// ## ⚠ The BAR row is written UNCONDITIONALLY — before the skip, for every declared symbol
    ///
    /// The two exclusions above are why neither table can answer "does this mount carry `symbol`",
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
    /// Same `(venue, symbol)` pair as the position row, hence the same `Self::leg_venue` rule: a leg's
    /// history comes from the venue its orders route to. A cross-venue mount reading `bars(leg)`
    /// during a bar of the SAME symbol on a different venue therefore gets the declared venue's
    /// series, matching what `position(leg)` already answers.
    fn push_view(
        &self,
        mount_index: usize,
        venue: &str,
        (name, symbol): (&str, &str),
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
        // to get a row, so `price(leg)` came from `resolved_position_price` — the `price_config`
        // resolver (mark > quote > trade > bar-close, with its staleness policy). Skipping drops it
        // to the raw `bar.close`, so a leg carrying a fresh venue mark would silently report the bar
        // close instead. That path was already wired and working; an earlier draft of this change
        // skipped unconditionally and altered it without saying so.
        if !out.bar_views.iter().any(|(s, _)| s == name) {
            let key = (venue.to_string(), symbol.to_string(), interval.to_string());
            let closed = self.bars.get(&key).map(|s| Arc::clone(&s.closed)).unwrap_or_default();
            out.bar_views.push((name.to_string(), closed));
        }
        if skip_symbol == Some(name) || out.positions.iter().any(|(s, _)| s == name) {
            return;
        }
        // ⚠ The MOUNT's engine when this leg is on the mount's own venue, the venue's default
        // account otherwise (a cross-exchange declared leg). Same rule as the WRITE half
        // (`EngineRoute::Mount`), reached through the same function — a leg whose read came from the
        // default account while its order went to a labelled one is the read/write split
        // [`Self::leg_venue`]'s own doc records as having been measured once already.
        let engine_index = self.leg_engine(mount_index, name, venue);
        let pos = self
            .engine(engine_index)
            .position_size_of(symbol, vike_model::events::PositionSide::Both);
        out.positions.push((name.to_string(), pos));
        if let Some(px) = self.engine(engine_index).resolved_position_price(
            venue,
            symbol,
            pos,
            &self.config.price_config,
        ) {
            out.prices.push((name.to_string(), px));
        }
    }

    /// Resolve [`CoreThread::mount_leg_values`]: for every DECLARED mount, its own symbol and each leg
    /// with the value the leg's account books it by (`Account::base_value_of`, on the engine
    /// [`Self::push_view`] reads that leg from, so the value, the position and the order name one
    /// book), the grid that engine's gate holds for it (its own `grid_by_symbol` row, which an
    /// admission installs) and its stream's delay label. COLD, and the table's one writer: at assembly,
    /// after a mount is added or removed (`recompute_mount_gates`), after an admission, a refresh or a
    /// release, after an account is added, stopped or detached, and after a stream's label changes on
    /// a core with a declared mount (`note_data_delay`) — every change to a value an account books a
    /// symbol by, to a label, or to the engine a leg resolves to. A dispatch only borrows the row.
    #[cold]
    pub(crate) fn rebuild_leg_values(&mut self) {
        let values = (0..self.mounts.len()).map(|i| self.leg_values_of(i)).collect();
        self.mount_leg_values = values;
        // …and the legs each published mount row names (`MountView::legs`), from the same declaration.
        let rows = (0..self.mounts.len()).map(|i| self.leg_rows_of(i)).collect();
        self.mount_leg_rows = rows;
    }

    /// One mount's `(venue, symbol)` legs for its published row ([`CoreThread::mount_leg_rows`]): `None`
    /// for a mount that declared nothing (or a slot that holds no mount).
    fn leg_rows_of(&self, mount_index: usize) -> crate::runtime::core_thread::MountLegRow {
        let legs = self.mount_symbols.get(mount_index).filter(|l| !l.is_empty())?;
        let m = self.mounts.get(mount_index)?.as_ref()?;
        let rows: Vec<(String, String)> = legs
            .iter()
            .map(|l| (self.leg_venue(mount_index, &l.name, &m.venue), l.symbol.clone()))
            .collect();
        Some(rows.into())
    }

    /// One mount's row of [`Self::rebuild_leg_values`]; EMPTY for a mount that declared nothing (or
    /// a slot that holds no mount).
    fn leg_values_of(&self, mount_index: usize) -> Arc<[crate::runtime::broker::LegFacts]> {
        let legs = self.mount_symbols.get(mount_index).filter(|l| !l.is_empty());
        let (Some(legs), Some(Some(m))) = (legs, self.mounts.get(mount_index)) else {
            return Arc::from([]);
        };
        // `declared_views`' order (the mount's own symbol first, first match wins) and its venue rule.
        let names = std::iter::once((m.venue.clone(), m.symbol.as_str(), m.symbol.as_str())).chain(
            legs.iter().map(|l| {
                (self.leg_venue(mount_index, &l.name, &m.venue), l.name.as_str(), l.symbol.as_str())
            }),
        );
        let mut out: Vec<crate::runtime::broker::LegFacts> = Vec::with_capacity(legs.len() + 1);
        for (venue, name, symbol) in names {
            if out.iter().any(|(s, ..)| s == name) {
                continue;
            }
            let engine = self.engine(self.leg_engine(mount_index, name, &venue));
            // The leg's OWN row only: `grid_for` answers the engine's scalars (another symbol's grid)
            // for a symbol with none.
            let limits = &engine.gate.limits;
            let grid = limits.grid_by_symbol.contains_key(symbol).then(|| limits.grid_for(symbol));
            // The label the leg's own `(venue, symbol)` stream reported (decision 0140), the table the
            // submit path stamps its orders from.
            let delay = self.delay_of(&venue, symbol);
            out.push((name.to_string(), engine.account.base_value_of(symbol), grid, delay));
        }
        out.into()
    }

    /// **THE one rule for "which venue does this declared leg live on".**
    ///
    /// A `MountLeg::at(symbol, venue)` names its venue; a `MountLeg::same_venue(symbol)` does not, and
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
    /// the mount's own symbol, which the reads read on the mount's venue and for which
    /// [`Self::resolve_intent_venue`] passes the mount's venue as `fallback` (step 12 of the one
    /// broker plan: `""` is the mount's own), and an undeclared symbol the order path is about to
    /// refuse.
    ///
    /// Byte-identical on the bar and tick lanes, which is where every shipped mount lives today: a
    /// same-venue leg's bar arrives on the mount's own venue, so `fallback` already WAS that venue
    /// and the two spellings pick the same string. Only a dispatch whose venue is foreign — a
    /// cross-venue fill or order event — changes, and only to the venue the leg was declared on.
    ///
    /// `a_same_venue_leg_reads_and_writes_the_same_book` is the gate, and it asserts BOTH halves
    /// land on the mount's venue rather than merely landing together — agreement alone is what the
    /// previous rule already had.
    pub(crate) fn leg_venue(&self, mount_index: usize, name: &str, fallback: &str) -> String {
        let Some(leg) = self.mount_symbols[mount_index].iter().find(|l| l.name == name) else {
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
        self.mounts[mount_index]
            .as_ref()
            .map(|m| m.venue.clone())
            .unwrap_or_else(|| fallback.to_string())
    }

    /// The leg this mount declared under `name` when the strategy names it by its account
    /// (`SYMBOL@venue/LABEL`, decision 0293); `None` for every other name, one scan of the mount's legs.
    fn leg_named_by_account(&self, mount_index: usize, name: &str) -> Option<&MountLeg> {
        self.mount_symbols.get(mount_index)?.iter().find(|l| l.names_an_account() && l.name == name)
    }

    /// **The ENGINE a leg the strategy names `name` books on**, read and written alike: a leg named by
    /// a LABELLED account, that account's engine (the one its orders' `account` routes to); every other
    /// name, the engine `EngineRoute::Mount` resolves for `venue`, as before (decision 0293).
    pub(crate) fn leg_engine(&self, mount_index: usize, name: &str, venue: &str) -> usize {
        self.leg_named_by_account(mount_index, name)
            .and_then(|leg| leg.account.as_ref())
            .and_then(|label| {
                let key = vike_model::accounts::account_keys::route_key_of(venue, label);
                self.engine_index_for_route_key(RouteKey::declared(&key))
            })
            .or_else(|| self.route_of(EngineRoute::Mount(mount_index), venue))
            .unwrap_or(0)
    }

    /// The SYMBOL and ACCOUNT an order on the leg the strategy names `name` is sent with: a leg named by
    /// its account sends its symbol and carries its account; every other name is its own symbol and
    /// carries none, as before (decision 0293).
    pub(crate) fn leg_order_target(
        &self,
        mount_index: usize,
        name: &str,
    ) -> (String, Option<vike_model::accounts::account_keys::AccountLabel>) {
        match self.leg_named_by_account(mount_index, name) {
            Some(leg) => (leg.symbol.clone(), leg.account.clone()),
            None => (name.to_string(), None),
        }
    }

    /// **The route a strategy intent of mount `mount_index` lowers by**: [`EngineRoute::Mount`], unless the
    /// intent names an ACCOUNT of the mount's own venue other than the mount's (a leg on another account of
    /// its venue, decision 0293). `EngineRoute::Mount` keeps the mount's own book on its own exchange and
    /// never consults the payload's account there, so such an intent lowers onto that account's engine
    /// ([`EngineRoute::Engine`]), or by the payload ([`EngineRoute::Payload`]) when no engine runs that
    /// account, which refuses it in words rather than placing it on the mount's book. A foreign venue's
    /// account is the payload's to resolve, as before. Order cadence: one match and, for an intent naming
    /// an account, one route-key lookup.
    pub(crate) fn strategy_route(&self, mount_index: usize, intent: &OrderIntent) -> EngineRoute {
        let (venue, account) = match intent {
            OrderIntent::Submit(r) => (r.venue.as_str(), r.account.as_ref()),
            OrderIntent::Bracket(s) => (s.venue.as_str(), s.account.as_ref()),
            OrderIntent::OrderList(s) => (s.venue.as_str(), s.account.as_ref()),
            _ => return EngineRoute::Mount(mount_index),
        };
        let mount_engine = self.mount_engine(mount_index);
        let Some(label) = account.filter(|_| self.engine(mount_engine).venue == venue) else {
            return EngineRoute::Mount(mount_index);
        };
        let key = vike_model::accounts::account_keys::route_key_of(venue, label);
        match self.engine_index_for_route_key(RouteKey::declared(&key)) {
            Some(i) if i == mount_engine => EngineRoute::Mount(mount_index),
            Some(i) => EngineRoute::Engine(i),
            None => EngineRoute::Payload,
        }
    }

    /// The name the strategy gives the instrument `symbol` a fill of engine `engine_index` (on `venue`)
    /// booked: the leg's name when the mount declared it by an account of that engine, else `symbol`
    /// (decision 0293). Order cadence; one scan of the mount's legs.
    pub(crate) fn leg_name_of_fill(
        &self,
        mount_index: usize,
        venue: &str,
        symbol: &str,
        engine_index: usize,
    ) -> String {
        let route_key = &self.engine(engine_index).route_key;
        self.mount_symbols
            .get(mount_index)
            .and_then(|legs| {
                legs.iter().find(|l| {
                    l.names_an_account()
                        && l.symbol == symbol
                        && l.venue.as_deref() == Some(venue)
                        && l.account.as_ref().is_none_or(|label| {
                            vike_model::accounts::account_keys::route_key_of(venue, label)
                                == *route_key
                        })
                })
            })
            .map_or_else(|| symbol.to_string(), |l| l.name.clone())
    }
}
