//! **One clock offset per client of the binance family.** A venue refuses a request stamped outside its window (binance's
//! `timestamp` against a 5 s `recvWindow`, `-1021`; aster's `nonce`, within about 10 s). One binance account used to sign
//! its spot orders on the venue's clock and its reconcile reads, perp orders and spot subscribe on the local one, so with a
//! clock off by enough the spot orders passed and the rest was refused, and
//! `vike_bridge_core::transport`'s `websocket_ack_is_transient` lists `-1021`, so the user-data lane reconnected for ever.
//!
//! The offset is `vike_bridge_core::signer`'s `VenueClock`, the one type every venue signs on, shared by clone. Every
//! signer and auth frame of one exec client and its reconcile client takes its stamp from one (the adapter builds it and
//! hands it to both, like the admitted-symbol list). A signer built for one call holds its own. [`RestTransportOnVenueClock`] is how
//! one comes to hold an answer here: the transport those clients sign through asks the venue before the first signed
//! request, so a client whose build must stay pure still signs on the venue's clock, and the exec thread asks at its start
//! so the first order pays nothing.
//!
//! **Nothing moves at zero skew:** the stamp is the local clock plus zero, the same bytes (`clock_tests.rs`, and the frozen
//! `fixtures/r6/signer.json` and `ws_auth.json` hold the signer and the frame). A read that fails leaves the offset as it
//! is, which is the local clock until one answers. It is read once and then kept, so a host clock stepped later is not
//! followed: the preflight's clock leg is what judges the host.

use vike_bridge_core::http::blocking_agent_with_timeout;
use vike_bridge_core::signer::{Signer, VENUE_CLOCK_READ_TIMEOUT, VenueClock};
use vike_bridge_core::transport::{RestTransport, UreqTransport, VenueApiError};

use crate::family::execution::rest_reads::server_time_offset;

/// A transport whose signed requests go out with their client's [`VenueClock`] read: `inner` carries every request,
/// and the clock is read on `reader`, a keyless agent of its own bounded at [`VENUE_CLOCK_READ_TIMEOUT`] that spends no
/// order window.
pub struct RestTransportOnVenueClock<T, R = UreqTransport> {
    inner: T,
    reader: R,
    clock: VenueClock,
    time_path: &'static str,
}

impl<T> RestTransportOnVenueClock<T> {
    /// `inner`, for the client signing on `clock`, whose host publishes its clock at `time_path`.
    pub fn new(inner: T, venue: &'static str, clock: &VenueClock, time_path: &'static str) -> Self {
        RestTransportOnVenueClock {
            inner,
            reader: UreqTransport::with_agent(
                venue,
                blocking_agent_with_timeout(VENUE_CLOCK_READ_TIMEOUT),
            ),
            clock: clock.clone(),
            time_path,
        }
    }
}

impl<T, R: RestTransport> RestTransportOnVenueClock<T, R> {
    /// The path this client reads its host's clock at.
    pub fn time_path(&self) -> &'static str {
        self.time_path
    }

    /// Read the clock from `base_url` if a read is due. A failure is warned and signs on the offset held.
    pub fn sync(&self, base_url: &str) {
        let read = self.clock.read_if_due(vike_model::now_ms(), |local| {
            server_time_offset(&self.reader, base_url, self.time_path, local)
        });
        if let Err(e) = read {
            tracing::warn!(
                target: "vike_binance::clock", base_url, path = self.time_path, code = e.code, msg = %e.msg,
                "server time read failed; signing on the offset held, the local clock until a read answers"
            );
        }
    }
}

impl<T: RestTransport, R: RestTransport> RestTransport for RestTransportOnVenueClock<T, R> {
    fn signed(
        &self,
        base_url: &str,
        path: &str,
        method: &str,
        params: &[(&str, String)],
        signer: &dyn Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        self.sync(base_url);
        self.inner.signed(base_url, path, method, params, signer)
    }

    fn public(
        &self,
        base_url: &str,
        path: &str,
        params: &[(&str, String)],
    ) -> Result<serde_json::Value, VenueApiError> {
        self.inner.public(base_url, path, params)
    }

    fn signed_requery(
        &self,
        base_url: &str,
        path: &str,
        method: &str,
        params: &[(&str, String)],
        signer: &dyn Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        self.sync(base_url);
        self.inner.signed_requery(base_url, path, method, params, signer)
    }
}

#[path = "transport_tests.rs"]
#[cfg(test)]
mod transport_tests;
