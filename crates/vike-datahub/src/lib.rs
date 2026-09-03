//! vike-datahub — Phase 1 of the vike data-service ("thin-client GUI" architecture).
//!
//! # What this crate is
//!
//! A headless, COMPUTE-TO-DATA backtest server (Design B). A client sends a small
//! [`BacktestProfile`](vike_backtest::harness::BacktestProfile) CONFIG over a localhost TCP socket;
//! the server runs the EXISTING `vike-backtest` engine right next to the data and returns just the
//! [`BacktestReport`](vike_backtest::harness::BacktestReport) ANSWER. The heavy history never
//! crosses the wire — only the profile in and the metrics summary out. This crate owns NO engine:
//! it is assembly of crates that already exist (`vike-backtest` for the engine/config/report,
//! `vike-data` for the store seam).
//!
//! # Transport
//!
//! Blocking [`std::net`] (`TcpListener` / `TcpStream`), thread-per-connection, with length-prefixed
//! `serde_json` frames ([`vike_datahub_client::proto`]). NO async runtime and NO second
//! HTTP/RPC/WS stack — the workspace `deny.toml` bans one, and this stays inside bare `std::net` +
//! `serde_json`.
//!
//! # Backend-agnostic
//!
//! The server holds the store as `Arc<dyn HistStore + Send + Sync>` (the `vike-data` trait seam),
//! so the same [`serve`] runs over `DataFusionHist` in production and over the in-memory
//! `MemHistStore` double in tests. The concrete DataFusion backend the shipped binary opens lives
//! behind the `serve-datafusion` feature (see the crate manifest).
//!
//! # Where the protocol lives (Phase 2 split)
//!
//! The wire [`proto`](vike_datahub_client::proto)col, the thin
//! [`DatahubClient`](vike_datahub_client::DatahubClient), and the `RemoteHistStore` (a `HistStore`
//! served over this protocol) live in the LIGHT `vike-datahub-client` crate — that crate is
//! DataFusion-free, so the GUI can link it without the Arrow/DataFusion tree, and it is the ONE
//! home of the wire schema: every consumer (this server included) imports `vike_datahub_client`
//! directly — no re-export here. This crate keeps the server: the [`serve`] loop + the shipped
//! binary.
//!
//! # Phased plan
//!
//! - Phase 1: compute-to-data backtest over localhost TCP (this crate's server).
//! - Phase 2: split the proto + client into `vike-datahub-client` and add its `RemoteHistStore` — a
//!   `HistStore` impl that reads over RPC, so an EXISTING local consumer can point at remote data.
//! - Phase 3 (future): the GUI thin-client — the desktop app reads history (and runs backtests)
//!   through this seam instead of embedding the engine + data locally.

pub mod server;

// The backfill-on-demand collector seam (split-plane REQ-9): the venue → collector table
// `serve_with_backfill` mounts. The TYPE is feature-free (a `Box<dyn Fn>` table — no collector
// name in any signature) so the server dispatches through it on every build; only
// `backfill::real_backfill_table` — the constructor naming the `vike-backfill` collectors — is
// behind `backfill-serve`, which is what keeps the default build's tree collector-free.
pub mod backfill;

// PR-3: the `RunSlice` boundary (DTO↔studio conversions + the local run). ONLY compiled under
// `serve-datafusion`, because it is the sole place this crate names `vike-studio-core` (which pulls
// DataFusion). A default build compiles none of it — `server.rs`'s `RunSlice` arm answers with a
// clean error instead. Re-exported so the parity test drives the SAME `run_slice_local` the server
// arm does (keeping "local" and "over the wire" one computation).
#[cfg(feature = "serve-datafusion")]
pub mod wire_run;
// The `vike-datahub` CLI as a library function, so the bin and the `vike` multicall dispatcher
// reach one copy. Not feature-gated: BOTH cfg arms live inside, so a feature-off build still gets
// its `--help` and its rebuild hint from the same place.
pub mod datahub_cli;

pub use server::{required_scope, serve, serve_authed, serve_with_backfill, VerbScope};

#[cfg(feature = "serve-datafusion")]
pub use wire_run::{
    run_error_to_wire, run_slice_local, run_sweep_local, run_walkforward_local, to_data_slice,
    to_engine_params, to_strategy_spec, to_wire_result, to_wire_sweep_result,
    to_wire_walkforward_result,
};
