//! vike-datahub-client — the LIGHT client half of the vike-datahub data-service.
//!
//! # What this crate is
//!
//! The DataFusion-free half of vike-datahub: the wire [`proto`]col, the blocking [`DatahubClient`],
//! and [`HistoryStoreDatahub`] — a [`vike_data::HistoryStore`] whose read verbs are answered by a remote
//! `vike-datahub` server over RPC, so a consumer can read the store WITHOUT linking the
//! Arrow/DataFusion engine.
//!
//! It is also home to the desktop's DATAHUB POLL (`tool_feeds::poll`, behind the off-by-default
//! `tool-feeds-poll` feature that only the GUI crates turn on): the one poll that asks the
//! backend's datahub for the News, Calendar and Options windows' data, over the one dial
//! ([`dial::DatahubDial`]) the desktop's datahub clients share, and the one venue-listing fetch
//! ([`catalog_fetch`]) the Options index and the desktop's catalog refresh ask through.
//!
//! # Why it stays light (the whole point)
//!
//! This crate depends on `vike-data` — the `HistoryStore` TRAIT only, never `vike-store` — plus
//! `vike-model` + serde. It names NO concrete backend, no `vike-backtest`,
//! no DataFusion/Arrow/Parquet, in every build (`hist-route` included). The heavy engine lives only
//! in `vike-datahub` (the server), behind its `datafusion-serve` feature. The GUI (`vike-desktop`,
//! `vike-studio`) links this crate and no DataFusion crate; the `studio-standalone` lane
//! (`scripts/ci_feature_suite.sh`) asserts that for the Studio with a `cargo tree`.
//!
//! The handshake and the frame codec live in `vike-hub-protocol`, the crate below both node
//! protocols (`docs/decisions/0107-the-node-protocol-substrate-is-a-crate-below-both-clients.md`);
//! `docs/decisions/0025-datahub-remote-posture.md` is the verdict on the shape of the borrow (one
//! scheme generalized over its domain constant, never a second one).

// Every module but `route`, `link` and `tool_feeds::poll` is UNGATED, and the verb vocabularies must be: `proto`'s
// `Request`/`Response` carry their types, and a default `vike-datahub` build must DECODE a verb in
// order to refuse it cleanly.
pub mod archive;
pub mod archive_fetch;
pub mod bind;
pub mod catalog;
pub mod catalog_fetch;
pub mod client;
pub mod dial;
pub mod execution_stream;
pub mod family;
pub mod gateway_links;
pub mod gateway_status;
pub mod history;
// The held connection and the link-failure streak the datahub poll (`tool_feeds::poll`) keeps; the poll's
// only, so behind the poll's feature.
#[cfg(feature = "tool-feeds-poll")]
mod link;
pub mod market;
pub mod options;
pub mod proto;
pub mod refusals;
pub mod remote;
// Behind `hist-route` because it names `vike-secrets` and `vike-config`, and nine crates take this
// one; the module's own doc carries why it lives here.
#[cfg(feature = "hist-route")]
pub mod route;
pub mod seed;
pub mod strat;
// The negotiation suites' one fake peer, compiled once here instead of once per binary. Behind
// `test-support`, which only the self dev-dependency turns on.
#[cfg(feature = "test-support")]
pub mod test_support;
pub mod tool_feeds;
pub mod upload;

// Root re-exports are the names outsiders use AT THE ROOT; every other item has one spelling, its
// module path. `vike_hub_protocol::auth::{HubKeys, Scope}` is never re-exported here (a second name
// for another crate's symbol).
pub use client::{DatahubClient, MarketDataSubscribedInfo, MarketDataUpdatedInfo};
pub use market::{BookSnapshot, MarketDataBye};
pub use proto::{
    BackfillDone, FEATURE_BACKFILL, FEATURE_BACKFILL_FUNDING, FEATURE_BACKFILL_MARKET_RULES,
    FEATURE_BACKFILL_OPTION_TRADES, FEATURE_BAR_SOURCE, FEATURE_COVERAGE, FEATURE_DELETE_SERIES,
    FEATURE_MARKET_DATA, FEATURE_OPTIONS_CHAIN, FEATURE_SCAN_BOOK_UPDATES, FEATURE_SCAN_COHORT,
    FEATURE_SCAN_DEPTH, FEATURE_SCAN_EQUITY, FEATURE_SCAN_EXECUTION_FILLS, FEATURE_SCAN_LIMIT,
    FEATURE_SCAN_PERP_METRICS, FEATURE_SEED_CLASS, FEATURE_SEED_SERIES, FEATURE_SERIES_FACTS,
    FEATURE_SOURCE, FEATURE_TOOL_FEEDS, FEATURE_VENUE_CATALOG, PROTO_VERSION,
};
pub use remote::HistoryStoreDatahub;
