//! Aster's listenKey REST auth — **the sole divergence** between Aster's user-data pumps and the
//! shared [`vike_binance::family::listenkey`] core they otherwise run verbatim.
//!
//! Where Binance mints its listenKey with an `X-MBX-APIKEY` header and no signature, Aster's v3
//! endpoints are SIGNED: an EMPTY business-param set signed by the agent wallet (EIP-712 via
//! [`AsterSigner`]), yielding `user`/`signer`/`nonce`/`signature` in the query — and NO
//! `X-MBX-APIKEY` header (sending one would be wrong, not merely redundant).
//!
//! ONE impl serves BOTH Aster streams — spot (`/api/v3/listenKey`) and perp (`/fapi/v3/listenKey`)
//! differ only in the `path` handed to [`AsterListenKeyAuth::new`]. Before rung 4a each stream
//! carried its own copy of this.
//!
//! OWNERSHIP: [`AsterSigner`] is NOT `Clone`, so this struct is built ONCE inside the pump thread
//! (from the moved `Credentials`, which IS `Clone`) via the family pump's `make_auth` factory, and
//! then borrowed `&self` (shared) by both the on-connect create and the periodic keepalive.
//!
//! The signer's µs nonce source is `vike_model::now_us` (Aster's `nonce` is µs, ±10 s of server
//! time; no skew correction wired here — deferred, see exec.rs).

use vike_binance::family::listenkey::ListenKeyAuth;
use vike_bridge_core::credentials::Credentials;
use vike_bridge_core::Signer;

use crate::signing::AsterSigner;

/// The v3-signed listenKey auth for one Aster stream, bound to that stream's endpoint `path`.
pub struct AsterListenKeyAuth {
    signer: AsterSigner,
    path: &'static str,
}

impl AsterListenKeyAuth {
    /// Build the signer for the stream served at `path` (`/api/v3/listenKey` or
    /// `/fapi/v3/listenKey`). Call this ON the pump thread — see the module doc's OWNERSHIP note.
    pub fn new(creds: &Credentials, path: &'static str) -> Self {
        AsterListenKeyAuth { signer: AsterSigner::new(creds, vike_model::now_us), path }
    }
}

impl ListenKeyAuth for AsterListenKeyAuth {
    fn query(&self, method: &str) -> String {
        // Signs an EMPTY business-param set — the signer appends `nonce`/`user`/`signer`/
        // `signature`, and the same ordered params form the query.
        self.signer.prepare(&[], method, self.path).query
    }
    fn header(&self) -> Option<(&'static str, String)> {
        None // v3 auth rides in the signed query — never an X-MBX-APIKEY header
    }
}
