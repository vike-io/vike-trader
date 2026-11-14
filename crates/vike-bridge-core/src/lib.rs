//! The shared plumbing every venue bridge is built from: blocking REST transport + signers, the
//! venue-neutral WS user-data pump with reconnect/backoff/keepalive/resync, the ExecutionClientActor command
//! thread, the pinned Decimal wire-format site, the venue-adapter contract, and the generic
//! `ws`/`rest`/`klines` fragments (`WebsocketSessionUserStreamTungstenite`/`WsSocket`, the `VenueRest`/`ExecutionClientRest<R>`
//! REST-exec seam, `kline_to_bar`) every sibling venue reaches into. Depends on vike-model,
//! vike-secrets, vike-execution (lanes/seams) and vike-data (the start contract's `PropertiesRecorder`).
//!
//! The credential loaders (the absent-credentials-is-the-live-gate authority), the account
//! directory, the passphrase table and the key-permission gate are NOT here: they live in
//! `vike-secrets` (`vike_secrets::credentials`, `::account_directory`, `::venue_passphrase`,
//! `::key_permissions`) since decision 0135, so a consumer that needs only them links no transport
//! stack. The HALT sentinel is not here either: its path half is `vike_boot::halt`, its engaged
//! check `vike_execution::halt::sentinel_engaged`. This crate has no `full` feature and no
//! default-features-off configuration: every build of it is the whole crate.

// What a venue said about an account's currency, and the currency the owner's ruling assumes when it said none.
pub mod account_currency;
#[cfg(feature = "capture")]
pub mod capture;
// The committed-fixture checks every captured-wire replay suite runs; capture + test-support only.
#[cfg(all(feature = "capture", feature = "test-support"))]
pub mod capture_replay;
// The ONE catalog read loop: the tolerant read list and the paged listing walk (each venue's reads,
// parsers, positions, ceiling and log lines as parameters).
pub mod catalog_read;
// Bounded backfill concurrency over ONE shared `pacer`. std::thread only — no async, no rayon.
pub mod concurrent;
// The ACTIVE confirm's ONE form: dispatch ledger, lookup window, verdict, closure.
pub mod confirm;
// The read-once cell and the never-printed wrapper a credentialed lane keeps a credential in.
pub mod connection_memo;
pub mod connectivity;
// The planted settings database and the captured log lines a credentialed lane's tests read, on both
// sides of the gatewayhub's crate boundary; test-support only.
#[cfg(any(test, feature = "test-support"))]
pub mod credential_fixture;
// The credentialed kline lane's ONE contract: each venue's spec in its bridge, the engine in the gatewayhub.
pub mod credentialed_klines;
// The market-data client over a bridge's SHARED source (one connection or process-wide source carries
// every symbol: dukascopy's bus, fxcm's desk); each venue's words and repeat-ask rule are its own
// bridge's `DataClientBusSpec`/`DataClientBusSpecHold` impl.
pub mod data_client;
pub mod depth;
#[cfg(feature = "eip712")]
pub mod eip712;
pub mod error_kind;
pub mod event_map;
pub mod exec_actor;
// The shared execution client's parts under their final names: the actor's hooks, the orders-held client.
pub mod execution_client;
pub mod exit;
// The rolling-family lane's ONE contract: each venue's spec in its bridge, the engine in the gatewayhub.
pub mod family;
pub mod format;
// No FX venue logs in while the FX market is closed: the process's clock over `vike_venues::session`'s FX week,
// OFF until the gatewayhub arms it. Each bridge's login and retry ladder asks it.
pub mod fx_login;
// A registry-backed market feed's one status writer, its DOM emission and the affinity-pinned registry
// its threads spawn on. Names vike-data's sink and registry.
pub mod feed_ctx;
// The classified-frame vocabulary (data / subscribe ack / subscribe reject / ignored) every
// subscribe-only market-data lane shares. Its payloads are vike-model types, plus the one
// map onto `market_pump`'s `WebsocketSessionFrameOutcome`.
pub mod feed_frame;
// Strict borrowing JSON scan blocks for the binance-family and bybit typed frame decode. std + serde +
// vike-model.
pub mod frame_scan;
pub mod http;
pub mod json;
pub mod klines;
// The ONE pure rule for the account leverage a perp exec arm POSTs at startup, from the operator's
// `[risk]` budget (`ProfileRisk` data) or the venue's own default. Here, not in each bridge, so the
// perp arms cannot drift from each other nor from the RiskGate's `initial_margin_requirement`. It names
// `vike_model::ProfileRisk` and `tracing`, nothing of the transport half.
pub mod leverage;
// The READ half of the same number (decision 0182): the read-only `LeverageReader` a posting bridge
// fills in, the pre-start `probe` and the bounded post-start `confirm`, over an injected clock.
// Additive: nothing in `leverage` changes. No transport: each bridge reads through its own client.
pub mod leverage_read;
pub mod link_history;
pub mod mark_stream;
// The ONE pure rule for the slippage band a venue with NO native market order prices its emulated
// market orders at; `leverage`'s sibling, resolved once at the start by a pure, bounded fn.
pub mod market_slippage;
pub mod net_probe;
// The oldest-first rule a hedging venue's close planner takes the facing positions by (ctrader, fxcm); the venue's
// keys and sizes are parameters.
pub mod oldest_first;
// The options lane's ONE contract: each venue's spec in its bridge, the engine in the gatewayhub.
pub mod options;
// A venue's order-status words as ONE table shape, and the one reading of a word no table names.
pub mod order_status;
// Pace a paged backfill against a DISCOVERED budget (`WeightBudget`, which the venue's bridge reads off the venue).
// Pure: no clock, no sleep.
pub mod pacer;
// One account engine, many symbols: the router binance, aster, bybit and okx sign each order on its
// own symbol through (each venue keeps its own refusal line and departures).
pub mod per_symbol;
// The leverage-rule and fixed-delay-pager pins the perp bridges run over their own numbers; test-support only.
#[cfg(any(test, feature = "test-support"))]
pub mod policy_pins;
pub mod poller;
// The one memory of a converting venue's rate reads (each venue's timings are its parameters), and the
// one answer a converting bridge gives for a symbol.
pub mod quote_rate;
pub mod ratelimit;
// The reconcile clients' admitted-symbol list, its logged forwarding, and the judgement of one admitted
// read with the pass-wide quiet period after a rate limit (each venue's facts and texts as parameters).
pub mod reconcile_admitted;
pub mod rest;
pub mod retry;
// The tick lane's ONE contract: each venue's spec in its bridge, the engine in the gatewayhub.
pub mod ticks;
// Scripted WS stream doubles (WebsocketSessionUserStream/WebsocketSessionMarketStream/WebsocketSessionDepthStream) for tests: in-crate unit tests
// via `cfg(test)`, the crate's own `tests/` and downstream crates via the `test-support` feature.
// Never in a normal build.
#[cfg(any(test, feature = "test-support"))]
pub mod scripted;
// The peer every bridge's `dial_bound` test dials; test-support only.
#[cfg(any(test, feature = "test-support"))]
pub mod silent_peer;
// Exact-decimal arithmetic for the live fine-tick smokes of four bridge crates; test-support only.
#[cfg(any(test, feature = "test-support"))]
pub mod grid_decimal;
// The fake venue loop the HALT test binaries drive an `ExecutionClientActor` against; test-support only.
#[cfg(any(test, feature = "test-support"))]
pub mod fake_venue;
// The venue error-code tables' one baseline-disposition pin; test-support only.
#[cfg(any(test, feature = "test-support"))]
pub mod error_table;
// One command through the real `run_loop` and what the lane said; test-support only.
#[cfg(any(test, feature = "test-support"))]
pub mod execution_drive;
// Decision 0175's order-client-id-map repro book, invariant and scenarios; test-support only.
#[cfg(any(test, feature = "test-support"))]
pub mod order_client_id_repro;
// The `ConfirmByOrderClientId` scenarios every by-order-client-id confirm answers alike; test-support only.
#[cfg(any(test, feature = "test-support"))]
pub mod confirm_pins;
// The golden grid every spot venue walks its base-coin fee rule over; test-support only.
#[cfg(any(test, feature = "test-support"))]
pub mod spot_fee_pins;
// The bars the binance, bybit and okx committed kline fixtures map to; test-support only.
#[cfg(any(test, feature = "test-support"))]
pub mod kline_fixture;
// The decoder totality harnesses' JSON generator kit (proptest); test-support only.
#[cfg(feature = "test-support")]
pub mod json_arb;
// The unit-test pins every venue's live market feed ran over its own wrapper, and the data-client
// engine's own suite runs; test-support only.
#[cfg(any(test, feature = "test-support"))]
pub mod market_feed_pins;
// `no_public_call!`/`no_signed_call!`, the offline `RestTransport` doubles' never-run arms; test-support only.
#[cfg(any(test, feature = "test-support"))]
pub mod rest_stub;
// A scripted signed REST wire (per-path answer queues and a call log); test-support only.
#[cfg(any(test, feature = "test-support"))]
pub mod scripted_rest;
pub mod side;
// The venue-neutral half of request signing: the `Signer`/`SignerJson` seams, the HMAC helpers, the pair encoders,
// `VenueClock` and `KeyClock`. Each venue's signer is its bridge's.
pub mod signer;
// The venue-neutral fill wait and restore-planner pass the bridges' live smokes share; test-support only.
#[cfg(any(test, feature = "test-support"))]
pub mod smoke;
pub mod stream_health;
pub mod sub_ack;
// What a bridge says about a symbol it is asked to admit (the refusal kinds, the account-listed venues' fences), below
// the gatewayhub that words it for the node.
pub mod symbol_properties;
pub mod transport;
pub mod trigger;
// The typed-frame oracle harnesses' venue-neutral half (proptest); test-support only.
#[cfg(feature = "test-support")]
pub mod typed_frame_kit;
// THE VENUE ADAPTER CONTRACT (docs/decisions/0096).
pub mod venue_adapter;
// Its test double, never in a normal build — the `scripted` shape.
#[cfg(any(test, feature = "test-support"))]
pub mod venue_adapter_fixture;
// The one reading of an account's path to its venue (`trade health`'s `gateway-link` row), and its cell.
pub mod venue_link;
pub mod websocket_proxy;

pub mod websocket;
pub mod websocket_session;

// The crate-root names consumers spell at the root (every other item is named at its module path,
// never through a second root name).
pub use event_map::terminal_events;
pub use mark_stream::{MarkPairings, is_valid_mark, is_valid_trade, mark_streams_from};
pub use net_probe::{
    DEFAULT_PROBE_INTERVAL, NetProbe, NetProbeConfig, NetProbeHandle, NetProbeThread,
};
pub use rest::resolve_ambiguous_submit;
pub use signer::urlencode;
pub use sub_ack::WebsocketSessionSubscribeAck;
pub use websocket_session::market::WebsocketSessionMarketKeepalive;
