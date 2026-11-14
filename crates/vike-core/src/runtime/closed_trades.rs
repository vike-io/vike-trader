//! A live mount's CLOSED TRADES (decision 0271 item 8): the rows `vike_model::OrderBroker::closed_trades` answers on a
//! paper, demo or live mount, booked by the simulator's own ledger
//! (`crates/vike-analytics/src/closed_trades.rs`'s `TradeClosedLedger`) with the simulator's own call
//! (`crates/vike-simulator/src/engine/sim_broker/fills.rs`'s `fold_fill`: `apply(&fill, "", bar, value, 1.0)`), so a
//! backtest and a mount cannot disagree about what a closed trade is.
//!
//! # What is booked, and when
//!
//! Every fill DELIVERED to a mount (`CoreThread::dispatch_applied_fills`: a fill of an order that mount minted),
//! BEFORE its `on_fill` runs, so a fill's rows are in the ledger when the strategy hears that fill. The fill is
//! the one the strategy hears: its fee in the money the contract settles in
//! (`vike_execution::Account::commission_in_settle`), as the simulator states its fee. The value is the one the
//! account books the fill's P&L with (`vike_execution::Account::contract_value_of`); on an account that holds a
//! foreign-quoted symbol it is what the account folded THIS fill with, and the fee the commission it booked
//! (`AppliedFill::fx`, both in the account's currency), the pair the mount's own ledger takes. On an inverse
//! contract the rows are in the settle coin, as the backtest's are. A fee correction is not booked (a row keeps
//! the fee its fill came with).
//!
//! **A LIQUIDATION of a position a mount holds is booked too** ([`CoreThread::book_liquidation`]): the venue's own
//! (`Event::PositionLiquidated`, which folds into the account with no fill a mount hears) and a close the account-wide
//! margin call released (`CoreThread::liquidation_order_client_ids`, an order no mount minted). Both close the position the
//! strategy reads through `OrderBroker::position`, so a ledger that left them out would hold lots the account no longer
//! holds, and the next opposite fill would book a close of them where the backtest, which books its own liquidations
//! (the simulator's ledger takes every fill of the run), opens a trade. It is booked into the mounts whose ledger holds
//! lots of the liquidated symbol on the liquidated engine, oldest mount first, each up to what it holds, at the
//! liquidation price, with its share of the liquidation fee.
//!
//! **Not booked: an operator's ticket and a reconcile's synthesized fill.** Neither is the strategy's trade or a forced
//! close of it: a ticket is the operator's own order (it lands in the residual row, decision 0116), and a reconcile fill
//! states what the venue did while the core was not listening, which the backtest of the strategy never saw. A mount's
//! ledger records the trades its own strategy and the market's forced closes made, as a backtest's does.
//!
//! # The bar index
//!
//! The simulator's rule, the index of the bar the fill falls in, over the mount's own series as the core holds
//! it (`CoreThread::bars` under the mount's `(venue, symbol, interval)`). That series holds CLOSED bars only and
//! a paper fill folds before its bar joins it, so a fill after the last closed bar's close falls in the bar in
//! progress: its index is the series' length, the one that bar takes when it closes ([`bar_of`]). The series
//! is never trimmed (a close appends, a re-close replaces the last bar), but a SEED replaces it whole (the
//! feed's warm-up and the datahub's start history at a start, a venue that seeds again on a resubscribe): a
//! row keeps the index it was booked at, so after a seed of another length its indices no longer count the
//! series the strategy reads, while its times stay right.
//!
//! # A restart
//!
//! **Every closed row is written ONCE to the closed-trade ARCHIVE beside the journal**
//! (`crates/vike-journal/src/closed_archive.rs`'s `TradeClosedArchive`, the journal's own checksummed frames, in its
//! directory): the core only enqueues a row as its booking closes it ([`CoreThread::archive_closed`], one channel
//! send, no disk work on the fold), and the archive's own thread writes and syncs it. **The ledger's STATE rides every
//! `Snapshot`**: `Snapshot.closed_trades` ([`CoreThread::closed_trades_for_snap`]) carries, per live mount, how far the
//! archive holds its rows ON DISK ("archived through #N"), the few rows after that (the ones the thread had not synced
//! yet), its open lots exactly (each lot's entry commission per unit and contract value with it) and its BAR CLOCK
//! (the newest closed bar of its own series, its index, one bar's length). So a `Snapshot`'s size does not grow with the
//! trades a mount closed. A clean stop waits [`ARCHIVE_EXIT_WAIT`] for the thread before its exit `Snapshot`.
//!
//! A restart reads the archive ONCE (`crate::replay`'s boot restore: a torn or duplicate frame dropped by its check
//! and its number), puts its rows `0..N` in front of the `Snapshot`'s own, and hands the list on (`CoreConfig::closed_trades`).
//! The fills after the `Snapshot` are journaled already, as the events the engine folds, so the boot restore hands their
//! list on too (`crate::replay::TradeClosedTail`, onto `CoreConfig::closed_tail`, only when the refold folded that tail into
//! the engines), and the core resumes each ledger (`TradeClosedLedger::resumed`) and books every fill and liquidation
//! of the tail on it through the same call, before any mount comes up ([`CoreThread::book_closed_tail`]); the mount
//! then hands the archive again every row from `N` on, whose LAST copy is the one a later read keeps. A fill the
//! engine folded cannot be missing from its mount's ledger: there is no second record to lose, and a row the archive
//! lost to a crash mid-append is in the `Snapshot` or rebuilt from the tail. A tail fill's bar index comes from its own time, counted from the bar
//! clock over the periods the mount's market traded in ([`clock_bar`], the `vike_catalog::trading_calendar` of its kept row): a
//! closed session inside the tail (a crash's tail; a clean stop writes a `Snapshot` and leaves none) counts no bar, as
//! the series the venue stores holds none. A holiday the calendar does not know, or a bar the venue lost, still
//! counts one.
//!
//! A mount the `Snapshot` holds no ledger for (a journal written before the key, or a mount mounted after the `Snapshot`)
//! holds what its restored ledger (`MountAttribution`: the ownership file's, else the journal's) says it holds: one
//! lot per open row, at the row's average price, opened when the mount came up, with no fee (it was charged before the
//! restart). Without it, a close after the restart would open the opposite side and book the next entry as its close.
//! The next `Snapshot` carries those lots.
//!
//! **No format step**: a build from before the key ignores it and restores the position as one lot.
//!
//! **Every closed row is kept**, for the mount's life, because `OrderBroker::closed_trades` is the whole list, as a
//! backtest's is, and a shortened list read as complete would be a wrong answer: in memory, and in the archive,
//! which grows by one frame of about 250 bytes per close (`crates/vike-journal/src/record_tests.rs`'s
//! `a_closed_row_is_about_250_bytes`) plus a copy per row a restart writes again, and is never compacted. An archive
//! that cannot be opened or written is an ERROR, and its rows then ride every `Snapshot` as they did before it.
//!
//! # The cost
//!
//! Per delivered fill: one `apply`, a binary search of the mount's bars, and a length compare (the archive); per row it
//! closes, one copy of the row and one channel send. Per strategy dispatch: one `Option<Arc>` copy (`None` until the
//! mount's first fill, a refcount bump after), nothing per tick beyond that and no allocation per message. The ledger
//! grows by a row per close for the mount's life, as a backtest's does for its run; a `Snapshot` writes only its unsynced
//! rows and lots (snapshot cadence, never the fold); an unmounted slot drops it. Per venue event on a core with a
//! mount: one discriminant test, for the liquidation.

use std::collections::VecDeque;

use vike_analytics::closed_trades::{OpenLot, TradeClosedLedger};
use vike_journal::{JournalSnapshotBarClock, JournalSnapshotClosedTrades, JournalSnapshotOpenLot};

use super::*;
use crate::replay::TradeClosedTail;

/// **How long a clean stop waits for the closed-trade archive's thread before its exit `Snapshot`**: 2 s. Long enough for
/// the thread to sync what a stop leaves queued (a batch is one write and one `sync_data`, milliseconds even on a slow
/// disk), so the exit `Snapshot` carries no row and a restart reads them from the archive; short enough that a stuck disk
/// cannot hold a stop. Past it nothing is lost: the rows not synced ride the exit `Snapshot`.
pub(crate) const ARCHIVE_EXIT_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// What the closed-trade ARCHIVE holds of one mount's rows (`vike_journal::TradeClosedArchive`): rows `0..next` were
/// handed to it, and the sequence numbers of the ones it may not have synced yet, oldest first.
#[derive(Debug, Clone, Default)]
pub(crate) struct TradeClosedMountArchive {
    next: u64,
    unsynced: VecDeque<u64>,
}

impl TradeClosedMountArchive {
    /// A ledger whose rows `0..archived` the archive holds on disk already.
    fn from(archived: u64) -> Self {
        TradeClosedMountArchive { next: archived, unsynced: VecDeque::new() }
    }

    /// How many of the rows are on disk, the archive's thread having synced every sequence number up to `durable`.
    fn archived(&self, durable: u64) -> u64 {
        let synced = self.unsynced.partition_point(|&s| s <= durable);
        self.next - (self.unsynced.len() - synced) as u64
    }
}

/// The index of the bar a fill at `ts` falls in, over `closed` (a mount's closed bars, oldest first, each
/// `bar_ms` long, `0` for an interval with no length): the simulator's `partition_point(|b| b.ts <= ts)` less
/// one, except that a fill after the last bar's close (`Bar::closed_at`) falls in the bar in progress, which
/// takes the next index. `0` over an empty series, as a backtest with no bars books.
pub(super) fn bar_of(closed: &[Bar], bar_ms: i64, ts: i64) -> usize {
    let seen = closed.partition_point(|b| b.timestamp <= ts);
    let in_progress = seen == closed.len()
        && bar_ms > 0
        && closed.last().is_some_and(|last| last.closed_at(bar_ms, ts));
    if in_progress { seen } else { seen.saturating_sub(1) }
}

/// The index of the bar a fill at `ts` falls in, counted from a `Snapshot`'s bar clock over the periods `calendar`'s
/// market traded in (`runtime/bar_grid.rs`'s `bar_at`): what [`bar_of`] counts over the series the venue stores,
/// whose newest closed bar is the clock's. `0` with no clock (a mount that had no bar yet).
pub(super) fn clock_bar(
    clock: Option<JournalSnapshotBarClock>,
    calendar: Option<&vike_venues::SessionCalendar>,
    ts: i64,
) -> usize {
    let Some(c) = clock.filter(|c| c.bar_ms > 0) else { return 0 };
    super::bar_grid::bar_at((c.timestamp, c.index), c.bar_ms, calendar, ts)
}

/// The bar clock of `closed`, a mount's series of bars `bar_ms` long: its newest bar and that bar's index.
fn clock_of(closed: &[Bar], bar_ms: i64) -> Option<JournalSnapshotBarClock> {
    let last = closed.last()?;
    Some(JournalSnapshotBarClock { timestamp: last.timestamp, index: closed.len() - 1, bar_ms })
}

/// A mount's closed-trade ledger as a `Snapshot` carries it ([`JournalSnapshotClosedTrades`]): the rows after the `archived` the
/// archive holds on disk, and its lots exactly.
fn snap_of(
    mount_id: &str,
    ledger: Option<&TradeClosedLedger>,
    clock: Option<JournalSnapshotBarClock>,
    archived: u64,
) -> JournalSnapshotClosedTrades {
    let all = ledger.map_or(&[][..], TradeClosedLedger::rows);
    let rows = all[usize::try_from(archived).unwrap_or(usize::MAX).min(all.len())..].to_vec();
    let lots =
        ledger.map_or(&[][..], TradeClosedLedger::lots).iter().map(|l| JournalSnapshotOpenLot {
            symbol: l.symbol.clone(),
            tag: l.tag.clone(),
            side: l.side,
            entry_timestamp: l.entry_timestamp,
            entry_bar: l.entry_bar,
            price: l.price,
            qty: l.qty,
            fee_per_unit: l.entry_fee_per_unit(),
            value: l.value(),
        });
    JournalSnapshotClosedTrades {
        mount_id: mount_id.to_string(),
        archived,
        rows,
        lots: lots.collect(),
        bar_clock: clock,
    }
}

/// A journal's closed-trade ledger for a mount id no slot has taken yet ([`CoreThread::pending_closed`]): the
/// ledger resumed from the archive and the `Snapshot`, the journal's tail booked on it, the bar clock the tail counted
/// from, and how many of its rows the archive holds on disk.
#[derive(Debug, Clone)]
pub(crate) struct TradeClosedPendingLedger {
    ledger: TradeClosedLedger,
    clock: Option<JournalSnapshotBarClock>,
    archived: u64,
}

impl TradeClosedPendingLedger {
    /// The ledger `snap` holds (the boot restore's copy: the archive's rows in front of the `Snapshot`'s), resumed
    /// (`TradeClosedLedger::resumed`).
    pub(crate) fn of(snap: &JournalSnapshotClosedTrades) -> Self {
        let lots = snap.lots.iter().map(|l| {
            let (symbol, tag) = (l.symbol.clone(), l.tag.clone());
            let at = (l.entry_timestamp, l.entry_bar);
            OpenLot::resumed(symbol, tag, l.side, at, l.price, l.qty, l.fee_per_unit, l.value)
        });
        TradeClosedPendingLedger {
            ledger: TradeClosedLedger::resumed(snap.rows.clone(), lots.collect()),
            clock: snap.bar_clock,
            archived: snap.archived,
        }
    }
}

/// A LIQUIDATION as the mounts' ledgers book it ([`CoreThread::book_liquidation`]): which instrument, which side
/// of it was closed (`None`: whichever side a ledger holds, a one-way position), how much (`0.0`: all of it),
/// at what price, the fee of the whole in the money the contract settles in (as a delivered fill states its
/// fee), and when. `Copy`, so the venue lane carries it across the fold with no allocation.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TradeClosedLiquidation {
    pub(crate) venue: ustr::Ustr,
    pub(crate) symbol: ustr::Ustr,
    pub(crate) held: Option<i32>,
    pub(crate) qty: f64,
    pub(crate) price: f64,
    pub(crate) fee: f64,
    pub(crate) timestamp: i64,
}

impl TradeClosedLiquidation {
    /// What `lots` hold of the liquidated symbol, on the side it closed: `(side, quantity)`, `None` for nothing.
    fn held_in(&self, lots: &[OpenLot]) -> Option<(i32, f64)> {
        let symbol = self.symbol.as_str();
        let side = lots
            .iter()
            .filter(|l| l.symbol == symbol)
            .map(|l| l.side)
            .find(|s| self.held.is_none_or(|h| h == *s))?;
        let on_side = lots.iter().filter(|l| l.symbol == symbol && l.side == side);
        Some((side, vike_model::py_sum(on_side.map(|l| l.qty))))
    }

    /// The closing fill of `qty` of what a holder holds on `side`, with its share of the fee of `whole`.
    fn fill(&self, side: i32, qty: f64, whole: f64) -> Fill {
        Fill {
            side: -side,
            size: qty,
            price: self.price,
            fee: self.fee * qty / whole,
            timestamp: self.timestamp,
            is_maker: false,
            symbol: self.symbol.to_string(),
            tag: None,
        }
    }
}

/// The share each of `holders` (`(holder, side, quantity)`, in order) takes of `liquidation`: `(holder, side, quantity)`
/// of every one reached, up to what is left of the liquidated quantity (all of it for `0.0`), and the whole the
/// fee is shared over.
fn shares<K>(
    liquidation: &TradeClosedLiquidation,
    holders: Vec<(K, i32, f64)>,
) -> (Vec<(K, i32, f64)>, f64) {
    let whole = if liquidation.qty > 0.0 {
        liquidation.qty
    } else {
        vike_model::py_sum(holders.iter().map(|h| h.2))
    };
    let mut left = whole;
    let mut out = Vec::new();
    for (k, side, held) in holders {
        let qty = left.min(held);
        if qty <= 1e-12 {
            break;
        }
        left -= qty;
        out.push((k, side, qty));
    }
    (out, whole)
}

impl<C: ExecutionClient> CoreThread<C> {
    /// Book `fill`, as mount `index` is about to hear it, into that mount's closed-trade ledger (the module
    /// doc): `value` is the one its strategy is handed, `fx` what an account holding a foreign-quoted symbol
    /// folded the fill with (the effective value and the booked commission), `closed` the mount's series and
    /// `interval` its bar length. Per delivered fill, never per message.
    pub(crate) fn book_closed_trade(
        &mut self,
        index: usize,
        fill: &Fill,
        fx: Option<(vike_model::ContractValue, f64)>,
        value: vike_model::ContractValue,
        (closed, interval): (&[Bar], &str),
    ) {
        let bar_ms = vike_marketdata::interval::interval_ms(interval).unwrap_or(0);
        let bar = bar_of(closed, bar_ms, fill.timestamp);
        let ledger = Arc::make_mut(self.mount_closed[index].get_or_insert_with(Arc::default));
        match fx {
            None => {
                ledger.apply(fill, "", bar, value, 1.0);
            }
            Some((value, charge)) => {
                ledger.apply(&Fill { fee: charge, ..fill.clone() }, "", bar, value, 1.0);
            }
        }
        self.archive_closed(index);
    }

    /// **Hand the closed-trade archive every row of mount `i` it was not handed yet** (the module doc's "A restart"):
    /// one copy and one channel send per row, nothing on a booking that closed none (a length compare), nothing on a
    /// core with no archive. The archive's thread writes them; no disk work here.
    pub(crate) fn archive_closed(&mut self, i: usize) {
        let (Some(archive), Some(ledger)) =
            (self.closed_archive.as_mut(), self.mount_closed[i].as_deref())
        else {
            return;
        };
        let a = &mut self.mount_archive[i];
        let rows = ledger.rows();
        let from = usize::try_from(a.next).unwrap_or(usize::MAX);
        if from >= rows.len() {
            return;
        }
        let durable = archive.durable();
        while a.unsynced.front().is_some_and(|&s| s <= durable) {
            a.unsynced.pop_front();
        }
        for r in &rows[from..] {
            a.unsynced.push_back(archive.enqueue(&self.mount_ids[i], a.next, r.clone()));
            a.next += 1;
        }
    }

    /// Mount `i`'s closed-trade ledger as it comes up at `at` (both doors, after the mount's own ledger was
    /// restored): the journal's for its id (`CoreThread::pending_closed`, the tail already booked on it), else
    /// one lot per open row of its restored ledger at the row's average price, with no fee (the module doc's "A
    /// restart"). Nothing for a tombstone.
    pub(crate) fn restore_closed_trades(&mut self, i: usize, at: i64) {
        self.mount_closed[i] = None;
        self.mount_closed_clock[i] = None;
        self.mount_archive[i] = TradeClosedMountArchive::default();
        if self.mounts[i].is_none() {
            return;
        }
        if let Some(p) = self.pending_closed.remove(&self.mount_ids[i]) {
            let holds = !p.ledger.rows().is_empty() || !p.ledger.lots().is_empty();
            self.mount_closed[i] = holds.then(|| Arc::new(p.ledger));
            self.mount_closed_clock[i] = p.clock;
            // The rows the archive does not hold on disk (the `Snapshot`'s own, and the tail's) go to it now.
            self.mount_archive[i] = TradeClosedMountArchive::from(p.archived);
            self.archive_closed(i);
            return;
        }
        let Some(m) = self.mounts[i].as_ref() else { return };
        let attr = &self.mount_attribution[i];
        let own =
            std::iter::once((m.venue.as_str(), m.symbol.as_str(), attr.size, attr.average_price));
        let legs = attr
            .legs
            .iter()
            .map(|l| (l.venue.as_str(), l.symbol.as_str(), l.attr.size, l.attr.average_price));
        let closed = self.bars.get(&(m.venue.clone(), m.symbol.clone(), m.interval.clone()));
        let closed = closed.map_or(&[][..], |s| s.closed.as_slice());
        let bar =
            bar_of(closed, vike_marketdata::interval::interval_ms(&m.interval).unwrap_or(0), at);
        let mut ledger: Option<TradeClosedLedger> = None;
        for (venue, symbol, size, avg_px) in own.chain(legs).filter(|r| r.2.abs() > 1e-12) {
            let fill = Fill {
                side: if size > 0.0 { 1 } else { -1 },
                size: size.abs(),
                price: avg_px,
                fee: 0.0,
                timestamp: at,
                is_maker: false,
                symbol: symbol.to_string(),
                tag: None,
            };
            let engine_index = self.route_of(EngineRoute::Mount(i), venue).unwrap_or(0);
            let value = self.engine(engine_index).account.contract_value_of(symbol);
            ledger.get_or_insert_default().apply(&fill, "", bar, value, 1.0);
        }
        self.mount_closed[i] = ledger.map(Arc::new);
    }

    /// Every closed-trade ledger a `Snapshot` carries (`CoreThread::write_snap`): one per LIVE mount, empty or not, with
    /// its bar clock (so a restart knows the ledger is the `Snapshot`'s and books the tail on it; a series with no bar
    /// yet keeps the clock the ledger was resumed with) and only the rows the archive does not hold on disk, then the
    /// journal's ledgers still waiting for their mount, by id. Snapshot cadence, O(lots + unsynced rows).
    pub(crate) fn closed_trades_for_snap(&self) -> Vec<JournalSnapshotClosedTrades> {
        let durable =
            self.closed_archive.as_ref().map_or(0, vike_journal::TradeClosedArchive::durable);
        let live = (0..self.mounts.len()).filter_map(|i| {
            let m = self.mounts[i].as_ref()?;
            let bar_ms = vike_marketdata::interval::interval_ms(&m.interval).unwrap_or(0);
            let key = (m.venue.clone(), m.symbol.clone(), m.interval.clone());
            let clock = self.bars.get(&key).and_then(|s| clock_of(&s.closed, bar_ms));
            let clock = clock.or(self.mount_closed_clock.get(i).copied().flatten());
            let archived = self.mount_archive[i].archived(durable);
            Some(snap_of(&self.mount_ids[i], self.mount_closed[i].as_deref(), clock, archived))
        });
        let pending = self
            .pending_closed
            .iter()
            .map(|(id, p)| snap_of(id, Some(&p.ledger), p.clock, p.archived));
        live.chain(pending).collect()
    }

    /// **Book the journal's TAIL on the ledgers the `Snapshot` held** (the module doc's "A restart"), at assembly, before
    /// any mount takes its ledger: each fill of a mount's order through the call that booked it live (the value the
    /// strategy is handed and the fee in the settle money, `CoreThread::dispatch_applied_fills`'s), at the bar the
    /// ledger's clock counts by the mount's calendar (`Self::tail_bar`), and each liquidation as
    /// [`Self::book_liquidation`] books one. A fill's
    /// mount is the one this core's ownership table names for its order, else the one the journal names. A ledger the
    /// `Snapshot` did not hold is not booked on: a mount without one holds its restored position, which already holds the
    /// tail. Cold: once, at assembly.
    pub(crate) fn book_closed_tail(&mut self, tail: Vec<TradeClosedTail>) {
        for item in tail {
            match item {
                TradeClosedTail::Fill { fill: f, owner, margin_close } => {
                    let order_client_id = f.order_client_id.as_str();
                    let slot = self.order_client_id_mount.get(order_client_id).copied();
                    let id = slot.map(|i| self.mount_ids[i].clone()).or(owner);
                    let engine_index =
                        match slot.or_else(|| id.as_ref().and_then(|id| self.slot_of(id))) {
                            Some(i) => self.route_of(EngineRoute::Mount(i), &f.venue),
                            None => {
                                self.engine_index_for_route_key(RouteKey::sole_account_of(&f.venue))
                            }
                        };
                    let engine_index = engine_index.unwrap_or(0);
                    let value = self.engine(engine_index).account.base_value_of(&f.symbol);
                    let fee = vike_execution::Account::commission_in_settle(value, &f);
                    if margin_close {
                        let liquidation = TradeClosedLiquidation {
                            venue: f.venue,
                            symbol: f.symbol,
                            held: Some(-f.side),
                            qty: f.last_qty,
                            price: f.last_price,
                            fee,
                            timestamp: f.timestamp,
                        };
                        self.book_tail_liquidation(engine_index, liquidation);
                        continue;
                    }
                    let Some(id) = id.filter(|id| self.pending_closed.contains_key(id)) else {
                        continue;
                    };
                    let bar =
                        self.tail_bar(&id, (f.venue.as_str(), f.symbol.as_str()), f.timestamp);
                    let fill = Fill {
                        side: f.side,
                        size: f.last_qty,
                        price: f.last_price,
                        fee,
                        timestamp: f.timestamp,
                        is_maker: f.liquidity_side.is_maker(),
                        symbol: f.symbol.to_string(),
                        tag: None,
                    };
                    let p = self.pending_closed.get_mut(&id).expect("checked above");
                    p.ledger.apply(&fill, "", bar, value, 1.0);
                }
                TradeClosedTail::Liquidated(event) => {
                    let engine_index = match event.route_key {
                        Some(key) => {
                            self.engine_index_for_route_key(RouteKey::declared(key.as_str()))
                        }
                        None => {
                            self.engine_index_for_route_key(RouteKey::sole_account_of(&event.venue))
                        }
                    };
                    if let Some(engine_index) = engine_index {
                        let liquidation = self.liquidation_of(engine_index, &event);
                        self.book_tail_liquidation(engine_index, liquidation);
                    }
                }
            }
        }
    }

    /// The live slot that carries mount id `id`.
    fn slot_of(&self, id: &str) -> Option<usize> {
        (0..self.mounts.len()).find(|&i| self.mounts[i].is_some() && self.mount_ids[i] == id)
    }

    /// The bar a tail fill at `ts` falls in on pending ledger `id`, by its bar clock ([`clock_bar`]) and the
    /// trading calendar of the mount's own market (`vike_catalog::trading_calendar` of the class its KEPT row states,
    /// `CoreConfig::session_rows`: its slot's market, else the fill's `market`, the mount's own for a one-symbol
    /// mount), so a market's closed hours inside the tail count no bar.
    fn tail_bar(&self, id: &str, market: (&str, &str), ts: i64) -> usize {
        let calendar_of = |venue: &str, symbol: &str| {
            let row = vike_catalog::kept_row(&self.config.session_rows, venue, symbol);
            vike_catalog::trading_calendar(venue, row)
        };
        let calendar = match self.slot_of(id).and_then(|i| self.mounts[i].as_ref()) {
            Some(m) => calendar_of(&m.venue, &m.symbol),
            None => calendar_of(market.0, market.1),
        };
        clock_bar(self.pending_closed.get(id).and_then(|p| p.clock), calendar.as_ref(), ts)
    }

    /// A liquidation of the journal's tail booked on the pending ledgers that hold it on engine `engine_index` (a slot's own
    /// route, else the venue's sole account), slots first in mount order, then the rest by id.
    fn book_tail_liquidation(&mut self, engine_index: usize, liquidation: TradeClosedLiquidation) {
        let mut ids: Vec<String> = (0..self.mounts.len())
            .filter(|&i| self.mounts[i].is_some())
            .map(|i| self.mount_ids[i].clone())
            .filter(|id| self.pending_closed.contains_key(id))
            .collect();
        let rest: Vec<String> =
            self.pending_closed.keys().filter(|id| !ids.contains(id)).cloned().collect();
        ids.extend(rest);
        let mut holders = Vec::new();
        for id in ids {
            let engine = match self.slot_of(&id) {
                Some(i) => self.route_of(EngineRoute::Mount(i), &liquidation.venue),
                None => {
                    self.engine_index_for_route_key(RouteKey::sole_account_of(&liquidation.venue))
                }
            };
            if engine != Some(engine_index) {
                continue;
            }
            if let Some((side, held)) = liquidation.held_in(self.pending_closed[&id].ledger.lots())
            {
                holders.push((id, side, held));
            }
        }
        let value = self.engine(engine_index).account.contract_value_of(&liquidation.symbol);
        let (reached, whole) = shares(&liquidation, holders);
        for (id, side, qty) in reached {
            let bar = self.tail_bar(
                &id,
                (liquidation.venue.as_str(), liquidation.symbol.as_str()),
                liquidation.timestamp,
            );
            let p = self.pending_closed.get_mut(&id).expect("a holder is pending");
            p.ledger.apply(&liquidation.fill(side, qty, whole), "", bar, value, 1.0);
        }
    }

    /// **Book a LIQUIDATION into the mounts that hold it** (the module doc's "What is booked"): every live mount
    /// whose own route for `liquidation.venue` is engine `engine_index` and whose ledger holds lots of `liquidation.symbol` (on the side
    /// closed, when the liquidation names one) is handed a closing fill of what it holds, up to what is left of
    /// `liquidation.qty` (all of it for `0.0`), oldest mount first, at the liquidation price and its share of the fee.
    /// Cold: a liquidation, never per message.
    #[cold]
    #[inline(never)]
    pub(crate) fn book_liquidation(
        &mut self,
        engine_index: usize,
        liquidation: TradeClosedLiquidation,
    ) {
        let mut holders: Vec<(usize, i32, f64)> = Vec::new();
        for i in 0..self.mounts.len() {
            if self.mounts[i].is_none()
                || self.route_of(EngineRoute::Mount(i), &liquidation.venue) != Some(engine_index)
            {
                continue;
            }
            let Some(ledger) = self.mount_closed[i].as_deref() else { continue };
            if let Some((side, held)) = liquidation.held_in(ledger.lots()) {
                holders.push((i, side, held));
            }
        }
        let (reached, whole) = shares(&liquidation, holders);
        for (i, side, qty) in reached {
            let fill = liquidation.fill(side, qty, whole);
            let value = self.engine(engine_index).account.contract_value_of(&liquidation.symbol);
            let key = {
                let m = self.mounts[i].as_ref().expect("a holder is mounted");
                (m.venue.clone(), m.symbol.clone(), m.interval.clone())
            };
            let closed = self.bars.get(&key).map(|s| Arc::clone(&s.closed)).unwrap_or_default();
            self.book_closed_trade(i, &fill, None, value, (closed.as_slice(), &key.2));
        }
    }

    /// A fill `af` of an order on engine `engine_index` that no mount minted: when it is a close the account-wide margin
    /// call released (`CoreThread::liquidation_order_client_ids`), the LIQUIDATION of what the mounts hold there
    /// ([`Self::book_liquidation`]), its value and fee as the account folded them. Its order client id is forgotten once the
    /// order `completed`. Cold: a fill no mount minted.
    #[cold]
    #[inline(never)]
    pub(crate) fn book_margin_call_fill(
        &mut self,
        af: &vike_execution::execution_engine::AppliedFill,
        engine_index: usize,
        completed: bool,
    ) {
        let f = &af.fill;
        let order_client_id = f.order_client_id.as_str();
        let found = if completed {
            self.liquidation_order_client_ids.remove(order_client_id)
        } else {
            self.liquidation_order_client_ids.contains(order_client_id)
        };
        if !found {
            return;
        }
        let fee = match af.fx {
            Some(fx) => fx.charge,
            None => {
                let value = self.engine(engine_index).account.contract_value_of(&f.symbol);
                vike_execution::Account::commission_in_settle(value, f)
            }
        };
        let liquidation = TradeClosedLiquidation {
            venue: f.venue,
            symbol: f.symbol,
            held: Some(-f.side),
            qty: f.last_qty,
            price: f.last_price,
            fee,
            timestamp: f.timestamp,
        };
        self.book_liquidation(engine_index, liquidation);
    }

    /// The venue's own liquidation `p`, folded by engine `engine_index`, as the mounts' ledgers book it: its fee in the
    /// money the contract settles in (the event states it in the account's). Cold.
    #[cold]
    #[inline(never)]
    pub(crate) fn liquidation_of(
        &self,
        engine_index: usize,
        p: &vike_model::events::PositionLiquidated,
    ) -> TradeClosedLiquidation {
        use vike_model::events::PositionSide;
        let held = match p.position_side {
            PositionSide::Long => Some(1),
            PositionSide::Short => Some(-1),
            PositionSide::Both => None,
        };
        let value = self.engine(engine_index).account.contract_value_of(&p.symbol);
        TradeClosedLiquidation {
            venue: p.venue,
            symbol: p.symbol,
            held,
            qty: p.qty.abs(),
            price: p.liquidation_price,
            fee: value.in_settle(p.fee, p.liquidation_price),
            timestamp: p.timestamp,
        }
    }
}
