//! Live Polymarket CLOB market channel → the vike-data live seam (tick-producer T2): the venue's
//! market data as the source of the shared engine `vike_bridge_core::data_client::bus::DataClientBus`
//! ([`DataClientBusSpecPolymarket`]), with the per-subscription thread lifecycle of
//! the shared `DataClientWebsocket` engine binance and okx run on (`DataClientRegistry`, deterministic stop+join teardown) but for
//! a WS that carries FULL L2 books + incremental deltas + last-trade prints instead of klines —
//! `subscribe_book`/`subscribe_quotes`/`subscribe_trades` deliver one token each from a connection
//! to the market channel (`super::config`'s `market_websocket_url`, the venue's `venue_url` row plus
//! `/ws/market`, looked up once by [`DataClientBusSpecPolymarket::new`]); `subscribe_bars` is
//! `Unsupported` (Polymarket serves ticks, not candles — bars are a resample done above this seam).
//! The engine counts the holders of each key (one seat per token and mode, owner ruling B-10) and
//! stamps each quote's and trade's receive time and symbol; the shard threads deliver into its fixed
//! sink, so every emission below reaches the client's sink through it.
//!
//! **WS batching (K tokens per socket).** The CLOB market channel's subscribe frame takes a LIST
//! (`{"assets_ids":[…],"type":"market"}` — [`crate::feed::decode::subscribe_message`]) and every inbound frame
//! names its own `asset_id`, so one socket can carry many tokens. A **shard** is one socket + one
//! feed thread + up to [`DEFAULT_TOKENS_PER_SOCKET`] tokens of ONE [`FeedLane`]; `subscribe_*` packs
//! its token into the first shard of that mode with a free seat (opening a new shard otherwise), so
//! N tokens × M modes cost `ceil(N/K) × M` sockets instead of `N × M`. K is per source
//! ([`DataClientBusSpecPolymarket::with_tokens_per_socket`], default [`DEFAULT_TOKENS_PER_SOCKET`], set per deployment by
//! `venue.polymarket.ws_tokens_per_socket` ([`tokens_per_socket`], which the root reads)).
//! **K = 1 is exactly the pre-batching shape** — one socket, one thread, one
//! token, the same thread name, the same status strings, the same emissions.
//!
//! - **Routing is per token, off the frame.** Every decoded [`MarketUpdate`] carries the `asset_id`
//!   it belongs to, and the pump routes each one to that token's own [`TokenSlot`] (its `TokenState`
//!   book/`seq`/`last_top` AND its own `StreamHealth`) — never to ambient socket-level state. The
//!   pre-batching pump already read the id (it guarded `asset_id == token_id` defensively against a
//!   stray cross-token push); batching turns that guard into the router. An `asset_id` no slot on
//!   this socket owns is still ignored.
//! - **Per-token state, never per socket.** The feed-LOCAL monotonic `seq` (see Book maintenance
//!   below) lives in `TokenState`, so token A's frames can never bump token B's `seq`; likewise each
//!   token discloses its OWN `stream_status` (`GapStart`/`Live`/`Stale`) under its own id.
//! - **Membership is picked up at a session boundary.** A shard's live token set is shared with its
//!   feed thread behind an epoch counter; adding/removing a token bumps the epoch, the shard's
//!   stream wrapper ends the current session on its next read poll, and the driver's
//!   reconnect-==-resubscribe path re-dials and subscribes the WHOLE new set (the subscribe frame is
//!   built PER SESSION, in the connect closure, from the live membership). A membership change is
//!   therefore disclosed exactly like any other reconnect: one `GapStart` per co-tenant token, then
//!   `Live` on the first frame back. Emptying a shard stops+joins its thread.
//!
//! **Book maintenance.** Polymarket market frames carry no per-event sequence number, so this
//! pump keeps its own feed-local monotonic `seq` (`TokenState::seq`), incremented once per applied
//! `book`/`price_change` frame and fed to [`vike_marketdata::L2Book::apply_snapshot`]/`apply_delta` —
//! `apply_snapshot` doesn't check staleness (always accepts) and `apply_delta` only rejects
//! `seq <= last_sequence`, so a feed-local counter that only ever increases satisfies both call sites
//! without needing the venue's own numbering. `apply_snapshot`/`apply_delta` take RAW (price, qty)
//! pairs and quantize internally via the book's own `tick_size` (verified in
//! `crates/vike-marketdata/src/orderbook.rs`); a qty-0 delta level is a removal (also the book's own behavior,
//! not reimplemented here). Tick size is resolved once per subscription, on the feed's own thread
//! (mirroring binance's REST warmup, which also runs on the lane's own thread so `subscribe_*` itself never
//! blocks on the network) via [`crate::instrument::properties::fetch_token_tick_size`].
//!
//! **What each pump mode emits (capabilities-not-obligations, D7 — `subscribe_book` documents its
//! exact behavior per `vike-data`'s `DataClient::subscribe_book` doc):**
//! - [`FeedLane::Book`]: applies `book`/`price_change` frames to the local book, pushes
//!   `sink.book()` on every applied frame, AND derives an L1 top from it — `sink.quote()` fires
//!   only when the `(bid, ask, bid_size, ask_size)` tuple actually changed (both sides present).
//!   Book mode ALSO emits the RECORDABLE `sink.book_update()` stream (the delta-preserving twin of
//!   `sink.book()`): one `Snapshot` per applied `book` frame and one `Delta` per applied
//!   `price_change` frame (RAW wire levels, `seq` = the freshly bumped feed-local seq), PLUS a
//!   synthetic full-book `Snapshot` anchor (its own bumped seq, levels folded from the local book)
//!   every [`ANCHOR_EVERY_DELTAS`] applied deltas so a replay never seeks unboundedly to re-anchor.
//!   Each frame's `book_update` is emitted BEFORE that frame's derived `quote` (recorded order
//!   matches the replay tie-break Book < Quote); every emitted event's `seq` increments by exactly
//!   1, so the synthetic anchor's seq bump keeps the emitted chain contiguous even though the live
//!   book's `last_sequence` then lags it (see the `PriceChange` arm's load-bearing comment).
//!   `last_trade` frames on this subscription are ignored (no `sink.trade()` call).
//! - [`FeedLane::Quotes`]: identical book bookkeeping and the same derived-quote emission rule as
//!   `Book`, but never calls `sink.book()` — quote-only on the wire.
//! - [`FeedLane::Trades`]: ignores `book`/`price_change` frames entirely (no book state needed);
//!   every `last_trade` frame becomes one `sink.trade()` call. `is_buyer_maker` is derived from the
//!   decoded `taker_is_buy`: a taker BUY means the resting (maker) side was a SELL, so
//!   `is_buyer_maker = !taker_is_buy.unwrap_or(true)` — an absent side (older frames) is treated as
//!   a taker buy, the conservative default (mirrors OKX's taker-side convention in
//!   `okx::market_data::decode_trade`).
//!
//! **Timestamps.** The decoder (`feed/decode.rs::frame_ts`) reads each frame's top-level `timestamp` field
//! (string or number JSON form) into `MarketUpdate`'s `ts: Option<i64>` when present. `TickTrade.ts`
//! uses a `last_trade` frame's own `ts` directly; the derived `TickQuote.ts` (emitted from
//! `after_book_change`) uses the `ts` of whichever `book`/`price_change` frame produced it. Either
//! falls back to `now_ms()` (local wall clock, receive time) when the frame carries no `timestamp`
//! — documented once here rather than at each call site. **Confirmed against a live capture
//! (2026-07-08)**: all three market-channel frame types carry a top-level `timestamp` as a string
//! of epoch-ms (mirroring the user-channel decoder's `s("timestamp").parse::<i64>()` in
//! `crates/bridges/polymarket/src/execution/user_data/decode.rs`'s `decode_trade`). A frame that omits the field only
//! ever degrades to today's receive-time stamping (`frame_ts` returns `None`), never produces an
//! incorrect-but-plausible timestamp.
//!
//! **Reconnect (dedup A6, wave 3 — the shared driver).** The WS session/reconnect LIFECYCLE rides
//! [`vike_bridge_core::websocket_session::market`]'s driver: connect (a 10 s BOUNDED TCP dial via the driver's
//! `connect_market_stream` — a black-holed route can't pin the feed thread past the stop flag),
//! subscribe replayed verbatim each session, the 10 s `"PING"` keepalive, the 30 s silent-stall
//! idle watchdog, §B freshness judged on the driver's transport-alive `on_tick` hook, and a
//! stop-aware EXPONENTIAL reconnect backoff (500 ms doubling capped at 30 s, reset on a successful
//! connect) — all five knobs declared in this venue's `WebsocketSessionSpec` row
//! ([`vike_bridge_core::websocket_session::spec::row`]), which [`feed_main`] consumes. Polymarket
//! is the venue the driver's three LAST knobs (bounded dial, exponential backoff, `on_tick`) were
//! added for. Only the PROTOCOL stays here: the subscribe/keepalive payloads, the frame decode +
//! book fold, the §B disclosure wiring, and the raw-frame tap. On a session fault the status
//! updates and the gap opens (`enter_gap` → `GapStart`); `last_top` resets to `None` (the gap
//! means whatever quote was last emitted is no longer trustworthy to compare against) but the book
//! itself is NOT manually cleared — Polymarket's `book` frame is always a wholesale replace
//! (`apply_snapshot` clears+rebuilds both sides), so the next snapshot after reconnect naturally
//! supersedes any stale state; the feed-local `seq` counter is never reset either, so
//! post-reconnect deltas are guaranteed to be accepted by `apply_delta`. The same closures (and so
//! the same `TokenState`/`StreamHealth`) are reused across reconnects by construction. Two
//! driver-unification deltas vs the pre-driver copy, both strict tightenings: a failed keepalive
//! SEND now ends the session (reconnect) instead of being ignored — a dead socket surfaces one
//! read earlier — and the connect-vs-session error statuses share one
//! `"ws error (reconnecting)"` wording.

pub(crate) mod session;
mod shard;
pub(crate) mod state;
mod update;

pub(crate) mod decode;
pub(crate) mod depth;
pub mod flow;
pub mod rtds;
pub mod tap;

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use vike_bridge_core::WebsocketSessionMarketKeepalive;
use vike_bridge_core::data_client::bus::{
    DataClientBusSink, DataClientBusSpec, DataClientBusSpecDeliver, DataClientBusSpecHold,
    DataClientBusSpecKey,
};
use vike_bridge_core::data_client::context::DataClientContext;
use vike_bridge_core::transport::UreqTransport;
use vike_bridge_core::websocket_session::market::{
    WebsocketSessionMarketOptions, WebsocketSessionStatus, run_market_on, run_market_session,
};
use vike_bridge_core::websocket_session::spec;
use vike_bridge_core::websocket_session::user::WebsocketSessionUserError;
use vike_data::{DataClientError, DataClientRegistry, DataClientSink, DataClientSubscriptionId};
use vike_venues::venue_urls::{UrlRefusal, VenueUrls};

use crate::feed::decode::subscribe_message;

use crate::feed::tap::{
    RawCaptureConfig, RawTap, RawTapHandle, RawTapOwner, WebsocketSessionMarketStreamTapped,
};

use session::FeedTiming;
use shard::{ShardMembership, WebsocketSessionMarketStreamShard, reconcile_slots, shard_label};
use state::{FeedLane, TokenSlot};
use update::{on_alive_tick, on_frame};

// The stream seam is the SHARED driver's now (dedup A6) — `vike_bridge_core::websocket_session::market::WebsocketSessionMarketStream`,
// named at that path by `raw_tap`'s `WebsocketSessionMarketStreamTapped` and the scripted tests, not re-exported here
// (a third consumer, vike-backfill's
// `poly_reparse`, implemented `WebsocketSessionMarketStream` over captured gz frames the same way until
// docs/decisions/0094 deleted it on 2026-09-28): `read_frame` returns exactly one TEXT
// frame per call (Ping auto-ponged, control frames swallowed, every inbound frame stamping the
// idle-watchdog liveness clock), `WebsocketSessionUserError::Timeout` is the stop-flag poll tick, `Closed`
// carries the reconnect reason.
//
// ⚠ The ERROR shape is named canonically, `vike_bridge_core::websocket_session::user::WebsocketSessionUserError`. It used to
// be re-exported here as `WebsocketSessionUserError` — a pure abbreviation that gave one type a second name, which
// the 2026-09-18 alias ruling retired; the seam itself is unchanged.
use vike_bridge_core::websocket_session::market::WebsocketSessionMarketStream;

pub(crate) const VENUE: &str = "polymarket";
/// The CLOB app-level keepalive payload (its 10 s cadence is a `WebsocketSessionSpec`-row knob). The
/// server answers with a non-JSON text `"PONG"` this pump skips as data but counts as liveness.
const PING_PAYLOAD: &str = "PING";
/// Net-hardening §B DATA-freshness threshold — the freshness sibling of the 30 s idle-watchdog
/// transport threshold (this venue's `WebsocketSessionSpec` row: no inbound frame of ANY kind — a market
/// update, the CLOB text `PONG`, OR a swallowed WS-control keepalive — for 30 s means the tick
/// stream is silently dead → gap + reconnect; sized well above the 10 s ping cadence so a
/// healthy-but-quiet market never false-trips. THIS is the feed the user actually trades on), and
/// the failure that watchdog CANNOT see. The idle watchdog trips on TRANSPORT silence (no inbound
/// frame of any kind); this trips on DATA staleness behind a still-live socket: the newest DATA timestamp
/// we've applied lags wall-clock by more than this while keepalives keep flowing. That is exactly
/// the silently-failed re-subscribe — the socket answers `PONG`s but no book/trade frame ever
/// arrives, so the transport watchdog reads `Live` forever. v1 discloses only (`StreamStatus::Stale`,
/// no auto-action), so a rare false positive is purely informational, never harmful.
///
/// Data-freshness thresholds, per subscription mode (net-hardening §B). The `StreamHealth` is
/// per-pump so each mode picks its own. **Tuned from live measurement (2026-07-11, via the Dublin
/// EU route since Polymarket is US-geo-blocked): 24 markets sampled for 20 min.** Book/`price_change`
/// cadence spans 4+ orders of magnitude across markets — very active markets update sub-second, but
/// even MODERATELY active ones (a tick trader's targets) had max inter-update gaps of 60–229s, and
/// genuinely quiet markets went 8–25 MINUTES between updates. The original 60s guess false-tripped
/// on most non-hot markets, so Book/Quotes is raised to 300s: it clears the measured active- and
/// moderate-market worst case, and only a near-dead book (updating < once/5min) trips — where the
/// disclosure is accurate. Trades are sparser still: across all 24 markets in 20 min only 2 prints
/// arrived, so NO fixed cutoff distinguishes a quiet market from a dead feed; 1800s (30 min) is a
/// coarse "no print in half an hour on a market you chose to trade" backstop that stops the old
/// 600s value from false-tripping on essentially every market. Both are DISCLOSE-ONLY
/// (`StreamStatus::Stale`, no auto-action), so erring loose costs only slower disclosure, never a
/// wrong trade; the fast dead-socket signal is the 30 s idle watchdog (transport). A per-market
/// adaptive threshold (k × observed cadence) is the real fix for the huge activity range — deferred;
/// this tune replaces the guesses with measured floors.
const FRESHNESS_THRESHOLD_BOOK: Duration = Duration::from_secs(300);
// 30 min — Polymarket trades are extremely sparse (measured ~0 prints in 20 min across 24 markets);
// a truly-dead trades subscription is still eventually caught, but this lane's freshness is coarse.
const FRESHNESS_THRESHOLD_TRADES: Duration = Duration::from_secs(1800);

/// How many tokens one market socket carries by default (the "K" of WS batching, module doc).
///
/// Polymarket publishes NO per-connection subscription cap. It has now been **measured live** from
/// the Dublin host (2026-07-22): one socket subscribed to **500 distinct token ids** delivered data
/// for **all 500** over a 25 s window (3,362 frames, zero foreign `asset_id`s, no error and no
/// close); 200 tokens behaved identically. So the server limit is ≥ 500 — the previous conservative
/// default of 8 was ~62× below it.
///
/// This default is NOT the measured ceiling, because K trades socket count against blast radius:
/// one socket's fault gaps ALL of its seated tokens for a reconnect + resubscribe (~500 ms backoff),
/// and a membership change costs co-tenants one `GapStart`→`Live` pair. 50 takes most of the
/// available win (500 tokens ⇒ 10 sockets instead of 63 at K=8, a further 6.25× cut) while keeping a
/// single fault's reach bounded and staying an order of magnitude inside the proven limit. Push it
/// higher with [`DataClientBusSpecPolymarket::with_tokens_per_socket`] or `venue.polymarket.ws_tokens_per_socket` when
/// socket count matters more than gap granularity.
pub const DEFAULT_TOKENS_PER_SOCKET: usize = 50;

/// **K from `venue.polymarket.ws_tokens_per_socket`**, as the composition root read it from the
/// settings database (decision 0095: the feeds read no environment for it). `get` answers a field name;
/// a missing, unparseable or zero value keeps [`DEFAULT_TOKENS_PER_SOCKET`]. The caller hands the
/// answer to [`DataClientBusSpecPolymarket::with_tokens_per_socket`].
#[must_use]
pub fn tokens_per_socket(get: impl Fn(&str) -> Option<String>) -> usize {
    get("ws_tokens_per_socket")
        .and_then(|v| super::config::first_token(&v).parse::<usize>().ok())
        .filter(|k| *k >= 1)
        .unwrap_or(DEFAULT_TOKENS_PER_SOCKET)
}

use vike_model::now_ms;

/// The BATCHED twin of [`run_session`] and the one real session body: drive ONE WS session that
/// carries `slots`' whole token set. The subscribe frame is built from the slots in order
/// (`{"assets_ids":[…]}`), each inbound frame is routed to its own slot, and the §B disclosures are
/// per token — see the module doc's WS-batching section. With one slot this is exactly the
/// pre-batching session.
pub fn run_shard_session<S: WebsocketSessionMarketStream>(
    stream: &mut S,
    sink: &dyn DataClientSink,
    mode: FeedLane,
    slots: &mut [TokenSlot],
    stop: &AtomicBool,
    timing: &FeedTiming<'_>,
    wake: impl Fn(),
) -> Result<(), String> {
    let tokens: Vec<String> = slots.iter().map(|s| s.token_id.clone()).collect();
    let sub = subscribe_message(&tokens);
    let knobs = spec::row(VENUE).knobs();
    let opts = WebsocketSessionMarketOptions {
        subscribe: Some(&sub),
        // The idle/keepalive knobs come from the TIMING (production `FeedTiming::new` reads the
        // venue row; scripted tests inject their own via `with_timing`) — never from the row
        // directly, so the test seam keeps its timing injection.
        keepalive: Some(WebsocketSessionMarketKeepalive {
            payload: PING_PAYLOAD,
            every: timing.ping_every,
        }),
        ack_timeout: None,
        idle_threshold: Some(timing.idle_threshold),
        // The session never dials or backs off (its caller owns connecting), so the remaining
        // knobs are inert here; filled from the venue row for coherence.
        read_timeout: knobs.read_timeout,
        backoff: knobs.backoff,
        connect_timeout: knobs.connect_timeout,
    };
    // Both driver closures need the same slots mutably; they run on one thread inside one driver
    // loop, so a RefCell shim is safe and borrow-scoped per callback.
    let slots = RefCell::new(slots);
    let label = timing.stream_label;
    run_market_session(
        stream,
        &opts,
        stop,
        &now_ms,
        &mut |txt| on_frame(txt, mode, &mut slots.borrow_mut()[..], sink, label, &wake),
        &mut || on_alive_tick(&mut slots.borrow_mut()[..], sink, label),
        // No status handle on this seam — it is the scripted/replay entry point, driven by tests
        // and by `shard_main`'s own connect closure, which writes the per-session string itself.
        &mut || {},
    )
}

/// What a shard thread is handed beyond the shared [`DataClientContext`] (its `own`).
struct ShardInputs {
    /// Best-effort raw-frame capture handle (raw-first tap). `None` = capture off (passthrough).
    raw_tap: Option<Arc<RawTapHandle>>,
    /// Opt-in PIT `SymbolProperties` recorder (`flags.record_properties`). `None` — the default,
    /// and every caller that does not call [`DataClientBusSpecPolymarket::with_properties_recorder`] — means
    /// [`reconcile_slots`] does exactly what it did before this field existed: resolve the tick
    /// size and nothing else, with NO extra network call. See that function for the contract.
    properties_rec: Option<Arc<vike_data::PropertiesRecorder>>,
    /// The addresses this shard dials, the [`DataClientBusSpecPolymarket`]'s own: the market channel, the CLOB REST base
    /// (the tick-size warmup and the taker-hold read) and the Gamma base (the taker-hold lookup).
    market_websocket: String,
    clob: String,
    gamma: String,
}

/// Wall-clock ceiling on ONE REST request of the per-token warmup a feed thread makes — the
/// tick-size lookup [`reconcile_slots`] performs for every newly-seated token, plus the two
/// taker-hold requests that join it when a properties recorder is armed (no root arms one:
/// `flags.record_properties` arms only the datahub's tape).
///
/// **Why this venue needed its own number.** Those requests ran on `crate::transport::agent()`, whose
/// [`crate::transport::REST_TIMEOUT`] is 30 s — the ORDER path's budget, three times the bounded dial two
/// lines further down, on a thread whose stop budget is 12 s in total
/// (`crates/vike-datahub/src/recorder.rs`'s `FEED_STOP_BUDGET_SECS`). And polymarket is
/// the venue actually recording on the CI box, so this was not a hypothetical lane: the derivation that
/// budget rests on covered the other venue.
///
/// **Ten seconds, and deliberately the SAME window as the dial**
/// (`vike_bridge_core::websocket_session::spec`'s `CONNECT_10S`) and as binance's two REST rungs
/// (`crates/bridges/binance/src/family/feed/tick/trades.rs`'s `WARMUP_TIMEOUT`,
/// `crates/bridges/binance/src/family/feed/tick/mod.rs`'s `DEPTH_SEED_TIMEOUT`). That is what keeps the
/// feed-stop budget a single largest-window number rather than a growing sum. It is also generous
/// against the measured route: this venue is reached through a SOCKS5 tunnel to Dublin, so the
/// per-request cost is a proxied round trip rather than a direct one, and ten seconds is the same
/// allowance the dial through that same tunnel already gets.
///
/// ⚠ **A per-request ceiling is only half the bound, and on this venue the smaller half.** One
/// token's resolution is up to `1 + TICK_SIZE_LOOKUP_MAX_PAGES` (51) requests, and a shard reseats
/// many tokens at once, so the ceiling that matters is the CONTINUE PREDICATE threaded through
/// [`reconcile_slots`] — see its doc. This constant bounds the one request already in flight when
/// the flag goes up; the predicate is what stops the next fifty.
///
/// **What a timeout costs:** the lookup resolves no tick, exactly what a REST failure already
/// produced, sooner: the token is seated waiting for its tick (`shard.rs`'s `reconcile_slots`), its frames
/// refused until its own `book` snapshot states one. Never a guessed tick (owner, 2026-11-13).
pub(crate) const FEED_WARMUP_TIMEOUT: Duration = Duration::from_secs(10);

/// The agent every feed-thread REST warmup request rides — a named rung so the bound is OBSERVABLE
/// (`agent.config().timeouts().global`) rather than merely written down, the shape
/// `crates/bridges/binance/src/family/feed/tick/trades.rs`'s `warmup_agent` and
/// `crates/bridges/binance/src/family/feed/tick/mod.rs`'s `depth_seed_agent` already have.
///
/// Built through `crate::transport::agent_with_timeout` rather than by hand, so it inherits the SOCKS
/// proxy arm every Polymarket lane needs — a second hand-rolled `ureq::Agent` here would dial direct
/// from a geo-blocked box and resolve no tick size at all. See
/// `the_feed_warmup_runs_on_a_bounded_agent_not_the_order_paths`.
pub(crate) fn feed_warmup_agent() -> ureq::Agent {
    crate::transport::agent_with_timeout(FEED_WARMUP_TIMEOUT)
}

/// The thread body `DataClientBusSpecPolymarket::seat` spawns for ONE SHARD (one socket, up to K tokens of one mode):
/// resolves each seated token's tick size (REST — off the subscribe's path, same discipline as
/// binance's REST warmup), then hands the whole connect → subscribe → session → backoff → reconnect
/// LIFECYCLE to the shared driver ([`run_market_on`]), with this venue's `WebsocketSessionSpec` row
/// supplying every knob (10 s bounded dial, 10 s `"PING"`, 30 s idle watchdog, exponential 500 ms →
/// 30 s backoff reset on a successful connect).
///
/// The per-token state lives OUTSIDE the driver loop as a [`TokenSlot`] each, so it survives
/// reconnects (and a neighbour's arrival/departure) by construction: `TokenState` (the book fold —
/// Polymarket's `book` frame is a wholesale replace, so the next snapshot naturally supersedes any
/// stale state; the feed-local `seq` never resets, so post-reconnect deltas are always accepted) and
/// that token's own unified `StreamHealth` (net-hardening §B): its transport half so a whole
/// TRANSPORT outage is one `Gap`/`Live` pair per token; its freshness half so an open DATA-staleness
/// episode survives a reconnect and is closed by the re-seed's `observe_data` rather than orphaned.
/// `reset_freshness` is deliberately NEVER called here — Polymarket PERSISTS freshness across
/// reconnects (this is `StreamHealth`'s default, no-arm-at-start behavior), unlike the crypto depth
/// driver, which resets freshness at each new session.
///
/// The closures delegate to the SAME [`on_frame`]/[`on_alive_tick`] protocol bodies
/// [`run_shard_session`]/[`run_session`] (the test/replay seams) drive, so production and the
/// scripted seam cannot drift. On any session/connect fault the error arm updates the status,
/// invalidates every seated token's `last_top` (stale after the gap — module doc), and opens each
/// one's §B gap (`enter_gap` → `GapStart`); the driver then walks the stop-aware backoff and redials.
///
/// **The one deviation from the pre-batching body:** the subscribe frame is sent by the CONNECT
/// closure rather than handed to the driver as `WebsocketSessionMarketOptions::subscribe`, because a shard's token
/// set can change between sessions and the driver's `subscribe` is fixed for the whole feed. The
/// contract is unchanged — every session still subscribes verbatim before its first read — it is
/// just built per session, from the membership snapshot that session dialled with.
fn shard_main(
    mode: FeedLane,
    membership: Arc<ShardMembership>,
    ctx: DataClientContext<ShardInputs>,
) {
    // PROXIED transport: the tick-size REST warmup ([`fetch_token_tick_size_while`]) must ride the
    // SAME SOCKS egress every other Polymarket lane uses. A plain `UreqTransport::new` dials direct
    // and, on a geo-blocked box, fails to resolve the host — logging `fetch_token_tick_size:
    // /markets request failed` and leaving every token without its tick — while the WS feed
    // correctly tunnels via [`crate::transport::websocket_proxy`].
    //
    // BOUNDED, not `crate::transport::agent()`: see [`FEED_WARMUP_TIMEOUT`]. Same proxy arm, shorter
    // ceiling, because this agent's calls are made from a feed thread with a stop budget.
    let transport = UreqTransport::with_agent(VENUE, feed_warmup_agent());
    let stream_label = mode.as_str();
    let freshness_ms = mode.freshness_threshold().as_millis() as i64;
    let knobs = spec::row(VENUE).knobs();
    // SOCKS5 egress (spec §0.1): the SAME tunnel the REST lane uses (`transport::proxy_url`) whenever
    // `transport::websocket_proxy` answers one — by default whenever the REST lane proxies, with
    // `venue.polymarket.ws_proxy_enabled` as the per-lane override; `None` is a direct dial.
    // Resolved ONCE per shard thread and reused across reconnects.
    let websocket_proxy = crate::transport::websocket_proxy();
    // The opt-in PIT properties recorder, cloned out of `ctx` so the reconnect closure below (which
    // borrows `ctx` for its sink/status) can hand it to `reconcile_slots`. `None` by default.
    let properties_rec = ctx.own.properties_rec.clone();
    // The warmup's continue predicate: THIS shard's stop flag, read fresh before every REST request
    // `reconcile_slots` makes. `Arc` clone rather than a borrow of `ctx` because the connect closure
    // below already borrows `ctx` for its sink/status.
    let warmup_stop = Arc::clone(&ctx.stop);
    let keep_warming = move || !warmup_stop.load(Ordering::Relaxed);
    // RefCell shims: the connect / on_text / on_tick / on_session_status closures all touch the
    // shared per-shard state on the one feed thread, borrow-scoped per callback.
    let slots: RefCell<Vec<TokenSlot>> = RefCell::new(Vec::new());
    let session = RefCell::new(Vec::<String>::new()); // the membership THIS session subscribed
    let label = RefCell::new(String::new());
    {
        // Seat the initial membership before the first dial, so the tick-size REST calls and the
        // first status line land in exactly the pre-batching order. No stop check is needed AFTER
        // this one: the driver's loop head is `while !stop.load(..)`, so the very next thing that
        // happens is the check.
        let tokens = membership.tokens();
        reconcile_slots(
            &mut slots.borrow_mut(),
            &tokens,
            &transport,
            &ctx.own.clob,
            &ctx.own.gamma,
            freshness_ms,
            properties_rec.as_ref(),
            &keep_warming,
        );
        *label.borrow_mut() = shard_label(&tokens);
        ctx.set_status(format!("connecting to Polymarket market WS ({})…", label.borrow()));
        *session.borrow_mut() = tokens;
    }
    // Every knob from the venue row EXCEPT `subscribe`, which the connect closure owns (see the
    // deviation note above); `knobs.opts()` is bypassed only because it debug-asserts a subscribe
    // payload against the row's `subscribe_frame: true` declaration, which this venue still honors
    // — one socket, one verbatim subscribe frame per session.
    let opts = WebsocketSessionMarketOptions {
        subscribe: None,
        keepalive: Some(WebsocketSessionMarketKeepalive {
            payload: PING_PAYLOAD,
            every: knobs.keepalive_every.expect("polymarket row declares keepalive"),
        }),
        ack_timeout: knobs.ack_timeout,
        idle_threshold: knobs.idle_threshold,
        read_timeout: knobs.read_timeout,
        backoff: knobs.backoff,
        connect_timeout: knobs.connect_timeout,
    };
    run_market_on(
        || {
            // Snapshot the membership this session will serve BEFORE dialling. Read the EPOCH
            // FIRST, then the tokens: the two reads are not one atomic step, and this order is the
            // safe one — a change landing between them yields a session subscribed to the NEW set
            // while guarding the OLD epoch, so `WebsocketSessionMarketStreamShard` immediately ends it and redials (one
            // wasted dial). The reverse order would leave a session serving a STALE token set while
            // believing it current, which no later signal would correct.
            let subscribed_at = membership.epoch();
            let tokens = membership.tokens();
            if tokens != *session.borrow() {
                reconcile_slots(
                    &mut slots.borrow_mut(),
                    &tokens,
                    &transport,
                    &ctx.own.clob,
                    &ctx.own.gamma,
                    freshness_ms,
                    properties_rec.as_ref(),
                    &keep_warming,
                );
                *label.borrow_mut() = shard_label(&tokens);
                *session.borrow_mut() = tokens.clone();
            }
            // The warmup above can spend its whole ceiling, and a shutdown raises the flag
            // before it joins anything — so by the time we get here the answer may already be
            // "stop". Dialling now would put a SECOND full window (`CONNECT_10S`) behind the first,
            // and the recorder's budget is derived as the LARGEST window plus a trailing read, not
            // as a sum. This is the polymarket twin of the stop check
            // `crates/vike-bridge-core/src/depth.rs`'s `run_depth_session` makes between its dial
            // and its REST book seed. The driver treats the error as a connect fault: status line,
            // then the stop-aware backoff, whose first act is to observe the same flag and return.
            if !keep_warming() {
                return Err("stop requested during the token warmup".to_string());
            }
            // The driver's bounded dial (`connect_timeout` from the row), wrapped in the raw-frame
            // tap: tees each raw frame to the RawTap when capture is on, branchless passthrough
            // when off; the driver is generic over WebsocketSessionMarketStream and unaware of the wrap.
            let stream = vike_bridge_core::websocket_session::market::connect_market_stream_via(
                &ctx.own.market_websocket,
                knobs.read_timeout,
                knobs.connect_timeout,
                websocket_proxy.as_ref(),
            )?;
            let mut tapped = WebsocketSessionMarketStreamTapped::new(
                stream,
                ctx.own.raw_tap.clone(),
                label.borrow().clone(),
            );
            tapped.send_text(&subscribe_message(&tokens)).map_err(|e| match e {
                WebsocketSessionUserError::Closed(m) => m,
                WebsocketSessionUserError::Timeout => "read timeout".to_string(),
            })?;
            ctx.set_status(format!("LIVE · Polymarket {}", label.borrow()));
            Ok(WebsocketSessionMarketStreamShard {
                inner: tapped,
                membership: Arc::clone(&membership),
                subscribed_at,
            })
        },
        &opts,
        &ctx.stop,
        &now_ms,
        |txt| {
            on_frame(
                txt,
                mode,
                &mut slots.borrow_mut()[..],
                ctx.sink.as_ref(),
                stream_label,
                &|| (ctx.wake)(),
            )
        },
        || on_alive_tick(&mut slots.borrow_mut()[..], ctx.sink.as_ref(), stream_label),
        |s| match s {
            // ⚠ **The ONE place in the tree where `{}` is the right answer, and it is not laziness.**
            // This venue's healthy write is KEPT in the connect closure above, and it must be: it
            // is already PER-SESSION (so it was never the latch this PR cures), it carries a label
            // the driver cannot reconstruct (`shard_label` over this session's token set), and —
            // decisively — this venue's `pump_spec` row declares `ack_timeout: None` because quiet
            // Polymarket books legitimately send NO frame for 8–25 minutes (measured 2026-07-11).
            // A Confirm-gated write alone would therefore leave a perfectly healthy quiet shard
            // reading `Error` across normal operation, which is this defect inverted.
            WebsocketSessionStatus::Live => {}
            WebsocketSessionStatus::Error(e) => {
                ctx.set_status(format!(
                    "polymarket {} ws error (reconnecting): {e}",
                    label.borrow()
                ));
                let now = now_ms();
                for slot in slots.borrow_mut().iter_mut() {
                    slot.state.last_top = None; // stale after the gap — module doc
                    if let Some(event) = slot.health.enter_gap(now) {
                        ctx.sink.stream_status(VENUE, &slot.token_id, stream_label, event);
                    }
                }
            }
        },
    );
}

/// One socket: a feed thread (owned by the shared [`DataClientRegistry`] — dedup A6, the registry half:
/// one stop flag + one `JoinHandle`) serving up to K tokens of ONE [`FeedLane`].
struct Shard {
    /// The source's own identity of the shard, so a seat can name it across `Vec` reshuffles.
    id: u64,
    mode: FeedLane,
    membership: Arc<ShardMembership>,
    /// the registry key owning this shard's thread + stop flag (never handed to a caller).
    feed: DataClientSubscriptionId,
}

/// The open shards, behind the source's one lock.
struct ShardSet {
    registry: DataClientRegistry,
    shards: Vec<Shard>,
    next_shard_id: u64,
}

/// The shard thread body, as a plain `fn` so a shard can be spawned from more than one call site
/// (`shard_main` in production, a network-free stand-in in tests).
type ShardBody = fn(FeedLane, Arc<ShardMembership>, DataClientContext<ShardInputs>);

/// **The venue's live feed set as the shared engine's source** (`vike_bridge_core::data_client::bus::DataClientBus`
/// over this): **shards** (one socket each, up to K tokens of one mode — module doc's WS-batching section) held in
/// the shared [`DataClientRegistry`]. The engine counts the holders of each key and mints the ids, so a key (a token in
/// one mode: `subscribe_book`, `subscribe_quotes` or `subscribe_trades`) takes ONE seat however many hold it
/// (owner ruling B-10), and its last holder's release frees it — stopping+joining the shard's thread only once its
/// LAST seat goes, otherwise resubscribing the remainder; a shutdown stops+joins every shard; nothing is ever
/// detached. At K = 1 every shard holds exactly one token and this is the pre-batching one-socket-per-subscription
/// lifecycle `bybit`/`okx`/`hyperliquid` still run.
///
/// The shard threads deliver into the engine's fixed sink (`vike_bridge_core::data_client::bus::DataClientBusSink`),
/// so the engine stamps every quote's and trade's receive time and symbol and hands each book, raw book update and
/// per-token status on as the pump made it. The client is
/// `DataClientBus::on(&Arc::new(DataClientBusSpecPolymarket::new(wake, urls)?), sink)`.
pub struct DataClientBusSpecPolymarket {
    pub status: Arc<Mutex<String>>,
    wake: Arc<dyn Fn() + Send + Sync>,
    /// The sink every shard thread delivers into; the engine's consumer attaches to it.
    relay: Arc<DataClientBusSink>,
    shards: Mutex<ShardSet>,
    /// Raw-frame capture: the owner (joined when the client closes) + the hot handle cloned into each shard's `ShardInputs`.
    /// `None` unless constructed via `with_raw_capture`.
    raw_tap_owner: Mutex<Option<RawTapOwner>>,
    raw_tap: Option<Arc<RawTapHandle>>,
    /// Opt-in PIT `SymbolProperties` recorder — see [`DataClientBusSpecPolymarket::with_properties_recorder`].
    /// `None` (the default) means no properties are recorded and the taker-hold lookups are never made.
    properties_rec: Option<Arc<vike_data::PropertiesRecorder>>,
    /// K — seats per socket ([`DEFAULT_TOKENS_PER_SOCKET`] / [`tokens_per_socket`] /
    /// [`DataClientBusSpecPolymarket::with_tokens_per_socket`]).
    tokens_per_socket: usize,
    /// The addresses every shard dials, looked up ONCE from the venue's `venue_url` rows by
    /// [`DataClientBusSpecPolymarket::new`] (`crate::config`'s `market_websocket_url`, `clob_base` and `gamma_base`).
    market_websocket: String,
    clob: String,
    gamma: String,
    /// [`shard_main`] in production; a network-free stand-in in the lifecycle tests.
    body: ShardBody,
}

impl DataClientBusSpecPolymarket {
    /// `wake` is the GUI repaint nudge fired on status changes and after each processed frame; pass `|| {}` for a
    /// headless caller. The registry pins each thread to the venue's HFT affinity role
    /// (`vike_bridge_core::feed_ctx::pinned_feed_registry`).
    ///
    /// `urls` are the venue's address rows (`VenueSettings::urls`): the market channel, the CLOB and
    /// Gamma, all at type `live`. A missing row is the refusal, and nothing is built.
    ///
    /// # Errors
    /// A row the feed dials is missing ([`UrlRefusal`]).
    pub fn new(
        wake: impl Fn() + Send + Sync + 'static,
        urls: &VenueUrls,
    ) -> Result<Self, UrlRefusal> {
        Ok(DataClientBusSpecPolymarket {
            status: Arc::new(Mutex::new("connecting to Polymarket…".into())),
            wake: Arc::new(wake),
            relay: Arc::default(),
            shards: Mutex::new(ShardSet {
                registry: vike_bridge_core::feed_ctx::pinned_feed_registry("polymarket"),
                shards: Vec::new(),
                next_shard_id: 0,
            }),
            raw_tap_owner: Mutex::new(None),
            raw_tap: None,
            properties_rec: None,
            tokens_per_socket: DEFAULT_TOKENS_PER_SOCKET,
            market_websocket: super::config::market_websocket_url(urls)?,
            clob: super::config::clob_base(urls)?.to_string(),
            gamma: super::config::gamma_base(urls)?.to_string(),
            body: shard_main,
        })
    }

    /// Like [`DataClientBusSpecPolymarket::new`] but with raw-frame capture ON: spawns a [`RawTap`] writer whose
    /// handle is cloned into every shard's `ShardInputs`, so each feed thread's `WebsocketSessionMarketStreamTapped` tees frames to it.
    /// The owner is held here and joined when the client closes, after every feed thread.
    ///
    /// Note the interaction with batching: the tap keys its gz files by the STREAM it wrapped, so a
    /// multi-token shard writes one `{first}+{n}` series carrying every seated token's frames
    /// interleaved. `poly_reparse` used to replay a single token out of it this way (its pump
    /// ignored any `asset_id` it did not ask for, it just read more bytes) until docs/decisions/0094
    /// deleted it on 2026-09-28. Set K = 1 for one file per token.
    pub fn with_raw_capture(
        wake: impl Fn() + Send + Sync + 'static,
        urls: &VenueUrls,
        config: RawCaptureConfig,
    ) -> std::io::Result<Self> {
        // The rows first: a missing one spawns no writer.
        let mut source = Self::new(wake, urls).map_err(std::io::Error::other)?;
        let (handle, owner) = RawTap::spawn(config)?;
        source.raw_tap_owner = Mutex::new(Some(owner));
        source.raw_tap = Some(handle);
        Ok(source)
    }

    /// Arm the opt-in PIT `SymbolProperties` recorder (the venue-adapter convention, best-effort).
    /// No root calls this today: `flags.record_properties` (on by default) arms only the datahub's
    /// own tape, so every live feed runs un-armed.
    ///
    /// This is the ONE switch that makes a subscribed token's grid — tick size AND the venue's
    /// declared [`crate::instrument::taker_hold`] — land on the `kind=properties` tape, which is where a
    /// backtest reads it back from (`HistoryStore::properties_as_of` → `StrategyEngineConfig::properties` →
    /// the latency gate's per-symbol hold table). Un-armed, the feed makes exactly the REST calls
    /// it made before this existed. See [`reconcile_slots`] for the per-token cost.
    pub fn with_properties_recorder(
        mut self,
        rec: Option<Arc<vike_data::PropertiesRecorder>>,
    ) -> Self {
        self.properties_rec = rec;
        self
    }

    /// Set K — how many tokens one socket carries (module doc). `1` restores the pre-batching
    /// one-socket-per-subscription shape exactly; `0` is clamped to `1`. Overrides
    /// [`DEFAULT_TOKENS_PER_SOCKET`].
    pub fn with_tokens_per_socket(mut self, tokens_per_socket: usize) -> Self {
        self.tokens_per_socket = tokens_per_socket.max(1);
        self
    }

    /// The lifecycle tests' network-free shard body in place of [`shard_main`].
    #[cfg(test)]
    fn with_body(mut self, body: ShardBody) -> Self {
        self.body = body;
        self
    }

    /// K — seats per socket, for a caller that wants to log/assert the effective batching.
    pub fn tokens_per_socket(&self) -> usize {
        self.tokens_per_socket
    }

    /// How many sockets (shards) are open right now — `ceil(N/K) × modes` in the steady state, and
    /// the number this whole change exists to cut.
    pub fn socket_count(&self) -> usize {
        self.shard_set().shards.len()
    }

    /// How many feed threads the registry holds (one per shard), for the lifecycle tests.
    #[cfg(test)]
    fn feed_count(&self) -> usize {
        self.shard_set().registry.len()
    }

    fn shard_set(&self) -> std::sync::MutexGuard<'_, ShardSet> {
        self.shards.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Seat `token_id` on a shard of `mode` and answer that shard's id: the first shard of that mode with a free seat
    /// that does not already carry this token takes it (bumping its epoch, so its live session resubscribes the whole
    /// set), otherwise a new shard/socket is opened.
    fn seat(&self, token_id: &str, mode: FeedLane) -> std::io::Result<u64> {
        let cap = self.tokens_per_socket.max(1);
        let mut set = self.shard_set();
        // `try_admit` only mutates the shard it returns `true` for, so a side-effecting `find` is
        // safe here: every earlier shard was full (or already carries this token) and untouched.
        let joined =
            set.shards.iter().find(|s| s.mode == mode && s.membership.try_admit(token_id, cap));
        match joined.map(|s| s.id) {
            Some(id) => Ok(id),
            None => self.spawn_shard(&mut set, token_id, mode),
        }
    }

    /// Open one new socket seeded with `token_id`, one [`DataClientRegistry::spawn`] call (dedup A6): the
    /// registry allocates its key + stop flag and owns the join handle (the MarketData affinity pin
    /// rides its spawn hook, see [`DataClientBusSpecPolymarket::new`]); this source only assembles its own
    /// [`DataClientContext`] — its [`ShardInputs`], the raw-tap handle included, as `own` — around the registry-issued
    /// stop flag. The thread is named after the token that OPENED it (stable for the thread's life,
    /// and identical to the pre-batching name at K = 1).
    fn spawn_shard(
        &self,
        set: &mut ShardSet,
        token_id: &str,
        mode: FeedLane,
    ) -> std::io::Result<u64> {
        let (sink, status, wake, raw_tap, properties_rec) = (
            Arc::clone(&self.relay) as Arc<dyn DataClientSink>,
            Arc::clone(&self.status),
            Arc::clone(&self.wake),
            self.raw_tap.clone(),
            self.properties_rec.clone(),
        );
        let (market_websocket, clob, gamma) =
            (self.market_websocket.clone(), self.clob.clone(), self.gamma.clone());
        let membership = Arc::new(ShardMembership::new(token_id.to_string()));
        let thread_membership = Arc::clone(&membership);
        let body = self.body;
        let feed = set.registry.spawn(format!("feed-poly-{token_id}-{mode:?}"), move |stop| {
            let ctx = DataClientContext {
                venue: VENUE,
                sink,
                status,
                wake,
                stop,
                own: ShardInputs { raw_tap, properties_rec, market_websocket, clob, gamma },
            };
            body(mode, thread_membership, ctx)
        })?;
        let id = set.next_shard_id;
        set.next_shard_id += 1;
        set.shards.push(Shard { id, mode, membership, feed });
        Ok(id)
    }

    /// Release `token_id`'s seat on shard `shard_id`; every other seat keeps running. The socket is stopped+JOINed
    /// only when that was its LAST seat — otherwise the shard resubscribes its remaining tokens (a co-tenant sees a
    /// normal reconnect: one `GapStart`, then `Live` on the first frame back).
    fn release(&self, shard_id: u64, token_id: &str) {
        let mut set = self.shard_set();
        let Some(pos) = set.shards.iter().position(|s| s.id == shard_id) else { return };
        if set.shards[pos].membership.release(token_id) {
            let shard = set.shards.remove(pos);
            set.registry.stop_join(shard.feed);
        }
    }
}

impl DataClientBusSpec for DataClientBusSpecPolymarket {
    type Hold = DataClientBusSpecHoldPolymarket;
    const VENUE: &'static str = VENUE;
    const CLOSED: &'static str = "polymarket: the market feed is closed";
    const BARS: Result<(), &'static str> = Err("polymarket serves ticks; bars via resample");
    const TRADES: Result<(), &'static str> = Ok(());
    const BOOK: Result<(), &'static str> = Ok(());
    /// Polymarket publishes no per-connection cap; K packs the tokens instead (module doc).
    const MAX_SYMBOLS: Option<(usize, &'static str)> = None;
    const PRICE_CHECK: bool = false;
    /// A token id is the venue's verbatim string.
    const SYMBOL_VERBATIM: bool = true;
    /// A shutdown stops every shard at once in the hold's drop (raise every flag, then join), not seat by seat.
    const UNWANT_AT_SHUTDOWN: bool = false;

    fn consume(
        self: &Arc<Self>,
        deliver: DataClientBusSpecDeliver,
    ) -> DataClientBusSpecHoldPolymarket {
        self.relay.attach(deliver);
        DataClientBusSpecHoldPolymarket { source: Arc::clone(self), seats: Mutex::default() }
    }
}

/// The engine's hold on [`DataClientBusSpecPolymarket`]: the shard each held key's seat is on.
pub struct DataClientBusSpecHoldPolymarket {
    source: Arc<DataClientBusSpecPolymarket>,
    seats: Mutex<HashMap<DataClientBusSpecKey, u64>>,
}

impl DataClientBusSpecHoldPolymarket {
    fn seats(&self) -> std::sync::MutexGuard<'_, HashMap<DataClientBusSpecKey, u64>> {
        self.seats.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// The token and the pump mode a key names (`BARS` refuses a bars key before it gets here).
fn seat_of(key: &DataClientBusSpecKey) -> (&str, FeedLane) {
    match key {
        DataClientBusSpecKey::Quotes(token) => (token, FeedLane::Quotes),
        DataClientBusSpecKey::Trades(token) => (token, FeedLane::Trades),
        DataClientBusSpecKey::Book(token) | DataClientBusSpecKey::Bars { symbol: token, .. } => {
            (token, FeedLane::Book)
        }
    }
}

impl DataClientBusSpecHold for DataClientBusSpecHoldPolymarket {
    /// A key's first holder takes a seat; a later holder of the same key shares it (owner ruling B-10). A thread the
    /// OS refused is the refusal, in the venue's words.
    fn ask(&self, key: &DataClientBusSpecKey, _streaming: bool) -> Result<(), DataClientError> {
        let mut seats = self.seats();
        if seats.contains_key(key) {
            return Ok(());
        }
        let (token, mode) = seat_of(key);
        let shard = self.source.seat(token, mode).map_err(|e| {
            DataClientError::Subscribe(format!("polymarket {token} ({mode:?}): {e}"))
        })?;
        seats.insert(key.clone(), shard);
        Ok(())
    }

    fn unwant(&self, key: &DataClientBusSpecKey) {
        let Some(shard) = self.seats().remove(key) else { return };
        self.source.release(shard, seat_of(key).0);
    }

    /// Raise every shard's stop flag and JOIN NOTHING — phase one of a teardown that spans several
    /// clients (`vike_data::DataClient::begin_shutdown`). A shard notices on its next read
    /// timeout, and a shard caught mid-dial notices only when this venue's `connect_timeout`
    /// expires, so starting every shard's clock at once is what keeps a whole recorder profile's
    /// stop at about one wind-down instead of one per socket.
    fn begin_shutdown(&self) {
        self.source.shard_set().registry.raise_stops();
    }
}

impl Drop for DataClientBusSpecHoldPolymarket {
    /// Deterministic teardown: raise every stop flag, then JOIN every feed thread, then the raw-tap writer, after
    /// every feed thread that fed it has stopped; nothing reaches the closed client after it.
    fn drop(&mut self) {
        {
            let mut set = self.source.shard_set();
            set.registry.shutdown();
            set.shards.clear();
        }
        if let Some(owner) = self
            .source
            .raw_tap_owner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            owner.shutdown();
        }
        self.source.relay.detach();
    }
}

#[path = "feed_tests.rs"]
#[cfg(test)]
mod feed_tests;

#[path = "warmup_bound_tests.rs"]
#[cfg(test)]
mod warmup_bound_tests;
