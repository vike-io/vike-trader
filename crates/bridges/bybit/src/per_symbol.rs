//! One account engine, many symbols: a [`VenueRest`] that signs every order, cancel, amend and
//! status query on the ORDER's own symbol, with THAT symbol's price/qty grid. Owner ruling
//! 2026-10-03/04: *an account trades any symbol of its exchange, added when first traded*; the engine
//! is per account and the adapter signs on the order's symbol, never on the mounted one.
//!
//! * **The mounted symbol is the default.** An order whose symbol is the mounted one (any case) or is
//!   empty goes to the mounted client UNCHANGED, byte for byte; the factory is never asked.
//! * **Another symbol gets its own client**, built lazily by the venue's [`ClientFactory`] (which
//!   fetches that symbol's `instruments-info` row, on the exec thread, never the core thread) and
//!   cached for the life of the router. A factory failure is a terminal `OrderRejected` carrying its
//!   reason (submit), an `Err` (cancel) or a non-terminal `OrderModifyRejected` (amend): nothing is
//!   ever signed on a guessed grid or on the mounted symbol. The fetch is bounded by the transport's
//!   own timeout.
//! * **A submit passes the account-mode check first** ([`PerSymbolRest::with_mode_check`], bybit's only
//!   departure from the binance copy: `crate::modes`): a symbol held in hedge position mode is put in
//!   one-way, or its order is refused in words. A cancel, amend or status query never asks.
//! * **A cancel or a status query never waits for the grid** ([`PerSymbolRest::with_light_factory`]):
//!   they take the full client when one exists, else a client built from fallbacks with no network, so
//!   a foreign order's cancel after a restart survives a down `instruments-info`. Amend still needs it.
//! * **Cancel, amend and status know their symbol from [`CoidSymbols`]**, filled at submit (and read
//!   from the order on amend). A coid the router never saw (an order adopted by reconcile, or one from
//!   before a restart) falls back to the MOUNTED symbol: the venue answers "unknown order" there.
//!   Carrying the symbol on `ExecutionClient`/`ExecCommand`/`VenueRest` would close that; it is a
//!   shared-home change (`crates/vike-bridge-core/CLAUDE.md`) and not taken here.
//!
//! The map is shared (`Arc`) between the command loop's router and the confirm worker's router, so a
//! status query for a foreign order lands on the right symbol too. This is a copy of
//! `crates/bridges/binance/src/per_symbol.rs` (bybit may not depend on a sibling venue): the rule is
//! the same (except the mode check above) and the pair should become ONE type in `vike-bridge-core` when
//! the shared seam moves.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use vike_bridge_core::confirm::ConfirmByCoid;
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::transport::VenueApiError;
use vike_model::OrderRequest;
use vike_model::events::{Event, OrderModifyRejected, OrderRejected, OrderSubmitted};

use crate::user_data::CancelNotes;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Builds the client for an order symbol other than the mounted one, or says why it cannot (the
/// reason is shown to the operator verbatim). Runs blocking REST (the grid fetch): exec thread only.
pub type ClientFactory<R> = Box<dyn Fn(&str) -> Result<R, String> + Send + Sync>;

/// Runs before the FIRST order of a symbol (`crate::modes::Modes::ensure`, which caches): `Ok` = the symbol is
/// in the modes the adapter orders in (or was put there), `Err(why)` = the order is refused in those words,
/// nothing signed. Blocking REST: exec thread only.
pub type ModeCheck = Box<dyn Fn(&str) -> Result<(), String> + Send + Sync>;

/// Which symbol each foreign order was submitted on, by client order id. Only orders OFF the mounted
/// symbol are recorded.
///
/// It also keeps the FOREIGN symbols the command loop's router has been asked about since the gap-sentinel
/// last looked ([`Self::touch`], [`Self::take_touched`]): the sentinel replays the history of those, and of
/// the mounted symbol, instead of every symbol the engine ever admitted.
#[derive(Default)]
pub struct CoidSymbols {
    by_coid: Mutex<HashMap<String, String>>,
    touched: Mutex<Vec<String>>,
}

impl CoidSymbols {
    pub fn record(&self, coid: &str, symbol: &str) {
        // shortcut: never evicted, one short entry per foreign order for the life of the process. A
        // CLEARED map once sent a live order's cancel to the mounted symbol, where the venue's
        // "unknown order" is swallowed as "already gone": a LIVE order reported canceled. Evict only
        // when the exec thread learns an order is terminal, if the growth ever matters.
        lock(&self.by_coid).insert(coid.to_string(), symbol.to_string());
    }

    pub fn symbol_of(&self, coid: &str) -> Option<String> {
        lock(&self.by_coid).get(coid).cloned()
    }

    /// A command (submit, cancel, amend, status) named the foreign `symbol`: the next gap-sentinel reads it.
    /// Upper-cased, de-duplicated and bounded by the engine's own cap of extra symbols; an empty symbol is
    /// the mounted one and is never kept (the sentinel always reads that).
    pub fn touch(&self, symbol: &str) {
        let symbol = symbol.to_ascii_uppercase();
        let mut touched = lock(&self.touched);
        if !symbol.is_empty()
            && touched.len() < vike_exec::MAX_EXTRA_SYMBOLS
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
/// one per order (and a cancel queued behind them does not wait for N timeouts).
const NEGATIVE_TTL: std::time::Duration = std::time::Duration::from_secs(30);

struct Clients<R> {
    mounted: R,
    /// Upper-cased order symbol -> its client.
    others: HashMap<String, R>,
    /// Upper-cased order symbol -> when its build failed, and why.
    failed: HashMap<String, (std::time::Instant, String)>,
    /// Upper-cased order symbol -> its GRID-LESS client, built by `make_light` for cancel and status.
    light: HashMap<String, R>,
}

/// See the module doc.
pub struct PerSymbolRest<R> {
    mounted_symbol: String,
    clients: Mutex<Clients<R>>,
    make: ClientFactory<R>,
    /// Builds a client that signs a cancel or a status query with NO network: those two never format a
    /// price or a size, so a venue whose grid fetch is down or slow must not hold them up. `None` =
    /// they take the full client like every other call.
    make_light: Option<ClientFactory<R>>,
    coids: Arc<CoidSymbols>,
    /// The cancels the client noted ([`CancelNotes`]); a cancel the venue REFUSES is struck from it.
    /// `None` = nobody notes cancels (the confirm worker's router, tests).
    notes: Option<Arc<CancelNotes>>,
    /// The account-mode rule every SUBMIT passes first ([`ModeCheck`]). `None` = none (the confirm worker's
    /// router, tests).
    modes: Option<ModeCheck>,
}

impl<R> PerSymbolRest<R> {
    /// `mounted_symbol` is the label orders on the mounted instrument carry.
    pub fn new(
        mounted_symbol: impl Into<String>,
        mounted: R,
        make: ClientFactory<R>,
        coids: Arc<CoidSymbols>,
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
            coids,
            notes: None,
            modes: None,
        }
    }

    /// Every submit (single or batch) asks `check` for its symbol first: an `Err` is a terminal
    /// `OrderRejected` in its words and the order is not signed. For a symbol other than the mounted one
    /// the check runs AFTER its grid is fetched, so an unknown symbol is still refused for its grid.
    #[must_use]
    pub fn with_mode_check(mut self, check: ModeCheck) -> Self {
        self.modes = Some(check);
        self
    }

    /// The mode check for `symbol` (empty = the mounted one).
    fn modes_ok(&self, symbol: &str) -> Result<(), String> {
        let Some(check) = &self.modes else { return Ok(()) };
        check(if symbol.is_empty() { &self.mounted_symbol } else { symbol })
    }

    /// A cancel this router sends and the venue REFUSES leaves the order live, so its note is struck
    /// from `notes` (a later `Deactivated` of that order is then the venue's own, an expiry).
    #[must_use]
    pub fn with_cancel_notes(mut self, notes: Arc<CancelNotes>) -> Self {
        self.notes = Some(notes);
        self
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

    fn is_mounted(&self, symbol: &str) -> bool {
        symbol.is_empty() || symbol.eq_ignore_ascii_case(&self.mounted_symbol)
    }

    /// Run `f` on the client for `symbol`, building (and caching) it first when it is not mounted.
    fn on<X>(&self, symbol: &str, f: impl FnOnce(&R) -> X) -> Result<X, String> {
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
                    failed.insert(key, (std::time::Instant::now(), why.clone()));
                    Err(why)
                }
            },
        }
    }

    fn symbol_of_coid(&self, coid: &str) -> String {
        let symbol = self.coids.symbol_of(coid).unwrap_or_default();
        self.coids.touch(&symbol);
        symbol
    }
}

/// `[OrderSubmitted, OrderRejected(why)]`: a refused order is never silent.
fn refused(request: &OrderRequest, why: String) -> Vec<Event> {
    vec![
        Event::OrderSubmitted(OrderSubmitted {
            client_order_id: request.client_order_id.clone(),
            ts: request.ts,
        }),
        Event::OrderRejected(OrderRejected {
            cause: None,
            client_order_id: request.client_order_id.clone(),
            reason: why.into(),
            ts: request.ts,
        }),
    ]
}

impl<R: VenueRest> VenueRest for PerSymbolRest<R> {
    fn submit_order(&self, request: &OrderRequest) -> Vec<Event> {
        if !self.is_mounted(&request.symbol) {
            self.coids.record(&request.client_order_id, &request.symbol);
            self.coids.touch(&request.symbol);
        }
        self.on(&request.symbol, |r| match self.modes_ok(&request.symbol) {
            Ok(()) => r.submit_order(request),
            Err(why) => {
                tracing::warn!(target: "vike_bybit::per_symbol", symbol = %request.symbol, %why, "order refused: the symbol's account mode is not one this adapter orders in");
                refused(request, why)
            }
        })
        .unwrap_or_else(|why| {
            tracing::warn!(target: "vike_bybit::per_symbol", symbol = %request.symbol, %why, "order refused: no client for its symbol");
            refused(request, why)
        })
    }

    fn cancel_order(&self, client_order_id: &str) -> Result<(), VenueApiError> {
        let symbol = self.symbol_of_coid(client_order_id);
        let answer = self
            .on_light(&symbol, |r| r.cancel_order(client_order_id))
            .unwrap_or_else(|why| Err(VenueApiError { code: 0, msg: why }));
        if let (Err(_), Some(notes)) = (&answer, &self.notes) {
            notes.forget(client_order_id);
        }
        answer
    }

    fn confirm_order(&self, client_order_id: &str, ts: i64) -> Vec<Event> {
        let symbol = self.symbol_of_coid(client_order_id);
        self.on_light(&symbol, |r| r.confirm_order(client_order_id, ts)).unwrap_or_default()
    }

    fn modify_order(
        &self,
        order: &OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
    ) -> Vec<Event> {
        let symbol = if order.symbol.is_empty() {
            self.symbol_of_coid(&order.client_order_id)
        } else {
            if !self.is_mounted(&order.symbol) {
                self.coids.touch(&order.symbol);
            }
            order.symbol.clone()
        };
        self.on(&symbol, |r| r.modify_order(order, new_qty, new_price)).unwrap_or_else(|why| {
            vec![Event::OrderModifyRejected(OrderModifyRejected {
                client_order_id: order.client_order_id.clone(),
                reason: why.into(),
                ts: 0,
            })]
        })
    }

    /// The mounted client's native batch when EVERY order is on the mounted symbol; otherwise the
    /// per-order fan-out, which routes each order to its own symbol.
    fn submit_batch(&self, requests: &[OrderRequest]) -> Vec<Event> {
        // A mounted batch the mode check refuses falls through to the per-order fan-out, which words
        // each order's refusal (the check caches, so it asks the venue once).
        if requests.iter().all(|r| self.is_mounted(&r.symbol)) && self.modes_ok("").is_ok() {
            return lock(&self.clients).mounted.submit_batch(requests);
        }
        requests.iter().flat_map(|r| self.submit_order(r)).collect()
    }

    /// The mounted client's native batch when no id is a foreign order; otherwise per id, first error
    /// wins (the `VenueRest` default).
    fn cancel_batch(&self, client_order_ids: &[String]) -> Result<(), VenueApiError> {
        if client_order_ids.iter().all(|c| self.coids.symbol_of(c).is_none()) {
            return lock(&self.clients).mounted.cancel_batch(client_order_ids);
        }
        let mut first_err = None;
        for c in client_order_ids {
            if let Err(e) = self.cancel_order(c) {
                first_err.get_or_insert(e);
            }
        }
        first_err.map_or(Ok(()), Err)
    }
}

impl<R: ConfirmByCoid> ConfirmByCoid for PerSymbolRest<R> {
    const LOOKUP_WINDOW: Option<std::time::Duration> = R::LOOKUP_WINDOW;

    fn confirm_when(&self, coid: &str, ts: i64, absent_rejects: bool) -> Vec<Event> {
        let symbol = self.symbol_of_coid(coid);
        self.on_light(&symbol, |r| r.confirm_when(coid, ts, absent_rejects)).unwrap_or_default()
    }
}
