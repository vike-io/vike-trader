//! `MdSession` — the desktop's ONE market-data connection to a `vike-datahub`, and the whole of
//! the client half of the market-data wire designed in
//! `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` (§9 items 1 and 2).
//!
//! # What this restores
//!
//! The `fat` deletion took the desktop's local market-data plane and left everything downstream of
//! it — [`BookStore`](crate::data::data_sink::BookStore), [`TradeStore`](crate::data::data_sink::TradeStore),
//! [`GuiFeedSink`](crate::data::data_sink::GuiFeedSink), [`TickVolAgg`](vike_orderflow::tickvol::TickVolAgg),
//! [`OrderflowAgg`](vike_orderflow::bar_agg::OrderflowAgg) and the whole of
//! [`feed_lifecycle`](crate::ui::feed_lifecycle) — compiled, CI-tested and **fed by nothing**.
//! `GuiFeedSink`, the struct built precisely to be the core-free market-data seam, had ZERO
//! production constructors: its only instantiation in the workspace was inside its own
//! `#[cfg(test)]` module. This module is that constructor, and the Trade window's ladder, the
//! trade tape and every chart on a symbol the daemon is not trading come back through it — sourced
//! from the
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
//! | frame (egui) | 1 | the [`DatahubFeed`](crate::data::datahub_feed::DatahubFeed)s | nothing — a short `Mutex` and a `Condvar::notify_all` |
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
//! [`observe_bridge`](crate::backend::observe_bridge) polls a cell rather than blocking on a socket.
//!
//! # ⚠ THE CONTROL EXCHANGE IS BOUNDED HERE, tighter than the client crate's default
//!
//! The datahub client bounds each reply by an idle timeout keyed on the verb
//! (`crates/vike-datahub-client/src/proto/verbs.rs`'s `reply_wait`); `md_subscribe` and
//! `md_update` are server-bounded verbs and get its 300 s default. This section predates that bound
//! and keeps its own, because 300 s of a wedged reconciler is still a desktop reading
//! "reconnecting" for five minutes. (Before that bound the reply read inside each had NO deadline,
//! and what follows was a wedge for the life of the process.) Both run on the `md-reconcile`
//! thread, on a `DatahubClient` no other thread holds a handle to: [`MdSession::begin_stop`] and
//! `begin_stream_teardown` can only shut the PUSH socket down, and a `poke()` cannot wake a thread
//! parked in `read`. So a peer that vanishes without a FIN or an RST — laptop suspend, a VPN or
//! tunnel dropping, a NAT eviction, exactly the case [`MD_READ_TIMEOUT`] exists to cover on the
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
//! `crates/vike-datahub/src/md/hub.rs`'s `push_trade` sets `tick.symbol = String::new()` before the
//! tape (§12.4's measured finding: a 78-character polymarket token id makes a real tape entry
//! ~142 B rather than 64, which alone breaks §5.5's memory budget). **Every `TradeTick` on this
//! wire therefore carries an EMPTY symbol**, and the envelope carries it once instead.
//!
//! [`GuiFeedSink::trade`](crate::data::data_sink::GuiFeedSink) deliberately ignores its `symbol` argument
//! and keys [`TradeStore`](crate::data::data_sink::TradeStore) by `trade.symbol`. Handing wire ticks
//! straight through would push EVERY print on EVERY venue under the key `(venue, "")`, and
//! [`core_sync::sync_from_core`](crate::ui::core_sync::sync_from_core)'s drain — which drains under
//! each aggregator's own `(venue, symbol)` — would find nothing. Result: tick/volume charts and
//! every orderflow overlay silently empty, for ever, with no error, no log and no failing test.
//! Nothing in the design mentions it, and "everything else on the receiving end is untouched" is
//! exactly what makes it invisible. So [`MdSession::apply_frame`] re-stamps each tick from the
//! envelope, and `crates/vike-app-core/tests/md_session_scripted/main.rs` carries the kill proof.
//!
//! # Staleness is a receipt stamp taken HERE (§7.4)
//!
//! Every book frame lands through `sink.l2_snapshot(.., vike_model::now_ms())` — the LOCAL clock.
//! `BookStore::update` stores that as the receipt and the shell computes
//! `now_ms - ts > TRADE_STALE_MS`; two boxes' clocks differ, so a wire timestamp would make every ladder read permanently stale or
//! falsely live across an SSH tunnel. `BookSnapshot::venue_ts`/`venue_seq` ride for diagnostics.
//!
//! # Refusals split by SYNCHRONY, and the split is load-bearing for `FeedRetries`
//!
//! A refusal that returns SYNCHRONOUSLY from `subscribe_*` reaches
//! [`FeedRetries::note_error`](crate::ui::feed_lifecycle::FeedRetries::note_error), which records
//! `LiveDataError::Unsupported` as `RetryState::Refused` and never retries it. That covers the
//! static capability matrix, the cached `md_venue=` advertisement, and a permanent refusal this
//! session has already SEEN. ⚠ **An asynchronous permanent refusal — one that arrives on the wire
//! after `subscribe_depth` already answered `Ok` — cannot reach `FeedRetries` at all**: `ensure_depth`
//! has already recorded its `trade_depth` entry and returns early for ever. Its disclosure is the
//! per-venue status string plus one `tracing::warn!`, and nothing else. That is accepted, and it is
//! written here because a reader will otherwise assume `FeedRetries` classifies every refusal.
//! A CAP refusal is not permanent: the spec stays in the desired set and the reconciler retries it.

use std::collections::{HashMap, HashSet};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use vike_data::{LiveDataError, LiveDataSink, SubscriptionId};
use vike_datahub_client::{
    DatahubClient, MdBye, MdSubscribedInfo, MdUpdatedInfo, advertised_md_venues,
    market::{MD_READ_TIMEOUT, MdFrame, MdLane, MdRefusal, MdSessionId, MdSpec},
    proto::{Response, read_frame},
};
use vike_node_proto::auth::{NodeKeys, Scope};

use crate::backend::split_plane::LOCAL_FEED_VENUES;
use crate::data::data_sink::{BookStore, DirectBarStore, GuiFeedSink, TradeStore};
use crate::lock;
use crate::ui::feed_lifecycle::{FeedMap, RETRY_BASE, RETRY_MAX, retry_backoff};
use wire::reconcile_loop;

mod session;
mod wire;

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
    /// One [`DatahubFeed`](crate::data::datahub_feed::DatahubFeed) per venue slug, over the shared
    /// session. The keys are [`LOCAL_FEED_VENUES`], which is `&'static str` — the type `FeedMap`
    /// demands and the same routing keys [`feed_lifecycle`](crate::ui::feed_lifecycle) resolves.
    pub feeds: FeedMap,
    /// One status line per venue, the handles the Connections tool reads per frame. Structurally
    /// FIXED at build time: the session rewrites the strings and never adds or removes a key.
    pub feed_statuses: HashMap<String, Arc<Mutex<String>>>,
}

/// The server's advertisement, cached off the first successful handshake.
///
/// ⚠ **Never cleared on a link drop, and that diverges from
/// [`BridgeHandle::advertised_datahub`](crate::backend::observe_bridge::BridgeHandle)'s clear-on-drop
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
/// Refcounted rather than a bare set because two callers can legitimately want one key — a Trade
/// window's depth leg and a chart's trade leg are different lanes, but two Trade windows on one
/// symbol are the same key — and an `unsubscribe` from one of them must not silently stop the
/// other's stream.
struct Wanted {
    spec: MdSpec,
    holders: u32,
}

/// The desktop's ONE market-data session. Shared as an `Arc` by every
/// [`DatahubFeed`](crate::data::datahub_feed::DatahubFeed) and by both background threads; every field
/// sits behind its own lock, so this type is `Send + Sync` and `DatahubFeed` is `Send` (which
/// [`FeedMap`]'s `Box<dyn DataClient + Send>` requires).
///
/// ⚠ **Lock order, stated once so it cannot be got wrong: `desired` → `served` → `refused` →
/// `gapped` → `caps` → `gap_epochs`.** No lock is ever held across a socket call, a sink call or a
/// `wake()`. (`link` is outside the order because it is never held ALONGSIDE another: every touch
/// is a statement-scoped read or write.)
pub struct MdSession {
    /// The datahub address, LATE-BOUND. ⚠ It is not known at `App::new`:
    /// [`datahub_resolve::resolve_datahub_addr`](crate::data::datahub_resolve::resolve_datahub_addr)'s
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
    /// answer in `served`. An operator staring at an empty ladder being told by the Connections tool
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
    /// because the NAME is a per-backend setting ([`crate::backend::backend_registry::BackendRecord`]'s
    /// `datahub_observe_key`) and the shell switches backends at runtime — see
    /// [`MdSession::set_key_name`].
    keys: Mutex<Option<NodeKeys>>,
    /// The key NAME the shell wants, pushed once per frame beside the address. The reconciler
    /// notices a change and re-resolves OFF the frame thread, because resolving opens a file.
    key_name: Mutex<String>,
    /// The composition root's ONE boot walk and ONE `std::env::vars()` sweep, retained so the
    /// reconciler can re-run [`crate::backend::backend_registry::datahub_observe_keys`] when the name moves.
    /// This crate still reads no environment of its own: both arrive as parameters to [`build`].
    settings_dir: Option<String>,
    env: HashMap<String, String>,
    /// The `Condvar`'s companion: a GENERATION counter, not a boolean. A poke bumps it; the
    /// reconciler samples it before it reads any state and parks only while it is unchanged — so a
    /// `want` landing between the reconciler's check and its `wait` cannot be lost into a 60 s park.
    poke_gen: Mutex<u64>,
    cv: Condvar,
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
/// `crates/vike-datahub-client/src/client/market_data.rs`'s own `md_subscribe` refusal carries: the plane
/// compiles on a default build and `--features live-feeds` alone gates NO code, so an unarmed
/// `flags.datahub_live` row is the cause in every case where the operator has not also chosen venues.
/// Leading with the rebuild sends them to recompile a server that would have worked.
const NO_MD_PLANE: &str = "this datahub serves no market-data plane — run `vike-cli config set flags.datahub_live \
     true` on the server and restart it. A rebuild is a separate question and only afterwards: `--features live-feeds` alone gates no code, \
     and the per-venue `live-<venue>` features are what link a venue's feed";
/// A key this session has already been permanently refused for.
const SERVER_REFUSED: &str =
    "this datahub permanently refused that key (the per-venue status line carries its own words)";
/// The declared, deliberate bar-lane refusal — see [`crate::data::datahub_feed`].
pub(crate) const NO_BAR_LANE: &str = "the datahub market-data wire serves no live bar lane: a chart's HISTORY comes from \
     Request::LoadBars/Backfill and its live tail from the backend snapshot";
/// The declared, deliberate quote-lane refusal — see [`crate::data::datahub_feed`].
pub(crate) const NO_QUOTE_LANE: &str = "the datahub market-data wire serves no quotes lane: GuiFeedSink::quote is a spelled no-op, so \
     a quotes lane would deliver into nothing";

/// How long [`bounded`] waits for a control exchange — `connect` + `md_subscribe`, or `connect` +
/// `md_update` — before abandoning it and letting the dial ladder climb.
///
/// ⚠ **ADOPTED from [`MD_READ_TIMEOUT`], not invented, and this whole design still introduces no
/// new timing constant.** That number is this wire's own "a peer that has said nothing for three
/// heartbeats is not slow, it is gone", and these two verbs are a hub lookup and an answer — they
/// COMPUTE nothing, which is why the client's `reply_wait` classes them server-bounded while it
/// leaves the verbs that do unbounded. Anything shorter would abort a live exchange on a loaded box; anything longer
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
/// [`split_plane::direct_bars_mount`](crate::backend::split_plane::direct_bars_mount) says so for
/// `ObserveOnly`, and [`core_sync::sync_from_core`](crate::ui::core_sync::sync_from_core) takes
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
        crate::backend::backend_registry::datahub_observe_keys(settings_dir, env, key_name);
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
                Box::new(crate::data::datahub_feed::DatahubFeed::new(venue, session.clone()));
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
