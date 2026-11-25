//! `MarketDataSession` — the desktop's ONE market-data connection to a `vike-datahub`, and the whole of
//! the client half of the market-data wire designed in
//! `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` (§9 items 1 and 2).
//!
//! # What this restores
//!
//! The `fat` deletion took the desktop's local market-data plane and left everything downstream of
//! it — [`L2BookStore`], [`TickStore`],
//! [`DataClientSinkGuiFeed`], [`TickVolAgg`](vike_orderflow::tickvol::TickVolAgg),
//! [`OrderflowAgg`](vike_orderflow::bar_agg::OrderflowAgg) and the whole of
//! [`feed_lifecycle`](crate::ui::feed_lifecycle) — compiled, CI-tested and **fed by nothing**.
//! `DataClientSinkGuiFeed`, the struct built precisely to be the core-free market-data seam, had ZERO
//! production constructors: its only instantiation in the workspace was inside its own
//! `#[cfg(test)]` module. This module is that constructor, and the Trade window's ladder, the
//! trade tape and every chart on a symbol the daemon is not trading come back through it — sourced
//! from the
//! BACKEND rather than from a venue socket, because ruling 2 says the desktop opens none.
//!
//! # Which venues get a feed is the SERVER's word, learned by a PROBE
//!
//! No list in this crate says which venues the desktop mounts a feed for. The datahub states them at
//! the handshake (`market_data_venue=<slug>` in `Welcome.features`, read into [`MarketDataSessionCapabilities`]) and
//! [`MarketDataSession::sync_mount`] mounts one [`DataClientDatahub`](crate::data::market_data_session::data_client::DataClientDatahub) and
//! one status line per advertised slug — and nothing for a venue the connected server does not
//! advertise. [`build`] therefore hands the shell EMPTY maps.
//!
//! ⚠ **That only works because the reconciler asks.** `MarketDataSessionCapabilities` is learned from a dial, and the
//! reconciler dials once something is WANTED — but nothing is wanted until a feed exists to want it
//! through, so gating the feeds on `MarketDataSessionCapabilities` alone would deadlock. When the reconciler has an address and
//! no `MarketDataSessionCapabilities`, it therefore opens a handshake-only connection (the PROBE, `connection.rs`'s `probe`):
//! it reads `Welcome.features`, sends no `market_data_subscribe`, closes, and wakes the frame thread so
//! `sync_mount` can mount.
//!
//! * **`MarketDataSessionCapabilities == None` HOLDS what is mounted, it does not clear it.** `None` means "unknown" — startup,
//!   or an address change that has not been answered yet — and clearing on it would flap every idle
//!   feed across each backend switch, with every window's `ensure_*` logging a missing venue. The set
//!   converges the moment the new server's advertisement lands.
//! * **A feed with a live holder is never removed.** `App`'s ledgers (`subscriptions`, `trade_depth`,
//!   `polymarket_subscriptions`) route their `unsubscribe` through `App::feeds`, so a venue the new server dropped
//!   keeps its feed until its last key is released (its `want`s are refused locally in the meantime).
//! * **A venue is mounted only if the roster knows it** ([`vike_model::venues::roster_id`], the
//!   interner that keeps `DataClientMap` keyed on `&'static str`): a slug outside the roster has no
//!   capability row, so the static matrix would refuse every verb on it anyway.
//!
//! # THE THREADING RULE, stated once
//!
//! egui here is REACTIVE: producers wake it. **Every byte of I/O happens off the frame thread**, and
//! the wake closure (`context.request_repaint()`) is called exactly as every deleted venue feed called
//! it. A blocking call on the frame thread is a frozen GUI. Four thread classes, and the frame
//! thread is not one of them:
//!
//! | thread | count | owns | blocks on |
//! |---|---|---|---|
//! | frame (egui) | 1 | the [`DataClientDatahub`](crate::data::market_data_session::data_client::DataClientDatahub)s | nothing — a short `Mutex` and a `Condvar::notify_all` |
//! | `ui-data-sync` | 1 per session | the desired set, the dial schedule, the session id | `Condvar::wait_timeout`, and `CONTROL_DEADLINE` on each control exchange |
//! | `ui-data-control` | 1 per control exchange, short-lived | nothing at all | `connect*`, `market_data_subscribe` / `market_data_update` — UNBOUNDED, which is why it is its own thread |
//! | `ui-data-reader` | 1 per CONNECTION (respawned) | the stream `TcpStream` | `read_frame`, under the deadline `market_data_subscribe` armed |
//!
//! **The reconciler and the reader are separate because the reader parks in `read_frame` for up to
//! the negotiated deadline** (`max(MARKET_DATA_READ_TIMEOUT, 3 × heartbeat_ms)` — at least 45 s), while a
//! subscription change must land within one dial. So the reconciler is the ONLY thread that dials:
//! it performs `market_data_subscribe`, publishes the session id, and hands the returned stream to a FRESHLY
//! SPAWNED reader. No socket is ever shared between them.
//!
//! **Nothing polls.** The reconciler parks on a `Condvar` (with a generation counter, so a poke
//! landing between its check and its wait cannot be lost); the reader is woken out of a blocking
//! read by `TcpStream::shutdown(Both)` on the clone this session keeps. That last one is what makes
//! `MarketDataSession::begin_stop` worth having: without it a teardown pays the 45 s read deadline inside
//! `App::run_bounded_teardown`'s 1500 ms budget.
//!
//! ⚠ **A read TIMEOUT is a DEAD LINK, not something to retry.** `read_frame` surfaces the
//! negotiated deadline as `WouldBlock` (unix) / `TimedOut` (windows); looping on it would make the
//! heartbeat — the whole reason `MarketDataSubscribed.heartbeat_ms` rides the wire — buy nothing. Any read
//! error ends the reader. There is no in-crate precedent to copy:
//! [`observe_bridge`](crate::backend::observe_bridge) polls a cell rather than blocking on a socket.
//!
//! # ⚠ THE CONTROL EXCHANGE IS BOUNDED HERE, tighter than the client crate's default
//!
//! The datahub client bounds each reply by an idle timeout keyed on the verb
//! (`crates/vike-datahub-client/src/proto/verbs.rs`'s `reply_wait`); `market_data_subscribe` and
//! `market_data_update` are server-bounded verbs and get its 300 s default. This section predates that bound
//! and keeps its own, because 300 s of a wedged reconciler is still a desktop reading
//! "reconnecting" for five minutes. (Before that bound the reply read inside each had NO deadline,
//! and what follows was a wedge for the life of the process.) Both run on the `ui-data-sync`
//! thread, on a `DatahubClient` no other thread holds a handle to: `MarketDataSession::begin_stop` and
//! `begin_stream_teardown` can only shut the PUSH socket down, and a `poke()` cannot wake a thread
//! parked in `read`. So a peer that vanishes without a FIN or an RST — laptop suspend, a VPN or
//! tunnel dropping, a NAT eviction, exactly the case [`MARKET_DATA_READ_TIMEOUT`] exists to cover on the
//! stream socket — wedges the reconciler until the read bound fires, and nothing in this binary
//! can return it sooner: the reader independently times out, brackets, renders
//! `reconnecting (the link dropped)` and pokes, but `reconcile_loop` is the ONLY thing that dials.
//!
//! `bounded` is the answer, and it is a THREAD rather than a socket option because it predates the
//! client's per-client hatch. Each control exchange runs on a short-lived
//! helper and the reconciler `recv_timeout`s its result, so a wedged exchange costs ONE leaked
//! thread (and the socket it holds, closed when the exchange finally returns and its value is
//! dropped) while the dial ladder keeps climbing. The upstream hatch now exists
//! (`DatahubClient::set_reply_timeout`); moving these two exchanges onto it, which would retire the
//! helper thread, is a follow-up rather than part of the change that added it.
//!
//! # ⚠ THE SYMBOL RE-STAMP — the single most load-bearing line in the reader
//!
//! `crates/vike-datahub/src/market_data/hub.rs`'s `push_trade` sets `tick.symbol` to the empty `Symbol` before
//! the tape: the symbol is constant per key and the `Trades` frame's envelope carries it once (until 2026-11-20
//! it was a `String`, and §12.4 measured its heap breaking §5.5's memory budget). **Every `TickTrade` on this
//! wire therefore carries an EMPTY symbol**, and the envelope carries it once instead.
//!
//! [`DataClientSinkGuiFeed::trade`](crate::data::data_sink::DataClientSinkGuiFeed) deliberately ignores its `symbol` argument
//! and keys [`TickStore`] by `trade.symbol`. Handing wire ticks
//! straight through would push EVERY print on EVERY venue under the key `(venue, "")`, and
//! [`core_sync::sync_from_core`](crate::ui::core_sync::sync_from_core)'s drain — which drains under
//! each aggregator's own `(venue, symbol)` — would find nothing. Result: tick/volume charts and
//! every orderflow overlay silently empty, for ever, with no error, no log and no failing test.
//! Nothing in the design mentions it, and "everything else on the receiving end is untouched" is
//! exactly what makes it invisible. So `MarketDataSession::apply_frame` re-stamps each tick from the
//! envelope, and `crates/vike-app-core/tests/market_data_session_scripted/main.rs` carries the kill proof.
//!
//! # Staleness is a receipt stamp taken HERE (§7.4)
//!
//! Every book frame lands through `sink.l2_snapshot(.., vike_model::now_ms())` — the LOCAL clock.
//! `L2BookStore::update` stores that as the receipt and the shell computes
//! `now_ms - ts > TRADE_STALE_MS`; two boxes' clocks differ, so a wire timestamp would make every ladder read permanently stale or
//! falsely live across an SSH tunnel. `BookSnapshot::venue_timestamp`/`venue_sequence` ride for diagnostics.
//!
//! # Refusals split by SYNCHRONY, and the split is load-bearing for `DatahubFeedRetries`
//!
//! A refusal that returns SYNCHRONOUSLY from `subscribe_*` reaches
//! `DatahubFeedRetries::note_error`, which records
//! `DataClientError::Unsupported` as `DatahubFeedRetryState::Refused` and never retries it. That covers the
//! static capability matrix, the cached `market_data_venue=` advertisement, and a permanent refusal this
//! session has already SEEN. ⚠ **An asynchronous permanent refusal — one that arrives on the wire
//! after `subscribe_depth` already answered `Ok` — cannot reach `DatahubFeedRetries` at all**: `ensure_depth`
//! has already recorded its `trade_depth` entry and returns early for ever. Its disclosure is the
//! per-venue status string plus one `tracing::warn!`, and nothing else. That is accepted, and it is
//! written here because a reader will otherwise assume `DatahubFeedRetries` classifies every refusal.
//! A CAP refusal is not permanent: the spec stays in the desired set and the reconciler retries it.

use std::collections::{HashMap, HashSet};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use vike_data::{DataClient, DataClientError, DataClientSink, DataClientSubscriptionId};
use vike_datahub_client::{
    DatahubClient, MarketDataBye, MarketDataSubscribedInfo, MarketDataUpdatedInfo,
    market::{
        MARKET_DATA_READ_TIMEOUT, MarketDataFrame, MarketDataRefusal, MarketDataSessionId,
        MarketDataSpec,
    },
    proto::{FEATURE_MARKET_DATA_LANE_QUOTES, Response},
};
use vike_hub_protocol::auth::{HubKeys, Scope};
use vike_hub_protocol::features::advertised_market_data_venues;
use vike_hub_protocol::frame::read_frame;
use vike_model::feed_status::{DataClientConnectionState, DataClientStatus};
use vike_utility::mutex::lock;

use crate::data::data_sink::{DataClientSinkGuiFeed, L2BookStore, StoredBarStore, TickStore};
use crate::data::datahub::feed_retry::{RETRY_BASE, RETRY_MAX, RetryRun};
use connection::reconcile_loop;

mod connection;
pub mod data_client;
mod session;

/// The GUI's venue-keyed live [`DataClient`] map (`"binance"`, `"bybit"`, `"okx"`,
/// `"polymarket"`, …). `+ Send` because the shell (`vike-desktop`) moves the whole set onto a
/// throwaway shutdown thread and fans each venue's teardown out in parallel (see its `on_exit`).
///
/// ⚠ **The map CHANGES at run time.** Its keys are the venues the connected datahub advertises, and
/// `MarketDataSession::sync_mount` adds and drops entries as that advertisement is learned or changes (it
/// starts EMPTY), so "no feed for this venue" is a state a series can be in at one frame and not the
/// next: [`retry_unroutable`](crate::ui::feed_lifecycle::retry_unroutable) is the half that notices.
pub type DataClientMap = HashMap<&'static str, Box<dyn DataClient + Send>>;

/// What [`build`] hands the shell: the `App::feeds` map and the `App::feed_statuses` map, both
/// EMPTY — [`MarketDataSession::sync_mount`] fills them once the datahub has advertised its venues. Returned
/// as a struct rather than a tuple so the shell's binding is one line and names what it binds —
/// `crates/vike-desktop` is RATCHETED (`crates/vike-ops/tests/
/// ci_excluded_gui_shell_ratchet.rs`) and every line of shape spent there is a line not spent on
/// something that had to be there.
pub struct MarketDataSessionMount {
    /// The session itself — held by the shell so it can push the late-bound address
    /// ([`MarketDataSession::set_address`]) and follow this desktop's device key ([`MarketDataSession::follow_device_key`]),
    /// mount the advertised venues ([`MarketDataSession::sync_mount`]), and read a key's tape-gap epoch
    /// ([`MarketDataSession::tape_gap_epoch`]) inside the fold. Teardown needs this handle too: with no feed
    /// mounted (nothing advertised yet) there is no `DataClient::shutdown` to stop the session.
    pub session: Arc<MarketDataSession>,
    /// One [`DataClientDatahub`](crate::data::market_data_session::data_client::DataClientDatahub) per venue the connected datahub
    /// advertises, over the shared session; EMPTY until the first advertisement lands. The keys are
    /// roster ids, which are `&'static str` — the type `DataClientMap` demands and the same routing keys
    /// [`feed_lifecycle`](crate::ui::feed_lifecycle) resolves.
    pub feeds: DataClientMap,
    /// One status (state and words) per mounted venue, the handles the Connections tool reads per
    /// frame; EMPTY until [`MarketDataSession::sync_mount`] mounts a feed. The session rewrites the
    /// statuses, and `sync_mount` adds and removes keys beside the feed it mounts or drops.
    pub feed_statuses: HashMap<String, Arc<Mutex<DataClientStatus>>>,
}

/// The server's advertisement, cached off the latest successful handshake — a PROBE (nothing is
/// wanted yet) or a dial — and the one source [`MarketDataSession::sync_mount`] mounts the feeds from.
///
/// ⚠ **Never cleared on a link drop, and that diverges from
/// [`BackendObserveBridgeHandle::advertised_datahub`](crate::backend::observe_bridge::BackendObserveBridgeHandle)'s clear-on-drop
/// discipline on purpose.** That cell carries an ADDRESS — a routing fact, and a different daemon
/// at the same address may front a different datahub, so it must not outlive its connection. This
/// one carries the capability set of a server already identified BY address. Clearing it on a blip
/// would flip every unserved venue from "refused, known" back to "unknown, accepted" on every
/// reconnect: churn carrying no information.
///
/// ⚠ **It IS cleared when the DIALLED address changes — and "dialled" is the load-bearing word,
/// not "connected".** `dial` caches the advertisement BEFORE it refuses, deliberately (see that
/// function's doc), so a server advertising no `market_data` leaves `MarketDataSessionCapabilities { market_data: false }`
/// behind while no connection was ever established. Keyed on a CONNECTION, the clear could not fire
/// for exactly that value: `MarketDataSession::want`'s rung 2 would answer `Unsupported(NO_MARKET_DATA_PLANE)` for
/// the life of the process, on every later address too, and `DatahubFeedRetries` records that answer as
/// `DatahubFeedRetryState::Refused` and never re-asks. So `reconcile_loop` tracks the address every cached fact
/// DESCRIBES — set by an ATTEMPT, not by a success — and clears on a change to it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MarketDataSessionCapabilities {
    /// Whether the server advertised `market_data` at all.
    pub market_data: bool,
    /// The `market_data_venue=<slug>` set, in advertisement order.
    pub venues: Vec<String>,
    /// Whether the server advertised the quote lane. It cannot decode a quote spec otherwise, and one
    /// would cost every other spec of the request.
    pub quote_lane: bool,
}

impl MarketDataSessionCapabilities {
    fn from_features(features: &[String]) -> MarketDataSessionCapabilities {
        MarketDataSessionCapabilities {
            market_data: features.iter().any(|f| f == vike_datahub_client::FEATURE_MARKET_DATA),
            venues: advertised_market_data_venues(features),
            quote_lane: features.iter().any(|f| f == FEATURE_MARKET_DATA_LANE_QUOTES),
        }
    }

    fn serves(&self, venue: &str) -> bool {
        self.venues.iter().any(|v| v == venue)
    }

    /// Why this advertisement leaves NOTHING to mount, or `None` when it names at least one roster
    /// venue (the ones [`MarketDataSession::sync_mount`] mounts). The no-plane sentence is the one a refused
    /// dial already carries, so the two cannot name different fixes.
    fn nothing_to_mount(&self) -> Option<&'static str> {
        if !self.market_data {
            Some(NO_MARKET_DATA_PLANE)
        } else if self.venues.iter().all(|v| vike_model::venues::roster_id(v).is_none()) {
            Some(NO_MARKET_DATA_VENUES)
        } else {
            None
        }
    }
}

/// One entry of the DESIRED set: the spec plus how many live [`DataClientSubscriptionId`]s hold it.
///
/// Refcounted rather than a bare set because two callers can legitimately want one key — a Trade
/// window's depth leg and a chart's trade leg are different lanes, but two Trade windows on one
/// symbol are the same key — and an `unsubscribe` from one of them must not silently stop the
/// other's stream.
struct MarketDataSessionWant {
    spec: MarketDataSpec,
    holders: u32,
}

/// What the log has heard and said of one `(venue, symbol, lane)` key ([`MarketDataSession::report_status`]).
#[derive(Default)]
struct MarketDataSessionStreamLog {
    /// It has carried data on this connection.
    flowed: bool,
    /// It has had a status on this connection: the hub's first is its attach answer.
    heard: bool,
    /// The start of the gap the log WARNed of and has not yet said `live again` for.
    gap: Option<i64>,
}

/// The desktop's ONE market-data session. Shared as an `Arc` by every
/// [`DataClientDatahub`](crate::data::market_data_session::data_client::DataClientDatahub) and by both background threads; every field
/// sits behind its own lock, so this type is `Send + Sync` and `DataClientDatahub` is `Send` (which
/// [`DataClientMap`]'s `Box<dyn DataClient + Send>` requires).
///
/// ⚠ **Lock order, stated once so it cannot be got wrong: `desired` → `served` → `refused` →
/// `unheld` → `gapped` → `capabilities` → `gap_epochs` → `mounted`.** No lock is ever held across a socket call, a sink
/// call or a `wake()`. (`link` is outside the order because it is never held ALONGSIDE another: every
/// touch is a statement-scoped read or write. `mounted` is last because `refresh_statuses` takes it
/// while it still holds the first four; the one place that MUTATES it,
/// [`MarketDataSession::sync_mount`], holds nothing else at the time.)
pub struct MarketDataSession {
    /// The datahub address, LATE-BOUND. ⚠ It is not known at `App::new`:
    /// [`datahub::resolve_datahub_address`](crate::data::datahub::resolve_datahub_address)'s
    /// second rung is the active backend's `Welcome` advertisement, which arrives only after the
    /// observe bridge handshakes — asynchronously, after `App::new` has returned. A session built
    /// with the address as a constructor argument would mount NOTHING, permanently, on any box
    /// configured purely by the daemon's advertisement. The shell pushes it once per frame through
    /// [`MarketDataSession::set_address`], which is one line where a redesign of `App::new` would have been
    /// many.
    address: Mutex<Option<String>>,
    desired: Mutex<Vec<MarketDataSessionWant>>,
    /// The server's AUTHORITATIVE accepted set — depths already clamped. ⚠ A failed `market_data_update`
    /// leaves this UNTOUCHED: a failure means UNKNOWN, never EMPTY, which is
    /// `crates/vike-recorder/src/session.rs`'s `SubscriptionSet::reconcile` rule and the reason
    /// this is not rewritten from a guess.
    served: Mutex<Vec<MarketDataSpec>>,
    /// PERMANENT refusals only (`MarketDataRefusal::is_permanent`), so a later `subscribe_*` for the same
    /// key can be refused SYNCHRONOUSLY and `DatahubFeedRetries` can classify it.
    refused: Mutex<Vec<(MarketDataSpec, MarketDataRefusal)>>,
    /// The keys the last exchange refused [`MarketDataRefusal::NotHeld`]: still wanted and asked for again on
    /// the ladder, and what a venue's line says while it serves nothing else. Every exchange restates
    /// them all, because its `add` is every wanted key not yet served.
    unheld: Mutex<Vec<MarketDataSpec>>,
    /// BOOK-lane `(venue, symbol)` keys the server has disclosed as gapped or stale and that have
    /// not produced a snapshot since — i.e. the keys whose ladder this client has just EMPTIED.
    ///
    /// ⚠ It exists so [`MarketDataSession::refresh_statuses`] cannot render `N/N stream(s) live` over a
    /// blank ladder: `served` is the server's ACCEPTED set, which a `Status::GapStart` does not
    /// change, so "subscribed" and "flowing" are different questions and only the first one has an
    /// answer in `served`. An operator staring at an empty ladder being told by the Connections tool
    /// that the venue is fine is the same operator-facing failure the status wording below exists
    /// to kill, one layer out.
    ///
    /// ⚠ **A DATA frame clears the mark, a `Status::Live` is not waited for**, and that is not
    /// laziness. `vike_bridge_core::stream_health`'s `recover` emits `Live` only to CLOSE an open
    /// gap (`recover_without_a_gap_is_none`), so a depth feed that has been healthy since it started
    /// emits no status at all — and `MarketDataHub::attach_frames` hands every new subscriber
    /// `GapStart{now_ms}` as the honest "wanted, not yet live". Keyed on `Live`, every such key
    /// would read gapped for ever while its ladder painted perfectly. A snapshot arriving IS the
    /// flow signal.
    ///
    /// TRADES keys are deliberately absent: this client has no flow signal for a tape (five of the
    /// six venues' trade lanes emit no `stream_status` at all), so counting one as not-flowing would
    /// be a guess in the other direction.
    gapped: Mutex<HashSet<(String, String)>>,
    /// What the log has heard and said of each wanted `(venue, symbol, lane)` key: what makes a gap's
    /// WARN have exactly one `live again` INFO, and a new key's or a closed key's status none
    /// ([`MarketDataSession::report_status`]). A key is in it from `want` to its release (`unwant`),
    /// so a status or frame still in flight for a released key says nothing; a new connection forgets
    /// what the last one heard and carried but keeps an open gap, which its first data closes. Never
    /// held alongside another lock.
    streams: Mutex<HashMap<(String, String, vike_model::StreamLabel), MarketDataSessionStreamLog>>,
    capabilities: Mutex<Option<MarketDataSessionCapabilities>>,
    session: Mutex<Option<MarketDataSessionId>>,
    /// A `try_clone` of the live stream, held ONLY so [`MarketDataSession::begin_stop`] can shut it down
    /// and return the reader from its blocking read at once.
    stream_handle: Mutex<Option<TcpStream>>,
    reader: Mutex<Option<JoinHandle<()>>>,
    reconciler: Mutex<Option<JoinHandle<()>>>,
    /// Monotonic per-`(venue, symbol)` tape-gap counter — the seam between the reader thread and the
    /// aggregators, which live on the FRAME thread and cannot be touched from here. See
    /// [`MarketDataSession::tape_gap_epoch`].
    gap_epochs: Mutex<HashMap<(String, String), u64>>,
    /// The status line of every MOUNTED venue — the same `Arc`s the shell holds in
    /// `App::feed_statuses`, added and removed by [`MarketDataSession::sync_mount`] beside the feed. The
    /// session owns this copy so the reconciler and the reader can rewrite the lines without
    /// reaching into the shell's map.
    mounted: Mutex<HashMap<&'static str, Arc<Mutex<DataClientStatus>>>>,
    /// The LINK half of every status line — `datahub <address>` / `reconnecting (…)` / `idle` — with
    /// the state the reconciler set beside it, kept so a re-render triggered by a health change
    /// ([`MarketDataSession::restate_statuses`]) does not have to invent one, and cannot therefore
    /// contradict the line the reconciler last wrote. It is also the ONLY place the link's state is
    /// written down when no venue is mounted at all (a server that advertises nothing has no line
    /// to carry it): [`MarketDataSession::link_status`] reads it.
    link: Mutex<DataClientStatus>,
    /// Session-wide, so an id minted for one venue can never collide with another's.
    next_id: AtomicU64,
    stop: AtomicBool,
    wake: Box<dyn Fn() + Send + Sync>,
    sink: DataClientSinkGuiFeed,
    /// This desktop's device key the session signs with, RESOLVED. Behind a lock and re-resolvable
    /// because the Server group may make the key mid-session — see
    /// [`MarketDataSession::follow_device_key`].
    keys: Mutex<Option<HubKeys>>,
    /// [`crate::backend::registry::device_key_epoch`] as it stood BEFORE [`build`] resolved
    /// `keys`: the reconciler re-resolves OFF the frame thread when it moves (resolving opens a
    /// file), so a key made after the build is used.
    keys_epoch: u64,
    /// The device key epoch [`MarketDataSession::follow_device_key`] last saw, so a move wakes the reconciler.
    pushed_epoch: AtomicU64,
    /// The composition root's ONE boot walk, retained so the reconciler can re-run
    /// [`crate::backend::registry::datahub_dial_keys`] when the device key moves.
    /// This crate still reads no environment of its own: it arrives as a parameter to [`build`].
    settings_dir: Option<String>,
    /// The `Condvar`'s companion: a GENERATION counter, not a boolean. A poke bumps it; the
    /// reconciler samples it before it reads any state and parks only while it is unchanged — so a
    /// `want` landing between the reconciler's check and its `wait` cannot be lost into a 60 s park.
    poke_gen: Mutex<u64>,
    cv: Condvar,
}

// ── The `&'static str` refusal vocabulary ───────────────────────────────────────────────────────
// ⚠ `DataClientError::Unsupported` carries a `&'static str` and a wire refusal arrives as an owned
// `String`; the only conversion is `Box::leak`, which is unbounded. `MarketDataRefusal::LaneUnsupported`'s
// own doc names that trap. So a refusal maps onto ONE of these locals and the server's own text
// goes into the per-venue STATUS STRING instead, where it is read and not leaked.

/// A venue this datahub build serves no market-data client for.
const NOT_ADVERTISED: &str = "this datahub advertises no market-data client for that venue (see the Connections tool for \
     the served set)";
/// A server with no market-data plane at all: an unarmed `flags.datahub_live` row.
const NO_MARKET_DATA_PLANE: &str = "this datahub serves no market-data plane — run `vike-cli config set flags.datahub_live \
     true` on the server and restart it";
/// A server whose market-data plane is up but advertises no venue this desktop's roster knows:
/// there is nothing to mount a feed for. Carried by the link line ([`MarketDataSession::link_status`]) —
/// no venue is mounted, so no per-venue line exists to say it.
const NO_MARKET_DATA_VENUES: &str = "this datahub serves a market-data plane but advertises no venue this desktop knows — it runs a newer venue roster than this desktop";
/// A key this session has already been permanently refused for.
const SERVER_REFUSED: &str =
    "this datahub permanently refused that key (the per-venue status line carries its own words)";
/// The declared, deliberate bar-lane refusal — see [`crate::data::market_data_session::data_client`].
pub(crate) const NO_BAR_LANE: &str = "the datahub market-data wire serves no live bar lane: a chart's HISTORY comes from \
     Request::LoadBars/Backfill and its live tail from the backend snapshot";
/// A datahub older than the quote lane.
pub(crate) const NO_QUOTE_LANE: &str =
    "this datahub serves no quotes lane: it predates the `market_data_lane_quotes` word";

/// How long [`bounded`] waits for a control exchange — `connect` + `market_data_subscribe`, or `connect` +
/// `market_data_update` — before abandoning it and letting the dial ladder climb.
///
/// ⚠ **ADOPTED from [`MARKET_DATA_READ_TIMEOUT`], not invented, and this whole design still introduces no
/// new timing constant.** That number is this wire's own "a peer that has said nothing for three
/// heartbeats is not slow, it is gone", and these two verbs are a hub lookup and an answer — they
/// COMPUTE nothing, which is why the client's `reply_wait` classes them server-bounded while it
/// leaves the verbs that do unbounded. Anything shorter would abort a live exchange on a loaded box; anything longer
/// buys nothing, because the ladder re-dials afterwards either way.
const CONTROL_DEADLINE: Duration = MARKET_DATA_READ_TIMEOUT;

/// A short label for one wire refusal, for the per-venue STATUS LINE.
///
/// `{why:?}` is not ours to control: `MarketDataRefusal::LaneUnsupported` carries
/// `vike_data::require_live_verb`'s own text verbatim (that is the point of the variant — the
/// wire's refusal set cannot drift from the declared matrix) and `VenueNotServed` a
/// server-supplied venue list. So the variant picks the words, and the server's own go to the
/// `tracing::warn!` [`MarketDataSession::absorb_refusals`] already emits. (While these lines were
/// classified by keyword, the interpolated `VenueCapabilities.live_data` read a REFUSED venue as
/// Connected; the state is now set beside the words.)
///
/// The match is exhaustive with no `_` arm, so a new `MarketDataRefusal` cannot inherit a label — and the
/// three CAP variants are here even though [`MarketDataSession::refresh_statuses`] only ever sees permanent
/// ones (`absorb_refusals` records nothing else), because a `_` arm is exactly how that would stop
/// being true silently.
fn refusal_label(why: &MarketDataRefusal) -> &'static str {
    match why {
        MarketDataRefusal::UnknownVenue => "not a venue this build knows",
        MarketDataRefusal::VenueNotServed(_) => "this datahub links no client for that venue",
        MarketDataRefusal::LaneUnsupported(_) => "that venue declares no such lane",
        MarketDataRefusal::SymbolRejected(_) => {
            "that symbol is blank, too long, or holds a control byte"
        }
        MarketDataRefusal::KeyCapTotal { .. } => "the server's key budget is full",
        MarketDataRefusal::KeyCapVenue { .. } => "that venue's key budget is full",
        MarketDataRefusal::SpecCapSession { .. } => "this session holds its maximum number of keys",
        MarketDataRefusal::NotHeld => "no strategy, recorder or resident row holds that key",
    }
}

/// Build the session, its reconciler thread, and the (empty) feed/status maps the shell installs.
/// The maps fill through [`MarketDataSession::sync_mount`] once the datahub's advertisement is known.
///
/// `settings_dir` is the binary's ONE boot walk (`vike_boot::Booted`), taken as a PARAMETER — this
/// crate reads no environment, so nothing here joins `LIBRARY_PIN`. The session signs with this
/// desktop's device key alone (`registry::datahub_dial_keys`, decision 0288).
///
/// ⚠ **The [`DataClientSinkGuiFeed`] is constructed HERE, not in the shell**, and its `bars` store is PRIVATE
/// to it: this wire serves no bar lane (design §10), so nothing will ever write that store.
/// `App::direct_bars` is a DIFFERENT store (the shell builds it and
/// `crate::data::stored`' reads fill it), and
/// [`core_sync::sync_from_core`](crate::ui::core_sync::sync_from_core) takes its render-source
/// mode signal from `bar_plane`, never from whether a store handle exists.
pub fn build(
    settings_dir: Option<&str>,
    books: Arc<L2BookStore>,
    trades: Arc<TickStore>,
    wake: impl Fn() + Send + Sync + 'static,
) -> MarketDataSessionMount {
    let keys_epoch = crate::backend::registry::device_key_epoch();
    let (keys, notices) = crate::backend::registry::datahub_dial_keys(settings_dir);
    for n in notices {
        tracing::warn!("datahub market-data: {n}");
    }

    let session = Arc::new(MarketDataSession {
        address: Mutex::new(None),
        desired: Mutex::new(Vec::new()),
        served: Mutex::new(Vec::new()),
        refused: Mutex::new(Vec::new()),
        unheld: Mutex::new(Vec::new()),
        gapped: Mutex::new(HashSet::new()),
        streams: Mutex::new(HashMap::new()),
        capabilities: Mutex::new(None),
        session: Mutex::new(None),
        stream_handle: Mutex::new(None),
        reader: Mutex::new(None),
        reconciler: Mutex::new(None),
        gap_epochs: Mutex::new(HashMap::new()),
        mounted: Mutex::new(HashMap::new()),
        link: Mutex::new(DataClientStatus::new(
            DataClientConnectionState::Disconnected,
            "disconnected",
        )),
        next_id: AtomicU64::new(1),
        stop: AtomicBool::new(false),
        wake: Box::new(wake),
        // ⚠ The private, never-written bar store — see this function's doc.
        sink: DataClientSinkGuiFeed { books, trades, bars: Arc::new(StoredBarStore::default()) },
        keys: Mutex::new(keys),
        keys_epoch,
        pushed_epoch: AtomicU64::new(keys_epoch),
        settings_dir: settings_dir.map(str::to_string),
        poke_gen: Mutex::new(0),
        cv: Condvar::new(),
    });

    let driver = session.clone();
    if let Ok(h) = std::thread::Builder::new()
        .name("ui-data-sync".to_string())
        .spawn(move || reconcile_loop(driver))
    {
        *lock(&session.reconciler) = Some(h);
    } else {
        session.set_all_statuses(DataClientStatus::new(
            DataClientConnectionState::Error,
            "market-data error: the OS refused the reconciler thread — no market data",
        ));
        tracing::error!(
            "market_data_session: the OS refused the reconciler thread — no market data"
        );
    }

    MarketDataSessionMount { session, feeds: DataClientMap::new(), feed_statuses: HashMap::new() }
}

#[cfg(test)]
#[path = "gap_log_tests.rs"]
mod gap_log_tests;
