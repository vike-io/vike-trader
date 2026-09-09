//! The shared plumbing every venue bridge is built from: blocking REST transport + signers, the
//! venue-neutral WS user-data pump with reconnect/backoff/keepalive/resync, the ExecActor command
//! thread, credentials (the absent-credentials-is-the-live-gate authority), and the pinned
//! Decimal wire-format site. Depends on vike-exec (lanes/seams) + vike-model only. Extracted out
//! of the venue-adapter tree ahead of every per-venue bridge crate (crate-reorg Phase 3, spec
//! D4/D6). `ws`/`rest`/`klines` (PR A) add the fragments
//! that physically lived inside binance/bybit but were reached into by every sibling venue:
//! `TungsteniteStream`/`WsSocket`, the `VenueRest`/`LiveRestClient<R>` REST-exec seam, and
//! `kline_to_bar`.

// The `full` feature (DEFAULT — see Cargo.toml's own note) gates the TRANSPORT/SIGNING half: the 14
// modules below that name a venue socket, an HTTP agent, a signer or the execution lanes, and with
// them the ureq / tungstenite / hmac / sha2 / hex / base64 / rust_decimal / vike-exec deps. What is
// left ungated is this crate's PURE half — std + vike-model + serde_json + tracing — and the reason
// the seam exists at all is `credentials`: it is the workspace's ONE `.env` authority
// (`load_workspace_dotenv` / `parse_dotenv`) in twelve std-only lines, and `vike-cli config show`
// must be able to reach that ONE definition without linking a transport stack to do it.
//
// Every gate is a single `#[cfg]` here, on a module declaration or its re-export. NO module body is
// conditional, and no module moved: the split is a property of the dependency graph that was
// already true, now merely declared.

#[cfg(feature = "capture")]
pub mod capture;
// Bounded CONCURRENCY for a paged backfill: split the WINDOW (never the page grid), run each venue's
// own unmodified pager over a span, and hand every lane its dispatch slot out of ONE shared `pacer`,
// so the venue's budget is still enforced in aggregate. std::thread only — no async, no rayon.
pub mod concurrent;
pub mod connectivity;
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
// The ONE pure rule for the account leverage a perp exec arm POSTs at startup: the operator's
// `[risk] max_leverage` when set, the venue's own historical literal when not. Lives HERE (not in
// each bridge) so the four perp arms cannot drift from each other, nor from the RiskGate's
// `im_requirement`, which is derived from the very same number.
//
// ⚠ `full`-gated because it names `vike_exec::ProfileRisk`, and `vike-exec` is optional under that
// feature. It was UNGATED when it landed (#1046), one PR after the feature split (#1042) made
// vike-exec optional — so `main` could not build `-p vike-cli` or
// `-p vike-bridge-core --no-default-features` at all. Neither PR's CI caught it: a WORKSPACE build
// unifies features, so `full` is always on there, and nothing gated the standalone path. The
// `no-default-features` CI lane added alongside this fix is what makes the gap visible.
#[cfg(feature = "full")]
pub mod leverage;
pub mod mainnet;
pub mod mark_stream;
#[cfg(feature = "full")]
pub mod market_pump;
// The ONE pure rule for the slippage band a venue with NO native market order prices its emulated
// market (and stop-market) orders at. Sibling of `leverage` above, and here for the same reason:
// the number reaches a live order path, so it is resolved once at the mount by a pure, bounded fn.
pub mod market_slippage;
#[cfg(feature = "full")]
pub mod net_probe;
// Pace a paged backfill against a DISCOVERED budget (`rate_discovery`) and the venue's own observed
// used-weight counter, so no per-request weight is ever hardcoded. Pure: no clock, no sleeping.
pub mod pacer;
pub mod poller;
#[cfg(feature = "full")]
pub mod pump_spec;
// Read a venue's OWN published request-weight budget out of the `exchangeInfo` body the catalog
// already downloads, instead of hand-transcribing it into a const. Feeds `ratelimit`'s gates.
pub mod rate_discovery;
pub mod ratelimit;
#[cfg(feature = "full")]
pub mod rest;
pub mod retry;
// Shared scripted WS stream doubles (UserStream/MarketStream/DepthStream) for tests — testing-arch
// Phase 4c. In-crate unit tests see it via `cfg(test)`; the crate's own `tests/` and downstream
// crates enable the `test-support` feature as a dev-dependency. Never in a normal build.
// (`full` is redundant with `test-support`, which implies it, but not with the bare `test` arm.)
#[cfg(all(feature = "full", any(test, feature = "test-support")))]
pub mod scripted;
#[cfg(feature = "full")]
pub mod signer;
pub mod stream_health;
pub mod sub_ack;
pub mod tif;
#[cfg(feature = "full")]
pub mod transport;
pub mod trigger;
#[cfg(feature = "full")]
pub mod user_data;
// The per-venue passphrase-requirement table `credentials` gates on. UNGATED like `credentials`
// itself: it is the loader's own authority, so a `--no-default-features` consumer that can resolve
// credentials must be able to resolve them CORRECTLY.
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
pub use credentials::{
    Credentials, Environment, load_credentials_from, load_workspace_dotenv,
    load_workspace_dotenv_from, missing_required_passphrase, parse_dotenv,
};
#[cfg(feature = "full")]
pub use depth::{BookOp, SessionOutcome, infer_tick_size, run_depth_feed, run_depth_session};
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
    EXE_DIR_FALLBACK_ADVISORY, HALT_FILE, HALT_FILE_ENV, HaltProject, HaltReport, HaltRung,
    declare_project_state_dir, declared_project_state_dir, halt_path_arming_error, halt_path_for,
    halt_path_for_rung, halt_path_from_env, halt_report, resolve_halt_path, resolve_halt_path_rung,
};
// The DECISION half, which `halt` re-exports from vike-exec — see the gate on that `pub use` for
// why it may not be named in a default-features-off build.
#[cfg(feature = "full")]
pub use halt::HALT_REJECT_REASON;
pub use json::{json_int, json_num, json_str};
pub use key_permissions::{
    ALLOW_WITHDRAW_KEYS_ENV, KeyPermissionProbe, KeyPermissions, WithdrawGate, allow_withdraw_keys,
    withdraw_gate,
};
pub use klines::kline_to_bar;
pub use mark_stream::{
    MarkPairings, is_valid_mark, mark_streams_enabled, mark_streams_enabled_for,
};
#[cfg(feature = "full")]
pub use market_pump::{
    FrameOutcome, Keepalive, MarketPumpOpts, MarketStream, PumpBackoff, connect_market_stream,
    connect_market_stream_via, run_market_feed, run_market_feed_on, run_market_session,
};
#[cfg(feature = "full")]
pub use net_probe::{
    DEFAULT_FAILURES_BEFORE_DOWN, DEFAULT_PROBE_HOSTS, DEFAULT_PROBE_INTERVAL, NetProbe,
    NetProbeConfig, NetProbeHandle, NetProbeThread, NetTransition, dns_resolves,
};
pub use pacer::Pacer;
pub use poller::{STOP_POLL_SLICE, StopHandle, sleep_stop_aware, spawn_poller};
#[cfg(feature = "full")]
pub use pump_spec::{MarketPumpSpec, PumpKnobs, market_pump_spec};
pub use rate_discovery::{WeightBudget, parse_weight_budget};
#[cfg(feature = "full")]
pub use rest::{LiveRestClient, VenueRest, resolve_ambiguous_submit};
pub use retry::{BackoffPolicy, Verdict, retry_rate_limited};
#[cfg(feature = "full")]
pub use signer::{
    BinanceHmacSigner, BybitV5Signer, OkxV5Signer, PreparedRequest, Signer, compact_json,
    iso8601_ms, py_str, urlencode,
};
pub use stream_health::{HealthEvent, StreamHealth};
pub use sub_ack::SubscribeAck;
pub use tif::{TifOutcome, venue_tif};
#[cfg(feature = "full")]
pub use transport::{E_TIMEOUT_AMBIGUOUS, ErrorKind, RestTransport, UreqTransport, VenueApiError};
#[cfg(feature = "full")]
pub use user_data::{
    OpenOutcome, StreamError, StreamMsg, UserDataAuthError, UserDataFeed, UserDataResyncFeed,
    UserStream, run_user_data_forever, run_user_data_forever_with_idle, sleep_unless_stopped,
};
pub use venue_passphrase::{PassphraseNeed, venue_passphrase};
#[cfg(feature = "full")]
pub use ws::{AckResult, TungsteniteStream, await_ack, configure_ws_stream, is_timeout};
#[cfg(feature = "full")]
pub use ws_proxy::{WsProxy, connect_ws, ws_target};
