//! vike-datahub-client — the LIGHT client half of the vike-datahub data-service (Phase 2 of the
//! "thin-client GUI" architecture).
//!
//! # What this crate is
//!
//! The DataFusion-free half of vike-datahub: the wire [`proto`]col, the blocking [`DatahubClient`],
//! and [`RemoteHistStore`] — a [`vike_data::HistStore`] whose GUI-relevant READ verbs are answered
//! by a remote `vike-datahub` server over RPC. It was split out of `vike-datahub` (Phase 1) so the
//! GUI can talk to the data-service WITHOUT linking the Arrow/DataFusion engine.
//!
//! # Why it stays light (the whole point)
//!
//! This crate depends on `vike-data` with **DEFAULT features** — the `HistStore` TRAIT only, NOT
//! `hist-datafusion` — plus `vike-model` + serde. It names NO concrete backend, no `vike-backtest`,
//! no DataFusion/Arrow/Parquet. `RemoteHistStore` implements the trait purely by RPC, so a consumer
//! that links this crate (the GUI, in Phase 3) inherits none of that weight. The heavy engine lives
//! only in `vike-datahub` (the server) and behind its `serve-datafusion` feature.
//!
//! # Phased plan
//!
//! - Phase 1 (`vike-datahub`): the compute-to-data backtest SERVER over localhost TCP.
//! - Phase 2 (this crate): split the proto + client out, and add [`RemoteHistStore`] so an EXISTING
//!   local `HistStore` consumer can point at remote data.
//! - Phase 3 (future): the GUI thin-client drops its DataFusion link and reads through this seam.
//!
//! # ⚠ The shared node-auth primitive LEFT this crate on 2026-09-23
//!
//! It lives in `vike-node-proto` now, beside the frame codec, and this section used to argue that
//! it belonged here: *"`vike-tradehub-client` (layer 50) already re-exports this crate's (layer 30)
//! frame codec rather than growing a second one, and auth is the same class of primitive."* The
//! second half of that sentence was right and is why BOTH moved together. The first half was
//! circular — `vike-tradehub-client` was at 50 *because* those two things lived here, since they
//! were the whole of its only `vike-*` edge.
//!
//! What is left of the old argument, unchanged and still true: the primitive adds no transport and
//! no I/O, `hmac`/`sha2` are the crates `vike-bridge-core`'s venue signer already links, and the
//! keys arrive as a caller-supplied map. `docs/decisions/0025-datahub-remote-posture.md` is still
//! the verdict on the SHAPE of the borrow — one scheme generalized over its domain constant, never
//! a second implementation — and that verdict is what this move preserves rather than disturbs.
//! `vike_node_proto::auth`'s own doc carries the measurement that moved it.

pub mod bind;
// The VENUE-CATALOG vocabulary — the bounds and the outcome enum both ends of
// `Request::VenueCatalog` need. Declared UNGATED for the identical reason `market` below is: a
// default `vike-datahub` build must DECODE the verb in order to refuse it cleanly, and this crate
// has no `[features]` table to gate it with.
pub mod catalog;
pub mod client;
// The FLAG VOCABULARY the `backtest` verb's two argv parsers share — one row per flag spelling, its
// arity, its value roster, and which route accepts it. It lives HERE for the reason
// [`proto::SEARCH_METHODS`] already argues in its own doc: this crate is a normal dependency of both
// `vike-cli` (which spelling-checks before a dial or a spawn) and `vike-backtest` (which implements
// what the flags select), so it is the one place below both where a roster can sit. Declared UNGATED
// and dependency-free on exactly the terms `market` states below: `const` data plus pure functions
// over `&str`, no environment, no I/O, nothing heavy for a feature to gate.
//
// ⚠ Deliberately NOT re-exported at the crate root, unlike every module above and below it. Its
// names — `spec`, `accept_value`, `Arity`, `Route` — read as nothing without the `flag_vocab::`
// qualifier, and a bare `spec` at the root of a WIRE-PROTOCOL crate would actively mislead. The
// root re-exports elsewhere in this file all name a type or a const that survives losing its
// module (`BookSnapshot`, `NodeKeys`, `SEARCH_METHODS`); these do not.
pub mod flag_vocab;
// The MARKET-DATA vocabulary (the datahub market-data wire design, §4.4). Declared UNGATED and
// dependency-free: this crate has no `[features]` table and must not grow one — three functions in
// `proto` are shared by TWO daemons, a default `vike-datahub` build must DECODE the verbs in order
// to refuse them cleanly, and `crates/vike-ops/tests/feature_lane_coverage.rs` would demand a whole
// CI lane for a feature that gates nothing heavy.
pub mod market;
// The NAMED-RUN vocabulary — the bounds, the request DTO and the outcome enum both ends of
// `Request::RunNamed` need (`docs/decisions/0064-a-named-run-carries-no-source.md`). Declared
// UNGATED for the same reason `catalog` and `market` above are: a default build must DECODE the
// verb in order to refuse it cleanly.
//
// ⚠ That reason used to end *"and this crate has no `[features]` table to gate it with"*, which
// stopped being true on 2026-09-23 when `route` below arrived with one. The argument survives
// the table intact — a verb a default build cannot decode is a verb it cannot REFUSE cleanly,
// which is a property of the protocol rather than of how many knobs the manifest has. The
// absence was never the reason; it was an extra sentence that happened to be true.
pub mod named_run;
pub mod proto;
pub mod remote;
// WHERE a reader gets history from — the local files or this crate's own wire client. Behind
// `hist-route` because its LOCAL arm opens a `DataFusionHist`, and nine crates take this one:
// see the module's own doc for why the decision lives here and what the feature buys them.
#[cfg(feature = "hist-route")]
pub mod route;
pub mod seed;
pub mod wire_studio;

pub use bind::{BindDecision, BindExposure, ServerAuth, bind_decision, bind_exposure};
pub use client::{DatahubClient, MdSubscribedInfo, MdUpdatedInfo};
pub use market::{
    BookSnapshot, MD_DEPTH_LEVELS_CEILING, MD_DEPTH_LEVELS_DEFAULT, MD_HEARTBEAT, MD_READ_TIMEOUT,
    MdBye, MdFrame, MdLane, MdRefusal, MdSessionId, MdSpec, WireStreamStatus,
};
pub use named_run::{
    NAMED_RUN_INTERVALS, NAMED_RUN_MAX_BARS, NAMED_RUN_MAX_PARAMS, NamedParam, NamedRoster,
    NamedRunOutcome, NamedRunRefusal, NamedRunSpec, named_run_bars, validate_named_run,
};
// ⚠ `pub use node_auth::{NodeKeys, Scope};` stood here and is DELETED rather than re-pointed at
// `vike_node_proto`. Re-exporting it would be a second name for a symbol that now lives in another
// crate — the alias shim the 2026-09-18 ruling forbids, and the "the consumer cannot name the
// canonical crate" exemption does not apply: `vike-node-proto` is layer 15 and every consumer of
// this one clears it by thirty rungs or more. The ten files that used the short spelling say
// `vike_node_proto::auth::NodeKeys` now.
pub use proto::{
    BackfillDone, DEFAULT_SEARCH_METHOD, DeleteDone, FEATURE_AUTH, FEATURE_BACKFILL,
    FEATURE_COVERAGE, FEATURE_DELETE_SERIES, FEATURE_MARKET_DATA, FEATURE_MD_VENUE_PREFIX,
    FEATURE_NAMED_RUN, FEATURE_SCAN_BOOK_UPDATES, FEATURE_SCAN_COHORT, FEATURE_SCAN_DEPTH,
    FEATURE_SCAN_EQUITY, FEATURE_SCAN_EXEC_FILLS, FEATURE_SCAN_LIMIT, FEATURE_SCAN_PERP_METRICS,
    FEATURE_SEARCH_METHOD, FEATURE_SEED_CLASS, FEATURE_SEED_SERIES, FEATURE_SERIES_FACTS,
    FEATURE_STUDY, FEATURE_VENUE_CATALOG, FEATURE_WALKFORWARD_SEARCH, MAX_FRAME_LEN, PROTO_VERSION,
    Plane, Request, Response, SEARCH_METHODS, SeedDone, VerbScope, WireSearch, WireStudy,
    advertised_md_venues, md_venue_feature, plane_of, read_frame, read_frame_raw,
    read_frame_raw_capped, request_kind, required_scope, scope_admits, write_frame,
    wrong_plane_message,
};
pub use remote::RemoteHistStore;
pub use seed::{
    SEED_BARS, SEED_INTERVALS, SEED_MAX_SYMBOL_BYTES, seed_range, validate_seed_interval,
    validate_seed_symbol,
};
pub use wire_studio::{
    NO_WINDOW_SEARCH, WireCostModel, WireEngineParams, WireParamscan, WireParamscanEntry,
    WireParamscanResult, WireRunError, WireRunResult, WireSlice, WireSliceKind, WireSpec,
    WireTrade, WireWalkforward, WireWalkforwardResult, WireWfWindow, WireWindowSearch,
};
