//! vike-gatewayhub-client — the LIGHT client half of the vike-gatewayhub venue gateway.
//!
//! # What this crate is
//!
//! `vike-gatewayhub` is the one daemon that opens every connection to every roster venue
//! (`docs/decisions/0164-one-daemon-owns-every-venue-connection-vike-gatewayhub.md`). This crate is
//! how a hub reaches it: the wire protocol the daemon serves, a blocking client, and the REMOTE
//! SEAMS a hub plugs into the traits it already holds — a remote `vike_data::DataClient` for the
//! datahub's feed broker, and remote kline, funding, tick, catalog, options and family sources for
//! its backfill, seed, catalog, options and recorder lanes. It is to the gatewayhub what
//! `vike-datahub-client` is to the datahub: the protocol lives HERE and the daemon imports it, so
//! there is one schema and one client library.
//!
//! # Layout (the `data` feature gates every module)
//!
//! * `proto` — the wire: handshake, the open of a `stream` socket (its frames, one per `LiveDataSink` verb,
//!   are the tube's: `vike_market_stream::wire`), the `rpc` frames (fetch with paging and progress, catalog, options, family,
//!   presence, status), the scope table and the connection constants. The daemon imports it.
//! * `client` — the blocking dial and handshake, and `exchange` for the positional verbs.
//! * `opener` — `GatewayOpener`, how the tube's `RemoteDataClient` (`vike_market_stream::market`, a
//!   `DataClient` over one `stream` socket per broker client) dials and handshakes with this daemon.
//! * `history` — `RemoteKlines`, `RemoteFunding` and the remote dukascopy tick fetch, with the two
//!   rosters the datahub's backfill table folds.
//! * `catalog`, `options`, `resolve`, `credential` — the remaining remote seams.
//!
//! # Two rules that make the remote seams honest
//!
//! * **A transport failure is an `Err`, never an empty answer.** An empty family would unsubscribe
//!   live streams and an empty source would be stored as "the venue had no rows"; every remote seam
//!   maps a dead link to the error type of the seam it implements, naming the gateway.
//! * **A refactor changes no working behaviour.** The in-process error variants
//!   (`SourceError::{Fetch, Refused}`, `LiveDataError::{Subscribe, Unsupported}`, `CatalogError`)
//!   all survive the wire with their inner text unchanged.
//!
//! # What it must NEVER link — the whole point of a LIGHT client
//!
//! * **No venue bridge and no `vike-bridge-core`.** Every bridge is built on the transport and
//!   signing core, so refusing that one crate in this crate's closure refuses every bridge with no
//!   roster written down. The rule is held by NAME in
//!   `crates/vike-ops/tests/architecture/named_run_closure_gate/fences.rs`, whatever the layer
//!   ranks say.
//! * **No `vike-datahub-client`.** The two hub clients sit at one rank over `vike-hub-proto`, so
//!   neither names the other; what this crate needs from the datahub's vocabulary (a credential
//!   presence word) it re-declares, and the datahub maps between the two.
//! * **No `vike-secrets` and no `vike-config`.** Libraries take configuration as parameters; only
//!   binaries read the process environment or the credential store (root `CLAUDE.md`). Hub keys
//!   and addresses arrive as arguments from the caller that holds the store.
//! * **No `tungstenite`, `rustls` or `ureq`.** This crate speaks the hub protocol over
//!   `std::net` and `serde_json` frames, as the other hub clients do, and opens no venue socket:
//!   the venue's sockets are the daemon's.
//!
//! The layer gate (`crates/vike-ops/tests/architecture/layer_gate.rs`) holds the direction of
//! every edge; the named fences hold what no rank can: the user-code hosts and the live core may
//! not reach this crate, and this crate may not reach the simulator or the harness (same file).

#[cfg(feature = "data")]
pub mod catalog;
#[cfg(feature = "data")]
pub mod client;
#[cfg(feature = "data")]
pub mod credential;
#[cfg(feature = "data")]
pub mod history;
#[cfg(feature = "data")]
pub mod opener;
#[cfg(feature = "data")]
pub mod options;
#[cfg(feature = "data")]
pub mod proto;
#[cfg(feature = "data")]
pub mod resolve;
