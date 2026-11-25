//! The wire to the venue: the transport, signing, rate gates, error codes and URLs.

#[cfg(feature = "execution")]
pub mod client;
// The instrument class (spot, swap, dated future) an instId's shape names, and the pure rules that differ by
// class (decision 0232). Keyless and signing nothing, so it rides both planes.
#[cfg(feature = "execution")]
pub mod error_codes;
pub mod ratelimit;
#[cfg(feature = "execution")]
pub mod ws_auth;
