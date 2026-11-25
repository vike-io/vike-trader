//! `backend_connection` — RUNTIME-MUTABLE backend selection for the observe GUI (split-plane B1): one
//! [`BackendConnection`] per live connection, [`connect_backend`] to build it from a
//! [`BackendRecord`], and [`switch_backend`] — the
//! safety-critical BACKEND-SESSION CLEAR that keeps backend A's data from ever painting under
//! backend B's connection. (B1 shipped it as a total clear; B2 split the surface into the backend
//! session, cleared on every switch, and the local feed plane — venue truth the third mode ran
//! beside the session until the `fat` build went — which survives a switch untouched, pinned by
//! test. ⚠ B2 also wrote that plane a teardown, `teardown_feed_plane`; no runtime path ever called
//! it, so it was deleted with its tests on 2026-10-24.)
//!
//! Before B1 the observe connection was a process-lifetime constant: `--observe ADDR` wired one
//! bridge + one optional control channel in `vike-app`'s `App::new` and nothing could ever change
//! it. This module lifts that wiring out of the shell's `main.rs` so every decision runs in a gate:
//! the shell (`vike-app` then, `vike-desktop` now) was compile-checked but never tested, so
//! anything in its `main.rs` was untestable by construction. (It still sits outside the derived
//! roster; the `app-check` job has since grown a nextest step, but it runs only on a PR whose plan
//! reaches the shell, while this crate's tests ride the roster.)
//!
//! - **One active backend at a time** (the spec's one-active-backend render model). Switching
//!   replaces the whole [`BackendConnection`]; there is no per-backend series keyspace (deferred I15),
//!   so a switch must CLEAR AND REFOLD rather than re-key.
//! - **Every dial signs with this desktop's DEVICE key** ([`registry::device_key`], decision 0288):
//!   the record holds no key and no key name, a desktop without a device key sends nothing, and an
//!   unarmed record (B9) can never mount a control channel.
//! - **The process-level master gate stays** — the `flags.tradehub_control` row of the PC's own
//!   settings database, which the desktop hands in as `master_control` — as an AND over the
//!   per-backend arming. Defense in depth for a write path: the record file (`backends.json`) is
//!   GUI-owned and editable by anything that can write the profile directory, so a record flipping
//!   `control = true` must not be sufficient on its own to arm REAL order placement; the operator's
//!   own opt-in in that database is still required, exactly as it was when the flag was the ONLY
//!   gate. (It was the `VIKE_TRADEHUB_CONTROL=1` variable until decision 0111.)
//! - **The fat local-core path was untouched**: `switching_available` answered `false` while a
//!   local core ran, and a process started without `--observe` and without a configured backend
//!   behaved byte-identically to before B1. Mixing planes was the THIRD MODE (B2, fat +
//!   `--observe`, `BackendMode::ObserveWithFeeds`): a remote backend's account plane beside local
//!   venue feeds, with NO local core — so switching stayed available there, and the switch left
//!   the feed plane alone. ⚠ Both arms went with the `fat` build on 2026-09-09 (#1727) and the
//!   third mode's variant was deleted from `split_plane` in 2026-10: the desktop has no local
//!   core, so switching is always available and there is no venue-fed plane for the switch to
//!   preserve. (This bullet spoke of both in the present tense until 2026-09-28;
//!   `switching_available`, a constant `true` from then on, has been deleted.)

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::backend::observe_bridge::{self, BackendObserveBridgeHandle};
use crate::backend::registry::{self, BackendRecord, BackendRegistry};
use crate::backend::split_plane;
use crate::data::data_sink::{L2BookStore, StoredBarStore, TickStore};
use crate::data::datahub::feed_retry::DatahubFeedRetries;
use crate::data::market_data_session::DataClientMap;
use crate::ui::feed_lifecycle::L2BookSubscriptions;
use vike_chart::chart::state;
use vike_model::feed_status::{DataClientConnectionState, DataClientStatus};
use vike_orderflow::bar_agg::OrderflowAgg;
use vike_orderflow::tickvol::TickVolAgg;
use vike_tradehub_client::liveness::LinkPatience;

/// One live backend connection: the record it was dialed from, the observe bridge, and the
/// optional Scope::Write write channel.
///
/// Dropping the whole struct is a full teardown: [`BackendObserveBridgeHandle`]'s `Drop` raises the stop flag
/// and JOINS the reconnect thread (B6), and `TradehubControlHandle`'s `Drop` shuts its socket and
/// joins its worker. [`switch_backend`] relies on exactly that.
pub struct BackendConnection {
    /// The registry record (or the synthetic [`cli_observe_record`]) this connection was built
    /// from — kept so the UI can mark the active row and a write can read its arming.
    pub record: BackendRecord,
    /// The observe (read-plane) bridge — [`observe_bridge::spawn_bridge`]'s stop handle.
    pub bridge: BackendObserveBridgeHandle,
    /// The Scope::Write (write-plane) channel — `Some` only when BOTH gates armed it (see
    /// [`connect_backend`]). ⚠ when `Some`, the GUI's order buttons place/cancel REAL orders on
    /// the remote daemon.
    pub control: Option<vike_tradehub_client::TradehubControlHandle>,
    /// The orders sent down [`Self::control`] whose answer has not been read: a ticket belongs to ONE
    /// handle, so it lives and dies with it (`crate::orders::node_refusals`).
    sent_orders: Mutex<
        crate::orders::node_refusals::SentOrders<
            vike_tradehub_client::tradehub_control::CommandTicket,
        >,
    >,
}

impl BackendConnection {
    /// Ask the node what it says of every Trade window's held order
    /// (`crate::ui::windows::preview_held_orders`), each on a short connection and a thread of its
    /// own, because the frame must never wait on the network. The answer shows on the next repaint,
    /// which the observe stream brings. A dry run that fails says nothing: the node judges at the send.
    pub fn preview_held(
        &self,
        wins: &[crate::ui::window::WindowState],
        views: &mut std::collections::HashMap<egui::Id, crate::ui::windows::WindowView>,
        snapshot: &vike_execution::CoreSnapshot,
        limits: &vike_tradehub_client::order_checks::OrderLimits,
    ) {
        // Signed with the device key, as the control channel is (decision 0288); none, nothing sent.
        let Some(device) = registry::device_key().filter(|_| self.control.is_some()) else {
            return;
        };
        crate::ui::windows::preview_held_orders(wins, views, snapshot, limits, |command, slot| {
            let address = self.record.address.clone();
            let signer = device.clone();
            let asked = std::thread::Builder::new().name("ui-preview".into()).spawn(move || {
                match vike_tradehub_client::preview_command(address.as_str(), &signer, &command) {
                    Ok(said) => *vike_utility::mutex::lock(&slot) = Some(said),
                    Err(e) => {
                        tracing::debug!(addr = %address, "a held order's dry run failed: {e}")
                    }
                }
            });
            if let Err(e) = asked {
                tracing::warn!("could not start a held order's dry run: {e}");
            }
        });
    }

    /// Whether the node behind [`Self::control`] takes a bracket naming its group stem
    /// (`vike_tradehub_client::command_lift::node_takes_bracket_stem`): the shell's `OrderEntryDispatchInputs::bracket_stem`.
    pub fn takes_bracket_stem(&self) -> bool {
        self.control.as_ref().is_some_and(|c| {
            vike_tradehub_client::command_lift::node_takes_bracket_stem(|wire| c.takes(wire))
        })
    }

    /// Keep `ticket`, the control handle's receipt for `command`, so the node's answer to an order can
    /// reach its window ([`Self::node_refusals`]).
    pub fn track_sent(
        &self,
        ticket: vike_tradehub_client::tradehub_control::CommandTicket,
        command: &vike_execution::Command,
    ) {
        vike_utility::mutex::lock(&self.sent_orders).track(ticket, command);
    }

    /// The node's refusals, since the last call, of orders sent down [`Self::control`], its notes on the
    /// ones it accepted and the ones whose link died unanswered, each with the address it was sent to.
    /// Never waits: an order the node has not answered yet is asked again next frame.
    pub fn node_refusals(
        &self,
    ) -> Vec<(
        vike_order_entry::trade_window::address::TradeWindowAddress,
        crate::orders::node_refusals::SentOrderWord,
    )> {
        let Some(ctrl) = &self.control else { return Vec::new() };
        vike_utility::mutex::lock(&self.sent_orders).poll(|t| ctrl.await_outcome(t, Duration::ZERO))
    }
}

/// The synthetic, unnamed [`BackendRecord`] the `--observe ADDR` CLI flag becomes:
///
/// - the record is ARMED (`control = true`), so the process-level master gate (the PC's
///   `flags.tradehub_control` row) and the device key remain the only gates on the CLI path. A
///   registry record, by contrast, must arm itself explicitly in `backends.json` (B9).
/// - `name` is empty: this record exists for one process's lifetime and is never persisted, so
///   `picker_rows` lists it as the unlisted active row rather than a registry entry.
pub fn cli_observe_record(address: &str) -> BackendRecord {
    BackendRecord {
        name: String::new(),
        address: address.to_string(),
        observe_key: String::new(),
        control_key: None,
        control: true,
    }
}

/// Dial `record` and build its [`BackendConnection`]: spawn the observe bridge (self-healing reconnect
/// loop — never blocks the caller) and, iff every control gate passes, the Scope::Write channel.
///
/// Every dial signs with this desktop's device key ([`registry::device_key`], decision 0288) — this
/// function never reads an environment variable, the credential store or a shared key.
///
/// **The control channel mounts only when ALL THREE gates arm it** (defense in depth for a write path —
/// see the module doc):
///
/// 1. the per-backend gate (B9): `record.control`, AND
/// 2. `master_control`, the process-level gate — the caller passes the `flags.tradehub_control`
///    row of its own settings database, read by the BINARY, AND
/// 3. this desktop holds its device key (the server's access list then decides its role).
///
/// A record that fails gate 1 is refused SILENTLY (an unarmed backend is the ordinary state, not a
/// misconfiguration); an armed record under `master_control` on a desktop without a device key
/// gets [`observe_bridge::connect_control`]'s warn, because the operator armed something that
/// cannot mount. Either way the observer stays read-only (`control: None`).
///
/// `patience` is the operator's resolved link patience, read by the BINARY from the two
/// `preferences.*` rows and passed down (this library reads no setting): the observe stream's
/// silence limit goes to the bridge, the control reply wait to the write channel, on every dial this
/// connection makes. `LinkPatience::default()` is the window every dial had before the rows existed.
///
/// `publish`/`repaint` are the same two GUI-owned closures [`observe_bridge::spawn_bridge`] takes
/// (the arc-swap store and the egui repaint wake), and `status` the same feed-status line handle.
/// `observe_poll` is the sleep between the bridge's snapshot reads — the caller's
/// `preferences.observe_poll_ms` row, read by the BINARY like `master_control` — handed straight to
/// [`observe_bridge::spawn_bridge`].
pub fn connect_backend<P, R>(
    record: &BackendRecord,
    master_control: bool,
    observe_poll: Duration,
    patience: LinkPatience,
    status: Arc<Mutex<DataClientStatus>>,
    publish: P,
    repaint: R,
) -> BackendConnection
where
    P: Fn(Arc<vike_execution::CoreSnapshot>) + Send + 'static,
    R: Fn() + Send + 'static,
{
    // What the bridge signs with is this PC's DEVICE key (it re-reads the store at every dial,
    // `registry::fresh_device_key`). So the only thing worth a line is having no device key, said once per connect with its
    // fix, and nothing at all when the key is held. It stays a diagnostic rather than a refusal: the
    // bridge is self-healing, and the Server group's connect makes the key without a restart.
    if let Some(line) = registry::signer_notice(registry::device_key().is_some(), &record.address) {
        tracing::warn!(addr = %record.address, "{line}");
    }

    let bridge = observe_bridge::spawn_bridge(
        record.address.clone(),
        // …and the record's NAME goes with the address, because the address identifies nothing: a
        // loopback-bound daemon is reached down an SSH tunnel and every client then reads
        // `127.0.0.1`. `crate::backend::identity` is the authority for both.
        crate::backend::identity::status_label(record).map(str::to_string),
        registry::fresh_device_key,
        observe_poll,
        patience,
        status,
        publish,
        repaint,
    );
    // Gates 1 and 2 short-circuit `enabled`, so an unarmed record or a masterless process never
    // even logs; gate 3 is the device key `connect_control` is handed.
    let control = observe_bridge::connect_control(
        master_control && record.control,
        &record.address,
        registry::device_key().as_ref(),
        patience,
    );
    BackendConnection { record: record.clone(), bridge, control, sent_orders: Mutex::default() }
}

/// The WHOLE surface a backend switch works over — the
/// borrow-bundle shape of [`core_sync::CoreSyncState`](crate::ui::core_sync::CoreSyncState) and
/// [`feed_lifecycle::FeedSlots`](crate::ui::feed_lifecycle::FeedSlots), widened to every store either
/// side of them (a prior audit named 11 sites that silently show stale data when one is missed).
///
/// ⚠ This sentence used to carry a COUNT of `CoreSyncState`'s fields ("six"), and it was already
/// wrong before the field that reminded anyone to look — the struct had SEVEN. The count is gone
/// rather than corrected, for the reason every other count in this tree was: it rots on the PR that
/// adds a field, and that PR has no reason to read a doc comment one crate over. The fields are
/// enumerated below, which is a list a compiler checks.
///
/// **Since B2 the surface is split by OWNER, not cleared wholesale.** The market-data feeds run
/// beside the backend session (B2 added them as the third mode's direct-to-venue plane; the
/// datahub-backed `market_data_session` feeds hold the same slots now), so each field belongs to exactly one of
/// two planes:
///
/// - **backend session** ([`clear_session_state`], runs on every switch): the snapshot-rendered
///   chart entries, `last_sequence`, the PUBLISHED-SERIES list, the status line, `hidden`;
/// - **local feed plane** (untouched by a switch, and by nothing else: no function tears it
///   down): everything else — the feed clients and their three subscription ledgers, the
///   tape-rendered folds, the trade tape and the books. Venue truth, identical under any
///   backend — a switch leaves this plane running and painted, which the bundle carries so a
///   test can prove it (`crates/vike-app-core/src/backend/connection_tests.rs`'s
///   `a_switch_clears_the_backend_session_and_never_touches_the_feed_plane`).
pub struct BackendSwitchSlots<'a> {
    // ── the render model — `core_sync::CoreSyncState`'s fields ───────────────────────────────
    /// Per-chart-key render state (`App::charts`). SPLIT-OWNED, entry by entry
    /// (`split_plane::folds_from_snapshot`): the snapshot-rendered (kline) entries are backend
    /// session — A's streamed bars must never paint under B — while the tape-rendered
    /// (tick/volume) entries are venue truth and survive a switch.
    pub charts: &'a mut HashMap<String, state::ChartState>,
    /// Tick/volume aggregators, `chart key -> (venue, symbol, agg)` (`App::tick_volume_aggregators`) — local plane
    /// (folded from the venue tape, not from any backend).
    pub tick_volume_aggregators: &'a mut HashMap<String, (String, String, TickVolAgg)>,
    /// Orderflow aggregators, same keyspace (`App::orderflow_aggregators`) — local plane, like `tick_volume_aggregators`.
    pub orderflow_aggregators: &'a mut HashMap<String, (String, String, OrderflowAgg)>,
    /// **What the CURRENT backend publishes** (`App::published_series`) — backend session, and
    /// emphatically so: it is read by
    /// [`series_follow::follow_backend`](crate::ui::series_follow::follow_backend), which RETARGETS
    /// chart windows, and by the symbol picker, which offers its rows as one-click destinations.
    /// Carrying A's list into B's session would let a chart adopt — or an operator pick — a series
    /// the connected node does not have. Cleared; B's first snapshot rewrites it.
    pub published: &'a mut Vec<crate::ui::series_follow::PublishedSeries>,
    /// The global snapshot dirty flag (`App::last_sequence`) — backend session; reset to 0 so B's
    /// first snapshot (whose `seq` may restart anywhere) always refolds from scratch.
    pub last_sequence: &'a mut u64,
    /// The status line the GUI paints (`App::status`) — backend session.
    pub status: &'a mut DataClientStatus,
    // ── the feed slots — `feed_lifecycle::FeedSlots`'s surface (all local plane) ────────────
    /// The venue-keyed live market-data clients (`App::feeds`). Never torn down by a switch —
    /// these are LOCAL venue feeds, not backend state (client shutdown is `App::on_exit`'s).
    pub feeds: &'a mut DataClientMap,
    /// Live subscription ids (`App::subscriptions`) — the venue streams keep flowing across a switch.
    pub subscriptions: &'a mut HashMap<String, vike_data::DataClientSubscriptionId>,
    /// Every key the GUI has ensured (`App::spawned`) — also `sync_from_core`'s fold filter.
    /// Survives a switch: the open windows (GUI-scoped) still want exactly these series, and the
    /// fold filter must keep passing them so B's snapshots repaint the kline windows.
    pub spawned: &'a mut HashSet<String>,
    /// Keys whose venue had no client (`App::unroutable`).
    pub unroutable: &'a mut HashSet<String>,
    /// The subscribe-leg retry lane (`App::feed_retries`).
    pub feed_retries: &'a mut DatahubFeedRetries,
    // ── the live caches — keyed `(venue, symbol)`, no backend dimension (local plane) ───────
    /// The live trade tape (`App::trades`).
    pub trades: &'a TickStore,
    /// The live L2 books (`App::books`).
    pub books: &'a L2BookStore,
    /// The GUI-side bar store (`App::direct_bars`) — `Some` exactly where a plane fills one. Its
    /// content is cleared by [`clear_session_state`] under [`split_plane::BackendBarPlane::BackendStore`]
    /// ONLY.
    ///
    /// ⚠ **The DOUBLE DUTY is gone.** Its presence used to BE the flag the retain fed to
    /// `split_plane::folds_from_snapshot`; that is now [`Self::bar_plane`], and the handle is a
    /// handle. See [`split_plane::BackendBarPlane`].
    pub direct_bars: Option<&'a StoredBarStore>,
    /// **WHAT FILLS the bar store** ([`split_plane::BackendBarPlane`]) — the flag the retain in
    /// [`clear_session_state`] feeds to `split_plane::folds_from_snapshot`, and the fact that
    /// decides whether a switch also empties the store.
    pub bar_plane: split_plane::BackendBarPlane,
    /// Trade window depth-stream ledger (`App::trade_depth`), each entry carrying the subscription ids
    /// `ensure_depth` minted ([`L2BookSubscriptions`]). A switch leaves the streams (and the painted
    /// ladder) alone.
    pub trade_depth: &'a mut HashMap<(String, String), L2BookSubscriptions>,
    /// Polymarket cockpit stream ledger (`App::polymarket_subscriptions`) — same treatment as `trade_depth`.
    pub polymarket_subscriptions: &'a mut HashMap<String, L2BookSubscriptions>,
    /// Data-manager-deleted keys (`App::hidden`) — backend session, cleared DELIBERATELY: a hide
    /// is a statement about one session's data, and a stale entry would silently blank the
    /// same-named series of every later backend. The tradeoff — an operator's hide does not
    /// survive a switch — is accepted; re-hiding is one click, an invisibly blank series is a
    /// support ticket.
    pub hidden: &'a mut HashSet<String>,
    /// The feed-status line handle the bridge writes (`App::feed_status`) — reset on disconnect,
    /// handed to the new bridge on connect.
    pub feed_status: &'a Arc<Mutex<DataClientStatus>>,
}

/// The BACKEND-SESSION clear — exactly the [`BackendSwitchSlots`] state the ACTIVE BACKEND's session
/// owns, and nothing the local feed plane owns. Called by [`switch_backend`] between stopping A
/// and dialing B; public so tests (and a future runtime mode exit) drive it directly.
///
/// **B2 narrowed this from B1's total clear.** Under B1 the observe GUI had no feeds, so "clear
/// everything" and "clear the backend session" were indistinguishable — every feed/tape/book slot
/// was empty anyway. B2's third mode (fat + `--observe`) ran live direct-to-venue feeds BESIDE
/// the backend session, and those are NOT backend state: the tape, the books, the tick/volume and
/// orderflow folds and the subscription ledgers are venue truth, identical under any backend, so a
/// switch must leave them running and painted — zero unsubscribes, zero drains. What IS
/// backend-session state, and is cleared:
///
/// - the **snapshot-rendered chart entries** (`split_plane::folds_from_snapshot` — the kline
///   family). Under [`split_plane::BackendBarPlane::BackendStore`] the store's content is a read of THIS
///   backend's hist store, so every kline chart AND the store itself clear — see the body: their
///   bars came from backend A's streamed tail and must never paint under B's connection. The
///   per-frame `ensure_feed_on` cadence re-creates each open window's entry and the fold
///   repaints it from B's snapshots. Tape-rendered entries (tick/volume) are kept: their fold
///   is the market-data tape, not A's session.
/// - **`last_sequence`** — reset to 0 so B's first snapshot (whose `seq` may restart anywhere) always
///   refolds from scratch.
/// - **the status line**, and — B1's documented tradeoff, unchanged — **`hidden`**: a hide is a
///   statement about one session's data; a stale entry would silently blank the same-named
///   series of every later backend. Re-hiding is one click, an invisibly blank series is a
///   support ticket.
///
/// Deliberately NOT cleared, because they are GUI-scoped, not session-scoped: the window layout
/// (`App::windows` — the open windows are exactly what drives the re-ensure/refold against the new
/// backend), the display timezone, indicator favourites, tool-view state, and the workspace
/// persistence family. Clearing those would make a backend switch also a layout reset, which no
/// operator asked for.
pub fn clear_session_state(slots: &mut BackendSwitchSlots<'_>) {
    // Backend-session chart state: exactly the snapshot-rendered entries. `retain` (not `clear`)
    // is the B2 point — a tick/volume chart's fold is the venue's tape (and a direct-bar kline
    // chart's is the venue's bar feed), not A's session, and blanking either on every switch
    // would punish the plane that did not change.
    let plane = slots.bar_plane;
    slots.charts.retain(|key, _| !split_plane::folds_from_snapshot(plane, key));
    // ⚠ AND THE STORE ITSELF, under the backend-store plane ONLY. There its content is bars read
    // out of THIS backend's hist store, so it is backend-session state exactly like the streamed
    // tail — leaving it would repaint A's history under B's connection, which is the whole bug
    // class this function exists for. (A plane whose store a venue filled would keep it across a
    // switch; no such plane exists today.)
    if plane == split_plane::BackendBarPlane::BackendStore
        && let Some(bars) = slots.direct_bars
    {
        bars.clear();
    }
    *slots.last_sequence = 0;
    slots.published.clear();
    *slots.status = DataClientStatus::default();
    slots.hidden.clear();

    // The snapshot-rendered render model must be provably gone when this returns — a future edit
    // that weakens the retain (or reorders something back in) should die here, not paint A's
    // bars under B.
    debug_assert!(slots.charts.keys().all(|k| !split_plane::folds_from_snapshot(plane, k)));
    debug_assert!(*slots.last_sequence == 0);
    debug_assert!(slots.published.is_empty());
    debug_assert!(*slots.status == DataClientStatus::default());
}

/// THE SWITCH ROUTINE (B1's safety-critical piece): stop the old backend, run the
/// [backend-session clear](clear_session_state), blank the published snapshot, and dial the new
/// record — or, with `target: None`, disconnect and stay down. The local feed plane is
/// deliberately untouched (B2): feeds are not backend-session state, so the tape, books, ladders and
/// tick/volume folds keep painting straight through a switch.
///
/// Ordering is load-bearing:
///
/// 1. **Stop A's bridge FIRST** ([`BackendObserveBridgeHandle::stop`] — raises the flag and JOINS), so no
///    late publish from A's thread can land after step 3's blank. Dropping the [`BackendConnection`]
///    also drops A's control channel (socket shut, worker joined).
/// 2. **Backend-session clear** — [`clear_session_state`]: every field the session owns.
/// 3. **Publish an empty snapshot** into the GUI's cell, so the render loop never paints A's
///    data under B's (or no) connection — not even for the frames B spends connecting.
/// 4. **Dial B** (when `target` is `Some`) via [`connect_backend`] — same gates, same closures,
///    and the same `observe_poll` and `patience` (the caller's resolved link patience, see there) the
///    first connection used.
pub fn switch_backend<P, R>(
    active: &mut Option<BackendConnection>,
    target: Option<&BackendRecord>,
    master_control: bool,
    observe_poll: Duration,
    patience: LinkPatience,
    mut slots: BackendSwitchSlots<'_>,
    publish: P,
    repaint: R,
) where
    P: Fn(Arc<vike_execution::CoreSnapshot>) + Send + 'static,
    R: Fn() + Send + 'static,
{
    if let Some(mut old) = active.take() {
        // Explicit for the reader; `drop(old)` alone would do both (stop is idempotent).
        old.bridge.stop();
    }
    clear_session_state(&mut slots);
    publish(Arc::new(vike_execution::CoreSnapshot::empty("observing", "")));
    repaint();
    match target {
        Some(record) => {
            let status = Arc::clone(slots.feed_status);
            *active = Some(connect_backend(
                record,
                master_control,
                observe_poll,
                patience,
                status,
                publish,
                repaint,
            ));
        }
        None => {
            if let Ok(mut s) = slots.feed_status.lock() {
                *s = DataClientStatus::new(DataClientConnectionState::Disconnected, "disconnected");
            }
        }
    }
}

/// What a picker click should do — the pure decision behind the Connections tool's buttons,
/// applied by the shell (`vike-desktop`) after the frame (the same OUT-slot idiom as every other
/// tool action).
#[derive(Debug, Clone, PartialEq)]
pub enum BackendAction {
    /// Dial this record (stopping the current backend first — [`switch_backend`]).
    Connect(BackendRecord),
    /// Stop the current backend and stay disconnected.
    Disconnect,
}

/// One picker row: a record, whether it is the ACTIVE connection, and whether it came from the
/// registry (`listed`) or is the synthetic/CLI connection the registry does not know
/// (`listed: false` — rendered so a `--observe ADDR` session still shows what it is connected to).
pub(crate) struct BackendPickerRow<'a> {
    /// The record this row shows.
    pub(crate) record: &'a BackendRecord,
    /// This row is the live connection.
    pub(crate) is_active: bool,
    /// This row is a `backends.json` entry (vs the unlisted active connection).
    pub(crate) listed: bool,
}

/// The picker's row list: every registry record in file order (marked active where it matches the
/// live connection's record), preceded by the active connection itself when the registry does not
/// list it — the `--observe ADDR` synthetic record case.
pub(crate) fn picker_rows<'a>(
    file: &'a BackendRegistry,
    active: Option<&'a BackendRecord>,
) -> Vec<BackendPickerRow<'a>> {
    let mut rows = Vec::with_capacity(file.backends.len() + 1);
    if let Some(a) = active
        && !file.backends.contains(a)
    {
        rows.push(BackendPickerRow { record: a, is_active: true, listed: false });
    }
    for record in &file.backends {
        rows.push(BackendPickerRow { record, is_active: active == Some(record), listed: true });
    }
    rows
}

/// **The REGISTRY's rows and only those** — [`picker_rows`] without its unlisted-active head row.
///
/// ⚠ This exists because the Connections redesign removed a duplication BY NAME and then kept it:
/// the tool's Backend tab rendered [`picker_rows`], whose `listed: false` head row restates the
/// live connection's dot, name, address, `control armed`, `(not in registry)` and a `Disconnect` —
/// the ambient foot strip's five facts, a second time, on a tab the strip is also drawn on. The
/// strip is the one the design keeps (it is process-level state, identical on both tabs and
/// belonging to neither), so the tab shows what the strip cannot: the `backends.json` records an
/// operator can Connect to, Edit and Delete.
///
/// The head row was never editable — `backend_tab` already suppressed its Edit/Delete with
/// `if row.listed`, which is the tell that it did not belong in a list of records. What remains for
/// the live connection when it is unlisted is a sentence naming the strip, not a row.
///
/// ⚠ [`picker_rows`] itself is UNCHANGED and still gated by
/// `picker_rows_mark_the_active_record_and_surface_an_unlisted_connection`: an unlisted active
/// connection is still a fact this module can answer, and a caller that wants it (a future picker
/// that switches from a list) has it. This is the one the TOOL asks.
pub(crate) fn registry_rows<'a>(
    file: &'a BackendRegistry,
    active: Option<&'a BackendRecord>,
) -> Vec<BackendPickerRow<'a>> {
    picker_rows(file, active).into_iter().filter(|r| r.listed).collect()
}

/// Is the live connection one the registry does not list — the `--observe ADDR` synthetic record?
///
/// The Backend tab renders no ROW for it ([`registry_rows`]); this is what it asks in order to say
/// so in a sentence instead, and to point at the strip that does render it.
#[must_use]
pub(crate) fn active_is_unlisted(file: &BackendRegistry, active: Option<&BackendRecord>) -> bool {
    active.is_some_and(|a| !file.backends.contains(a))
}

/// The pure click decision: clicking the ACTIVE row disconnects, clicking any other row connects
/// to it (which [`switch_backend`] makes an implicit disconnect-then-connect).
pub(crate) fn click_action(
    clicked: &BackendRecord,
    active: Option<&BackendRecord>,
) -> BackendAction {
    if active == Some(clicked) {
        BackendAction::Disconnect
    } else {
        BackendAction::Connect(clicked.clone())
    }
}

/// THE STARTUP LADDER: which backend a launch would connect to, as a whole RECORD — with the
/// registry answer supplied, so every rung is testable without a file on disk and the tests drive
/// the shipped decision rather than a copy. [`startup_observe`] is the impure entry point that
/// supplies it (and the ONLY one — see [`BackendStartup`] for why answering the address question
/// alone was a P1).
///
/// `flag` is the `--observe` ARGUMENT, scanned out of argv by `startup_observe_from` (the shell's
/// job stays in the shell; nothing here reads the environment). The rungs, in order:
///
/// 1. **The FLAG**, when non-blank — an OVERRIDE, and it becomes the synthetic
///    [`cli_observe_record`], so `--observe ADDR` keeps meaning exactly what it always meant:
///    control armed by the master gate.
/// 2. **The registry's ACTIVE record** ([`crate::backend::registry::active_record`]) — answered as
///    ITSELF, never re-synthesized: that record is the only place its NAME and its `control`
///    arming live (that function's doc carries the measurement).
/// 3. **The local default** ([`vike_config::config::DEFAULT_TRADEHUB_ADDRESS`], where the daemon
///    binds by default, so a client on its box reaches it), synthetic like the flag: a floor so the
///    GUI OPENS, never a guess at what the user wants.
///
/// ⚠ There is no `None` rung. `--observe <host>:<port>` used to be MANDATORY in a thin build and
/// its absence EXITED — so the viewer asked a person to retype a flag naming the one thing that
/// binary can do, and a typo closed the window instead of opening it. A viewer that opens and says
/// "not connected" in its status bar is strictly better: nothing here can place an order, so a
/// wrong address costs a reconnect line and nothing else.
///
/// ⚠ **That is an answer to "what address would this launch watch", and it is NOT an answer to
/// "is this launch an observer at all"** — [`BackendStartup::requested`] is. Reading rung 3 as the
/// second question is the regression that record documents.
///
/// ⚠ `config.tradehub_addr` is deliberately NOT a rung, and the attempt to make one is worth
/// recording: that setting is the DAEMON's BIND address (`0.0.0.0:7879` on the box being watched),
/// which a client cannot dial. A viewer's address is a different fact and belongs here.
#[must_use]
pub fn startup_backend_from(flag: Option<&str>, active: Option<BackendRecord>) -> BackendRecord {
    if let Some(address) = flag.map(str::trim).filter(|a| !a.is_empty()) {
        return cli_observe_record(address);
    }
    if let Some(record) = active {
        return record;
    }
    let address = vike_config::config::DEFAULT_TRADEHUB_ADDRESS;
    tracing::info!(
        address,
        "no --observe and no active backend — watching the local default. Add or pick a node in \
         Connections, or pass --observe <host>:<port>."
    );
    cli_observe_record(address)
}

/// The `--observe` flag as argv spells it. A separate constant because two different questions read
/// it — the ADDRESS ladder ([`startup_backend_from`]) and the MODE ([`BackendStartup::requested`]) —
/// and a launch that spelled the word without an address must still answer the second one.
pub(crate) const OBSERVE_FLAG: &str = "--observe";

/// The WHOLE startup answer: the backend a launch would watch, AND whether observing was actually
/// REQUESTED. One struct, from one [`crate::backend::registry::active_record`] read, because the two
/// facts are read together and a launch whose mode disagreed with its address would be
/// unexplainable.
///
/// # Why `requested` exists (the P1 this type was added to make impossible)
///
/// [`startup_backend_from`]'s ladder deliberately has no `None` rung: the Connections UI needs an
/// address to show, so a launch with no flag and no configured node still resolves
/// [`vike_config::config::DEFAULT_TRADEHUB_ADDRESS`]. `crates/vike-desktop/src/main.rs` wrapped
/// that answer in `Some` UNCONDITIONALLY and handed it to `App::new`, whose `if let Some(..)` is the
/// OBSERVER ARM — so from #1610 (2026-09-03) every ordinary desktop launch took that arm: no local
/// core, no exec engines, no recorder, no journal materializer, no recon driver, and
/// `crates/vike-desktop/src/app_ui/mod.rs`'s `dispatch` resolved to the read-only no-op. **The GUI could
/// not place an order at all**, for three days, while its status bar retried a daemon nobody was
/// running. The address ladder was right for the question it answers; nothing asked the other one.
///
/// So the mode question is answered HERE and nowhere else, and
/// [`split_plane::observes`] turns it into whether a launch
/// observes (it fed the `observing` argument of `split_plane::app_mode`, the arm table deleted with
/// the local-core arm in 2026-10).
pub struct BackendStartup {
    /// The record a connection would be dialled from — [`startup_backend_from`]'s ladder, which
    /// always answers, so the address exists whatever `requested` says.
    pub record: BackendRecord,
    /// Was observing actually ASKED FOR: the `--observe` word on the command line (with or without
    /// an address — the word alone means "the configured node"), or an active registry record.
    /// `false` is the ordinary fat launch, which runs a LOCAL trading core and dials no backend.
    pub requested: bool,
}

/// [`BackendStartup`] for this process: scan `argv` for `--observe [ADDR]` and answer both facts off
/// ONE [`crate::backend::registry::active_record`] read.
#[must_use]
pub fn startup_observe(argv: impl IntoIterator<Item = String>) -> BackendStartup {
    startup_observe_from(argv, crate::backend::registry::active_record())
}

/// [`startup_observe`]'s PURE half — argv as a parameter and the registry answer supplied, so every
/// combination is testable without a process or a file on disk (the binary passes
/// `std::env::args()`). The BARE-flag case is exactly why the scan moved in here from the
/// CI-excluded shell: `--observe` as the last word on the line yields no address at all, so it
/// contributes no rung to [`startup_backend_from`] while still being a request to observe.
///
/// ⚠ **Deliberately NOT read by `clap` (decision 0155, "What would reopen this"): this is a GUI's
/// argv.** A desktop app must never refuse to start because of a token the OS or a launcher put on
/// its command line (a macOS `-psn_…` serial, a file-manager path, a `%U` expansion), and
/// `vike_utility::cli::Program` refuses an unknown flag, a stray positional and a blank value by
/// design. `--observe` also takes the NEXT token whatever it is, as this scan always did. So the
/// scan stays a scan: it reads the one word it knows and ignores the rest. `crates/vike-app-core/src/ui/desktop_entry.rs`'s
/// `run` is the same kind of reader for the same reason.
#[must_use]
pub(crate) fn startup_observe_from(
    argv: impl IntoIterator<Item = String>,
    active: Option<BackendRecord>,
) -> BackendStartup {
    let mut args = argv.into_iter();
    let mut flag = None;
    let mut present = false;
    while let Some(a) = args.next() {
        if a == OBSERVE_FLAG {
            present = true;
            flag = args.next();
            break;
        }
    }
    let requested = present || active.is_some();
    BackendStartup { record: startup_backend_from(flag.as_deref(), active), requested }
}

/// The registry's `active` pointer after one picker action — the "configure once, launch bare
/// afterwards" half of [`crate::backend::registry::active_record`].
///
/// ⚠ This is the ONLY thing that SETS that pointer. `backend::editor` retargets it on a rename and
/// clears it on a delete, and nothing else touched it: a node could be added in the Connections
/// tool, connected to, and watched all session, and the pointer stayed `None` — so the next bare
/// launch fell through to [`vike_config::config::DEFAULT_TRADEHUB_ADDRESS`] and silently observed a
/// different daemon. Measured against a three-node setup on 2026-09-03: connecting to a `the CI box`
/// record left `"active": null` on disk, and the relaunch dialled the local default.
///
/// Connect names the record only when the registry LISTS it. The `--observe` connection is
/// unlisted by construction (`picker_rows`'s `listed: false` row), and a pointer at a record
/// `backends` does not hold is an orphan [`crate::backend::registry::active_record`] refuses anyway.
/// Disconnect clears it: staying down is the operator saying the next launch should not dial.
#[must_use]
pub fn active_after(action: &BackendAction, backends: &[BackendRecord]) -> Option<String> {
    match action {
        BackendAction::Connect(r) if backends.iter().any(|b| b.name == r.name) => {
            Some(r.name.clone())
        }
        BackendAction::Connect(_) | BackendAction::Disconnect => None,
    }
}

#[path = "connection_tests.rs"]
#[cfg(test)]
mod connection_tests;
