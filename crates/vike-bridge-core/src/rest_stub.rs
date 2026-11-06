//! The one spelling of an offline `RestTransport` double's arm that must never run (`test-support`
//! only, the `scripted` shape).
//!
//! Most bridge stubs answer the SIGNED lane only: their `public` is a guard that panics naming the
//! path it was asked for. [`no_public_call!`] stamps that arm into the stub's
//! `impl RestTransport for …` block, so the guard is written here once instead of once per stub;
//! [`no_signed_call!`] is the same for a stub that answers the PUBLIC lane only. The macros name
//! `::serde_json::Value`, which every bridge's tests already depend on.

/// Expands, inside an `impl vike_bridge_core::transport::RestTransport for …` block, to a
/// `public` that panics naming the path: `no_public_call!()` panics `unexpected public {path}`,
/// `no_public_call!("why")` panics `why ({path})`, and `no_public_call!(unreachable: "why")` is
/// `unreachable!("why")`, for a stub whose message names no path.
#[macro_export]
macro_rules! no_public_call {
    (unreachable: $why:literal) => {
        fn public(
            &self,
            _base: &str,
            _path: &str,
            _params: &[(&str, String)],
        ) -> Result<::serde_json::Value, $crate::transport::VenueApiError> {
            unreachable!($why)
        }
    };
    () => {
        fn public(
            &self,
            _base: &str,
            path: &str,
            _params: &[(&str, String)],
        ) -> Result<::serde_json::Value, $crate::transport::VenueApiError> {
            panic!("unexpected public {path}")
        }
    };
    ($why:literal) => {
        fn public(
            &self,
            _base: &str,
            path: &str,
            _params: &[(&str, String)],
        ) -> Result<::serde_json::Value, $crate::transport::VenueApiError> {
            panic!(concat!($why, " ({})"), path)
        }
    };
}

/// [`no_public_call!`]'s twin for a stub that answers the PUBLIC lane only: expands, inside an
/// `impl vike_bridge_core::transport::RestTransport for …` block, to a `signed` that panics `why
/// ({path})`.
#[macro_export]
macro_rules! no_signed_call {
    ($why:literal) => {
        fn signed(
            &self,
            _base: &str,
            path: &str,
            _method: &str,
            _params: &[(&str, String)],
            _signer: &dyn $crate::signer::Signer,
        ) -> Result<::serde_json::Value, $crate::transport::VenueApiError> {
            panic!(concat!($why, " ({})"), path)
        }
    };
}
