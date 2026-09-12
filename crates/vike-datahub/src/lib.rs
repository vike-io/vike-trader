//! vike-datahub — the DATA daemon: the hist store, served over the node protocol.
//!
//! # What this crate is
//!
//! A headless server for a `vike_data::HistStore`. A client sends a small typed request over a
//! localhost TCP socket and gets back the ANSWER — bars, ticks, a symbol-properties row, a catalog
//! page, a coverage report — never a raw upstream slice, and never the whole store. It also serves
//! the two verbs that CHANGE the store: `Backfill` (fetch a range in) and `DeleteSeries` (take one
//! out). This crate owns NO engine and no collector: it is assembly of crates that already exist.
//!
//! ⚠ **It used to serve the COMPUTE verbs too, and ruling 7 took them away**
//! (`docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` §0). `RunBacktest`,
//! `RunSlice`, `RunSweep`, `RunWalkforward`, `RunSweepProfile`, `RunWalkforwardProfile` and
//! `ListStrategies` are served by **`vike-backend backtest --addr`**
//! (`vike_backtest::compute_server`) — one word for backtesting, on the crate responsible for it.
//! Both daemons run on the same box against the same store root, so compute still sits next to
//! data; what changed is that a runaway sweep and the market-data reads no longer share one
//! `MemoryMax`, one restart and one address.
//!
//! A compute verb arriving here is answered with a named `Response::Error` — never a drop — and the
//! `Welcome` handshake does not advertise those verbs, so a client that reads it never sends one.
//! `crates/vike-datahub/tests/plane_split.rs` drives both halves.
//!
//! # ⚠ …and since ruling 10, it FILLS the store as well as serving it
//!
//! `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` §0.5 merged
//! `vike-recorder`'s daemon into this one: *"the recorder OWNS venue subscriptions and has no
//! network surface at all; the data server has the wire and no feeds. One process should own venue
//! connections, the store, and serving."* [`recorder`] is that plane — the retired daemon body,
//! behind the `record` feature — and `vike-recorder` is now the LIBRARY it is assembled from.
//! Nothing about the assembly claim changes: this crate still owns no engine and no venue logic.
//!
//! ⚠ The `record` feature is what keeps a serving-only build honest. Without it this crate names
//! `vike-recorder` nowhere, links no bridge, and a `--record` flag is a startup error naming the
//! feature — never a daemon that comes up healthy and accumulates nothing.
//!
//! # Transport
//!
//! Blocking [`std::net`] (`TcpListener` / `TcpStream`), thread-per-connection, with length-prefixed
//! `serde_json` frames ([`vike_datahub_client::proto`]). NO async runtime and NO second
//! HTTP/RPC/WS stack — the workspace `deny.toml` bans one, and this stays inside bare `std::net` +
//! `serde_json`. The COMPUTE daemon speaks the identical protocol over the identical handshake:
//! one schema, one client library, two served surfaces.
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
//! [`DatahubClient`](vike_datahub_client::DatahubClient), the `RemoteHistStore` (a `HistStore`
//! served over this protocol) and — since ruling 7 — the plane/scope tables and the bind guard BOTH
//! daemons enforce live in the LIGHT `vike-datahub-client` crate. That crate is DataFusion-free, so
//! the GUI can link it without the Arrow/DataFusion tree, and it is the ONE home of the wire
//! schema: every consumer (this server included) imports `vike_datahub_client` directly — no
//! re-export here. This crate keeps the data server: the [`serve`] loop + the shipped binary.

pub mod server;

// ⚠ RULING 10: the RECORDING plane — venue feeds in, Parquet out, into the very store `server`
// answers from. This is `vike-recorder`'s retired daemon body; `vike-recorder` itself is now the
// library it is assembled from. Behind the `record` feature because the venue bridges it mounts
// drag k256/keccak/tungstenite/rustls, and a data server that records nothing must not link them —
// the same argument the recorder's own venue features have always made, one level up.
#[cfg(feature = "record")]
pub mod recorder;

// The backfill-on-demand collector seam (split-plane REQ-9): the venue → collector table
// `serve_with_backfill` mounts. The TYPE is feature-free (a `Box<dyn Fn>` table — no collector
// name in any signature) so the server dispatches through it on every build; only
// `backfill::real_backfill_table` — the constructor naming the `vike-backfill` collectors — is
// behind `backfill-serve`, which is what keeps the default build's tree collector-free.
pub mod backfill;

// ⚠ `wire_run` is GONE — it MOVED to `vike-studio-core` (`crates/vike-studio-core/src/wire_run.rs`)
// when ruling 7 took the compute verbs off this daemon. It was the DTO↔studio boundary for
// `RunSlice`/`RunSweep`/`RunWalkforward`, and it was the sole place this crate named
// `vike-studio-core`; with the verbs served by `vike-backend backtest --addr`, keeping the boundary
// here would have left the data crate holding the compute plane's conversions and its
// `vike-studio-core`/`vike-script` edges for nobody. It went to the crate that owns the types it
// converts INTO, which is also the only crate below the compute daemon's mount point that can name
// both those types and the wire DTOs.

// The `vike-datahub` CLI as a library function, so the bin and the `vike` multicall dispatcher
// reach one copy. Not feature-gated: BOTH cfg arms live inside, so a feature-off build still gets
// its `--help` and its rebuild hint from the same place.
pub mod datahub_cli;

// The LIVE MARKET-DATA plane (the datahub market-data wire design, §5–§7): the hub that owns every
// venue subscription, its sink, its mailbox and its venue table.
//
// ⚠ NOT feature-gated, and §8 item 4 specified that it would be. The TYPE appears in
// `server::serve_authed`'s public signature, so a `#[cfg]` on it would force one at every call site
// AND on `handle_connection`'s `MdSubscribe` arm — and cfg-ing THAT away breaks leg (3) of
// `FEATURE_MARKET_DATA`'s contract, under which a build without the plane must still DECODE the
// verb and answer a clean `Response::Error`. It is the `BackfillTable` rule two paragraphs up,
// applied again: only `md::venues::real_market_venue_table` — the one function naming a bridge —
// sits behind `live-feeds`, and it costs nothing because `pub mod live` is UNGATED in `vike-data`
// and this crate already takes that crate with default features.
pub mod md;

// ⚠ `VerbScope`/`required_scope` are NOT re-exported here any more: they MOVED to
// `vike_datahub_client::proto`, which is the crate BELOW both daemons, so the compute server
// (`vike_backtest::compute_server`, layer 50) enforces the identical table this one does. Import
// them from there — no `pub use` shim, per this workspace's convention on a code MOVE.
pub use server::{serve, serve_authed, serve_with_backfill};
