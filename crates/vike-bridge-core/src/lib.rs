//! The shared plumbing every venue bridge is built from: blocking REST transport + signers, the
//! venue-neutral WS user-data pump with reconnect/backoff/keepalive/resync, the ExecActor command
//! thread, credentials (the absent-credentials-is-the-live-gate authority), the pinned Decimal
//! wire-format site, the venue mount contract, and the generic `ws`/`rest`/`klines` fragments
//! (`TungsteniteStream`/`WsSocket`, the `VenueRest`/`LiveRestClient<R>` REST-exec seam,
//! `kline_to_bar`) every sibling venue reaches into. Depends on vike-model and vike-secrets, plus
//! vike-exec (lanes/seams) and vike-data (the mount contract's `PropertiesRecorder`) under `full`.

// The `full` feature (DEFAULT) gates the TRANSPORT/SIGNING half: the 15 modules below that name a
// venue socket, an HTTP agent, a signer, the execution lanes or the venue mount contract, and with
// them the ureq / tungstenite / hmac / sha2 / hex / base64 / rust_decimal / vike-exec / vike-data
// deps. What is left ungated is the PURE half — std + vike-model + vike-secrets + serde_json +
// tracing — and the seam exists for `credentials`: the workspace's ONE credential-store authority,
// which `vike-cli` (`default-features = false`) reaches without linking a transport stack.
//
// ⚠ Anything naming a `vike_exec` or `vike_data` item stays behind `full`: a workspace build unions
// features and will not catch the break; `scripts/ci_feature_suite.sh`'s `light-consumers` arm
// does. A gate is a `#[cfg]` here, on a module declaration or its re-export, except for the few
// items inside an ungated module that name those crates (`halt`'s re-export of `vike_exec::halt`,
// `stream_health::health_to_stream_status`, `mark_stream::unsubscribe_with_mark`), each gated at
// the item.

#[cfg(feature = "capture")]
pub mod capture;
// Bounded backfill concurrency over ONE shared `pacer`. std::thread only — no async, no rayon.
pub mod concurrent;
pub mod connectivity;
// The settings database's `account` table as the composition root read it, carried by the mount
// contract (`venue_mount::MountInputs::accounts`). Ungated: std plus the `credentials` types.
pub mod account_directory;
pub mod credentials;
#[cfg(feature = "full")]
pub mod depth;
#[cfg(feature = "eip712")]
pub mod eip712;
#[cfg(feature = "full")]
pub mod error_kind;
pub mod event_map;
#[cfg(feature = "full")]
pub mod exec_actor;
#[cfg(feature = "full")]
pub mod format;
pub mod halt;
#[cfg(feature = "full")]
pub mod http;
pub mod json;
pub mod key_permissions;
pub mod klines;
// The ONE pure rule for the account leverage a perp exec arm POSTs at startup, from the operator's
// `[risk]` budget (`ProfileRisk` data) or the venue's own default. Here, not in each bridge, so the
// perp arms cannot drift from each other nor from the RiskGate's `im_requirement`. Ungated: it
// names `vike_model::ProfileRisk` and `tracing`, nothing of the transport half, and the ungated
// `market_slippage` doc-links it (the light half's rustdoc cannot resolve a gated target).
pub mod leverage;
pub mod mark_stream;
#[cfg(feature = "full")]
pub mod market_pump;
// The ONE pure rule for the slippage band a venue with NO native market order prices its emulated
// market orders at; `leverage`'s sibling, resolved once at the mount by a pure, bounded fn.
pub mod market_slippage;
#[cfg(feature = "full")]
pub mod net_probe;
// Pace a paged backfill against a DISCOVERED budget (`rate_discovery`). Pure: no clock, no sleep.
pub mod pacer;
pub mod poller;
#[cfg(feature = "full")]
pub mod pump_spec;
// A venue's OWN published request-weight budget, read out of its `exchangeInfo` body.
pub mod rate_discovery;
pub mod ratelimit;
#[cfg(feature = "full")]
pub mod rest;
pub mod retry;
// Scripted WS stream doubles (UserStream/MarketStream/DepthStream) for tests: in-crate unit tests
// via `cfg(test)`, the crate's own `tests/` and downstream crates via the `test-support` feature.
// Never in a normal build. (`full` is redundant with `test-support`, not with the bare `test` arm.)
#[cfg(all(feature = "full", any(test, feature = "test-support")))]
pub mod scripted;
#[cfg(feature = "full")]
pub mod signer;
pub mod stream_health;
pub mod sub_ack;
#[cfg(feature = "full")]
pub mod transport;
pub mod trigger;
#[cfg(feature = "full")]
pub mod user_data;
// THE VENUE MOUNT CONTRACT (docs/decisions/0096).
#[cfg(feature = "full")]
pub mod venue_mount;
// Its test double, never in a normal build — the `scripted` shape.
#[cfg(all(feature = "full", any(test, feature = "test-support")))]
pub mod venue_mount_fixture;
// The per-venue passphrase-requirement table `credentials` gates on. UNGATED like `credentials`:
// a `--no-default-features` consumer that resolves credentials must resolve them CORRECTLY.
pub mod venue_passphrase;
#[cfg(feature = "full")]
pub mod ws;
#[cfg(feature = "full")]
pub mod ws_proxy;

#[cfg(feature = "capture")]
pub use capture::{
    CapturedFixture, FrameCapture, REDACT_KEYS, SANITIZER_VERSION, SanitizedFrame, capture_frame,
    frame_mutations, load_captured,
};
pub use connectivity::{
    ConnectivityProbe, DEFAULT_NEUTRAL_ENDPOINTS, DEFAULT_PROBE_TIMEOUT, OutageClass, tcp_reachable,
};
pub use credentials::{load_credentials_from, missing_required_passphrase};
#[cfg(feature = "full")]
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
#[cfg(feature = "full")]
pub use error_kind::{
    CodeTable, ExecErrorPolicy, MsgTable, SubmitDisposition, VenueTaxonomy, classify_venue,
    reject_reason, submit_disposition,
};
pub use event_map::terminal_events;
#[cfg(feature = "full")]
pub use format::{format_to_step, format_to_step_f};
pub use halt::{
    EXE_DIR_FALLBACK_ADVISORY, HALT_FILE, HaltProject, HaltReport, HaltRung,
    declare_project_state_dir, declared_project_state_dir, halt_path_arming_error, halt_path_for,
    halt_path_for_rung, halt_report, resolve_halt_path, resolve_halt_path_rung, sentinel_engaged,
};
pub use json::{json_int, json_num, json_str};
pub use key_permissions::{
    ALLOW_WITHDRAW_KEYS_ENV, KeyPermissionProbe, KeyPermissions, WithdrawGate, allow_withdraw_keys,
    withdraw_gate,
};
pub use klines::kline_to_bar;
pub use mark_stream::{MarkPairings, is_valid_mark, mark_streams_from};
#[cfg(feature = "full")]
pub use market_pump::{
    Keepalive, MarketPumpOpts, PumpBackoff, connect_market_stream, connect_market_stream_via,
    run_market_feed, run_market_session,
};
#[cfg(feature = "full")]
pub use net_probe::{
    DEFAULT_FAILURES_BEFORE_DOWN, DEFAULT_PROBE_HOSTS, DEFAULT_PROBE_INTERVAL, NetProbe,
    NetProbeConfig, NetProbeHandle, NetProbeThread, NetTransition, dns_resolves,
};
pub use pacer::Pacer;
pub use poller::{STOP_POLL_SLICE, StopHandle, sleep_stop_aware, spawn_poller};
#[cfg(feature = "full")]
pub use pump_spec::{MarketPumpSpec, PumpKnobs};
pub use rate_discovery::{WeightBudget, parse_weight_budget};
#[cfg(feature = "full")]
pub use rest::{LiveRestClient, resolve_ambiguous_submit};
pub use retry::{BackoffPolicy, Verdict, retry_rate_limited};
#[cfg(feature = "full")]
pub use signer::{BybitV5Signer, OkxV5Signer, compact_json, iso8601_ms, py_str, urlencode};
pub use sub_ack::SubscribeAck;
#[cfg(feature = "full")]
pub use transport::E_TIMEOUT_AMBIGUOUS;
#[cfg(feature = "full")]
pub use user_data::{
    OpenOutcome, StreamMsg, UserDataAuthError, UserDataFeed, UserDataResyncFeed, UserStream,
    sleep_unless_stopped,
};
pub use venue_passphrase::{PassphraseNeed, venue_passphrase};
#[cfg(feature = "full")]
pub use ws::{AckResult, TungsteniteStream, await_ack, configure_ws_stream, is_timeout};
#[cfg(feature = "full")]
pub use ws_proxy::{WsProxy, connect_ws, ws_target};
