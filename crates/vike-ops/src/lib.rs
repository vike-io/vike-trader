//! `vike-ops` — the headless-usable **off-fold operations layer**.
//!
//! Everything here was extracted verbatim from `vike-app-core` (plus one file from
//! `vike-connections`) for ONE dependency reason: `vike-tradehub`, the headless daemon that runs on
//! the production trading box, used exactly three of these modules and paid for the ENTIRE egui
//! widget stack to get them — `egui`, `egui_plot`, `epaint`, `epaint_default_fonts`, `vike-chart`,
//! `vike-panels`, `vike-cockpit`, `vike-ui-theme`, `vike-connections`, `vike-data-manager` — through
//! that single edge. The cost is not CI minutes; it is daemon build time, binary size, and the
//! `cargo deny` supply-chain audit surface on the box that signs real orders.
//!
//! So the rule for this crate is a hard one: **no egui, ever.** Its whole dependency set is
//! `vike-core` / `vike-model` / `vike-exec` / `vike-data` (DEFAULT features — the `HistStore` trait,
//! no DataFusion) / `vike-alerting`, plus indexmap/tracing, `vike-bridge-core`'s PURE half +
//! serde_json for the `docs-data` feature (see [`docs_data`] — no transport stack rides that
//! edge), and one `cfg(unix)` package —
//! `signal-hook`, the workspace's only signal surface, carried for [`stop`]. A module that needs to draw belongs in
//! `vike-app-core`, not here. The second rule follows the first: everything here is **off-fold** —
//! it consumes the published `CoreSnapshot`/event/journal streams and never sits in the `vike-core`
//! hot fold that the `p99 < 10µs` gate protects.
//!
//! `vike-app-core` re-exports every module below under its historical path, so existing
//! `vike_app_core::journal_mat::…` / `::alerting::…` / `::settings::…` call sites resolve unchanged.
//!
//! - [`alerting`] — a verbatim re-export of the **`vike-alerting`** crate (with its `core` feature
//!   on — the only one it has now), so every historical `vike_ops::alerting::…` /
//!   `vike_app_core::alerting::…` path still resolves: a persisted rule model (price-cross/level,
//!   indicator threshold, and event/status-driven triggers — fills, order-rejected, drawdown latch,
//!   feed Degraded, recon alert, fill-rate breaker, Polymarket resolution), a PURE evaluator, and a
//!   delivery seam (in-process toast buffer + optional Telegram/webhook POST over the existing
//!   `ureq`+rustls stack, secrets redacted). A strict OFF-FOLD consumer of the published
//!   `CoreSnapshot`/event stream (the [`journal_mat`] precedent); inert + byte-identical when no
//!   rules are configured. It moved OUT into its own crate because its rule/delivery/persist half
//!   needs no vike crate at all, and a standalone watchdog binary (page when an external recorder
//!   stops writing) should not link `vike-core` -> `vike-exec` -> `vike-model` to send a message.
//!   The egui window that edits rules lives in `vike-app` (an explicit follow-up), not here.
//! - [`feed_status`] — a verbatim re-export of **`vike_model::feed_status`**: the pure
//!   live-connection model + string classifier (`ConnectionState`/`parse_feed_status`) each bridge's
//!   market feed produces, which [`reconcile_config`]'s `health_from_feed_status` folds into
//!   `ReconHealth`. It landed here first (down from the egui `vike-connections` crate, so the
//!   headless daemon could reach it without egui), but this crate was only ever the lesser of two
//!   wrong homes: the file is 142 lines with ZERO deps, and the egui side was linking
//!   `vike-core`/`vike-data`/`vike-alerting` through this crate to get at them. It now sits at the
//!   bottom layer, which costs neither consumer anything; the re-export keeps every
//!   `vike_ops::feed_status::…` path resolving.
//! - [`docs_data`] — the docs-data exporter (behind the default-on `docs-data` feature): renders
//!   the CI-gated per-venue capability tables (`vike_model::VENUES` and the registries over it,
//!   plus `vike_bridge_core::tif::venue_tif`) into the `venues.json`/`stats.json` release assets
//!   the website/docs consume, and declares [`docs_data::WIRING`] — the one docs-facing per-venue
//!   axis no other table encodes. The `docs_data` bin is its file-writing shell;
//!   `tests/docs_data_gate.rs` gates roster- and schema-completeness in-process.
//! - [`journal_mat`] — the off-path `JournalMaterializer`: tail-follows the core's WAL and
//!   materializes its fill/order records into the Tier-2 `kind=exec_fill`/`kind=exec_order`
//!   `HistStore` series `vike-report` reads. Drains on a seconds interval, never the fold;
//!   at-least-once and idempotent.
//! - [`reconcile_config`] — the pure `VIKE_RECONCILE*` env-parsing seam: `reconcile_enabled` +
//!   `build_recon_config` (the `vike_core::ReconConfig` builder) + `health_from_feed_status` (the
//!   feed-status → `vike_core::ReconHealth` map the mounted `ReconManager` consults). All helpers
//!   take a `&HashMap<String, String>` (a testability seam); the BINARY reads the REAL process env
//!   (`std::env::vars()`) into that map at the mount site — env reads stay in the binary.
//! - [`scan`] — the pure source scanner behind the settings-registry gate: resolves every
//!   `env::var`/`env::var_os` call site and `vars.get("NAME")` map lookup in a file's text against
//!   its `const` table. String in, data out (no filesystem); the filesystem walk itself lives in
//!   this crate's `tests/settings_registry.rs`, not here.
//! - [`settings`] — `SETTINGS`: the verbatim, machine-gated registry of every environment
//!   variable this workspace reads (STEP 1 of the settings program) — DATA, not behavior; nothing
//!   in the runtime consults it. `tests/settings_registry.rs` asserts it against the real tree.
//!   It lives here rather than in the GUI's crate because it is a WORKSPACE-wide operator table,
//!   and because the rule it enforces ("only binaries read the environment") is an ops rule.
//! - [`shutdown`] — the pure, bounded teardown orchestration (`run_with_deadline`): fan parallel
//!   shutdown tasks out, run a load-bearing sequential tail, and return within one overall deadline
//!   regardless. Extracted from `vike-app`'s `on_exit` (PR #589, the >6s window-close hang) and
//!   reused verbatim by the daemon so a wedged `shutdown_and_join` can never hang exit.
//! - [`stop`] — the other half of that story: **one stop flag, many triggers**. `StopSignal` plus
//!   `install_handlers`, which points SIGTERM/SIGINT at the SAME `AtomicBool` a daemon's stdio stop
//!   word raises, so `systemctl stop` reaches the teardown [`shutdown`] then bounds. Before it,
//!   SIGTERM ran no Rust at all: the recorder never flushed and the trading daemon abandoned its
//!   resting book at the venue. The handler only STORES A BOOL (see that module's doc on
//!   async-signal-safety); on Windows it is a documented no-op and the `signal-hook` dependency is
//!   `cfg(unix)`-gated out.

// The `full` feature (DEFAULT, see Cargo.toml's own note) gates the OPERATIONAL half — everything
// that names a vike type. What is left ungated is `settings` (the workspace-wide operator table),
// `scan` (its pure text scanner), `shutdown` — all three std-only — and `stop`, which is std plus
// one `cfg(unix)` package and names no vike type either. That is what lets
// `vike-cli` — the deliberately light, DataFusion-free thin client — take this crate with
// `default-features = false` to read `SETTINGS` for `vike-cli config show` without linking
// vike-core/vike-exec/vike-data (and, until its `workspace-env` feature was deleted along with the
// credential-store read behind it, vike-alerting's edge onto the venue transport stack). Every
// pre-existing consumer takes default features and is unchanged.

// Continuity re-export (the `reconcile_config` / #864 pattern): the alerting tree now lives in its
// own `vike-alerting` crate, but every consumer keeps naming it `vike_ops::alerting::…`.
#[cfg(feature = "full")]
pub use vike_alerting as alerting;

// Continuity re-export (same pattern as `alerting` above): `feed_status` moved DOWN to `vike-model`
// — it is 142 dependency-free lines, and its egui consumer was paying for this crate's whole
// `vike-core`/`vike-data`/`vike-alerting` closure to reach them. Every `vike_ops::feed_status::…`
// path (including this crate's own `crate::feed_status::…` in `reconcile_config`) resolves unchanged.
// Ungated: vike-model is not optional, precisely because this re-export costs nothing.
pub use vike_model::feed_status;

#[cfg(feature = "docs-data")]
pub mod docs_data;
#[cfg(feature = "full")]
pub mod journal_mat;
pub mod live_lock;
#[cfg(feature = "full")]
pub mod reconcile_config;
pub mod scan;
pub mod settings;
pub mod shutdown;
pub mod stop;
