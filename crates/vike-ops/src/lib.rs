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
//! `vike-model` plus the per-platform stop-signal pair — `signal-hook` under `cfg(unix)`, the
//! workspace's only signal surface, and `ctrlc` under `cfg(windows)`, both carried for [`stop`] —
//! which is what lets it sit at tier `leaf` (15) under every daemon and the light CLI alike.
//! (It named `vike-core`/`vike-exec`/`vike-data`/`vike-alerting`/`vike-bridge-core` and more until
//! 2026-09-26; each left with the module that used it, the last being the docs-data exporter, now
//! `crates/vike-docs`.) A module that needs to draw belongs in
//! `vike-app-core`, not here. The second rule follows the first: everything here is **off-fold** —
//! nothing here ever sits in the `vike-core` hot fold that the `p99 < 10µs` gate protects, and the
//! core names no part of this crate. ⚠ It said everything here "consumes the published
//! `CoreSnapshot`/event/journal streams" until 2026-09-28; no module here has consumed any of them
//! since `journal_mat` left on 2026-09-25. The snapshot's readers are `vike-tradehub`'s (its
//! summary, alert mount and observe publisher) and the journal's is `vike_journal::materialize`.
//!
//! Every module below has ONE path: `vike_ops::…`. ⚠ `vike-app-core` re-exported them under their
//! historical `vike_app_core::…` paths until 2026-09-28 (the 2026-09-18 ruling deleted that second
//! name); `::journal_mat::…` and `::alerting::…` had already left it on 2026-09-25 — the modules
//! are `vike_journal::materialize` and the `vike-alerting` crate.
//!
//! - ⚠ `alerting` — **GONE from this crate since 2026-09-25.** It was a verbatim re-export of the
//!   **`vike-alerting`** crate, kept so historical `vike_ops::alerting::…` paths resolved; its last
//!   consumer names that crate directly now. Read it at `crates/vike-alerting/src/lib.rs`: a
//!   persisted rule model (price-cross/level,
//!   indicator threshold, and event/status-driven triggers — fills, order-rejected, drawdown latch,
//!   feed Degraded, recon alert, fill-rate breaker, Polymarket resolution), a PURE evaluator, and a
//!   delivery seam (in-process toast buffer + optional Telegram/webhook POST over the existing
//!   `ureq`+rustls stack, secrets redacted). A strict OFF-FOLD consumer of the published
//!   `CoreSnapshot`/event stream (the `vike_journal::materialize` precedent); inert + byte-identical when no
//!   rules are configured. It moved OUT into its own crate because its rule/delivery/persist half
//!   needs no vike crate at all, and a standalone watchdog binary (page when an external recorder
//!   stops writing) should not link `vike-core` -> `vike-exec` -> `vike-model` to send a message.
//!   No window edits rules: no GUI crate links `vike-alerting`, and the daemon reads its rules from
//!   the `alerts.json` file `crates/vike-tradehub/src/tradehub_cli.rs`'s `alerts_path` names.
//!   ⚠ This said "The egui window that edits rules lives in `vike-app` (an explicit follow-up)"
//!   until 2026-09-28; that follow-up was never built.
//! - ⚠ `feed_status` — **GONE from this crate since 2026-09-23.** It was a verbatim re-export of
//!   **`vike_model::feed_status`**: the pure live-connection model + string classifier
//!   (`ConnectionState`/`parse_feed_status`) each bridge's market feed produces, which
//!   `crates/vike-tradehub/src/reconcile_config.rs`'s `health_from_feed_status` folds into
//!   `ReconHealth`. The module landed here first (down from the egui `vike-connections` crate, so
//!   the headless daemon could reach it without egui), but this crate was only ever the lesser of
//!   two wrong homes: the file had ZERO deps, and the egui side was linking
//!   `vike-core`/`vike-data`/`vike-alerting` through this crate to get at it. It moved down to the
//!   bottom layer on 2026-08-02, and the re-export that kept `vike_ops::feed_status::…` resolving
//!   was deleted on 2026-09-23 with `reconcile_config`, its last user — consumers name
//!   `vike_model::feed_status` directly. ⚠ This bullet described that re-export as present, as an
//!   intra-doc link, until 2026-09-28.
//! - ⚠ `docs_data` — **GONE from this crate since 2026-09-26**, and a crate of its own:
//!   `crates/vike-docs` (the renderer is its `src/lib.rs`, the `docs_data` bin and
//!   `tests/docs_data_gate.rs` moved with it). It renders the CI-gated per-venue capability tables
//!   into the docs-data release assets, and it was the only module here naming anything above the
//!   vocabulary floor — `vike-bridge-core`'s TIF table, the strategy registry, the reconcile
//!   policies, the hist store's layouts — so its departure is what took this crate from rank 50 to
//!   tier `leaf`.
//! - ⚠ `journal_mat` — **GONE from this crate since 2026-09-25**, and living at
//!   `crates/vike-journal/src/materialize.rs`: the
//!   off-path `JournalMaterializer` tail-follows the core's WAL and
//!   materializes its fill/order records into the Tier-2 `kind=exec_fill`/`kind=exec_order`
//!   `HistStore` series `vike-report` reads. Drains on a seconds interval, never the fold;
//!   at-least-once and idempotent.
//! - [`live_lock`] — ONE live process per venue ACCOUNT (split-plane B11): `LiveLock::acquire`
//!   takes an OS advisory lock on `<state_dir>/LIVE-<route_key>.lock` and REFUSES a second process
//!   arming the same account live, so two books cannot rewrite each other through
//!   `PositionDrift`. A PAPER mount claims nothing. `vike-tradehub` claims before it builds the
//!   node; the lock names no vike type, which is what lets it live in a leaf.
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
//!   It is TWO tables — `SETTINGS` and the generated credential key grid, which has a private
//!   module of its own (`settings_grid`) — and [`settings::all_settings`] is the one way to read
//!   both. It lives here rather than in the GUI's crate because it is a WORKSPACE-wide operator
//!   table, and because the rule it enforces ("only binaries read the environment") is an ops rule.
//! - [`shutdown`] — the pure, bounded teardown orchestration (`run_with_deadline`): fan parallel
//!   shutdown tasks out, run a load-bearing sequential tail, and return within one overall deadline
//!   regardless. Extracted from `vike-app`'s `on_exit` (PR #589, the >6s window-close hang) and
//!   reused verbatim by the daemon so a wedged `shutdown_and_join` can never hang exit.
//! - [`stop`] — the other half of that story: **one stop flag, many triggers**. `StopSignal` plus
//!   `install_handlers`, which points SIGTERM/SIGINT at the SAME `AtomicBool` a daemon's stdio stop
//!   word raises, so `systemctl stop` reaches the teardown [`shutdown`] then bounds. Before it,
//!   SIGTERM ran no Rust at all: the recorder never flushed and the trading daemon abandoned its
//!   resting book at the venue. The handler only STORES A BOOL (see that module's doc on
//!   async-signal-safety); on Windows the same flag is raised by `ctrlc`'s console-control-event
//!   handler (and by `arm_stop_file`'s polled sentinel for a detached daemon), and each platform's
//!   bridge dependency (`signal-hook`, `ctrlc`) is gated out of the other's build.

// ⚠ NO feature gates anything here any more. A `full` feature (DEFAULT) gated the OPERATIONAL half
// — everything that named a vike type — so `vike-cli` could take this crate with
// `default-features = false` and read `SETTINGS` for `vike-cli config show` without linking
// vike-core/vike-exec/vike-data. Its last module left on 2026-09-25 and the feature was deleted on
// 2026-09-26 together with `docs-data`, whose module is `crates/vike-docs` now. What is left —
// `settings`, `scan`, `shutdown`, `stop`, `live_lock` — is the whole crate in every build, and it
// is what `vike-cli` always linked; the lightness is structural now rather than a feature being off.

// ⚠ `pub use vike_alerting as alerting;` stood here until 2026-09-25 and is DELETED, not re-pointed.
// It was a continuity re-export for consumers that kept naming `vike_ops::alerting::…`, and by the
// end it had exactly ONE: `vike-tradehub`, which held no direct edge of its own. MEASURED the day
// it went: this crate used the dependency in ZERO lines of code — the `pub use` was the only line
// naming it, and the three `krate: "vike-alerting"` rows in `settings.rs` are registry DATA that
// stay valid whatever this manifest says, because the scanner walks the whole `crates/` tree.
// The daemon names `vike-alerting` directly now, and this crate dropped the dependency, which sat
// under its `full` feature (itself deleted on 2026-09-26) — which also took `ureq` out of its
// graph, the ONE chain `cargo tree -i` reported.

// ⚠ `pub use vike_model::feed_status;` stood here and is DELETED, not re-pointed. Its own note
// named its last user: "including this crate's own `crate::feed_status::…` in
// `reconcile_config`" — and that module left on 2026-09-23. MEASURED the same day: zero callers
// outside this crate, so the continuity it provided was continuity for nobody. `vike-tradehub`
// names `vike_model::feed_status` directly, which is the canonical path and the only one now.

// ⚠ `#[cfg(feature = "docs-data")] pub mod docs_data;` stood here until 2026-09-26. It is
// `crates/vike-docs/src/lib.rs` now — see the module list above for why it left.
// ⚠ `pub mod journal_mat;` stood here until 2026-09-25. It is
// `crates/vike-journal/src/materialize.rs` now. It sat
// here because this crate could name BOTH the WAL and `vike-data`; the WAL became its own crate on
// 2026-09-23, so the module whose whole job is reading it went to live with it.
pub mod live_lock;
// ⚠ A SECOND `#[cfg(feature = "full")]` stood on the next line and is DELETED as a DEFECT, not as
// tidying. It belonged to `pub mod reconcile_config;`, which left on 2026-09-23 — and an attribute
// applies to the next ITEM, not the next line, so with that module gone it had silently landed on
// `pub mod scan;` below and made it `full`-only. Nothing broke and CI stayed green, because the one
// consumer that took this crate `default-features = false` (`vike-cli`) named `settings` and not
// `scan`. A module changing visibility with no caller to notice is exactly the failure a green
// build cannot report. (There is no `full` feature left for an attribute to name: it was deleted
// on 2026-09-26 — see the "NO feature gates anything here" note after the module doc.)
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
// ⚠ It HAS been dropped, and by measurement: `journal_mat` left for `vike-journal` on 2026-09-25
// (#2167), which re-counted zero `vike_core` code sites here and took the edge out of
// `[dependencies]`; since 2026-09-26 this crate names `vike-model` alone. The two paragraphs above
// are kept for their method — they describe an edge that no longer exists.
pub mod scan;
pub mod settings;
mod settings_grid;
pub mod shutdown;
pub mod stop;
