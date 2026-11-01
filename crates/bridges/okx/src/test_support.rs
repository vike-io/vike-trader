//! The crate's own offline doubles (`test-support`, never in a normal build; the `vike-ctrader` idiom):
//! a signer over fake credentials, a transport no call may reach, and [`no_okx_signed_call!`] for a
//! double whose form-body `signed` arm must never run. Unit tests reach them as `crate::test_support`,
//! the `tests/` binaries through the self dev-dependency.

use serde_json::Value;
use vike_bridge_core::signer::OkxV5Signer;
use vike_bridge_core::transport::VenueApiError;
use vike_secrets::credentials::Credentials;

use crate::transport::OkxTransport;

/// Expands, inside an `impl OkxTransport for …` block, to a `signed` that panics `why ({path})`: the
/// form-body arm of a double that must never send one (`vike_bridge_core::no_public_call!`'s twin).
#[macro_export]
macro_rules! no_okx_signed_call {
    ($why:literal) => {
        fn signed(
            &self,
            _base: &str,
            path: &str,
            _method: &str,
            _params: &[(&str, ::serde_json::Value)],
            _signer: &::vike_bridge_core::signer::OkxV5Signer,
        ) -> Result<::serde_json::Value, ::vike_bridge_core::transport::VenueApiError> {
            panic!(concat!($why, " ({})"), path)
        }
    };
}

/// A signer over the fake credentials `k`/`s`/`p` and a clock frozen at `0`: nothing it signs is
/// meant for a real venue.
pub fn signer() -> OkxV5Signer {
    let creds = Credentials {
        api_key: "k".to_string().into(),
        api_secret: "s".to_string().into(),
        passphrase: Some("p".to_string().into()),
    };
    OkxV5Signer::new(&creds, || 0)
}

/// A transport that must never be reached: every verb panics naming itself and the path, which proves
/// a refused order (a margin mode or a TIF okx cannot express) never leaves the process.
pub struct NoWire;

impl OkxTransport for NoWire {
    no_okx_signed_call!("a denied order must never reach the wire: signed");
    fn signed_json(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        _body: &str,
        _signer: &OkxV5Signer,
    ) -> Result<Value, VenueApiError> {
        panic!("a denied order must never reach the wire (signed_json {path})")
    }
    vike_bridge_core::no_public_call!("a denied order must never reach the wire (public)");
}
