//! THE FUNDING-RATE LANE's fold half (`Ingest::FundingRate`): a PAPER engine pays and receives funding
//! from its venue's PUBLIC rate, the way the venue charges a real account.
//!
//! The venue's market feed states its current rate and the symbol's next settlement time
//! (`vike_data::DataClientSink::funding`, carried gateway -> datahub -> tradehub). Each observation goes
//! to every engine of that venue whose STATED tier is paper and that holds the symbol
//! (`vike_execution::Engine::note_funding_rate`, the engine's own funding clock), and the first time
//! this process sees a settlement time a [`TimerKind::Funding`] is armed AT it. The settlement is booked
//! when that timer fires ([`CoreThread::settle_due_funding`]), so it is charged on the position held at
//! the settlement instant, at the symbol's OWN time (the feed's `T`/`nextFundingTime`, never a fixed
//! interval: 1, 2, 4 or 8 hours, an hour on a capped binance or okx rate); the venue's roll to its next
//! settlement books it instead if that comes first. The amount is `vike_model::ContractValue::funding`
//! (armed by `CoreConfig::charge_paper_funding`; the simulator's one formula, an inverse contract
//! pays in its settle coin, decision 0258) on the ENGINE's position (its book, so a position restored
//! after a restart pays too) at the venue's funding price: the observation's `price` where the venue
//! states one (Hyperliquid's oracle), else the engine's mark. It is folded as an ordinary
//! `Event::Funding` through `dispatch`: journaled write-ahead and routed like a venue's own funding, so a
//! replay books the same money with no rate lane, and `Account::apply_funding` books it as it books a
//! live venue's.
//!
//! What it never does: charge a LIVE or demo engine (its venue's own settlement events are its
//! funding, and charging from the rate too would count it twice), charge when the host did not ask for it
//! (`charge_paper_funding` off: every replay core), or charge a settlement twice (the clock remembers the last one booked; a
//! funding event folded on the engine, replayed too, advances it). A clock that booked a settlement is
//! snapshotted at once (`write_snap`), so a restart never re-books one this engine already passed.
//!
//! THE RESTART CATCH-UP ([`CoreThread::catch_up_funding`]). A node down across SEVERAL settlements
//! catches them up when the host hands in the venue's stored history (`CoreConfig::funding_history`,
//! the datahub's `interval=funding` series): at the first observation after the restart, a RESTORED
//! clock (one this process has not armed) that the venue rolled past asks the history once for the
//! settlements between its last booked one and the venue's next, and each one the series holds is
//! booked at its STORED rate (and stored mark, else the engine's) on the engine's position, which is
//! the restored one: a paper engine does nothing while the node is down. A missed settlement the series
//! lacks is booked at no rate, never a guessed one, and named in one WARN and ring line: the one the
//! clock was told of when the series lacks it, and those inside a spacing wider than one and a half
//! times the wider of its neighbours (the backtest's yardstick, `vike_backtest`'s `funding_gaps`), at
//! that neighbour's spacing. The clock then counts every missed settlement passed, booked or not, and
//! the clock is snapshotted, so a second restart books nothing more. Without a history the old restart
//! stands: the first settlement the venue rolled past is booked at the rate it stated last.
//!
//! Limits, stated: the catch-up charges the position the engine holds at the first observation after
//! the restart (a strategy that trades before the venue's first funding message has moved it); a
//! series whose missed stretch has no neighbouring spacing (one point and no booked settlement before
//! it) cannot be judged for holes; a symbol whose price the engine does not hold is not charged (said
//! once per settlement); and only a symbol whose venue mark lane runs (a perpetual's bars
//! subscription) is fed.
//!
//! COLD: an observation arrives about once a second per perpetual, never per trade. On a node with no
//! paper engine (every live node) it returns at its first line (`CoreThread::any_paper_engine`); on one
//! with paper engines it walks the venue's engines by index and collects no list. A settlement is one
//! timer per settlement time; the history is read once per restored clock. Nothing here runs on the
//! per-message fold.

use vike_model::AccountTier;
use vike_model::events::{FundingEvent, PositionSide};

use super::*;

impl<C: ExecutionClient> CoreThread<C> {
    /// One funding-rate observation (the module doc): the paper engines' clocks, a settlement the venue
    /// rolled past booked at once, and a timer armed at a settlement time seen for the first time.
    #[cold]
    #[inline(never)]
    pub(crate) fn note_funding_rate(&mut self, update: vike_execution::FundingRateUpdate) {
        // A node with no paper engine (every live node) does nothing more per observation.
        if !self.any_paper_engine {
            return;
        }
        if !self.config.charge_paper_funding {
            return;
        }
        let vike_execution::FundingRateUpdate { venue, symbol, funding } = update;
        let Some(next_ts) = funding.next_timestamp else { return };
        let mut charges = Vec::new();
        let mut booked = false;
        // The venue's engines as `engines_of_venue` orders them (its sole account first, then, on a core that
        // mounts several accounts of a venue, the others), walked by index: nothing is collected.
        let primary = self.engine_index_for_route_key(RouteKey::sole_account_of(&venue));
        let multi = self.multi_account;
        let others = (0..=self.extra_engines.len()).filter(move |&i| multi && Some(i) != primary);
        for engine_index in primary.into_iter().chain(others) {
            let engine = self.engine(engine_index);
            let of_venue = Some(engine_index) == primary
                || (engine.venue == venue && !self.is_retired(engine_index));
            if !of_venue
                || engine.stated_tier() != Some(AccountTier::Paper)
                || !engine.accepts_symbol(&symbol)
            {
                continue;
            }
            if let Some(caught) = self.catch_up_funding(engine_index, &venue, &symbol, next_ts) {
                booked = true;
                charges.extend(caught);
            }
            let note = self.engine_mut(engine_index).note_funding_rate(
                &symbol,
                funding.rate,
                funding.price,
                next_ts,
            );
            if let Some(at) = note.arm_at {
                self.timers.insert(at, TimerKind::Funding);
            }
            if let Some(due) = note.due {
                booked = true;
                charges.extend(self.paper_funding_event(engine_index, &symbol, due));
            }
        }
        self.book_paper_funding(charges, booked);
    }

    /// [`TimerKind::Funding`]: book every paper engine's settlement whose time has come, on the position
    /// it holds now (the settlement instant, give or take the boundary).
    #[cold]
    #[inline(never)]
    pub(crate) fn settle_due_funding(&mut self, now_ms: i64) {
        if !self.config.charge_paper_funding {
            return;
        }
        let mut charges = Vec::new();
        let mut booked = false;
        for engine_index in 0..=self.extra_engines.len() {
            if self.is_retired(engine_index)
                || self.engine(engine_index).stated_tier() != Some(AccountTier::Paper)
            {
                continue;
            }
            for (symbol, due) in self.engine_mut(engine_index).settle_due_funding(now_ms) {
                booked = true;
                charges.extend(self.paper_funding_event(engine_index, &symbol, due));
            }
        }
        self.book_paper_funding(charges, booked);
    }

    /// Whether any running engine states the `paper` tier ([`Self::any_paper_engine`]): asked where the engine
    /// set changes, never per observation.
    pub(crate) fn refresh_paper_engines(&mut self) {
        self.any_paper_engine = (0..=self.extra_engines.len()).any(|engine_index| {
            !self.is_retired(engine_index)
                && self.engine(engine_index).stated_tier() == Some(AccountTier::Paper)
        });
    }

    /// THE RESTART CATCH-UP (the module doc) of engine `engine_index`'s clock of `symbol`, the venue now
    /// announcing its next settlement at `next_timestamp`: the funding events of every missed settlement the
    /// stored history holds, the misses it lacks named once, and the clock counting them all passed.
    /// `None` when there is nothing to catch up: no history handed in, no clock, a clock this process
    /// armed (not a restored one), or one the venue has not rolled past.
    #[cold]
    #[inline(never)]
    fn catch_up_funding(
        &mut self,
        engine_index: usize,
        venue: &str,
        symbol: &str,
        next_ts: i64,
    ) -> Option<Vec<FundingEvent>> {
        let clock =
            self.engine(engine_index).funding_clocks().iter().find(|c| c.symbol == symbol)?.clone();
        if clock.armed || next_ts <= clock.next_timestamp.max(clock.settled_timestamp) {
            return None;
        }
        // With no booked settlement the clock starts at the one it was told of: nothing before it is owed.
        let after = if clock.settled_timestamp > 0 {
            clock.settled_timestamp
        } else {
            clock.next_timestamp - SAME_SETTLEMENT_MS
        };
        let history = self.config.funding_history.as_mut()?;
        let read = history(venue, symbol, after, next_ts);
        let route_key = self.engine(engine_index).route_key.clone();
        let stored = read.unwrap_or_else(|error| {
            tracing::warn!(
                target: "vike_core::core",
                %route_key,
                symbol,
                %error,
                "PAPER FUNDING CATCH-UP: the stored funding history could not be read"
            );
            Vec::new()
        });
        let (dues, missing) = plan_catch_up(&clock, next_ts, &stored);
        if !missing.is_empty() {
            tracing::warn!(
                target: "vike_core::core",
                %route_key,
                symbol,
                ?missing,
                "PAPER FUNDING NOT BOOKED: no stored rate for a settlement missed while the node was down"
            );
            self.note(format!(
                "PAPER FUNDING NOT BOOKED on {route_key} {symbol}: no stored rate for the settlement(s) \
                 {missing:?} missed while the node was down; they are not booked later"
            ));
        }
        // The settlement the clock was told of is passed too, whatever millisecond the store stamped it at.
        let passed = dues
            .iter()
            .map(|d| d.settled_timestamp)
            .chain(missing.iter().copied())
            .fold(clock.next_timestamp, i64::max);
        self.engine_mut(engine_index).settle_funding_clock(symbol, passed);
        Some(
            dues.into_iter()
                .filter_map(|d| self.paper_funding_event(engine_index, symbol, d))
                .collect(),
        )
    }

    /// The funding event engine `engine_index` owes for `due` on `symbol`: `ContractValue::funding` on its position at the venue's
    /// funding price (the stated one, else the engine's mark). `None` when the engine is flat there, or
    /// holds no usable price (said once, by name), or the formula answers no finite amount.
    fn paper_funding_event(
        &mut self,
        engine_index: usize,
        symbol: &str,
        due: vike_execution::FundingDue,
    ) -> Option<FundingEvent> {
        let engine = self.engine(engine_index);
        let size = engine.gate_position_size(symbol);
        if size == 0.0 {
            return None;
        }
        let price = due.price.unwrap_or_else(|| engine.mark_of(symbol));
        if !(price.is_finite() && price > 0.0) {
            let route_key = engine.route_key.clone();
            tracing::warn!(
                target: "vike_core::core",
                %route_key,
                symbol,
                settled_ts = due.settled_timestamp,
                "PAPER FUNDING NOT BOOKED: the engine holds no price for the symbol"
            );
            self.note(format!(
                "PAPER FUNDING NOT BOOKED on {route_key} {symbol}: no price at the settlement {}",
                due.settled_timestamp
            ));
            return None;
        }
        let paid = engine.account.contract_value_of(symbol).funding(size, price, due.rate);
        if !paid.is_finite() {
            return None;
        }
        Some(FundingEvent {
            venue: vike_marketdata::Symbol::from(engine.venue.as_str()),
            symbol: vike_marketdata::Symbol::from(symbol),
            position_side: PositionSide::Both,
            funding_rate: due.rate,
            amount: -paid,
            mark_price: Some(price),
            timestamp: due.settled_timestamp,
            // The venue's default account carries no key (its bytes are a venue event's), a labelled one
            // its own, so the journaled event routes back to this engine on a replay.
            route_key: (engine.route_key != engine.venue)
                .then(|| vike_marketdata::Symbol::from(engine.route_key.as_str())),
            currency: None,
        })
    }

    /// Fold `charges` as journaled venue events through [`Self::dispatch`], then snapshot when a clock
    /// booked a settlement (`booked`), charged or passed flat.
    fn book_paper_funding(&mut self, charges: Vec<FundingEvent>, booked: bool) {
        for event in charges {
            tracing::info!(
                target: "vike_core::core",
                venue = %event.venue,
                symbol = %event.symbol,
                rate = event.funding_rate,
                amount = event.amount,
                price = ?event.mark_price,
                settled_ts = event.timestamp,
                "paper funding booked from the venue's rate"
            );
            self.note(format!(
                "PAPER FUNDING {} {}: {} at rate {} (settlement {})",
                event.venue, event.symbol, event.amount, event.funding_rate, event.timestamp
            ));
            self.dispatch(Ingest::Event(Event::Funding(event)));
        }
        if booked {
            self.write_snap(self.engine.now_ms);
        }
    }
}

/// Two settlement times closer than this are one settlement: a venue's history stamps a settlement a
/// few milliseconds off its round time, and settlements are an hour apart at the closest.
const SAME_SETTLEMENT_MS: i64 = 60_000;

/// What a restored `clock` missed before the venue's next settlement at `next_timestamp`, planned from the
/// venue's `stored` history `(ts, rate, mark)`: the dues (every stored point after the last booked
/// settlement, or from the one the clock was told of when it booked none) and, apart, the missed
/// settlements the store lacks (the module doc's catch-up), ascending.
fn plan_catch_up(
    clock: &vike_execution::FundingClock,
    next_ts: i64,
    stored: &[(i64, f64, Option<f64>)],
) -> (Vec<vike_execution::FundingDue>, Vec<i64>) {
    let (settled, held) = (clock.settled_timestamp, clock.next_timestamp);
    let near = |a: i64, b: i64| (a - b).abs() < SAME_SETTLEMENT_MS;
    let missed = |ts: i64| {
        ts < next_ts
            && if settled > 0 {
                ts > settled && !near(ts, settled)
            } else {
                ts > held - SAME_SETTLEMENT_MS
            }
    };
    let mut dues: Vec<vike_execution::FundingDue> = stored
        .iter()
        .filter(|&&(ts, rate, _)| missed(ts) && rate.is_finite())
        .map(|&(ts, rate, mark)| vike_execution::FundingDue {
            settled_timestamp: ts,
            rate,
            price: mark.filter(|p| p.is_finite() && *p > 0.0),
        })
        .collect();
    // One per settlement, a guard: a series written before the funding ingest's one-bar-per-hour rule can
    // hold a settlement twice.
    dues.dedup_by(|later, earlier| near(later.settled_timestamp, earlier.settled_timestamp));
    let mut missing: Vec<i64> = (missed(held)
        && !dues.iter().any(|d| near(d.settled_timestamp, held)))
    .then_some(held)
    .into_iter()
    .collect();
    // The settlement times known: the last booked, every due, the one the clock was told of, the venue's next.
    let mut known: Vec<i64> = (settled > 0)
        .then_some(settled)
        .into_iter()
        .chain(dues.iter().map(|d| d.settled_timestamp))
        .chain(missing.iter().copied())
        .chain([next_ts])
        .collect();
    known.sort_unstable();
    // The last booked settlement and the one the venue named next after it are adjacent by its word.
    let adjacent = usize::from(settled > 0 && known.get(1).is_some_and(|&t| near(t, held)));
    let spacing: Vec<i64> = known.windows(2).map(|w| w[1] - w[0]).collect();
    for (i, &gap) in spacing.iter().enumerate().skip(adjacent) {
        let before = if i > 0 { spacing[i - 1] } else { 0 };
        let step = before.max(spacing.get(i + 1).copied().unwrap_or(0));
        if step > 0 && gap * 2 > step * 3 {
            missing.extend(
                (1..).map(|k| known[i] + k * step).take_while(|&t| t < known[i + 1] - step / 2),
            );
        }
    }
    missing.sort_unstable();
    (dues, missing)
}
