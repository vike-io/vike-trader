//! One account engine, many symbols: a [`VenueRest`] that signs every order, cancel, modify and
//! status query on the ORDER's own symbol, with THAT symbol's price/qty grid. Owner ruling
//! 2026-10-03/04: *an account trades any symbol of its exchange, added when first traded*; the engine
//! is per account and the adapter signs on the order's symbol, never on the mounted one.
//!
//! * **The mounted symbol is the default.** An order whose symbol is the mounted one (any case) or is
//!   empty goes to the mounted client UNCHANGED, byte for byte; the factory is never asked.
//! * **Another symbol gets its own client**, built lazily by the venue's [`ClientFactory`] (which
//!   fetches that symbol's grid, off the core thread, on the exec thread) and cached for the life of
//!   the router. A factory failure is a terminal `OrderRejected` carrying its reason (submit), a
//!   `Err` (cancel) or a non-terminal `OrderModifyRejected` (modify): nothing is ever signed on a
//!   guessed grid or on the mounted symbol. The fetch is bounded by the transport's own timeout.
//! * **A cancel or a status query never waits for the grid where the venue supplies a light factory**
//!   ([`VenueRestPerSymbol::with_light_factory`]; bybit, okx and binance PERP do, binance spot and aster do
//!   not yet): they take the full client when one exists, else a client built with no network, so a
//!   foreign order's cancel after a restart survives a down grid endpoint. Modify still needs the grid.
//! * **Cancel, modify and status know their symbol from [`OrderClientIdSymbols`]**, filled at submit (and read
//!   from the order on modify). An order client id the router never saw (an order adopted by reconcile, or one
//!   from before a restart) falls back to the MOUNTED symbol: the venue answers "unknown order" there.
//!   Carrying the symbol on `ExecutionClient`/`ExecutionClientActorCommand`/`VenueRest` would close that; that seam
//!   change is declined (decision 0250), so the map stays.
//!
//! The map is shared (`Arc`) between the command loop's router and the confirm worker's router, so a
//! status query for a foreign order lands on the right symbol too. ONE copy for binance, aster and
//! okx, used as is, and for bybit inside its own `VenueRestPerSymbol`
//! (`crates/bridges/bybit/src/execution/rest/per_symbol.rs`), which adds bybit's account-mode check before a submit
//! and strikes a refused cancel from its cancel notes: those two are bybit's alone and stay there.
//! okx's contract sizes (`crates/bridges/okx/src/execution/rest/per_symbol.rs`'s `ContractSizes`) stay in okx. Each
//! venue keeps its OWN refusal log line ([`RefusalLog`]: its target and its words), handed in at
//! [`VenueRestPerSymbol::new`].
//!
//! [`OrderClientIdSymbols`] is also the table alpaca, oanda and vike-ibkr stamp a fill's symbol from, bounded at
//! each venue's own cap and with each venue's own line when it is full ([`OrderClientIdSymbols::with_cap`]).

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use vike_model::OrderRequest;
use vike_model::events::{Event, OrderModifyRejected};

use crate::confirm::ConfirmByOrderClientId;
use crate::rest::{VenueRest, refused};
use crate::transport::VenueApiError;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Builds the client for an order symbol other than the mounted one, or says why it cannot (the
/// reason is shown to the operator verbatim). Runs blocking REST (the grid fetch): exec thread only.
pub type ClientFactory<R> = Box<dyn Fn(&str) -> Result<R, String> + Send + Sync>;

/// The venue's own line for an order refused because no client could be built for its symbol
/// (`why` is the factory's reason). A plain `fn` so each venue's `tracing` target and words stay its
/// own: a log target is a compile-time constant of the callsite, so it cannot be a parameter here.
pub type RefusalLog = fn(&OrderRequest, &str);

/// Which symbol each order was submitted on, by order client id: the ONE such table of every bridge.
/// An empty order client id or symbol is never recorded.
///
/// * **The router's** ([`VenueRestPerSymbol`]; binance, aster, bybit, okx) records only the orders OFF the
///   mounted symbol, in a table with no cap (`OrderClientIdSymbols::default()`) that nothing empties (`record` says why).
///   It also keeps the FOREIGN symbols the command loop's router has been asked about since the
///   gap-sentinel last looked ([`Self::touch`], [`Self::take_touched`]): the sentinel replays the history
///   of those, and of the mounted symbol, instead of every symbol the engine ever admitted.
/// * **A bridge whose fills carry the symbol their ORDER was submitted under** (alpaca, oanda, vike-ibkr)
///   records every order in a table bounded at the venue's own cap ([`Self::with_cap`]), and each order's
///   entry leaves when it ends ([`Self::forget`]).
pub struct OrderClientIdSymbols {
    by_order_client_id: Mutex<HashMap<String, String>>,
    touched: Mutex<Vec<String>>,
    /// The most orders kept; `OrderClientIdSymbols::default()`'s is `usize::MAX`.
    cap: usize,
    /// The venue's own line for an order not recorded because the table is full: a plain `fn`, so its
    /// `tracing` target and words stay the venue's (as [`RefusalLog`]'s do).
    full_log: fn(&str, usize),
}

impl Default for OrderClientIdSymbols {
    /// The router's table: no cap, so nothing is ever refused and the log never runs.
    fn default() -> Self {
        Self::with_cap(usize::MAX, |_, _| {})
    }
}

impl OrderClientIdSymbols {
    /// A table that keeps at most `cap` orders. Past it a new order is not recorded and
    /// `full_log(order client id, cap)` says so; nothing is evicted to make room, because a live order's entry is
    /// the only thing that books its fill. Entries leave with their order ([`Self::forget`]), so the cap
    /// is a leak guard.
    pub fn with_cap(cap: usize, full_log: fn(&str, usize)) -> Self {
        Self { by_order_client_id: Mutex::default(), touched: Mutex::default(), cap, full_log }
    }

    /// Remember that `order_client_id` was submitted (or, for a restored order, taught) under `symbol`.
    pub fn record(&self, order_client_id: &str, symbol: &str) {
        if order_client_id.is_empty() || symbol.is_empty() {
            return;
        }
        let mut by_order_client_id = lock(&self.by_order_client_id);
        if by_order_client_id.len() >= self.cap && !by_order_client_id.contains_key(order_client_id)
        {
            (self.full_log)(order_client_id, self.cap);
            return;
        }
        // shortcut (the router's table): never evicted, one short entry per foreign order for the life
        // of the process. A CLEARED map once sent a live order's cancel to the mounted symbol, where the
        // venue's "unknown order" is swallowed as "already gone": a LIVE order reported canceled. Evict
        // only when the exec thread learns an order is terminal, if the growth ever matters.
        by_order_client_id.insert(order_client_id.to_string(), symbol.to_string());
    }

    /// The orders the core restored at boot (`ExecutionClient::adopt_restored`'s orders): each one
    /// that names a symbol is recorded, so its cancel goes to its own symbol and its fill carries it;
    /// an empty symbol is skipped, and the venue order id is not this table's. Never touches the
    /// network (the fold calls it).
    pub fn adopt(&self, orders: &[vike_execution::RestoredOrder]) {
        for o in orders {
            self.record(&o.order_client_id, &o.symbol);
        }
    }

    pub fn symbol_of(&self, order_client_id: &str) -> Option<String> {
        lock(&self.by_order_client_id).get(order_client_id).cloned()
    }

    /// The symbol a fill of `order_client_id` carries: the one the order was submitted under, else the venue's own
    /// spelling `venue_symbol` (an order nobody recorded).
    pub fn label(&self, order_client_id: &str, venue_symbol: &str) -> String {
        self.symbol_of(order_client_id).unwrap_or_else(|| venue_symbol.to_string())
    }

    /// Drop what is kept for `order_client_id`: its order ended.
    pub fn forget(&self, order_client_id: &str) {
        lock(&self.by_order_client_id).remove(order_client_id);
    }

    /// A command (submit, cancel, amend, status) named the foreign `symbol`: the next gap-sentinel reads it.
    /// Upper-cased, de-duplicated and bounded by the engine's own cap of extra symbols; an empty symbol is
    /// the mounted one and is never kept (the sentinel always reads that).
    pub fn touch(&self, symbol: &str) {
        let symbol = symbol.to_ascii_uppercase();
        let mut touched = lock(&self.touched);
        if !symbol.is_empty()
            && touched.len() < vike_execution::MAX_EXTRA_SYMBOLS
            && !touched.contains(&symbol)
        {
            touched.push(symbol);
        }
    }

    /// The foreign symbols touched since the last call, oldest first; the list starts again.
    pub fn take_touched(&self) -> Vec<String> {
        std::mem::take(&mut *lock(&self.touched))
    }
}

/// How long a failed client build (a grid fetch that found no row or could not be reached) is
/// remembered, so a burst of orders on a bad symbol costs ONE blocking fetch on the exec thread, not
/// one per order (and a cancel queued behind them does not wait for N timeouts). Shortcut: not
/// configurable and no early retry; a symbol the venue lists a moment later is tried again after this
/// long.
pub const NEGATIVE_TTL: Duration = Duration::from_secs(30);

struct Clients<R> {
    mounted: R,
    /// Upper-cased order symbol -> its client.
    others: HashMap<String, R>,
    /// Upper-cased order symbol -> when its build failed, and why ([`NEGATIVE_TTL`]).
    failed: HashMap<String, (Instant, String)>,
    /// Upper-cased order symbol -> its GRID-LESS client, built by `make_light` for cancel and status.
    light: HashMap<String, R>,
}

/// See the module doc.
pub struct VenueRestPerSymbol<R> {
    mounted_symbol: String,
    clients: Mutex<Clients<R>>,
    make: ClientFactory<R>,
    /// Builds a client that signs a cancel or a status query with NO network: those two never format a
    /// price or a size, so a venue whose grid fetch is down or slow must not hold them up. `None` =
    /// they take the full client like every other call (aster, and binance spot, today).
    make_light: Option<ClientFactory<R>>,
    order_client_ids: Arc<OrderClientIdSymbols>,
    refusal_log: RefusalLog,
}

impl<R> VenueRestPerSymbol<R> {
    /// `mounted_symbol` is the label orders on the mounted instrument carry (the core label, `.P`
    /// included on a binance-family perp lane; okx's `instId`). `refusal_log` is the venue's own
    /// line for a submit refused for want of a client ([`RefusalLog`]).
    pub fn new(
        mounted_symbol: impl Into<String>,
        mounted: R,
        make: ClientFactory<R>,
        order_client_ids: Arc<OrderClientIdSymbols>,
        refusal_log: RefusalLog,
    ) -> Self {
        Self {
            mounted_symbol: mounted_symbol.into(),
            clients: Mutex::new(Clients {
                mounted,
                others: HashMap::new(),
                failed: HashMap::new(),
                light: HashMap::new(),
            }),
            make,
            make_light: None,
            order_client_ids,
            refusal_log,
        }
    }

    /// Cancel and status of a foreign order use the client `make_light` builds (it must fetch nothing)
    /// unless the full one already exists. After a restart a foreign order's cancel then does not wait
    /// for, nor fail with, the venue's grid fetch (HALT's mass-cancel over many symbols would
    /// otherwise serialise behind one fetch each). Amend and submit still need the grid.
    #[must_use]
    pub fn with_light_factory(mut self, make_light: ClientFactory<R>) -> Self {
        self.make_light = Some(make_light);
        self
    }

    /// The label orders on the mounted instrument carry, as given to [`Self::new`].
    pub fn mounted_symbol(&self) -> &str {
        &self.mounted_symbol
    }

    /// [`Self::on`] for an amend of `order`: on its own symbol (a symbol not mounted is touched in the
    /// order client id table), or on its order client id's when it names none; a client that cannot be had answers the
    /// non-terminal `OrderModifyRejected` in its words.
    fn amend_on(&self, order: &OrderRequest, f: impl FnOnce(&R) -> Vec<Event>) -> Vec<Event> {
        let symbol = if order.symbol.is_empty() {
            self.symbol_of_order_client_id(&order.order_client_id)
        } else {
            if !self.is_mounted(&order.symbol) {
                self.order_client_ids.touch(&order.symbol);
            }
            order.symbol.clone()
        };
        self.on(&symbol, f).unwrap_or_else(|why| {
            vec![Event::OrderModifyRejected(OrderModifyRejected {
                order_client_id: order.order_client_id.clone(),
                reason: why.into(),
                timestamp: crate::rest::amend_answer_time(None),
            })]
        })
    }

    /// [`Self::on`] for the calls that never format a price or a size (cancel, status).
    fn on_light<X>(&self, symbol: &str, f: impl FnOnce(&R) -> X) -> Result<X, String> {
        let Some(make_light) = &self.make_light else { return self.on(symbol, f) };
        let mut clients = lock(&self.clients);
        if self.is_mounted(symbol) {
            return Ok(f(&clients.mounted));
        }
        let key = symbol.to_uppercase();
        let Clients { others, light, .. } = &mut *clients;
        if let Some(full) = others.get(&key) {
            return Ok(f(full));
        }
        match light.entry(key) {
            Entry::Occupied(e) => Ok(f(e.get())),
            Entry::Vacant(e) => Ok(f(e.insert(make_light(symbol)?))),
        }
    }

    /// Run `f` on the mounted client (the confirm closure's lazy server-time read, a mounted batch).
    pub fn with_mounted<X>(&self, f: impl FnOnce(&R) -> X) -> X {
        f(&lock(&self.clients).mounted)
    }

    /// The mounted symbol (any case) or an empty one.
    pub fn is_mounted(&self, symbol: &str) -> bool {
        symbol.is_empty() || symbol.eq_ignore_ascii_case(&self.mounted_symbol)
    }

    /// Run `f` on the client for `symbol`, building (and caching) it first when it is not mounted.
    /// `Err` is the factory's reason (remembered for [`NEGATIVE_TTL`]).
    pub fn on<X>(&self, symbol: &str, f: impl FnOnce(&R) -> X) -> Result<X, String> {
        let mut clients = lock(&self.clients);
        if self.is_mounted(symbol) {
            return Ok(f(&clients.mounted));
        }
        let key = symbol.to_uppercase();
        if let Some((at, why)) = clients.failed.get(&key)
            && at.elapsed() < NEGATIVE_TTL
        {
            return Err(why.clone());
        }
        let Clients { others, failed, .. } = &mut *clients;
        match others.entry(key.clone()) {
            Entry::Occupied(e) => Ok(f(e.get())),
            Entry::Vacant(e) => match (self.make)(symbol) {
                Ok(client) => {
                    failed.remove(&key);
                    Ok(f(e.insert(client)))
                }
                Err(why) => {
                    failed.insert(key, (Instant::now(), why.clone()));
                    Err(why)
                }
            },
        }
    }

    fn symbol_of_order_client_id(&self, order_client_id: &str) -> String {
        let symbol = self.order_client_ids.symbol_of(order_client_id).unwrap_or_default();
        self.order_client_ids.touch(&symbol);
        symbol
    }

    /// The submit's bookkeeping, before its client is asked: a FOREIGN order's order client id is recorded on
    /// its symbol and the symbol touched for the gap-sentinel; a mounted one records nothing.
    pub fn note_submit(&self, request: &OrderRequest) {
        if !self.is_mounted(&request.symbol) {
            self.order_client_ids.record(&request.order_client_id, &request.symbol);
            self.order_client_ids.touch(&request.symbol);
        }
    }

    /// Every order of the batch is on the mounted symbol (or names none).
    pub fn all_mounted(&self, requests: &[OrderRequest]) -> bool {
        requests.iter().all(|r| self.is_mounted(&r.symbol))
    }

    /// The mounted client's native batch cancel when no id is a foreign order; otherwise `cancel_one`
    /// per id, first error wins (the `VenueRest` default). `cancel_one` is the caller's own cancel, so
    /// a wrapper's per-cancel bookkeeping (bybit's cancel notes) runs for each id of the fan-out.
    pub fn cancel_batch_with(
        &self,
        order_client_ids: &[String],
        cancel_one: impl Fn(&str) -> Result<crate::rest::OrderCancelAck, VenueApiError>,
    ) -> Result<(), VenueApiError>
    where
        R: VenueRest,
    {
        if order_client_ids.iter().all(|c| self.order_client_ids.symbol_of(c).is_none()) {
            return lock(&self.clients).mounted.cancel_batch(order_client_ids);
        }
        let mut first_err = None;
        for c in order_client_ids {
            if let Err(e) = cancel_one(c) {
                first_err.get_or_insert(e);
            }
        }
        first_err.map_or(Ok(()), Err)
    }
}

impl<R: VenueRest> VenueRest for VenueRestPerSymbol<R> {
    fn submit_order(&self, request: &OrderRequest) -> Vec<Event> {
        self.note_submit(request);
        // A first order on a symbol fetches its grid here: what that spent counts, so the age is asked again.
        self.on(&request.symbol, |r| match crate::exec_actor::too_old_to_post(request) {
            Some(why) => refused(request, why),
            None => r.submit_order(request),
        })
        .unwrap_or_else(|why| {
            (self.refusal_log)(request, &why);
            refused(request, why)
        })
    }

    fn cancel_order(
        &self,
        order_client_id: &str,
    ) -> Result<crate::rest::OrderCancelAck, VenueApiError> {
        let symbol = self.symbol_of_order_client_id(order_client_id);
        self.on_light(&symbol, |r| r.cancel_order(order_client_id))
            .unwrap_or_else(|why| Err(VenueApiError { code: 0, msg: why }))
    }

    fn confirm_order(&self, order_client_id: &str, ts: i64) -> Vec<Event> {
        let symbol = self.symbol_of_order_client_id(order_client_id);
        self.on_light(&symbol, |r| r.confirm_order(order_client_id, ts)).unwrap_or_default()
    }

    fn modify_order(
        &self,
        order: &OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
    ) -> Vec<Event> {
        self.amend_on(order, |r| r.modify_order(order, new_qty, new_price))
    }

    fn modify_trigger(
        &self,
        order: &OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
        new_trigger_price: f64,
    ) -> Vec<Event> {
        self.amend_on(order, |r| r.modify_trigger(order, new_qty, new_price, new_trigger_price))
    }

    /// The mounted client's native batch when EVERY order is on the mounted symbol; otherwise the
    /// per-order fan-out, which routes each order to its own symbol.
    fn submit_batch(&self, requests: &[OrderRequest]) -> Vec<Event> {
        if self.all_mounted(requests) {
            return self.with_mounted(|m| m.submit_batch(requests));
        }
        requests.iter().flat_map(|r| self.submit_order(r)).collect()
    }

    /// The mounted client's native batch when no id is a foreign order; otherwise per id, first error
    /// wins (the `VenueRest` default).
    fn cancel_batch(&self, order_client_ids: &[String]) -> Result<(), VenueApiError> {
        self.cancel_batch_with(order_client_ids, |c| self.cancel_order(c))
    }
}

/// The active confirm asks about the order's OWN symbol (a foreign order client id) or the mounted one.
impl<R: ConfirmByOrderClientId> ConfirmByOrderClientId for VenueRestPerSymbol<R> {
    const LOOKUP_WINDOW: Option<Duration> = R::LOOKUP_WINDOW;

    fn confirm_when(&self, order_client_id: &str, ts: i64, absent_rejects: bool) -> Vec<Event> {
        let symbol = self.symbol_of_order_client_id(order_client_id);
        self.on_light(&symbol, |r| r.confirm_when(order_client_id, ts, absent_rejects))
            .unwrap_or_default()
    }
}

#[cfg(test)]
#[path = "per_symbol_tests.rs"]
mod tests;
