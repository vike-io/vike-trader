//! vike-datahub-client — the LIGHT client half of the vike-datahub data-service.
//!
//! # What this crate is
//!
//! The DataFusion-free half of vike-datahub: the wire [`proto`]col, the blocking [`DatahubClient`],
//! and [`RemoteHistStore`] — a [`vike_data::HistStore`] whose read verbs are answered by a remote
//! `vike-datahub` server over RPC, so a consumer can read the store WITHOUT linking the
//! Arrow/DataFusion engine.
//!
//! # Why it stays light (the whole point)
//!
//! This crate depends on `vike-data` with **DEFAULT features** — the `HistStore` TRAIT only, NOT
//! `hist-datafusion` — plus `vike-model` + serde. It names NO concrete backend, no `vike-backtest`,
//! no DataFusion/Arrow/Parquet, in every build (`hist-route` included). The heavy engine lives only
//! in `vike-datahub` (the server), behind its `serve-datafusion` feature. The GUI (`vike-desktop`,
//! `vike-studio`) links this crate and no DataFusion crate; the `studio-standalone` lane
//! (`scripts/ci_feature_suite.sh`) asserts that for the Studio with a `cargo tree`.
//!
//! The handshake and the frame codec live in `vike-node-proto`, the crate below both node
//! protocols (`docs/decisions/0107-the-node-protocol-substrate-is-a-crate-below-both-clients.md`);
//! `docs/decisions/0025-datahub-remote-posture.md` is the verdict on the shape of the borrow (one
//! scheme generalized over its domain constant, never a second one).

// Every module but `route` is UNGATED, and the verb vocabularies must be: `proto`'s
// `Request`/`Response` carry their types, and a default `vike-datahub` build must DECODE a verb in
// order to refuse it cleanly.
pub mod archive;
pub mod bind;
pub mod catalog;
pub mod client;
// No root re-exports: `spec`, `accept_value`, `Arity`, `Route` read as nothing without the
// `flag_vocab::` qualifier. Why the vocabulary lives in this crate is the module's own doc.
pub mod flag_vocab;
pub mod history;
pub mod market;
pub mod named_run;
pub mod proto;
pub mod remote;
// Behind `hist-route` because it names `vike-secrets` and `vike-config`, and nine crates take this
// one; the module's own doc carries why it lives here.
#[cfg(feature = "hist-route")]
pub mod route;
pub mod seed;
pub mod wire_studio;

// Root re-exports are the names outsiders use AT THE ROOT; every other item has one spelling, its
// module path. `vike_node_proto::auth::{NodeKeys, Scope}` is never re-exported here (a second name
// for another crate's symbol).
pub use client::{CancelHandle, DatahubClient, MdSubscribedInfo, MdUpdatedInfo};
pub use market::{BookSnapshot, MdBye};
pub use proto::{
    BackfillDone, COMPUTE_PLANE_SENTINEL, DATA_PLANE_SENTINEL, DEFAULT_SEARCH_METHOD, FEATURE_AUTH,
    FEATURE_BACKFILL, FEATURE_BACKFILL_FUNDING, FEATURE_COVERAGE, FEATURE_DELETE_SERIES,
    FEATURE_MARKET_DATA, FEATURE_NAMED_RUN, FEATURE_SCAN_BOOK_UPDATES, FEATURE_SCAN_COHORT,
    FEATURE_SCAN_DEPTH, FEATURE_SCAN_EQUITY, FEATURE_SCAN_EXEC_FILLS, FEATURE_SCAN_LIMIT,
    FEATURE_SCAN_PERP_METRICS, FEATURE_SEARCH_METHOD, FEATURE_SEED_CLASS, FEATURE_SEED_SERIES,
    FEATURE_SERIES_FACTS, FEATURE_STUDY, FEATURE_VENUE_CATALOG, FEATURE_WALKFORWARD_SEARCH,
    PROTO_VERSION, SEARCH_METHODS, STUDIO_RUNNER_SENTINEL, WireSearch, WireStudy,
    advertised_md_venues, md_venue_feature, welcome_plane,
};
pub use remote::RemoteHistStore;
pub use wire_studio::{WireParamscanEntry, WireTrade, WireWfWindow};
