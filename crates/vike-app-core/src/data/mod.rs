//! Market data and history: the market data session, the data sink, datahub resolution and feed, the venue
//! catalog, chart seeding, stored-series loading, the Account window's execution-history read and backfill.

// The datahub verb a Backfill runs through, and (`plan`) the Data Manager's bulk-Backfill job planner.
pub mod backfill;
// The chart key, built and parsed in one file (`series_key`, `venue_of_key`, `venue_bar_instrument`).
pub mod chart_key;
// The EXPLICIT per-venue instrument-catalog refresh: the cache, the budget and the fold, and its
// SECOND route (`server_route`), the client of `Request::VenueCatalog`: the socket and the refusals.
pub mod catalog_refresh;
// The Data Manager's inventory load (the Stored grid's walk) and (`mode`) which store it reads.
pub mod data_manager;
pub mod data_sink;
// The Account window's History read: the datahub walk its History tab's page comes from (a widget opens no
// connection, so the read is here, beside the Data Manager's, whose deadline it runs under).
pub mod execution_history;
// The one datahub-address adoption rule, the dial built from it and the feed retry policy.
pub mod datahub;
// The session that owns the desired set, the two threads and every client-side wire rule, and the
// client half of the datahub MARKET-DATA wire (`data_client`: one `DataClient` per venue slug).
pub mod market_data_session;
// A chart asks the BACKEND'S STORE for the interval it was given, and (`chart_seed`) when that
// store holds NOTHING, asks the server to FETCH it (decision 0058).
pub mod stored;
// The Data Manager's HISTORY column (decision 0102), pure so its text and clamp are CI-tested.
pub mod history_column;
// The Data Manager's split warnings, the datahub's open list read in the background for the Overview.
pub mod split_warnings;
// The user's named symbol lists (the DataSets), read and saved through the datahub.
pub mod symbol_sets;
