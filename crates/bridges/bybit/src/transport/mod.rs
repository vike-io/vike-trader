//! The wire to the venue: the transport, signing, rate gates, error codes and URLs.

#[cfg(feature = "execution")]
pub mod client;
#[cfg(feature = "execution")]
pub mod error_codes;
pub mod ratelimit;
#[cfg(feature = "execution")]
pub mod ws_auth;
