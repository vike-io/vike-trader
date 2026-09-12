//! `MdSession` — the desktop's ONE market-data connection to a `vike-datahub`, and the whole of
//! the client half of the market-data wire designed in
//! `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` (§9 items 1 and 2).
//!
//! # What this restores
//!
//! The `fat` deletion took the desktop's local market-data plane and left everything downstream of
//! it — [`BookStore`](crate::data_sink::BookStore), [`TradeStore`](crate::data_sink::TradeStore),
//! [`GuiFeedSink`](crate::data_sink::GuiFeedSink), [`TickVolAgg`](crate::tickvol::TickVolAgg),
//! [`OrderflowAgg`](crate::orderflow::OrderflowAgg) and the whole of
//! [`feed_lifecycle`](crate::feed_lifecycle) — compiled, CI-tested and **fed by nothing**.
//! `GuiFeedSink`, the struct built precisely to be the core-free market-data seam, had ZERO
//! production constructors: its only instantiation in the workspace was inside its own
//! `#[cfg(test)]` module. This module is that constructor, and the DOM ladder, the trade tape and
//! every chart on a symbol the daemon is not trading come back through it — sourced from the
//! BACKEND rather than from a venue socket, because ruling 2 says the desktop opens none.
//!
//! # THE THREADING RULE, stated once
//!
//! egui here is REACTIVE: producers wake it. **Every byte of I/O happens off the frame thread**, and
//! the wake closure (`ctx.request_repaint()`) is called exactly as every deleted venue feed called
//! it. A blocking call on the frame thread is a frozen GUI. Four thread classes, and the frame
//! thread is not one of them:
//!
//! | thread | count | owns | blocks on |
//! |---|---|---|---|
//! | frame (egui) | 1 | the [`DatahubFeed`](crate::datahub_feed::DatahubFeed)s | nothing — a short `Mutex` and a `Condvar::notify_all` |
//! | `md-reconcile` | 1 per session | the desired set, the dial schedule, the session id | `Condvar::wait_timeout`, and `CTRL_DEADLINE` on each control exchange |
//! | `md-ctrl` | 1 per control exchange, short-lived | nothing at all | `connect*`, `md_subscribe` / `md_update` — UNBOUNDED, which is why it is its own thread |
//! | `md-reader` | 1 per CONNECTION (respawned) | the stream `TcpStream` | `read_frame`, under the deadline `md_subscribe` armed |
//!
//! **The reconciler and the reader are separate because the reader parks in `read_frame` for up to
//! the negotiated deadline** (`max(MD_READ_TIMEOUT, 3 × heartbeat_ms)` — at least 45 s), while a
//! subscription change must land within one dial. So the reconciler is the ONLY thread that dials:
//! it performs `md_subscribe`, publishes the session id, and hands the returned stream to a FRESHLY
//! SPAWNED reader. No socket is ever shared between them.
//!
//! **Nothing polls.** The reconciler parks on a `Condvar` (with a generation counter, so a poke
//! landing between its check and its wait cannot be lost); the reader is woken out of a blocking
//! read by `TcpStream::shutdown(Both)` on the clone this session keeps. That last one is what makes
//! [`MdSession::begin_stop`] worth having: without it a teardown pays the 45 s read deadline inside
//! `App::run_bounded_teardown`'s 1500 ms budget.
//!
//! ⚠ **A read TIMEOUT is a DEAD LINK, not something to retry.** `read_frame` surfaces the
//! negotiated deadline as `WouldBlock` (unix) / `TimedOut` (windows); looping on it would make the
//! heartbeat — the whole reason `MdSubscribed.heartbeat_ms` rides the wire — buy nothing. Any read
//! error ends the reader. There is no in-crate precedent to copy:
//! [`observe_bridge`](crate::observe_bridge) polls a cell rather than blocking on a socket.
//!
//! # ⚠ THE CONTROL EXCHANGE IS BOUNDED HERE, because the client crate deliberately does not bound it
//!
//! `crates/vike-datahub-client/src/client.rs`'s `arm_request_timeouts` CLEARS the read timeout at
//! the end of both `connect` and `connect_authed`, and its own doc argues why: past the handshake a
//! positional read waits on SERVER-SIDE COMPUTATION (a sweep, a walk-forward, a backfill) whose
//! legitimate duration that crate cannot name. **`md_subscribe` and `md_update` are not in that
//! family** — they touch a hub and answer — but they ride the same socket options, so the reply read
//! inside each has NO deadline. Both run on the `md-reconcile` thread, on a `DatahubClient` no other
//! thread holds a handle to: [`MdSession::begin_stop`] and `begin_stream_teardown` can only shut the
//! PUSH socket down, and a `poke()` cannot wake a thread parked in `read`. So a peer that vanishes
//! without a FIN or an RST — laptop suspend, a VPN or tunnel dropping, a NAT eviction, exactly the
//! case [`MD_READ_TIMEOUT`] exists to cover on the stream socket — would wedge the reconciler for
//! the life of the process, and nothing in this binary could return it: the reader independently
//! times out, brackets, renders `reconnecting (the link dropped)` and pokes, but `reconcile_loop` is
//! the ONLY thing that dials. The desktop would read "reconnecting" for ever and market data would
//! never come back.
//!
//! `bounded` is the answer, and it is deliberately a THREAD rather than a socket option: the
//! option lives on a stream this crate cannot reach. Each control exchange runs on a short-lived
//! helper and the reconciler `recv_timeout`s its result, so a wedged exchange costs ONE leaked
//! thread (and the socket it holds, closed when the exchange finally returns and its value is
//! dropped) while the dial ladder keeps climbing. The narrower fix belongs upstream — a bounded
//! reply read on those two verbs, or a hatch to re-arm the read timeout for one verb — and is
//! REPORTED rather than taken here, because `crates/vike-datahub-client` is another branch's file.
//!
//! # ⚠ THE SYMBOL RE-STAMP — the single most load-bearing line in the reader
//!
//! `crates/vike-datahub/src/md/hub.rs`'s `push_trade` sets `tick.symbol = String::new()` before the
//! tape (§12.4's measured finding: a 78-character polymarket token id makes a real tape entry
//! ~142 B rather than 64, which alone breaks §5.5's memory budget). **Every `TradeTick` on this
//! wire therefore carries an EMPTY symbol**, and the envelope carries it once instead.
//!
//! [`GuiFeedSink::trade`](crate::data_sink::GuiFeedSink) deliberately ignores its `symbol` argument
//! and keys [`TradeStore`](crate::data_sink::TradeStore) by `trade.symbol`. Handing wire ticks
//! straight through would push EVERY print on EVERY venue under the key `(venue, "")`, and
//! [`core_sync::sync_from_core`](crate::core_sync::sync_from_core)'s drain — which drains under
//! each aggregator's own `(venue, symbol)` — would find nothing. Result: tick/volume charts and
//! every orderflow overlay silently empty, for ever, with no error, no log and no failing test.
//! Nothing in the design mentions it, and "everything else on the receiving end is untouched" is
//! exactly what makes it invisible. So [`MdSession::apply_frame`] re-stamps each tick from the
//! envelope, and `crates/vike-app-core/tests/md_session_scripted.rs` carries the kill proof.
//!
//! # Staleness is a receipt stamp taken HERE (§7.4)
//!
//! Every book frame lands through `sink.l2_snapshot(.., vike_model::now_ms())` — the LOCAL clock.
//! `BookStore::update` stores that as the receipt and the DOM computes `now_ms - ts > DOM_STALE_MS`;
//! two boxes' clocks differ, so a wire timestamp would make every ladder read permanently stale or
//! falsely live across an SSH tunnel. `BookSnapshot::venue_ts`/`venue_seq` ride for diagnostics.
//!
//! # Refusals split by SYNCHRONY, and the split is load-bearing for `FeedRetries`
//!
//! A refusal that returns SYNCHRONOUSLY from `subscribe_*` reaches
//! [`FeedRetries::note_error`](crate::feed_lifecycle::FeedRetries::note_error), which records
//! `LiveDataError::Unsupported` as `RetryState::Refused` and never retries it. That covers the
//! static capability matrix, the cached `md_venue=` advertisement, and a permanent refusal this
//! session has already SEEN. ⚠ **An asynchronous permanent refusal — one that arrives on the wire
//! after `subscribe_depth` already answered `Ok` — cannot reach `FeedRetries` at all**: `ensure_depth`
//! has already recorded its `dom_depth` entry and returns early for ever. Its disclosure is the
//! per-venue status string plus one `tracing::warn!`, and nothing else. That is accepted, and it is
//! written here because a reader will otherwise assume `FeedRetries` classifies every refusal.
//! A CAP refusal is not permanent: the spec stays in the desired set and the reconciler retries it.

use std::collections::{HashMap, HashSet};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use vike_data::{LiveDataError, LiveDataSink, SubscriptionId};
use vike_datahub_client::{
    DatahubClient, MD_READ_TIMEOUT, MdBye, MdFrame, MdLane, MdRefusal, MdSessionId, MdSpec,
    MdSubscribedInfo, MdUpdatedInfo, NodeKeys, Response, Scope, advertised_md_venues, read_frame,
};

use crate::data_sink::{BookStore, DirectBarStore, GuiFeedSink, TradeStore};
use crate::feed_lifecycle::{FeedMap, RETRY_BASE, RETRY_MAX, retry_backoff};
use crate::split_plane::LOCAL_FEED_VENUES;

/// What [`build`] hands the shell: the `App::feeds` map and the `App::feed_statuses` map, already
/// populated. Returned as a struct rather than a tuple so the shell's binding is one line and names
/// what it binds — `crates/vike-desktop` is RATCHETED (`crates/vike-ops/tests/
/// ci_excluded_gui_shell_ratchet.rs`) and every line of shape spent there is a line not spent on
/// something that had to be there.
pub struct MdMount {
    /// The session itself — held by the shell so it can push the late-bound address
    /// ([`MdSession::set_addr`]) and the late-bound observe-key name ([`MdSession::set_key_name`]),
    /// and read a key's tape-gap epoch ([`MdSession::tape_gap_epoch`]) inside the fold. Teardown
    /// needs no separate handle: every `DatahubFeed` in `feeds` carries a clone and
    /// `App::run_bounded_teardown` already fans `DataClient::shutdown` over them.
    pub session: Arc<MdSession>,
    /// One [`DatahubFeed`](crate::datahub_feed::DatahubFeed) per venue slug, over the shared
    /// session. The keys are [`LOCAL_FEED_VENUES`], which is `&'static str` — the type `FeedMap`
    /// demands and the same routing keys [`feed_lifecycle`](crate::feed_lifecycle) resolves.
    pub feeds: FeedMap,
    /// One status line per venue, the handles the Connections tool reads per frame. Structurally
    /// FIXED at build time: the session rewrites the strings and never adds or removes a key.
    pub feed_statuses: HashMap<String, Arc<Mutex<String>>>,
}

/// The server's advertisement, cached off the first successful handshake.
///
/// ⚠ **Never cleared on a link drop, and that diverges from
/// [`BridgeHandle::advertised_datahub`](crate::observe_bridge::BridgeHandle)'s clear-on-drop
/// discipline on purpose.** That cell carries an ADDRESS — a routing fact, and a different daemon
/// at the same address may front a different datahub, so it must not outlive its connection. This
/// one carries the capability set of a server already identified BY address. Clearing it on a blip
/// would flip every unserved venue from "refused, known" back to "unknown, accepted" on every
/// reconnect: churn carrying no information.
///
/// ⚠ **It IS cleared when the DIALLED address changes — and "dialled" is the load-bearing word,
/// not "connected".** `dial` caches the advertisement BEFORE it refuses, deliberately (see that
/// function's doc), so a server advertising no `market_data` leaves `Caps { market_data: false }`
/// behind while no connection was ever established. Keyed on a CONNECTION, the clear could not fire
/// for exactly that value: `MdSession::want`'s rung 2 would answer `Unsupported(NO_MD_PLANE)` for
/// the life of the process, on every later address too, and `FeedRetries` records that answer as
/// `RetryState::Refused` and never re-asks. So `reconcile_loop` tracks the address every cached fact
/// DESCRIBES — set by an ATTEMPT, not by a success — and clears on a change to it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Caps {
    /// Whether the server advertised `market_data` at all.
    pub market_data: bool,
    /// The `md_venue=<slug>` set, in advertisement order.
    pub venues: Vec<String>,
}

impl Caps {
    fn from_features(features: &[String]) -> Caps {
        Caps {
            market_data: features.iter().any(|f| f == vike_datahub_client::FEATURE_MARKET_DATA),
            venues: advertised_md_venues(features),
        }
    }

    fn serves(&self, venue: &str) -> bool {
        self.venues.iter().any(|v| v == venue)
    }
}

/// One entry of the DESIRED set: the spec plus how many live [`SubscriptionId`]s hold it.
///
/// Refcounted rather than a bare set because two callers can legitimately want one key — the DOM's
/// depth leg and a chart's trade leg are different lanes, but two DOM windows on one symbol are the
/// same key — and an `unsubscribe` from one of them must not silently stop the other's stream.
struct Wanted {
    spec: MdSpec,
    holders: u32,
}

/// The desktop's ONE market-data session. Shared as an `Arc` by every
/// [`DatahubFeed`](crate::datahub_feed::DatahubFeed) and by both background threads; every field
/// sits behind its own lock, so this type is `Send + Sync` and `DatahubFeed` is `Send` (which
/// [`FeedMap`]'s `Box<dyn DataClient + Send>` requires).
///
/// ⚠ **Lock order, stated once so it cannot be got wrong: `desired` → `served` → `refused` →
/// `gapped` → `caps` → `gap_epochs`.** No lock is ever held across a socket call, a sink call or a
/// `wake()`. (`link` is outside the order because it is never held ALONGSIDE another: every touch
/// is a statement-scoped read or write.)
pub struct MdSession {
    /// The datahub address, LATE-BOUND. ⚠ It is not known at `App::new`:
    /// [`datahub_resolve::resolve_datahub_addr`](crate::datahub_resolve::resolve_datahub_addr)'s
    /// second rung is the active backend's `Welcome` advertisement, which arrives only after the
    /// observe bridge handshakes — asynchronously, after `App::new` has returned. A session built
    /// with the address as a constructor argument would mount NOTHING, permanently, on any box
    /// configured purely by the daemon's advertisement. The shell pushes it once per frame through
    /// [`MdSession::set_addr`], which is one line where a redesign of `App::new` would have been
    /// many.
    addr: Mutex<Option<String>>,
    desired: Mutex<Vec<Wanted>>,
    /// The server's AUTHORITATIVE accepted set — depths already clamped. ⚠ A failed `md_update`
    /// leaves this UNTOUCHED: a failure means UNKNOWN, never EMPTY, which is
    /// `crates/vike-recorder/src/session.rs`'s `SubscriptionSet::reconcile` rule and the reason
    /// this is not rewritten from a guess.
    served: Mutex<Vec<MdSpec>>,
    /// PERMANENT refusals only (`MdRefusal::is_permanent`), so a later `subscribe_*` for the same
    /// key can be refused SYNCHRONOUSLY and `FeedRetries` can classify it.
    refused: Mutex<Vec<(MdSpec, MdRefusal)>>,
    /// BOOK-lane `(venue, symbol)` keys the server has disclosed as gapped or stale and that have
    /// not produced a snapshot since — i.e. the keys whose ladder this client has just EMPTIED.
    ///
    /// ⚠ It exists so [`MdSession::refresh_statuses`] cannot render `N/N stream(s) live` over a
    /// blank ladder: `served` is the server's ACCEPTED set, which a `Status::GapStart` does not
    /// change, so "subscribed" and "flowing" are different questions and only the first one has an
    /// answer in `served`. An operator staring at an empty DOM being told by the Connections tool
    /// that the venue is fine is the same operator-facing failure the status wording below exists
    /// to kill, one layer out.
    ///
    /// ⚠ **A DATA frame clears the mark, a `Status::Live` is not waited for**, and that is not
    /// laziness. `vike_bridge_core::stream_health`'s `recover` emits `Live` only to CLOSE an open
    /// gap (`recover_without_a_gap_is_none`), so a depth feed that has been healthy since it started
    /// emits no status at all — and `MdHub::attach_frames` hands every new subscriber
    /// `GapStart{now_ms}` as the honest "wanted, not yet live". Keyed on `Live`, every such key
    /// would read gapped for ever while its ladder painted perfectly. A snapshot arriving IS the
    /// flow signal.
    ///
    /// TRADES keys are deliberately absent: this client has no flow signal for a tape (five of the
    /// six venues' trade lanes emit no `stream_status` at all), so counting one as not-flowing would
    /// be a guess in the other direction.
    gapped: Mutex<HashSet<(String, String)>>,
    caps: Mutex<Option<Caps>>,
    session: Mutex<Option<MdSessionId>>,
    /// A `try_clone` of the live stream, held ONLY so [`MdSession::begin_stop`] can shut it down
    /// and return the reader from its blocking read at once.
    stream_handle: Mutex<Option<TcpStream>>,
    reader: Mutex<Option<JoinHandle<()>>>,
    reconciler: Mutex<Option<JoinHandle<()>>>,
    /// Monotonic per-`(venue, symbol)` tape-gap counter — the seam between the reader thread and the
    /// aggregators, which live on the FRAME thread and cannot be touched from here. See
    /// [`MdSession::tape_gap_epoch`].
    gap_epochs: Mutex<HashMap<(String, String), u64>>,
    statuses: HashMap<&'static str, Arc<Mutex<String>>>,
    /// The LINK half of every status line — `datahub <addr>` / `reconnecting (…)` / `idle` — kept
    /// so a re-render triggered by a health change ([`MdSession::restate_statuses`]) does not have
    /// to invent one, and cannot therefore contradict the line the reconciler last wrote.
    link: Mutex<String>,
    /// Session-wide, so an id minted for one venue can never collide with another's.
    next_id: AtomicU64,
    stop: AtomicBool,
    wake: Box<dyn Fn() + Send + Sync>,
    sink: GuiFeedSink,
    /// The datahub observe key this session signs with, RESOLVED. Behind a lock and re-resolvable
    /// because the NAME is a per-backend setting ([`crate::backend_registry::BackendRecord`]'s
    /// `datahub_observe_key`) and the shell switches backends at runtime — see
    /// [`MdSession::set_key_name`].
    keys: Mutex<Option<NodeKeys>>,
    /// The key NAME the shell wants, pushed once per frame beside the address. The reconciler
    /// notices a change and re-resolves OFF the frame thread, because resolving opens a file.
    key_name: Mutex<String>,
    /// The composition root's ONE boot walk and ONE `std::env::vars()` sweep, retained so the
    /// reconciler can re-run [`crate::backend_registry::datahub_observe_keys`] when the name moves.
    /// This crate still reads no environment of its own: both arrive as parameters to [`build`].
    settings_dir: Option<String>,
    env: HashMap<String, String>,
    /// The `Condvar`'s companion: a GENERATION counter, not a boolean. A poke bumps it; the
    /// reconciler samples it before it reads any state and parks only while it is unchanged — so a
    /// `want` landing between the reconciler's check and its `wait` cannot be lost into a 60 s park.
    poke_gen: Mutex<u64>,
    cv: Condvar,
}

/// Recover a poisoned lock rather than propagating the panic. A panic in one background thread must
/// not convert into a frozen GUI — the same `unwrap_or_else(PoisonError::into_inner)` discipline
/// the server side of this wire uses for exactly the same reason.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

// ── The `&'static str` refusal vocabulary ───────────────────────────────────────────────────────
// ⚠ `LiveDataError::Unsupported` carries a `&'static str` and a wire refusal arrives as an owned
// `String`; the only conversion is `Box::leak`, which is unbounded. `MdRefusal::LaneUnsupported`'s
// own doc names that trap. So a refusal maps onto ONE of these locals and the server's own text
// goes into the per-venue STATUS STRING instead, where it is read and not leaked.

/// A venue this datahub build serves no market-data client for.
const NOT_ADVERTISED: &str = "this datahub advertises no market-data client for that venue (see the Connections tool for \
     the served set)";
/// A server with no market-data plane at all.
///
/// ⚠ **The ARM comes first and the rebuild second**, and the order is the correction
/// `crates/vike-datahub-client/src/client.rs`'s own `md_subscribe` refusal carries: the plane
/// compiles on a default build and `--features live-feeds` alone gates NO code, so an unset
/// `VIKE_DATAHUB_LIVE` is the cause in every case where the operator has not also chosen venues.
/// Leading with the rebuild sends them to recompile a server that would have worked.
const NO_MD_PLANE: &str = "this datahub serves no market-data plane — set VIKE_DATAHUB_LIVE=1 on the server and \
     restart it. A rebuild is a separate question and only afterwards: `--features live-feeds` alone gates no code, \
     and the per-venue `live-<venue>` features are what link a venue's feed";
/// A key this session has already been permanently refused for.
const SERVER_REFUSED: &str =
    "this datahub permanently refused that key (the per-venue status line carries its own words)";
/// The declared, deliberate bar-lane refusal — see [`crate::datahub_feed`].
pub(crate) const NO_BAR_LANE: &str = "the datahub market-data wire serves no live bar lane: a chart's HISTORY comes from \
     Request::LoadBars/Backfill and its live tail from the backend snapshot";
/// The declared, deliberate quote-lane refusal — see [`crate::datahub_feed`].
pub(crate) const NO_QUOTE_LANE: &str = "the datahub market-data wire serves no quotes lane: GuiFeedSink::quote is a spelled no-op, so \
     a quotes lane would deliver into nothing";

/// How long [`bounded`] waits for a control exchange — `connect` + `md_subscribe`, or `connect` +
/// `md_update` — before abandoning it and letting the dial ladder climb.
///
/// ⚠ **ADOPTED from [`MD_READ_TIMEOUT`], not invented, and this whole design still introduces no
/// new timing constant.** That number is this wire's own "a peer that has said nothing for three
/// heartbeats is not slow, it is gone", and these two verbs are a hub lookup and an answer — they
/// COMPUTE nothing, which is the exact argument `arm_request_timeouts` uses to refuse a ceiling on
/// the verbs that do. Anything shorter would abort a live exchange on a loaded box; anything longer
/// buys nothing, because the ladder re-dials afterwards either way.
const CTRL_DEADLINE: Duration = MD_READ_TIMEOUT;

/// A keyword-free label for one wire refusal, for the per-venue STATUS STRING.
///
/// ⚠ **It exists because a status string is PARSED and `{why:?}` is not ours to control.**
/// `MdRefusal::LaneUnsupported` carries `vike_data::require_live_verb`'s own text verbatim (that is
/// the point of the variant — the wire's refusal set cannot drift from the declared matrix), and
/// every one of those five messages names `VenueCaps.live_data`, which contains `live`, which is a
/// rung-3 keyword in `vike_model::feed_status::parse_feed_status`. Interpolated, it classified a
/// REFUSED venue as **Connected**. `VenueNotServed` carries a server-supplied venue list and is the
/// same hazard wearing different text. So the variant picks the words and the server's own go to
/// the `tracing::warn!` [`MdSession::absorb_refusals`] already emits.
///
/// The match is exhaustive with no `_` arm, so a new `MdRefusal` cannot inherit a label — and the
/// three CAP variants are here even though [`MdSession::refresh_statuses`] only ever sees permanent
/// ones (`absorb_refusals` records nothing else), because a `_` arm is exactly how that would stop
/// being true silently.
fn refusal_label(why: &MdRefusal) -> &'static str {
    match why {
        MdRefusal::UnknownVenue => "not a venue this build knows",
        MdRefusal::VenueNotServed(_) => "this datahub links no client for that venue",
        MdRefusal::LaneUnsupported(_) => "that venue declares no such lane",
        MdRefusal::SymbolRejected(_) => "that symbol is blank, too long, or holds a control byte",
        MdRefusal::KeyCapTotal { .. } => "the server's key budget is full",
        MdRefusal::KeyCapVenue { .. } => "that venue's key budget is full",
        MdRefusal::SpecCapSession { .. } => "this session holds its maximum number of keys",
    }
}

/// Build the session, its two threads, and the feed/status maps the shell installs.
///
/// `settings_dir` is the binary's ONE boot walk (`vike_boot::Booted`), taken as a PARAMETER — this
/// crate reads no environment, so nothing here joins `LIBRARY_PIN`. `key_name` is the datahub
/// observe-key NAME (`backend_registry::datahub_observe_key_name`); `env` is the composition root's
/// single `std::env::vars()` sweep.
///
/// ⚠ **The [`GuiFeedSink`] is constructed HERE, not in the shell**, and its `bars` store is PRIVATE
/// to it: this wire serves no bar lane (design §10), so nothing will ever write that store, and
/// `App::direct_bars` must stay `None` —
/// [`split_plane::direct_bars_mount`](crate::split_plane::direct_bars_mount) says so for
/// `ObserveOnly`, and [`core_sync::sync_from_core`](crate::core_sync::sync_from_core) takes
/// `direct_bars.is_some()` AS the render-source mode signal, so binding it `Some` would reassign
/// every kline series on the five CEX venues to a store nothing fills — blank charts.
pub fn build(
    settings_dir: Option<&str>,
    env: &HashMap<String, String>,
    key_name: &str,
    books: Arc<BookStore>,
    trades: Arc<TradeStore>,
    wake: impl Fn() + Send + Sync + 'static,
) -> MdMount {
    let (keys, notices) =
        crate::backend_registry::datahub_observe_keys(settings_dir, env, key_name);
    for n in notices {
        tracing::warn!("datahub market-data: {n}");
    }
    let statuses: HashMap<&'static str, Arc<Mutex<String>>> = LOCAL_FEED_VENUES
        .iter()
        .map(|v| (*v, Arc::new(Mutex::new("disconnected — no datahub configured yet".to_string()))))
        .collect();
    let feed_statuses: HashMap<String, Arc<Mutex<String>>> =
        statuses.iter().map(|(v, h)| ((*v).to_string(), h.clone())).collect();

    let session = Arc::new(MdSession {
        addr: Mutex::new(None),
        desired: Mutex::new(Vec::new()),
        served: Mutex::new(Vec::new()),
        refused: Mutex::new(Vec::new()),
        gapped: Mutex::new(HashSet::new()),
        caps: Mutex::new(None),
        session: Mutex::new(None),
        stream_handle: Mutex::new(None),
        reader: Mutex::new(None),
        reconciler: Mutex::new(None),
        gap_epochs: Mutex::new(HashMap::new()),
        statuses,
        link: Mutex::new("disconnected".to_string()),
        next_id: AtomicU64::new(1),
        stop: AtomicBool::new(false),
        wake: Box::new(wake),
        // ⚠ The private, never-written bar store — see this function's doc.
        sink: GuiFeedSink { books, trades, bars: Arc::new(DirectBarStore::default()) },
        keys: Mutex::new(keys),
        key_name: Mutex::new(key_name.to_string()),
        settings_dir: settings_dir.map(str::to_string),
        env: env.clone(),
        poke_gen: Mutex::new(0),
        cv: Condvar::new(),
    });

    let feeds: FeedMap = LOCAL_FEED_VENUES
        .iter()
        .map(|venue| {
            let feed: Box<dyn vike_data::DataClient + Send> =
                Box::new(crate::datahub_feed::DatahubFeed::new(venue, session.clone()));
            (*venue, feed)
        })
        .collect();

    let driver = session.clone();
    if let Ok(h) = std::thread::Builder::new()
        .name("md-reconcile".to_string())
        .spawn(move || reconcile_loop(driver))
    {
        *lock(&session.reconciler) = Some(h);
    } else {
        session.set_all_statuses(
            "market-data error: the OS refused the reconciler thread — no market data",
        );
        tracing::error!("md_session: the OS refused the reconciler thread — no market data");
    }

    MdMount { session, feeds, feed_statuses }
}

impl MdSession {
    // ── the FRAME-THREAD surface: every method here is wait-free ─────────────────────────────────

    /// Point (or re-point) the session at a datahub address. Called once per frame by the shell
    /// from the resolution it already computes; a value equal to the current one is a no-op, so the
    /// per-frame cost is one `Mutex` and a compare.
    ///
    /// ⚠ **A change to a DIFFERENT `Some` tears the stream down and applies §7.3 rule 4's bracket**
    /// (forget every book, forget every tape position, disclose) rather than carrying state across.
    /// That contradicts [`backend_conn`](crate::backend_conn)'s standing claim that "a switch leaves
    /// this plane running and painted" — a claim that was true while the feed plane was venue-direct
    /// and stops being true now the plane is sourced through the backend's ADVERTISED datahub. The
    /// conservative reading is the correct one: a different server is a different book.
    ///
    /// ⚠ **`None` is NOT a different server — it is "no new answer", and the session HOLDS what it
    /// has.** `crates/vike-app-core/src/datahub_resolve.rs`'s second rung is the active backend's
    /// advertisement, and [`observe_bridge`](crate::observe_bridge)'s reconnect path CLEARS that
    /// cell the moment the link drops, restoring it only after the next handshake a `DIAL_BACKOFF`
    /// later. On the documented default box — no explicit `config.datahub_addr`, the advertisement
    /// IS the whole address ladder — every tradehub blip the bridge is designed to absorb
    /// transparently would otherwise arrive here as a re-point: stream torn down, [`Caps`] thrown
    /// away (the exact churn that type's doc argues against), every permanent refusal re-opened, and
    /// every venue's line rewritten to name a `config.datahub_addr` misconfiguration that does not
    /// exist. A datahub is a DIFFERENT PROCESS from the tradehub that advertised it and did not go
    /// anywhere. The cost of holding is narrow and stated: an operator who deletes
    /// `config.datahub_addr` on a box with no advertisement keeps streaming from the server already
    /// connected until the process restarts — which is data that is real, from a server that is up.
    pub fn set_addr(&self, addr: Option<&str>) {
        let next = addr.map(str::to_string);
        {
            let mut cur = lock(&self.addr);
            if *cur == next {
                return;
            }
            *cur = next;
        }
        self.poke();
    }

    /// Point the session at a datahub observe key NAME — [`crate::backend_registry::
    /// datahub_observe_key_name`] of the ACTIVE backend record, pushed once per frame beside
    /// [`MdSession::set_addr`] for the same reason: it is a property of a record the shell switches
    /// at runtime, and a session that took it once at `App::new` would sign the NEW backend's
    /// datahub with the PREVIOUS record's key — `bad mac` at the handshake with nothing on screen
    /// that could explain it, which is the exact symptom
    /// `crates/vike-app-core/src/backend_editor.rs`'s
    /// `an_edit_preserves_the_datahub_key_name_the_form_does_not_carry` names.
    ///
    /// Wait-free: a compare and a store. RESOLVING the name opens a file, so it happens on the
    /// reconciler (`resolve_keys_if_renamed`) and never here.
    pub fn set_key_name(&self, name: &str) {
        {
            let mut cur = lock(&self.key_name);
            if *cur == name {
                return;
            }
            *cur = name.to_string();
        }
        self.poke();
    }

    /// This `(venue, symbol)`'s tape-gap epoch — a monotone counter the reader bumps on every
    /// disclosed hole, and [`core_sync::sync_from_core`](crate::core_sync::sync_from_core)'s only
    /// route to the aggregators, which live on the FRAME thread where the reader cannot reach them.
    ///
    /// ⚠ **The fold calls this AFTER its drain, per key, and that is why it is a CALL rather than a
    /// map.** It was a `HashMap` snapshot taken as an argument expression, i.e. strictly BEFORE
    /// `sync_from_core` was entered, while the drain it is meant to be read after happens hundreds
    /// of lines inside — so the rule the fold asserts in three places did not hold, and the whole
    /// prologue of that fold was a window in which a gap could be disclosed, its buffered ticks
    /// drained by the reader, and the POST-hole batch then folded into an unrepaired aggregator.
    /// The next frame repairs, but [`TickVolAgg::mark_gap`](crate::tickvol::TickVolAgg::mark_gap)
    /// keeps `closed` whole on the argument that every bar in it closed BEFORE the hole — so a
    /// `100t` bar made of 60 pre-hole and 40 post-hole prints is permanent and unmarked.
    #[must_use]
    pub fn tape_gap_epoch(&self, venue: &str, symbol: &str) -> u64 {
        lock(&self.gap_epochs).get(&(venue.to_string(), symbol.to_string())).copied().unwrap_or(0)
    }

    /// Raise the stop flags and RETURN — the `begin_shutdown` half of the two-phase teardown.
    /// Shutting the socket is what returns the reader from its blocking read AT ONCE instead of at
    /// the 45 s deadline, which is the whole reason this phase exists. Idempotent, and safe to call
    /// concurrently from every feed — which is exactly what `App::run_bounded_teardown` does.
    pub fn begin_stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(s) = lock(&self.stream_handle).as_ref() {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
        self.poke();
    }

    /// [`MdSession::begin_stop`] plus a JOIN of both threads. ⚠ `App::run_bounded_teardown` fans
    /// `DataClient::shutdown` out in PARALLEL over every feed, so this is called N times at once on
    /// ONE session: the handles are `take`n under a lock, so exactly one caller joins each thread
    /// and the rest return immediately. An unguarded double join would panic five times out of six.
    pub fn stop_and_join(&self) {
        self.begin_stop();
        let reader = lock(&self.reader).take();
        let reconciler = lock(&self.reconciler).take();
        if let Some(h) = reader {
            let _ = h.join();
        }
        if let Some(h) = reconciler {
            let _ = h.join();
        }
    }

    /// The cached advertisement, or `None` before any handshake has landed.
    pub(crate) fn caps(&self) -> Option<Caps> {
        lock(&self.caps).clone()
    }

    /// Record the intent to stream one key, and answer the SYNCHRONOUS refusal ladder.
    ///
    /// ⚠ **The intent is recorded BEFORE anything is sent** — structurally, because this method only
    /// ever touches `desired` and the reconciler is the only thing that sends. Frames for an added
    /// key can arrive on the stream socket before the ack arrives on the control socket, which is
    /// the same ordering rule `crates/vike-recorder/src/runtime.rs` states as *"membership is
    /// published BEFORE the subscription is made"*. Here it cannot be got wrong: routing is by
    /// `(venue, symbol)` straight into the shared stores and consults no intent ledger at all.
    pub(crate) fn want(
        &self,
        venue: &'static str,
        symbol: &str,
        lane: MdLane,
    ) -> Result<SubscriptionId, LiveDataError> {
        // 1. The STATIC capability matrix — the same authority the server consults, so a venue that
        //    declares no such lane is refused here with no wire traffic at all.
        vike_data::require_live_verb(venue, lane.live_verb())?;
        // 2. The cached advertisement. `None` (nothing has handshaked yet) accepts OPTIMISTICALLY:
        //    blocking here is forbidden, and a premature `Unsupported` would be recorded by
        //    `FeedRetries` as PERMANENT and never re-asked.
        if let Some(caps) = self.caps() {
            if !caps.market_data {
                return Err(LiveDataError::Unsupported(NO_MD_PLANE));
            }
            if !caps.serves(venue) {
                return Err(LiveDataError::Unsupported(NOT_ADVERTISED));
            }
        }
        // 3. A permanent refusal this session has already SEEN for this exact key.
        if lock(&self.refused)
            .iter()
            .any(|(s, _)| s.venue == venue && s.symbol == symbol && s.lane == lane)
        {
            return Err(LiveDataError::Unsupported(SERVER_REFUSED));
        }
        let spec = MdSpec {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            lane,
            // `None` = the server's own `MD_DEPTH_LEVELS_DEFAULT` (50/side). Asking for more would
            // inflate the frame for EVERY subscriber of that key — the server resolves the depth as
            // the max over subscribers — for rows the DOM does not paint.
            depth_levels: None,
        };
        {
            let mut desired = lock(&self.desired);
            match desired.iter_mut().find(|w| w.spec.key() == spec.key()) {
                Some(w) => w.holders = w.holders.saturating_add(1),
                None => desired.push(Wanted { spec, holders: 1 }),
            }
        }
        self.poke();
        Ok(SubscriptionId(self.next_id.fetch_add(1, Ordering::Relaxed)))
    }

    /// Drop one holder of a key; the key leaves the desired set when the last one goes. No I/O.
    pub(crate) fn unwant(&self, spec: &MdSpec) {
        let mut changed = false;
        {
            let mut desired = lock(&self.desired);
            if let Some(i) = desired.iter().position(|w| w.spec.key() == spec.key()) {
                desired[i].holders = desired[i].holders.saturating_sub(1);
                if desired[i].holders == 0 {
                    desired.remove(i);
                    changed = true;
                }
            }
        }
        if changed {
            self.poke();
        }
    }

    // ── internals ───────────────────────────────────────────────────────────────────────────────

    fn stopping(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    fn poke(&self) {
        {
            let mut g = lock(&self.poke_gen);
            *g = g.wrapping_add(1);
        }
        self.cv.notify_all();
    }

    fn poke_generation(&self) -> u64 {
        *lock(&self.poke_gen)
    }

    /// Park until poked, or for `at_most`. `seen` is the generation sampled BEFORE this iteration
    /// read any state: if it has moved, something changed while we worked and we must not sleep.
    fn park(&self, seen: u64, at_most: Duration) {
        let g = lock(&self.poke_gen);
        if *g != seen {
            return;
        }
        let _ = self.cv.wait_timeout(g, at_most);
    }

    fn set_all_statuses(&self, text: &str) {
        for h in self.statuses.values() {
            *lock(h) = text.to_string();
        }
    }

    /// Re-render every venue's status line from the CURRENT desired/served/refused/gapped state.
    /// One place, so a line can never describe a state two frames old.
    ///
    /// ⚠ **THESE STRINGS ARE PARSED, NOT DISPLAYED, AND FOUR WORDS ARE LOAD-BEARING, NOT ONE.**
    /// `vike_model::feed_status::parse_feed_status` is an ORDERED substring classifier — it tests
    /// `disconnected`, then `fault|error|failed`, then **`connected` | `live` | `streaming` |
    /// `subscribed`**, then `connect*` — and the Connections tool renders whatever it answers. So
    /// NONE of those four may appear on a line that is not serving, and this was got wrong twice by
    /// guarding `live` alone:
    ///
    /// * `{link} — nothing subscribed on this venue` contains **`subscribed`** and therefore read
    ///   **Connected**. That is not an edge case, it is the steady state: `reconcile_loop` renders
    ///   it through `refresh_statuses("idle")` on the `desired_specs().is_empty()` branch, which
    ///   every venue is in from the first frame an address resolves until a DOM window or chart
    ///   opens on it. At launch ALL SIX [`LOCAL_FEED_VENUES`] read Connected with no socket open,
    ///   and after one binance ladder opens the other five stay that way for the session. Note that
    ///   rung 1's `idle` test is `==`, not `contains`, so prefixing the link does not save it.
    /// * the refusal arm interpolated `{why:?}`, and `MdRefusal::LaneUnsupported` carries
    ///   `vike_data::require_live_verb`'s own words — every one of which contains
    ///   `VenueCaps.live_data`, i.e. **`live`**. So a refused venue read Connected too, and the arm
    ///   OUTRANKED every other, so it kept reading Connected while the link was down.
    ///
    /// The rule this file now follows: **no uncontrolled text is interpolated into a parsed
    /// string.** A refusal renders through [`refusal_label`], a keyword-free `&'static str` per
    /// variant; the server's own words go to the `tracing::warn!` `absorb_refusals` already emits.
    ///
    /// ⚠ `served` means SUBSCRIBED, not FLOWING — the last arm's `live` count therefore subtracts
    /// the book keys `gapped` holds, or a venue whose ladder this client has just emptied would
    /// still read `1/1 stream(s) live`.
    fn refresh_statuses(&self, link: &str) {
        *lock(&self.link) = link.to_string();
        let desired = lock(&self.desired);
        let served = lock(&self.served);
        let refused = lock(&self.refused);
        let gapped = lock(&self.gapped);
        for (venue, handle) in &self.statuses {
            let wanted = desired.iter().filter(|w| w.spec.venue == *venue).count();
            let serving = served.iter().filter(|s| s.venue == *venue);
            let mut live = 0usize;
            let mut stalled = 0usize;
            for s in serving {
                let book_lane = matches!(s.lane, MdLane::Depth | MdLane::Book);
                if book_lane && gapped.contains(&(s.venue.clone(), s.symbol.clone())) {
                    stalled += 1;
                } else {
                    live += 1;
                }
            }
            let refusal = refused.iter().find(|(s, _)| s.venue == *venue);
            let text = match (wanted, live, refusal) {
                // A venue with streams actually flowing reads Connected even if ANOTHER of its keys
                // was refused: the refusal is disclosed in the log and in the count, and reporting
                // a working venue as not-working is the worse error. This arm is the ONLY one that
                // may carry a rung-3 keyword.
                (_, 1.., _) => format!("{link} — {live}/{wanted} stream(s) live"),
                // Refused and serving nothing. No rung-3 keyword at all, so this reads Unknown.
                // Deliberate — the LINK is fine and the DATA is not flowing, and
                // `parse_feed_status` has no rung that says that. Error would blame the transport.
                (_, 0, Some((s, why))) => {
                    format!("{link} — refused {}/{:?} ({})", s.symbol, s.lane, refusal_label(why))
                }
                // Nothing wanted here. ⚠ NOT "nothing subscribed" — see this function's doc.
                (0, 0, None) => format!("{link} — no streams wanted on this venue"),
                // Wanted but not yet serving. This arm carries `connecting`, so it lands on rung 4
                // whatever `{link}` says, and adds no rung-3 keyword to outrank it.
                //
                // ⚠ `stalled` rides HERE rather than in an arm of its own, and the reason is that
                // the two states are indistinguishable on screen and nearly so on the wire: a key
                // whose book was dropped by a disclosed `GapStart` and a key that has not received
                // its first snapshot both show a BLANK ladder, and `MdHub::attach_frames` hands
                // every brand-new subscriber `GapStart{now_ms}` as the honest "wanted, not yet
                // live". Rendering the first as a `fault` would therefore paint an Error dot over
                // an ordinary subscribe. What the operator needs is the half that WAS being got
                // wrong — that this venue is not flowing — and that is now true on both paths.
                (_, 0, None) => {
                    let why = if stalled > 0 { ", the venue feed has sent no book" } else { "" };
                    format!("{link} — connecting, 0/{wanted} stream(s){why}")
                }
            };
            *lock(handle) = text;
        }
    }

    /// [`MdSession::refresh_statuses`] against the link text it last wrote — what the READER calls
    /// when a health change (not a subscription change) has moved what a line should say.
    fn restate_statuses(&self) {
        let link = lock(&self.link).clone();
        self.refresh_statuses(&link);
    }

    /// `add`/`remove`, computed against the server's authoritative `served` set.
    ///
    /// The two rules that matter, both `SubscriptionSet::reconcile`'s: **stop what left and start
    /// what arrived, and leave survivors strictly alone** (a survivor re-subscribed would drop the
    /// server's book state for a key somebody is looking at).
    fn diff(&self) -> (Vec<MdSpec>, Vec<MdSpec>) {
        let desired = lock(&self.desired);
        let served = lock(&self.served);
        let add: Vec<MdSpec> = desired
            .iter()
            .filter(|w| !served.iter().any(|s| s.key() == w.spec.key()))
            .map(|w| w.spec.clone())
            .collect();
        let remove: Vec<MdSpec> = served
            .iter()
            .filter(|s| !desired.iter().any(|w| w.spec.key() == s.key()))
            .cloned()
            .collect();
        (add, remove)
    }

    fn desired_specs(&self) -> Vec<MdSpec> {
        lock(&self.desired).iter().map(|w| w.spec.clone()).collect()
    }

    /// Fold one `refused` list from the wire: a PERMANENT refusal is recorded and leaves the desired
    /// set (nothing can make it succeed); a CAP refusal stays wanted and is retried on backoff,
    /// because a cap frees up when another window closes.
    fn absorb_refusals(&self, refused: Vec<(MdSpec, MdRefusal)>) {
        for (spec, why) in refused {
            if why.is_permanent() {
                tracing::warn!(
                    venue = %spec.venue,
                    symbol = %spec.symbol,
                    lane = ?spec.lane,
                    "datahub market-data: permanently refused — {why:?}"
                );
                lock(&self.desired).retain(|w| w.spec.key() != spec.key());
                let mut refused_now = lock(&self.refused);
                if !refused_now.iter().any(|(s, _)| s.key() == spec.key()) {
                    refused_now.push((spec, why));
                }
            } else {
                tracing::info!(
                    venue = %spec.venue,
                    symbol = %spec.symbol,
                    "datahub market-data: capped for now, will retry — {why:?}"
                );
            }
        }
    }

    /// Bump a key's tape epoch — the reader's only way to reach the frame thread's aggregators.
    fn bump_gap(&self, venue: &str, symbol: &str) {
        let mut g = lock(&self.gap_epochs);
        let e = g.entry((venue.to_string(), symbol.to_string())).or_insert(0);
        *e = e.saturating_add(1);
    }

    /// §7.3 rule 4's OPENING bracket, synthesized locally because the server that would have sent
    /// it is the one that went away: forget every book and every tape position this connection
    /// held, and disclose it. Never resume a book or a tape across a socket.
    fn open_gap_bracket(&self, why: &str) {
        let now = vike_model::now_ms();
        let held: Vec<MdSpec> = lock(&self.served).clone();
        for spec in &held {
            match spec.lane {
                MdLane::Depth | MdLane::Book => {
                    self.sink.books.remove(&spec.venue, &spec.symbol);
                }
                MdLane::Trades => self.bump_gap(&spec.venue, &spec.symbol),
            }
            self.sink.stream_status(
                &spec.venue,
                &spec.symbol,
                spec.lane.feed_stream_label(),
                vike_data::StreamStatus::GapStart { at_ts_ms: now },
            );
        }
        if !held.is_empty() {
            tracing::warn!(
                keys = held.len(),
                "datahub market-data stream ended ({why}) — every book dropped and every tape \
                 position forgotten; nothing is resumed across a socket"
            );
        }
    }

    /// Mark (or unmark) one BOOK key as not-flowing and re-render the venue lines if that CHANGED.
    /// Returns nothing: the only observable is the status string, and the re-render is conditional
    /// precisely so an ordinary snapshot — which arrives at the hub's publish cadence — does not
    /// take five mutexes and rewrite six strings on every frame.
    fn note_book_health(&self, venue: &str, symbol: &str, flowing: bool) {
        let key = (venue.to_string(), symbol.to_string());
        let changed = {
            let mut g = lock(&self.gapped);
            if flowing { g.remove(&key) } else { g.insert(key) }
        };
        if changed {
            self.restate_statuses();
        }
    }

    /// Route one pushed frame. Returns `false` when the stream is over.
    fn apply_frame(&self, frame: MdFrame, st: &mut ReaderState) -> bool {
        let ReaderState { last_seq, disclosed } = st;
        match frame {
            // Both lanes go through `l2_snapshot`, never `book`: the wire carries RAW LEVELS plus
            // the feed's authoritative `tick_size`, and `BookStore::update` is exactly the rebuild
            // path for those (`insert_book` wants an already-folded `L2Book`). The lanes differ
            // only by NAME, which is precisely the property `MdLane`'s split asserts.
            MdFrame::Depth(s) | MdFrame::Book(s) => {
                // A snapshot IS the flow signal for this key — see `MdSession::gapped` for why a
                // `Status::Live` cannot be waited for instead.
                self.note_book_health(&s.venue, &s.symbol, true);
                self.sink.l2_snapshot(
                    &s.venue,
                    &s.symbol,
                    s.tick_size,
                    s.bids,
                    s.asks,
                    // §7.4 — the LOCAL clock. `venue_ts`/`venue_seq` are diagnostics.
                    vike_model::now_ms(),
                );
                (self.wake)();
            }
            MdFrame::Trades { venue, symbol, ticks, seq } => {
                // §7.2's backstop: a tape `seq` jump means this connection did not receive frames
                // the server produced, so the hole is treated as a `TapeGap` of the implied size
                // rather than folded. (On the book lanes a jump is informational: the lane is
                // conflating by contract and each frame is a self-healing whole snapshot.)
                //
                // ⚠ **THE DIAGNOSIS SPLITS ON WHETHER THIS KEY HAS ALREADY BEEN DISCLOSED ON THIS
                // CONNECTION, and reading every jump as a server bug was a FALSE ACCUSATION on the
                // server's own documented path.** `crates/vike-datahub/src/md/mailbox.rs`'s
                // `Inner::owe` merges two holes for one key by taking the MINIMUM `resume_at` — so
                // the marker lands at the EARLIEST hole and `dropped` is the total, and its own doc
                // states the consequence as intended: "a later hole inside the same disclosure
                // shows up as a §7.2 `seq` jump rather than a second marker — which is exactly what
                // that backstop is for". A mailbox drop implies a backlog by definition, so that is
                // the ORDINARY slow-consumer case, not an edge. Logging it at `error!` as a SERVER
                // bug clears the default `info` console filter and sends an operator hunting a
                // defect that does not exist. The `bump_gap` is identical on both paths; only the
                // words differ, and §7.2's actual claim is the undisclosed one.
                let key = (venue.clone(), symbol.clone());
                if let Some(prev) = last_seq.get(&key).copied()
                    && seq > prev.saturating_add(1)
                {
                    if disclosed.contains(&key) {
                        tracing::warn!(
                            venue = %venue, symbol = %symbol, from_seq = prev, to_seq = seq,
                            "datahub tape sequence jumped after a TapeGap on this key — a later \
                             hole inside a MERGED disclosure (the server's `Inner::owe` discloses \
                             at the earliest hole, by design); repairing again"
                        );
                    } else {
                        tracing::error!(
                            venue = %venue, symbol = %symbol, from_seq = prev, to_seq = seq,
                            "datahub tape sequence jumped with NO TapeGap on this key — that is a \
                             SERVER bug; treating it as a gap of the implied size rather than \
                             folding a hole"
                        );
                    }
                    self.bump_gap(&venue, &symbol);
                }
                last_seq.insert(key, seq);
                for mut tick in ticks {
                    // ⚠⚠ THE RE-STAMP. The hub STRIPS this field (`md/hub.rs`'s `push_trade`) and
                    // the envelope carries it once; `GuiFeedSink::trade` keys `TradeStore` by
                    // `tick.symbol`, so an un-restamped tick lands under `(venue, "")` and every
                    // tick/volume and orderflow chart stays empty for ever, silently. See this
                    // module's doc.
                    tick.symbol.clone_from(&symbol);
                    self.sink.trade(&venue, &symbol, tick);
                }
                (self.wake)();
            }
            MdFrame::Status { venue, symbol, lane, status } => {
                let status: vike_data::StreamStatus = status.into();
                let broken = matches!(
                    status,
                    vike_data::StreamStatus::GapStart { .. }
                        | vike_data::StreamStatus::Stale { .. }
                );
                match lane {
                    // §7.3 rules 1 and 2: a book lane that has gapped or gone stale must be
                    // FORGOTTEN, not left frozen on screen. `Live` needs no action for the STORE —
                    // the next snapshot repopulates — but it does un-mark the key, so a venue that
                    // recovers before its next publish tick stops reading as not-flowing.
                    MdLane::Depth | MdLane::Book => {
                        if broken {
                            self.sink.books.remove(&venue, &symbol);
                        }
                        self.note_book_health(&venue, &symbol, !broken);
                    }
                    // ⚠ **THE TAPE GETS THE SYMMETRIC TREATMENT, and §7.3 rule 1 names only the
                    // book lanes — so this is a deliberate WIDENING, flagged rather than silent.**
                    // A venue-side hole in a trade tape is prints that were genuinely lost at the
                    // venue, and NOTHING ELSE discloses it: it increments no `entry.dropped`, so the
                    // server emits no `TapeGap`; the wire `seq` counts published FRAMES rather than
                    // venue ticks, so §7.2's backstop sees nothing either. Forwarding the notice and
                    // folding on would leave `OrderflowAgg::ingest` — which that method's own doc
                    // calls out as having no per-trade dedup and no gap concept — understating that
                    // bar's cells and every CVD value after it, permanently and undisclosed, which
                    // §6.2's "Loss here must be impossible to hide" forbids. It is reachable today
                    // rather than latent: `crates/bridges/polymarket/src/market_feed.rs`'s
                    // `PumpMode::Trades` renders `stream_label` as `"trades"`, and both its recovery
                    // and freshness paths call `sink.stream_status` with it.
                    //
                    // `bump_gap` is idempotent per epoch VALUE at the fold, and the producer-side
                    // `StreamHealth` is edge-triggered (one `Stale` per episode), so a repeated
                    // disclosure costs one rebuild, not one per frame.
                    MdLane::Trades => {
                        if broken {
                            let _ = self.sink.trades.drain(&venue, &symbol);
                            self.bump_gap(&venue, &symbol);
                            disclosed.insert((venue.clone(), symbol.clone()));
                        }
                    }
                }
                self.sink.stream_status(&venue, &symbol, lane.feed_stream_label(), status);
                (self.wake)();
            }
            MdFrame::TapeGap { venue, symbol, dropped, from_seq, to_seq } => {
                // ORDER MATTERS: discard what is buffered, THEN bump the epoch. The frame thread
                // reads the epoch after ITS drain, so the two together mean "everything that was
                // in flight for this key at the moment of the hole is gone".
                let _ = self.sink.trades.drain(&venue, &symbol);
                self.bump_gap(&venue, &symbol);
                last_seq.insert((venue.clone(), symbol.clone()), to_seq);
                // §7.2's backstop reads this: a LATER jump on a key already disclosed is the
                // server's merged-`owe` path, not a server bug. See the `Trades` arm.
                disclosed.insert((venue.clone(), symbol.clone()));
                tracing::warn!(
                    venue = %venue, symbol = %symbol, dropped, from_seq, to_seq,
                    "datahub tape gap disclosed — this key's aggregators are rebuilt from empty; \
                     the missing prints come from the store, never from this wire"
                );
                (self.wake)();
            }
            // The READ itself is the liveness proof; nothing else to do, and deliberately no wake —
            // a heartbeat changes no pixel, and it must never refresh a book's staleness receipt.
            MdFrame::Heartbeat => {}
            MdFrame::Bye(why) => {
                match why {
                    MdBye::TooSlow { lapses } => tracing::warn!(
                        lapses,
                        "datahub ended the market-data stream: this client fell behind. It will \
                         re-dial; if this repeats, fewer open ladders or a faster link is the fix"
                    ),
                    MdBye::ServerStopping => {
                        tracing::info!("datahub is stopping — market-data stream closed")
                    }
                    MdBye::SessionIdle => {
                        tracing::info!("datahub closed an idle market-data session")
                    }
                    // ⚠ A DISTINCT diagnosis from `SessionIdle`, and the server branch added the
                    // variant precisely because its writer used to send that one here. They are
                    // OPPOSITE findings — idle means this session asked for nothing, this means it
                    // asked for more than it could read — and once the socket is gone the reason is
                    // the only thing anybody has to go on. So it is logged as the slow-consumer
                    // fault it is, at `warn!` beside `TooSlow`, rather than at `info!` beside the
                    // two orderly closes.
                    MdBye::ControlLaneOverflow => tracing::warn!(
                        "datahub ended the market-data stream: this client could not absorb even \
                         the status frames its own keys produced. It will re-dial; if this \
                         repeats, fewer open ladders or a faster link is the fix"
                    ),
                }
                return false;
            }
        }
        true
    }
}

// ── The reader thread ───────────────────────────────────────────────────────────────────────────

/// The reader's OWN state, which lives for exactly one connection and is shared with nothing: the
/// per-key wire sequence, and which keys have had a tape hole DISCLOSED on this connection.
///
/// It is a struct rather than two parameters because both are per-connection and both are read by
/// the same arm — §7.2's backstop, whose diagnosis depends on the second one.
#[derive(Default)]
struct ReaderState {
    last_seq: HashMap<(String, String), u64>,
    disclosed: HashSet<(String, String)>,
}

/// One CONNECTION's life. Owns the stream; reads until a fault, a `Bye`, a non-`Md` response or a
/// commanded stop; then hands the session back a torn-down state and lets the reconciler re-dial.
fn reader_loop(session: Arc<MdSession>, mut stream: TcpStream) {
    let mut st = ReaderState::default();
    let why = loop {
        match read_frame::<_, Response>(&mut stream) {
            Ok(Response::Md(frame)) => {
                if !session.apply_frame(*frame, &mut st) {
                    break "the server said goodbye";
                }
            }
            Ok(other) => {
                // The stream socket carries `Response::Md` and nothing else after `MdSubscribed`
                // (the `market` module's §0 invariant), so anything else is a protocol desync.
                tracing::error!(
                    "datahub market-data stream desync: expected Md, got {other:?} — closing"
                );
                break "protocol desync";
            }
            // ⚠ A read TIMEOUT lands here too, and that is deliberate — see the module doc.
            Err(_) => break "the link dropped",
        }
    };

    let commanded = session.stopping();
    // A COMMANDED stop is not a dropped link: no bracket, no status churn, no re-dial — the
    // `observe_bridge` discipline, verbatim.
    if !commanded {
        session.open_gap_bracket(why);
    }
    *lock(&session.served) = Vec::new();
    *lock(&session.session) = None;
    *lock(&session.stream_handle) = None;
    // Per-CONNECTION health, so the next one starts from "wanted, not yet live" rather than
    // inheriting a mark for a key the new server may serve perfectly.
    lock(&session.gapped).clear();
    if !commanded {
        session.refresh_statuses(&format!("reconnecting ({why})"));
        (session.wake)();
        session.poke();
    }
}

// ── The reconciler thread ───────────────────────────────────────────────────────────────────────

/// The ONE thread that dials. See the module doc for why it is not the reader.
fn reconcile_loop(session: Arc<MdSession>) {
    // Two INDEPENDENT attempt counters over ONE schedule (`feed_lifecycle::retry_backoff`: 1s → 60s
    // then flat forever, no attempt limit). Reusing that function is deliberate: it is already this
    // crate's one answer to "how often does a background thing re-ask something currently saying
    // no", so this whole design introduces ZERO new timing constants.
    let mut dial_attempts: u32 = 0;
    let mut dial_after: Option<Instant> = None;
    let mut ctrl_attempts: u32 = 0;
    let mut ctrl_after: Option<Instant> = None;
    // The keys the last `MdUpdate` tried to add, so a ladder earned on a capped key can be told
    // from one gating a key that has never been asked. See where it is consulted.
    let mut last_add: HashSet<(String, String, MdLane)> = HashSet::new();
    // The address every cached fact in this session DESCRIBES — set by a dial ATTEMPT, not by a
    // success, because `dial` caches the advertisement before it refuses (see [`Caps`]).
    let mut server: Option<String> = None;
    // The key NAME the resolved `session.keys` came from; re-resolved off this thread when the
    // shell moves it. See [`resolve_keys_if_renamed`].
    let mut key_name = lock(&session.key_name).clone();
    // When the live stream opened. ⚠ **A dial that SUCCEEDS does not reset the ladder — a
    // connection that LASTED does.** Without this a server that answers `MdSubscribed` and then
    // immediately writes `Bye(TooSlow)` (or drops) is dialled again at full speed for ever: every
    // attempt "succeeds", so a success-keyed reset never lets the backoff climb. A connection that
    // lived at least one [`RETRY_BASE`] is treated as healthy and zeroes the count; anything
    // shorter is a failed attempt wearing a success's clothes.
    let mut connected_at: Option<Instant> = None;

    loop {
        if session.stopping() {
            return;
        }
        let seen = session.poke_generation();
        let mut addr = lock(&session.addr).clone();
        let live = lock(&session.session).is_some();
        resolve_keys_if_renamed(&session, &mut key_name);

        // ⚠ **A `None` address is "no new answer", NOT a different server** — see
        // [`MdSession::set_addr`]. The observe bridge clears its advertisement on every link blip,
        // and on the documented default box that advertisement is the whole address ladder, so
        // treating `None` as a re-point tore the entire market-data plane down on a blip the bridge
        // exists to absorb. Hold what this session already dialled.
        if addr.is_none() {
            addr = server.clone();
        }

        // A change to a DIFFERENT address IS a different server: tear the stream down and forget
        // every fact that describes the old one.
        if server.is_some() && server != addr {
            if live {
                // ⚠ **SHUT THE SOCKET AND STOP HERE.** `served` and `session` belong to the READER,
                // which owns §7.3 rule 4's bracket and builds it by reading `served` —
                // `open_gap_bracket`'s entire input is `lock(&self.served).clone()`. Clearing it
                // here raced that read away: the reconciler is already running and needs three
                // uncontended mutex ops, while the reader must first be woken by the kernel, so the
                // reconciler won essentially always and the bracket forgot NOTHING — no
                // `BookStore::remove`, no `GapStart`, no `bump_gap`, not even its own `warn!`. The
                // previous server's ladder then stayed painted (flagged STALE after
                // `DOM_STALE_MS`) and nothing in this binary would ever remove it.
                //
                // The reader POKES when it has finished, so the park below normally lasts
                // microseconds; the duration is only the backstop for the one case where
                // `stream_handle` could not be cloned, the shutdown therefore did nothing, and the
                // reader is waiting out its own read deadline instead.
                session.begin_stream_teardown();
                session.park(seen, RETRY_MAX);
                continue;
            }
            *lock(&session.caps) = None;
            lock(&session.refused).clear();
            server = None;
            connected_at = None;
            dial_attempts = 0;
            dial_after = None;
            ctrl_attempts = 0;
            ctrl_after = None;
            last_add.clear();
            continue;
        }

        let Some(addr) = addr else {
            session.set_all_statuses(
                "disconnected — no datahub configured; set `config.datahub_addr`, or connect a \
                 backend that advertises one",
            );
            session.park(seen, RETRY_MAX);
            continue;
        };

        if !live {
            // A stream that just ended: was it up long enough to count as a working link?
            if let Some(at) = connected_at.take() {
                if at.elapsed() >= RETRY_BASE {
                    dial_attempts = 0;
                } else {
                    dial_attempts = dial_attempts.saturating_add(1);
                    dial_after = Some(Instant::now() + retry_backoff(dial_attempts));
                }
            }
            if session.desired_specs().is_empty() {
                // Nothing is wanted: do not open a connection to say so. The first DOM window or
                // chart pokes us. (This is also why the capability answer is only cached once a
                // real subscription has been attempted — see `MdSession::want`'s optimistic arm.)
                session.refresh_statuses("idle");
                session.park(seen, RETRY_MAX);
                continue;
            }
            if let Some(at) = dial_after
                && Instant::now() < at
            {
                session.park(seen, at.saturating_duration_since(Instant::now()));
                continue;
            }
            // ⚠ Set BEFORE the dial, and on EVERY attempt: `dial` caches the advertisement before
            // it can refuse for a capability reason, so a refused attempt leaves cached facts
            // describing this address just as a successful one does. Keyed on a CONNECTION instead,
            // a `Caps { market_data: false }` from a plane-less server could never be cleared —
            // see that type's doc.
            server = Some(addr.clone());
            match dial(&session, &addr) {
                Ok(()) => {
                    // ⚠ NOT `dial_attempts = 0` — see `connected_at`. The ladder is reset by a
                    // connection that LASTED, which is judged when this one ends.
                    dial_after = None;
                    connected_at = Some(Instant::now());
                    // A fresh connection re-asks the whole desired set through `md_subscribe`, so
                    // whatever the control ladder was climbing was a question about a socket that
                    // no longer exists.
                    ctrl_attempts = 0;
                    ctrl_after = None;
                    last_add.clear();
                }
                Err(msg) => {
                    dial_attempts = dial_attempts.saturating_add(1);
                    let wait = retry_backoff(dial_attempts);
                    dial_after = Some(Instant::now() + wait);
                    // ⚠ `error:` is not decoration — see `refresh_statuses`. `msg` routinely CARRIES
                    // the word `live` (`NO_MD_PLANE` names `VIKE_DATAHUB_LIVE` and `live-feeds`,
                    // which is the whole point of that message), and `parse_feed_status` tests the
                    // error rung BEFORE the live one. Without this prefix a datahub with no plane
                    // at all renders as Connected in the Connections tool.
                    session.set_all_statuses(&format!(
                        "datahub {addr} error: {msg} (retrying in {}s)",
                        wait.as_secs()
                    ));
                    (session.wake)();
                }
            }
            continue;
        }

        let (add, remove) = session.diff();
        if add.is_empty() && remove.is_empty() {
            session.park(seen, RETRY_MAX);
            continue;
        }
        // ⚠ **A LADDER EARNED ON A CAPPED KEY MUST NOT GATE A KEY IT HAS NOTHING TO DO WITH.**
        // A cap refusal deliberately leaves its spec in `desired` (`absorb_refusals`' non-permanent
        // arm), so `diff()` stays non-empty for ever against a capped server and `ctrl_attempts`
        // climbs to `RETRY_MAX`. That backoff is right — it is what makes a permanently capped key
        // cost one dial a minute instead of thousands — but it is SESSION-wide, so a user opening a
        // DOM window on a perfectly servable key waited up to a MINUTE with no wire traffic and no
        // explanation. Caps are a designed feature (§5.4 argues them from a desktop with several
        // ladders plus a cockpit), not a pathology, so this is the ordinary case.
        //
        // The test is "does this `add` list name a key the last one did not". A key already asked
        // for and capped does not reset anything, so the hot loop the ladder exists to stop stays
        // stopped; a genuinely NEW key goes out at once.
        let add_keys: HashSet<(String, String, MdLane)> =
            add.iter().map(|s| (s.venue.clone(), s.symbol.clone(), s.lane)).collect();
        if !add_keys.is_subset(&last_add) {
            ctrl_attempts = 0;
            ctrl_after = None;
        }
        if let Some(at) = ctrl_after
            && Instant::now() < at
        {
            session.park(seen, at.saturating_duration_since(Instant::now()));
            continue;
        }
        last_add = add_keys;
        match push_update(&session, &addr, add, remove) {
            Ok(()) => {
                // ⚠ **A SUCCESSFUL EXCHANGE IS NOT A CONVERGED SET**, and resetting the ladder on
                // the transport's verdict rather than the OUTCOME's is a hot loop against the one
                // server that just said no.
                //
                // A CAP refusal deliberately leaves its spec in the desired set — a cap frees up
                // when another window closes, so the key stays wanted and is retried. That means
                // `diff()` answers non-empty again on the very next pass, and with the ladder zeroed
                // there is no delay before the next dial: an `MdUpdate` per loop iteration, for
                // ever, on a fresh connection each time. It is the same failure the `MdSpec::key()`
                // diff avoids, reached from the other side, and it is invisible against a fake that
                // accepts whatever it is sent.
                //
                // So this is the control-lane twin of `connected_at` above: the ladder resets on
                // CONVERGENCE, not on a reply. A set that did not converge advances it, which is
                // what makes a permanently capped key cost one dial a minute instead of thousands.
                let (still_add, still_remove) = session.diff();
                if still_add.is_empty() && still_remove.is_empty() {
                    ctrl_attempts = 0;
                    ctrl_after = None;
                } else {
                    ctrl_attempts = ctrl_attempts.saturating_add(1);
                    ctrl_after = Some(Instant::now() + retry_backoff(ctrl_attempts));
                }
            }
            Err(msg) => {
                ctrl_attempts = ctrl_attempts.saturating_add(1);
                let wait = retry_backoff(ctrl_attempts);
                ctrl_after = Some(Instant::now() + wait);
                tracing::warn!(
                    "datahub market-data: MdUpdate failed ({msg}) — the served set is UNKNOWN, not \
                     empty; retrying in {}s",
                    wait.as_secs()
                );
            }
        }
    }
}

impl MdSession {
    /// Shut the live stream's socket so its reader returns at once, WITHOUT raising the global stop
    /// flag — the reader then takes its ordinary fault path (bracket, clear, poke) and the
    /// reconciler re-dials.
    fn begin_stream_teardown(&self) {
        if let Some(s) = lock(&self.stream_handle).as_ref() {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
    }
}

/// The connection both dial paths open: authenticated when this desktop holds a datahub observe
/// key, plain otherwise. ⚠ There is no third branch — `connect_authed` against a KEY-LESS server
/// is a clean UNAUTHENTICATED connect by that method's own contract, which is exactly what lets one
/// caller hold keys and still work against a local dev server.
///
/// A free function over an OWNED `Option<NodeKeys>` rather than a method, because it runs on
/// [`bounded`]'s helper thread and must borrow nothing from the session.
fn connect_with(addr: &str, keys: Option<&NodeKeys>) -> Result<DatahubClient, String> {
    match keys {
        Some(k) => DatahubClient::connect_authed(addr, k, Scope::Observe),
        None => DatahubClient::connect(addr),
    }
    .map_err(|e| e.to_string())
}

/// Re-resolve the datahub observe key when the shell has moved its NAME — a backend switch, or an
/// edit to the active record.
///
/// ⚠ **It runs on the RECONCILER, and that is the whole reason the name and the key are separate
/// fields.** [`crate::backend_registry::datahub_observe_keys`] opens `<project>/settings/node.env`,
/// and the shell pushes this per frame; resolving at the push site would put a file read on the
/// frame thread. A change does NOT tear the live stream down: that connection is already
/// authenticated and working, and a re-key buys nothing until the next dial — which a re-POINT
/// (the usual companion of a switch) triggers on its own through the address arm.
fn resolve_keys_if_renamed(session: &Arc<MdSession>, current: &mut String) {
    let wanted = lock(&session.key_name).clone();
    if wanted == *current {
        return;
    }
    let (keys, notices) = crate::backend_registry::datahub_observe_keys(
        session.settings_dir.as_deref(),
        &session.env,
        &wanted,
    );
    for n in notices {
        tracing::warn!("datahub market-data: {n}");
    }
    tracing::info!(
        key_name = %wanted,
        configured = keys.is_some(),
        "datahub market-data: the observe key NAME changed — the next dial signs with it"
    );
    *lock(&session.keys) = keys;
    *current = wanted;
}

/// Run one blocking control exchange on a SHORT-LIVED helper thread and give up on it after
/// [`CTRL_DEADLINE`].
///
/// ⚠ **This is the module doc's bounded-control-exchange rule, and it is a THREAD because the
/// socket option is not ours to set**: `DatahubClient` clears its read timeout at the end of every
/// `connect`, deliberately, and exposes no hatch to re-arm it for one verb. Abandoning the helper is
/// safe because it borrows NOTHING from the session — it returns a value, and a value nobody
/// receives is dropped, closing the socket inside it. The cost of a wedged peer is therefore one
/// leaked thread, bounded by how often the dial ladder fires (1 s → 60 s, flat), instead of a
/// reconciler that never returns and a market-data plane that never comes back.
fn bounded<T: Send + 'static>(
    what: &str,
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, String> {
    // ⚠ `sync_channel(1)`, so the helper's `send` NEVER blocks: a rendezvous channel would park the
    // abandoned thread for ever on the very path this function exists to bound.
    let (tx, rx) = std::sync::mpsc::sync_channel::<T>(1);
    std::thread::Builder::new()
        .name("md-ctrl".to_string())
        .spawn(move || {
            let _ = tx.send(f());
        })
        .map_err(|e| format!("the OS refused the market-data control thread: {e}"))?;
    rx.recv_timeout(CTRL_DEADLINE).map_err(|_| {
        format!(
            "{what} did not answer within {}s — treating the link as dead (the datahub client \
             clears its read timeout past the handshake, so this deadline is the only one there is)",
            CTRL_DEADLINE.as_secs()
        )
    })
}

/// What [`dial_blocking`] learned: the advertisement (on BOTH arms — it is read before the refusal
/// and [`MdSession::want`] needs it either way, see [`Caps`]) plus the opened stream or the reason
/// there is none. A named alias so the signature stays under `clippy::type_complexity`.
type Dialed = (Option<Caps>, Result<(MdSubscribedInfo, TcpStream), String>);

/// The BLOCKING half of a dial: connect, learn the advertisement, `md_subscribe`. It touches no
/// session state at all, which is exactly what makes it safe to abandon — see [`bounded`]. Every
/// publication of what it learned happens back on the reconciler, in [`dial`].
fn dial_blocking(addr: String, keys: Option<NodeKeys>, specs: Vec<MdSpec>) -> Dialed {
    let client = match connect_with(&addr, keys.as_ref()) {
        Ok(c) => c,
        Err(e) => return (None, Err(e)),
    };
    let caps = Caps::from_features(client.features());
    if !caps.market_data {
        return (Some(caps), Err(NO_MD_PLANE.to_string()));
    }
    let opened = client.md_subscribe(specs).map_err(|(_client, msg)| msg);
    (Some(caps), opened)
}

/// Open the stream: connect, cache the advertisement, `md_subscribe`, spawn the reader.
///
/// A free function taking the `Arc` because the reader thread needs one and only the reconciler —
/// which was spawned WITH the `Arc` — can hand it over. Re-deriving one from `&self` is not
/// expressible, and a `Weak` back-pointer would be machinery bought for nothing.
///
/// ⚠ The advertisement is cached BEFORE the subscribe, so a server that answers `Response::Error`
/// (one predating the verb, or built with no market-data plane) still teaches [`MdSession::want`]
/// to refuse locally from the next frame on, instead of re-attempting for ever.
fn dial(session: &Arc<MdSession>, addr: &str) -> Result<(), String> {
    let keys = lock(&session.keys).clone();
    let specs = session.desired_specs();
    let a = addr.to_string();
    let (caps, opened) =
        bounded("the datahub market-data subscribe", move || dial_blocking(a, keys, specs))?;
    if let Some(c) = caps {
        *lock(&session.caps) = Some(c);
    }
    let (info, stream) = opened?;

    *lock(&session.session) = Some(info.session);
    *lock(&session.served) = info.accepted.clone();
    // A fresh connection knows nothing about any key's flow yet, and the server re-states each
    // one's `Status` as its first frame.
    lock(&session.gapped).clear();
    session.absorb_refusals(info.refused);
    // §7.3 rule 4's CLOSING half for the tape: every accepted trade key starts from a fresh
    // aggregator, because a reconnect is a hole and the server keeps no replay buffer.
    for spec in &info.accepted {
        if spec.lane == MdLane::Trades {
            session.bump_gap(&spec.venue, &spec.symbol);
        }
    }
    match stream.try_clone() {
        Ok(handle) => *lock(&session.stream_handle) = Some(handle),
        Err(e) => tracing::warn!(
            "datahub market-data: could not clone the stream handle ({e}) — teardown will wait out \
             the read deadline instead of returning at once"
        ),
    }
    session.refresh_statuses(&format!("datahub {addr}"));
    (session.wake)();

    let reader = session.clone();
    match std::thread::Builder::new()
        .name("md-reader".to_string())
        .spawn(move || reader_loop(reader, stream))
    {
        Ok(h) => {
            if let Some(old) = lock(&session.reader).replace(h) {
                // The previous connection's reader has already RETURNED — clearing `session` is
                // what let us dial again — so this join reaps a finished thread rather than waiting
                // on a live one.
                let _ = old.join();
            }
            Ok(())
        }
        Err(e) => {
            *lock(&session.session) = None;
            *lock(&session.served) = Vec::new();
            *lock(&session.stream_handle) = None;
            Err(format!("the OS refused the market-data reader thread: {e}"))
        }
    }
}

/// Change the live session's set on a SHORT-LIVED connection of its own. ⚠ Never on the stream
/// socket: the server's side of that one has left its read loop and a frame written there is never
/// read. There is deliberately no long-lived control connection — a dead one would be a state that
/// leaks topics.
fn push_update(
    session: &Arc<MdSession>,
    addr: &str,
    add: Vec<MdSpec>,
    remove: Vec<MdSpec>,
) -> Result<(), String> {
    let Some(session_id) = *lock(&session.session) else {
        return Err("the stream closed before the update was sent".to_string());
    };
    let keys = lock(&session.keys).clone();
    let a = addr.to_string();
    // Bounded for the reason [`bounded`] states: this read has no deadline of its own either, and a
    // wedge here is the same permanently-dark plane a wedged subscribe would be.
    let info =
        bounded("the datahub market-data update", move || -> Result<MdUpdatedInfo, String> {
            let mut client = connect_with(&a, keys.as_ref())?;
            client.md_update(session_id, add, remove)
        })??;
    {
        let mut served = lock(&session.served);
        served.retain(|s| !info.released.iter().any(|r| r.key() == s.key()));
        for spec in &info.accepted {
            if !served.iter().any(|s| s.key() == spec.key()) {
                served.push(spec.clone());
            }
        }
    }
    for spec in &info.accepted {
        if spec.lane == MdLane::Trades {
            session.bump_gap(&spec.venue, &spec.symbol);
        }
    }
    session.absorb_refusals(info.refused);
    session.refresh_statuses(&format!("datahub {addr}"));
    (session.wake)();
    Ok(())
}
