//! The shared plumbing every venue bridge is built from: blocking REST transport + signers, the
//! venue-neutral WS user-data pump with reconnect/backoff/keepalive/resync, the ExecActor command
//! thread, the pinned Decimal wire-format site, the venue-adapter contract, and the generic
//! `ws`/`rest`/`klines` fragments (`TungsteniteStream`/`WsSocket`, the `VenueRest`/`LiveRestClient<R>`
//! REST-exec seam, `kline_to_bar`) every sibling venue reaches into. Depends on vike-model,
//! vike-secrets, vike-exec (lanes/seams) and vike-data (the start contract's `PropertiesRecorder`).
//!
//! The credential loaders (the absent-credentials-is-the-live-gate authority), the account
//! directory, the passphrase table and the key-permission gate are NOT here: they live in
//! `vike-secrets` (`vike_secrets::credentials`, `::account_directory`, `::venue_passphrase`,
//! `::key_permissions`) since decision 0135, so a consumer that needs only them links no transport
//! stack. The HALT sentinel is not here either: its path half is `vike_tradehub_core::halt`, its engaged
//! check `vike_exec::halt::sentinel_engaged`. This crate has no `full` feature and no
//! default-features-off configuration: every build of it is the whole crate.

#[cfg(feature = "capture")]
pub mod capture;
// Bounded backfill concurrency over ONE shared `pacer`. std::thread only — no async, no rayon.
pub mod concurrent;
pub mod connectivity;
pub mod depth;
#[cfg(feature = "eip712")]
pub mod eip712;
pub mod error_kind;
pub mod event_map;
pub mod exec_actor;
pub mod format;
// The classified-frame vocabulary (data / subscribe ack / subscribe reject / ignored) every
// subscribe-only market-data lane shares. Its payloads are vike-model types, plus the one
// map onto `market_pump`'s `FrameOutcome`.
pub mod feed_frame;
// Strict borrowing JSON scan blocks for the binance-family and bybit typed frame decode. std + serde +
// vike-model.
pub mod frame_scan;
pub mod http;
pub mod json;
pub mod klines;
// The ONE pure rule for the account leverage a perp exec arm POSTs at startup, from the operator's
// `[risk]` budget (`ProfileRisk` data) or the venue's own default. Here, not in each bridge, so the
// perp arms cannot drift from each other nor from the RiskGate's `im_requirement`. It names
// `vike_model::ProfileRisk` and `tracing`, nothing of the transport half.
pub mod leverage;
pub mod mark_stream;
pub mod market_pump;
// The ONE pure rule for the slippage band a venue with NO native market order prices its emulated
// market orders at; `leverage`'s sibling, resolved once at the start by a pure, bounded fn.
pub mod market_slippage;
pub mod net_probe;
// Pace a paged backfill against a DISCOVERED budget (`rate_discovery`). Pure: no clock, no sleep.
pub mod pacer;
pub mod poller;
pub mod pump_spec;
// A venue's OWN published request-weight budget, read out of its `exchangeInfo` body.
pub mod rate_discovery;
pub mod ratelimit;
pub mod rest;
pub mod retry;
// Scripted WS stream doubles (UserStream/MarketStream/DepthStream) for tests: in-crate unit tests
// via `cfg(test)`, the crate's own `tests/` and downstream crates via the `test-support` feature.
// Never in a normal build.
#[cfg(any(test, feature = "test-support"))]
pub mod scripted;
pub mod side;
pub mod signer;
pub mod stream_health;
pub mod sub_ack;
pub mod transport;
pub mod trigger;
pub mod user_data;
// THE VENUE ADAPTER CONTRACT (docs/decisions/0096).
pub mod venue_adapter;
// Its test double, never in a normal build — the `scripted` shape.
#[cfg(any(test, feature = "test-support"))]
pub mod venue_adapter_fixture;
pub mod ws;
pub mod ws_proxy;

// The crate-root names consumers spell at the root (every other item is named at its module path,
// never through a second root name).
pub use event_map::terminal_events;
pub use mark_stream::{MarkPairings, is_valid_mark, mark_streams_from};
pub use market_pump::Keepalive;
pub use net_probe::{
    DEFAULT_PROBE_INTERVAL, NetProbe, NetProbeConfig, NetProbeHandle, NetProbeThread,
};
pub use rest::resolve_ambiguous_submit;
pub use signer::urlencode;
pub use sub_ack::SubscribeAck;
