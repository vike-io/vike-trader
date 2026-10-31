//! One account engine, many instruments: a [`VenueRest`] that signs every order, cancel, amend and
//! status query on the ORDER's own `instId`, with THAT instrument's tick/lot grid and `ctVal`. Owner
//! ruling 2026-10-03/04: *an account trades any symbol of its exchange, added when first traded*; the
//! engine is per account and the adapter signs on the order's instrument, never on the mounted one.
//!
//! OKX sizes in CONTRACTS, so the instrument is part of the quantity: `sz` is the base qty divided by
//! THAT instrument's `ctVal`. A foreign order therefore needs its own client (own `ctVal`, own grid,
//! own order-rate window: the venue meters 60 per 2 s per instrument), and its FILLS need the same
//! `ctVal` back ([`ContractSizes`], read by [`crate::event_mapper::map_okx_perp_by_inst`]).
//!
//! * **The mounted instrument is the default.** An order whose symbol is the mounted one (any case) or
//!   is empty goes to the mounted client UNCHANGED, byte for byte; the factory is never asked.
//! * **Another instrument gets its own client**, built lazily by the venue's [`ClientFactory`] (which
//!   fetches that instrument's `public/instruments` SWAP row on the exec thread, never the core
//!   thread) and cached for the life of the router. A factory failure (unreachable, unknown, not a
//!   SWAP, an inverse swap) is a terminal `OrderRejected` carrying its reason (submit), an `Err`
//!   (cancel) or a non-terminal `OrderModifyRejected` (amend): nothing is ever signed on a guessed
//!   grid, on the mounted instrument, or with another instrument's `ctVal`. The failure is remembered
//!   for [`NEGATIVE_TTL`] (also the `ctVal` load of a fill: [`ContractSizes::get_or_load`]), so a
//!   burst on a bad instrument costs one blocking fetch.
//! * **A cancel or a status query never waits for the grid** ([`PerSymbolRest::with_light_factory`]):
//!   they take the full client when one exists, else a client built from fallbacks with no network, so
//!   a foreign order's cancel after a restart survives a down `public/instruments`. Amend still needs it.
//! * **Cancel, amend and status know their instrument from [`CoidSymbols`]**, filled at submit (and
//!   read from the order on amend). A coid the router never saw (an order adopted by reconcile, or one
//!   from before a restart) falls back to the MOUNTED instrument. Carrying the symbol on
//!   `ExecutionClient`/`ExecCommand`/`VenueRest` would close that; it is a shared-home change
//!   (`crates/vike-bridge-core/CLAUDE.md`) and not taken here.
//!
//! The coid map is shared (`Arc`) between the command loop's router and the confirm worker's client,
//! so a status query for a foreign order lands on the right instrument too. This is a copy of
//! `crates/bridges/bybit/src/per_symbol.rs` (okx may not depend on a sibling venue): the rule is the
//! same and the types should become ONE in `vike-bridge-core` when the shared seam moves.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use vike_bridge_core::confirm::ConfirmByCoid;
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::transport::VenueApiError;
use vike_model::OrderRequest;
use vike_model::events::{Event, OrderModifyRejected, OrderRejected, OrderSubmitted};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How long an instrument whose row could not be read (a client build or a `ctVal` load that failed)
/// is remembered as unreadable, so a burst of orders (or of fills) on a bad instrument costs ONE
/// blocking fetch, not one each: the fetch runs on the exec thread (a cancel queued behind it waits)
/// or on the user-data pump. Same figure as binance's. Shortcut: not configurable and no early
/// retry; an instrument the venue lists a moment later is tried again after this long.
const NEGATIVE_TTL: Duration = Duration::from_secs(30);

/// Builds the client for an order symbol other than the mounted one, or says why it cannot (the
/// reason is shown to the operator verbatim). Runs blocking REST (the instrument fetch): exec thread
/// only.
pub type ClientFactory<R> = Box<dyn Fn(&str) -> Result<R, String> + Send + Sync>;

/// Which instrument each foreign order was submitted on, by client order id. Only orders OFF the
/// mounted instrument are recorded.
///
/// It also keeps the FOREIGN instruments the command loop's router has been asked about since the
/// gap-sentinel last looked ([`Self::touch`], [`Self::take_touched`]): the sentinel replays the history
/// of those, and of the mounted instrument, instead of every instrument the engine ever admitted.
#[derive(Default)]
pub struct CoidSymbols {
    by_coid: Mutex<HashMap<String, String>>,
    touched: Mutex<Vec<String>>,
}

impl CoidSymbols {
    pub fn record(&self, coid: &str, symbol: &str) {
        // shortcut: never evicted. A cleared or capped map would send an old foreign order's cancel to
        // the MOUNTED instrument, where okx answers 51401/51402 and the cancel (after its retry on the
        // algo endpoint, which also answers "unknown") reads "already gone" over a resting order. One
        // entry (~100 B) per foreign order of the process lifetime; upgrade: evict on a terminal event.
        let mut map = lock(&self.by_coid);
        map.insert(coid.to_string(), symbol.to_string());
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

    /// The foreign instruments touched since the last call, oldest first; the list starts again.
    pub fn take_touched(&self) -> Vec<String> {
        std::mem::take(&mut *lock(&self.touched))
    }
}

/// `ctVal` (base units per contract) of every instrument this account has a client for or has been
/// filled on: the factor a fill's CONTRACT count is multiplied by to reach base units. The mounted
/// instrument's is given at construction; a foreign one is [`Self::register`]ed by the factory when
/// it builds that instrument's client, or loaded on first sight by [`Self::get_or_load`] (a fill of
/// an order placed before a restart, or by hand on the exchange, arrives with no client).
pub struct ContractSizes {
    mounted: String,
    mounted_ct_val: f64,
    others: Mutex<HashMap<String, f64>>,
    /// Upper-cased instrument -> when its `ctVal` last failed to load ([`NEGATIVE_TTL`]).
    unreadable: Mutex<HashMap<String, Instant>>,
}

impl ContractSizes {
    pub fn new(mounted: impl Into<String>, mounted_ct_val: f64) -> Self {
        Self {
            mounted: mounted.into(),
            mounted_ct_val,
            others: Mutex::new(HashMap::new()),
            unreadable: Mutex::new(HashMap::new()),
        }
    }

    pub fn register(&self, inst: &str, ct_val: f64) {
        lock(&self.others).insert(inst.to_uppercase(), ct_val);
    }

    /// The instrument's `ctVal`, from `load` (one blocking REST row read, run OUTSIDE the lock) the
    /// first time a foreign instrument is seen. `None` = it cannot be read: the caller must not size
    /// anything with a stand-in. A failed load is remembered for [`NEGATIVE_TTL`] (the answer is
    /// `None` without asking again), so a run of fills on an unreadable instrument does not stall the
    /// user-data pump once per fill; after that the next fill asks again.
    pub fn get_or_load(&self, inst: &str, load: impl FnOnce() -> Option<f64>) -> Option<f64> {
        if inst.eq_ignore_ascii_case(&self.mounted) {
            return Some(self.mounted_ct_val);
        }
        let key = inst.to_uppercase();
        if let Some(known) = lock(&self.others).get(&key) {
            return Some(*known);
        }
        if lock(&self.unreadable).get(&key).is_some_and(|at| at.elapsed() < NEGATIVE_TTL) {
            return None;
        }
        let Some(loaded) = load() else {
            lock(&self.unreadable).insert(key, Instant::now());
            return None;
        };
        lock(&self.unreadable).remove(&key);
        self.register(inst, loaded);
        Some(loaded)
    }
}

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
pub struct PerSymbolRest<R> {
    mounted_symbol: String,
    clients: Mutex<Clients<R>>,
    make: ClientFactory<R>,
    /// Builds a client that signs a cancel or a status query with NO network: those two never format a
    /// price or a size, so a venue whose grid fetch is down or slow must not hold them up. `None` =
    /// they take the full client like every other call.
    make_light: Option<ClientFactory<R>>,
    coids: Arc<CoidSymbols>,
}

impl<R> PerSymbolRest<R> {
    /// `mounted_symbol` is the `instId` orders on the mounted instrument carry.
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
                    failed.insert(key, (Instant::now(), why.clone()));
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

impl<R: VenueRest> VenueRest for PerSymbolRest<R> {
    fn submit_order(&self, request: &OrderRequest) -> Vec<Event> {
        if !self.is_mounted(&request.symbol) {
            self.coids.record(&request.client_order_id, &request.symbol);
            self.coids.touch(&request.symbol);
        }
        self.on(&request.symbol, |r| r.submit_order(request)).unwrap_or_else(|why| {
            tracing::warn!(target: "vike_okx::per_symbol", symbol = %request.symbol, %why, "order refused: no client for its instrument");
            vec![
                Event::OrderSubmitted(OrderSubmitted {
                    client_order_id: request.client_order_id.clone(),
                    ts: request.ts,
                }),
                Event::OrderRejected(OrderRejected { cause: None,
                    client_order_id: request.client_order_id.clone(),
                    reason: why.into(),
                    ts: request.ts,
                }),
            ]
        })
    }

    fn cancel_order(&self, client_order_id: &str) -> Result<(), VenueApiError> {
        let symbol = self.symbol_of_coid(client_order_id);
        self.on_light(&symbol, |r| r.cancel_order(client_order_id))
            .unwrap_or_else(|why| Err(VenueApiError { code: 0, msg: why }))
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

    /// The mounted client's native batch when EVERY order is on the mounted instrument; otherwise the
    /// per-order fan-out, which routes each order to its own instrument.
    fn submit_batch(&self, requests: &[OrderRequest]) -> Vec<Event> {
        if requests.iter().all(|r| self.is_mounted(&r.symbol)) {
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

/// The active confirm asks about the order's OWN instrument (a foreign coid) or the mounted one.
impl<R: ConfirmByCoid> ConfirmByCoid for PerSymbolRest<R> {
    const LOOKUP_WINDOW: Option<Duration> = R::LOOKUP_WINDOW;

    fn confirm_when(&self, coid: &str, ts: i64, absent_rejects: bool) -> Vec<Event> {
        let symbol = self.symbol_of_coid(coid);
        self.on_light(&symbol, |r| r.confirm_when(coid, ts, absent_rejects)).unwrap_or_default()
    }
}
