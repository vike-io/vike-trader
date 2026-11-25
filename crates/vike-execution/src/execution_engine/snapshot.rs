//! The journal `Snapshot` round trip: `journal_snapshot` captures the OMS as the `Snapshot` carries it,
//! `snapshot_state` captures all of it, `from_snapshot` rebuilds either.
//!
//! **The `Snapshot` is bounded by what is LIVE, not by history** (`journal_snapshot`). The process keeps
//! every order and every trade id it ever folded, as it always did, so nothing it reads or publishes
//! changes; the `Snapshot`, which the core rewrites WHOLE every 1024 records, keeps:
//!
//! - every order that has not ENDED, whatever its age (working, resting, pending cancel, an unknown
//!   fate: `!OrderStatus::is_terminal`), and an ended one only while it was created within
//!   [`ENDED_ORDER_KEEP_MS`] of the registry's newest order. A restart puts no journaled order in a
//!   registry (decision 0120), so what an ended order still owes a restart is the crash tail's refold
//!   (a late venue word about it, an unadmit's `recently-traded`) and the late-retry memory of its
//!   id, which the core carries in the `Snapshot`'s `recent_order_client_ids` (`vike_core`'s `write_snap`).
//! - the trade ids of the engine's window of days of fold (its node's reconcile read-back plus a margin,
//!   [`Engine::size_fill_window`]), the FSM's as its difference from the account's, and the floor below them
//!   (`crate::account::fill_window`).
//! - the newest [`JOURNAL_CLOSED_PNLS`] closed-PnL rows: nothing reads `Account::closed_pnls` after a
//!   restart (the closed trades ride their own archive), and a reader of the journal reads the newest.
//!
//! Each rule is a function of folded state alone, so a replay core keeps what the live core kept and
//! `state_hash` fences the same bytes.

use crate::account::fill_window;
use crate::risk::RiskGate;

use super::{Engine, ExecutionClient};

/// How long after the NEWEST order of the registry was created an ENDED order still rides the journal
/// `Snapshot` ([`Engine::journal_snapshot`]). The engine's own figure for the time a venue's late word
/// about an order that ended in the same instant needs to arrive (`UNADMIT_QUIET_MS`, which an unadmit reads
/// off the registry too): a crash-tail refold then finds the order a late fill or cancel names, and a replayed
/// unadmit finds the order that refused it `recently-traded`, as the live run did.
pub const ENDED_ORDER_KEEP_MS: i64 = super::UNADMIT_QUIET_MS;

/// How many of the newest closed-PnL rows (`Account::closed_pnls`) the journal `Snapshot` keeps. Nothing in a
/// restarted process reads the rows (the closed trades ride their own archive); a reader of the journal itself
/// (a replay's check of what each close booked, an incident's triage) reads the newest ones. About 20 KB at most.
pub const JOURNAL_CLOSED_PNLS: usize = 1024;

/// The fold days a snapshot writes: none for the full form, and none when every id folded on the day of the
/// engine's own clock (what a restore assumes for a snapshot without them), else `days` as they are.
fn days_worth_writing(bounded: bool, days: Vec<u32>, now_ms: i64) -> Vec<u32> {
    let today = fill_window::day_of(now_ms);
    if bounded && days.iter().any(|&d| d != today) { days } else { Vec::new() }
}

impl<C: ExecutionClient> Engine<C> {
    /// Capture the full OMS state (every order and trade id, no fold days). Dedup sets are SORTED
    /// (canonical). The journal `Snapshot` writes [`Self::journal_snapshot`] instead.
    pub fn snapshot_state(&self) -> crate::engine_snapshot::EngineSnapshot {
        self.snapshot_with(false)
    }

    /// The OMS state as the journal `Snapshot` carries it: bounded by what is LIVE (this module's doc). A
    /// snapshot of an engine rebuilt from one is the same bytes again.
    pub fn journal_snapshot(&self) -> crate::engine_snapshot::EngineSnapshot {
        self.snapshot_with(true)
    }

    /// The earliest `created_ms` an ENDED order may carry and still ride [`Self::journal_snapshot`]:
    /// [`ENDED_ORDER_KEEP_MS`] before the newest order of the registry. `None` while no order states one.
    fn ended_order_floor(&self) -> Option<i64> {
        self.registry.values().filter_map(|o| o.created_ms).max().map(|n| n - ENDED_ORDER_KEEP_MS)
    }

    /// Whether [`Self::journal_snapshot`] keeps `order`, given [`Self::ended_order_floor`].
    fn journal_keeps(order: &vike_model::OrderRecord, floor: Option<i64>) -> bool {
        !order.status.is_terminal()
            || order.created_ms.zip(floor).is_some_and(|(created, floor)| created >= floor)
    }

    /// The ENDED orders [`Self::journal_snapshot`] leaves out, `(order client id, order)`: the core carries their ids
    /// in the `Snapshot`'s late-retry memory. Cold: once per `Snapshot`.
    pub fn journal_left_out(&self) -> impl Iterator<Item = (&str, &vike_model::OrderRecord)> {
        let floor = self.ended_order_floor();
        self.registry
            .iter()
            .filter(move |(_, o)| !Self::journal_keeps(o, floor))
            .map(|(order_client_id, o)| (order_client_id.as_str(), o))
    }

    fn snapshot_with(&self, bounded: bool) -> crate::engine_snapshot::EngineSnapshot {
        let floor = bounded.then(|| self.ended_order_floor());
        let registry: Vec<(String, vike_model::OrderRecord)> = self
            .registry
            .iter()
            .filter(|(_, o)| floor.is_none_or(|f| Self::journal_keeps(o, f)))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let left_out = (self.registry.len() - registry.len()) as u64;
        let window = bounded.then_some(self.fill_window_days);
        let (ledger, fill_floor_ms) = self.account.fill_ledger(window);
        let fsm = fill_window::windowed(
            self.seen_fsm_trade_ids.iter().map(|(id, &day)| (id, day)),
            window,
        );
        // The FSM set as its difference from the account's; by day only where the days are written (bounded).
        let (fsm_only, account_only) =
            fill_window::difference(&ledger.entries, &fsm.entries, bounded);
        let (seen_trade_ids, seen_trade_days) = fill_window::written(&ledger.entries);
        let (fsm_only_trade_ids, fsm_only_trade_days) = fill_window::written(&fsm_only);
        let account =
            self.account.snapshot_with(if bounded { JOURNAL_CLOSED_PNLS } else { usize::MAX });
        let mut liquidation: Vec<String> = self.seen_liquidation_ids.iter().cloned().collect();
        liquidation.sort_unstable();
        crate::engine_snapshot::EngineSnapshot {
            venue: self.venue.clone(),
            symbol: self.symbol.clone(),
            quote_asset: self.quote_asset.clone(),
            reduce_only_on_close: self.reduce_only_on_close,
            extra_symbols: self.extra_symbols.clone(),
            trading_state: self.trading_state,
            limits: self.gate.limits.clone(),
            gate_order_times: self.gate.throttle_times(),
            registry,
            account,
            seen_trade_ids,
            fsm_only_trade_ids,
            account_only_trade_ids: account_only.into_iter().map(str::to_string).collect(),
            seen_liquidation_ids: liquidation,
            dropped_terminal_on_live: self.closing_events_dropped_order_still_live,
            equity_seed: self.equity_seed,
            collect_applied_fills: self.collect_applied_fills,
            now_ms: self.now_ms,
            // `None` for the venue's sole/default account, so its bytes (and `state_hash`) are
            // a v16 snapshot's; see the field.
            route_key: (self.route_key != self.venue).then(|| self.route_key.clone()),
            // Only the case `quote_asset` cannot say (see the field): told "USDT", the guess's own
            // spelling. Every other engine writes nothing here and hashes as it always did.
            account_currency_set: self.account_currency_set
                && self.quote_asset == super::DEFAULT_QUOTE_ASSET,
            // WHOSE book this is. Only a STATEMENT is written: a tier that is not `Paper`, or one the
            // mount told (`set_tier`), so the seed of `new` (and of a rebuild from a tier-less
            // snapshot) says nothing and the canonical JSON of such an engine is what it always was.
            tier: self.stated_tier(),
            // A paper engine's funding clocks (empty, so absent from the bytes, on every other engine).
            funding_clocks: self.funding_clocks.clone(),
            native_time_release: self.native_time_release,
            // The id shape's inputs, written only when told (an engine nobody told writes the bytes it always did).
            account_code: (self.account_code
                != vike_model::orders::client_id::OrderClientIdAccountCode::NONE)
                .then_some(self.account_code),
            broker_code_len: (self.broker_code_len > 0).then_some(self.broker_code_len),
            // The fold days are the journal form's bookkeeping, and only when they say something: an engine
            // rebuilt from a snapshot without them holds every id at the day of that snapshot's own clock,
            // so the full form, and a ledger folded all on that day, write the bytes they always did.
            seen_trade_days: days_worth_writing(bounded, seen_trade_days, self.now_ms),
            fsm_only_trade_days: days_worth_writing(bounded, fsm_only_trade_days, self.now_ms),
            // Written only when it says something: an engine nobody sized writes the bytes it always did.
            fill_window_days: (self.fill_window_days != fill_window::WIDEST_WINDOW_DAYS)
                .then_some(self.fill_window_days),
            fill_floor_ms,
            ended_orders_left_out: self.ended_orders_left_out + left_out,
        }
    }

    /// Seed the two trade-id sets and the ledger's floor from `snap`, each id at its fold day (a snapshot
    /// written without the days restores every id at the day of its own clock): the account's as written, the
    /// FSM's as the account's less [`crate::EngineSnapshot::account_only_trade_ids`], plus its own entries (one the
    /// account holds at another day takes its own).
    fn seed_fill_sets(&mut self, snap: &crate::engine_snapshot::EngineSnapshot) {
        use fill_window::{day_of, restored};
        let day = day_of(snap.now_ms);
        let account: Vec<(String, u32)> =
            restored(&snap.seen_trade_ids, &snap.seen_trade_days, day).collect();
        let lacks: std::collections::HashSet<&str> =
            snap.account_only_trade_ids.iter().map(String::as_str).collect();
        self.seen_fsm_trade_ids = account
            .iter()
            .filter(|(id, _)| !lacks.contains(id.as_str()))
            .cloned()
            .chain(restored(&snap.fsm_only_trade_ids, &snap.fsm_only_trade_days, day))
            .collect();
        self.account.seed_fill_ledger(account, snap.fill_floor_ms);
    }

    /// Rebuild an engine from a snapshot (restart/replay). `applied_fills` starts empty
    /// (deliveries are not replayed). The tier is the snapshot's when it carries one
    /// ([`crate::EngineSnapshot::tier`]), so a replay core re-snapshots exactly what it was given;
    /// a snapshot without one (written before the field) seeds `Paper` as [`Self::new`] does and
    /// stays UNSTATED, so it is written back without one.
    pub fn from_snapshot(snap: &crate::engine_snapshot::EngineSnapshot, client: C) -> Self {
        let mut gate = RiskGate::new(snap.limits.clone());
        gate.set_throttle_times(snap.gate_order_times.clone());
        let mut e = Engine::new(
            crate::Account::restore(&snap.account),
            gate,
            client,
            &snap.venue,
            &snap.symbol,
        );
        e.quote_asset = snap.quote_asset.clone();
        e.account_currency_set = snap.account_currency_set;
        e.refresh_ledger_currency();
        if let Some(tier) = snap.tier {
            e.set_tier(tier);
        }
        e.native_time_release = snap.native_time_release;
        e.account_code = snap
            .account_code
            .unwrap_or(vike_model::orders::client_id::OrderClientIdAccountCode::NONE);
        e.broker_code_len = snap.broker_code_len.unwrap_or(0);
        e.reduce_only_on_close = snap.reduce_only_on_close;
        e.extra_symbols = snap.extra_symbols.clone();
        e.trading_state = snap.trading_state;
        e.registry = snap.registry.iter().cloned().collect();
        // ...and how many ended orders the snapshot left out of it, so the next one counts them too.
        e.ended_orders_left_out = snap.ended_orders_left_out;
        // ...and the window the live engine cut its trade-id sets to, so this one cuts them alike.
        e.fill_window_days = snap.fill_window_days.unwrap_or(fill_window::WIDEST_WINDOW_DAYS);
        e.seed_fill_sets(snap);
        e.seen_liquidation_ids = snap.seen_liquidation_ids.iter().cloned().collect();
        e.closing_events_dropped_order_still_live = snap.dropped_terminal_on_live;
        e.equity_seed = snap.equity_seed;
        e.collect_applied_fills = snap.collect_applied_fills;
        e.now_ms = snap.now_ms;
        e.funding_clocks = super::funding::restored_clocks(&snap.funding_clocks);
        // A stored rate stamped in the future is not believed (`Account::clamp_future_rates`).
        e.account.clamp_future_rates(snap.now_ms);
        // A labelled account's key (`None` leaves `new`'s `route_key == venue`).
        if let Some(route_key) = &snap.route_key {
            e.route_key = route_key.clone();
        }
        // Re-seed the read-side `price_board` from the restored `Account.marks`. `PriceBoard` sits
        // OUTSIDE `EngineSnapshot` (the journal hash-fence), so a bare restore leaves it EMPTY and
        // `resolved_position_price` would return `Missing` for a symbol whose restored mark is
        // present, until the next tick. Pairs the two stores as `apply_snapshot` does, stamped with
        // the restored core clock; the board's dead-price guard drops a non-positive mark. Inert to
        // the determinism fence (the board is not serialized, marks are outside `state_hash`).
        for ((venue, symbol), mark) in &snap.account.marks {
            e.price_board.set_mark(venue, symbol, *mark, e.now_ms);
        }
        e
    }

    /// Put a journaled engine's FOLDED state back onto THIS engine, which its mount already built
    /// (client, gate limits, route key, mode and every config-derived field stay this engine's) —
    /// the boot-restore half of a `Snapshot` (`vike_core::replay::restore_for_boot`, applied once at
    /// the core's assembly, never on the fold).
    ///
    /// Written, all in one call so they can never disagree: the account's folded money
    /// ([`crate::Account::restore_folded`]: positions, realized PnL, closed PnLs, balances and an
    /// `Authoritative` balance mode, funding, fees), the THREE fill-dedup sets (the account's
    /// bare-fill ledger with its fold days and its floor, the FSM's and the liquidation lane's — a
    /// position restored without its dedup would book a fill the venue re-delivers on connect a second
    /// time, and a dedup restored without its position would lose it), the gate's rate-window
    /// timestamps and `closing_events_dropped_order_still_live`.
    ///
    /// ⚠ It does not look at [`crate::EngineSnapshot::tier`]: whether the snapshot is THIS account's
    /// book is the caller's question, asked before this call (`vike_core`'s `apply_restored_engines`
    /// refuses a snapshot whose stated tier is not this engine's).
    ///
    /// NOT written: the RISK LIMITS (settings and the run profile are their authority, decisions
    /// 0086 and 0111; a journaled ceiling is not a ceiling), `trading_state` (the caller applies it
    /// only when it is stricter), the route key, the tier, the native time release, the client, `reduce_only_on_close`,
    /// the DECLARED `extra_symbols`, `quote_asset`, `equity_seed`, the trade-id window (configuration: a read-back
    /// raised since is safe, the restored floor still refuses every re-delivery), the marks (market data) and
    /// the order registry (a journaled order is a CANDIDATE the venue's first reconcile pass
    /// confirms, decision 0120 — never a registry entry at boot).
    pub fn restore_runtime_state(&mut self, snap: &crate::engine_snapshot::EngineSnapshot) {
        self.account.restore_folded(&snap.account);
        self.seed_fill_sets(snap);
        self.seen_liquidation_ids = snap.seen_liquidation_ids.iter().cloned().collect();
        self.gate.set_throttle_times(snap.gate_order_times.clone());
        self.closing_events_dropped_order_still_live = snap.dropped_terminal_on_live;
        // A paper engine's funding clocks: a settlement the venue rolled past while the node was down
        // is booked on the restored position at the first tick, and one already booked never again.
        self.funding_clocks = super::funding::restored_clocks(&snap.funding_clocks);
        self.restore_admitted_symbols(snap);
        // The rates it brought back are as old as the snapshot says, and a stamp the snapshot's own
        // clock had not reached is not believed (`Account::clamp_future_rates`).
        self.account.clamp_future_rates(snap.now_ms);
    }

    /// Put back the symbols `Command::AdmitSymbol` admitted before the restart (decision 0173): their
    /// `extra_symbols` entry, their `on_demand` grid row, their contract value, their margin rate and their
    /// margin mode. Unlike a declared
    /// leg these are RUNTIME state (an operator traded the symbol), so a restart that kept the
    /// positions and dropped the symbol would hold a position the engine does not know it trades. The
    /// node re-admits each one with fresh facts right after the boot (`vike_tradehub::readmit`); this
    /// is what keeps the engine whole until that lands, and when the lookup fails. A declared leg's
    /// row is never overwritten, the cap holds, and nothing here is journaled (it IS the journal).
    fn restore_admitted_symbols(&mut self, snap: &crate::engine_snapshot::EngineSnapshot) {
        // The MOUNTED symbol's margin row, which an admission may have written (a spot mount on a
        // perp's leverage; `admit_symbol`'s mounted arm) and which the loop below never reaches: it
        // has no `on_demand` grid row. `from_snapshot` brings it back with the whole limits; without
        // this a boot restore margined a 1x spot mount at the engine-wide perp rate until the node's
        // re-admission landed, UNDER-stating the margin it needs. A row this mount already holds is
        // the mount's and wins, as it does for the admitted symbols below.
        if let Some(initial_margin) = snap.limits.initial_margin_by_symbol.get(&self.symbol) {
            self.gate
                .limits
                .initial_margin_by_symbol
                .entry(self.symbol.clone())
                .or_insert(*initial_margin);
        }
        // ...and the MOUNTED symbol's conversion rate, when the mount declared it foreign
        // (`Self::declare_mount_quote_currency`: a ledger that is not in the mount's money). The loop below
        // never reaches the mount, so without this a restored position on it is valued with no rate at
        // all until the node's first admission lands. The rate is as old as the snapshot (stale by the
        // window, so opening stays denied; the node renews it right after the boot), never invented:
        // a snapshot that holds none, or lists the mount under another currency, restores none.
        if let Some(fxs) = snap.account.fx.as_ref() {
            self.account.restore_declared_rate(fxs, &self.symbol);
        }
        for (symbol, row) in &snap.limits.grid_by_symbol {
            if !row.on_demand || *symbol == self.symbol {
                continue;
            }
            if !self.accepts_symbol(symbol) && self.extra_symbols.len() >= crate::MAX_EXTRA_SYMBOLS
            {
                continue;
            }
            let value = snap
                .account
                .inverse
                .iter()
                .find(|(s, _)| s == symbol)
                .map(|(_, v)| vike_model::ContractValue::inverse(*v))
                .or_else(|| {
                    snap.account
                        .mult
                        .iter()
                        .find(|(s, _)| s == symbol)
                        .map(|(_, v)| vike_model::ContractValue::linear(*v))
                });
            // A symbol quoted in a foreign currency comes back through the fx book: the snapshot
            // holds its BASE value and its currency's rate, and the account rebuilds the effective
            // value from them (the node then re-admits it with a fresh rate; until that lands the
            // restored rate is as old as the snapshot, so an opening order is refused and a reduce
            // is not). One the snapshot holds no rate for (a hand-edited snapshot only) is NOT
            // accepted at all: its fills are dropped as an unadmitted symbol's are, because folding
            // them at the symbol's own unit is the par valuation a restore must never produce.
            let foreign = match snap.account.fx.as_ref() {
                None => false,
                Some(fxs) => {
                    let base = value.unwrap_or_else(|| self.account.contract_value_default());
                    match self.account.restore_fx_symbol(fxs, symbol, base) {
                        crate::account::FxRestore::NotForeign => false,
                        crate::account::FxRestore::Restored => true,
                        crate::account::FxRestore::Unusable => continue,
                    }
                }
            };
            if !self.accepts_symbol(symbol) {
                self.extra_symbols.push(symbol.clone());
            }
            if self.gate.limits.grid_by_symbol.get(symbol).is_none_or(|g| g.on_demand) {
                self.gate.limits.grid_by_symbol.insert(symbol.clone(), row.clone());
                // ...and the margin rate the admission stated for it (its `initial_margin_by_symbol` row), which
                // is runtime state like the grid row and is guarded like it: a restart that dropped
                // it would margin a 1x spot symbol at the engine-wide perp rate until the node's
                // re-admission landed, and a row the mount already holds is the mount's.
                if let Some(initial_margin) = snap.limits.initial_margin_by_symbol.get(symbol) {
                    self.gate
                        .limits
                        .initial_margin_by_symbol
                        .entry(symbol.clone())
                        .or_insert(*initial_margin);
                }
                // ...and the margin MODE it stated (decision 0218): the next position opened from flat
                // on an isolated-only coin must be born isolated even if the re-admission fails.
                if let Some((_, mode)) =
                    snap.account.admitted_margin_modes.iter().find(|(s, _)| s == symbol)
                {
                    self.account.admit_margin_mode(symbol, Some(*mode));
                }
            }
            if !foreign && let Some(value) = value {
                self.account.admit_contract_value(symbol, value);
            }
        }
    }
}

#[cfg(test)]
#[path = "snapshot_tests.rs"]
mod snapshot_tests;
