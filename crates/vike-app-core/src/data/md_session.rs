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
//! # Which venues get a feed is the SERVER's word, learned by a PROBE
//!
//! No list in this crate says which venues the desktop mounts a feed for. The datahub states them at
//! the handshake (`md_venue=<slug>` in `Welcome.features`, read into [`Caps`]) and
//! [`MdSession::sync_mount`] mounts one [`DatahubFeed`](crate::data::datahub_feed::DatahubFeed) and
//! one status line per advertised slug — and nothing for a venue the connected server does not
//! advertise. [`build`] therefore hands the shell EMPTY maps.
//!
//! ⚠ **That only works because the reconciler asks.** `Caps` is learned from a dial, and the
//! reconciler dials once something is WANTED — but nothing is wanted until a feed exists to want it
//! through, so gating the feeds on `Caps` alone would deadlock. When the reconciler has an address and
//! no `Caps`, it therefore opens a handshake-only connection (the PROBE, `wire.rs`'s `probe`):
//! it reads `Welcome.features`, sends no `md_subscribe`, closes, and wakes the frame thread so
//! `sync_mount` can mount.
//!
//! * **`Caps == None` HOLDS what is mounted, it does not clear it.** `None` means "unknown" — startup,
//!   or an address change that has not been answered yet — and clearing on it would flap every idle
//!   feed across each backend switch, with every window's `ensure_*` logging a missing venue. The set
//!   converges the moment the new server's advertisement lands.
//! * **A feed with a live holder is never removed.** `App`'s ledgers (`subs`, `trade_depth`,
//!   `poly_subs`) route their `unsubscribe` through `App::feeds`, so a venue the new server dropped
//!   keeps its feed until its last key is released (its `want`s are refused locally in the meantime).
//! * **A venue is mounted only if the roster knows it** ([`vike_model::venues::roster_id`], the
//!   interner that keeps `FeedMap` keyed on `&'static str`): a slug outside the roster has no
//!   capability row, so the static matrix would refuse every verb on it anyway.
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
use vike_hub_proto::auth::{HubKeys, Scope};

use crate::data::data_sink::{BookStore, DirectBarStore, GuiFeedSink, TradeStore};
use crate::lock;
use crate::ui::feed_lifecycle::{FeedMap, RETRY_BASE, RETRY_MAX, retry_backoff};
use wire::reconcile_loop;

mod session;
mod wire;

/// What [`build`] hands the shell: the `App::feeds` map and the `App::feed_statuses` map, both
/// EMPTY — [`MdSession::sync_mount`] fills them once the datahub has advertised its venues. Returned
/// as a struct rather than a tuple so the shell's binding is one line and names what it binds —
/// `crates/vike-desktop` is RATCHETED (`crates/vike-ops/tests/
/// ci_excluded_gui_shell_ratchet.rs`) and every line of shape spent there is a line not spent on
/// something that had to be there.
pub struct MdMount {
    /// The session itself — held by the shell so it can push the late-bound address
    /// ([`MdSession::set_addr`]) and the late-bound observe-key name ([`MdSession::set_key_name`]),
    /// mount the advertised venues ([`MdSession::sync_mount`]), and read a key's tape-gap epoch
    /// ([`MdSession::tape_gap_epoch`]) inside the fold. Teardown needs this handle too: with no feed
    /// mounted (nothing advertised yet) there is no `DataClient::shutdown` to stop the session.
    pub session: Arc<MdSession>,
    /// One [`DatahubFeed`](crate::data::datahub_feed::DatahubFeed) per venue the connected datahub
    /// advertises, over the shared session; EMPTY until the first advertisement lands. The keys are
    /// roster ids, which are `&'static str` — the type `FeedMap` demands and the same routing keys
    /// [`feed_lifecycle`](crate::ui::feed_lifecycle) resolves.
    pub feeds: FeedMap,
    /// One status line per mounted venue, the handles the Connections tool reads per frame; EMPTY
    /// until [`MdSession::sync_mount`] mounts a feed. The session rewrites the strings, and
    /// `sync_mount` adds and removes keys beside the feed it mounts or drops.
    pub feed_statuses: HashMap<String, Arc<Mutex<String>>>,
}

/// The server's advertisement, cached off the latest successful handshake — a PROBE (nothing is
/// wanted yet) or a dial — and the one source [`MdSession::sync_mount`] mounts the feeds from.
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

    /// Why this advertisement leaves NOTHING to mount, or `None` when it names at least one roster
    /// venue (the ones [`MdSession::sync_mount`] mounts). The no-plane sentence is the one a refused
    /// dial already carries, so the two cannot name different fixes.
    fn nothing_to_mount(&self) -> Option<&'static str> {
        if !self.market_data {
            Some(NO_MD_PLANE)
        } else if self.venues.iter().all(|v| vike_model::venues::roster_id(v).is_none()) {
            Some(NO_MD_VENUES)
        } else {
            None
        }
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
/// `gapped` → `caps` → `gap_epochs` → `mounted`.** No lock is ever held across a socket call, a sink
/// call or a `wake()`. (`link` is outside the order because it is never held ALONGSIDE another: every
/// touch is a statement-scoped read or write. `mounted` is last because `refresh_statuses` takes it
/// while it still holds the first four; the one place that MUTATES it,
/// [`MdSession::sync_mount`], holds nothing else at the time.)
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
    /// The status line of every MOUNTED venue — the same `Arc`s the shell holds in
    /// `App::feed_statuses`, added and removed by [`MdSession::sync_mount`] beside the feed. The
    /// session owns this copy so the reconciler and the reader can rewrite the lines without
    /// reaching into the shell's map.
    mounted: Mutex<HashMap<&'static str, Arc<Mutex<String>>>>,
    /// The LINK half of every status line — `datahub <addr>` / `reconnecting (…)` / `idle` — kept
    /// so a re-render triggered by a health change ([`MdSession::restate_statuses`]) does not have
    /// to invent one, and cannot therefore contradict the line the reconciler last wrote. It is also
    /// the ONLY place the link's state is written down when no venue is mounted at all (a server
    /// that advertises nothing has no line to carry it): [`MdSession::link_status`] reads it.
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
    keys: Mutex<Option<HubKeys>>,
    /// The key NAME the shell wants, pushed once per frame beside the address. The reconciler
    /// notices a change and re-resolves OFF the frame thread, because resolving opens a file.
    key_name: Mutex<String>,
    /// The composition root's ONE boot walk, retained so the reconciler can re-run
    /// [`crate::backend::backend_registry::datahub_observe_keys`] when the name moves.
    /// This crate still reads no environment of its own: it arrives as a parameter to [`build`].
    settings_dir: Option<String>,
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
/// A server whose market-data plane is up but advertises no venue this desktop's roster knows:
/// there is nothing to mount a feed for. Carried by the link line ([`MdSession::link_status`]) —
/// no venue is mounted, so no per-venue line exists to say it.
const NO_MD_VENUES: &str = "this datahub serves a market-data plane but advertises no venue this desktop knows — it was built \
     with no `live-<venue>` feed (rebuild it with one), or it runs a newer venue roster than this desktop";
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

/// Build the session, its reconciler thread, and the (empty) feed/status maps the shell installs.
/// The maps fill through [`MdSession::sync_mount`] once the datahub's advertisement is known.
///
/// `settings_dir` is the binary's ONE boot walk (`vike_boot::Booted`), taken as a PARAMETER — this
/// crate reads no environment, so nothing here joins `LIBRARY_PIN`. `key_name` is the datahub
/// observe-key NAME (`backend_registry::datahub_observe_key_name`).
///
/// ⚠ **The [`GuiFeedSink`] is constructed HERE, not in the shell**, and its `bars` store is PRIVATE
/// to it: this wire serves no bar lane (design §10), so nothing will ever write that store.
/// `App::direct_bars` is a DIFFERENT store (the shell builds it and
/// `crate::data::store_bars`' reads fill it), and
/// [`core_sync::sync_from_core`](crate::ui::core_sync::sync_from_core) takes its render-source
/// mode signal from `bar_plane`, never from whether a store handle exists.
pub fn build(
    settings_dir: Option<&str>,
    key_name: &str,
    books: Arc<BookStore>,
    trades: Arc<TradeStore>,
    wake: impl Fn() + Send + Sync + 'static,
) -> MdMount {
    let (keys, notices) =
        crate::backend::backend_registry::datahub_observe_keys(settings_dir, key_name);
    for n in notices {
        tracing::warn!("datahub market-data: {n}");
    }

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
        mounted: Mutex::new(HashMap::new()),
        link: Mutex::new("disconnected".to_string()),
        next_id: AtomicU64::new(1),
        stop: AtomicBool::new(false),
        wake: Box::new(wake),
        // ⚠ The private, never-written bar store — see this function's doc.
        sink: GuiFeedSink { books, trades, bars: Arc::new(DirectBarStore::default()) },
        keys: Mutex::new(keys),
        key_name: Mutex::new(key_name.to_string()),
        settings_dir: settings_dir.map(str::to_string),
        poke_gen: Mutex::new(0),
        cv: Condvar::new(),
    });

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

    MdMount { session, feeds: FeedMap::new(), feed_statuses: HashMap::new() }
}
