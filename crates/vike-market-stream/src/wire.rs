//! The `stream` class of the live-data wire: one frame per `DataClientSink` verb, keyed by a per-session
//! `k`. These are the frames the tube carries (`docs/decisions/0167-one-live-data-tube-vike-market-stream-on-a-data-plane-tier.md`):
//! [`StreamUp`] from the subscriber, [`StreamDown`] from the sender.
//!
//! The handshake, the socket classes and the `Request`/`Response` that open a `stream` socket are the
//! hosting protocol's (for the gatewayhub, `crates/vike-gatewayhub-client/src/proto.rs`); what rides the
//! socket AFTER its open reply is this module's, so a `match` on [`StreamDown`] is a complete
//! protocol and anything else is a desync.
//!
//! # The 13 sink verbs
//!
//! Thirteen of `DataClientSink`'s methods are each a [`StreamDown`] frame: `seed_bars`
//! ([`StreamDown::Seed`]), `close_bar` ([`StreamDown::BarClose`]), `forming_bar`
//! ([`StreamDown::Forming`]), `mark_tick` ([`StreamDown::Mark`]), `bar_close_tick`
//! ([`StreamDown::BarCloseTick`]), `l2_snapshot` ([`StreamDown::Depth`]), `quote`, `trade`, `book`,
//! `book_update` ([`StreamDown::BookDelta`]), `stream_status` ([`StreamDown::Status`]),
//! `data_delay` ([`StreamDown::Delay`]) and `funding` ([`StreamDown::Funding`], only on a session that
//! asked for it with [`StreamUp::Funding`]). A fourteenth method, `top_of_book_tick`, is NOT a frame: only the
//! tradehub's price tap sends it, in process, to its own core (decision 0208's amendment of 2026-11-15), and a
//! tube's sink inherits its no-op. Frames carry the session-assigned key id `k` instead of
//! repeating the venue and symbol (a polymarket token id is 78 digits): the venue is the session's,
//! the symbol is bound by [`StreamDown::SubscribeOk`] (or announced by [`StreamDown::Key`]).
//!
//! * **Ordering**: one TCP stream, so wire order IS delivery order; the receiver delivers every frame
//!   INLINE into the sink on its reader thread, in order.
//! * **`seq`** is assigned by the sender per `(session, k, verb)` BEFORE any drop decision,
//!   strictly +1, on every data frame. The six LOSSLESS verbs ([`StreamGapVerb`]) must be continuous; a
//!   jump with no [`StreamDown::Gap`] before it is a silent loss and the receiver discloses it as one.
//!   The five CONFLATABLE verbs (`forming_bar`, `mark_tick`, `bar_close_tick`, `l2_snapshot`,
//!   `funding`) supersede in place by contract, so a `seq` jump on them is not a loss.
//! * **The delivery class of a subscription is the subscriber's to raise** ([`DataClientSinkDelivery`]): a
//!   recorder that writes `l2_snapshot` rows into a tape cannot take a superseded frame as "not a
//!   loss", so a [`StreamUp::Subscribe`] may carry a `class` and a [`StreamUp::Delivery`] changes it on a
//!   live subscription. A raised `l2_snapshot` is the seventh lossless verb ([`StreamGapVerb::Depth`]):
//!   queued by the lossless rule, a loss owed as a [`StreamDown::Gap`]. The sender tells the receiver
//!   WHERE in the frame order the class changed with [`StreamDown::Delivery`], so the receiver checks
//!   `seq` continuity exactly from there and never before. The verb table is
//!   `vike_data::delivery`'s; the feature string that gates all of it is the hosting protocol's
//!   (the gatewayhub's `stream_class`, never a version bump).
//! * **Back-pressure** never blocks a venue pump: an overflow is a [`StreamDown::Gap`] before the
//!   next surviving frame of the key, or [`StreamDown::Bye`]`(TooSlow)` and a closed socket after
//!   which the receiver reconnects and replays its held set. Nothing is ever dropped silently.
//!
//! The wire's bytes are pinned by the committed goldens in `fixtures/stream_wire/`, read by
//! `crates/vike-market-stream/tests/stream_tag_fixtures.rs`. A session that sent [`StreamUp::BinaryTicks`]
//! gets its per-tick frames as binary bodies instead (`crate::binary_ticks`, the same values).

use serde::{Deserialize, Serialize};
use vike_data::DataClientError;
use vike_data::delivery::DataClientSinkDelivery;
use vike_marketdata::{Bar, FundingRate, L2Book, L2BookLevel, L2BookUpdate, TickQuote, TickTrade};
use vike_model::{StreamDelay, StreamLabel, StreamStatus};
use vike_venues::link_deadman::TICK_LINK_STREAM;

/// A client frame on a `stream` socket, after the hosting protocol's open reply (the gatewayhub's
/// `Response::DataOpened`). Small by construction: the daemon reads them under its up-frame ceiling
/// (`GATEWAY_UP_FRAME_CEILING_BYTES`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum StreamUp {
    /// Start one stream. `sid` is allocated by the CLIENT, so the call is idempotent: re-sending the
    /// same `sid` (a reconnect replaying its held set) yields the same answer, and the daemon dedups
    /// on it. Answered by [`StreamDown::SubscribeOk`] or [`StreamDown::SubscribeError`] with the same `sid`.
    Subscribe {
        /// The client's subscription id (`DataClientSubscriptionId.0`), unique per client.
        sid: u64,
        /// Which `DataClient::subscribe_*` call.
        op: SubscriptionOperation,
        /// The delivery class floor the subscriber wants for the subscription's lane, `None` for the
        /// verbs' own classes (the default, and what every peer without `stream_class` sees: the
        /// field is then absent from the frame). The class is applied BEFORE the venue is asked, so
        /// even a frame a venue client emits from inside `subscribe_*` is queued under it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        class: Option<DataClientSinkDelivery>,
    },
    /// Change the delivery class floor of the live subscription `sid` names (a recorder joined a depth
    /// key the market-data hub already held, or left it). Keyed like [`StreamUp::Unsubscribe`]: by the
    /// client's own `sid`, which survives a reconnect where the session's `k` does not. An unknown
    /// `sid`, or a subscription with no tick lane, is a no-op; no reply (the receiver learns of the
    /// change in frame order from [`StreamDown::Delivery`]). Only sent to a peer that advertised the
    /// hosting protocol's `stream_class` feature.
    Delivery {
        /// The subscription whose class changes.
        sid: u64,
        /// The new floor.
        class: DataClientSinkDelivery,
    },
    /// Stop exactly the stream `sid` names. Unknown ids are a no-op. No reply.
    Unsubscribe {
        /// The subscription to stop.
        sid: u64,
    },
    /// Raise the stop flags of every client of this session and RETURN AT ONCE — phase one of the
    /// two-phase teardown (`DataClient::begin_shutdown`). The session stays usable afterwards, exactly
    /// as an in-process client does: the datahub's broker may subscribe again once its drain ends.
    BeginShutdown,
    /// Stop everything and close (`DataClient::shutdown`). The daemon answers
    /// [`StreamDown::Bye`]`(ClientClosed)` and closes the socket.
    Close,
    /// Liveness probe; the daemon answers with a [`StreamDown::Heartbeat`] at once.
    Ping,
    /// Send this session the venue's FUNDING frames ([`StreamDown::Funding`]). Sent once, right after the
    /// open and before any `Subscribe`, and only to a peer whose hosting protocol advertised the funding
    /// feature ([`crate::opener::StreamConnection::funding`]): a session that never sends it is sent no funding
    /// frame, which is what keeps a subscriber that cannot decode one whole (decision 0112). No reply.
    Funding,
    /// Send this session's per-tick frames as BINARY bodies (`crate::binary_ticks`). Sent once, right after the
    /// open and before any `Subscribe`, and only to a peer whose hosting protocol advertised it
    /// ([`crate::opener::StreamConnection::binary_ticks`]): a session that never sends it is sent JSON only,
    /// which is what keeps an older subscriber whole (decision 0112). No reply; the receiver reads either
    /// body on every frame, so the frames queued before the switch need no marker.
    BinaryTicks,
}

/// Which `DataClient::subscribe_*` verb a [`StreamUp::Subscribe`] is. One variant per verb — the variant IS
/// the lane, so a conflated depth subscription can never be mistaken for the lossless book.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SubscriptionOperation {
    /// `subscribe_bars(symbol, interval)`.
    Bars {
        /// The venue's own symbol spelling.
        symbol: String,
        /// The bar interval.
        interval: String,
    },
    /// `subscribe_quotes(symbol)`.
    Quotes {
        /// The venue's own symbol spelling.
        symbol: String,
    },
    /// `subscribe_trades(symbol)`.
    Trades {
        /// The venue's own symbol spelling.
        symbol: String,
    },
    /// `subscribe_book(symbol)` — the LOSSLESS L2 lane.
    Book {
        /// The venue's own symbol spelling.
        symbol: String,
    },
    /// `subscribe_depth(symbol)` — the CONFLATING L2 lane.
    Depth {
        /// The venue's own symbol spelling.
        symbol: String,
    },
    /// The CEX tick PUMP (`spawn_*_market_data`): one subscription that carries a symbol's `quote`,
    /// `trade` and `book` frames at the pump's own granularity, sized to the CALLER's price grid. It is
    /// not a `DataClient` verb (`vike_data::DataClient` has a lane per label and no tick verb, and is a
    /// measured crate, so it is not edited): the tube's client answers it with the inherent
    /// [`crate::market::DataClientStream::subscribe_ticks`] and a sender hosts the pump behind it. Its
    /// link-health word is the [`StreamWordWire::Link`] `"ticks"` word, not a lane label.
    ///
    /// ⚠ `tick_size` is NAMED BY THE CLIENT (a maker sizes the book grid with its own number) and so is
    /// capped by the sender: [`validate_tick_size`] is the one rule, finite and inside
    /// `TICK_SIZE_FLOOR <= tick_size <= TICK_SIZE_CEILING`.
    Ticks {
        /// The venue's own symbol spelling.
        symbol: String,
        /// The price grid the book is sized to; see [`validate_tick_size`].
        tick_size: f64,
    },
}

/// The largest `tick_size` a [`SubscriptionOperation::Ticks`] may name: a SERVER POLICY start value, far above any real
/// price grid (a whole-dollar grid is `1.0`; the coarsest instrument in the rosters is well under a
/// thousand) and far below the point where the pump's book would be one level. A client that names
/// more is refused by name, never clamped: a clamped grid is a different book than the one asked for.
pub const TICK_SIZE_CEILING: f64 = 1.0e6;

/// The smallest `tick_size` a [`SubscriptionOperation::Ticks`] may name: the same kind of start value, far below any real
/// price grid (the finest instrument in the rosters trades at 1e-8) and far above the point where a price
/// divided by the grid saturates the book's integer tick (`L2Book::tick_at` is a saturating `as i64`: a
/// subnormal grid such as 5e-324 collapsed every level onto `i64::MAX` and priced the book near 1e-305).
/// Refused by name, never clamped, like the ceiling.
pub const TICK_SIZE_FLOOR: f64 = 1.0e-12;

/// Whether `tick_size` is a grid a pump may be sized to: finite (a non-finite `f64` is `null` on this
/// JSON wire and would read back as a different frame), strictly positive, and at most
/// [`TICK_SIZE_FLOOR`] and at most [`TICK_SIZE_CEILING`]. `Err` is the sentence a refusal carries. The ONE rule: the client checks it
/// before framing and the sender checks it again before the pump is asked.
pub fn validate_tick_size(tick_size: f64) -> Result<f64, String> {
    if !tick_size.is_finite() {
        return Err("the tick size is not a finite number".to_string());
    }
    if tick_size <= 0.0 {
        return Err(format!("the tick size {tick_size} is not positive"));
    }
    if tick_size < TICK_SIZE_FLOOR {
        return Err(format!(
            "the tick size {tick_size} is under the {TICK_SIZE_FLOOR} floor a tick pump is sized to"
        ));
    }
    if tick_size > TICK_SIZE_CEILING {
        return Err(format!(
            "the tick size {tick_size} is over the {TICK_SIZE_CEILING} ceiling a tick pump is sized to"
        ));
    }
    Ok(tick_size)
}

impl SubscriptionOperation {
    /// The venue's own symbol spelling this subscription names.
    pub fn symbol(&self) -> &str {
        match self {
            SubscriptionOperation::Bars { symbol, .. }
            | SubscriptionOperation::Quotes { symbol }
            | SubscriptionOperation::Trades { symbol }
            | SubscriptionOperation::Book { symbol }
            | SubscriptionOperation::Depth { symbol }
            | SubscriptionOperation::Ticks { symbol, .. } => symbol,
        }
    }

    /// The `stream` word a status disclosure for this subscription carries: the interval for a bar
    /// lane, the [`StreamLabel`] word for a tick lane, the pump's `"ticks"` link word for
    /// [`SubscriptionOperation::Ticks`]. Exhaustive.
    pub fn stream(&self) -> StreamWordWire {
        match self {
            SubscriptionOperation::Bars { interval, .. } => StreamWordWire::Bar(interval.clone()),
            SubscriptionOperation::Quotes { .. } => StreamWordWire::Lane(StreamLabel::Quotes),
            SubscriptionOperation::Trades { .. } => StreamWordWire::Lane(StreamLabel::Trades),
            SubscriptionOperation::Book { .. } => StreamWordWire::Lane(StreamLabel::Book),
            SubscriptionOperation::Depth { .. } => StreamWordWire::Lane(StreamLabel::Depth),
            SubscriptionOperation::Ticks { .. } => StreamWordWire::Link,
        }
    }

    /// The tick lane this subscription is on, `None` for a bar lane (a bar lane's words are
    /// intervals, and its class is the verbs' own: only a tick lane has a floor to raise) and for the
    /// pump, whose frames are quotes, trades and books at the verbs' own classes.
    pub fn lane(&self) -> Option<StreamLabel> {
        match self.stream() {
            StreamWordWire::Lane(lane) => Some(lane),
            StreamWordWire::Bar(_) | StreamWordWire::Link => None,
        }
    }

    /// Why a hub refuses this subscription's symbol or interval before anything is asked, or `None`: a
    /// blank value, one over the HOST's `max_bytes` (each hosting protocol's own ceiling), or a control
    /// character. The ONE rule of the hubs; a client's framing limit is its own.
    pub fn refuse_spelling(&self, max_bytes: usize) -> Option<String> {
        let mut fields: Vec<(&str, &str)> = vec![("symbol", self.symbol())];
        if let SubscriptionOperation::Bars { interval, .. } = self {
            fields.push(("interval", interval));
        }
        for (what, value) in fields {
            if value.trim().is_empty() {
                return Some(format!("the {what} is blank"));
            }
            if value.len() > max_bytes {
                return Some(format!(
                    "the {what} is {} bytes, over the {max_bytes}-byte limit",
                    value.len()
                ));
            }
            if value.chars().any(char::is_control) {
                return Some(format!("the {what} carries a control character"));
            }
        }
        None
    }
}

/// Why one subscription was not started — the in-process `DataClientError` variants, with the daemon's
/// own reasons added.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StreamSubscriptionErrorKind {
    /// `DataClientError::Unsupported`: the venue's client serves no such verb. The text is the
    /// client's own `&'static str`, forwarded verbatim; the CLIENT maps it back through
    /// [`crate::intern`] (the same canonical string, without a leak).
    Unsupported(String),
    /// `DataClientError::Subscribe`: the in-process call returned an error, text verbatim.
    Subscribe(String),
    /// The symbol is not one the daemon will carry (blank, over-long, a control byte). Text verbatim.
    SymbolRejected(String),
    /// A daemon budget or cap is full: transient, the caller's retry cadence is the remedy.
    Budget {
        /// Which budget or cap (a name, never a value).
        scope: String,
        /// How many are held.
        held: u32,
        /// The cap.
        cap: u32,
    },
    /// The session is draining after a [`StreamUp::BeginShutdown`]: transient.
    Draining,
    /// `DataClientError::NotNow`: nothing was started and the caller's next pass may start it (a link that is
    /// down behind the daemon, a join still in progress). Text verbatim. Sent only to a subscriber that said it
    /// reads it (`vike_hub_protocol::features::FEATURE_REFUSAL_FRAME`); an older one is sent a `Subscribe` worded
    /// as it reads one (`crate::legacy`).
    NotNow(String),
    /// `DataClientError::Cap`: a cap behind the daemon is full (a pump, session or stream cap) and only an
    /// operator mends it. Text verbatim; sent as [`StreamSubscriptionErrorKind::NotNow`] is.
    Cap(String),
}

impl StreamSubscriptionErrorKind {
    /// Whether retrying the SAME subscription can never help, so a client replaying it after a lost
    /// link may stop trying: the venue's client serves no such verb ([`StreamSubscriptionErrorKind::Unsupported`]) or
    /// the daemon will not carry the spelling ([`StreamSubscriptionErrorKind::SymbolRejected`]). Everything else is
    /// retried at the client's reconnect backoff: a venue start failure
    /// ([`StreamSubscriptionErrorKind::Subscribe`]: a refused thread, a venue that was briefly unreachable, a symbol
    /// the venue's list has not caught up with), a full budget ([`StreamSubscriptionErrorKind::Budget`]), a draining
    /// session ([`StreamSubscriptionErrorKind::Draining`]), a not-now ([`StreamSubscriptionErrorKind::NotNow`]) and a
    /// cap ([`StreamSubscriptionErrorKind::Cap`]: a place may free up, and a replay that waits for it costs one
    /// frame per backoff). EXHAUSTIVE: a new refusal must say which it is.
    pub fn is_permanent(&self) -> bool {
        match self {
            StreamSubscriptionErrorKind::Unsupported(_)
            | StreamSubscriptionErrorKind::SymbolRejected(_) => true,
            StreamSubscriptionErrorKind::Subscribe(_)
            | StreamSubscriptionErrorKind::Budget { .. }
            | StreamSubscriptionErrorKind::Draining
            | StreamSubscriptionErrorKind::NotNow(_)
            | StreamSubscriptionErrorKind::Cap(_) => false,
        }
    }

    /// The wire form of an in-process start failure — the daemon's half of
    /// [`StreamSubscriptionErrorKind::into_live_error`]. Each `DataClientError` keeps its CLASS (`NotNow` and `Cap`
    /// cross as themselves, so the datahub forwarding its gatewayhub's refusal to a tradehub keeps it) and its text
    /// verbatim; a variant this crate does not know (the enum is `#[non_exhaustive]`) crosses as a `Subscribe`
    /// carrying its `Display`, so a new variant is a message and never a dropped refusal.
    pub fn from_live_error(error: &DataClientError) -> Self {
        match error {
            DataClientError::Unsupported(reason) => {
                StreamSubscriptionErrorKind::Unsupported((*reason).to_string())
            }
            DataClientError::Subscribe(text) => {
                StreamSubscriptionErrorKind::Subscribe(text.clone())
            }
            DataClientError::NotNow(text) => StreamSubscriptionErrorKind::NotNow(text.clone()),
            DataClientError::Cap(text) => StreamSubscriptionErrorKind::Cap(text.clone()),
            other => StreamSubscriptionErrorKind::Subscribe(other.to_string()),
        }
    }

    /// The in-process error a daemon refusal is, its class kept and its text unchanged — the client's half. The
    /// `&'static str` of `Unsupported` comes from [`crate::intern`] (the canonical reasons without a leak, anything
    /// else leaked once into a bounded set); `SymbolRejected` is a `Subscribe` with the daemon's words; a full
    /// budget and a draining session are `NotNow`, worded under `peer`: the daemon that refused
    /// (`crate::opener::StreamOpener::peer`), since the datahub's strategy lane sends them too. A `Subscribe` from a
    /// daemon that predates the classes is classed by its words, in `crate::legacy` alone.
    pub fn into_live_error(self, peer: &str) -> DataClientError {
        match self {
            StreamSubscriptionErrorKind::Unsupported(text) => {
                DataClientError::Unsupported(crate::intern::unsupported_reason(&text))
            }
            StreamSubscriptionErrorKind::Subscribe(text) => crate::legacy::classed(text),
            StreamSubscriptionErrorKind::SymbolRejected(text) => DataClientError::Subscribe(text),
            StreamSubscriptionErrorKind::Budget { scope, held, cap } => {
                DataClientError::NotNow(format!("{peer} budget: {scope} holds {held} of {cap}"))
            }
            StreamSubscriptionErrorKind::Draining => DataClientError::NotNow(format!(
                "{peer}: the session is draining after a begin_shutdown"
            )),
            StreamSubscriptionErrorKind::NotNow(text) => DataClientError::NotNow(text),
            StreamSubscriptionErrorKind::Cap(text) => DataClientError::Cap(text),
        }
    }
}

/// A server frame on a `stream` socket, after the hosting protocol's open reply (the gatewayhub's
/// `Response::DataOpened`). See the module doc for the thirteen framed sink verbs, `k`, `seq` and the back-pressure rule.
///
/// ⚠ **`Seed`, `BarClose`, `Quote`, `Trade`, `Book` and `BookDelta` are LOSSLESS** ([`StreamGapVerb`]) — a
/// missed one is a hole downstream — and `Forming`, `Mark`, `BarCloseTick`, `Depth` and `Funding` are
/// CONFLATABLE (latest-wins by the sink's own contract) - except that a subscriber may raise `Depth` to
/// lossless ([`DataClientSinkDelivery`], told in frame order by [`StreamDown::Delivery`]), when its `seq` is
/// continuous and a loss is a [`StreamDown::Gap`] naming [`StreamGapVerb::Depth`]. `Status`, `Delay`,
/// `DataClientSinkDelivery`, `Gap`, `Heartbeat` and `Bye` ride a reserved control lane and are never evicted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StreamDown {
    /// Reply to [`StreamUp::Subscribe`]: the stream started (the in-process `subscribe_*` returned) and
    /// frames for the symbol carry key `k`. Sent BEFORE any frame carrying that `k`. A second
    /// subscription on the same symbol MAY return the same `k`.
    SubscribeOk {
        /// The `sid` of the `Subscribe`.
        sid: u64,
        /// The session-assigned key id for the subscription's symbol.
        k: u32,
    },
    /// Reply to [`StreamUp::Subscribe`]: the stream did not start.
    SubscribeError {
        /// The `sid` of the `Subscribe`.
        sid: u64,
        /// Why.
        err: StreamSubscriptionErrorKind,
    },
    /// Bind key `k` to `symbol` without a subscription: the escape for a feed that emits a symbol
    /// spelling it was not asked for. A `k` already bound to a different symbol is a protocol fault.
    Key {
        /// The key id.
        k: u32,
        /// The venue's own symbol spelling.
        symbol: String,
    },
    /// `DataClientSink::seed_bars`. LOSSLESS.
    Seed {
        /// The key.
        k: u32,
        /// The `(k, Seed)` sequence number.
        sequence: u64,
        /// The bar interval.
        interval: String,
        /// The seed, ts-ascending.
        bars: Vec<Bar>,
    },
    /// `DataClientSink::close_bar`. LOSSLESS. (Named apart from [`StreamUp::Close`], which stops the
    /// session.)
    BarClose {
        /// The key.
        k: u32,
        /// The `(k, BarClose)` sequence number.
        sequence: u64,
        /// The bar interval.
        interval: String,
        /// The closed bar.
        bar: Bar,
    },
    /// `DataClientSink::forming_bar`. CONFLATABLE.
    Forming {
        /// The key.
        k: u32,
        /// The `(k, Forming)` sequence number (jumps are not a loss).
        sequence: u64,
        /// The bar interval.
        interval: String,
        /// The forming bar's latest snapshot.
        bar: Bar,
    },
    /// `DataClientSink::mark_tick` — a venue-published mark/index price. CONFLATABLE.
    Mark {
        /// The key.
        k: u32,
        /// The `(k, Mark)` sequence number (jumps are not a loss).
        sequence: u64,
        /// The mark price.
        price: f64,
        /// The stamp, epoch-ms.
        timestamp: i64,
    },
    /// `DataClientSink::bar_close_tick` — a kline feed's candle-close snapshot. CONFLATABLE.
    BarCloseTick {
        /// The key.
        k: u32,
        /// The `(k, BarCloseTick)` sequence number (jumps are not a loss).
        sequence: u64,
        /// The candle-close price.
        price: f64,
        /// The stamp, epoch-ms.
        timestamp: i64,
    },
    /// `DataClientSink::l2_snapshot` — a depth snapshot. CONFLATABLE, unless the subscriber raised the
    /// lane ([`StreamDown::Delivery`]): then LOSSLESS like `Quote`.
    Depth {
        /// The key.
        k: u32,
        /// The `(k, Depth)` sequence number (jumps are not a loss while the lane is conflatable).
        sequence: u64,
        /// The feed's authoritative price grid; `0.0` means the feed did not know.
        tick_size: f64,
        /// Bids, best first, as the feed passed them.
        bids: Vec<L2BookLevel>,
        /// Asks, best first, as the feed passed them.
        asks: Vec<L2BookLevel>,
        /// The venue/receipt stamp, epoch-ms.
        timestamp: i64,
    },
    /// `DataClientSink::quote`. LOSSLESS.
    Quote {
        /// The key.
        k: u32,
        /// The `(k, Quote)` sequence number.
        sequence: u64,
        /// The tick, `symbol` field exactly as the feed set it.
        quote: TickQuote,
    },
    /// `DataClientSink::trade`. LOSSLESS.
    Trade {
        /// The key.
        k: u32,
        /// The `(k, Trade)` sequence number.
        sequence: u64,
        /// The print, `symbol` field exactly as the feed set it.
        trade: TickTrade,
    },
    /// `DataClientSink::book` — the folded L2 state, whole. LOSSLESS. BOXED: the largest sink payload.
    Book {
        /// The key.
        k: u32,
        /// The `(k, Book)` sequence number.
        sequence: u64,
        /// The folded book.
        book: Box<L2Book>,
    },
    /// `DataClientSink::book_update` — a raw delta, snapshot anchor or status marker. LOSSLESS.
    BookDelta {
        /// The key.
        k: u32,
        /// The `(k, BookDelta)` sequence number.
        sequence: u64,
        /// The update.
        update: Box<L2BookUpdate>,
    },
    /// `DataClientSink::stream_status`. Control lane.
    Status {
        /// The key.
        k: u32,
        /// Which lane the transition is about.
        stream: StreamWordWire,
        /// The transition, verbatim.
        status: StreamStatus,
    },
    /// `DataClientSink::data_delay`. Control lane.
    Delay {
        /// The key.
        k: u32,
        /// Which lane the label is about.
        stream: StreamWordWire,
        /// The label, verbatim.
        delay: StreamDelay,
    },
    /// `DataClientSink::funding` — the venue's public funding rate and next settlement. CONFLATABLE.
    /// Sent only on a session that asked for it ([`StreamUp::Funding`]).
    Funding {
        /// The key.
        k: u32,
        /// The `(k, Funding)` sequence number (jumps are not a loss).
        sequence: u64,
        /// The observation, `symbol` field exactly as the feed set it.
        funding: FundingRate,
    },
    /// **Frames were LOST and here is how many.** The daemon synthesizes it immediately BEFORE the
    /// next surviving frame of the key (structural ordering): an overflow drops the oldest frames of
    /// a lossless verb, counts them, and owes this marker. The client discloses it to the sink as a
    /// `GapStart` then `Live` on `stream`, **both stamped `from_timestamp_ms`** and never the client's
    /// clock: the recorder files a `GapStart` under its `at_timestamp_ms` and replay orders rows by
    /// `(ts, seq)`, so a marker stamped with the arrival time of a late frame would land AFTER the
    /// survivors it is supposed to precede. SEVERAL holes merge into one marker at the earliest frame
    /// any of them precedes.
    Gap {
        /// The key.
        k: u32,
        /// Which lossless verb's sequence has the hole.
        verb: StreamGapVerb,
        /// The lane the hole is in, so the disclosure names the right stream.
        stream: StreamWordWire,
        /// How many frames were lost.
        dropped: u64,
        /// The `seq` of the last frame delivered before the hole.
        from_sequence: u64,
        /// The `seq` of the first frame delivered after it.
        to_sequence: u64,
        /// **When the hole opens, epoch-ms, on the clock the frames themselves carry**: the stamp of the
        /// last frame delivered before the hole (a bar's `ts`, a quote's, a trade's, a book update's;
        /// the last bar of a seed). With nothing delivered before the hole it is the stamp of the first
        /// frame LOST, and for a verb whose payload carries no stamp (`book`, the folded state) it is
        /// the daemon's receive clock for the frame. Never later than the stamp of the first frame
        /// after the hole for a feed whose stamps do not run backwards.
        from_timestamp_ms: i64,
    },
    /// **The delivery class of lane `lane` of key `k` changed, from this frame on.** Queued in the
    /// control lane's emission order, so every data frame of the lane ahead of it was queued under the
    /// OLD class and every one behind it under `class`: the receiver restarts its `seq` check for the
    /// lane here (superseded frames before the marker are not a loss; a gap after it is). `class` is the
    /// lane's EFFECTIVE class (the verbs' own, or the subscriber's floor if stronger). Sent on every
    /// effective change a [`StreamUp::Subscribe`] `class` or a [`StreamUp::Delivery`] makes, never otherwise.
    Delivery {
        /// The key.
        k: u32,
        /// The lane whose class changed.
        lane: StreamLabel,
        /// The lane's class from here on.
        class: DataClientSinkDelivery,
    },
    /// Written after the session's heartbeat period of silence, and in answer to [`StreamUp::Ping`],
    /// so a quiet stream proves the link is alive rather than merely silent.
    Heartbeat,
    /// The server is ending this stream, and why. The socket closes after it.
    Bye(StreamByeReason),
}

/// Why the daemon is ending a stream or feed. Carried by [`StreamDown::Bye`] and
/// the gatewayhub's `OptionsFeedDown::Bye`, the last frame on that socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StreamByeReason {
    /// The subscriber accumulated more lapses than the daemon's budget inside its window: a client
    /// that cannot keep up is being lied to more slowly. The client reconnects and re-seeds.
    TooSlow {
        /// How many lapses were counted.
        lapses: u64,
    },
    /// The daemon is shutting down.
    ServerStopping,
    /// The session held nothing for long enough that keeping the socket bought nothing.
    SessionIdle,
    /// The reserved CONTROL lane overflowed: the subscriber could not absorb even the status frames
    /// its own keys produced.
    ControlLaneOverflow,
    /// The answer to [`StreamUp::Close`].
    ClientClosed,
}

/// The lossless verbs, the ones a [`StreamDown::Gap`] can name and whose `seq` must be continuous.
/// Exhaustive over the sink: a new lossless verb fails to compile in `market.rs` until decided.
///
/// [`StreamGapVerb::Depth`] is lossless only on a lane a subscriber raised ([`DataClientSinkDelivery`]); on any other
/// it is a conflatable verb and a `Gap` never names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum StreamGapVerb {
    /// `seed_bars`.
    Seed,
    /// `close_bar`.
    BarClose,
    /// `quote`.
    Quote,
    /// `trade`.
    Trade,
    /// `book`.
    Book,
    /// `book_update`.
    BookDelta,
    /// `l2_snapshot` - on a depth lane whose subscriber raised it to lossless (decision 0167, the delivery
    /// class): a recorder's `kind=depth` tape owes a marker for a snapshot it lost, which latest-wins
    /// would not give it.
    Depth,
}

/// A `stream` word on the wire — what a status or delay disclosure's `stream` argument is.
///
/// `DataClientSink::stream_status` takes a `&str` because a bar lane's label is its INTERVAL, while the
/// tick lanes are the four closed [`StreamLabel`] words. The wire carries that split as a type:
/// [`StreamWordWire::Bar`] for an interval, [`StreamWordWire::Lane`] for a label, and [`StreamWordWire::Link`]
/// for the CEX pump's `"ticks"` link word, which is not a [`StreamLabel`]. The two conversions are
/// total and mutually inverse over every string ([`StreamWordWire::from_word`], [`StreamWordWire::word`]), so
/// the word a feed passed is the word the sink receives, bit for bit.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StreamWordWire {
    /// A bar interval (`"1m"`), or any word that is neither a label nor the link word.
    Bar(String),
    /// A tick-lane label. Its tag is [`StreamLabel`]'s own serde tag, the variant name, never the sink
    /// word ([`StreamLabel::word`]).
    Lane(StreamLabel),
    /// The CEX tick pump's link-health word ([`TICK_LINK_STREAM`]).
    Link,
}

impl StreamWordWire {
    /// The wire form of the `stream` argument a feed passed to the sink. Total.
    pub fn from_word(word: &str) -> Self {
        if let Some(label) = StreamLabel::from_word(word) {
            return StreamWordWire::Lane(label);
        }
        if word == TICK_LINK_STREAM {
            return StreamWordWire::Link;
        }
        StreamWordWire::Bar(word.to_string())
    }

    /// The `stream` argument a sink receives. Total, and the inverse of [`StreamWordWire::from_word`].
    pub fn word(&self) -> &str {
        match self {
            StreamWordWire::Bar(interval) => interval,
            StreamWordWire::Lane(lane) => lane.word(),
            StreamWordWire::Link => TICK_LINK_STREAM,
        }
    }
}

#[cfg(test)]
#[path = "wire_tests.rs"]
mod wire_tests;
