//! Binance API-key permission introspection — the FIRST concrete row of the bridge-core
//! [`KeyPermissionProbe`] seam (api-key-permissions capability map, STEP-1).
//!
//! Reads `GET /sapi/v1/account/apiRestrictions` (signed, over the SAME `ureq` + HMAC stack the exec
//! / recon clients use — no new stack) and maps it into [`KeyPermissions`]:
//! - `enableWithdrawals` -> `can_withdraw` (the field the withdraw gate keys off)
//! - `enableSpotAndMarginTrading` -> `can_trade` (informational; futures/options tracked by
//!   separate flags, deliberately not folded in)
//! - `ipRestrict` -> `ip_restricted` (informational)
//!
//! Each maps to `Some(bool)` when present-and-boolean, else `None` (Unknown).
//!
//! sapi CAVEAT: `/sapi/v1/*` (wallet endpoints) are MAINNET-only — the spot testnet
//! (`demo-api.binance.com`) does not serve them, so a live introspection call must target
//! [`crate::spot::MAINNET_REST`]. `base_url` is therefore a parameter (a test injects any host); the
//! pure [`parse_api_restrictions`] is what the fixture tests exercise — there is NO network here.
//!
//! Not wired into `make_engine` yet — see the bridge-core module doc for the reported STEP-2 wiring.

use vike_bridge_core::credentials::Credentials;
use vike_bridge_core::key_permissions::{KeyPermissionProbe, KeyPermissions};
use vike_bridge_core::signer::{BinanceHmacSigner, Signer};
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_model::now_ms;

/// The signed wallet endpoint that reports the key's restrictions (mainnet-only — see module doc).
pub const PATH_API_RESTRICTIONS: &str = "/sapi/v1/account/apiRestrictions";

/// Pure: a `/sapi/v1/account/apiRestrictions` response body -> [`KeyPermissions`]. Each field is
/// `Some(bool)` when present-and-boolean, else `None` (Unknown — a malformed/partial 200 body maps
/// to Unknown, which the policy does NOT refuse; the step-2 caller decides how to treat a hard
/// fetch error). Malformed JSON is a hard `Err`, never a panic. Fixture-tested, no network.
pub fn parse_api_restrictions(body: &str) -> Result<KeyPermissions, String> {
    let v: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let flag = |key: &str| v.get(key).and_then(|x| x.as_bool());
    Ok(KeyPermissions {
        can_withdraw: flag("enableWithdrawals"),
        can_trade: flag("enableSpotAndMarginTrading"),
        ip_restricted: flag("ipRestrict"),
    })
}

/// A thin signed-REST probe that fetches the configured key's permissions. Signer/transport are
/// seams (offline tests stub the transport with canned JSON; a live mount uses `UreqTransport` + the
/// HMAC signer via [`key_permission_probe`]). `base_url` MUST be a mainnet host (sapi caveat).
pub struct BinanceKeyPermissionProbe<S: Signer, T: RestTransport> {
    pub signer: S,
    pub transport: T,
    pub base_url: String,
}

impl<S: Signer, T: RestTransport> BinanceKeyPermissionProbe<S, T> {
    pub fn new(signer: S, transport: T, base_url: impl Into<String>) -> Self {
        BinanceKeyPermissionProbe { signer, transport, base_url: base_url.into() }
    }
}

impl<S: Signer, T: RestTransport> KeyPermissionProbe for BinanceKeyPermissionProbe<S, T> {
    /// Signed GET (no params) — the same `signed` path the recon/exec reads use; the `Value` is
    /// round-tripped to text and fed through [`parse_api_restrictions`] (mirrors the family recon
    /// `signed` helper). A transport error surfaces as `Err(msg)`.
    fn fetch_key_permissions(&self) -> Result<KeyPermissions, String> {
        let body = self
            .transport
            .signed(&self.base_url, PATH_API_RESTRICTIONS, "GET", &[], &self.signer)
            .map(|v| v.to_string())
            .map_err(|e| e.to_string())?;
        parse_api_restrictions(&body)
    }
}

/// The concrete live probe (HMAC signer + `ureq` transport) `make_engine` will build at STEP-2.
pub type BinanceKeyProbe = BinanceKeyPermissionProbe<BinanceHmacSigner, UreqTransport>;

/// Build a live probe from credentials over a FRESH HMAC signer + `ureq` transport — the SAME stack
/// the exec/recon clients use (no new HTTP/TLS/crypto dep). `base_url` MUST be a mainnet host
/// ([`crate::spot::MAINNET_REST`]); sapi is mainnet-only (module doc). No rate gate: this is a
/// one-shot startup read, not a hot path. Not called anywhere yet (STEP-2 wiring is reported).
pub fn key_permission_probe(creds: &Credentials, base_url: impl Into<String>) -> BinanceKeyProbe {
    BinanceKeyPermissionProbe::new(
        BinanceHmacSigner::new(creds, now_ms),
        UreqTransport::new("binance"),
        base_url,
    )
}
