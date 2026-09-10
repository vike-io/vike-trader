//! `backend_conn` — RUNTIME-MUTABLE backend selection for the observe GUI (split-plane B1): one
//! [`BackendConn`] per live connection, [`connect_backend`] to build it from a
//! [`BackendRecord`](crate::backend_registry::BackendRecord), and [`switch_backend`] — the
//! safety-critical BACKEND-SESSION CLEAR that keeps backend A's data from ever painting under
//! backend B's connection. (B1 shipped it as a total clear; B2 split the surface into the backend
//! session, cleared on every switch, and the local feed plane — venue truth the third mode runs
//! beside the session — which survives a switch and has its own teardown,
//! [`teardown_feed_plane`]. The two compose to B1's total clear, pinned by test.)
//!
//! Before B1 the observe connection was a process-lifetime constant: `--observe ADDR` wired one
//! bridge + one optional control channel in `vike-app`'s `App::new` and nothing could ever change
//! it. This module lifts that wiring out of the CI-excluded `main.rs` (vike-app is compile-checked
//! but never tested — anything in `main.rs` is untestable by construction) so every decision runs
//! in a gate:
//!
//! - **One active backend at a time** (the spec's one-active-backend render model). Switching
//!   replaces the whole [`BackendConn`]; there is no per-backend series keyspace (deferred I15),
//!   so a switch must CLEAR AND REFOLD rather than re-key.
//! - **Key resolution strictly through
//!   [`backend_registry::resolve_keys`](crate::backend_registry::resolve_keys)** (B9): the record
//!   holds credential-store KEY NAMES, resolution to bytes happens here at connect time, and an
//!   unarmed record can never mount a control channel.
//! - **The process-level master gate stays** —
//!   [`tradehub_control::control_enabled`](crate::tradehub_control::control_enabled) — as an AND
//!   over the per-backend arming. Defense in depth for a write path: the record file
//!   (`backends.json`) is GUI-owned and editable by anything that can write the profile directory,
//!   so a record flipping `control = true` must not be sufficient on its own to arm REAL order
//!   placement; the operator's process-level opt-in (`VIKE_TRADEHUB_CONTROL=1`) is still required,
//!   exactly as it was when the flag was the ONLY gate.
//! - **The fat local-core path is untouched**: [`switching_available`] answers `false` while a
//!   local core runs, and a process started without `--observe` and without a configured backend
//!   behaves byte-identically to before B1. Mixing planes is the THIRD MODE (B2, fat +
//!   `--observe` — [`split_plane::app_mode`](crate::split_plane::app_mode)): a remote backend's
//!   account plane beside local venue feeds, with NO local core — so switching stays available
//!   there, and the switch leaves the feed plane alone.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

use crate::backend_registry::{self, BackendRecord, BackendsFile};
use crate::data_sink::{BookStore, DirectBarStore, TradeStore};
use crate::feed_lifecycle::{
    BackfillReport, BackfillRetries, DomDepthSubs, FeedMap, FeedRetries, PolyBookSubs,
};
use crate::observe_bridge::{self, BridgeHandle};
use crate::orderflow::OrderflowAgg;
use crate::split_plane;
use crate::tickvol::TickVolAgg;
use crate::venue_routing::venue_of_key;
use vike_chart::model;

// ⚠ The `--observe` path's two key names USED to be a third crate-local pair here
// (`backend_registry::OBSERVE_KEY_NAME` / `backend_registry::CONTROL_KEY_NAME`). They were byte-identical to
// [`backend_registry::OBSERVE_KEY_NAME`] / [`backend_registry::CONTROL_KEY_NAME`] and, unlike those,
// bought nothing: the settings scanner merges constants per CRATE, so a sibling module's constant
// still resolves for it (`settings_registry.rs`'s `cross_file_consts_resolve_within_a_crate` is the
// regression test). This pair also had no `.get(` site anywhere — it only ever WROTE record fields —
// so removing it cannot touch MAP_LOOKUP_PROVEN. The workspace-wide duplication that DOES buy
// something is documented on `vike_tradehub_client::auth::OBSERVE_KEY_ENV`, the reference spelling.

/// One live backend connection: the record it was dialed from, the observe bridge, and the
/// optional Scope::Control write channel.
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
    /// The Scope::Control (write-plane) channel — `Some` only when BOTH gates armed it (see
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
    }
}

/// Dial `record` and build its [`BackendConn`]: spawn the observe bridge (self-healing reconnect
/// loop — never blocks the caller) and, iff BOTH control gates pass, the Scope::Control channel.
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
///    [`tradehub_control::control_enabled`](crate::tradehub_control::control_enabled) (or its
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
    P: Fn(Arc<vike_core::CoreSnapshot>) + Send + 'static,
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
    // credential store — observe server NOT started (absent credential is the gate)` and declines.
    // This is the client's half of the same sentence.
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
             an EMPTY key and the node will answer `bad mac` on every attempt. The key is read from \
             the credential store (<project>/settings/secrets.env) — an environment variable alone \
             does not reach it unless the store is where this process resolves one. It must match \
             the daemon's own key byte for byte.",
            record.observe_key
        );
    }

    let bridge = observe_bridge::spawn_bridge(
        record.addr.clone(),
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
/// borrow-bundle shape of [`core_sync::CoreSyncState`](crate::core_sync::CoreSyncState) and
/// [`feed_lifecycle::FeedSlots`](crate::feed_lifecycle::FeedSlots), widened to every store either
/// side of them (the measured fan-out of `sync_from_core`'s render model — six fields; a prior
/// audit named 11 sites that silently show stale data when one is missed).
///
/// **Since B2 the surface is split by OWNER, not cleared wholesale.** The third mode runs live
/// venue feeds beside the backend session, so each field belongs to exactly one of two planes:
///
/// - **backend session** ([`clear_session_state`], runs on every switch): the snapshot-rendered
///   chart entries, `last_seq`, the status line, `hidden`;
/// - **local feed plane** ([`teardown_feed_plane`], the mode-exit teardown): everything else —
///   the feed clients and their three subscription ledgers, the tape-rendered folds, the trade
///   tape, the books, and the whole Binance backfill lane. Venue truth, identical under any
///   backend — a switch leaves this plane running and painted.
///
/// The two routines compose to B1's original total clear, field for field, and the composition
/// is pinned by test so no field can silently fall between them.
pub struct SwitchSlots<'a> {
    // ── the render model — `core_sync::CoreSyncState`'s six fields ──────────────────────────
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
    /// Staged undeliverable backfill batches, keyed by BARE SYMBOL (`App::bf_pending`) — local
    /// plane: the ticks are Binance REST truth destined for `of_aggs`, which also survives, so a
    /// held batch stays deliverable across a switch.
    pub bf_pending: &'a mut HashMap<String, Vec<vike_model::TradeTick>>,
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
    /// The live feed's earliest-emitted-aggTrade-id map (`App::earliest_live_ids`) — per-symbol
    /// paging boundaries of the LIVE VENUE tape; backend-independent by construction.
    pub earliest_live_ids: &'a Mutex<HashMap<String, u64>>,
    /// In-flight backfill batches (`App::bf_rx`) — venue truth; a switch leaves them deliverable,
    /// the feed-plane teardown drains them.
    pub bf_rx: &'a Receiver<(String, Vec<vike_model::TradeTick>)>,
    /// In-flight backfill exit reports (`App::bf_done_rx`).
    pub bf_done_rx: &'a Receiver<BackfillReport>,
    // ── the live caches — keyed `(venue, symbol)`, no backend dimension (local plane) ───────
    /// The live trade tape (`App::trades`).
    pub trades: &'a TradeStore,
    /// The live L2 books (`App::books`).
    pub books: &'a BookStore,
    /// The direct-bar store (`App::direct_bars`) — `Some` exactly in the third mode
    /// ([`split_plane::direct_bars_mount`]), and DOUBLE-DUTY here: its presence is the
    /// `direct_bars` flag both clears feed to [`split_plane::folds_from_snapshot`] (so the two
    /// retains stay complementary by construction — same slots, same flag), and its content is
    /// venue truth cleared by [`teardown_feed_plane`] beside the tape and the books. A switch
    /// leaves it painting: in the third mode a direct-bar kline chart shows the VENUE's bars, so
    /// backend A's session never owned them.
    pub direct_bars: Option<&'a DirectBarStore>,
    /// DOM depth-stream ledger (`App::dom_depth`), each entry carrying the subscription ids
    /// `ensure_depth` minted ([`DomDepthSubs`]) — which is what makes a REAL teardown possible
    /// (#1379, the B2 prerequisite): [`teardown_feed_plane`] unsubscribes every id against
    /// `feeds` before dropping the map. A switch leaves the streams (and the painted DOM) alone.
    pub dom_depth: &'a mut HashMap<(String, String), DomDepthSubs>,
    /// Polymarket cockpit stream ledger (`App::poly_subs`) — same treatment as `dom_depth`, with
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
///   `DirectBarStore::seed_backend_tail` refuses any series already holding closed bars): their
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
    let direct = slots.direct_bars.is_some();
    slots.charts.retain(|key, _| !split_plane::folds_from_snapshot(direct, key));
    *slots.last_seq = 0;
    slots.status.clear();
    slots.hidden.clear();

    // The snapshot-rendered render model must be provably gone when this returns — a future edit
    // that weakens the retain (or reorders something back in) should die here, not paint A's
    // bars under B.
    debug_assert!(slots.charts.keys().all(|k| !split_plane::folds_from_snapshot(direct, k)));
    debug_assert!(*slots.last_seq == 0);
    debug_assert!(slots.status.is_empty());
}

/// The FEED-PLANE teardown — the [`SwitchSlots`] complement of [`clear_session_state`]: stop
/// every local venue stream through the ledgers (#1379's discipline — each minted id
/// unsubscribed against the client that issued it) and clear every local-plane slot. Composing
/// the two IS B1's total clear, field for field; the composition test pins that no slot has
/// silently fallen between them.
///
/// **No runtime path calls this today, and that is stated rather than hidden.** The arm is a
/// startup fact ([`split_plane::app_mode`] — build feature + `--observe`), so "leaving observe
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
    // The DOM/cockpit ledgers get the same unsubscribe-before-drop (their entries' venue is the
    // key's own first element / the fixed "polymarket" feed — the same routing
    // `feed_lifecycle::reap_orphaned_dom_cockpit_streams` uses per frame). A venue with no
    // registered client is tolerated, exactly as it is there.
    for ((venue, _canonical), sub) in slots.dom_depth.drain() {
        if let Some(feed) = slots.feeds.get_mut(venue.as_str()) {
            feed.unsubscribe(sub.depth);
            if let Some(bars) = sub.bars {
                feed.unsubscribe(bars);
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

    // The tape- and direct-bar-rendered render model (the snapshot-rendered half is
    // `clear_session_state`'s) — the same predicate, mirrored, off the same slots.
    let direct = slots.direct_bars.is_some();
    slots.charts.retain(|key, _| split_plane::folds_from_snapshot(direct, key));
    slots.aggs.clear();
    slots.of_aggs.clear();
    slots.bf_pending.clear();

    // The backfill lane, including everything in flight.
    slots.bf_spawned.clear();
    slots.bf_retries.reset();
    slots.earliest_live_ids.lock().unwrap().clear();
    while slots.bf_rx.try_recv().is_ok() {}
    while slots.bf_done_rx.try_recv().is_ok() {}

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
/// deliberately untouched (B2): feeds are not backend-session state, so the tape, books, DOM and
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
    P: Fn(Arc<vike_core::CoreSnapshot>) + Send + 'static,
    R: Fn() + Send + 'static,
{
    if let Some(mut old) = active.take() {
        // Explicit for the reader; `drop(old)` alone would do both (stop is idempotent).
        old.bridge.stop();
    }
    clear_session_state(&mut slots);
    publish(Arc::new(vike_core::CoreSnapshot::empty("observing", "")));
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
/// applied by `vike-app` after the frame (the same OUT-slot idiom as every other tool action).
#[derive(Debug, Clone, PartialEq)]
pub enum BackendAction {
    /// Dial this record (stopping the current backend first — [`switch_backend`]).
    Connect(BackendRecord),
    /// Stop the current backend and stay disconnected.
    Disconnect,
}

/// Is runtime backend switching available at all? `false` while a LOCAL trading core runs — the
/// picker renders but its buttons are inert, and the fat no-`--observe` path stays byte-identical
/// to pre-B1. The THIRD MODE (B2, fat + `--observe`) mounts local FEEDS but no local CORE, so
/// switching is available there: the feed plane is not backend state
/// ([`clear_session_state`] leaves it alone), and the account plane it switches is the remote
/// backend's.
pub fn switching_available(has_local_core: bool) -> bool {
    !has_local_core
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
/// 2. **The registry's ACTIVE record** ([`crate::backend_registry::active_record`]) — answered as
///    ITSELF, never re-synthesized. That record is the only place its `observe_key`/`control_key`
///    NAMES live, and a launch that kept only its address signed with the fixed
///    [`backend_registry::OBSERVE_KEY_NAME`] and failed `bad mac` forever (that function's doc carries the
///    measurement).
/// 3. **The local default** ([`crate::backend_registry::DEFAULT_OBSERVE_ADDR`]), synthetic like the
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
    let addr = crate::backend_registry::DEFAULT_OBSERVE_ADDR;
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
/// REQUESTED. One struct, from one [`crate::backend_registry::active_record`] read, because the two
/// facts are read together and a launch whose mode disagreed with its address would be
/// unexplainable.
///
/// # Why `requested` exists (the P1 this type was added to make impossible)
///
/// [`startup_backend_from`]'s ladder deliberately has no `None` rung: the Connections UI needs an
/// address to show, so a launch with no flag and no configured node still resolves
/// [`crate::backend_registry::DEFAULT_OBSERVE_ADDR`]. `crates/vike-desktop/src/main.rs` wrapped that
/// answer in `Some` UNCONDITIONALLY and handed it to `App::new`, whose `if let Some(..)` is the
/// OBSERVER ARM — so from #1610 (2026-09-03) every ordinary desktop launch took that arm: no local
/// core, no exec engines, no recorder, no journal materializer, no recon driver, and
/// `crates/vike-desktop/src/app_ui.rs`'s `dispatch` resolved to the read-only no-op. **The GUI could
/// not place an order at all**, for three days, while its status bar retried a daemon nobody was
/// running. The address ladder was right for the question it answers; nothing asked the other one.
///
/// So the mode question is answered HERE and nowhere else, and
/// [`split_plane::observes`](crate::split_plane::observes) turns it into the `observing` argument
/// [`split_plane::app_mode`](crate::split_plane::app_mode) takes.
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
/// ONE [`crate::backend_registry::active_record`] read.
#[must_use]
pub fn startup_observe(argv: impl IntoIterator<Item = String>) -> StartupObserve {
    startup_observe_from(argv, crate::backend_registry::active_record())
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
/// afterwards" half of [`crate::backend_registry::active_addr`].
///
/// ⚠ This is the ONLY thing that SETS that pointer. `backend_editor` retargets it on a rename and
/// clears it on a delete, and nothing else touched it: a node could be added in the Connections
/// tool, connected to, and watched all session, and the pointer stayed `None` — so the next bare
/// launch fell through to [`crate::backend_registry::DEFAULT_OBSERVE_ADDR`] and silently observed a
/// different daemon. Measured against a three-node setup on 2026-09-03: connecting to a `the CI box`
/// record left `"active": null` on disk, and the relaunch dialled the local default.
///
/// Connect names the record only when the registry LISTS it. The `--observe` connection is
/// unlisted by construction ([`picker_rows`]'s `listed: false` row), and a pointer at a record
/// `backends` does not hold is an orphan [`crate::backend_registry::active_addr`] refuses anyway.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use vike_data::{DataClient, LiveDataError, SubscriptionId};

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn rec(name: &str, control: bool) -> BackendRecord {
        BackendRecord {
            name: name.to_string(),
            addr: "127.0.0.1:1".to_string(), // closed port: dials fail fast, nothing listens
            observe_key: "PROD2_OBSERVE_KEY".to_string(),
            control_key: Some("PROD2_CONTROL_KEY".to_string()),
            control,
        }
    }

    fn status() -> Arc<Mutex<String>> {
        Arc::new(Mutex::new(String::new()))
    }

    /// The `--observe ADDR` synthetic record reproduces today's env-key behavior exactly: observe
    /// key from `VIKE_TRADEHUB_OBSERVE_KEY`, control key from `VIKE_TRADEHUB_CONTROL_KEY`, and
    /// the record ARMED so the process-level master gate stays the ONLY gate on the CLI path.
    #[test]
    fn the_synthetic_observe_record_reproduces_todays_env_key_behavior() {
        let r = cli_observe_record("the CI box.example:9040");
        assert_eq!(r.addr, "the CI box.example:9040");
        assert_eq!(r.name, "", "synthetic record is unnamed — never persisted");
        assert_eq!(r.observe_key, backend_registry::OBSERVE_KEY_NAME);
        assert_eq!(r.control_key.as_deref(), Some(backend_registry::CONTROL_KEY_NAME));
        assert!(
            r.control,
            "CLI record must be ARMED: pre-B1, VIKE_TRADEHUB_CONTROL=1 + the key's presence decided alone"
        );

        // Resolution over the same map `App::new` always read: both present -> both resolve;
        // control key absent -> read-only (exactly `vars.get(..)` before B1); empty map -> none.
        let both = vars(&[
            (backend_registry::OBSERVE_KEY_NAME, "obs"),
            (backend_registry::CONTROL_KEY_NAME, "ctl"),
        ]);
        assert_eq!(backend_registry::resolve_keys(&r, &both), (Some("obs"), Some("ctl")));
        let observe_only = vars(&[(backend_registry::OBSERVE_KEY_NAME, "obs")]);
        assert_eq!(backend_registry::resolve_keys(&r, &observe_only), (Some("obs"), None));
        assert_eq!(backend_registry::resolve_keys(&r, &vars(&[])), (None, None));
    }

    /// `connect_backend` resolves keys per record and refuses the control channel when EITHER
    /// gate is down: an unarmed record under the master gate, and an armed record without it.
    /// (Both refuse-paths need no network — nothing dials a control socket. The bridge thread
    /// does dial the dead observe addr in the background; dropping the conn stops-and-joins it.)
    #[test]
    fn connect_backend_refuses_control_when_either_gate_is_down() {
        let m = vars(&[("PROD2_OBSERVE_KEY", "obs"), ("PROD2_CONTROL_KEY", "ctl")]);

        // Per-backend gate down (record unarmed), master up: read-only.
        let conn = connect_backend(&rec("unarmed", false), &m, true, status(), |_| {}, || {});
        assert!(conn.ctrl.is_none(), "unarmed record must never mount control");
        assert_eq!(conn.record, rec("unarmed", false));
        drop(conn); // stop + join the bridge thread

        // Master gate down, record armed: read-only.
        let conn = connect_backend(&rec("armed", true), &m, false, status(), |_| {}, || {});
        assert!(
            conn.ctrl.is_none(),
            "master gate off must refuse control even for an armed record"
        );
        drop(conn);

        // Both gates up but no daemon at the addr: the connect fails and the observer DEGRADES to
        // read-only rather than erroring — the same contract `connect_control` always had.
        let conn = connect_backend(&rec("armed", true), &m, true, status(), |_| {}, || {});
        assert!(conn.ctrl.is_none(), "a dead daemon degrades to read-only, never Some");
        drop(conn);
    }

    // ── the switch routine ──────────────────────────────────────────────────────────────────

    type Log = Arc<Mutex<Vec<String>>>;

    /// A recording `DataClient` double — enough of `feed_lifecycle`'s `FakeFeed` to prove the
    /// unsubscribe routing (that module's double is `#[cfg(test)]`-private to it).
    struct RecClient {
        venue: &'static str,
        log: Log,
    }

    impl DataClient for RecClient {
        fn subscribe_bars(&mut self, _: &str, _: &str) -> Result<SubscriptionId, LiveDataError> {
            Ok(SubscriptionId(1))
        }
        fn subscribe_quotes(&mut self, _: &str) -> Result<SubscriptionId, LiveDataError> {
            Ok(SubscriptionId(2))
        }
        fn subscribe_trades(&mut self, _: &str) -> Result<SubscriptionId, LiveDataError> {
            Ok(SubscriptionId(3))
        }
        fn subscribe_book(&mut self, _: &str) -> Result<SubscriptionId, LiveDataError> {
            Ok(SubscriptionId(4))
        }
        fn unsubscribe(&mut self, id: SubscriptionId) {
            self.log.lock().unwrap().push(format!("{}:unsub {}", self.venue, id.0));
        }
        fn shutdown(&mut self) {
            self.log.lock().unwrap().push(format!("{}:shutdown", self.venue));
        }
    }

    /// Owns every field of [`SwitchSlots`], POPULATED — the CoreSyncState-shaped fixture the
    /// brief asks for, widened to the whole clear surface.
    struct Fixture {
        charts: HashMap<String, model::ChartState>,
        aggs: HashMap<String, (String, String, TickVolAgg)>,
        of_aggs: HashMap<String, (String, String, OrderflowAgg)>,
        bf_pending: HashMap<String, Vec<vike_model::TradeTick>>,
        last_seq: u64,
        status_line: String,
        feeds: FeedMap,
        subs: HashMap<String, SubscriptionId>,
        spawned: HashSet<String>,
        unroutable: HashSet<String>,
        feed_retries: FeedRetries,
        bf_spawned: HashSet<String>,
        bf_retries: BackfillRetries,
        earliest_live_ids: Mutex<HashMap<String, u64>>,
        /// Held so the backfill channel stays CONNECTED and the drain sees Empty after the queued
        /// batch. ⚠ This used to say "exactly like the live App, which owns its sender for the
        /// process life" — no longer: the desktop shell's producers left with its local market-data
        /// plane, and `crates/vike-desktop/src/main.rs`'s `dead_receiver` drops the sender on the
        /// spot, so the LIVE drain sees Disconnected on every frame. Both answers end the drain
        /// loop identically; this fixture keeps a sender only so a batch can be queued for the
        /// switch to prove it survives.
        _bf_tx: std::sync::mpsc::Sender<(String, Vec<vike_model::TradeTick>)>,
        bf_rx: Receiver<(String, Vec<vike_model::TradeTick>)>,
        bf_done_rx: Receiver<BackfillReport>,
        trades: TradeStore,
        books: BookStore,
        direct_bars: DirectBarStore,
        dom_depth: HashMap<(String, String), DomDepthSubs>,
        poly_subs: HashMap<String, PolyBookSubs>,
        hidden: HashSet<String>,
        feed_status: Arc<Mutex<String>>,
        log: Log,
    }

    fn tick(symbol: &str) -> vike_model::TradeTick {
        vike_model::TradeTick {
            ts: 1,
            local_ts: 1,
            price: 100.0,
            size: 1.0,
            is_buyer_maker: false,
            symbol: symbol.to_string(),
        }
    }

    fn populated() -> Fixture {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let mut feeds: FeedMap = HashMap::new();
        feeds.insert("binance", Box::new(RecClient { venue: "binance", log: Arc::clone(&log) }));
        feeds.insert("okx", Box::new(RecClient { venue: "okx", log: Arc::clone(&log) }));
        feeds.insert(
            "polymarket",
            Box::new(RecClient { venue: "polymarket", log: Arc::clone(&log) }),
        );

        let mut charts = HashMap::new();
        // The fixture is THIRD-MODE shaped (feeds + backend + the direct-bar store), so the
        // session/plane SPLIT has three observable chart families:
        // - `deribit:BTC-PERPETUAL@1m` — a kline on a venue with NO local bar feed:
        //   snapshot-rendered, backend A's streamed tail — the switch clear must drop it;
        // - `BTCUSDT@1m` — a kline on a DIRECT_BAR_VENUES venue: rendered from the venue-fed
        //   DirectBarStore (B2's direct-bar follow-up), venue truth — a switch must KEEP it;
        // - `BTCUSDT@100t` — tape-rendered, venue truth — kept, as since B2.
        charts.insert("deribit:BTC-PERPETUAL@1m".to_string(), model::ChartState::default());
        charts.insert("BTCUSDT@1m".to_string(), model::ChartState::default());
        charts.insert("BTCUSDT@100t".to_string(), model::ChartState::default());
        let mut aggs = HashMap::new();
        aggs.insert(
            "BTCUSDT@100t".to_string(),
            (
                "binance".to_string(),
                "BTCUSDT".to_string(),
                TickVolAgg::new(&crate::tickvol::BarKind::Tick(100)).expect("tick agg"),
            ),
        );
        let mut of_aggs = HashMap::new();
        of_aggs.insert(
            "BTCUSDT@1m".to_string(),
            ("binance".to_string(), "BTCUSDT".to_string(), OrderflowAgg::new(0.5)),
        );
        let mut bf_pending = HashMap::new();
        bf_pending.insert("BTCUSDT".to_string(), vec![tick("BTCUSDT")]);

        let mut subs = HashMap::new();
        subs.insert("BTCUSDT@1m".to_string(), SubscriptionId(101));
        subs.insert("okx:ETHUSDT@1m".to_string(), SubscriptionId(201));

        let mut feed_retries = FeedRetries::default();
        feed_retries.note_missing(&crate::feed_lifecycle::RetryKey::series("deribit:X@1m"));
        let mut bf_retries = BackfillRetries::default();
        bf_retries.note_report(&BackfillReport::failed("BTCUSDT", 0, "boom"));

        let (bf_tx, bf_rx) = std::sync::mpsc::channel();
        bf_tx.send(("BTCUSDT".to_string(), vec![tick("BTCUSDT")])).unwrap();
        let (bf_done_tx, bf_done_rx) = std::sync::mpsc::channel();
        bf_done_tx.send(BackfillReport::finished("BTCUSDT", 3)).unwrap();

        let trades = TradeStore::default();
        trades.push("binance", &tick("BTCUSDT"));
        let books = BookStore::default();
        books.update("binance", "BTCUSDT", 0.1, vec![(100.0, 1.0)], vec![(100.1, 1.0)], 1);
        let direct_bars = DirectBarStore::default();
        direct_bars.seed(
            "binance",
            "BTCUSDT",
            "1m",
            vec![vike_model::Bar {
                ts: 60_000,
                open: 100.0,
                high: 100.0,
                low: 100.0,
                close: 100.0,
                volume: 1.0,
                funding: None,
                bid: None,
                ask: None,
                symbol: None,
            }],
        );

        Fixture {
            charts,
            aggs,
            of_aggs,
            bf_pending,
            last_seq: 42,
            status_line: "OBSERVING the CI box".to_string(),
            feeds,
            subs,
            spawned: ["BTCUSDT@1m".to_string(), "okx:ETHUSDT@1m".to_string()].into(),
            unroutable: ["deribit:X@1m".to_string()].into(),
            feed_retries,
            bf_spawned: ["BTCUSDT".to_string()].into(),
            bf_retries,
            earliest_live_ids: Mutex::new([("BTCUSDT".to_string(), 7_u64)].into()),
            _bf_tx: bf_tx,
            bf_rx,
            bf_done_rx,
            trades,
            books,
            direct_bars,
            // One Binance entry (depth only — no bar leg there) and one OKX entry with BOTH legs
            // live, so the clear must stop three DOM streams across two venues; one cockpit token
            // with both legs live. Ids are disjoint from `subs`' so the log lines are unambiguous.
            dom_depth: [
                (
                    ("binance".to_string(), "BTCUSDT".to_string()),
                    DomDepthSubs {
                        inst: "BTCUSDT".to_string(),
                        depth: SubscriptionId(150),
                        bars: None,
                    },
                ),
                (
                    ("okx".to_string(), "ETHUSDT".to_string()),
                    DomDepthSubs {
                        inst: "ETH-USDT-SWAP".to_string(),
                        depth: SubscriptionId(250),
                        bars: Some(SubscriptionId(251)),
                    },
                ),
            ]
            .into(),
            poly_subs: [(
                "1071".to_string(),
                PolyBookSubs { book: SubscriptionId(450), trades: Some(SubscriptionId(451)) },
            )]
            .into(),
            hidden: ["ETHUSDT@1m".to_string()].into(),
            feed_status: Arc::new(Mutex::new("OBSERVING the CI box".to_string())),
            log,
        }
    }

    impl Fixture {
        fn slots(&mut self) -> SwitchSlots<'_> {
            SwitchSlots {
                charts: &mut self.charts,
                aggs: &mut self.aggs,
                of_aggs: &mut self.of_aggs,
                bf_pending: &mut self.bf_pending,
                last_seq: &mut self.last_seq,
                status: &mut self.status_line,
                feeds: &mut self.feeds,
                subs: &mut self.subs,
                spawned: &mut self.spawned,
                unroutable: &mut self.unroutable,
                feed_retries: &mut self.feed_retries,
                bf_spawned: &mut self.bf_spawned,
                bf_retries: &mut self.bf_retries,
                earliest_live_ids: &self.earliest_live_ids,
                bf_rx: &self.bf_rx,
                bf_done_rx: &self.bf_done_rx,
                trades: &self.trades,
                books: &self.books,
                direct_bars: Some(&self.direct_bars),
                dom_depth: &mut self.dom_depth,
                poly_subs: &mut self.poly_subs,
                hidden: &mut self.hidden,
                feed_status: &self.feed_status,
            }
        }

        /// The backend session is gone (snapshot-rendered charts, seq gate, status, hidden) —
        /// [`clear_session_state`]'s whole contract on the populated fixture.
        fn assert_backend_session_cleared(&self) {
            assert!(
                !self.charts.contains_key("deribit:BTC-PERPETUAL@1m"),
                "the snapshot-rendered (backend-tail kline) chart is backend-session state and \
                 must clear"
            );
            assert_eq!(self.last_seq, 0, "last_seq");
            assert!(self.status_line.is_empty(), "status");
            assert!(self.hidden.is_empty(), "hidden");
        }

        /// The local feed plane is UNTOUCHED — every ledger, fold, cache and in-flight batch
        /// still present. ⚠ Consuming reads (`try_recv`, `TradeStore::drain`) — call this once,
        /// as a test's final assertion block.
        fn assert_feed_plane_intact(&self) {
            assert!(
                self.charts.contains_key("BTCUSDT@100t"),
                "the tape-rendered chart is venue truth and survives a switch"
            );
            assert!(
                self.charts.contains_key("BTCUSDT@1m"),
                "the DIRECT-BAR kline chart paints the venue's own bars and survives a switch"
            );
            assert!(
                self.direct_bars.series("binance", "BTCUSDT", "1m").is_some(),
                "the direct-bar store is venue truth and survives a switch"
            );
            assert_eq!(self.subs.len(), 2, "subs — the venue streams keep flowing");
            assert_eq!(self.spawned.len(), 2, "spawned — still the fold filter for B's bars");
            assert_eq!(self.unroutable.len(), 1, "unroutable");
            assert!(!self.feed_retries.is_empty(), "feed_retries");
            assert!(self.aggs.contains_key("BTCUSDT@100t"), "aggs");
            assert!(self.of_aggs.contains_key("BTCUSDT@1m"), "of_aggs");
            assert!(self.bf_pending.contains_key("BTCUSDT"), "bf_pending stays deliverable");
            assert_eq!(self.bf_spawned.len(), 1, "bf_spawned");
            assert!(!self.bf_retries.is_empty(), "bf_retries");
            assert!(!self.earliest_live_ids.lock().unwrap().is_empty(), "earliest_live_ids");
            assert_eq!(self.dom_depth.len(), 2, "dom_depth — the DOM keeps painting");
            assert_eq!(self.poly_subs.len(), 1, "poly_subs — the cockpit keeps painting");
            assert!(self.bf_rx.try_recv().is_ok(), "in-flight backfill batch survives");
            assert!(self.bf_done_rx.try_recv().is_ok(), "in-flight backfill report survives");
            assert_eq!(self.trades.drain("binance", "BTCUSDT").len(), 1, "trade tape survives");
            assert!(self.books.get("binance", "BTCUSDT").is_some(), "books survive");
        }

        /// Every named field is empty/reset — B1's original TOTAL-clear contract, now the
        /// composition of [`clear_session_state`] and [`teardown_feed_plane`].
        fn assert_cleared(&self) {
            assert!(self.charts.is_empty(), "charts");
            assert!(self.aggs.is_empty(), "aggs");
            assert!(self.of_aggs.is_empty(), "of_aggs");
            assert!(self.bf_pending.is_empty(), "bf_pending");
            assert_eq!(self.last_seq, 0, "last_seq");
            assert!(self.status_line.is_empty(), "status");
            assert!(self.subs.is_empty(), "subs");
            assert!(self.spawned.is_empty(), "spawned");
            assert!(self.unroutable.is_empty(), "unroutable");
            assert!(self.feed_retries.is_empty(), "feed_retries");
            assert!(self.bf_spawned.is_empty(), "bf_spawned");
            assert!(self.bf_retries.is_empty(), "bf_retries");
            assert!(self.earliest_live_ids.lock().unwrap().is_empty(), "earliest_live_ids");
            assert!(self.bf_rx.try_recv().is_err(), "bf_rx drained");
            assert!(self.bf_done_rx.try_recv().is_err(), "bf_done_rx drained");
            assert!(self.trades.drain("binance", "BTCUSDT").is_empty(), "trades cleared");
            assert!(self.books.get("binance", "BTCUSDT").is_none(), "books cleared");
            assert!(self.direct_bars.keys().is_empty(), "direct-bar store cleared");
            assert!(self.dom_depth.is_empty(), "dom_depth");
            assert!(self.poly_subs.is_empty(), "poly_subs");
            assert!(self.hidden.is_empty(), "hidden");
        }
    }

    /// The full unsubscribe fan-out `teardown_feed_plane` must produce on the populated fixture:
    /// every `subs` id, both DOM ledger entries (depth legs + the OKX 1m bar leg) and both
    /// cockpit legs, each against the venue client that issued it.
    const FULL_TEARDOWN_LOG: [&str; 7] = [
        "binance:unsub 101",    // subs: BTCUSDT@1m
        "binance:unsub 150",    // dom_depth: (binance, BTCUSDT) depth leg
        "okx:unsub 201",        // subs: okx:ETHUSDT@1m
        "okx:unsub 250",        // dom_depth: (okx, ETHUSDT) depth leg
        "okx:unsub 251",        // dom_depth: (okx, ETHUSDT) 1m bar leg
        "polymarket:unsub 450", // poly_subs: 1071 book leg
        "polymarket:unsub 451", // poly_subs: 1071 trade leg
    ];

    /// THE B2 SWITCH CONTRACT — feeds are NOT backend-session state. A switch clears the backend
    /// session (snapshot-rendered charts, the seq gate, status, hidden) and performs **ZERO feed
    /// unsubscribes and zero shutdowns**: the recording clients must observe nothing at all,
    /// and every local-plane slot — ledgers, folds, caches, in-flight backfill — stays intact.
    #[test]
    fn a_switch_clears_the_backend_session_and_never_touches_the_feed_plane() {
        let mut fx = populated();
        clear_session_state(&mut fx.slots());

        fx.assert_backend_session_cleared();
        assert!(
            fx.log.lock().unwrap().is_empty(),
            "a backend switch must not unsubscribe or shut down ANY local feed stream"
        );
        assert_eq!(fx.feeds.len(), 3, "feeds map untouched");
        fx.assert_feed_plane_intact();
    }

    /// THE MODE-EXIT TEARDOWN — [`teardown_feed_plane`] stops every local stream through the
    /// ledgers: each id unsubscribed against the venue client that issued it (the #1379
    /// discipline — DOM depth/bar and cockpit book/trade legs included, which pre-#1379 were
    /// dropped without any unsubscribe), the local plane cleared, and the BACKEND session left
    /// alone (it is `clear_session_state`'s, not this function's).
    #[test]
    fn the_feed_plane_teardown_unsubscribes_every_stream_through_the_ledgers() {
        let mut fx = populated();
        teardown_feed_plane(&mut fx.slots());

        let mut log = fx.log.lock().unwrap().clone();
        log.sort();
        assert_eq!(
            log,
            FULL_TEARDOWN_LOG.map(str::to_string).to_vec(),
            "each id must be unsubscribed against the venue that issued it — never handed onward"
        );
        // The feed CLIENTS survive even here: shutdown is `App::on_exit`'s (bounded, parallel).
        assert_eq!(fx.feeds.len(), 3, "feeds map is not torn down — clients are on_exit's");
        assert!(fx.subs.is_empty() && fx.dom_depth.is_empty() && fx.poly_subs.is_empty());
        assert!(fx.aggs.is_empty() && fx.of_aggs.is_empty() && fx.bf_pending.is_empty());
        assert!(fx.spawned.is_empty() && fx.unroutable.is_empty());
        assert!(fx.feed_retries.is_empty() && fx.bf_retries.is_empty());
        assert!(fx.trades.drain("binance", "BTCUSDT").is_empty(), "tape cleared");
        assert!(fx.books.get("binance", "BTCUSDT").is_none(), "books cleared");
        assert!(fx.direct_bars.keys().is_empty(), "direct-bar store is feed-plane state: cleared");
        // The backend session is deliberately NOT this function's: still folded, still painted.
        assert!(
            fx.charts.contains_key("deribit:BTC-PERPETUAL@1m"),
            "the backend-tail kline chart is the session clear's"
        );
        assert!(!fx.charts.contains_key("BTCUSDT@100t"), "tape chart is the feed plane's");
        assert!(
            !fx.charts.contains_key("BTCUSDT@1m"),
            "the direct-bar kline chart is the feed plane's (its bars are the venue's, not A's)"
        );
        assert_eq!(fx.last_seq, 42, "seq gate untouched");
        assert_eq!(fx.status_line, "OBSERVING the CI box", "status untouched");
    }

    /// The two clears COMPOSE to B1's original total clear, field for field — so no
    /// [`SwitchSlots`] slot can silently fall between the session half and the feed-plane half,
    /// and the full unsubscribe fan-out still happens exactly once.
    #[test]
    fn the_session_clear_and_the_feed_plane_teardown_compose_to_the_b1_total_clear() {
        let mut fx = populated();
        clear_session_state(&mut fx.slots());
        teardown_feed_plane(&mut fx.slots());

        fx.assert_cleared();
        let mut log = fx.log.lock().unwrap().clone();
        log.sort();
        assert_eq!(log, FULL_TEARDOWN_LOG.map(str::to_string).to_vec());
    }

    /// `switch_backend(target: None)` — the Disconnect path: old conn stopped and dropped, the
    /// empty snapshot published (so the render loop never keeps painting A's data), the backend
    /// session cleared — the feed plane still running (a disconnect stays IN observe mode, so the
    /// DOM/tape keep painting) — and the feed-status line says so.
    #[test]
    fn a_disconnect_stops_the_old_backend_and_blanks_the_published_snapshot() {
        let mut fx = populated();
        let m = vars(&[("PROD2_OBSERVE_KEY", "obs")]);
        let mut active =
            Some(connect_backend(&rec("the CI box", false), &m, false, status(), |_| {}, || {}));

        let published: Arc<Mutex<Option<Arc<vike_core::CoreSnapshot>>>> =
            Arc::new(Mutex::new(None));
        let repainted = Arc::new(AtomicBool::new(false));
        let (p, r) = (Arc::clone(&published), Arc::clone(&repainted));
        switch_backend(
            &mut active,
            None,
            &m,
            false,
            fx.slots(),
            move |snap| *p.lock().unwrap() = Some(snap),
            move || r.store(true, Ordering::Relaxed),
        );

        assert!(active.is_none(), "disconnect leaves no active backend");
        fx.assert_backend_session_cleared();
        let snap = published.lock().unwrap().clone().expect("an empty snapshot must be published");
        assert!(snap.bars.is_empty() && snap.orders.is_empty(), "published snapshot is EMPTY");
        assert!(repainted.load(Ordering::Relaxed), "a repaint is requested so the blank shows");
        assert_eq!(*fx.feed_status.lock().unwrap(), "disconnected");
        assert!(fx.log.lock().unwrap().is_empty(), "a disconnect touches no local feed stream");
        fx.assert_feed_plane_intact();
    }

    /// `switch_backend(target: Some)` — the Connect path: the old conn is replaced by one dialed
    /// from the NEW record (key resolution per that record), over a cleared backend session.
    #[test]
    fn a_switch_replaces_the_active_backend_with_the_target_record() {
        let mut fx = populated();
        let m = vars(&[("PROD2_OBSERVE_KEY", "obs")]);
        let mut active =
            Some(connect_backend(&rec("old", false), &m, false, status(), |_| {}, || {}));

        let target = rec("new", false);
        switch_backend(&mut active, Some(&target), &m, false, fx.slots(), |_| {}, || {});

        assert_eq!(
            active.as_ref().map(|c| c.record.name.as_str()),
            Some("new"),
            "the active backend is now the target record"
        );
        fx.assert_backend_session_cleared();
        assert!(fx.log.lock().unwrap().is_empty(), "a switch touches no local feed stream");
        drop(active); // stop + join the new bridge
    }

    // ── the pure UI decisions ───────────────────────────────────────────────────────────────

    /// Which entry is active, and how an unlisted (CLI-synthetic) connection is shown: it leads
    /// the list, marked active; registry rows keep file order; a listed active row is marked in
    /// place with no extra row.
    #[test]
    fn picker_rows_mark_the_active_record_and_surface_an_unlisted_connection() {
        let file = BackendsFile { backends: vec![rec("a", false), rec("b", true)], active: None };

        // Active is a registry record: marked in place, no extra row.
        let listed_active = rec("b", true);
        let rows = picker_rows(&file, Some(&listed_active));
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.listed));
        assert_eq!(
            rows.iter().map(|r| r.is_active).collect::<Vec<_>>(),
            vec![false, true],
            "the matching registry row is the active one"
        );

        // Active is the CLI synthetic record: surfaced as an extra, unlisted first row.
        let synthetic = cli_observe_record("cli.example:9040");
        let rows = picker_rows(&file, Some(&synthetic));
        assert_eq!(rows.len(), 3);
        assert!(rows[0].is_active && !rows[0].listed, "unlisted active row leads");
        assert_eq!(rows[0].record.addr, "cli.example:9040");
        assert!(rows[1..].iter().all(|r| r.listed && !r.is_active));

        // No active connection: plain registry list.
        let rows = picker_rows(&file, None);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| !r.is_active));
    }

    /// What a click does: the active row disconnects, any other row connects (implicit
    /// disconnect-then-connect inside `switch_backend`).
    #[test]
    fn clicking_the_active_backend_disconnects_and_any_other_connects() {
        let a = rec("a", false);
        let b = rec("b", false);
        assert_eq!(click_action(&a, Some(&a)), BackendAction::Disconnect);
        assert_eq!(click_action(&b, Some(&a)), BackendAction::Connect(b.clone()));
        assert_eq!(click_action(&a, None), BackendAction::Connect(a.clone()));
    }

    /// The B2 fence: no switching while a local core runs.
    #[test]
    fn switching_is_unavailable_while_a_local_core_runs() {
        assert!(switching_available(false));
        assert!(!switching_available(true));
    }

    /// Connecting to a LISTED record names it active, so a bare relaunch dials the node the
    /// operator last picked instead of the local default.
    #[test]
    fn connecting_to_a_listed_record_names_it_active() {
        let listed = vec![rec("the latency box", false), rec("the CI box", false)];
        let action = BackendAction::Connect(rec("the CI box", false));
        assert_eq!(active_after(&action, &listed), Some("the CI box".to_string()));
    }

    /// Disconnecting clears the pointer: staying down is a decision, and a launch that redialled
    /// the node the operator just left would be the app overruling them.
    #[test]
    fn disconnecting_clears_the_active_pointer() {
        let listed = vec![rec("the latency box", false)];
        assert_eq!(active_after(&BackendAction::Disconnect, &listed), None);
    }

    /// The `--observe` connection is UNLISTED, so it can never be named: `active_addr` resolves a
    /// pointer through `backends`, and one naming no record would answer nothing anyway.
    #[test]
    fn an_unlisted_connection_is_never_named_active() {
        let listed = vec![rec("the latency box", false)];
        let action = BackendAction::Connect(rec("cli-observe", false));
        assert_eq!(active_after(&action, &listed), None);
    }

    /// An empty registry names nothing — the first-run state, where the picker has no rows at all.
    #[test]
    fn an_empty_registry_names_nothing_active() {
        let action = BackendAction::Connect(rec("the CI box", false));
        assert_eq!(active_after(&action, &[]), None);
    }

    /// The flag OVERRIDES the registry and stays synthetic, so `--observe ADDR` keeps its
    /// pre-registry meaning even when an active record exists.
    #[test]
    fn the_observe_flag_overrides_the_registry_and_stays_synthetic() {
        let active = rec("the CI box", false);
        let r = startup_backend_from(Some("other.example:9040"), Some(active));
        assert_eq!(r.addr, "other.example:9040");
        assert_eq!(r.name, "", "the flag's record is unnamed — never persisted");
        assert_eq!(r.observe_key, backend_registry::OBSERVE_KEY_NAME);
    }

    /// A blank or whitespace-only flag is NOT a rung: a bare `--observe` means "the configured
    /// node", so it falls through to the registry rather than dialling an empty address.
    #[test]
    fn a_blank_flag_falls_through_to_the_registry() {
        let mut active = rec("the CI box", false);
        active.addr = "<host>:9097".to_string();
        assert_eq!(startup_backend_from(Some("   "), Some(active.clone())).addr, "<host>:9097");
        assert_eq!(startup_backend_from(None, Some(active)).addr, "<host>:9097");
    }

    /// The registry rung answers with the record ITSELF — key names and arming intact. Rebuilding
    /// a synthetic record here is what made a configured launch sign with the wrong key.
    #[test]
    fn the_registry_rung_answers_with_the_record_itself() {
        let mut active = rec("the CI box", true);
        active.observe_key = "PROD2_OBSERVE_KEY".to_string();
        let r = startup_backend_from(None, Some(active.clone()));
        assert_eq!(r, active, "the record rides through unchanged");
        assert_ne!(r.observe_key, backend_registry::OBSERVE_KEY_NAME, "NOT the synthetic key name");
    }

    /// Nothing configured at all still yields a record — the viewer opens on the local default
    /// instead of refusing to start.
    #[test]
    fn nothing_configured_still_yields_the_local_default() {
        let r = startup_backend_from(None, None);
        assert_eq!(r.addr, crate::backend_registry::DEFAULT_OBSERVE_ADDR);
        assert_eq!(r.observe_key, backend_registry::OBSERVE_KEY_NAME);
    }

    // ── the MODE question: `StartupObserve::requested` (the 2026-09-06 P1) ──────────────────────

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| (*w).to_string()).collect()
    }

    /// ⚠ THE REGRESSION GATE, half one. An ordinary desktop launch — no `--observe` anywhere in
    /// argv, no active registry record — REQUESTS NOTHING, so `vike-app` composes
    /// [`crate::split_plane::AppMode::LocalCore`]: a local trading core, live exec engines, and an
    /// order path that can actually place an order. It still resolves an ADDRESS (the ladder has no
    /// `None` rung, and the Connections UI needs one), and reading THAT as the mode is exactly the
    /// bug — hence both assertions in one test, so a future edit cannot satisfy one by breaking the
    /// other.
    #[test]
    fn a_normal_launch_requests_nothing_and_still_resolves_an_address() {
        let s = startup_observe_from(argv(&["vike-app"]), None);
        assert!(
            !s.requested,
            "no --observe and no active record: observing was never requested, so the app must \
             keep its LOCAL CORE"
        );
        assert_eq!(
            s.record.addr,
            crate::backend_registry::DEFAULT_OBSERVE_ADDR,
            "the address ladder is unchanged — it always answers"
        );
        assert_eq!(
            crate::split_plane::app_mode(true, crate::split_plane::observes(true, s.requested)),
            Some(crate::split_plane::AppMode::LocalCore),
            "a fat launch that requested nothing is the local-core arm"
        );
    }

    /// ⚠ THE REGRESSION GATE, half two — the reverse direction, so the fix cannot regress the other
    /// way: `--observe ADDR` still takes the observer arm, on the synthetic record, with the local
    /// core gone.
    #[test]
    fn the_observe_flag_requests_observing() {
        let s = startup_observe_from(argv(&["vike-app", "--observe", "the CI box.example:9040"]), None);
        assert!(s.requested, "--observe ADDR is a request to observe");
        assert_eq!(s.record.addr, "the CI box.example:9040");
        assert_eq!(s.record.observe_key, backend_registry::OBSERVE_KEY_NAME, "synthetic record");
        assert_eq!(
            crate::split_plane::app_mode(true, crate::split_plane::observes(true, s.requested)),
            Some(crate::split_plane::AppMode::ObserveWithFeeds),
            "fat + --observe is the third mode: no local core"
        );
    }

    /// A BARE `--observe` (the word last on the line, no address after it) is a request, and it is
    /// the case the scan had to leave the CI-excluded shell to be tested at all: `args.next()`
    /// yields `None`, so the word contributes NO address rung while still meaning "observe the
    /// configured node". Reading presence off the ARGUMENT would have made this launch local-core.
    #[test]
    fn a_bare_observe_flag_is_a_request_with_no_address_rung() {
        let s = startup_observe_from(argv(&["vike-app", "--observe"]), None);
        assert!(s.requested, "the word alone means the configured node");
        assert_eq!(s.record.addr, crate::backend_registry::DEFAULT_OBSERVE_ADDR);
        // …and a blank argument behaves the same way (the ladder already trims it).
        let blank = startup_observe_from(argv(&["vike-app", "--observe", "   "]), None);
        assert!(blank.requested);
        assert_eq!(blank.record.addr, crate::backend_registry::DEFAULT_OBSERVE_ADDR);
    }

    /// An ACTIVE registry record requests observing on its own — "configure once, launch bare
    /// afterwards" (`active_after`'s half of the same story) — and rides through as ITSELF, key
    /// names intact, which is #1611's fix and must survive this one.
    #[test]
    fn an_active_registry_record_requests_observing_and_rides_through_unchanged() {
        let mut active = rec("the CI box", true);
        active.addr = "<host>:9097".to_string();
        let s = startup_observe_from(argv(&["vike-app"]), Some(active.clone()));
        assert!(s.requested, "a configured active node is a request to observe");
        assert_eq!(s.record, active, "the record rides through unchanged — NOT re-synthesized");
        // The flag still OVERRIDES it, synthetic, exactly as the ladder's rung 1 says.
        let flagged =
            startup_observe_from(argv(&["vike-app", "--observe", "other:9040"]), Some(active));
        assert!(flagged.requested);
        assert_eq!(flagged.record.addr, "other:9040");
        assert_eq!(flagged.record.name, "", "the flag's record is unnamed — never persisted");
    }

    /// The scan reads the word ANYWHERE in argv (after the program name, before or after other
    /// flags) and takes the FIRST occurrence's argument — the shape `std::env::args()` hands it.
    #[test]
    fn the_scan_finds_the_flag_anywhere_in_argv() {
        let s =
            startup_observe_from(argv(&["vike-app", "--style", "dark", "--observe", "a:1"]), None);
        assert!(s.requested);
        assert_eq!(s.record.addr, "a:1");
        let none = startup_observe_from(argv(&["vike-app", "--observed", "a:1"]), None);
        assert!(!none.requested, "a LONGER flag that merely starts with the word is not a match");
    }
}
