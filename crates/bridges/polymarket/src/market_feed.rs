//! Live Polymarket CLOB market channel → the vike-data live seam (tick-producer T2): the venue's
//! `DataClient` implementation, mirroring `binance`/`okx`'s `market_feed::Feeds` per-subscription
//! thread lifecycle (`HashMap<SubscriptionId, (stop flag, JoinHandle)>`, deterministic
//! stop+join teardown) but for a WS that carries FULL L2 books + incremental deltas + last-trade
//! prints instead of klines — `subscribe_book`/`subscribe_quotes`/`subscribe_trades` deliver one
//! token each from a connection to [`super::config::WS_MARKET`]; `subscribe_bars` is `Unsupported`
//! (Polymarket serves ticks, not candles — bars are a resample done above this seam).
//!
//! **WS batching (K tokens per socket).** The CLOB market channel's subscribe frame takes a LIST
//! (`{"assets_ids":[…],"type":"market"}` — [`super::ws::subscribe_message`]) and every inbound frame
//! names its own `asset_id`, so one socket can carry many tokens. A **shard** is one socket + one
//! feed thread + up to [`DEFAULT_TOKENS_PER_SOCKET`] tokens of ONE [`PumpMode`]; `subscribe_*` packs
//! its token into the first shard of that mode with a free seat (opening a new shard otherwise), so
//! N tokens × M modes cost `ceil(N/K) × M` sockets instead of `N × M`. K is per-`Feeds`
//! ([`Feeds::with_tokens_per_socket`], default [`DEFAULT_TOKENS_PER_SOCKET`], overridable by the
//! `POLY_WS_TOKENS_PER_SOCKET` env for an operator who wants to probe the venue's real
//! per-connection cap). **K = 1 is exactly the pre-batching shape** — one socket, one thread, one
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
//! `book`/`price_change` frame and fed to [`vike_model::L2Book::apply_snapshot`]/`apply_delta` —
//! `apply_snapshot` doesn't check staleness (always accepts) and `apply_delta` only rejects
//! `seq <= last_seq`, so a feed-local counter that only ever increases satisfies both call sites
//! without needing the venue's own numbering. `apply_snapshot`/`apply_delta` take RAW (price, qty)
//! pairs and quantize internally via the book's own `tick_size` (verified in
//! `vike-model/src/orderbook.rs`); a qty-0 delta level is a removal (also the book's own behavior,
//! not reimplemented here). Tick size is resolved once per subscription, on the feed's own thread
//! (mirroring binance's REST warmup, which also runs off `try_spawn` so `subscribe_*` itself never
//! blocks on the network) via [`super::instruments::fetch_token_tick_size`].
//!
//! **What each pump mode emits (capabilities-not-obligations, D7 — `subscribe_book` documents its
//! exact behavior per `vike-data`'s `DataClient::subscribe_book` doc):**
//! - [`PumpMode::Book`]: applies `book`/`price_change` frames to the local book, pushes
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
//!   book's `last_seq` then lags it (see the `PriceChange` arm's load-bearing comment).
//!   `last_trade` frames on this subscription are ignored (no `sink.trade()` call).
//! - [`PumpMode::Quotes`]: identical book bookkeeping and the same derived-quote emission rule as
//!   `Book`, but never calls `sink.book()` — quote-only on the wire.
//! - [`PumpMode::Trades`]: ignores `book`/`price_change` frames entirely (no book state needed);
//!   every `last_trade` frame becomes one `sink.trade()` call. `is_buyer_maker` is derived from the
//!   decoded `taker_is_buy`: a taker BUY means the resting (maker) side was a SELL, so
//!   `is_buyer_maker = !taker_is_buy.unwrap_or(true)` — an absent side (older frames) is treated as
//!   a taker buy, the conservative default (mirrors OKX's taker-side convention in
//!   `okx::market_data::decode_trade`).
//!
//! **Timestamps.** The decoder (`ws.rs::frame_ts`) reads each frame's top-level `timestamp` field
//! (string or number JSON form) into `MarketUpdate`'s `ts: Option<i64>` when present. `TradeTick.ts`
//! uses a `last_trade` frame's own `ts` directly; the derived `QuoteTick.ts` (emitted from
//! `after_book_change`) uses the `ts` of whichever `book`/`price_change` frame produced it. Either
//! falls back to `now_ms()` (local wall clock, receive time) when the frame carries no `timestamp`
//! — documented once here rather than at each call site. **Confirmed against a live capture
//! (2026-07-08)**: all three market-channel frame types carry a top-level `timestamp` as a string
//! of epoch-ms (mirroring the user-channel decoder's `s("timestamp").parse::<i64>()` in
//! `crates/bridges/polymarket/src/user_ws.rs`'s `decode_trade`). A frame that omits the field only
//! ever degrades to today's receive-time stamping (`frame_ts` returns `None`), never produces an
//! incorrect-but-plausible timestamp.
//!
//! **Reconnect (dedup A6, wave 3 — the shared driver).** The WS session/reconnect LIFECYCLE rides
//! [`vike_bridge_core::market_pump`]'s driver: connect (a 10 s BOUNDED TCP dial via the driver's
//! `connect_market_stream` — a black-holed route can't pin the feed thread past the stop flag),
//! subscribe replayed verbatim each session, the 10 s `"PING"` keepalive, the 30 s silent-stall
//! idle watchdog, §B freshness judged on the driver's transport-alive `on_tick` hook, and a
//! stop-aware EXPONENTIAL reconnect backoff (500 ms doubling capped at 30 s, reset on a successful
//! connect) — all five knobs declared in this venue's `MarketPumpSpec` row
//! ([`vike_bridge_core::pump_spec::market_pump_spec`]), which [`feed_main`] consumes. Polymarket
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

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use vike_bridge_core::market_pump::{
    MarketPumpOpts, SessionStatus, run_market_feed_on, run_market_session,
};
use vike_bridge_core::pump_spec::market_pump_spec;
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_bridge_core::{HealthEvent, Keepalive, StreamHealth};
use vike_data::{
    DataClient, FeedRegistry, LiveDataError, LiveDataSink, StreamStatus, SubscriptionId,
};
use vike_model::{BookUpdate, BookUpdateKind, L2Book, QuoteTick, TradeTick};

use super::config::WS_MARKET;
use super::instruments::fetch_token_tick_size_while;
use super::ws::{MarketUpdate, decode_market, subscribe_message};

use crate::raw_tap::{RawCaptureConfig, RawTap, RawTapHandle, RawTapOwner, TappedStream};

// The stream seam + its error shape are the SHARED driver's now (dedup A6) — re-exported under the
// crate's historical names so `raw_tap`'s `TappedStream`, the scripted tests, and vike-backfill's
// `poly_reparse` (which implements `MarketStream` over captured gz frames) keep their seam:
// `read_frame` returns exactly one TEXT frame per call (Ping auto-ponged, control frames swallowed,
// every inbound frame stamping the idle-watchdog liveness clock), `StreamErr::Timeout` is the
// stop-flag poll tick, `Closed` carries the reconnect reason.
pub use vike_bridge_core::market_pump::{FrameOutcome, MarketStream};
pub use vike_bridge_core::user_data::StreamError as StreamErr;

pub(crate) const VENUE: &str = "polymarket";
/// The CLOB app-level keepalive payload (its 10 s cadence is a `MarketPumpSpec`-row knob). The
/// server answers with a non-JSON text `"PONG"` this pump skips as data but counts as liveness.
const PING_PAYLOAD: &str = "PING";
/// Net-hardening §B DATA-freshness threshold — the freshness sibling of the 30 s idle-watchdog
/// transport threshold (this venue's `MarketPumpSpec` row: no inbound frame of ANY kind — a market
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
/// Synthetic-anchor cadence: after this many applied deltas since the last anchor, the pump
/// emits a full-book `Snapshot` (from its own folded state) so a replay never has to seek
/// further back than this many events to re-anchor. Venue snapshots also reset the counter.
const ANCHOR_EVERY_DELTAS: u64 = 512;

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
/// higher with [`Feeds::with_tokens_per_socket`] or the [`TOKENS_PER_SOCKET_ENV`] env when socket
/// count matters more than gap granularity.
pub const DEFAULT_TOKENS_PER_SOCKET: usize = 50;

/// Operator override for [`DEFAULT_TOKENS_PER_SOCKET`], read once per [`Feeds`] construction. A
/// missing/unparseable/zero value keeps the default; [`Feeds::with_tokens_per_socket`] wins over it.
pub const TOKENS_PER_SOCKET_ENV: &str = "POLY_WS_TOKENS_PER_SOCKET";

use vike_model::now_ms;

/// The env-or-default seat count one socket carries — see [`TOKENS_PER_SOCKET_ENV`].
fn env_tokens_per_socket() -> usize {
    std::env::var(TOKENS_PER_SOCKET_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|k| *k >= 1)
        .unwrap_or(DEFAULT_TOKENS_PER_SOCKET)
}

/// Map the neutral [`HealthEvent`] onto this crate's `vike_data::StreamStatus` disclosure
/// vocabulary (1:1) — mirrors the same-named fn in the crypto depth venues (binance/bybit/okx).
/// Kept in the venue crate because `vike-bridge-core` is deliberately `vike-data`-free: the ONE
/// `StreamHealth` this pump now owns across reconnects dedups transport gaps and gates freshness
/// during an open gap internally, so this is a pure translation at the `LiveDataSink` boundary.
fn health_to_stream_status(ev: HealthEvent) -> StreamStatus {
    match ev {
        HealthEvent::Gap { at_ts_ms } => StreamStatus::GapStart { at_ts_ms },
        HealthEvent::Live { gap_started_ts_ms } => StreamStatus::Live { gap_started_ts_ms },
        HealthEvent::Stale { newest_data_ts_ms, now_ms } => {
            StreamStatus::Stale { newest_data_ts_ms, now_ms }
        }
    }
}

/// What one `Feeds` subscription pumps to the sink — see the module doc for the exact emission
/// rules per mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PumpMode {
    /// `subscribe_book`: book snapshots/deltas → `sink.book()` + derived-quote `sink.quote()`.
    Book,
    /// `subscribe_quotes`: same book bookkeeping as `Book`, but only the derived `sink.quote()`.
    Quotes,
    /// `subscribe_trades`: `last_trade` frames only → `sink.trade()`.
    Trades,
}

impl PumpMode {
    /// The `stream` key used in this pump's `LiveDataSink::stream_status` disclosures
    /// (net-hardening §B) — a stable per-mode label so a consumer can tell a dead quote stream from
    /// a dead trade stream on the same token.
    fn as_str(self) -> &'static str {
        match self {
            PumpMode::Book => "book",
            PumpMode::Quotes => "quotes",
            PumpMode::Trades => "trades",
        }
    }

    /// Per-mode data-freshness threshold (net-hardening §B). Book/Quotes 300s, Trades 1800s —
    /// measured floors from the 2026-07-11 Dublin sample, not placeholders (see the constants' doc).
    fn freshness_threshold(self) -> Duration {
        match self {
            PumpMode::Book | PumpMode::Quotes => FRESHNESS_THRESHOLD_BOOK,
            PumpMode::Trades => FRESHNESS_THRESHOLD_TRADES,
        }
    }
}

/// One token's book-maintenance state, carried across reconnects within a single subscription's
/// lifetime. `pub book`/`pub last_top` so the scripted test seam can assert quantized tops
/// directly; `seq` stays private — it is a pure bookkeeping counter with no test-visible meaning
/// beyond what `book`'s own accessors already expose.
pub struct TokenState {
    /// The token's standing book, behind an [`Arc`] so `sink.book()` ships a refcount bump instead
    /// of deep-cloning both `BTreeMap`s on EVERY applied delta (perf audit finding #1 — see
    /// `vike_exec::BookUpdate`). Mutated through [`Arc::make_mut`]: O(1) while this pump holds the
    /// only handle, copy-on-write (on the PUMP thread, never the single-writer core) while a
    /// consumer still holds the last one emitted. Read-only uses (`best_bid`, `top_n`, `tick_size`,
    /// the scripted test's assertions) go through `Deref` unchanged.
    pub book: Arc<L2Book>,
    seq: u64,
    pub last_top: Option<(f64, f64, f64, f64)>,
    /// Applied deltas since the last emitted anchor (Book mode) — drives the synthetic-anchor
    /// cadence ([`ANCHOR_EVERY_DELTAS`]); reset to 0 by any snapshot (venue or synthetic).
    deltas_since_anchor: u64,
}

impl TokenState {
    pub fn new(tick_size: f64) -> Self {
        TokenState {
            book: Arc::new(L2Book::new(tick_size)),
            seq: 0,
            last_top: None,
            deltas_since_anchor: 0,
        }
    }
}

/// One token's seat on a shard's socket: everything the frame protocol mutates for that token —
/// its book fold ([`TokenState`], which owns the feed-local `seq` and `last_top`) and its OWN
/// §B [`StreamHealth`] (so a per-token `Stale` disclosure, and a `Gap`/`Live` pair per token, ride
/// the shared socket without collapsing into one socket-wide signal).
///
/// Slots are what makes batching per-token by construction: a frame is routed to the slot whose
/// `token_id` equals the frame's own `asset_id`, so token A's frames can never touch token B's book
/// or bump its `seq`.
pub struct TokenSlot {
    pub token_id: String,
    pub state: TokenState,
    pub health: StreamHealth,
}

impl TokenSlot {
    /// A fresh slot: an empty book quantized at `tick_size` and a fresh `StreamHealth` armed with
    /// `freshness_threshold_ms` (the mode's [`PumpMode::freshness_threshold`] in production).
    pub fn new(token_id: impl Into<String>, tick_size: f64, freshness_threshold_ms: i64) -> Self {
        TokenSlot {
            token_id: token_id.into(),
            state: TokenState::new(tick_size),
            health: StreamHealth::new(freshness_threshold_ms),
        }
    }
}

/// The token a decoded update belongs to — the ROUTING key of a batched socket. Every
/// market-channel frame names its asset (`book`/`last_trade_price` at the top level, `price_change`
/// per `price_changes` entry, which [`decode_market`] has already split into one update per asset).
fn update_asset_id(up: &MarketUpdate) -> &str {
    match up {
        MarketUpdate::Book { book, .. } => &book.asset_id,
        MarketUpdate::PriceChange { asset_id, .. } => asset_id,
        MarketUpdate::LastTrade { asset_id, .. } => asset_id,
    }
}

/// The slot this socket keeps for `asset_id`, or `None` when no subscription on this socket owns it
/// (a stray cross-token push, or a token whose seat was just released — ignored, never applied).
/// Linear over the shard's handful of seats, in subscribe order, so disclosure order is stable.
fn slot_for<'s>(slots: &'s mut [TokenSlot], asset_id: &str) -> Option<&'s mut TokenSlot> {
    slots.iter_mut().find(|s| s.token_id == asset_id)
}

/// Apply one decoded update to `state`/`sink` per `mode`'s emission rules (module doc). Updates for
/// an `asset_id` other than `token_id` are ignored defensively — the caller has already ROUTED this
/// update to the slot owning `token_id` ([`slot_for`]), so the guard is redundant-by-construction
/// and kept as the belt-and-suspenders it always was.
fn handle_update(
    mode: PumpMode,
    state: &mut TokenState,
    sink: &dyn LiveDataSink,
    token_id: &str,
    up: &MarketUpdate,
    health: &mut StreamHealth,
) {
    match up {
        MarketUpdate::Book { book: pb, ts }
            if pb.asset_id == token_id && mode != PumpMode::Trades =>
        {
            state.seq += 1;
            Arc::make_mut(&mut state.book).apply_snapshot(state.seq, &pb.bids, &pb.asks);
            state.deltas_since_anchor = 0;
            if mode == PumpMode::Book {
                // Recordable snapshot anchor: the RAW wire levels + the freshly bumped feed-local
                // seq, emitted BEFORE `after_book_change`'s derived quote so the recorded order
                // (book_update then quote) matches the replay tie-break (Book < Quote).
                sink.book_update(
                    VENUE,
                    token_id,
                    BookUpdate {
                        ts: ts.unwrap_or_else(now_ms),
                        local_ts: now_ms(),
                        seq: state.seq,
                        kind: BookUpdateKind::Snapshot,
                        tick_size: state.book.tick_size,
                        bids: pb.bids.clone(),
                        asks: pb.asks.clone(),
                        symbol: String::new(),
                    },
                );
            }
            after_book_change(mode, state, sink, token_id, *ts, health);
        }
        MarketUpdate::PriceChange { asset_id, changes, ts }
            if asset_id == token_id && mode != PumpMode::Trades =>
        {
            let bids: Vec<(f64, f64)> =
                changes.iter().filter(|c| c.is_bid).map(|c| (c.price, c.size)).collect();
            let asks: Vec<(f64, f64)> =
                changes.iter().filter(|c| !c.is_bid).map(|c| (c.price, c.size)).collect();
            state.seq += 1;
            Arc::make_mut(&mut state.book).apply_delta(state.seq, &bids, &asks);
            if mode == PumpMode::Book {
                // Recordable delta: the RAW changed levels + the freshly bumped feed-local seq,
                // emitted BEFORE `after_book_change`'s derived quote (recorded order matches the
                // replay tie-break Book < Quote).
                sink.book_update(
                    VENUE,
                    token_id,
                    BookUpdate {
                        ts: ts.unwrap_or_else(now_ms),
                        local_ts: now_ms(),
                        seq: state.seq,
                        kind: BookUpdateKind::Delta,
                        tick_size: state.book.tick_size,
                        bids: bids.clone(),
                        asks: asks.clone(),
                        symbol: String::new(),
                    },
                );
                state.deltas_since_anchor += 1;
                if state.deltas_since_anchor >= ANCHOR_EVERY_DELTAS {
                    state.deltas_since_anchor = 0;
                    state.seq += 1; // the anchor is its own event in the seq chain
                    // The synthetic anchor bumps `state.seq` WITHOUT a book mutation —
                    // `apply_delta`'s next-seq check only requires `seq > last_seq`, and the live
                    // book's `last_seq` now lags the emitted chain by the anchor bumps. That is fine
                    // for the LIVE book (its seq only gates staleness) but replay's contiguity rule
                    // needs the emitted chain contiguous — which it is: every emitted event's seq
                    // increments by exactly 1.
                    let (ab, aa) = state.book.top_n(usize::MAX);
                    sink.book_update(
                        VENUE,
                        token_id,
                        BookUpdate {
                            ts: ts.unwrap_or_else(now_ms),
                            local_ts: now_ms(),
                            seq: state.seq,
                            kind: BookUpdateKind::Snapshot,
                            tick_size: state.book.tick_size,
                            bids: ab,
                            asks: aa,
                            symbol: String::new(),
                        },
                    );
                }
            }
            after_book_change(mode, state, sink, token_id, *ts, health);
        }
        MarketUpdate::LastTrade { asset_id, price, size, taker_is_buy, ts }
            if asset_id == token_id && mode == PumpMode::Trades =>
        {
            // The SAME ts value handed to `sink.trade` feeds the data-freshness timer (§B): a real
            // trade frame is fresh DATA (unlike a keepalive PONG, which never reaches here).
            let tick_ts = ts.unwrap_or_else(now_ms);
            health.observe_data(tick_ts);
            sink.trade(
                VENUE,
                token_id,
                TradeTick {
                    ts: tick_ts,
                    local_ts: now_ms(),
                    price: *price,
                    size: *size,
                    // taker buy ⇒ the resting (maker) side was a sell ⇒ is_buyer_maker = false
                    is_buyer_maker: !taker_is_buy.unwrap_or(true),
                    symbol: String::new(),
                },
            );
        }
        // wrong asset_id, wrong mode for this frame type, or (defensively) an unmatched shape
        _ => {}
    }
}

/// Post book-mutation hook shared by the `Book`/`PriceChange` arms: pushes `sink.book()` (Book
/// mode only) then the derived-quote emission (`Book`+`Quotes`), change-gated on `last_top`.
/// `frame_ts` is the wire ts of the book/price_change frame that triggered this call (module doc's
/// Timestamps section) — used for the derived `QuoteTick.ts`, falling back to `now_ms()` when the
/// frame carried none.
fn after_book_change(
    mode: PumpMode,
    state: &mut TokenState,
    sink: &dyn LiveDataSink,
    token_id: &str,
    frame_ts: Option<i64>,
    health: &mut StreamHealth,
) {
    // Resolve the frame ts ONCE so the data-freshness timer (§B) observes the exact value the
    // derived quote carries. Observe per APPLIED book/price_change frame — not per emitted quote —
    // so a busy book whose top happens to stay put (deep-level churn only, no `sink.quote`) still
    // counts as live DATA and never false-trips staleness. Keepalive PONGs never reach here.
    let ts = frame_ts.unwrap_or_else(now_ms);
    health.observe_data(ts);
    if mode == PumpMode::Book {
        // Refcount bump, NOT a deep copy of both `BTreeMap`s (perf audit finding #1): the pump
        // keeps folding into the same allocation until a consumer still holds it at the next
        // mutation, at which point `Arc::make_mut` copies ON THIS THREAD.
        sink.book(VENUE, token_id, Arc::clone(&state.book));
    }
    if (mode == PumpMode::Book || mode == PumpMode::Quotes)
        && let (Some((bid, bid_size)), Some((ask, ask_size))) =
            (state.book.best_bid(), state.book.best_ask())
    {
        let top = (bid, ask, bid_size, ask_size);
        if state.last_top != Some(top) {
            state.last_top = Some(top);
            sink.quote(
                VENUE,
                token_id,
                QuoteTick {
                    ts,
                    local_ts: now_ms(),
                    bid,
                    ask,
                    bid_size,
                    ask_size,
                    symbol: String::new(),
                },
            );
        }
    }
}

/// Liveness-watchdog wiring threaded into [`run_session`] (net-hardening §B). It bundles the ONE
/// unified [`StreamHealth`] — owned by [`feed_main`] ACROSS reconnects — with this stream's
/// disclosure label and the timing knobs. `StreamHealth` fuses what used to be two separate
/// trackers into one state machine:
/// - TRANSPORT liveness: one `Gap`/`Live` pair per outage;
/// - DATA freshness (the follow-up the `run_session` scope note foreshadowed) — `Stale` when the
///   newest applied data ts lags wall-clock past the mode's freshness threshold
///   (`PumpMode::freshness_threshold`) behind a still-live socket, `Live` on recovery. Persists
///   across reconnects too, so a reconnect's re-seed `observe_data` naturally closes any open
///   staleness episode.
///
/// Production builds it via [`Watchdog::new`] (real 30s idle / 10s keepalive / per-mode freshness —
/// `PumpMode::freshness_threshold`); the scripted tests build it via [`Watchdog::with_timing`] to
/// drive the idle-trip and keepalive paths with zero-length durations, no real-time sleeps.
pub struct Watchdog<'a> {
    health: &'a mut StreamHealth,
    timing: PumpTiming<'a>,
}

impl<'a> Watchdog<'a> {
    /// Production wiring: the 30 s idle / 10 s keepalive knobs of this venue's `MarketPumpSpec`
    /// row (the one edit site for both values).
    pub fn new(health: &'a mut StreamHealth, stream_label: &'a str) -> Self {
        Watchdog { health, timing: PumpTiming::new(stream_label) }
    }

    /// Explicit-timing constructor so tests can force the idle/keepalive paths deterministically
    /// (e.g. `idle_threshold = Duration::ZERO` trips on the first read-timeout tick). The
    /// data-freshness threshold is carried by the injected `health` state itself
    /// ([`StreamHealth::new`]), so a test picks it there — mirroring how the idle threshold is
    /// passed here rather than read from the production const.
    pub fn with_timing(
        health: &'a mut StreamHealth,
        stream_label: &'a str,
        idle_threshold: Duration,
        ping_every: Duration,
    ) -> Self {
        Watchdog {
            health,
            timing: PumpTiming::with_timing(stream_label, idle_threshold, ping_every),
        }
    }
}

/// The health-free half of [`Watchdog`]: the disclosure label + the two timing knobs a session
/// needs, split out because a BATCHED session's health lives per token in its [`TokenSlot`]s rather
/// than in one socket-wide tracker. `Watchdog` is now this plus the single-token seam's `health`
/// borrow, so the two constructors above stay the only places the venue's row is read.
#[derive(Debug, Clone, Copy)]
pub struct PumpTiming<'a> {
    /// The `stream` key of this pump's `LiveDataSink::stream_status` disclosures (§B) — in
    /// production [`PumpMode::as_str`].
    pub stream_label: &'a str,
    /// Silent-stall watchdog window (production: the venue row's 30 s).
    pub idle_threshold: Duration,
    /// Client `"PING"` cadence (production: the venue row's 10 s).
    pub ping_every: Duration,
}

impl<'a> PumpTiming<'a> {
    /// Production wiring: the 30 s idle / 10 s keepalive knobs of this venue's `MarketPumpSpec` row.
    pub fn new(stream_label: &'a str) -> Self {
        let knobs = market_pump_spec(VENUE).knobs();
        PumpTiming {
            stream_label,
            idle_threshold: knobs.idle_threshold.expect("polymarket row declares idle watchdog"),
            ping_every: knobs.keepalive_every.expect("polymarket row declares keepalive"),
        }
    }

    /// Explicit-timing constructor (the test seam — see [`Watchdog::with_timing`]).
    pub fn with_timing(
        stream_label: &'a str,
        idle_threshold: Duration,
        ping_every: Duration,
    ) -> Self {
        PumpTiming { stream_label, idle_threshold, ping_every }
    }
}

/// One inbound TEXT frame → the driver's [`FrameOutcome`], running the whole per-frame protocol —
/// the `on_text` body shared VERBATIM by [`run_session`] (the test/replay seam) and [`feed_main`]
/// (production), so the two cannot drift:
/// - §B transport recovery: the FIRST inbound frame after a gap — market data OR the `"PONG"`
///   keepalive, i.e. proof the transport is responsive again — closes any open gap (`Live`),
///   ordered BEFORE this frame's own data so the consumer sees "recovered" first (`recover()` is a
///   no-op when no gap is open).
/// - a malformed (non-JSON) frame — junk or the `"PONG"` keepalive — is skipped as data
///   (`Ignore`), never an error; it already counted as a liveness tick inside the stream.
/// - a JSON frame's updates are ROUTED per `asset_id` to the owning [`TokenSlot`] ([`slot_for`])
///   and fold through [`handle_update`] there (each APPLIED data frame observes its ts into that
///   token's own freshness half), then `wake` fires once per PROCESSED frame (a frame may yield zero
///   or more sink calls, for zero or more of this socket's tokens) — `Confirm`, though the
///   polymarket row arms no ack watchdog, so the outcome is inert to the lifecycle.
///
/// With one slot this is byte-for-byte the pre-batching body (the recover loop runs once, the router
/// resolves to the only slot, and an unowned `asset_id` is dropped exactly as the old `asset_id ==
/// token_id` guard dropped it).
fn on_frame(
    txt: &str,
    mode: PumpMode,
    slots: &mut [TokenSlot],
    sink: &dyn LiveDataSink,
    stream_label: &str,
    wake: &dyn Fn(),
) -> FrameOutcome {
    // Transport recovery is a SOCKET fact: any inbound frame proves the shared transport is
    // responsive again, so every token riding it closes its own open gap (one `Live` each).
    for slot in slots.iter_mut() {
        if let Some(ev) = slot.health.recover() {
            sink.stream_status(VENUE, &slot.token_id, stream_label, health_to_stream_status(ev));
        }
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(txt) else {
        return FrameOutcome::Ignore; // malformed frame / "PONG" keepalive — ignored as data
    };
    for up in decode_market(&value) {
        let Some(slot) = slot_for(slots, update_asset_id(&up)) else { continue };
        handle_update(mode, &mut slot.state, sink, &slot.token_id, &up, &mut slot.health);
    }
    wake();
    FrameOutcome::Confirm
}

/// The driver's transport-ALIVE read-timeout tick (`on_tick` — the knob added FOR this venue):
/// the moment to judge DATA freshness, distinct from the transport gap the driver's idle watchdog
/// owns. Discloses `Stale` ONCE when the newest applied data ts ages past the mode's freshness
/// threshold ([`PumpMode::freshness_threshold`]) behind the live socket, and `Live` once on
/// recovery; a no-op until then. The driver only fires this AFTER the idle watchdog passes, so it
/// never races the transport gap — `StreamHealth::check_freshness` also gates internally on
/// `in_gap()`, belt-and-suspenders, so this can never double-signal. This catches the failure the
/// transport watchdog structurally CANNOT: a reconnect whose re-subscribe silently failed, so the
/// socket answers keepalives (reads alive) yet no book/trade frame ever arrives.
/// Judged PER TOKEN on a batched socket: a shard whose busy token keeps the wire hot while a quiet
/// co-tenant's book has frozen must still disclose that co-tenant's staleness, so each slot runs its
/// own `check_freshness` against its own newest applied data ts.
fn on_alive_tick(slots: &mut [TokenSlot], sink: &dyn LiveDataSink, stream_label: &str) {
    let now = now_ms();
    for slot in slots.iter_mut() {
        if let Some(ev) = slot.health.check_freshness(now) {
            sink.stream_status(VENUE, &slot.token_id, stream_label, health_to_stream_status(ev));
        }
    }
}

/// Drive ONE WS session against `stream` on the SHARED driver session
/// ([`run_market_session`], dedup A6): subscribe (replayed verbatim by every caller session), then
/// read frames until `stop` fires (clean `Ok(())`), the stream errs (`Err(reason)` — the caller
/// reconnects), the idle watchdog trips (the driver's attributable `"no frames within {N}s
/// (silent stall)"`), or the keepalive/subscribe send fails. The client `"PING"` goes out every
/// `wd.ping_every`; the per-frame protocol is [`on_frame`]; §B data freshness rides the driver's
/// transport-alive tick hook ([`on_alive_tick`]).
///
/// This is the test/replay seam: production ([`feed_main`]) runs the SAME
/// closures under the driver's reconnect loop; `tests/offline/market_feed_scripted.rs` and vike-backfill's
/// `poly_reparse` call this directly with a scripted/replayed [`MarketStream`].
#[allow(clippy::too_many_arguments)] // pump params are all distinct; a struct would obscure them
pub fn run_session<S: MarketStream>(
    stream: &mut S,
    sink: &dyn LiveDataSink,
    token_id: &str,
    mode: PumpMode,
    state: &mut TokenState,
    stop: &AtomicBool,
    wd: &mut Watchdog<'_>,
    wake: impl Fn(),
) -> Result<(), String> {
    // The single-token seam is a ONE-SEAT shard, so there is exactly one session body (no
    // single-vs-batched drift): take the caller's `state`/`health` into a temporary slot, run, put
    // them back. Placeholders (`TokenState::new(0.0)`/`StreamHealth::new(0)`) are never observed —
    // the slot is handed straight to the session and moved back out before returning. Only a PANIC
    // inside the session would leave the placeholders behind, and that unwinds the whole feed
    // thread anyway.
    let timing = wd.timing;
    let mut slot = TokenSlot {
        token_id: token_id.to_string(),
        state: std::mem::replace(state, TokenState::new(0.0)),
        health: std::mem::replace(wd.health, StreamHealth::new(0)),
    };
    let res =
        run_shard_session(stream, sink, mode, std::slice::from_mut(&mut slot), stop, &timing, wake);
    *state = slot.state;
    *wd.health = slot.health;
    res
}

/// The BATCHED twin of [`run_session`] and the one real session body: drive ONE WS session that
/// carries `slots`' whole token set. The subscribe frame is built from the slots in order
/// (`{"assets_ids":[…]}`), each inbound frame is routed to its own slot, and the §B disclosures are
/// per token — see the module doc's WS-batching section. With one slot this is exactly the
/// pre-batching session.
pub fn run_shard_session<S: MarketStream>(
    stream: &mut S,
    sink: &dyn LiveDataSink,
    mode: PumpMode,
    slots: &mut [TokenSlot],
    stop: &AtomicBool,
    timing: &PumpTiming<'_>,
    wake: impl Fn(),
) -> Result<(), String> {
    let tokens: Vec<String> = slots.iter().map(|s| s.token_id.clone()).collect();
    let sub = subscribe_message(&tokens);
    let knobs = market_pump_spec(VENUE).knobs();
    let opts = MarketPumpOpts {
        subscribe: Some(&sub),
        // The idle/keepalive knobs come from the TIMING (production `PumpTiming::new` reads the
        // venue row; scripted tests inject their own via `with_timing`) — never from the row
        // directly, so the test seam keeps its timing injection.
        keepalive: Some(Keepalive { payload: PING_PAYLOAD, every: timing.ping_every }),
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

struct FeedCtx {
    sink: Arc<dyn LiveDataSink>,
    status: Arc<Mutex<String>>,
    wake: Arc<dyn Fn() + Send + Sync>,
    stop: Arc<AtomicBool>,
    /// Best-effort raw-frame capture handle (raw-first tap). `None` = capture off (passthrough).
    raw_tap: Option<Arc<RawTapHandle>>,
    /// Opt-in PIT `SymbolProperties` recorder (`VIKE_RECORD_PROPERTIES=1`). `None` — the default,
    /// and every caller that does not call [`Feeds::with_properties_recorder`] — means
    /// [`reconcile_slots`] does exactly what it did before this field existed: resolve the tick
    /// size and nothing else, with NO extra network call. See that function for the contract.
    properties_rec: Option<Arc<vike_data::PropertiesRecorder>>,
}

impl FeedCtx {
    fn set_status(&self, s: String) {
        *self.status.lock().unwrap() = s;
        (self.wake)();
    }
}

/// One shard's live token set, shared between [`Feeds`] (which mutates it on subscribe/unsubscribe)
/// and that shard's feed thread (which reads it at every session boundary). `epoch` is bumped by
/// every mutation; the thread's [`ShardStream`] wrapper compares it against the epoch its session
/// subscribed at and ends the session the moment they differ, so the driver's
/// reconnect-==-resubscribe path re-dials with the WHOLE new set. Membership is therefore never
/// mutated mid-session, and a token is never half-subscribed.
struct ShardMembership {
    tokens: Mutex<Vec<String>>,
    epoch: AtomicU64,
}

impl ShardMembership {
    fn new(first: String) -> Self {
        ShardMembership { tokens: Mutex::new(vec![first]), epoch: AtomicU64::new(0) }
    }

    fn tokens(&self) -> Vec<String> {
        self.tokens.lock().unwrap().clone()
    }

    fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Relaxed)
    }

    /// Seat `token` here when this shard has room AND does not already carry it, bumping the epoch;
    /// `false` (no mutation) when full or duplicate. A duplicate is refused deliberately: two
    /// subscriptions of the SAME token+mode must stay two independent streams (the pre-batching
    /// behavior — one wire subscription can only be delivered once).
    fn try_admit(&self, token: &str, cap: usize) -> bool {
        let mut tokens = self.tokens.lock().unwrap();
        if tokens.len() >= cap || tokens.iter().any(|t| t == token) {
            return false;
        }
        tokens.push(token.to_string());
        self.epoch.fetch_add(1, Ordering::Relaxed);
        true
    }

    /// Release ONE seat held by `token`, bumping the epoch. Returns whether the shard is now empty
    /// (its caller then stops+joins the feed thread instead of resubscribing).
    fn release(&self, token: &str) -> bool {
        let mut tokens = self.tokens.lock().unwrap();
        if let Some(pos) = tokens.iter().position(|t| t == token) {
            tokens.remove(pos);
            self.epoch.fetch_add(1, Ordering::Relaxed);
        }
        tokens.is_empty()
    }
}

/// A [`MarketStream`] decorator that ends the session as soon as its shard's membership changes:
/// `read_frame` compares the live epoch against the one this session subscribed at and reports
/// `Closed` when they differ, which the driver treats like any other session fault — disclose,
/// back off, reconnect, and (in the connect closure) resubscribe the whole new token set. Pickup
/// latency is bounded by the row's read timeout (2 s). `send_text`/`since_last_frame` pass through.
struct ShardStream<S: MarketStream> {
    inner: S,
    membership: Arc<ShardMembership>,
    subscribed_at: u64,
}

impl<S: MarketStream> MarketStream for ShardStream<S> {
    fn read_frame(&mut self) -> Result<String, StreamErr> {
        if self.membership.epoch() != self.subscribed_at {
            return Err(StreamErr::Closed("token set changed — resubscribing".into()));
        }
        self.inner.read_frame()
    }

    fn send_text(&mut self, s: &str) -> Result<(), StreamErr> {
        self.inner.send_text(s)
    }

    fn since_last_frame(&self) -> Duration {
        self.inner.since_last_frame()
    }
}

/// Human-readable name for a shard's token set — the token itself for a one-seat shard (so a K = 1
/// feed's thread name and status lines read exactly as they did before batching), `"{first}+{n}"`
/// once it carries more.
fn shard_label(tokens: &[String]) -> String {
    match tokens.split_first() {
        Some((first, [])) => first.clone(),
        Some((first, rest)) => format!("{first}+{}", rest.len()),
        None => String::new(),
    }
}

/// Wall-clock ceiling on ONE REST request of the per-token warmup a feed thread makes — the
/// tick-size lookup [`reconcile_slots`] performs for every newly-seated token, plus the two
/// taker-hold requests that join it when `VIKE_RECORD_PROPERTIES=1`.
///
/// **Why this venue needed its own number.** Those requests ran on `crate::egress::agent()`, whose
/// [`crate::egress::REST_TIMEOUT`] is 30 s — the ORDER path's budget, three times the bounded dial two
/// lines further down, on a thread whose stop budget is 12 s in total
/// (`crates/vike-datahub/src/recorder.rs`'s `FEED_STOP_BUDGET_SECS`). And polymarket is
/// the venue actually recording on the CI box, so this was not a hypothetical lane: the derivation that
/// budget rests on covered the other venue.
///
/// **Ten seconds, and deliberately the SAME window as the dial**
/// (`vike_bridge_core::pump_spec`'s `CONNECT_10S`) and as binance's two REST rungs
/// (`crates/bridges/binance/src/family/trades.rs`'s `WARMUP_TIMEOUT`,
/// `crates/bridges/binance/src/family/depth.rs`'s `DEPTH_SEED_TIMEOUT`). That is what keeps the
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
/// **What a timeout costs:** the lookup falls through to `instruments`' `DEFAULT_TICK_SIZE` (0.01),
/// which is this venue's most common value and exactly what a REST failure already produced — the
/// same degradation, sooner.
pub const FEED_WARMUP_TIMEOUT: Duration = Duration::from_secs(10);

/// The agent every feed-thread REST warmup request rides — a named rung so the bound is OBSERVABLE
/// (`agent.config().timeouts().global`) rather than merely written down, the shape
/// `crates/bridges/binance/src/family/trades.rs`'s `warmup_agent` and
/// `crates/bridges/binance/src/family/depth.rs`'s `depth_seed_agent` already have.
///
/// Built through `crate::egress::agent_with_timeout` rather than by hand, so it inherits the SOCKS
/// proxy arm every Polymarket lane needs — a second hand-rolled `ureq::Agent` here would dial direct
/// from a geo-blocked box and silently default every tick size. See
/// `the_feed_warmup_runs_on_a_bounded_agent_not_the_order_paths`.
pub fn feed_warmup_agent() -> ureq::Agent {
    crate::egress::agent_with_timeout(FEED_WARMUP_TIMEOUT)
}

/// Bring `slots` in line with `tokens`: drop the seats no longer subscribed, create one (resolving
/// its tick size over REST — on the FEED thread, off `subscribe_*`'s path, the same discipline as
/// binance's REST warmup) for each newly seated token, and order them like the membership. Slots
/// that survive keep their book, feed-local `seq` and health untouched, so a co-tenant is not
/// disturbed by a neighbour joining or leaving.
///
/// **This is the live token-resolution site, and so the ONE place the venue's declared taker hold
/// is put on the tape** ([`crate::taker_hold`]). When — and only when — `properties_rec` is armed
/// (`VIKE_RECORD_PROPERTIES=1`, off by default), a NEWLY-seated token additionally costs two REST
/// requests ([`crate::taker_hold::fetch_token_taker_hold_ms`]: one Gamma point lookup for the
/// `condition_id`, one `/clob-markets/{cid}` for the hold itself) and one best-effort
/// `kind=properties` row carrying the resolved tick size AND hold. The contract that matters:
///
/// * `properties_rec == None` ⇒ **zero** extra requests and zero extra work, byte-identical to
///   before — the recorder is the gate, so an un-armed process never touches those endpoints.
/// * ONCE per newly-seated token, never per tick and never per order. A surviving seat is not
///   re-resolved, so a reconnect or a neighbour joining costs nothing.
/// * Any failure resolves to `0` (= this venue declares no hold), never a guess.
///
/// **`keep_going` is the feed thread's stop flag, and it is what makes this bounded at all.** Every
/// REST call below is a blocking window on a LIVE FEED THREAD, spent from
/// `crates/vike-datahub/src/recorder.rs`'s `FEED_STOP_BUDGET_SECS` — and the count is not
/// one: it is per NEWLY-SEATED TOKEN, each of which is up to `1 + TICK_SIZE_LOOKUP_MAX_PAGES`
/// requests, plus two more when the properties recorder is armed. A per-request ceiling alone would
/// therefore bound nothing that matters; the predicate is checked in front of EVERY request (here,
/// per token, and inside `fetch_token_tick_size_while` / `fetch_token_taker_hold_ms_while` per
/// request), so a raised flag is ignored for exactly the ONE request already in flight.
///
/// A token seated after the flag goes up keeps its slot — `slots` must stay aligned with `tokens`
/// or the frame router indexes the wrong book — and takes the default tick size, which nothing will
/// read: the driver's next act is a stop check.
fn reconcile_slots<T: RestTransport>(
    slots: &mut Vec<TokenSlot>,
    tokens: &[String],
    transport: &T,
    freshness_ms: i64,
    properties_rec: Option<&Arc<vike_data::PropertiesRecorder>>,
    keep_going: &dyn Fn() -> bool,
) {
    let mut old = std::mem::take(slots);
    for token in tokens {
        match old.iter().position(|s| s.token_id == *token) {
            Some(pos) => slots.push(old.remove(pos)),
            None => {
                let tick_size = fetch_token_tick_size_while(transport, token, keep_going);
                record_token_grid(properties_rec, transport, token, tick_size, keep_going);
                slots.push(TokenSlot::new(token.clone(), tick_size, freshness_ms));
            }
        }
    }
}

/// The recorder half of [`reconcile_slots`], split out so the gate is one readable early return:
/// no recorder (or a recorder built with recording DISABLED) ⇒ return before any network call.
///
/// `PropertiesRecorder::record` is itself a no-op when disabled, but checking `enabled()` here is
/// load-bearing rather than defensive: the taker-hold resolution is TWO REST requests, and they
/// must not be paid by a process that would then throw the row away.
fn record_token_grid<T: RestTransport>(
    rec: Option<&Arc<vike_data::PropertiesRecorder>>,
    transport: &T,
    token_id: &str,
    tick_size: f64,
    keep_going: &dyn Fn() -> bool,
) {
    let Some(rec) = rec.filter(|r| r.enabled()) else {
        return;
    };
    let taker_hold_ms =
        crate::taker_hold::fetch_token_taker_hold_ms_while(transport, token_id, keep_going);
    tracing::debug!(token_id, tick_size, taker_hold_ms, "recording polymarket token properties");
    crate::filters_rec::record_token_properties(
        &Some(Arc::clone(rec)),
        token_id,
        tick_size,
        taker_hold_ms,
        vike_model::now_ns(),
    );
}

/// The thread body `Feeds::spawn_with` runs for ONE SHARD (one socket, up to K tokens of one mode):
/// resolves each seated token's tick size (REST — off `try_spawn`'s path, same discipline as
/// binance's REST warmup), then hands the whole connect → subscribe → session → backoff → reconnect
/// LIFECYCLE to the shared driver ([`run_market_feed_on`]), with this venue's `MarketPumpSpec` row
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
/// closure rather than handed to the driver as `MarketPumpOpts::subscribe`, because a shard's token
/// set can change between sessions and the driver's `subscribe` is fixed for the whole feed. The
/// contract is unchanged — every session still subscribes verbatim before its first read — it is
/// just built per session, from the membership snapshot that session dialled with.
fn shard_main(mode: PumpMode, membership: Arc<ShardMembership>, ctx: FeedCtx) {
    // PROXIED transport: the tick-size REST warmup ([`fetch_token_tick_size_while`]) must ride the
    // SAME SOCKS egress every other Polymarket lane uses. A plain `UreqTransport::new` dials direct
    // and, on a geo-blocked box, fails to resolve the host — logging `fetch_token_tick_size:
    // /markets request failed` and silently defaulting the tick size to 0.01 — while the WS feed
    // correctly tunnels via [`crate::egress::ws_proxy`].
    //
    // BOUNDED, not `crate::egress::agent()`: see [`FEED_WARMUP_TIMEOUT`]. Same proxy arm, shorter
    // ceiling, because this agent's calls are made from a feed thread with a stop budget.
    let transport = UreqTransport::with_agent(VENUE, feed_warmup_agent());
    let stream_label = mode.as_str();
    let freshness_ms = mode.freshness_threshold().as_millis() as i64;
    let knobs = market_pump_spec(VENUE).knobs();
    // Optional SOCKS5 egress (spec §0.1): `None` unless POLY_WS_PROXY_ENABLED is on, in which case
    // the endpoint is the SAME tunnel the REST lane uses (`egress::proxy_url`). Resolved ONCE per
    // shard thread and reused across reconnects.
    let ws_proxy = crate::egress::ws_proxy();
    // The opt-in PIT properties recorder, cloned out of `ctx` so the reconnect closure below (which
    // borrows `ctx` for its sink/status) can hand it to `reconcile_slots`. `None` by default.
    let properties_rec = ctx.properties_rec.clone();
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
    let opts = MarketPumpOpts {
        subscribe: None,
        keepalive: Some(Keepalive {
            payload: PING_PAYLOAD,
            every: knobs.keepalive_every.expect("polymarket row declares keepalive"),
        }),
        ack_timeout: knobs.ack_timeout,
        idle_threshold: knobs.idle_threshold,
        read_timeout: knobs.read_timeout,
        backoff: knobs.backoff,
        connect_timeout: knobs.connect_timeout,
    };
    run_market_feed_on(
        || {
            // Snapshot the membership this session will serve BEFORE dialling. Read the EPOCH
            // FIRST, then the tokens: the two reads are not one atomic step, and this order is the
            // safe one — a change landing between them yields a session subscribed to the NEW set
            // while guarding the OLD epoch, so `ShardStream` immediately ends it and redials (one
            // wasted dial). The reverse order would leave a session serving a STALE token set while
            // believing it current, which no later signal would correct.
            let subscribed_at = membership.epoch();
            let tokens = membership.tokens();
            if tokens != *session.borrow() {
                reconcile_slots(
                    &mut slots.borrow_mut(),
                    &tokens,
                    &transport,
                    freshness_ms,
                    properties_rec.as_ref(),
                    &keep_warming,
                );
                *label.borrow_mut() = shard_label(&tokens);
                *session.borrow_mut() = tokens.clone();
            }
            // The warmup above can spend its whole ceiling, and `Feeds::stop_all` raises the flag
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
            // when off; the driver is generic over MarketStream and unaware of the wrap.
            let stream = vike_bridge_core::market_pump::connect_market_stream_via(
                WS_MARKET,
                knobs.read_timeout,
                knobs.connect_timeout,
                ws_proxy.as_ref(),
            )?;
            let mut tapped = TappedStream::new(stream, ctx.raw_tap.clone(), label.borrow().clone());
            tapped.send_text(&subscribe_message(&tokens)).map_err(|e| match e {
                StreamErr::Closed(m) => m,
                StreamErr::Timeout => "read timeout".to_string(),
            })?;
            ctx.set_status(format!("LIVE · Polymarket {}", label.borrow()));
            Ok(ShardStream { inner: tapped, membership: Arc::clone(&membership), subscribed_at })
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
            SessionStatus::Live => {}
            SessionStatus::Error(e) => {
                ctx.set_status(format!(
                    "polymarket {} ws error (reconnecting): {e}",
                    label.borrow()
                ));
                let now = now_ms();
                for slot in slots.borrow_mut().iter_mut() {
                    slot.state.last_top = None; // stale after the gap — module doc
                    if let Some(ev) = slot.health.enter_gap(now) {
                        ctx.sink.stream_status(
                            VENUE,
                            &slot.token_id,
                            stream_label,
                            health_to_stream_status(ev),
                        );
                    }
                }
            }
        },
    );
}

/// One socket: a feed thread (owned by the shared [`FeedRegistry`] — dedup A6, the registry half:
/// one stop flag + one `JoinHandle`) serving up to K tokens of ONE [`PumpMode`].
struct Shard {
    /// `Feeds`-local identity, so a subscription can name its shard across `Vec` reshuffles.
    id: u64,
    mode: PumpMode,
    membership: Arc<ShardMembership>,
    /// the registry key owning this shard's thread + stop flag (never handed to a caller).
    feed: SubscriptionId,
}

/// The venue's live feed set: **shards** (one socket each, up to K tokens of one mode — module doc's
/// WS-batching section) held in the shared [`FeedRegistry`], plus the caller-facing subscription
/// bookkeeping mapping each issued [`SubscriptionId`] to its (shard, token) seat. `unsubscribe`
/// releases exactly one seat — stopping+joining the shard's thread only once its LAST seat goes,
/// otherwise resubscribing the remainder; `shutdown` stops+joins every shard; nothing is ever
/// detached. At K = 1 every shard holds exactly one token and this is the pre-batching
/// one-socket-per-subscription lifecycle `bybit`/`okx`/`hyperliquid` still run.
pub struct Feeds {
    sink: Arc<dyn LiveDataSink>,
    pub status: Arc<Mutex<String>>,
    wake: Arc<dyn Fn() + Send + Sync>,
    registry: FeedRegistry,
    /// Raw-frame capture: the owner (joined on shutdown) + the hot handle cloned into each FeedCtx.
    /// `None` unless constructed via `with_raw_capture`.
    raw_tap_owner: Option<RawTapOwner>,
    raw_tap: Option<Arc<RawTapHandle>>,
    /// Opt-in PIT `SymbolProperties` recorder — see [`Feeds::with_properties_recorder`]. `None`
    /// (the default) means no properties are recorded and the taker-hold lookups are never made.
    properties_rec: Option<Arc<vike_data::PropertiesRecorder>>,
    /// K — seats per socket ([`DEFAULT_TOKENS_PER_SOCKET`] / [`TOKENS_PER_SOCKET_ENV`] /
    /// [`Feeds::with_tokens_per_socket`]).
    tokens_per_socket: usize,
    shards: Vec<Shard>,
    /// issued subscription → the seat it holds (`shard id`, `token_id`).
    subs: HashMap<SubscriptionId, (u64, String)>,
    next_shard_id: u64,
    /// This `Feeds`'s OWN `SubscriptionId` space — the registry's ids key shards, which are an
    /// implementation detail a caller never sees (many subscriptions can share one).
    next_sub_id: u64,
}

/// The shard thread body, as a plain `fn` so a shard can be spawned from more than one call site
/// (`shard_main` in production, a network-free stand-in in tests).
type ShardBody = fn(PumpMode, Arc<ShardMembership>, FeedCtx);

impl Feeds {
    /// `sink` receives every quote/trade/book call from every subscription this `Feeds` spawns
    /// (shared — construct once, subscribe many tokens/modes). `wake` is the GUI repaint nudge
    /// fired on status changes and after each processed frame; pass `|| {}` for a headless caller.
    /// The registry's spawn hook is the venue's HFT affinity pin (opt-in via `VIKE_PIN_CORES`,
    /// no-op otherwise) — threaded in as a closure because `vike-data` deliberately never depends
    /// on `vike-exec`.
    pub fn new(sink: Arc<dyn LiveDataSink>, wake: impl Fn() + Send + Sync + 'static) -> Self {
        Feeds {
            sink,
            status: Arc::new(Mutex::new("connecting to Polymarket…".into())),
            wake: Arc::new(wake),
            registry: FeedRegistry::with_spawn_hook(|| {
                vike_exec::affinity::pin_current_thread(
                    vike_exec::affinity::Role::MarketData,
                    "polymarket",
                );
            }),
            raw_tap_owner: None,
            raw_tap: None,
            properties_rec: None,
            tokens_per_socket: env_tokens_per_socket(),
            shards: Vec::new(),
            subs: HashMap::new(),
            next_shard_id: 0,
            next_sub_id: 0,
        }
    }

    /// Like [`Feeds::new`] but with raw-frame capture ON: spawns a [`RawTap`] writer whose handle is
    /// cloned into every subscription's `FeedCtx`, so each feed thread's `TappedStream` tees frames
    /// to it. The owner is held here and joined on `shutdown`.
    ///
    /// Note the interaction with batching: the tap keys its gz files by the STREAM it wrapped, so a
    /// multi-token shard writes one `{first}+{n}` series carrying every seated token's frames
    /// interleaved. `poly_reparse` still replays a single token out of it — its pump ignores any
    /// `asset_id` it did not ask for — it just reads more bytes. Set K = 1 for one file per token.
    pub fn with_raw_capture(
        sink: Arc<dyn LiveDataSink>,
        wake: impl Fn() + Send + Sync + 'static,
        cfg: RawCaptureConfig,
    ) -> std::io::Result<Self> {
        let (handle, owner) = RawTap::spawn(cfg)?;
        let mut feeds = Self::new(sink, wake);
        feeds.raw_tap_owner = Some(owner);
        feeds.raw_tap = Some(handle);
        Ok(feeds)
    }

    /// Arm the opt-in PIT `SymbolProperties` recorder (the venue-adapter convention:
    /// `VIKE_RECORD_PROPERTIES=1`, off by default, best-effort). Pass the handle
    /// `vike_data::PropertiesRecorder::open_from_env` hands back — it is `None` unless the env gate
    /// is on, so a caller that always threads it through still gets the default-off behavior.
    ///
    /// This is the ONE switch that makes a subscribed token's grid — tick size AND the venue's
    /// declared [`crate::taker_hold`] — land on the `kind=properties` tape, which is where a
    /// backtest reads it back from (`HistStore::properties_as_of` → `EngineParams::properties` →
    /// the latency gate's per-symbol hold table). Un-armed, the feed makes exactly the REST calls
    /// it made before this existed. See [`reconcile_slots`] for the per-token cost.
    ///
    /// Only shards opened AFTER this call see the recorder.
    pub fn with_properties_recorder(
        mut self,
        rec: Option<Arc<vike_data::PropertiesRecorder>>,
    ) -> Self {
        self.properties_rec = rec;
        self
    }

    /// Set K — how many tokens one socket carries (module doc). `1` restores the pre-batching
    /// one-socket-per-subscription shape exactly; `0` is clamped to `1`. Overrides
    /// [`TOKENS_PER_SOCKET_ENV`]/[`DEFAULT_TOKENS_PER_SOCKET`]; only shards opened AFTER this call
    /// see the new value.
    pub fn with_tokens_per_socket(mut self, tokens_per_socket: usize) -> Self {
        self.tokens_per_socket = tokens_per_socket.max(1);
        self
    }

    /// K — seats per socket, for a caller that wants to log/assert the effective batching.
    pub fn tokens_per_socket(&self) -> usize {
        self.tokens_per_socket
    }

    /// How many sockets (shards) are open right now — `ceil(N/K) × modes` in the steady state, and
    /// the number this whole change exists to cut.
    pub fn socket_count(&self) -> usize {
        self.shards.len()
    }

    /// Seat `token_id` on a shard of `mode` and issue the caller's [`SubscriptionId`]: the first
    /// shard of that mode with a free seat that does not already carry this token takes it (bumping
    /// its epoch, so its live session resubscribes the whole set), otherwise a new shard/socket is
    /// opened. `body` is [`shard_main`] in production; tests substitute a network-free stand-in.
    fn spawn_with(
        &mut self,
        token_id: &str,
        mode: PumpMode,
        body: ShardBody,
    ) -> std::io::Result<SubscriptionId> {
        let cap = self.tokens_per_socket.max(1);
        // `try_admit` only mutates the shard it returns `true` for, so a side-effecting `find` is
        // safe here: every earlier shard was full (or already carries this token) and untouched.
        let joined =
            self.shards.iter().find(|s| s.mode == mode && s.membership.try_admit(token_id, cap));
        let shard_id = match joined.map(|s| s.id) {
            Some(id) => id,
            None => self.spawn_shard(token_id, mode, body)?,
        };
        let id = SubscriptionId(self.next_sub_id);
        self.next_sub_id += 1;
        self.subs.insert(id, (shard_id, token_id.to_string()));
        Ok(id)
    }

    /// Open one new socket seeded with `token_id`, one [`FeedRegistry::spawn`] call (dedup A6): the
    /// registry allocates its key + stop flag and owns the join handle (the MarketData affinity pin
    /// rides its spawn hook, see [`Feeds::new`]); this venue wrapper only assembles its own
    /// [`FeedCtx`] — including the raw-tap handle, this venue's delta — around the registry-issued
    /// stop flag. The thread is named after the token that OPENED it (stable for the thread's life,
    /// and identical to the pre-batching name at K = 1).
    fn spawn_shard(
        &mut self,
        token_id: &str,
        mode: PumpMode,
        body: ShardBody,
    ) -> std::io::Result<u64> {
        let (sink, status, wake, raw_tap, properties_rec) = (
            Arc::clone(&self.sink),
            Arc::clone(&self.status),
            Arc::clone(&self.wake),
            self.raw_tap.clone(),
            self.properties_rec.clone(),
        );
        let membership = Arc::new(ShardMembership::new(token_id.to_string()));
        let thread_membership = Arc::clone(&membership);
        let feed = self.registry.spawn(format!("feed-poly-{token_id}-{mode:?}"), move |stop| {
            let ctx = FeedCtx { sink, status, wake, stop, raw_tap, properties_rec };
            body(mode, thread_membership, ctx)
        })?;
        let id = self.next_shard_id;
        self.next_shard_id += 1;
        self.shards.push(Shard { id, mode, membership, feed });
        Ok(id)
    }

    fn try_spawn(
        &mut self,
        token_id: &str,
        mode: PumpMode,
    ) -> Result<SubscriptionId, LiveDataError> {
        self.spawn_with(token_id, mode, shard_main)
            .map_err(|e| LiveDataError::Subscribe(format!("polymarket {token_id} ({mode:?}): {e}")))
    }
}

impl DataClient for Feeds {
    fn subscribe_bars(
        &mut self,
        _symbol: &str,
        _interval: &str,
    ) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported("polymarket serves ticks; bars via resample"))
    }

    fn subscribe_quotes(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.try_spawn(symbol, PumpMode::Quotes)
    }

    fn subscribe_trades(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.try_spawn(symbol, PumpMode::Trades)
    }

    fn subscribe_book(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.try_spawn(symbol, PumpMode::Book)
    }

    /// Release exactly the seat `id` names; every other subscription on this `Feeds` keeps running.
    /// The socket carrying it is stopped+JOINed only when that was its LAST seat — otherwise the
    /// shard resubscribes its remaining tokens (a co-tenant sees a normal reconnect: one
    /// `GapStart`, then `Live` on the first frame back). Unknown ids (already released, or never
    /// issued) are a no-op.
    fn unsubscribe(&mut self, id: SubscriptionId) {
        let Some((shard_id, token_id)) = self.subs.remove(&id) else { return };
        let Some(pos) = self.shards.iter().position(|s| s.id == shard_id) else { return };
        if self.shards[pos].membership.release(&token_id) {
            let shard = self.shards.remove(pos);
            self.registry.stop_join(shard.feed);
        }
    }

    /// Raise every shard's stop flag and JOIN NOTHING — phase one of a teardown that spans several
    /// clients (`vike_data::live::DataClient::begin_shutdown`). A shard notices on its next read
    /// timeout, and a shard caught mid-dial notices only when this venue's `connect_timeout`
    /// expires, so starting every shard's clock at once is what keeps a whole recorder profile's
    /// stop at about one wind-down instead of one per socket.
    fn begin_shutdown(&mut self) {
        self.registry.raise_stops();
    }

    /// Deterministic teardown: raise every stop flag, then JOIN every feed thread.
    fn shutdown(&mut self) {
        self.registry.shutdown();
        self.shards.clear();
        self.subs.clear();
        // join the raw-tap writer last, after every feed thread that fed it has stopped
        if let Some(owner) = self.raw_tap_owner.take() {
            owner.shutdown();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{FeedCtx, Feeds, PumpMode, ShardMembership, shard_label};
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    use vike_data::DataClient;

    // The shared capturing sink (testing-arch Phase 4d) — replaces the inline formatted-string
    // RecordingSink copy that used to live here (same canonical `calls()` line forms).
    use vike_data::RecordingSink;

    /// A network-free stand-in for [`super::shard_main`]: just polls its own stop flag, without
    /// ever touching a socket. Lets the per-shard lifecycle (spawn/pack/unsubscribe/shutdown) be
    /// exercised deterministically.
    fn fake_feed_body(_mode: PumpMode, _membership: Arc<ShardMembership>, ctx: FeedCtx) {
        while !ctx.stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// K = 1 — the pre-batching shape: every subscription gets its own socket.
    fn unbatched(sink: Arc<RecordingSink>) -> Feeds {
        Feeds::new(sink, || {}).with_tokens_per_socket(1)
    }

    #[test]
    fn freshness_threshold_maps_book_and_quotes_tight_trades_loose() {
        // The per-mode mapping the whole change hinges on — asserted directly here, because the
        // scripted pump tests build the tracker from mirror constants and can't reach this private
        // method (only network-doing `shard_main` calls it). A swapped arm would otherwise slip by.
        assert_eq!(PumpMode::Book.freshness_threshold(), super::FRESHNESS_THRESHOLD_BOOK);
        assert_eq!(PumpMode::Quotes.freshness_threshold(), super::FRESHNESS_THRESHOLD_BOOK);
        assert_eq!(PumpMode::Trades.freshness_threshold(), super::FRESHNESS_THRESHOLD_TRADES);
    }

    #[test]
    fn subscribe_returns_distinct_ids() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = unbatched(sink);
        let id1 = feeds.spawn_with("111", PumpMode::Book, fake_feed_body).expect("spawn ok");
        let id2 = feeds.spawn_with("222", PumpMode::Trades, fake_feed_body).expect("spawn ok");
        assert_ne!(id1, id2, "distinct ids per subscribe");
        feeds.shutdown();
    }

    /// Ids stay distinct even when the subscriptions SHARE one socket — the caller-facing id space
    /// is `Feeds`'s own, not the registry's shard keys.
    #[test]
    fn subscribe_returns_distinct_ids_when_batched_onto_one_socket() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = Feeds::new(sink, || {}).with_tokens_per_socket(4);
        let id1 = feeds.spawn_with("111", PumpMode::Book, fake_feed_body).expect("spawn ok");
        let id2 = feeds.spawn_with("222", PumpMode::Book, fake_feed_body).expect("spawn ok");
        assert_ne!(id1, id2, "distinct ids per subscribe, even sharing a socket");
        assert_eq!(feeds.socket_count(), 1, "both tokens rode one socket");
        feeds.shutdown();
    }

    #[test]
    fn unsubscribe_stops_only_that_one_feed_others_keep_running() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = unbatched(sink);
        let id1 = feeds.spawn_with("111", PumpMode::Book, fake_feed_body).expect("spawn ok");
        let _id2 = feeds.spawn_with("222", PumpMode::Book, fake_feed_body).expect("spawn ok");

        feeds.unsubscribe(id1);
        assert_eq!(feeds.socket_count(), 1, "only the unsubscribed stream's socket is removed");
        assert_eq!(feeds.registry.len(), 1, "the other subscription's thread keeps running");

        feeds.shutdown();
        assert!(feeds.registry.is_empty());
    }

    #[test]
    fn unsubscribe_of_an_unknown_id_is_a_no_op() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = unbatched(sink);
        let id = feeds.spawn_with("111", PumpMode::Book, fake_feed_body).expect("spawn ok");
        feeds.unsubscribe(vike_data::SubscriptionId(id.0 + 100)); // never issued
        assert_eq!(feeds.registry.len(), 1, "unknown id must not disturb the real subscription");
        feeds.shutdown();
    }

    #[test]
    fn shutdown_joins_every_feed() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = unbatched(sink);
        feeds.spawn_with("111", PumpMode::Book, fake_feed_body).expect("spawn ok");
        feeds.spawn_with("222", PumpMode::Quotes, fake_feed_body).expect("spawn ok");
        feeds.spawn_with("333", PumpMode::Trades, fake_feed_body).expect("spawn ok");
        feeds.shutdown();
        assert!(feeds.registry.is_empty());
        assert_eq!(feeds.socket_count(), 0);
    }

    // ---- WS batching: packing, K = 1 equivalence, per-mode isolation ----------------------------

    /// K = 1 is the pre-batching shape: N subscriptions ⇒ N sockets, one token each.
    #[test]
    fn k_of_one_opens_one_socket_per_subscription() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = unbatched(sink);
        for token in ["111", "222", "333"] {
            feeds.spawn_with(token, PumpMode::Book, fake_feed_body).expect("spawn ok");
        }
        assert_eq!(feeds.socket_count(), 3, "K=1 never batches");
        assert_eq!(feeds.registry.len(), 3, "one feed thread per socket");
        feeds.shutdown();
    }

    /// The whole point: K tokens ride one socket, so N tokens cost `ceil(N/K)` sockets.
    #[test]
    fn tokens_pack_into_ceil_n_over_k_sockets() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = Feeds::new(sink, || {}).with_tokens_per_socket(3);
        for token in ["1", "2", "3", "4", "5", "6", "7"] {
            feeds.spawn_with(token, PumpMode::Book, fake_feed_body).expect("spawn ok");
        }
        assert_eq!(feeds.socket_count(), 3, "7 tokens at K=3 ⇒ ceil(7/3) = 3 sockets");
        assert_eq!(feeds.registry.len(), 3);
        feeds.shutdown();
    }

    /// A shard carries ONE mode: the emission rules differ per mode, so a quotes token never shares
    /// a socket with a trades token even when seats are free.
    #[test]
    fn modes_never_share_a_socket() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = Feeds::new(sink, || {}).with_tokens_per_socket(8);
        feeds.spawn_with("111", PumpMode::Book, fake_feed_body).expect("spawn ok");
        feeds.spawn_with("222", PumpMode::Quotes, fake_feed_body).expect("spawn ok");
        feeds.spawn_with("333", PumpMode::Trades, fake_feed_body).expect("spawn ok");
        assert_eq!(feeds.socket_count(), 3, "one socket per mode despite 8 free seats each");
        feeds.shutdown();
    }

    /// The same token subscribed twice in one mode stays TWO independent streams (pre-batching
    /// behavior): one wire subscription can only be delivered once, so the duplicate opens its own
    /// socket rather than silently aliasing the first.
    #[test]
    fn a_duplicate_token_in_one_mode_gets_its_own_socket() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = Feeds::new(sink, || {}).with_tokens_per_socket(8);
        let a = feeds.spawn_with("111", PumpMode::Book, fake_feed_body).expect("spawn ok");
        let b = feeds.spawn_with("111", PumpMode::Book, fake_feed_body).expect("spawn ok");
        assert_ne!(a, b);
        assert_eq!(feeds.socket_count(), 2, "a duplicate token never shares a seat");
        feeds.shutdown();
    }

    /// Releasing one seat of a shared socket keeps the socket (and its co-tenants) alive; releasing
    /// the LAST seat stops+joins it.
    #[test]
    fn a_shared_socket_survives_until_its_last_seat_is_released() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = Feeds::new(sink, || {}).with_tokens_per_socket(4);
        let a = feeds.spawn_with("111", PumpMode::Book, fake_feed_body).expect("spawn ok");
        let b = feeds.spawn_with("222", PumpMode::Book, fake_feed_body).expect("spawn ok");
        assert_eq!(feeds.socket_count(), 1);

        feeds.unsubscribe(a);
        assert_eq!(feeds.socket_count(), 1, "the co-tenant keeps the socket open");
        assert_eq!(feeds.registry.len(), 1, "its feed thread was NOT joined");

        feeds.unsubscribe(b);
        assert_eq!(feeds.socket_count(), 0, "the last seat closes the socket");
        assert!(feeds.registry.is_empty(), "and joins its feed thread");
        feeds.shutdown();
    }

    /// A released seat frees room for the next token on the SAME socket — no socket leak from
    /// churn.
    #[test]
    fn a_released_seat_is_reused_by_the_next_subscription() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = Feeds::new(sink, || {}).with_tokens_per_socket(2);
        let a = feeds.spawn_with("111", PumpMode::Book, fake_feed_body).expect("spawn ok");
        feeds.spawn_with("222", PumpMode::Book, fake_feed_body).expect("spawn ok");
        assert_eq!(feeds.socket_count(), 1, "the socket is full");
        feeds.unsubscribe(a);
        feeds.spawn_with("333", PumpMode::Book, fake_feed_body).expect("spawn ok");
        assert_eq!(feeds.socket_count(), 1, "the freed seat took the new token");
        feeds.shutdown();
    }

    /// Membership mutation bumps the epoch — the signal a live shard's stream watches to end its
    /// session and resubscribe the whole set. A refused admission (full / duplicate) must NOT bump
    /// it, or every subscribe attempt would churn an unrelated socket.
    #[test]
    fn membership_bumps_the_epoch_only_on_a_real_change() {
        let m = ShardMembership::new("a".into());
        assert_eq!(m.epoch(), 0);
        assert!(m.try_admit("b", 2));
        assert_eq!(m.epoch(), 1, "an admitted token bumps the epoch");
        assert!(!m.try_admit("c", 2), "full");
        assert!(!m.try_admit("a", 4), "duplicate");
        assert_eq!(m.epoch(), 1, "a refused admission never mutates");
        assert_eq!(m.tokens(), vec!["a".to_string(), "b".to_string()]);
        assert!(!m.release("a"), "still seated: b");
        assert_eq!(m.epoch(), 2);
        assert!(m.release("b"), "now empty");
        assert!(m.release("nope"), "releasing an unseated token is a no-op on an empty shard");
    }

    /// The label a shard's thread name / status lines carry: a one-seat shard reads exactly as it
    /// did before batching (bare token id), a batched one names its opener plus the extra count.
    #[test]
    fn shard_label_is_the_bare_token_at_one_seat() {
        assert_eq!(shard_label(&["tok".to_string()]), "tok");
        assert_eq!(shard_label(&["tok".to_string(), "b".to_string(), "c".to_string()]), "tok+2");
        assert_eq!(shard_label(&[]), "");
    }

    /// K comes from the env when unset explicitly, and `with_tokens_per_socket` clamps 0 to 1
    /// (a zero-seat socket could never accept a token).
    #[test]
    fn k_defaults_and_clamps() {
        let sink = Arc::new(RecordingSink::default());
        let feeds = Feeds::new(sink, || {});
        assert_eq!(
            feeds.tokens_per_socket(),
            super::env_tokens_per_socket(),
            "the default is the env-or-DEFAULT_TOKENS_PER_SOCKET value"
        );
        let sink = Arc::new(RecordingSink::default());
        assert_eq!(Feeds::new(sink, || {}).with_tokens_per_socket(0).tokens_per_socket(), 1);
    }

    #[test]
    fn bars_are_unsupported() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = Feeds::new(sink, || {});
        assert!(matches!(
            feeds.subscribe_bars("111", "1m"),
            Err(vike_data::LiveDataError::Unsupported(_))
        ));
    }

    #[test]
    fn with_raw_capture_spawns_tap_and_shuts_down_clean() {
        let sink = Arc::new(RecordingSink::default());
        let dir = tempfile::tempdir().unwrap();
        let cfg = super::RawCaptureConfig { dir: dir.path().to_path_buf(), channel_cap: 16 };
        let mut feeds = Feeds::with_raw_capture(sink, || {}, cfg).expect("spawn ok");
        // a fake feed body that never touches a socket — just proves lifecycle with capture on
        feeds.spawn_with("111", PumpMode::Book, fake_feed_body).expect("spawn ok");
        feeds.shutdown(); // must join the feed thread AND the tap writer, no hang
        assert!(feeds.registry.is_empty());
    }

    #[test]
    fn plain_new_has_no_capture() {
        let sink = Arc::new(RecordingSink::default());
        let feeds = Feeds::new(sink, || {});
        assert!(feeds.raw_tap.is_none(), "Feeds::new must not enable capture");
    }
}

#[cfg(test)]
mod warmup_bound_tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use vike_bridge_core::signer::Signer;
    use vike_bridge_core::transport::VenueApiError;

    /// A transport that answers nothing and COUNTS. `stop_after` requests in, it raises `stop` —
    /// standing in for the operator's `systemctl stop` landing mid-warmup.
    struct CountingStub {
        seen: AtomicUsize,
        stop: Arc<AtomicBool>,
        stop_after: usize,
    }

    impl RestTransport for CountingStub {
        fn signed(
            &self,
            _b: &str,
            _p: &str,
            _m: &str,
            _q: &[(&str, String)],
            _s: &dyn Signer,
        ) -> Result<serde_json::Value, VenueApiError> {
            unreachable!("the warmup is unauthenticated")
        }

        fn public(
            &self,
            _b: &str,
            _p: &str,
            _q: &[(&str, String)],
        ) -> Result<serde_json::Value, VenueApiError> {
            let n = self.seen.fetch_add(1, Ordering::Relaxed) + 1;
            if n >= self.stop_after {
                self.stop.store(true, Ordering::Relaxed);
            }
            // A network failure — the shape a black-holed request eventually takes, and the one
            // that sends `fetch_token_tick_size_while` on to its `/markets` page walk.
            Err(VenueApiError { code: 0, msg: "stubbed: no wire".into() })
        }
    }

    /// **The feed-thread warmup rides a BOUNDED agent, not the order path's 30 s one.**
    ///
    /// MUTATION PROOF: point [`feed_warmup_agent`] back at `crate::egress::agent()` — assertion one
    /// fails on the value (30 s ≠ 10 s) and assertion two on the ordering. Reads only agent
    /// configuration, no clock and no network, so it fails identically on any box.
    #[test]
    fn the_feed_warmup_runs_on_a_bounded_agent_not_the_order_paths() {
        let warmup = feed_warmup_agent().config().timeouts().global;
        assert_eq!(
            warmup,
            Some(FEED_WARMUP_TIMEOUT),
            "the per-token tick-size warmup must run on its own bounded agent — it is a blocking \
             call on a live feed thread, inside the driver's connect closure"
        );

        let order_path = crate::egress::agent()
            .config()
            .timeouts()
            .global
            .expect("the shared polymarket agent has always carried a global timeout");
        assert!(
            FEED_WARMUP_TIMEOUT < order_path,
            "the warmup bound ({FEED_WARMUP_TIMEOUT:?}) must be shorter than the order path's \
             ({order_path:?}), or this test is satisfied by the defect"
        );

        assert!(
            FEED_WARMUP_TIMEOUT <= vike_bridge_core::pump_spec::CONNECT_10S,
            "the polymarket warmup ({FEED_WARMUP_TIMEOUT:?}) is now the LARGEST window a feed \
             thread can be caught in, bigger than the dial \
             ({:?}) the recorder's FEED_STOP_BUDGET_SECS is derived from. Shorten it, or re-derive \
             that budget deliberately.",
            vike_bridge_core::pump_spec::CONNECT_10S
        );
    }

    /// **A stop raised mid-warmup stops the NEXT request** — the half a per-request ceiling cannot
    /// buy, and the half that dominates on this venue: one token's resolution is up to 51 requests
    /// and a shard reseats many tokens at once.
    ///
    /// The stub raises the flag while answering the first request, so a correct `reconcile_slots`
    /// issues exactly ONE and seats every remaining token from the default.
    ///
    /// MUTATION PROOF: delete the `if !keep_going()` guard at the top of
    /// `crates/bridges/polymarket/src/instruments.rs`'s `fetch_token_tick_size_while` and its twin
    /// in `fetch_token_tick_size_paged_while` — the count goes from 1 to 6 and this fails on the
    /// first assertion. (Six, not 153: this stub ERRORS, and an errored page ends the walk after
    /// one request, so it is two requests per token. Against a venue that answers, the same three
    /// tokens are up to 153 — which is the number that matters on a real box and the reason the
    /// predicate exists at all.) Counts stubbed calls, so it fails identically on any box.
    #[test]
    fn a_stop_raised_during_the_warmup_stops_the_next_request() {
        let stop = Arc::new(AtomicBool::new(false));
        let stub =
            CountingStub { seen: AtomicUsize::new(0), stop: Arc::clone(&stop), stop_after: 1 };
        let flag = Arc::clone(&stop);
        let keep_going = move || !flag.load(Ordering::Relaxed);

        let tokens: Vec<String> = ["aaa", "bbb", "ccc"].iter().map(|s| s.to_string()).collect();
        let mut slots: Vec<TokenSlot> = Vec::new();
        reconcile_slots(&mut slots, &tokens, &stub, 1_000, None, &keep_going);

        assert_eq!(
            stub.seen.load(Ordering::Relaxed),
            1,
            "exactly the ONE request already in flight when the flag went up — every later one \
             must be refused"
        );
        assert_eq!(
            slots.iter().map(|s| s.token_id.clone()).collect::<Vec<_>>(),
            tokens,
            "an interrupted warmup still seats every token: `slots` is indexed positionally \
             against the session's token list, so a short vec routes frames into the wrong book"
        );
    }

    /// The control the test above needs to mean anything: with the flag DOWN, the same stub, the
    /// same tokens, the resolution runs to completion — so a scanner-shaped mistake (a predicate
    /// wired to a constant `false`, a stub that answers nothing) cannot make the assertion above
    /// pass by doing nothing at all.
    #[test]
    fn with_no_stop_the_warmup_resolves_every_token() {
        let stop = Arc::new(AtomicBool::new(false));
        let stub = CountingStub {
            seen: AtomicUsize::new(0),
            stop: Arc::clone(&stop),
            stop_after: usize::MAX,
        };
        let keep_going = || true;

        let tokens: Vec<String> = ["aaa", "bbb"].iter().map(|s| s.to_string()).collect();
        let mut slots: Vec<TokenSlot> = Vec::new();
        reconcile_slots(&mut slots, &tokens, &stub, 1_000, None, &keep_going);

        // Per token: the `/tick-size` point lookup, then the `/markets` walk. The stub errors, and
        // an ERRORED page ends the walk (`fetch_token_tick_size_paged_while` returns `None` on a
        // REST failure), so it is 2 requests per token, not 1 + 50.
        assert_eq!(stub.seen.load(Ordering::Relaxed), 4, "two requests per newly-seated token");
        assert_eq!(slots.len(), 2);
    }
}
