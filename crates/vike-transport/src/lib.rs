//! **The ONE network layer**: the blocking HTTP agents, the send-outcome rules, the TLS client configs and (behind the
//! `websocket` feature) the bounded WebSocket dial, below the venue bridges and the hubs alike
//! (`docs/decisions/0299-one-network-layer-vike-transport.md`).
//!
//! The contract, one module each:
//! - [`http`]: every ureq agent of the workspace is built here or ends in one of its finishers
//!   ([`http::agent_no_redirect`], [`http::agent_public_get`], [`http::agent_following`]), so each one installs the DNS
//!   resolver and STATES its redirect limit. A call that carries a credential, a signature or a body follows no
//!   redirect; only a keyless public GET follows a few. A body that may outgrow ureq's 10 MiB default is read with
//!   [`http::read_body_text`]. Every agent sends [`http::USER_AGENT`].
//! - [`dial`]: the bounded TCP dial, no feature needed: the lookup and every address it names under ONE deadline.
//! - [`send_outcome`]: whether a ureq send failure provably preceded the write, and which statuses leave a write's
//!   outcome unknown. The venue layer words them (`vike_bridge_core::transport::classify_send_error`).
//! - [`tls`]: the rustls client config (ring, webpki roots) a raw TLS socket is wrapped with, and the loopback-only
//!   unverified one.
//! - `websocket` and `websocket_proxy` (feature `websocket`; SOCKS5 with `socks-proxy`): the WS socket type, its
//!   stream options and the dial with one window over its TCP phase ([`dial`]) and each handshake read.
//! - `socks` (feature `socks-proxy`): the one SOCKS client, every step inside one deadline: the HTTP lane's ureq
//!   connector `SocksDial` (polymarket's tunnelled agent) and the SOCKS5 exchange of the WS dial's proxied arm.
//!
//! **It names no `vike-*` crate and holds no signer** (decision 0164: the hubs link no signer). That is what lets a crate
//! that may not link the venue layer (`vike-alerting`, the datahub, `vike-backfill`, `vike-nlp`) build its agent here
//! instead of copying one; `crates/vike-ops/tests/architecture/layer_gate/vike_free.rs`'s `VIKE_FREE_CRATES` holds it.
//! Everything venue-shaped (the venue error, the reply engine, the rate gates, the pumps, the signers) stays in
//! `vike-bridge-core`. Every caller names the module path; nothing is re-exported at the root.

pub mod dial;
pub mod http;
pub mod send_outcome;
#[cfg(feature = "socks-proxy")]
pub mod socks;
pub mod tls;
#[cfg(feature = "websocket")]
pub mod websocket;
#[cfg(feature = "websocket")]
pub mod websocket_proxy;
