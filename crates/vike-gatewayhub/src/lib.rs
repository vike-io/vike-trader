//! vike-gatewayhub — the GATEWAY daemon: the one process that owns every venue connection.
//!
//! # What this crate is
//!
//! Two processes on one box used to open venue connections, each counting only its own traffic
//! (`docs/decisions/0164-one-daemon-owns-every-venue-connection-vike-gatewayhub.md` has the
//! measurements). This daemon is the single owner: it opens the market-data sockets, the venue REST
//! of backfill, catalog, chart seed and options polling, and the order and execution sessions. Its
//! request budget ([`budget`]) sees order-class commands alone. The datahub and
//! the tradehub open none; they reach a venue only through `vike-gatewayhub-client`, whose wire
//! protocol this crate imports rather than declares (one schema, one client library, as
//! `vike-datahub` imports `vike-datahub-client`'s). The design is
//! `docs/superpowers/specs/2026-10-13-vike-gatewayhub-design.md`.
//!
//! # Status
//!
//! The daemon serves. The SERVER is real: a listener, the always-keyed handshake, the scopes, the
//! caps, the per-session bounded queues and the stop, serving the wire `vike-gatewayhub-client`
//! declares and calling two seams for everything that touches a venue. The venue bridges are optional:
//! the `data` feature links their feeds, and with it the two seams have their real venue lanes
//! (`data::venues::DataLaneBridge`, `rest::lane::RestLaneBridge`); the `trade` feature adds their exec halves. The COMMAND LINE
//! ([`gatewayhub_cli`]) boots through `vike-boot`, arms the stop handlers, reads the venue settings
//! rows, declares Polymarket's egress, applies the loopback-only bind guard and hands a
//! [`gatewayhub_cli::Ready`] to the wire hook, which requires the hub keys, builds the lanes, binds
//! exactly the checked addresses and serves until the stop. A build without the `data` feature links
//! no bridge and has no venue lane, so its binary says so and exits 2 rather than binding a socket
//! and answering nothing.
//!
//! A build with the `trade` feature also serves the order and execution sessions (`trade`, wired to the
//! socket by `server::trade`): one lease per account, a per-account event ring, the HALT gate and the
//! live-account lock, over the venue half in `trade::adapters` (`TradeLaneBridge`, the real `TradeLane`). The hub keys are read by
//! `gatewayhub_cli`'s `keys` module, and the datahub dials this daemon through the client crate for every
//! venue call.
//!
//! # The modules
//!
//! * [`gatewayhub_cli`] — `run(vars, cwd, args)`, the daemon's entry point as a library function, and
//!   the ordered phases it is made of (the module doc is the run-order contract).
//! * [`budget`] — one counter per venue, host and scope, fed from the `vike-venues` rate-limit
//!   tables. It refuses nothing (`budget::mode`'s `MODE`), and the trade dispatch is the only lane that
//!   calls `Budget::spend`, so it counts orders alone.
//! * `server` — the listener: clients over `vike-hub-protocol`, scopes, the feature strings a client
//!   negotiates, the back-pressure contract and the loopback-only bind guard.
//! * `registry` — the venue tables: which lane serves which venue on the data side, and with the
//!   `trade` feature which bridge starts each venue (`registry::trade`).
//! * `session` — what a connection holds (`ConnectionMemos`, where a credentialed history lane keeps a
//!   connection's source and the read-once memo inside it), and the one supervised reconnect
//!   (`session::reconnect`), whose only user is IBKR.
//! * `data` — the seam to the venue clients and their sockets (`DataLane`); the wire sink that carries
//!   every sink event across the hop without blocking a venue pump is the server's.
//! * `rest` — the seam to the venue calls of backfill, catalog, chart seed and the Deribit options
//!   poll (`RestLane`). FETCH only: commit keys and every store write stay in the datahub (decision
//!   0084).
//! * `trade` (feature `trade`) — the order and execution sessions: `TradeLane`, the seam to the venue half, and
//!   the hub, leases, rings and dispatch built on it. It names nothing of `server`; the wire adapter is
//!   `server::trade`.
//!
//! # What it must NEVER link
//!
//! `vike-store`, `vike-backfill`, `vike-recorder` and `vike-tradehub-core`, and never the datahub or the
//! tradehub: the daemon is a venue owner, not a store writer and not a trading root. Held by NAME in
//! `crates/vike-ops/tests/architecture/named_run_closure_gate/fences.rs`'s `GATEWAYHUB_FENCE`, in
//! addition to the layer gate (`crates/vike-ops/tests/architecture/layer_gate.rs`) refusing an edge
//! to another daemon.
//!
//! # Rules the daemon inherits
//!
//! A refactor changes no working behaviour: what the datahub did against a venue, this daemon does,
//! identically. Libraries take configuration as parameters and only binaries read the process
//! environment or the credential store (root `CLAUDE.md`). Only the gatewayhub reads venue
//! credentials, and a credential value never reaches a log line, a reply or `Debug`.

pub mod budget;
pub mod data;
pub mod gatewayhub_cli;
pub mod health;
// The refusals this daemon raises and records itself (its module doc); the wire's are the client crate's.
pub mod refusals;
// The one writer of the venue-session history (its module doc). Needs the bridges' report call, so `data`.
#[cfg(feature = "data")]
pub(crate) mod link_writer;
pub mod registry;
pub mod rest;
pub mod server;
pub mod session;
// The settings database's check at the start and its nightly copy (its module doc). Started by the wire hook, so `data`.
#[cfg(feature = "data")]
pub(crate) mod store_backup;
// The venue addresses' seeding at the start and their daily probe (its module doc). The probe dials,
// so `data`, like `link_writer`.
#[cfg(feature = "data")]
pub(crate) mod url_probe;
// The order and execution sessions (`trade/mod.rs`'s module doc); the wire adapter is `server/trade.rs`.
#[cfg(feature = "trade")]
pub mod trade;
// The scripted lanes and the loopback gateway other crates' tests dial (`testkit`'s module doc). Never in
// a normal build; `any(test, feature = "test-support")` is the one spelling the CI planner reads as a
// `test-support` module (`xtask/src/ci/graph.rs`'s `feature_test_module`).
#[cfg(any(test, feature = "test-support"))]
pub mod testkit;
