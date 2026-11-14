//! The wire to the venue: the transport, signing, rate gates, error codes and URLs.

#[cfg(feature = "execution")]
pub mod client;
pub mod ratelimit;
pub mod rpc;
#[cfg(feature = "execution")]
pub mod ws_auth;
