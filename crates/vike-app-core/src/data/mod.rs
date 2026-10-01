//! Market data and history: the md session, the data sink, datahub resolution and feed, the venue
//! catalog, chart seeding, stored-series loading and backfill.

pub mod backfill_plan;
pub mod backfill_route;
pub mod backfill_wire;
// The EXPLICIT per-venue instrument-catalog refresh — the cache `vike_catalog::persist` was
// designed around, the per-venue stamp its doc promised the Data Manager would show, and the
// control that triggers one. Here rather than in the shell for the reason every decision in this
// crate is: `xtask/src/ci/tables.rs`'s `EXCLUDE_FROM_CI` names the GUI binary, so a refresh spelled
// there would be gated by nothing.
pub mod catalog_refresh;
// ...and its SECOND route: the client of `Request::VenueCatalog`, the Observe verb by which this
// binary asks the backend's datahub to list a venue whose bridge it does not link
// (`docs/decisions/0062`). Separate from `catalog_refresh` because the split is real — that module
// owns the cache, the budget and the fold; this one owns the socket and the refusal sentences —
// and because the wire half is the part a test can drive with no cache and no thread.
pub mod catalog_wire;
pub mod data_sink;
// The client half of the datahub MARKET-DATA wire (design §9). `datahub_feed` is one
// `vike_data::DataClient` per venue slug; `md_session` owns the desired set, the two background
// threads and every rule the wire's §6.3/§7.3/§7.4 place on a client. They live HERE, not in the
// shell, for the reason everything else did: `xtask/src/ci/tables.rs`'s `EXCLUDE_FROM_CI` names
// `vike-desktop`, and a thread-and-socket state machine nobody runs is a state machine nobody has.
pub mod datahub_feed;
pub mod datahub_resolve;
pub mod md_session;
// THE CHART ASKS THE BACKEND'S STORE for the interval it was given — the read that lets a
// 5m/15m/1h/4h/1d chart paint at all. The daemon streams only what its mounted strategies hold
// (the CI box trades 1m), so every other interval in the menu was a permanently empty pane until this
// existed. Pure planning + a one-thread read, both CI-tested against a real `HistStore` double.
pub mod store_bars;
// ...and when that store holds NOTHING, the chart asks the server to FETCH it — the other half of
// the same defect, and the half `store_bars`' own module doc names as belonging elsewhere. A thin
// caller by construction: every bound on the fetch is the SERVER's, so what lives here is one plan
// rule, one blocking Observe-scope call, and one sentence naming the switch an operator must set.
// `docs/decisions/0058-a-chart-gap-fetch-is-an-observe-verb.md` is why a WRITE verb is reachable
// from the observe key this binary holds.
pub mod chart_seed;
pub mod stored_load;
pub mod stored_mode;
