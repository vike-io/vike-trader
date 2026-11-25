//! One account engine, many symbols: bybit's router, which signs every order, cancel, amend and
//! status query on the ORDER's own symbol, with THAT symbol's price/qty grid. The rule (the mounted
//! default, the lazily built and cached foreign client from `instruments-info`, the negative cache,
//! the light client for cancel and status, the order client id map and its mounted fallback) is
//! `vike_bridge_core::per_symbol`'s `VenueRestPerSymbol`, one copy for binance, aster, bybit and okx, held
//! inside this type; its module doc is the contract. Bybit's departure stays here: **a submit passes the
//! account-mode check first** ([`VenueRestPerSymbolBybit::with_mode_check`]: `crate::account::modes`): a symbol held in
//! hedge position mode is put in one-way, or its order is refused in words. A cancel, amend or status
//! query never asks.

use std::sync::Arc;

use vike_bridge_core::confirm::ConfirmByOrderClientId;
use vike_bridge_core::per_symbol::{self, ClientFactory, OrderClientIdSymbols};
use vike_bridge_core::rest::{VenueRest, refused};
use vike_bridge_core::transport::VenueApiError;
use vike_model::OrderRequest;
use vike_model::events::Event;

/// Runs before the FIRST order of a symbol (`crate::account::modes::Modes::ensure`, which caches): `Ok` = the symbol is
/// in the modes the adapter orders in (or was put there), `Err(why)` = the order is refused in those words,
/// nothing signed. Blocking REST: exec thread only.
pub type ModeCheck = Box<dyn Fn(&str) -> Result<(), String> + Send + Sync>;

/// bybit's line for a submit refused for want of a client (`vike_bridge_core::per_symbol::RefusalLog`).
fn refused_no_client(request: &OrderRequest, why: &str) {
    tracing::warn!(target: "vike_bybit::execution::rest::per_symbol", symbol = %request.symbol, %why, "order refused: no client for its symbol");
}

/// See the module doc.
pub struct VenueRestPerSymbolBybit<R> {
    router: per_symbol::VenueRestPerSymbol<R>,
    /// The account-mode rule every SUBMIT passes first ([`ModeCheck`]). `None` = none (the confirm worker's
    /// router, tests).
    modes: Option<ModeCheck>,
}

impl<R> VenueRestPerSymbolBybit<R> {
    /// `mounted_symbol` is the label orders on the mounted instrument carry.
    pub fn new(
        mounted_symbol: impl Into<String>,
        mounted: R,
        make: ClientFactory<R>,
        order_client_ids: Arc<OrderClientIdSymbols>,
    ) -> Self {
        Self {
            router: per_symbol::VenueRestPerSymbol::new(
                mounted_symbol,
                mounted,
                make,
                order_client_ids,
                refused_no_client,
            ),
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

    /// Cancel and status of a foreign order use the client `make_light` builds (it must fetch nothing)
    /// unless the full one already exists (`vike_bridge_core::per_symbol`'s
    /// `VenueRestPerSymbolBybit::with_light_factory`). Amend and submit still need the grid.
    #[must_use]
    pub fn with_light_factory(mut self, make_light: ClientFactory<R>) -> Self {
        self.router = self.router.with_light_factory(make_light);
        self
    }
}

impl<R: VenueRest> VenueRest for VenueRestPerSymbolBybit<R> {
    fn submit_order(&self, request: &OrderRequest) -> Vec<Event> {
        self.router.note_submit(request);
        self.router
            .on(&request.symbol, |r| match self.modes_ok(&request.symbol) {
                // The grid fetch and the mode check spend the order's time: the age is asked again before the POST.
                Ok(()) => match vike_bridge_core::exec_actor::too_old_to_post(request) {
                    Some(why) => refused(request, why),
                    None => r.submit_order(request),
                },
                Err(why) => {
                    tracing::warn!(target: "vike_bybit::execution::rest::per_symbol", symbol = %request.symbol, %why, "order refused: the symbol's account mode is not one this adapter orders in");
                    refused(request, why)
                }
            })
            .unwrap_or_else(|why| {
                refused_no_client(request, &why);
                refused(request, why)
            })
    }

    fn cancel_order(
        &self,
        order_client_id: &str,
    ) -> Result<vike_bridge_core::rest::OrderCancelAck, VenueApiError> {
        self.router.cancel_order(order_client_id)
    }

    fn confirm_order(&self, order_client_id: &str, ts: i64) -> Vec<Event> {
        self.router.confirm_order(order_client_id, ts)
    }

    fn modify_order(
        &self,
        order: &OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
    ) -> Vec<Event> {
        self.router.modify_order(order, new_qty, new_price)
    }

    // Forwarded by hand: the trait's default would shadow the venue's trigger amend with its refusal.
    fn modify_trigger(
        &self,
        order: &OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
        new_trigger_price: f64,
    ) -> Vec<Event> {
        self.router.modify_trigger(order, new_qty, new_price, new_trigger_price)
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
    /// THIS router's cancel, first error wins.
    fn cancel_batch(&self, order_client_ids: &[String]) -> Result<(), VenueApiError> {
        self.router.cancel_batch_with(order_client_ids, |c| self.cancel_order(c))
    }
}

impl<R: ConfirmByOrderClientId> ConfirmByOrderClientId for VenueRestPerSymbolBybit<R> {
    const LOOKUP_WINDOW: Option<std::time::Duration> = R::LOOKUP_WINDOW;

    fn confirm_when(&self, order_client_id: &str, ts: i64, absent_rejects: bool) -> Vec<Event> {
        self.router.confirm_when(order_client_id, ts, absent_rejects)
    }
}
