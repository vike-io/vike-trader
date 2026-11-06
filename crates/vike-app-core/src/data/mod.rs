//! Market data and history: the md session, the data sink, datahub resolution and feed, the venue
//! catalog, chart seeding, stored-series loading and backfill.

pub mod backfill_plan;
pub mod backfill_route;
pub mod backfill_wire;
// The EXPLICIT per-venue instrument-catalog refresh: the cache, the budget and the fold.
pub mod catalog_refresh;
// ...and its SECOND route, the client of `Request::VenueCatalog`: the socket and the refusals.
pub mod catalog_wire;
pub mod data_sink;
// The client half of the datahub MARKET-DATA wire: one `DataClient` per venue slug...
pub mod datahub_feed;
pub mod datahub_resolve;
// The Tearsheet's EXECUTION HISTORY: the account's fills and orders out of the datahub's store.
pub mod exec_history;
// ...and the session that owns the desired set, the two threads and every client-side wire rule.
pub mod md_session;
// A chart asks the BACKEND'S STORE for the interval it was given.
pub mod store_bars;
// ...and when that store holds NOTHING, asks the server to FETCH it (decision 0058).
pub mod chart_seed;
// The Data Manager's HISTORY column (decision 0102), pure so its text and clamp are CI-tested.
pub mod history_column;
pub mod stored_load;
pub mod stored_mode;
