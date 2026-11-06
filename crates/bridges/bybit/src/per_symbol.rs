//! One account engine, many symbols: bybit's router, which signs every order, cancel, amend and
//! status query on the ORDER's own symbol, with THAT symbol's price/qty grid. The rule (the mounted
//! default, the lazily built and cached foreign client from `instruments-info`, the negative cache,
//! the light client for cancel and status, the coid map and its mounted fallback) is
//! `vike_bridge_core::per_symbol`'s `PerSymbolRest`, one copy for binance, aster, bybit and okx, held
//! inside this type; its module doc is the contract. Bybit's two departures stay here:
//!
//! * **A submit passes the account-mode check first** ([`PerSymbolRest::with_mode_check`]:
//!   `crate::modes`): a symbol held in hedge position mode is put in one-way, or its order is refused
//!   in words. A cancel, amend or status query never asks.
//! * **A cancel the venue REFUSES is struck from the cancel notes** ([`PerSymbolRest::with_cancel_notes`]),
//!   for each id of a batch fan-out too.

use std::sync::Arc;

use vike_bridge_core::confirm::ConfirmByCoid;
use vike_bridge_core::per_symbol::{self, ClientFactory, CoidSymbols};
use vike_bridge_core::rest::{VenueRest, refused};
use vike_bridge_core::transport::VenueApiError;
use vike_model::OrderRequest;
use vike_model::events::Event;

use crate::user_data::CancelNotes;

/// Runs before the FIRST order of a symbol (`crate::modes::Modes::ensure`, which caches): `Ok` = the symbol is
/// in the modes the adapter orders in (or was put there), `Err(why)` = the order is refused in those words,
/// nothing signed. Blocking REST: exec thread only.
pub type ModeCheck = Box<dyn Fn(&str) -> Result<(), String> + Send + Sync>;

/// bybit's line for a submit refused for want of a client (`vike_bridge_core::per_symbol::RefusalLog`).
fn refused_no_client(request: &OrderRequest, why: &str) {
    tracing::warn!(target: "vike_bybit::per_symbol", symbol = %request.symbol, %why, "order refused: no client for its symbol");
}

/// See the module doc.
pub struct PerSymbolRest<R> {
    router: per_symbol::PerSymbolRest<R>,
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
            router: per_symbol::PerSymbolRest::new(
                mounted_symbol,
                mounted,
                make,
                coids,
                refused_no_client,
            ),
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
        check(if symbol.is_empty() { self.router.mounted_symbol() } else { symbol })
    }

    /// A cancel this router sends and the venue REFUSES leaves the order live, so its note is struck
    /// from `notes` (a later `Deactivated` of that order is then the venue's own, an expiry).
    #[must_use]
    pub fn with_cancel_notes(mut self, notes: Arc<CancelNotes>) -> Self {
        self.notes = Some(notes);
        self
    }

    /// Cancel and status of a foreign order use the client `make_light` builds (it must fetch nothing)
    /// unless the full one already exists (`vike_bridge_core::per_symbol`'s
    /// `PerSymbolRest::with_light_factory`). Amend and submit still need the grid.
    #[must_use]
    pub fn with_light_factory(mut self, make_light: ClientFactory<R>) -> Self {
        self.router = self.router.with_light_factory(make_light);
        self
    }
}

impl<R: VenueRest> VenueRest for PerSymbolRest<R> {
    fn submit_order(&self, request: &OrderRequest) -> Vec<Event> {
        self.router.note_submit(request);
        self.router
            .on(&request.symbol, |r| match self.modes_ok(&request.symbol) {
                Ok(()) => r.submit_order(request),
                Err(why) => {
                    tracing::warn!(target: "vike_bybit::per_symbol", symbol = %request.symbol, %why, "order refused: the symbol's account mode is not one this adapter orders in");
                    refused(request, why)
                }
            })
            .unwrap_or_else(|why| {
                refused_no_client(request, &why);
                refused(request, why)
            })
    }

    fn cancel_order(&self, client_order_id: &str) -> Result<(), VenueApiError> {
        let answer = self.router.cancel_order(client_order_id);
        if let (Err(_), Some(notes)) = (&answer, &self.notes) {
            notes.forget(client_order_id);
        }
        answer
    }

    fn confirm_order(&self, client_order_id: &str, ts: i64) -> Vec<Event> {
        self.router.confirm_order(client_order_id, ts)
    }

    fn modify_order(
        &self,
        order: &OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
    ) -> Vec<Event> {
        self.router.modify_order(order, new_qty, new_price)
    }

    /// The mounted client's native batch when EVERY order is on the mounted symbol; otherwise the
    /// per-order fan-out, which routes each order to its own symbol.
    fn submit_batch(&self, requests: &[OrderRequest]) -> Vec<Event> {
        // A mounted batch the mode check refuses falls through to the per-order fan-out, which words
        // each order's refusal (the check caches, so it asks the venue once).
        if self.router.all_mounted(requests) && self.modes_ok("").is_ok() {
            return self.router.with_mounted(|m| m.submit_batch(requests));
        }
        requests.iter().flat_map(|r| self.submit_order(r)).collect()
    }

    /// The mounted client's native batch when no id is a foreign order; otherwise per id through
    /// THIS router's cancel (so a refused one is struck from the notes), first error wins.
    fn cancel_batch(&self, client_order_ids: &[String]) -> Result<(), VenueApiError> {
        self.router.cancel_batch_with(client_order_ids, |c| self.cancel_order(c))
    }
}

impl<R: ConfirmByCoid> ConfirmByCoid for PerSymbolRest<R> {
    const LOOKUP_WINDOW: Option<std::time::Duration> = R::LOOKUP_WINDOW;

    fn confirm_when(&self, coid: &str, ts: i64, absent_rejects: bool) -> Vec<Event> {
        self.router.confirm_when(coid, ts, absent_rejects)
    }
}
