//! The shared plumbing every venue bridge is built from: blocking REST transport + signers, the
//! venue-neutral WS user-data pump with reconnect/backoff/keepalive/resync, the ExecActor command
//! thread, the pinned Decimal wire-format site, the venue mount contract, and the generic
//! `ws`/`rest`/`klines` fragments (`TungsteniteStream`/`WsSocket`, the `VenueRest`/`LiveRestClient<R>`
//! REST-exec seam, `kline_to_bar`) every sibling venue reaches into. Depends on vike-model,
//! vike-secrets, vike-exec (lanes/seams) and vike-data (the mount contract's `PropertiesRecorder`).
//!
//! The credential loaders (the absent-credentials-is-the-live-gate authority), the account
//! directory, the passphrase table, the key-permission gate and the HALT sentinel's FILE half are
//! NOT here: they live in `vike-secrets` (`vike_secrets::credentials`, `::account_directory`,
//! `::venue_passphrase`, `::key_permissions`, `::halt`) since decision 0135, so a consumer that
//! needs only them links no transport stack. This crate has no `full` feature and no
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
// market orders at; `leverage`'s sibling, resolved once at the mount by a pure, bounded fn.
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
pub mod signer;
pub mod stream_health;
pub mod sub_ack;
pub mod transport;
pub mod trigger;
pub mod user_data;
// THE VENUE MOUNT CONTRACT (docs/decisions/0096).
pub mod venue_mount;
// Its test double, never in a normal build — the `scripted` shape.
#[cfg(any(test, feature = "test-support"))]
pub mod venue_mount_fixture;
pub mod ws;
pub mod ws_proxy;

#[cfg(feature = "capture")]
pub use capture::{
    CapturedFixture, FrameCapture, REDACT_KEYS, SANITIZER_VERSION, SanitizedFrame, capture_frame,
    frame_mutations, load_captured,
};
pub use connectivity::{
    ConnectivityProbe, DEFAULT_NEUTRAL_ENDPOINTS, DEFAULT_PROBE_TIMEOUT, OutageClass, tcp_reachable,
};
pub use depth::{
    BookOp, DepthFaultLog, FAULT_LOG_EVERY, SessionOutcome, infer_tick_size, run_depth_feed,
    run_depth_session,
};
#[cfg(feature = "eip712")]
pub use eip712::{
    digest, domain_separator, domain_separator_no_contract, enc_address, enc_string, enc_uint,
    enc_uint256_dec, eth_address_from_private_key, hash_struct, keccak256, sign_digest,
    sign_digest_hex,
};
pub use error_kind::{
    CodeTable, ExecErrorPolicy, MsgTable, SubmitDisposition, VenueTaxonomy, classify_venue,
    reject_reason, submit_disposition,
};
pub use event_map::terminal_events;
pub use format::{format_to_step, format_to_step_f};
pub use json::{json_int, json_num, json_str};
pub use klines::kline_to_bar;
pub use mark_stream::{MarkPairings, is_valid_mark, mark_streams_from};
pub use market_pump::{
    Keepalive, MarketPumpOpts, PumpBackoff, connect_market_stream, connect_market_stream_via,
    run_market_feed, run_market_session,
};
pub use net_probe::{
    DEFAULT_FAILURES_BEFORE_DOWN, DEFAULT_PROBE_HOSTS, DEFAULT_PROBE_INTERVAL, NetProbe,
    NetProbeConfig, NetProbeHandle, NetProbeThread, NetTransition, dns_resolves,
};
pub use pacer::Pacer;
pub use poller::{STOP_POLL_SLICE, StopHandle, sleep_stop_aware, spawn_poller};
pub use pump_spec::{MarketPumpSpec, PumpKnobs};
pub use rate_discovery::{WeightBudget, parse_weight_budget};
pub use rest::{LiveRestClient, resolve_ambiguous_submit};
pub use retry::{BackoffPolicy, Verdict, retry_rate_limited};
pub use signer::{BybitV5Signer, OkxV5Signer, compact_json, iso8601_ms, py_str, urlencode};
pub use sub_ack::SubscribeAck;
pub use transport::E_TIMEOUT_AMBIGUOUS;
pub use user_data::{
    OpenOutcome, StreamMsg, UserDataAuthError, UserDataFeed, UserDataResyncFeed, UserStream,
    sleep_unless_stopped,
};
pub use ws::{AckResult, TungsteniteStream, await_ack, configure_ws_stream, is_timeout};
pub use ws_proxy::{WsProxy, connect_ws, ws_target};
