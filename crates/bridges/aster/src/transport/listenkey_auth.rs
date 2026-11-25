//! Aster's listenKey REST auth — **the sole divergence** between Aster's user-data pumps and the
//! shared [`vike_binance::family::execution::user_data`] core they otherwise run verbatim.
//!
//! Where Binance mints its listenKey with an `X-MBX-APIKEY` header and no signature, Aster's v3
//! endpoints are SIGNED: an EMPTY business-param set signed by the agent wallet (EIP-712 via
//! [`SignerAster`]), yielding `user`/`signer`/`nonce`/`signature` in the query — and NO
//! `X-MBX-APIKEY` header (sending one would be wrong, not merely redundant).
//!
//! ONE impl serves BOTH Aster streams — spot (`/api/v3/listenKey`) and perp (`/fapi/v3/listenKey`)
//! differ only in the `path` handed to [`ListenKeyAuthenticationAster::new`]. Before rung 4a each stream
//! carried its own copy of this.
//!
//! OWNERSHIP: [`SignerAster`] is NOT `Clone`, so this struct is built ONCE per pump (from the
//! `Credentials`, which IS `Clone`) and handed to the family pump, which shares it (`Arc`) between the
//! on-connect create and the periodic keepalive.
//!
//! The signer's µs nonce comes from the exec client's [`VenueClock`] (Aster's `nonce` is µs, ±10 s of
//! server time), which `crate::execution`'s drivers read before they start this pump.

use vike_binance::family::execution::user_data::ListenKeyAuthentication;
use vike_bridge_core::signer::Signer;
use vike_bridge_core::signer::VenueClock;
use vike_secrets::credentials::Credentials;

use crate::transport::sign::SignerAster;

/// The v3-signed listenKey auth for one Aster stream, bound to that stream's endpoint `path`.
pub struct ListenKeyAuthenticationAster {
    signer: SignerAster,
    path: &'static str,
}

impl ListenKeyAuthenticationAster {
    /// Build the signer for the stream served at `path` (`/api/v3/listenKey` or
    /// `/fapi/v3/listenKey`). Once per pump — see the module doc's OWNERSHIP note.
    pub fn new(creds: &Credentials, path: &'static str, clock: &VenueClock) -> Self {
        ListenKeyAuthenticationAster { signer: SignerAster::new(creds, clock.us()), path }
    }
}

impl ListenKeyAuthentication for ListenKeyAuthenticationAster {
    fn query(&self, method: &str) -> String {
        // Signs an EMPTY business-param set — the signer appends `nonce`/`user`/`signer`/
        // `signature`, and the same ordered params form the query. Empty on the PUT too: the futures
        // page lists no parameter and the spot page says `listenKey` is "accepted but ignored"
        // (`asterdex/api-docs`, `V3(Recommended)/EN/`), which no aster PUT has yet proven live.
        self.signer.prepare(&[], method, self.path).query
    }
    fn header(&self) -> Option<(&'static str, String)> {
        None // v3 auth rides in the signed query — never an X-MBX-APIKEY header
    }
}
