//! **`DataClientWebsocket<S>`: the ONE data client of a venue whose every subscription is its own text
//! WebSocket** (owner ruling U-2: binance, aster, bybit, okx, deribit, hyperliquid). The venue hands
//! it a small [`DataClientSpec`] (its roster id and display name, its perp-naming and bar-refusal
//! rules, and per lane a [`DataClientLanePlan`]: what to dial, what to subscribe, how to read a
//! frame). Everything else is here once:
//!
//! - **The lifecycle.** One affinity-pinned registry thread per subscription
//!   (`crate::feed_ctx::pinned_feed_registry`); a perp's companion mark stream is ref-counted per
//!   symbol (`crate::mark_stream::MarkPairings`, the venue's `mark_streams` row through
//!   [`DataClientWebsocket::with_mark_streams`]); `unsubscribe` stops and joins exactly one stream (and its
//!   mark when it held the last reference); `begin_shutdown` raises every stop and joins nothing;
//!   `shutdown` joins them all.
//! - **The refusals, on the caller's thread.** Every verb asks the venue's declared caps row FIRST
//!   (`vike_data::require_live_verb`, owner ruling B-21), so a lane the venue does not serve is the
//!   table's refusal and never a thread; a bars subscription then asks [`DataClientSpec::refuse_bars`]
//!   (deribit's resolution table, B-22).
//! - **The status, spelled once**, each with the state the engine KNOWS, and written through the one
//!   writer (`crate::feed_ctx::set_status`): `Connecting` `connecting to {DISPLAY}…` at construction,
//!   `Connected` [`live_status`] on a bar seed and on every session's first confirmed frame, `Error`
//!   `"{key} seed error: {e}"`, `Error` `"{key} ws error (reconnecting): {e}"`, `Connecting`
//!   `"{key} reconnecting: {notice}"` for a session the venue announced the end of (the plan's `notice`), and `Error` for a plan's
//!   refusal `"{key}: {e}"` ([`DataClientLane::key`]): the lane ends and never streams.
//!   A `Depth` plan writes none: it discloses its health on the `depth` stream instead.
//! - **The drivers.** A `Session` lane rides `crate::websocket_session::market` with the venue's `pump_spec` row; a
//!   `Depth` lane rides `crate::depth::run_depth` with the depth row the plan names, publishing
//!   through `crate::feed_ctx::publish_top`.
//! - **The ONE place a decoded [`DataClientEvent`] reaches the sink** ([`emit`]): a bar ticks the
//!   bar-close cache, then lands on the closed (lossless) or forming (conflating) lane, and a close
//!   the decoder inferred from its successor's push lands on the closed lane alone (that push ticks
//!   the cache); a print passes the one trade guard (`crate::is_valid_trade`) and carries its receive
//!   time; a quote takes its receive time, and a quote with no venue time takes it as its `ts` too
//!   (`crate::depth::stamp_quote`, the A1 rule, B-21); a book a `Session` lane folds itself lands
//!   whole on the lossless `book` lane, and a full-depth snapshot on the DOM's `l2_snapshot`.
//! - **The threads' name, by the thread rule** (`{hub}-{role}`, at most 15 bytes: the feed threads
//!   run in the gatewayhub): every lane thread is [`DATA_CLIENT_THREAD`], and its venue, symbol and
//!   lane are fields of the span its body runs in (B-20).
//!
//! A lane whose loop stays the venue's (binance's trade tape: warm-up, drain, splice) is an `Own`
//! plan: the engine still spawns, names, pins, refuses and stops it. A loop that only needed a sink
//! call no event carried is not one: an inferred close, a self-folded book and a full-depth snapshot
//! are events.
//!
//! A lane whose SOCKET and WORDS are the venue's is a `SessionOn` plan (ig: every lane is a
//! subscription on the client's ONE Lightstreamer session, with its own refusal ladder, silence
//! watch and status words): the engine runs the session driver with the venue's `pump_spec` row and
//! the lane's stop, and the venue's hooks dial, read each frame, judge the dataless tick and speak
//! for each session's start and end. Like an `Own` lane it writes the sink itself, never through
//! `emit`. A client-wide part of the venue's (ig's link) stops with the client: the spec's
//! [`DataClientSpec::begin_shutdown`] and [`DataClientSpec::shutdown`], after the lanes'.

use std::sync::{Arc, Mutex};

use vike_data::{
    DataClient, DataClientError, DataClientRegistry, DataClientSink, DataClientSubscriptionId,
    require_live_verb,
};
use vike_marketdata::{Bar, L2Book, TickTrade};
use vike_model::feed_status::{DataClientConnectionState, DataClientStatus};
use vike_model::{StreamLabel, StreamStatus, now_ms};
use vike_venues::DataClientVerb;
use vike_venues::venue_timings::{VenueTimingsDepth, VenueTimingsReconnect};

use super::context::DataClientContext;
use super::event::{BarKline, DataClientBook, DataClientEvent, DataClientMark};
use crate::depth::{WebsocketSessionDepthOperation, run_depth, stamp_quote};
use crate::feed_ctx::{HEALTHY_STATUS_PREFIX, pinned_feed_registry, publish_top};
use crate::mark_stream::{MarkPairings, unsubscribe_with_mark};
use crate::websocket::WebsocketSessionUserStreamTungstenite;
use crate::websocket_session::market::{
    WebsocketSessionFrameOutcome, WebsocketSessionMarketNotice, WebsocketSessionMarketOptions,
    WebsocketSessionMarketStream, WebsocketSessionMarketStreamTungstenite, WebsocketSessionStatus,
    connect_market_socket, run_market_on,
};
use crate::websocket_session::spec;

/// The name of every data-client lane thread, by the owner's thread rule (`{hub}-{role}`, at most 15
/// bytes, which is all Linux keeps): `gw`, the gatewayhub, the one process that opens a venue socket
/// (decision 0164). The venue, symbol and lane are fields of the span the thread's body runs in, never
/// part of its name.
pub const DATA_CLIENT_THREAD: &str = "gw-data-client";

/// One lane of a subscription: the five `DataClient` verbs, plus the mark stream a perp's bars
/// subscription pairs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataClientLane {
    /// `subscribe_bars`.
    Bars,
    /// The venue's REAL mark price, paired to a perp's bars subscription.
    Mark,
    /// `subscribe_quotes`.
    Quotes,
    /// `subscribe_trades`.
    Trades,
    /// `subscribe_book` (the lossless L2 lane).
    Book,
    /// `subscribe_depth` (the conflating DOM lane).
    Depth,
}

impl DataClientLane {
    /// The caps verb that serves this lane (`None` for the mark, which rides a bars subscription).
    fn verb(self) -> Option<DataClientVerb> {
        match self {
            DataClientLane::Bars => Some(DataClientVerb::Bars),
            DataClientLane::Mark => None,
            DataClientLane::Quotes => Some(DataClientVerb::Quotes),
            DataClientLane::Trades => Some(DataClientVerb::Trades),
            DataClientLane::Book => Some(DataClientVerb::Book),
            DataClientLane::Depth => Some(DataClientVerb::Depth),
        }
    }

    /// The lane's word: the bars `interval`, `mark`, or the tick lane's `StreamLabel` word.
    #[must_use]
    pub fn word(self, interval: &str) -> &str {
        match self {
            DataClientLane::Bars => interval,
            DataClientLane::Mark => "mark",
            DataClientLane::Quotes => StreamLabel::Quotes.word(),
            DataClientLane::Trades => StreamLabel::Trades.word(),
            DataClientLane::Book => StreamLabel::Book.word(),
            DataClientLane::Depth => StreamLabel::Depth.word(),
        }
    }

    /// What the lane's status words and subscribe errors name: `{symbol}@{interval}` for bars,
    /// `{symbol} {word}` for every other lane.
    #[must_use]
    pub fn key(self, symbol: &str, interval: &str) -> String {
        match self {
            DataClientLane::Bars => format!("{symbol}@{interval}"),
            other => format!("{symbol} {}", other.word(interval)),
        }
    }
}

/// What one lane dials and how it reads it: the venue's half of a lane, from [`DataClientSpec::plan`].
/// It is built on the lane's own thread and consumed there, so nothing in it needs to be `Send`.
#[expect(
    clippy::type_complexity,
    reason = "each closure's own signature, spelled once at its field"
)]
pub enum DataClientLanePlan {
    /// One socket on the session driver (`crate::websocket_session::market`), with the venue's `pump_spec` row.
    Session {
        /// The socket's address.
        url: String,
        /// The frame sent at the start of every session; `None` where the URL path subscribes.
        subscribe: Option<String>,
        /// The app-level keepalive text, sent at the row's cadence; `None` where the venue needs none.
        keepalive: Option<&'static str>,
        /// The frame by which the venue announces this socket's end (the pump's planned reconnect,
        /// `crate::websocket_session::market`'s `WebsocketSessionMarketNotice`); `None` where the bridge reads none.
        notice: Option<WebsocketSessionMarketNotice>,
        /// The REST bar warm-up, run ONCE per feed before the first dial (bars lanes only; `None`
        /// elsewhere). A reconnect does not re-seed.
        seed: Option<Box<dyn FnOnce() -> Result<Vec<Bar>, String>>>,
        /// One text frame: emit what it holds through the given `emit`, and say what it meant to the
        /// handshake (data or an ack confirms; a reject is the session's fatal error).
        decode:
            Box<dyn FnMut(&str, &mut dyn FnMut(DataClientEvent)) -> WebsocketSessionFrameOutcome>,
    },
    /// One book socket on the depth driver (`crate::depth::run_depth`).
    Depth {
        /// The socket's address.
        url: String,
        /// The book's subscribe frame; `None` where the URL path subscribes.
        subscribe: Option<String>,
        /// The app-level keepalive text; `None` where the venue needs none.
        keepalive: Option<&'static str>,
        /// A WS-seeded venue's unsubscribe and subscribe frames for the in-place re-seed; empty for a
        /// REST-seeded one.
        resubscribe: Vec<String>,
        /// The REST book seed (`None` back: the venue seeds the book on the socket).
        seed: Box<dyn Fn() -> Option<L2Book> + Sync>,
        /// Fold one frame into the book.
        decode: Box<dyn FnMut(&str, &mut Option<Arc<L2Book>>) -> WebsocketSessionDepthOperation>,
        /// How many levels per side the DOM receives.
        levels: usize,
        /// The venue's `venue_timings` depth row: watchdog, freshness, re-seed and backoff.
        timings: VenueTimingsDepth,
    },
    /// A lane whose loop stays the venue's: it runs on the lane's thread with the lane's context.
    Own(Box<dyn FnOnce(&DataClientContext)>),
    /// One session lane on the session driver (`crate::websocket_session::market::run_market_on`, the venue's
    /// `pump_spec` row, the lane's stop) whose every seam is the venue's: what it dials, how it reads
    /// a frame and the dataless tick, and what it says when a session starts and ends. The hooks write
    /// the sink and the status line themselves; the engine writes neither.
    SessionOn {
        /// Hand the next session its stream (ig: its subscription on the client's one shared
        /// session); `Err` is that session's fault, in the venue's words.
        connect: Box<
            dyn FnMut(&DataClientContext) -> Result<Box<dyn WebsocketSessionMarketStream>, String>,
        >,
        /// One text frame: what it holds reaches the sink, and what it meant to the session.
        on_text: Box<dyn FnMut(&DataClientContext, &str) -> WebsocketSessionFrameOutcome>,
        /// The driver's dataless tick (a read timeout with no frame).
        on_tick: Box<dyn FnMut(&DataClientContext)>,
        /// A session's first confirmed frame.
        on_live: Box<dyn FnMut(&DataClientContext)>,
        /// A session that ended as a fault, or a dial that failed, in the driver's words.
        on_error: Box<dyn FnMut(&DataClientContext, &str)>,
        /// The wait before the next dial in place of the row's; `None` keeps the row's.
        backoff: Option<VenueTimingsReconnect>,
    },
}

/// A venue's half of [`DataClientWebsocket`]: its facts, as constants and per-lane plans.
pub trait DataClientSpec: Send + Sync + 'static {
    /// The roster id: the caps row every verb asks first, the `pump_spec` row every session reads, the
    /// registry's pin label, and the venue every sink call and journal line names.
    const VENUE: &'static str;
    /// The display name the status line speaks: `connecting to {DISPLAY}…`, `LIVE · {DISPLAY}`.
    const DISPLAY: &'static str;
    /// Whether a perp's mark stream runs when the venue's `mark_streams` row says nothing (decision 0095).
    const MARK_STREAMS_DEFAULT_ON: bool;
    /// Whether the newest bar of the REST seed is still FORMING (it then goes to the forming lane, the
    /// rest are seeded); `false` for a venue whose seed holds closed bars only.
    const SEED_LAST_BAR_FORMING: bool = true;

    /// Whether a bars subscription on `symbol` also opens the venue's mark stream (the venue's perp
    /// naming). Default: never.
    fn pairs_mark(&self, _symbol: &str) -> bool {
        false
    }

    /// A bars `interval` the venue cannot serve, refused on the CALLER's thread before any spawn: the
    /// refusal's words. Default: none.
    fn refuse_bars(&self, _interval: &str) -> Result<(), String> {
        Ok(())
    }

    /// What `lane` of `symbol` dials and reads. Runs on the LANE's thread (a route may read the
    /// network). `interval` is the bars interval, or the lane's word. `Err` is the lane's refusal: it is
    /// written as `"{key}: {why}"` and the lane ends.
    ///
    /// # Errors
    /// The venue cannot route `symbol` on `lane`.
    fn plan(
        &self,
        lane: DataClientLane,
        symbol: &str,
        interval: &str,
    ) -> Result<DataClientLanePlan, String>;

    /// Phase one of the client's teardown, after every lane's stop is raised: stop what the venue
    /// holds for the whole client (ig: its one Lightstreamer link), joining nothing. Default: nothing.
    fn begin_shutdown(&self) {}

    /// The end of the client's teardown, after every lane is joined: wind down what the venue holds
    /// for the whole client (ig: its link's reader). Default: nothing.
    fn shutdown(&self) {}
}

/// The healthy status text of the venue displayed as `display`: [`HEALTHY_STATUS_PREFIX`] and the
/// name. ONE string per venue, not per lane: every lane of a client writes the one status line, and
/// the writer journals a TRANSITION, so a per-lane spelling would make every session edge a line.
#[must_use]
pub fn live_status(display: &str) -> String {
    format!("{HEALTHY_STATUS_PREFIX} {display}")
}

/// The one data client over a venue's [`DataClientSpec`] (the module doc says what it owns).
pub struct DataClientWebsocket<S: DataClientSpec> {
    spec: Arc<S>,
    sink: Arc<dyn DataClientSink>,
    status: Arc<Mutex<DataClientStatus>>,
    wake: Arc<dyn Fn() + Send + Sync>,
    registry: DataClientRegistry,
    /// Whether a perp's bars subscription pairs the venue's mark stream (its `mark_streams` row).
    mark_streams: bool,
    mark_pairings: MarkPairings<DataClientSubscriptionId>,
}

impl<S: DataClientSpec> DataClientWebsocket<S> {
    /// A client over `spec` writing to `sink`. `wake` is the GUI repaint nudge, fired after a status
    /// write and after data; a headless caller passes `|| {}`.
    pub fn new(
        spec: S,
        sink: Arc<dyn DataClientSink>,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        DataClientWebsocket {
            spec: Arc::new(spec),
            sink,
            status: Arc::new(Mutex::new(DataClientStatus::new(
                DataClientConnectionState::Connecting,
                format!("connecting to {}…", S::DISPLAY),
            ))),
            wake: Arc::new(wake),
            registry: pinned_feed_registry(S::VENUE),
            mark_streams: S::MARK_STREAMS_DEFAULT_ON,
            mark_pairings: MarkPairings::default(),
        }
    }

    /// Apply the venue's `mark_streams` row (decision 0095): `stored` is the switch as the composition root
    /// read it (`vike_secrets::venue_setting::mark_streams_setting`), `None` when no legal row says
    /// anything, which keeps [`DataClientSpec::MARK_STREAMS_DEFAULT_ON`].
    #[must_use]
    pub fn with_mark_streams(mut self, stored: Option<bool>) -> Self {
        self.mark_streams = stored.unwrap_or(S::MARK_STREAMS_DEFAULT_ON);
        self
    }

    /// The client's one status (state and words), as its lanes last wrote it.
    #[must_use]
    pub fn status(&self) -> DataClientStatus {
        self.status.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    /// How many lane threads are running, every one of which writes the ONE status line: a consumer
    /// that reads that line as evidence asks this first, because a second lane makes it ambiguous.
    /// Counts SUBSCRIPTIONS, a paired mark stream included.
    #[must_use]
    pub fn status_writer_lanes(&self) -> usize {
        self.registry.len()
    }

    /// Spawn `lane` of `symbol` on its own named, pinned registry thread. An OS spawn failure is
    /// returned, never panicked.
    fn spawn(
        &mut self,
        lane: DataClientLane,
        symbol: &str,
        interval: &str,
    ) -> std::io::Result<DataClientSubscriptionId> {
        let spec = Arc::clone(&self.spec);
        let (sink, status, wake) =
            (Arc::clone(&self.sink), Arc::clone(&self.status), Arc::clone(&self.wake));
        let (symbol, interval) = (symbol.to_string(), interval.to_string());
        self.registry.spawn(DATA_CLIENT_THREAD.to_string(), move |stop| {
            let ctx = DataClientContext { venue: S::VENUE, sink, status, wake, stop, own: () };
            let _span = tracing::info_span!(
                "data_client",
                venue = S::VENUE,
                symbol = %symbol,
                lane = lane.word(&interval)
            )
            .entered();
            run_lane(&*spec, lane, &symbol, &interval, &ctx);
        })
    }

    /// Pair the venue's mark stream to the bars subscription `bars_id` just made for `symbol`: at most
    /// ONE mark socket per symbol, ref-counted. Fail-soft: a mark thread that cannot spawn leaves the
    /// bars feed live and valuation on the resolver's bar-close rung.
    fn pair_mark(&mut self, bars_id: DataClientSubscriptionId, symbol: &str) {
        let pair = self.mark_streams && self.spec.pairs_mark(symbol);
        if self.mark_pairings.attach(bars_id, symbol, pair) {
            return; // nothing to pair, or the running stream was ref-counted
        }
        let mark = DataClientLane::Mark;
        if let Ok(mark_id) = self.spawn(mark, symbol, mark.word("")) {
            self.mark_pairings.record(bars_id, symbol, mark_id);
        }
    }

    /// A tick-lane verb: the caps row first, then the lane's thread.
    fn subscribe(
        &mut self,
        lane: DataClientLane,
        symbol: &str,
    ) -> Result<DataClientSubscriptionId, DataClientError> {
        if let Some(verb) = lane.verb() {
            require_live_verb(S::VENUE, verb)?;
        }
        let interval = lane.word("");
        self.spawn(lane, symbol, interval).map_err(|e| {
            DataClientError::Subscribe(format!("{} {}: {e}", S::VENUE, lane.key(symbol, interval)))
        })
    }
}

impl<S: DataClientSpec> DataClient for DataClientWebsocket<S> {
    fn subscribe_bars(
        &mut self,
        symbol: &str,
        interval: &str,
    ) -> Result<DataClientSubscriptionId, DataClientError> {
        require_live_verb(S::VENUE, DataClientVerb::Bars)?;
        self.spec.refuse_bars(interval).map_err(DataClientError::Subscribe)?;
        let lane = DataClientLane::Bars;
        let id = self.spawn(lane, symbol, interval).map_err(|e| {
            DataClientError::Subscribe(format!("{} {}: {e}", S::VENUE, lane.key(symbol, interval)))
        })?;
        self.pair_mark(id, symbol);
        Ok(id)
    }

    fn subscribe_quotes(
        &mut self,
        symbol: &str,
    ) -> Result<DataClientSubscriptionId, DataClientError> {
        self.subscribe(DataClientLane::Quotes, symbol)
    }

    fn subscribe_trades(
        &mut self,
        symbol: &str,
    ) -> Result<DataClientSubscriptionId, DataClientError> {
        self.subscribe(DataClientLane::Trades, symbol)
    }

    fn subscribe_book(
        &mut self,
        symbol: &str,
    ) -> Result<DataClientSubscriptionId, DataClientError> {
        self.subscribe(DataClientLane::Book, symbol)
    }

    fn subscribe_depth(
        &mut self,
        symbol: &str,
    ) -> Result<DataClientSubscriptionId, DataClientError> {
        self.subscribe(DataClientLane::Depth, symbol)
    }

    /// Stop and JOIN exactly the stream `id` names, and its paired mark stream when this was the last
    /// bars subscription holding it. An unknown id is a no-op.
    fn unsubscribe(&mut self, id: DataClientSubscriptionId) {
        unsubscribe_with_mark(&mut self.mark_pairings, &mut self.registry, id);
    }

    /// Raise every lane's stop and join nothing: phase one of a teardown across several clients, so
    /// it costs one read timeout and not one per lane (B-20). Then the spec's own.
    fn begin_shutdown(&mut self) {
        self.registry.raise_stops();
        self.spec.begin_shutdown();
    }

    /// Raise every stop, then join every lane thread. Then the spec's own.
    fn shutdown(&mut self) {
        self.mark_pairings.clear();
        self.registry.shutdown();
        self.spec.shutdown();
    }
}

/// A lane thread's body: the venue's plan, then [`run_plan`]; a refused plan writes `"{key}: {why}"`
/// and the lane ends.
fn run_lane<S: DataClientSpec>(
    spec: &S,
    lane: DataClientLane,
    symbol: &str,
    interval: &str,
    ctx: &DataClientContext,
) {
    match spec.plan(lane, symbol, interval) {
        Ok(plan) => run_plan::<S>(plan, lane, symbol, interval, ctx),
        Err(why) => ctx.set_status(
            DataClientConnectionState::Error,
            format!("{}: {why}", lane.key(symbol, interval)),
        ),
    }
}

/// Run one lane's `plan` on this thread until `ctx.stop`, dialing the real venue: the body every
/// spawned lane runs. Public so a venue's offline test can drive a real plan against a loopback host.
pub fn run_plan<S: DataClientSpec>(
    plan: DataClientLanePlan,
    lane: DataClientLane,
    symbol: &str,
    interval: &str,
    ctx: &DataClientContext,
) {
    run_plan_on::<S, _>(plan, lane, symbol, interval, ctx, dial);
}

/// The production dial of a `Session` lane: the bounded connect (`opts.connect_timeout`, the venue's
/// row) and the read timeout the stop flag is polled on.
fn dial(
    url: &str,
    opts: &WebsocketSessionMarketOptions<'_>,
) -> Result<WebsocketSessionMarketStreamTungstenite, String> {
    let socket = connect_market_socket(url, opts.read_timeout, opts.connect_timeout, None)?;
    Ok(WebsocketSessionMarketStreamTungstenite::new(WebsocketSessionUserStreamTungstenite(socket)))
}

/// [`run_plan`] over any `dial` of a `Session` lane's socket (a scripted stream in a test). A `Depth`
/// lane dials through its own driver, and a `SessionOn` lane through its venue's `connect`.
pub fn run_plan_on<S: DataClientSpec, St: WebsocketSessionMarketStream>(
    plan: DataClientLanePlan,
    lane: DataClientLane,
    symbol: &str,
    interval: &str,
    ctx: &DataClientContext,
    mut dial: impl FnMut(&str, &WebsocketSessionMarketOptions<'_>) -> Result<St, String>,
) {
    let key = lane.key(symbol, interval);
    let knobs = spec::row(S::VENUE);
    match plan {
        DataClientLanePlan::Session { url, subscribe, keepalive, notice, seed, mut decode } => {
            if let Some(seed) = seed {
                seed_bars::<S>(seed(), symbol, interval, &key, ctx);
            }
            let opts = WebsocketSessionMarketOptions {
                notice,
                ..knobs.opts(subscribe.as_deref(), keepalive)
            };
            run_market_on(
                || dial(&url, &opts),
                &opts,
                &ctx.stop,
                &now_ms,
                |txt| decode(txt, &mut |event| emit(ctx, symbol, interval, event)),
                || {}, // no dataless-tick judgment on these lanes (polymarket's freshness knob)
                |s| match s {
                    WebsocketSessionStatus::Live => {
                        ctx.set_status(
                            DataClientConnectionState::Connected,
                            live_status(S::DISPLAY),
                        );
                    }
                    WebsocketSessionStatus::Error(e) => ctx.set_status(
                        DataClientConnectionState::Error,
                        format!("{key} ws error (reconnecting): {e}"),
                    ),
                    WebsocketSessionStatus::Announced(words) => ctx.set_status(
                        DataClientConnectionState::Connecting,
                        format!("{key} reconnecting: {words}"),
                    ),
                },
            );
        }
        DataClientLanePlan::Depth {
            url,
            subscribe,
            keepalive,
            resubscribe,
            seed,
            decode,
            levels,
            timings,
        } => {
            let resubscribe: Vec<&str> = resubscribe.iter().map(String::as_str).collect();
            run_depth(
                &url,
                subscribe.as_deref(),
                keepalive,
                &resubscribe,
                seed,
                decode,
                |book: &Arc<L2Book>| {
                    publish_top(&*ctx.sink, &*ctx.wake, ctx.venue, symbol, book, levels);
                },
                |health: StreamStatus| {
                    ctx.sink.stream_status(ctx.venue, symbol, StreamLabel::Depth.word(), health);
                },
                &ctx.stop,
                knobs.read_timeout,
                timings.idle_threshold(),
                timings.freshness_threshold(),
                timings.reseed_interval.map(|t| t.value),
                &now_ms,
                timings.backoff(),
            );
        }
        DataClientLanePlan::Own(body) => body(ctx),
        DataClientLanePlan::SessionOn {
            mut connect,
            mut on_text,
            mut on_tick,
            mut on_live,
            mut on_error,
            backoff,
        } => {
            let mut opts = knobs.opts(None, None);
            if let Some(backoff) = backoff {
                opts.backoff = backoff;
            }
            run_market_on(
                || connect(ctx),
                &opts,
                &ctx.stop,
                &now_ms,
                |txt| on_text(ctx, txt),
                || on_tick(ctx),
                |s| match s {
                    WebsocketSessionStatus::Live => on_live(ctx),
                    WebsocketSessionStatus::Error(e) => on_error(ctx, e),
                    // `knobs.opts(None, None)` arms no notice hook, so no session of this lane ends announced.
                    WebsocketSessionStatus::Announced(_) => {}
                },
            );
        }
    }
}

/// The REST warm-up of a bars lane: the closed bars seed the series and, where the venue's seed ends
/// in the still-forming bar (`S::SEED_LAST_BAR_FORMING`, more than one bar), that bar ticks the
/// bar-close cache and lands on the forming lane; then the healthy text. A failed seed is the lane's
/// `seed error`, and the stream starts anyway.
fn seed_bars<S: DataClientSpec>(
    seed: Result<Vec<Bar>, String>,
    symbol: &str,
    interval: &str,
    key: &str,
    ctx: &DataClientContext,
) {
    match seed {
        Ok(mut bars) => {
            let forming =
                if S::SEED_LAST_BAR_FORMING && bars.len() > 1 { bars.pop() } else { None };
            ctx.sink.seed_bars(ctx.venue, symbol, interval, bars);
            if let Some(f) = forming {
                ctx.sink.bar_close_tick(ctx.venue, symbol, f.close, f.timestamp);
                ctx.sink.forming_bar(ctx.venue, symbol, interval, f);
            }
            ctx.set_status(DataClientConnectionState::Connected, live_status(S::DISPLAY));
        }
        Err(e) => {
            ctx.set_status(DataClientConnectionState::Error, format!("{key} seed error: {e}"));
        }
    }
}

/// The ONE place a decoded event reaches the sink, under the series `symbol` (and `interval` for a
/// bar); the module doc lists the rules. Every event that reached the sink fires the wake nudge once,
/// a funding rate and an inferred close excepted (each rides a frame whose mark or forming bar wakes).
fn emit(ctx: &DataClientContext, symbol: &str, interval: &str, event: DataClientEvent) {
    let (sink, venue) = (&ctx.sink, ctx.venue);
    match event {
        DataClientEvent::Bar(BarKline { bar, closed }) => {
            // A candle close is not the venue mark: it feeds the bar-close cache, never `mark_tick`.
            sink.bar_close_tick(venue, symbol, bar.close, bar.timestamp);
            if closed {
                sink.close_bar(venue, symbol, interval, bar);
            } else {
                sink.forming_bar(venue, symbol, interval, bar);
            }
        }
        DataClientEvent::InferredClose(bar) => {
            sink.close_bar(venue, symbol, interval, bar);
            return;
        }
        DataClientEvent::Book(DataClientBook(book)) => sink.book(venue, symbol, book),
        DataClientEvent::DepthSnapshot { tick_size, bids, asks, ts } => {
            sink.l2_snapshot(venue, symbol, tick_size, bids, asks, ts);
        }
        DataClientEvent::Mark(DataClientMark { px, ts }) => sink.mark_tick(venue, symbol, px, ts),
        DataClientEvent::Funding(funding) => {
            sink.funding(venue, symbol, funding);
            return;
        }
        DataClientEvent::Quote(mut quote) => {
            stamp_quote(&mut quote);
            sink.quote(venue, symbol, quote);
        }
        DataClientEvent::Trade(trade) => {
            if !emit_trade(ctx, symbol, trade) {
                return;
            }
        }
        DataClientEvent::Trades(trades) => {
            for trade in trades {
                emit_trade(ctx, symbol, trade);
            }
        }
        // A tick pump's book words: a `Depth` plan's driver publishes the book it folds, and a
        // `Session` lane that folds its own hands it over as `Book`.
        DataClientEvent::BookUpdated { .. }
        | DataClientEvent::Resync
        | DataClientEvent::Ignored => {
            return;
        }
    }
    (ctx.wake)();
}

/// One print to the sink: refused by the one trade guard (a price or size that is not finite and
/// positive), else stamped with its receive time. `false` when it was refused.
fn emit_trade(ctx: &DataClientContext, symbol: &str, mut trade: TickTrade) -> bool {
    if !crate::is_valid_trade(trade.price, trade.size) {
        return false;
    }
    trade.local_timestamp = now_ms();
    ctx.sink.trade(ctx.venue, symbol, trade);
    true
}

#[path = "websocket_tests.rs"]
#[cfg(test)]
mod websocket_tests;
