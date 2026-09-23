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
//!   market feed produces, which `crates/vike-tradehub/src/reconcile_config.rs`'s
//!   `health_from_feed_status` folds into
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
//! - ⚠ `reconcile_config` — **GONE from this crate since 2026-09-23.** The bullet is kept as a
//!   departure note rather than deleted: the module is cited from some fifty files across this
//!   tree, and a reader arriving from one of them is better served by being told where it went
//!   than by finding nothing. It is `crates/vike-tradehub/src/reconcile_config.rs` now — the same
//!   pure `VIKE_RECONCILE*` env-parsing seam (`reconcile_enabled` + `build_recon_config` +
//!   `health_from_feed_status`), moved unchanged. It sat HERE on the stated ground that its gate
//!   is "called once by each live root so the GUI and the daemon cannot answer differently".
//!   There is ONE live root since the desktop lost its local core (#1610), and MEASURED on the day
//!   it moved it had exactly ONE code caller — that daemon's own CLI — so the module that was
//!   shared was shared with nobody.
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

// ⚠ `pub use vike_model::feed_status;` stood here and is DELETED, not re-pointed. Its own note
// named its last user: "including this crate's own `crate::feed_status::…` in
// `reconcile_config`" — and that module left on 2026-09-23. MEASURED the same day: zero callers
// outside this crate, so the continuity it provided was continuity for nobody. `vike-tradehub`
// names `vike_model::feed_status` directly, which is the canonical path and the only one now.

#[cfg(feature = "docs-data")]
pub mod docs_data;
#[cfg(feature = "full")]
pub mod journal_mat;
pub mod live_lock;
#[cfg(feature = "full")]
// ⚠ `pub mod reconcile_config;` stood here until 2026-09-23. It is
// `crates/vike-tradehub/src/reconcile_config.rs` now, because the reason it was SHARED stopped
// being true: the gate was "called once by each live root so the GUI and the daemon cannot answer
// differently", and there is ONE live root since the desktop lost its local core (#1610). It had
// exactly one code caller, in that daemon's own CLI.
// ⚠ An earlier draft of this note ended "Its departure is also what takes `vike-core` and
// `vike-exec` out of this crate's graph." That is FALSE as written, and it was never measured.
// MEASURED 2026-09-23, after the move: `journal_mat` names `vike_core` at 12 CODE sites and
// `vike_exec` at four of its own, and `docs_data` names `vike_exec` too — so both edges stay
// exactly where they were. What this move changes is ONE module's home, not this crate's
// dependency closure.
// ⚠ It is worth saying what WOULD change it, because the answer is close and a later reader
// will ask: all twelve of those `vike_core` sites are `vike_core::journal::…`, and a sibling branch
// moves that module into its own `vike-journal` crate. With both landed, `journal_mat` reads
// `vike_journal::` and the ONLY other `vike_core` CODE use in this crate was `reconcile_config`'s
// `use vike_core::{ReconConfig, ReconHealth}` — which is exactly what left here. So the vike-core
// edge does become droppable, by the two changes TOGETHER and by neither alone. Do not drop it on
// the strength of this note: measure the tree you are standing in.
pub mod scan;
pub mod settings;
pub mod shutdown;
pub mod stop;
