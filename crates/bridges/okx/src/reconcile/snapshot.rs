//! `VenueRestOkx`'s ticker read, `last_price`. Positions, open orders and balance are read by
//! `crate::reconcile`'s `ReconcileClientOkx`, not here.

use vike_bridge_core::json::json_number;
use vike_bridge_core::transport::VenueApiError;
use vike_bridge_core::transport::json::RestTransportJson;

use crate::execution::rest::{PATH_TICKER, VenueRestOkx};
use crate::transport::client::unwrap_okx;

impl<T: RestTransportJson> VenueRestOkx<T> {
    pub fn last_price(&self) -> Result<f64, VenueApiError> {
        let data = self.transport.public(
            &self.base_url,
            PATH_TICKER,
            &[("instId", self.symbol.clone())],
        )?;
        let data = unwrap_okx(data)?;
        Ok(data
            .as_array()
            .and_then(|d| d.first())
            .and_then(|t| t.get("last"))
            .and_then(json_number)
            .unwrap_or(0.0))
    }
}
