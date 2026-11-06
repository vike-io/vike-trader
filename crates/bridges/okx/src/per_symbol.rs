//! One account engine, many instruments: okx's side of the router that signs every order, cancel,
//! amend and status query on the ORDER's own `instId`, with THAT instrument's tick/lot grid and
//! `ctVal`. The router itself (the mounted default, the lazily built and cached foreign client, the
//! negative cache, the light client for cancel and status, the coid map) is
//! `vike_bridge_core::per_symbol`, one copy for binance, aster, bybit and okx; its module doc is the
//! rule. What stays here is okx's own:
//!
//! OKX sizes in CONTRACTS, so the instrument is part of the quantity: `sz` is the base qty divided by
//! THAT instrument's `ctVal`. A foreign order therefore needs its own client (own `ctVal`, own grid,
//! own order-rate window: the venue meters 60 per 2 s per instrument), and its FILLS need the same
//! `ctVal` back ([`ContractSizes`], read by [`crate::event_mapper::map_okx_perp_by_inst`]). The
//! factory refuses an instrument it cannot trade (unreachable, unknown, not a tradable class, an
//! inverse swap) in words, so nothing is ever signed with another instrument's `ctVal`. A failed
//! `ctVal` load is remembered for the router's own `vike_bridge_core::per_symbol::NEGATIVE_TTL`
//! ([`ContractSizes::get_or_load`]), so a burst on a bad instrument costs one blocking fetch.
//!
//! And [`refused_no_client`], okx's line for a submit refused for want of a client.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use vike_bridge_core::per_symbol::NEGATIVE_TTL;
use vike_model::OrderRequest;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The refusal line handed to `vike_bridge_core::per_symbol::PerSymbolRest::new`
/// (`vike_bridge_core::per_symbol::RefusalLog`).
pub fn refused_no_client(request: &OrderRequest, why: &str) {
    tracing::warn!(target: "vike_okx::per_symbol", symbol = %request.symbol, %why, "order refused: no client for its instrument");
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
