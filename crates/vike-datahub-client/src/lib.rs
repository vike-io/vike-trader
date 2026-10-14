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
//! This crate depends on `vike-data` — the `HistStore` TRAIT only, never `vike-store` — plus
//! `vike-model` + serde. It names NO concrete backend, no `vike-backtest`,
//! no DataFusion/Arrow/Parquet, in every build (`hist-route` included). The heavy engine lives only
//! in `vike-datahub` (the server), behind its `datafusion-serve` feature. The GUI (`vike-desktop`,
//! `vike-studio`) links this crate and no DataFusion crate; the `studio-standalone` lane
//! (`scripts/ci_feature_suite.sh`) asserts that for the Studio with a `cargo tree`.
//!
//! The handshake and the frame codec live in `vike-hub-proto`, the crate below both node
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
pub mod history;
pub mod market;
pub mod named_run;
pub mod options;
pub mod proto;
pub mod remote;
// Behind `hist-route` because it names `vike-secrets` and `vike-config`, and nine crates take this
// one; the module's own doc carries why it lives here.
#[cfg(feature = "hist-route")]
pub mod route;
pub mod seed;
pub mod tool_feeds;
pub mod wire_studio;

// Root re-exports are the names outsiders use AT THE ROOT; every other item has one spelling, its
// module path. `vike_hub_proto::auth::{HubKeys, Scope}` is never re-exported here (a second name
// for another crate's symbol).
pub use client::{CancelHandle, DatahubClient, MdSubscribedInfo, MdUpdatedInfo};
pub use market::{BookSnapshot, MdBye};
pub use proto::{
    BACKTEST_PLANE_SENTINEL, BackfillDone, DATA_PLANE_SENTINEL, FEATURE_AUTH, FEATURE_BACKFILL,
    FEATURE_BACKFILL_FUNDING, FEATURE_COVERAGE, FEATURE_DELETE_SERIES, FEATURE_MARKET_DATA,
    FEATURE_NAMED_RUN, FEATURE_OPTIONS_CHAIN, FEATURE_SCAN_BOOK_UPDATES, FEATURE_SCAN_COHORT,
    FEATURE_SCAN_DEPTH, FEATURE_SCAN_EQUITY, FEATURE_SCAN_EXEC_FILLS, FEATURE_SCAN_LIMIT,
    FEATURE_SCAN_PERP_METRICS, FEATURE_SEARCH_METHOD, FEATURE_SEED_CLASS, FEATURE_SEED_SERIES,
    FEATURE_SERIES_FACTS, FEATURE_STUDY, FEATURE_TOOL_FEEDS, FEATURE_VENUE_CATALOG,
    FEATURE_WALKFORWARD_SEARCH, PROTO_VERSION, STUDIO_RUNNER_SENTINEL, WireSearch, WireStudy,
    advertised_md_venues, md_venue_feature, welcome_plane,
};
pub use remote::RemoteHistStore;
pub use wire_studio::{WireParamscanEntry, WireTrade, WireWfWindow};
