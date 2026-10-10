//! `backend_conn` — RUNTIME-MUTABLE backend selection for the observe GUI (split-plane B1): one
//! [`BackendConn`] per live connection, [`connect_backend`] to build it from a
//! [`BackendRecord`](crate::backend::backend_registry::BackendRecord), and [`switch_backend`] — the
//! safety-critical BACKEND-SESSION CLEAR that keeps backend A's data from ever painting under
//! backend B's connection. (B1 shipped it as a total clear; B2 split the surface into the backend
//! session, cleared on every switch, and the local feed plane — venue truth the third mode ran
//! beside the session until the `fat` build went — which survives a switch and has its own teardown,
//! [`teardown_feed_plane`]. The two compose to B1's total clear, pinned by test.)
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
//!   replaces the whole [`BackendConn`]; there is no per-backend series keyspace (deferred I15),
//!   so a switch must CLEAR AND REFOLD rather than re-key.
//! - **Key resolution strictly through
//!   [`backend_registry::resolve_keys`](crate::backend::backend_registry::resolve_keys)** (B9): the record
//!   holds credential-store KEY NAMES, resolution to bytes happens here at connect time, and an
//!   unarmed record can never mount a control channel.
//! - **The process-level master gate stays** —
//!   [`tradehub_control::control_enabled`](crate::backend::tradehub_control::control_enabled) — as an AND
//!   over the per-backend arming. Defense in depth for a write path: the record file
//!   (`backends.json`) is GUI-owned and editable by anything that can write the profile directory,
//!   so a record flipping `control = true` must not be sufficient on its own to arm REAL order
//!   placement; the operator's process-level opt-in (`VIKE_TRADEHUB_CONTROL=1`) is still required,
//!   exactly as it was when the flag was the ONLY gate.
//! - **The fat local-core path was untouched**: `switching_available` answered `false` while a
//!   local core ran, and a process started without `--observe` and without a configured backend
//!   behaved byte-identically to before B1. Mixing planes was the THIRD MODE (B2, fat +
//!   `--observe` — [`split_plane::AppMode::ObserveWithFeeds`](crate::backend::split_plane::AppMode::ObserveWithFeeds)): a remote backend's
//!   account plane beside local venue feeds, with NO local core — so switching stayed available
//!   there, and the switch left the feed plane alone. ⚠ Both arms went with the `fat` build on
//!   2026-09-09 (#1727): the desktop has no local core, so switching is always available and the
//!   feed plane the switch preserves is empty. (This bullet spoke of both in the present tense
//!   until 2026-09-28; `switching_available`, a constant `true` from then on, has been deleted.)

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use crate::backend::backend_registry::{self, BackendRecord, BackendsFile};
use crate::backend::observe_bridge::{self, BridgeHandle};
use crate::backend::split_plane;
use crate::backend::venue_routing::venue_of_key;
use crate::data::data_sink::{BookStore, DirectBarStore, TradeStore};
use crate::ui::feed_lifecycle::{
    BackfillRetries, FeedMap, FeedRetries, PolyBookSubs, TradeDepthSubs,
};
use vike_chart::model;
use vike_orderflow::bar_agg::OrderflowAgg;
use vike_orderflow::tickvol::TickVolAgg;

// ⚠ The `--observe` path's two key names USED to be a third crate-local pair here
// (`backend_registry::OBSERVE_KEY_NAME` / `backend_registry::CONTROL_KEY_NAME`). They were byte-identical to
// [`backend_registry::OBSERVE_KEY_NAME`] / [`backend_registry::CONTROL_KEY_NAME`] and, unlike those,
// bought nothing: the settings scanner merges constants per CRATE, so a sibling module's constant
// still resolves for it (`settings_registry.rs`'s `cross_file_consts_resolve_within_a_crate` is the
// regression test). This pair also had no `.get(` site anywhere — it only ever WROTE record fields —
// so removing it cannot touch MAP_LOOKUP_PROVEN. The workspace-wide duplication that DOES buy
// something is documented on `vike_tradehub_client::auth::OBSERVE_KEY_ENV`, the reference spelling.

/// One live backend connection: the record it was dialed from, the observe bridge, and the
/// optional Scope::Write write channel.
///
/// Dropping the whole struct is a full teardown: [`BridgeHandle`]'s `Drop` raises the stop flag
/// and JOINS the reconnect thread (B6), and `RemoteControlHandle`'s `Drop` shuts its socket and
/// joins its worker. [`switch_backend`] relies on exactly that.
pub struct BackendConn {
    /// The registry record (or the synthetic [`cli_observe_record`]) this connection was built
    /// from — kept so the UI can mark the active row and a reconnect can re-resolve keys.
    pub record: BackendRecord,
    /// The observe (read-plane) bridge — [`observe_bridge::spawn_bridge`]'s stop handle.
    pub bridge: BridgeHandle,
    /// The Scope::Write (write-plane) channel — `Some` only when BOTH gates armed it (see
    /// [`connect_backend`]). ⚠ when `Some`, the GUI's order buttons place/cancel REAL orders on
    /// the remote daemon.
    pub ctrl: Option<vike_tradehub_client::RemoteControlHandle>,
}

/// The synthetic, unnamed [`BackendRecord`] the `--observe ADDR` CLI flag becomes — compat with
/// the pre-B1 observe arm, byte for byte:
///
/// - `observe_key` is [`backend_registry::OBSERVE_KEY_NAME`], the exact name the old arm looked up in the workspace
///   credentials map (absent ⇒ empty key, and the daemon refuses the handshake — same as today).
/// - `control_key` is [`backend_registry::CONTROL_KEY_NAME`] and the record is ARMED (`control = true`), so the
///   process-level `VIKE_TRADEHUB_CONTROL=1` master gate remains the ONLY gate on the CLI path —
///   exactly the pre-B1 behavior, where that flag plus the key's presence decided alone. A
///   registry record, by contrast, must arm itself explicitly in `backends.json` (B9).
/// - `name` is empty: this record exists for one process's lifetime and is never persisted, so
///   [`picker_rows`] lists it as the unlisted active row rather than a registry entry.
pub fn cli_observe_record(addr: &str) -> BackendRecord {
    BackendRecord {
        name: String::new(),
        addr: addr.to_string(),
        observe_key: backend_registry::OBSERVE_KEY_NAME.to_string(),
        control_key: Some(backend_registry::CONTROL_KEY_NAME.to_string()),
        control: true,
        datahub_observe_key: String::new(),
    }
}

/// Dial `record` and build its [`BackendConn`]: spawn the observe bridge (self-healing reconnect
/// loop — never blocks the caller) and, iff BOTH control gates pass, the Scope::Write channel.
///
/// Key resolution is STRICTLY [`backend_registry::resolve_keys`] — this function never reads an
/// environment variable or the credential store; `vars` is the caller-loaded credentials map
/// (libraries take configuration as parameters).
///
/// **The control channel mounts only when BOTH gates arm it** (defense in depth for a write path —
/// see the module doc):
///
/// 1. the per-backend gate (B9): `record.control` AND a named `control_key` present in `vars`
///    (enforced inside [`backend_registry::resolve_keys`]), AND
/// 2. `master_control`, the process-level gate — the caller passes
///    [`tradehub_control::control_enabled`](crate::backend::tradehub_control::control_enabled) (or its
///    flags-file equivalent), read by the BINARY.
///
/// A record that fails gate 1 is refused SILENTLY (an unarmed backend is the ordinary state, not a
/// misconfiguration); an armed record under `master_control` with the named key missing from
/// `vars` gets [`observe_bridge::connect_control`]'s warn, because the operator armed something
/// that cannot mount. Either way the observer stays read-only (`ctrl: None`).
///
/// `publish`/`repaint` are the same two GUI-owned closures [`observe_bridge::spawn_bridge`] takes
/// (the arc-swap store and the egui repaint wake), and `status` the same feed-status line handle.
pub fn connect_backend<P, R>(
    record: &BackendRecord,
    vars: &HashMap<String, String>,
    master_control: bool,
    status: Arc<Mutex<String>>,
    publish: P,
    repaint: R,
) -> BackendConn
where
    P: Fn(Arc<vike_exec::CoreSnapshot>) + Send + 'static,
    R: Fn() + Send + 'static,
{
    let (observe_key, control_key) = backend_registry::resolve_keys(record, vars);

    // ⚠ An ABSENT key must not sign. `unwrap_or_default()` below turns a missing key into `""`, and
    // an empty key produces a perfectly well-formed HMAC that the node rejects — so the only thing
    // the operator ever saw was `tradehub observe auth denied: bad mac`, on a reconnect loop, which
    // reads as "wrong key" rather than "no key". Diagnosing it costs a trip through the handshake
    // to discover the client never had one.
    //
    // The daemon already gets this right from the other side: with no key in the store it logs
    // `a node-server address is configured but there is no VIKE_TRADEHUB_OBSERVE_KEY in the
    // node-key store — observe server NOT started (absent credential is the gate)` and declines.
    // This is the client's half of the same sentence, and it NAMES THE SAME STORE.
    //
    // It stays a DIAGNOSTIC rather than a refusal: the bridge is self-healing and an operator who
    // adds the key to the store and restarts should not have had the connection torn down
    // differently in the meantime. What changes is that the cause is now on the first line instead
    // of inferred from the fourth.
    if observe_key.is_none_or(str::is_empty) {
        tracing::error!(
            addr = %record.addr,
            key = %record.observe_key,
            "no observe key: {} is absent from the credentials map, so this client will sign with \
             an EMPTY key and the node will answer `bad mac` on every attempt. A PLATFORM key is \
             read from the NODE store (the settings database's node_key table, decision 0051) or \
             from the process environment; a record naming its OWN key name comes from the venue \
             credential store. It must match the daemon's own key byte for byte.",
            record.observe_key
        );
    }

    let bridge = observe_bridge::spawn_bridge(
        record.addr.clone(),
        // …and the record's NAME goes with the address, because the address identifies nothing: a
        // loopback-bound daemon is reached down an SSH tunnel and every client then reads
        // `127.0.0.1`. `crate::backend::backend_identity` is the authority for both.
        crate::backend::backend_identity::status_label(record).map(str::to_string),
        observe_key.unwrap_or_default().to_string(),
        status,
        publish,
        repaint,
    );
    // Gate 2 (master) short-circuits `enabled`, so an unarmed record or a masterless process
    // never even logs; gate 1's key half arrives through `resolve_keys` (an unarmed record
    // yields `None` there even when the store holds the key).
    let ctrl = observe_bridge::connect_control(
        master_control && record.control,
        &record.addr,
        control_key,
    );
    BackendConn { record: record.clone(), bridge, ctrl }
}

/// The WHOLE clear surface a backend switch or a feed-plane teardown works over — the
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
/// **Since B2 the surface is split by OWNER, not cleared wholesale.** The third mode runs live
/// venue feeds beside the backend session, so each field belongs to exactly one of two planes:
///
/// - **backend session** ([`clear_session_state`], runs on every switch): the snapshot-rendered
///   chart entries, `last_seq`, the PUBLISHED-SERIES list, the status line, `hidden`;
/// - **local feed plane** ([`teardown_feed_plane`], the mode-exit teardown): everything else —
///   the feed clients and their three subscription ledgers, the tape-rendered folds, the trade
///   tape, the books, and the whole Binance backfill lane. Venue truth, identical under any
///   backend — a switch leaves this plane running and painted.
///
/// The two routines compose to B1's original total clear, field for field, and the composition
/// is pinned by test so no field can silently fall between them.
pub struct SwitchSlots<'a> {
    // ── the render model — `core_sync::CoreSyncState`'s fields ───────────────────────────────
    /// Per-chart-key render state (`App::charts`). SPLIT-OWNED, entry by entry
    /// ([`split_plane::folds_from_snapshot`]): the snapshot-rendered (kline) entries are backend
    /// session — A's streamed bars must never paint under B — while the tape-rendered
    /// (tick/volume) entries are venue truth and survive a switch.
    pub charts: &'a mut HashMap<String, model::ChartState>,
    /// Tick/volume aggregators, `chart key -> (venue, symbol, agg)` (`App::aggs`) — local plane
    /// (folded from the venue tape, not from any backend).
    pub aggs: &'a mut HashMap<String, (String, String, TickVolAgg)>,
    /// Orderflow aggregators, same keyspace (`App::of_aggs`) — local plane, like `aggs`.
    pub of_aggs: &'a mut HashMap<String, (String, String, OrderflowAgg)>,
    /// **What the CURRENT backend publishes** (`App::published_series`) — backend session, and
    /// emphatically so: it is read by
    /// [`series_follow::follow_backend`](crate::ui::series_follow::follow_backend), which RETARGETS
    /// chart windows, and by the symbol picker, which offers its rows as one-click destinations.
    /// Carrying A's list into B's session would let a chart adopt — or an operator pick — a series
    /// the connected node does not have. Cleared; B's first snapshot rewrites it.
    pub published: &'a mut Vec<crate::ui::series_follow::PublishedSeries>,
    /// The global snapshot dirty flag (`App::last_seq`) — backend session; reset to 0 so B's
    /// first snapshot (whose `seq` may restart anywhere) always refolds from scratch.
    pub last_seq: &'a mut u64,
    /// The status line the GUI paints (`App::status`) — backend session.
    pub status: &'a mut String,
    // ── the feed slots — `feed_lifecycle::FeedSlots`'s surface (all local plane) ────────────
    /// The venue-keyed live market-data clients (`App::feeds`). Never torn down by a switch —
    /// these are LOCAL venue feeds, not backend state; even [`teardown_feed_plane`] only
    /// unsubscribes their streams (client shutdown is `App::on_exit`'s).
    pub feeds: &'a mut FeedMap,
    /// Live subscription ids (`App::subs`) — the venue streams keep flowing across a switch;
    /// [`teardown_feed_plane`] unsubscribes each against the client that issued it.
    pub subs: &'a mut HashMap<String, vike_data::SubscriptionId>,
    /// Every key the GUI has ensured (`App::spawned`) — also `sync_from_core`'s fold filter.
    /// Survives a switch: the open windows (GUI-scoped) still want exactly these series, and the
    /// fold filter must keep passing them so B's snapshots repaint the kline windows.
    pub spawned: &'a mut HashSet<String>,
    /// Keys whose venue had no client (`App::unroutable`).
    pub unroutable: &'a mut HashSet<String>,
    /// The subscribe-leg retry lane (`App::feed_retries`).
    pub feed_retries: &'a mut FeedRetries,
    // ── the backfill lane (all local plane — Binance REST + tape truth) ─────────────────────
    /// The run-once backfill gate (`App::bf_spawned`). ⚠ Insert-only in normal operation;
    /// [`teardown_feed_plane`]'s clear is the one sanctioned second way the gate reopens (the
    /// first being `bf_retries`).
    pub bf_spawned: &'a mut HashSet<String>,
    /// The backfill re-walk lane (`App::bf_retries`).
    pub bf_retries: &'a mut BackfillRetries,
    // ── the live caches — keyed `(venue, symbol)`, no backend dimension (local plane) ───────
    /// The live trade tape (`App::trades`).
    pub trades: &'a TradeStore,
    /// The live L2 books (`App::books`).
    pub books: &'a BookStore,
    /// The GUI-side bar store (`App::direct_bars`) — `Some` exactly where a plane fills one
    /// ([`split_plane::direct_bars_mount`]). Its content is cleared by [`teardown_feed_plane`]
    /// beside the tape and the books, and — under [`split_plane::BarPlane::BackendStore`] ONLY —
    /// by [`clear_session_state`] as well.
    ///
    /// ⚠ **The DOUBLE DUTY is gone.** Its presence used to BE the flag both retains fed to
    /// [`split_plane::folds_from_snapshot`]; that is now [`Self::bar_plane`], and the handle is a
    /// handle. See [`split_plane::BarPlane`].
    pub direct_bars: Option<&'a DirectBarStore>,
    /// **WHAT FILLS the bar store** ([`split_plane::BarPlane`]) — the flag BOTH retains below feed
    /// to [`split_plane::folds_from_snapshot`], so they stay complementary by construction (same
    /// slots, same plane), and the fact that decides whether a switch also empties the store.
    pub bar_plane: split_plane::BarPlane,
    /// Trade window depth-stream ledger (`App::trade_depth`), each entry carrying the subscription ids
    /// `ensure_depth` minted ([`TradeDepthSubs`]) — which is what makes a REAL teardown possible
    /// (#1379, the B2 prerequisite): [`teardown_feed_plane`] unsubscribes every id against
    /// `feeds` before dropping the map. A switch leaves the streams (and the painted ladder) alone.
    pub trade_depth: &'a mut HashMap<(String, String), TradeDepthSubs>,
    /// Polymarket cockpit stream ledger (`App::poly_subs`) — same treatment as `trade_depth`, with
    /// every unsubscribe routed to the `"polymarket"` feed `ensure_poly_book` subscribed on.
    pub poly_subs: &'a mut HashMap<String, PolyBookSubs>,
    /// Data-manager-deleted keys (`App::hidden`) — backend session, cleared DELIBERATELY: a hide
    /// is a statement about one session's data, and a stale entry would silently blank the
    /// same-named series of every later backend. The tradeoff — an operator's hide does not
    /// survive a switch — is accepted; re-hiding is one click, an invisibly blank series is a
    /// support ticket.
    pub hidden: &'a mut HashSet<String>,
    /// The feed-status line handle the bridge writes (`App::feed_status`) — reset on disconnect,
    /// handed to the new bridge on connect.
    pub feed_status: &'a Arc<Mutex<String>>,
}

/// The BACKEND-SESSION clear — exactly the [`SwitchSlots`] state the ACTIVE BACKEND's session
/// owns, and nothing the local feed plane owns. Called by [`switch_backend`] between stopping A
/// and dialing B; public so tests (and a future runtime mode exit) drive it directly.
///
/// **B2 narrowed this from B1's total clear.** Under B1 the observe GUI had no feeds, so "clear
/// everything" and "clear the backend session" were indistinguishable — every feed/tape/book slot
/// was empty anyway. The third mode (fat + `--observe`) runs live direct-to-venue feeds BESIDE
/// the backend session, and those are NOT backend state: the tape, the books, the tick/volume and
/// orderflow folds, the subscription ledgers and the Binance backfill lane are venue truth,
/// identical under any backend, so a switch must leave them running and painted — zero
/// unsubscribes, zero drains. What IS backend-session state, and is cleared:
///
/// - the **snapshot-rendered chart entries** ([`split_plane::folds_from_snapshot`] — the kline
///   family, MINUS the direct-bar-rendered series where the store is mounted: in the third mode
///   a kline chart on a [`split_plane::DIRECT_BAR_VENUES`] venue paints the VENUE's own bars
///   from the [`DirectBarStore`], so it is venue truth and survives a switch exactly like the
///   tape charts; the store's one-time backend-tail seed cannot re-import B's tail under it —
///   `DirectBarStore::seed_backend_tail` refuses any series already holding closed bars). ⚠ That
///   carve-out is the [`split_plane::BarPlane::VenueFeeds`] plane's and ONLY that plane's: under
///   [`split_plane::BarPlane::BackendStore`] the store's content is a read of THIS backend's hist
///   store, so every kline chart AND the store itself clear — see the body: their
///   bars came from backend A's streamed tail and must never paint under B's connection. The
///   per-frame `ensure_feed_on` cadence re-creates each open window's entry and the fold
///   repaints it from B's snapshots. Tape-rendered entries (tick/volume) are venue truth and
///   survive.
/// - **`last_seq`** — reset to 0 so B's first snapshot (whose `seq` may restart anywhere) always
///   refolds from scratch.
/// - **the status line**, and — B1's documented tradeoff, unchanged — **`hidden`**: a hide is a
///   statement about one session's data; a stale entry would silently blank the same-named
///   series of every later backend. Re-hiding is one click, an invisibly blank series is a
///   support ticket.
///
/// Deliberately NOT cleared, because they are GUI-scoped, not session-scoped: the window layout
/// (`App::wins` — the open windows are exactly what drives the re-ensure/refold against the new
/// backend), the display timezone, indicator favourites, tool-view state, and the workspace
/// persistence family. Clearing those would make a backend switch also a layout reset, which no
/// operator asked for. The feed plane's own teardown is [`teardown_feed_plane`] — the two
/// compose to B1's original total clear, and that composition is pinned by test.
pub fn clear_session_state(slots: &mut SwitchSlots<'_>) {
    // Backend-session chart state: exactly the snapshot-rendered entries. `retain` (not `clear`)
    // is the B2 point — a tick/volume chart's fold is the venue's tape (and a direct-bar kline
    // chart's is the venue's bar feed), not A's session, and blanking either on every switch
    // would punish the plane that did not change. The store-mounted flag comes off the SAME
    // slots the teardown reads, so the two retains cannot disagree about a key.
    let plane = slots.bar_plane;
    slots.charts.retain(|key, _| !split_plane::folds_from_snapshot(plane, key));
    // ⚠ AND THE STORE ITSELF, under the backend-store plane ONLY. There its content is bars read
    // out of THIS backend's hist store, so it is backend-session state exactly like the streamed
    // tail — leaving it would repaint A's history under B's connection, which is the whole bug
    // class this function exists for. Under `VenueFeeds` the content is VENUE truth and survives a
    // switch untouched (only `teardown_feed_plane` drops it), unchanged from B2.
    if plane == split_plane::BarPlane::BackendStore
        && let Some(bars) = slots.direct_bars
    {
        bars.clear();
    }
    *slots.last_seq = 0;
    slots.published.clear();
    slots.status.clear();
    slots.hidden.clear();

    // The snapshot-rendered render model must be provably gone when this returns — a future edit
    // that weakens the retain (or reorders something back in) should die here, not paint A's
    // bars under B.
    debug_assert!(slots.charts.keys().all(|k| !split_plane::folds_from_snapshot(plane, k)));
    debug_assert!(*slots.last_seq == 0);
    debug_assert!(slots.published.is_empty());
    debug_assert!(slots.status.is_empty());
}

/// The FEED-PLANE teardown — the [`SwitchSlots`] complement of [`clear_session_state`]: stop
/// every local venue stream through the ledgers (#1379's discipline — each minted id
/// unsubscribed against the client that issued it) and clear every local-plane slot. Composing
/// the two IS B1's total clear, field for field; the composition test pins that no slot has
/// silently fallen between them.
///
/// **No runtime path calls this today, and that is stated rather than hidden.** The arm is a
/// startup fact ([`split_plane::AppMode`] — build feature + `--observe` while there were two
/// builds; one constant arm since), so "leaving observe
/// mode" only happens at process exit, where `App::on_exit`'s bounded teardown `shutdown()`s
/// every feed client outright — a stronger stop than per-id unsubscribes. This function exists
/// so the ledger discipline has one named, CI-tested owner for the day a runtime mode switch (or
/// a "stop the local feeds" control) arrives, and so the switch clear above could go PARTIAL
/// without B1's teardown coverage rotting.
pub fn teardown_feed_plane(slots: &mut SwitchSlots<'_>) {
    // Every live subscription id dies against the SAME venue client it was issued by (the key
    // prefix carries the venue — `venue_of_key`).
    for (key, id) in slots.subs.drain() {
        if let Some(feed) = slots.feeds.get_mut(venue_of_key(&key)) {
            feed.unsubscribe(id);
        }
    }
    // The depth/cockpit ledgers get the same unsubscribe-before-drop (their entries' venue is the
    // key's own first element / the fixed "polymarket" feed — the same routing
    // `feed_lifecycle::reap_orphaned_trade_cockpit_streams` uses per frame). A venue with no
    // registered client is tolerated, exactly as it is there.
    for ((venue, _inst), sub) in slots.trade_depth.drain() {
        if let Some(feed) = slots.feeds.get_mut(venue.as_str()) {
            feed.unsubscribe(sub.depth);
            if let Some(trades) = sub.trades {
                feed.unsubscribe(trades);
            }
        }
    }
    for (_token, sub) in slots.poly_subs.drain() {
        if let Some(feed) = slots.feeds.get_mut("polymarket") {
            feed.unsubscribe(sub.book);
            if let Some(trades) = sub.trades {
                feed.unsubscribe(trades);
            }
        }
    }
    slots.spawned.clear();
    slots.unroutable.clear();
    slots.feed_retries.reset();

    // The tape- and store-rendered render model (the snapshot-rendered half is
    // `clear_session_state`'s) — the same predicate, mirrored, off the same slots.
    let plane = slots.bar_plane;
    slots.charts.retain(|key, _| split_plane::folds_from_snapshot(plane, key));
    slots.aggs.clear();
    slots.of_aggs.clear();

    // The backfill lane.
    slots.bf_spawned.clear();
    slots.bf_retries.reset();

    // The live caches. (The two stream ledgers were already drained — with their streams
    // stopped — beside `subs` above.)
    slots.trades.clear();
    slots.books.clear();
    if let Some(bars) = slots.direct_bars {
        bars.clear(); // venue truth, same family as the tape/books — feed-plane state
    }
}

/// THE SWITCH ROUTINE (B1's safety-critical piece): stop the old backend, run the
/// [backend-session clear](clear_session_state), blank the published snapshot, and dial the new
/// record — or, with `target: None`, disconnect and stay down. The local feed plane is
/// deliberately untouched (B2): feeds are not backend-session state, so the tape, books, ladders and
/// tick/volume folds keep painting straight through a switch.
///
/// Ordering is load-bearing:
///
/// 1. **Stop A's bridge FIRST** ([`BridgeHandle::stop`] — raises the flag and JOINS), so no
///    late publish from A's thread can land after step 3's blank. Dropping the [`BackendConn`]
///    also drops A's control channel (socket shut, worker joined).
/// 2. **Backend-session clear** — [`clear_session_state`]: every field the session owns.
/// 3. **Publish an empty snapshot** into the GUI's cell, so the render loop never paints A's
///    data under B's (or no) connection — not even for the frames B spends connecting.
/// 4. **Dial B** (when `target` is `Some`) via [`connect_backend`] — same gates, same closures.
pub fn switch_backend<P, R>(
    active: &mut Option<BackendConn>,
    target: Option<&BackendRecord>,
    vars: &HashMap<String, String>,
    master_control: bool,
    mut slots: SwitchSlots<'_>,
    publish: P,
    repaint: R,
) where
    P: Fn(Arc<vike_exec::CoreSnapshot>) + Send + 'static,
    R: Fn() + Send + 'static,
{
    if let Some(mut old) = active.take() {
        // Explicit for the reader; `drop(old)` alone would do both (stop is idempotent).
        old.bridge.stop();
    }
    clear_session_state(&mut slots);
    publish(Arc::new(vike_exec::CoreSnapshot::empty("observing", "")));
    repaint();
    match target {
        Some(record) => {
            let status = Arc::clone(slots.feed_status);
            *active = Some(connect_backend(record, vars, master_control, status, publish, repaint));
        }
        None => {
            if let Ok(mut s) = slots.feed_status.lock() {
                *s = "disconnected".to_string();
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
pub struct PickerRow<'a> {
    /// The record this row shows.
    pub record: &'a BackendRecord,
    /// This row is the live connection.
    pub is_active: bool,
    /// This row is a `backends.json` entry (vs the unlisted active connection).
    pub listed: bool,
}

/// The picker's row list: every registry record in file order (marked active where it matches the
/// live connection's record), preceded by the active connection itself when the registry does not
/// list it — the `--observe ADDR` synthetic record case.
pub fn picker_rows<'a>(
    file: &'a BackendsFile,
    active: Option<&'a BackendRecord>,
) -> Vec<PickerRow<'a>> {
    let mut rows = Vec::with_capacity(file.backends.len() + 1);
    if let Some(a) = active
        && !file.backends.contains(a)
    {
        rows.push(PickerRow { record: a, is_active: true, listed: false });
    }
    for record in &file.backends {
        rows.push(PickerRow { record, is_active: active == Some(record), listed: true });
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
pub fn registry_rows<'a>(
    file: &'a BackendsFile,
    active: Option<&'a BackendRecord>,
) -> Vec<PickerRow<'a>> {
    picker_rows(file, active).into_iter().filter(|r| r.listed).collect()
}

/// Is the live connection one the registry does not list — the `--observe ADDR` synthetic record?
///
/// The Backend tab renders no ROW for it ([`registry_rows`]); this is what it asks in order to say
/// so in a sentence instead, and to point at the strip that does render it.
#[must_use]
pub fn active_is_unlisted(file: &BackendsFile, active: Option<&BackendRecord>) -> bool {
    active.is_some_and(|a| !file.backends.contains(a))
}

/// The pure click decision: clicking the ACTIVE row disconnects, clicking any other row connects
/// to it (which [`switch_backend`] makes an implicit disconnect-then-connect).
pub fn click_action(clicked: &BackendRecord, active: Option<&BackendRecord>) -> BackendAction {
    if active == Some(clicked) {
        BackendAction::Disconnect
    } else {
        BackendAction::Connect(clicked.clone())
    }
}

/// THE STARTUP LADDER: which backend a launch would connect to, as a whole RECORD — with the
/// registry answer supplied, so every rung is testable without a file on disk and the tests drive
/// the shipped decision rather than a copy. [`startup_observe`] is the impure entry point that
/// supplies it (and the ONLY one — see [`StartupObserve`] for why answering the address question
/// alone was a P1).
///
/// `flag` is the `--observe` ARGUMENT, scanned out of argv by [`startup_observe_from`] (the shell's
/// job stays in the shell; nothing here reads the environment). The rungs, in order:
///
/// 1. **The FLAG**, when non-blank — an OVERRIDE, and it becomes the synthetic
///    [`cli_observe_record`], so `--observe ADDR` keeps meaning exactly what it always meant: env
///    key names, control armed by the master gate.
/// 2. **The registry's ACTIVE record** ([`crate::backend::backend_registry::active_record`]) — answered as
///    ITSELF, never re-synthesized. That record is the only place its `observe_key`/`control_key`
///    NAMES live, and a launch that kept only its address signed with the fixed
///    [`backend_registry::OBSERVE_KEY_NAME`] and failed `bad mac` forever (that function's doc carries the
///    measurement).
/// 3. **The local default** ([`crate::backend::backend_registry::DEFAULT_OBSERVE_ADDR`]), synthetic like the
///    flag.
///
/// ⚠ There is no `None` rung. `--observe <host>:<port>` used to be MANDATORY in a thin build and
/// its absence EXITED — so the viewer asked a person to retype a flag naming the one thing that
/// binary can do, and a typo closed the window instead of opening it. A viewer that opens and says
/// "not connected" in its status bar is strictly better: nothing here can place an order, so a
/// wrong address costs a reconnect line and nothing else.
///
/// ⚠ **That is an answer to "what address would this launch watch", and it is NOT an answer to
/// "is this launch an observer at all"** — [`StartupObserve::requested`] is. Reading rung 3 as the
/// second question is the regression that record documents.
///
/// ⚠ `config.tradehub_addr` is deliberately NOT a rung, and the attempt to make one is worth
/// recording: that setting is the DAEMON's BIND address (`0.0.0.0:9099` on the box being watched),
/// which a client cannot dial. A viewer's address is a different fact and belongs here.
#[must_use]
pub fn startup_backend_from(flag: Option<&str>, active: Option<BackendRecord>) -> BackendRecord {
    if let Some(addr) = flag.map(str::trim).filter(|a| !a.is_empty()) {
        return cli_observe_record(addr);
    }
    if let Some(record) = active {
        return record;
    }
    let addr = crate::backend::backend_registry::DEFAULT_OBSERVE_ADDR;
    tracing::info!(
        addr,
        "no --observe and no active backend — watching the local default. Add or pick a node in \
         Connections, or pass --observe <host>:<port>."
    );
    cli_observe_record(addr)
}

/// The `--observe` flag as argv spells it. A separate constant because two different questions read
/// it — the ADDRESS ladder ([`startup_backend_from`]) and the MODE ([`StartupObserve::requested`]) —
/// and a launch that spelled the word without an address must still answer the second one.
pub const OBSERVE_FLAG: &str = "--observe";

/// The WHOLE startup answer: the backend a launch would watch, AND whether observing was actually
/// REQUESTED. One struct, from one [`crate::backend::backend_registry::active_record`] read, because the two
/// facts are read together and a launch whose mode disagreed with its address would be
/// unexplainable.
///
/// # Why `requested` exists (the P1 this type was added to make impossible)
///
/// [`startup_backend_from`]'s ladder deliberately has no `None` rung: the Connections UI needs an
/// address to show, so a launch with no flag and no configured node still resolves
/// [`crate::backend::backend_registry::DEFAULT_OBSERVE_ADDR`]. `crates/vike-desktop/src/main.rs` wrapped that
/// answer in `Some` UNCONDITIONALLY and handed it to `App::new`, whose `if let Some(..)` is the
/// OBSERVER ARM — so from #1610 (2026-09-03) every ordinary desktop launch took that arm: no local
/// core, no exec engines, no recorder, no journal materializer, no recon driver, and
/// `crates/vike-desktop/src/app_ui.rs`'s `dispatch` resolved to the read-only no-op. **The GUI could
/// not place an order at all**, for three days, while its status bar retried a daemon nobody was
/// running. The address ladder was right for the question it answers; nothing asked the other one.
///
/// So the mode question is answered HERE and nowhere else, and
/// [`split_plane::observes`](crate::backend::split_plane::observes) turns it into whether a launch
/// observes (it fed the `observing` argument of `split_plane::app_mode`, the arm table deleted with
/// the local-core arm in 2026-10).
pub struct StartupObserve {
    /// The record a connection would be dialled from — [`startup_backend_from`]'s ladder, which
    /// always answers, so the address exists whatever `requested` says.
    pub record: BackendRecord,
    /// Was observing actually ASKED FOR: the `--observe` word on the command line (with or without
    /// an address — the word alone means "the configured node"), or an active registry record.
    /// `false` is the ordinary fat launch, which runs a LOCAL trading core and dials no backend.
    pub requested: bool,
}

/// [`StartupObserve`] for this process: scan `argv` for `--observe [ADDR]` and answer both facts off
/// ONE [`crate::backend::backend_registry::active_record`] read.
#[must_use]
pub fn startup_observe(argv: impl IntoIterator<Item = String>) -> StartupObserve {
    startup_observe_from(argv, crate::backend::backend_registry::active_record())
}

/// [`startup_observe`]'s PURE half — argv as a parameter and the registry answer supplied, so every
/// combination is testable without a process or a file on disk (the binary passes
/// `std::env::args()`). The BARE-flag case is exactly why the scan moved in here from the
/// CI-excluded shell: `--observe` as the last word on the line yields no address at all, so it
/// contributes no rung to [`startup_backend_from`] while still being a request to observe.
#[must_use]
pub fn startup_observe_from(
    argv: impl IntoIterator<Item = String>,
    active: Option<BackendRecord>,
) -> StartupObserve {
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
    StartupObserve { record: startup_backend_from(flag.as_deref(), active), requested }
}

/// The registry's `active` pointer after one picker action — the "configure once, launch bare
/// afterwards" half of [`crate::backend::backend_registry::active_addr`].
///
/// ⚠ This is the ONLY thing that SETS that pointer. `backend_editor` retargets it on a rename and
/// clears it on a delete, and nothing else touched it: a node could be added in the Connections
/// tool, connected to, and watched all session, and the pointer stayed `None` — so the next bare
/// launch fell through to [`crate::backend::backend_registry::DEFAULT_OBSERVE_ADDR`] and silently observed a
/// different daemon. Measured against a three-node setup on 2026-09-03: connecting to a `the CI box`
/// record left `"active": null` on disk, and the relaunch dialled the local default.
///
/// Connect names the record only when the registry LISTS it. The `--observe` connection is
/// unlisted by construction ([`picker_rows`]'s `listed: false` row), and a pointer at a record
/// `backends` does not hold is an orphan [`crate::backend::backend_registry::active_addr`] refuses anyway.
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

#[path = "backend_conn_tests.rs"]
#[cfg(test)]
mod backend_conn_tests;
