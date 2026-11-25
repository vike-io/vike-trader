//! The per-session BOUNDED outgoing queue, and the overflow policy that makes a laggard's losses honest.
//!
//! # Why this is not the market data mailbox
//!
//! `crates/vike-datahub/src/market_data/mailbox.rs`'s `MarketDataMailbox` is the model. It was re-implemented here, not
//! imported, because both crates were layer 65 and `crates/vike-ops/tests/architecture/layer_gate.rs`
//! refuses an edge between siblings; the queue now sits below both (the data-plane tier, decision 0167).
//! The mailbox does NOT move onto it: the twin test (`crates/vike-datahub/src/market_data/mailbox_twin_tests.rs`)
//! shows the two policies differ, and the owner ruled on 2026-11-15 to keep the mailbox its own queue.
//! The shape is borrowed (bytes first and frames second, a reserved control lane that is never
//! evicted, supersede only where the contract says it is not a loss, a gap synthesized by the WRITER
//! immediately before the next post-hole frame) and the policy is this wire's own: the
//! lossless / conflatable / control split the sink's documented contracts draw
//! (`crates/vike-data/src/live.rs`'s `DataClientSink`).
//!
//! # The three classes, and what each does on overflow
//!
//! | class | verbs | on overflow |
//! |---|---|---|
//! | lossless | `Seed`, `BarClose`, `Quote`, `Trade`, `Book`, `BookDelta` (and `Depth` on a lane a subscriber raised) | **evict the OLDEST queued frame of the pushing key** (the key that is filling the queue pays) when it has older frames queued, else the globally oldest lossless frame; a frame bigger than the whole bound is refused alone; every eviction is counted and OWES a `Gap` |
//! | conflatable | `Forming`, `Mark`, `BarCloseTick`, `Funding`, `Depth` (unless raised) | **supersede the unsent frame of that slot** - by the sink's own contract latest-wins is not a loss. The slot is `(k, verb)` and, for `Forming`, the bar INTERVAL as well: a forming bar is latest-wins per `(symbol, interval)`, and one symbol key carries every interval, so a 5m snapshot must never supersede an unsent 1m one |
//! | control | `SubscribeOk`, `SubscribeError`, `Key`, `Status`, `Delay`, `DataClientSinkDelivery` | **never evicted**; a separate bound; if even it cannot be filled the session is closed (`ControlLaneOverflow`) |
//!
//! `Gap` and `Heartbeat` and `Bye` are not queued at all: the writer synthesizes them.
//!
//! # A subscriber can RAISE a class (`Depth`), and a raise is the lossless rule whole
//!
//! The table above is each verb's OWN class (`vike_data::delivery`'s, read through [`StreamQueueVerb::base`]).
//! A subscriber that cannot take a superseded frame (a recorder writing `l2_snapshot` rows into a
//! tape) declares a FLOOR for a key's lane ([`StreamQueue::set_floor`]); the class a `(k, verb)` runs at is
//! the stronger of its own and the floor, and only a verb with a wire name ([`StreamQueueVerb::gap_verb`]) can be
//! raised. Today that is `Depth`: with its floor at lossless a depth frame is queued, evicted, numbered
//! and disclosed exactly as a `Quote` is. ⚠ **Not "stop superseding"**: a depth frame that survives but
//! is still conflatable by position would make room by evicting lossless quotes and trades, so the
//! raised verb joins the lossless index, the ledger and the `Gap` machinery in full.
//!
//! The class is decided PER FRAME when it is pushed and travels with the entry (`Entry::lossless`), so a
//! change never re-files a frame already queued: a conflatable depth frame queued before a raise is
//! still superseded by a later conflatable one and still counted as such, a lossless one queued before
//! a relax is still evictable with a `Gap`. The change itself is announced in the control lane's
//! emission order ([`StreamQueueControl::Delivery`]), which is what lets the receiver restart its `seq` check at
//! exactly the frame where the rule changed.
//!
//! # `seq` is assigned BEFORE any drop decision
//!
//! Every data frame takes the next number of its `(k, verb)` sequence the instant it is pushed, so a
//! hole in the numbers IS the loss, and nothing downstream can renumber it away. Sequences start at 1,
//! so `0` is "nothing delivered yet". The six lossless verbs are continuous on the wire except across a
//! `Gap`; the four conflatable verbs may skip, which is not a loss.
//!
//! # A `Gap` rides the first POST-HOLE frame, and the tail is flushed
//!
//! An eviction records a hole in a per-`(k, verb)` ledger, not a queued frame, so the marker lands
//! exactly between the last frame delivered before the hole and the first surviving frame after it -
//! never ahead of queued frames that precede the hole (the `CTRL`-lane defect the market data mailbox's module doc
//! rejects). A hole with NO surviving frame after it (the newest frames were the ones dropped) is
//! disclosed as soon as the queue has nothing else for that `(k, verb)`: `StreamQueue::tail_gaps` hands the
//! writer a `Gap` whose `to_sequence` is the number the NEXT frame will carry, so the consumer learns of the
//! loss when the writer goes idle and not only when more data happens to arrive.
//!
//! # A `Gap` carries WHEN the hole opens
//!
//! The consumer files its `GapStart` under a time, and replay orders rows by `(ts, seq)`
//! (`crates/vike-store/src/datafusion_hist/codec/book.rs`'s `sort_key`; the recorder's marker has `seq`
//! 0 and lands under `at_timestamp_ms`). A marker stamped with the CLIENT's arrival clock would sort after
//! every survivor still queued behind a slow reader and un-anchor the replayed book too late. So the
//! ledger remembers the stamp of every frame it delivers and the first lost frame's stamp, and a `Gap`
//! names `from_timestamp_ms`: the last delivered frame's stamp, or with none delivered the first lost
//! frame's. A payload with no stamp of its own (`StreamQueuePayload::Book`) is stamped with the daemon's clock
//! when it is pushed.
//!
//! # One ordered sequence, and a held key
//!
//! The classes share one emission order (a stamp per entry), so a `Status` is never delivered ahead of
//! the data queued before it and a superseding frame takes the POSITION of its own emission (a
//! `Forming` snapshot of bar t+1 never overtakes the `BarClose` of bar t). The one exception is the
//! HELD key: a key a `Subscribe` is still starting ([`StreamQueue::hold`]) has its frames queued but not
//! delivered until the writer dequeues that key's `SubscribeOk`, which is what makes "`SubscribeOk` before any frame
//! carrying that `k`" true even when a venue client emits a seed from inside `subscribe_*`.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;

use vike_data::delivery::{DataClientSinkDelivery, DataClientSinkVerb, lane_verbs, stronger};
use vike_marketdata::{Bar, FundingRate, L2Book, L2BookLevel, L2BookUpdate, TickQuote, TickTrade};
use vike_model::{StreamDelay, StreamLabel, StreamStatus};

use crate::wire::{StreamDown, StreamGapVerb, StreamSubscriptionErrorKind, StreamWordWire};

/// The eleven sequenced data verbs of a stream (the thirteen framed sink verbs less `stream_status` and
/// `data_delay`, which ride the control lane; `top_of_book_tick` is never framed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum StreamQueueVerb {
    /// `seed_bars`. Lossless.
    Seed,
    /// `close_bar`. Lossless.
    BarClose,
    /// `forming_bar`. Conflatable.
    Forming,
    /// `mark_tick`. Conflatable.
    Mark,
    /// `bar_close_tick`. Conflatable.
    BarCloseTick,
    /// `l2_snapshot`. Conflatable, and lossless on a lane a subscriber raised ([`StreamQueue::set_floor`]).
    Depth,
    /// `quote`. Lossless.
    Quote,
    /// `trade`. Lossless.
    Trade,
    /// `book`. Lossless.
    Book,
    /// `book_update`. Lossless.
    BookDelta,
    /// `funding`. Conflatable.
    Funding,
}

impl StreamQueueVerb {
    /// Every verb that CAN be lossless: the six whose own class is, and `Depth`, which a subscriber may
    /// raise. Exactly the verbs [`StreamQueueVerb::gap_verb`] names.
    pub const GAPPABLE: [StreamQueueVerb; 7] = [
        StreamQueueVerb::Seed,
        StreamQueueVerb::BarClose,
        StreamQueueVerb::Quote,
        StreamQueueVerb::Trade,
        StreamQueueVerb::Book,
        StreamQueueVerb::BookDelta,
        StreamQueueVerb::Depth,
    ];

    /// The sink verb this queue verb is a frame of. Exhaustive: a queue verb is a role in the
    /// `vike_data::delivery` table, and the table is where its class is read from.
    pub const fn sink_verb(self) -> DataClientSinkVerb {
        match self {
            StreamQueueVerb::Seed => DataClientSinkVerb::SeedBars,
            StreamQueueVerb::BarClose => DataClientSinkVerb::CloseBar,
            StreamQueueVerb::Forming => DataClientSinkVerb::FormingBar,
            StreamQueueVerb::Mark => DataClientSinkVerb::MarkTick,
            StreamQueueVerb::BarCloseTick => DataClientSinkVerb::BarCloseTick,
            StreamQueueVerb::Depth => DataClientSinkVerb::L2Snapshot,
            StreamQueueVerb::Quote => DataClientSinkVerb::Quote,
            StreamQueueVerb::Trade => DataClientSinkVerb::Trade,
            StreamQueueVerb::Book => DataClientSinkVerb::Book,
            StreamQueueVerb::BookDelta => DataClientSinkVerb::BookUpdate,
            StreamQueueVerb::Funding => DataClientSinkVerb::Funding,
        }
    }

    /// The queue verb of a sink verb, `None` for the two control verbs (`stream_status`, `data_delay`),
    /// which are not sequenced data frames, and for `top_of_book_tick`, which no tube carries. Exhaustive.
    pub const fn from_sink_verb(verb: DataClientSinkVerb) -> Option<StreamQueueVerb> {
        match verb {
            DataClientSinkVerb::SeedBars => Some(StreamQueueVerb::Seed),
            DataClientSinkVerb::CloseBar => Some(StreamQueueVerb::BarClose),
            DataClientSinkVerb::FormingBar => Some(StreamQueueVerb::Forming),
            DataClientSinkVerb::MarkTick => Some(StreamQueueVerb::Mark),
            DataClientSinkVerb::BarCloseTick => Some(StreamQueueVerb::BarCloseTick),
            DataClientSinkVerb::L2Snapshot => Some(StreamQueueVerb::Depth),
            DataClientSinkVerb::Quote => Some(StreamQueueVerb::Quote),
            DataClientSinkVerb::Trade => Some(StreamQueueVerb::Trade),
            DataClientSinkVerb::Book => Some(StreamQueueVerb::Book),
            DataClientSinkVerb::BookUpdate => Some(StreamQueueVerb::BookDelta),
            DataClientSinkVerb::Funding => Some(StreamQueueVerb::Funding),
            DataClientSinkVerb::StreamStatus | DataClientSinkVerb::DataDelay => None,
            // The tradehub price tap's in-process top of book (decision 0208's amendment): never a tube frame.
            DataClientSinkVerb::TopOfBookTick => None,
        }
    }

    /// The verb's own class, before any subscriber's floor: `vike_data::delivery`'s table, the one
    /// every tube reads.
    pub const fn base(self) -> DataClientSinkDelivery {
        self.sink_verb().base()
    }

    /// The wire's name for a verb that can be lossless, `None` for one that never is. Exhaustive.
    /// ⚠ `Some` does not say the verb IS lossless on a given key (`Depth` is not, until a subscriber
    /// raises it): the class of a key is [`StreamQueue::is_lossless`].
    pub const fn gap_verb(self) -> Option<StreamGapVerb> {
        match self {
            StreamQueueVerb::Seed => Some(StreamGapVerb::Seed),
            StreamQueueVerb::BarClose => Some(StreamGapVerb::BarClose),
            StreamQueueVerb::Quote => Some(StreamGapVerb::Quote),
            StreamQueueVerb::Trade => Some(StreamGapVerb::Trade),
            StreamQueueVerb::Book => Some(StreamGapVerb::Book),
            StreamQueueVerb::BookDelta => Some(StreamGapVerb::BookDelta),
            StreamQueueVerb::Depth => Some(StreamGapVerb::Depth),
            StreamQueueVerb::Forming
            | StreamQueueVerb::Mark
            | StreamQueueVerb::BarCloseTick
            | StreamQueueVerb::Funding => None,
        }
    }
}

/// What a data frame carries, kept TYPED in the queue so the writer thread (not the venue pump)
/// builds and serializes the frame.
#[derive(Debug)]
pub enum StreamQueuePayload {
    /// `seed_bars`.
    Seed {
        /// The bar interval.
        interval: String,
        /// The seed, ts-ascending.
        bars: Vec<Bar>,
    },
    /// `close_bar`.
    BarClose {
        /// The bar interval.
        interval: String,
        /// The closed bar.
        bar: Bar,
    },
    /// `forming_bar`.
    Forming {
        /// The bar interval.
        interval: String,
        /// The latest snapshot.
        bar: Bar,
    },
    /// `mark_tick`.
    Mark {
        /// The mark price.
        price: f64,
        /// The stamp, epoch-ms.
        timestamp: i64,
    },
    /// `bar_close_tick`.
    BarCloseTick {
        /// The candle-close price.
        price: f64,
        /// The stamp, epoch-ms.
        timestamp: i64,
    },
    /// `l2_snapshot`.
    Depth {
        /// The feed's price grid, `0.0` when unknown.
        tick_size: f64,
        /// Bids, best first.
        bids: Vec<L2BookLevel>,
        /// Asks, best first.
        asks: Vec<L2BookLevel>,
        /// The venue/receipt stamp, epoch-ms.
        timestamp: i64,
    },
    /// `quote`.
    Quote(TickQuote),
    /// `trade`.
    Trade(TickTrade),
    /// `book`. Held behind the feed's own `Arc` until the writer clones it into the frame, so the
    /// copy is the writer thread's cost and not the pump's.
    Book(Arc<L2Book>),
    /// `book_update`.
    BookDelta(L2BookUpdate),
    /// `funding`.
    Funding(FundingRate),
}

/// A rough per-frame fixed cost (tags, the key, the sequence number, queue bookkeeping) added to every
/// payload's size. A bound, not a measurement.
const FRAME_OVERHEAD: usize = 96;

impl StreamQueuePayload {
    /// The verb this payload is a frame of.
    pub fn verb(&self) -> StreamQueueVerb {
        match self {
            StreamQueuePayload::Seed { .. } => StreamQueueVerb::Seed,
            StreamQueuePayload::BarClose { .. } => StreamQueueVerb::BarClose,
            StreamQueuePayload::Forming { .. } => StreamQueueVerb::Forming,
            StreamQueuePayload::Mark { .. } => StreamQueueVerb::Mark,
            StreamQueuePayload::BarCloseTick { .. } => StreamQueueVerb::BarCloseTick,
            StreamQueuePayload::Depth { .. } => StreamQueueVerb::Depth,
            StreamQueuePayload::Quote(_) => StreamQueueVerb::Quote,
            StreamQueuePayload::Trade(_) => StreamQueueVerb::Trade,
            StreamQueuePayload::Book(_) => StreamQueueVerb::Book,
            StreamQueuePayload::BookDelta(_) => StreamQueueVerb::BookDelta,
            StreamQueuePayload::Funding(_) => StreamQueueVerb::Funding,
        }
    }

    /// What the queue's byte bound counts this frame as: its in-memory size, not its JSON size. A tick's
    /// symbol is an interned `vike_marketdata::Symbol` (one pointer, no heap of its own), so a quote or a
    /// trade costs its row's size alone.
    pub fn cost(&self) -> usize {
        use std::mem::size_of;
        FRAME_OVERHEAD
            + match self {
                StreamQueuePayload::Seed { interval, bars } => {
                    interval.len() + bars.len() * size_of::<Bar>()
                }
                StreamQueuePayload::BarClose { interval, .. }
                | StreamQueuePayload::Forming { interval, .. } => interval.len() + size_of::<Bar>(),
                StreamQueuePayload::Mark { .. } | StreamQueuePayload::BarCloseTick { .. } => 0,
                StreamQueuePayload::Depth { bids, asks, .. } => {
                    (bids.len() + asks.len()) * size_of::<L2BookLevel>()
                }
                StreamQueuePayload::Quote(_) => size_of::<TickQuote>(),
                StreamQueuePayload::Trade(_) => size_of::<TickTrade>(),
                StreamQueuePayload::Book(b) => {
                    (b.bid_levels() + b.ask_levels()) * (size_of::<L2BookLevel>() + 16)
                }
                StreamQueuePayload::BookDelta(u) => {
                    size_of::<L2BookUpdate>()
                        + (u.bids.len() + u.asks.len()) * size_of::<L2BookLevel>()
                        + u.symbol.len()
                }
                StreamQueuePayload::Funding(f) => size_of::<FundingRate>() + f.symbol.len(),
            }
    }

    /// The stream word a `Gap` over this payload's verb names: the interval for a bar verb, the lane
    /// for a tick verb. (Meaningful for the six lossless verbs.)
    pub fn stream(&self) -> StreamWordWire {
        match self {
            StreamQueuePayload::Seed { interval, .. }
            | StreamQueuePayload::BarClose { interval, .. }
            | StreamQueuePayload::Forming { interval, .. } => StreamWordWire::Bar(interval.clone()),
            StreamQueuePayload::Quote(_) => StreamWordWire::Lane(StreamLabel::Quotes),
            StreamQueuePayload::Trade(_) => StreamWordWire::Lane(StreamLabel::Trades),
            StreamQueuePayload::Book(_) | StreamQueuePayload::BookDelta(_) => {
                StreamWordWire::Lane(StreamLabel::Book)
            }
            StreamQueuePayload::Depth { .. } => StreamWordWire::Lane(StreamLabel::Depth),
            StreamQueuePayload::Mark { .. }
            | StreamQueuePayload::BarCloseTick { .. }
            | StreamQueuePayload::Funding(_) => StreamWordWire::Link,
        }
    }

    /// The stamp (epoch-ms) the frame's own payload carries, `None` for the folded `Book` (an
    /// `L2Book` has no time). A seed is stamped by its newest bar; one with no bars has none.
    pub fn stamp(&self) -> Option<i64> {
        match self {
            StreamQueuePayload::Seed { bars, .. } => bars.last().map(|b| b.timestamp),
            StreamQueuePayload::BarClose { bar, .. } | StreamQueuePayload::Forming { bar, .. } => {
                Some(bar.timestamp)
            }
            StreamQueuePayload::Mark { timestamp: ts, .. }
            | StreamQueuePayload::BarCloseTick { timestamp: ts, .. }
            | StreamQueuePayload::Depth { timestamp: ts, .. } => Some(*ts),
            StreamQueuePayload::Quote(q) => Some(q.timestamp),
            StreamQueuePayload::Trade(t) => Some(t.timestamp),
            StreamQueuePayload::Book(_) => None,
            StreamQueuePayload::BookDelta(u) => Some(u.timestamp),
            StreamQueuePayload::Funding(f) => Some(f.timestamp),
        }
    }

    /// The sub-dimension of this payload's conflation slot: the bar interval for a `Forming` snapshot
    /// (latest-wins per `(symbol, interval)`), `""` for every verb whose slot is just `(k, verb)`.
    /// Exhaustive: a new conflatable verb must say whether its key carries a dimension.
    fn slot_dimension(&self) -> &str {
        match self {
            StreamQueuePayload::Forming { interval, .. } => interval,
            StreamQueuePayload::Seed { .. }
            | StreamQueuePayload::BarClose { .. }
            | StreamQueuePayload::Mark { .. }
            | StreamQueuePayload::BarCloseTick { .. }
            | StreamQueuePayload::Depth { .. }
            | StreamQueuePayload::Quote(_)
            | StreamQueuePayload::Trade(_)
            | StreamQueuePayload::Book(_)
            | StreamQueuePayload::BookDelta(_)
            | StreamQueuePayload::Funding(_) => "",
        }
    }

    /// Whether every float the frame carries is finite. A non-finite `f64` serializes to `null` and
    /// the client cannot decode the frame (it ends the session), so the sink refuses such a row
    /// instead of sending it - and discloses it when it is a lossless one.
    pub fn is_finite(&self) -> bool {
        let bar_ok = |b: &Bar| {
            b.open.is_finite()
                && b.high.is_finite()
                && b.low.is_finite()
                && b.close.is_finite()
                && b.volume.is_finite()
                && b.funding.is_none_or(f64::is_finite)
                && b.bid.is_none_or(f64::is_finite)
                && b.ask.is_none_or(f64::is_finite)
        };
        let levels_ok =
            |l: &[L2BookLevel]| l.iter().all(|x| x.price.is_finite() && x.qty.is_finite());
        match self {
            StreamQueuePayload::Seed { bars, .. } => bars.iter().all(bar_ok),
            StreamQueuePayload::BarClose { bar, .. } | StreamQueuePayload::Forming { bar, .. } => {
                bar_ok(bar)
            }
            StreamQueuePayload::Mark { price: px, .. }
            | StreamQueuePayload::BarCloseTick { price: px, .. } => px.is_finite(),
            StreamQueuePayload::Depth { tick_size, bids, asks, .. } => {
                tick_size.is_finite() && levels_ok(bids) && levels_ok(asks)
            }
            StreamQueuePayload::Quote(q) => {
                q.bid.is_finite()
                    && q.ask.is_finite()
                    && q.bid_size.is_finite()
                    && q.ask_size.is_finite()
            }
            StreamQueuePayload::Trade(t) => t.price.is_finite() && t.size.is_finite(),
            // The whole folded state: the grid AND every resting quantity, read through `top_n` over
            // every level (the book's maps are private to `vike-marketdata`, whose published surface
            // this daemon does not widen: it sits in the latency-measured closure). One non-finite
            // level would serialize as `null`, and a client that cannot decode a frame ends its
            // session, so the book is refused as a hole instead and the NEXT book (the next fold)
            // crosses.
            StreamQueuePayload::Book(b) => {
                let (bids, asks) = b.top_n(b.bid_levels().max(b.ask_levels()));
                b.tick_size.is_finite() && levels_ok(&bids) && levels_ok(&asks)
            }
            StreamQueuePayload::BookDelta(u) => {
                u.tick_size.is_finite() && levels_ok(&u.bids) && levels_ok(&u.asks)
            }
            StreamQueuePayload::Funding(f) => {
                f.rate.is_finite() && f.predicted.is_none_or(f64::is_finite)
            }
        }
    }
}

/// A control frame: carried in order, never evicted.
#[derive(Debug)]
pub enum StreamQueueControl {
    /// The stream started. Dequeuing it RELEASES a held key.
    SubscribeOk {
        /// The client's subscription id.
        sid: u64,
        /// The key the symbol is bound to.
        k: u32,
    },
    /// The stream did not start.
    SubscribeError {
        /// The client's subscription id.
        sid: u64,
        /// Why.
        err: StreamSubscriptionErrorKind,
    },
    /// Bind `k` to `symbol` without a subscription.
    Key {
        /// The key.
        k: u32,
        /// The venue's own spelling.
        symbol: String,
    },
    /// `stream_status`.
    Status {
        /// The key.
        k: u32,
        /// Which lane.
        stream: StreamWordWire,
        /// The transition, verbatim.
        status: StreamStatus,
    },
    /// `data_delay`.
    Delay {
        /// The key.
        k: u32,
        /// Which lane.
        stream: StreamWordWire,
        /// The label, verbatim.
        delay: StreamDelay,
    },
    /// The class of a key's lane changed (a subscriber raised or relaxed it): queued in the control
    /// lane's emission order, so it lands between the frames queued under the old class and the ones
    /// queued under the new ([`StreamDown::Delivery`]).
    Delivery {
        /// The key.
        k: u32,
        /// The lane whose class changed.
        lane: StreamLabel,
        /// The lane's class from here on.
        class: DataClientSinkDelivery,
    },
}

/// One queued frame.
#[derive(Debug)]
pub enum StreamQueueItem {
    /// A sequenced data frame.
    Data {
        /// The key.
        k: u32,
        /// The `(k, verb)` sequence number.
        sequence: u64,
        /// The payload.
        payload: StreamQueuePayload,
    },
    /// A control frame.
    Control(StreamQueueControl),
}

impl StreamQueueItem {
    /// The key a frame carries, for the held-key rule and a purge. `SubscribeOk`, `SubscribeError` and `Key` are
    /// not "frames carrying a key" in that sense: the first releases it and the others bind it.
    fn key(&self) -> Option<u32> {
        match self {
            StreamQueueItem::Data { k, .. } => Some(*k),
            StreamQueueItem::Control(
                StreamQueueControl::Status { k, .. }
                | StreamQueueControl::Delay { k, .. }
                | StreamQueueControl::Delivery { k, .. },
            ) => Some(*k),
            StreamQueueItem::Control(
                StreamQueueControl::SubscribeOk { .. }
                | StreamQueueControl::SubscribeError { .. }
                | StreamQueueControl::Key { .. },
            ) => None,
        }
    }

    /// The wire frame. Done by the WRITER, outside the queue's lock.
    pub fn into_frame(self) -> StreamDown {
        match self {
            StreamQueueItem::Data { k, sequence: seq, payload } => match payload {
                StreamQueuePayload::Seed { interval, bars } => {
                    StreamDown::Seed { k, sequence: seq, interval, bars }
                }
                StreamQueuePayload::BarClose { interval, bar } => {
                    StreamDown::BarClose { k, sequence: seq, interval, bar }
                }
                StreamQueuePayload::Forming { interval, bar } => {
                    StreamDown::Forming { k, sequence: seq, interval, bar }
                }
                StreamQueuePayload::Mark { price: px, timestamp: ts } => {
                    StreamDown::Mark { k, sequence: seq, price: px, timestamp: ts }
                }
                StreamQueuePayload::BarCloseTick { price: px, timestamp: ts } => {
                    StreamDown::BarCloseTick { k, sequence: seq, price: px, timestamp: ts }
                }
                StreamQueuePayload::Depth { tick_size, bids, asks, timestamp: ts } => {
                    StreamDown::Depth { k, sequence: seq, tick_size, bids, asks, timestamp: ts }
                }
                StreamQueuePayload::Quote(quote) => StreamDown::Quote { k, sequence: seq, quote },
                StreamQueuePayload::Trade(trade) => StreamDown::Trade { k, sequence: seq, trade },
                StreamQueuePayload::Book(book) => {
                    StreamDown::Book { k, sequence: seq, book: Box::new((*book).clone()) }
                }
                StreamQueuePayload::BookDelta(update) => {
                    StreamDown::BookDelta { k, sequence: seq, update: Box::new(update) }
                }
                StreamQueuePayload::Funding(funding) => {
                    StreamDown::Funding { k, sequence: seq, funding }
                }
            },
            StreamQueueItem::Control(StreamQueueControl::SubscribeOk { sid, k }) => {
                StreamDown::SubscribeOk { sid, k }
            }
            StreamQueueItem::Control(StreamQueueControl::SubscribeError { sid, err }) => {
                StreamDown::SubscribeError { sid, err }
            }
            StreamQueueItem::Control(StreamQueueControl::Key { k, symbol }) => {
                StreamDown::Key { k, symbol }
            }
            StreamQueueItem::Control(StreamQueueControl::Status { k, stream, status }) => {
                StreamDown::Status { k, stream, status }
            }
            StreamQueueItem::Control(StreamQueueControl::Delay { k, stream, delay }) => {
                StreamDown::Delay { k, stream, delay }
            }
            StreamQueueItem::Control(StreamQueueControl::Delivery { k, lane, class }) => {
                StreamDown::Delivery { k, lane, class }
            }
        }
    }
}

/// The bounds of one queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamQueueLimits {
    /// Bytes of data frames (the first bound).
    pub bytes: usize,
    /// Data frames (the second).
    pub frames: usize,
    /// Control frames (the reserved lane).
    pub control_frames: usize,
}

/// What the queue has done, for the session's closing log line and the tests.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StreamQueueStatistics {
    /// Conflatable frames replaced by a newer one (not a loss).
    pub superseded: u64,
    /// Lossless frames evicted (each owes a disclosure).
    pub lossless_evicted: u64,
    /// Conflatable frames evicted as a last resort, with nothing lossless left to give.
    pub conflatable_evicted: u64,
    /// Frames refused because a float was not finite (lossless ones owe a disclosure).
    pub nonfinite: u64,
    /// Frames the writer could not send (over the frame ceiling); lossless ones owe a disclosure.
    pub unsendable: u64,
}

/// What a push did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamQueuePush {
    /// Queued.
    Queued,
    /// A conflatable frame replaced this slot's unsent one.
    Superseded,
    /// The frame was queued and `n` lossless frames (possibly itself) were evicted to fit.
    Evicted(usize),
    /// The frame carried a non-finite float and was refused (and disclosed, when lossless).
    Refused,
}

/// A contiguous run of lost frames of one `(k, verb)`.
#[derive(Debug)]
struct Hole {
    /// The highest lost `seq` of the run; a surviving frame with a higher `seq` is on its far side.
    last_missing: u64,
    dropped: u64,
    stream: StreamWordWire,
    /// The stamp of the FIRST frame of the run (epoch-ms): where the hole opens when nothing was
    /// delivered before it.
    first_timestamp: i64,
}

#[derive(Debug, Default)]
struct Ledger {
    /// The `seq` of the last frame HANDED to the writer, `None` before the first.
    last_delivered: Option<u64>,
    /// The stamp (epoch-ms) of that frame: where the next hole opens.
    last_delivered_timestamp: Option<i64>,
    /// Holes in ascending `seq` order.
    holes: Vec<Hole>,
    /// Every frame this lane has LOST since the key was bound, delivered holes included: never reset
    /// by a disclosure, so a poller that remembers the last figure it saw learns of a loss the `Gap`
    /// already carried to the wire ([`StreamQueue::lost`]).
    lost: u64,
}

impl Ledger {
    fn add_hole(&mut self, seq: u64, stream: StreamWordWire, ts: i64) {
        self.lost += 1;
        if let Some(last) = self.holes.last_mut()
            && last.last_missing + 1 == seq
            && last.stream == stream
        {
            last.last_missing = seq;
            last.dropped += 1;
            return;
        }
        self.holes.push(Hole { last_missing: seq, dropped: 1, stream, first_timestamp: ts });
    }

    /// Drain the holes that lie before a surviving frame of `seq`.
    fn take_before(&mut self, seq: u64) -> Vec<Hole> {
        let n = self.holes.partition_point(|h| h.last_missing < seq);
        self.holes.drain(..n).collect()
    }
}

/// Where a run of `holes` opens: the stamp of the last frame delivered before them, else the first
/// lost frame's (`from_timestamp_ms` on the wire). `holes` are in ascending `seq` order and never empty here.
fn opens_at(last_delivered_timestamp: Option<i64>, holes: &[Hole]) -> i64 {
    last_delivered_timestamp
        .or_else(|| holes.first().map(|h| h.first_timestamp))
        .unwrap_or_default()
}

/// The `Gap` frames for `holes` (one per distinct stream), spanning `from` (the last frame delivered
/// before them, `0` when none) to `to` (the number of the first frame after them), opening at `from_timestamp`.
fn gaps_for(
    k: u32,
    verb: StreamQueueVerb,
    from: u64,
    to: u64,
    from_timestamp: i64,
    holes: Vec<Hole>,
) -> Vec<StreamDown> {
    let Some(gap_verb) = verb.gap_verb() else { return Vec::new() };
    let mut groups: Vec<(StreamWordWire, u64)> = Vec::new();
    for hole in holes {
        match groups.iter_mut().find(|(s, _)| *s == hole.stream) {
            Some(group) => group.1 += hole.dropped,
            None => groups.push((hole.stream, hole.dropped)),
        }
    }
    groups
        .into_iter()
        .map(|(stream, dropped)| StreamDown::Gap {
            k,
            verb: gap_verb,
            stream,
            dropped,
            from_sequence: from,
            to_sequence: to,
            from_timestamp_ms: from_timestamp,
        })
        .collect()
}

struct Entry {
    item: StreamQueueItem,
    cost: usize,
    /// The frame's stamp (epoch-ms): its payload's own, else the push clock. Meaningful for a
    /// lossless data frame, which is what the ledger tracks.
    timestamp: i64,
    /// Whether the frame was queued under the LOSSLESS rule: its verb's class and the key's floor AT
    /// THE TIME OF THE PUSH (`StreamQueue::is_lossless`). Decided once and carried, so the books (the
    /// lossless index or the conflatable slot) are balanced by the rule the frame entered under even
    /// when the key's floor changed while it waited.
    lossless: bool,
}

/// What the writer needs to take back a frame it turned out unable to send.
#[derive(Debug)]
pub struct StreamQueueUndo {
    k: u32,
    verb: StreamQueueVerb,
    sequence: u64,
    previous: Option<u64>,
    previous_timestamp: Option<i64>,
    stream: StreamWordWire,
    timestamp: i64,
}

/// One dequeued frame, with the gaps that precede it.
#[derive(Debug)]
pub struct StreamQueueTake {
    /// `Gap` frames to write BEFORE `item` (empty almost always).
    pub gaps: Vec<StreamDown>,
    /// The frame.
    pub item: StreamQueueItem,
    /// Present for a lossless data frame: how to un-deliver it if it cannot be written.
    pub undo: Option<StreamQueueUndo>,
}

/// The bounded queue. See the module doc. Not synchronized: the sink wraps it in its own lock.
pub struct StreamQueue {
    limits: StreamQueueLimits,
    stamp: u64,
    entries: BTreeMap<u64, Entry>,
    data_bytes: usize,
    data_frames: usize,
    control_frames: usize,
    /// `(k, verb)` -> `slot dimension` (the bar interval for `Forming`, `""` otherwise) -> the stamp
    /// of that conflatable slot's unsent frame. Nested so the hot lookup borrows the interval
    /// instead of allocating a key.
    slots: HashMap<(u32, StreamQueueVerb), HashMap<String, u64>>,
    /// `(k, verb)` -> the stamps of its queued lossless frames, oldest first.
    lossless: HashMap<(u32, StreamQueueVerb), VecDeque<u64>>,
    /// Every queued lossless stamp, for the globally-oldest eviction.
    lossless_stamps: BTreeSet<u64>,
    /// `(k, verb)` -> the subscriber's class FLOOR where it is stronger than the verb's own (only a
    /// verb with a wire name is ever raised: [`StreamQueue::set_floor`]). Absent = the verb's own class.
    floors: HashMap<(u32, StreamQueueVerb), DataClientSinkDelivery>,
    /// `(k, verb)` -> the next `seq` to assign.
    sequences: HashMap<(u32, StreamQueueVerb), u64>,
    ledger: HashMap<(u32, StreamQueueVerb), Ledger>,
    /// The `(k, verb)` pairs whose ledger holds a hole.
    holey: HashSet<(u32, StreamQueueVerb)>,
    pending: HashSet<u32>,
    /// How many queued entries mention each key (so a released key is freed once it drains).
    queued: HashMap<u32, usize>,
    released: HashSet<u32>,
    stats: StreamQueueStatistics,
}

impl StreamQueue {
    /// An empty queue at `limits`.
    pub fn new(limits: StreamQueueLimits) -> Self {
        Self {
            limits,
            stamp: 0,
            entries: BTreeMap::new(),
            data_bytes: 0,
            data_frames: 0,
            control_frames: 0,
            slots: HashMap::new(),
            lossless: HashMap::new(),
            lossless_stamps: BTreeSet::new(),
            floors: HashMap::new(),
            sequences: HashMap::new(),
            ledger: HashMap::new(),
            holey: HashSet::new(),
            pending: HashSet::new(),
            queued: HashMap::new(),
            released: HashSet::new(),
            stats: StreamQueueStatistics::default(),
        }
    }

    /// What the queue has done so far.
    pub fn stats(&self) -> StreamQueueStatistics {
        self.stats
    }

    /// Data frames queued.
    pub fn data_frames(&self) -> usize {
        self.data_frames
    }

    /// Bytes of data frames queued.
    pub fn data_bytes(&self) -> usize {
        self.data_bytes
    }

    /// Control frames queued.
    pub fn control_frames(&self) -> usize {
        self.control_frames
    }

    /// Whether nothing at all is queued.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn next_stamp(&mut self) -> u64 {
        self.stamp += 1;
        self.stamp
    }

    /// Hold `k`: its frames queue but are not delivered until its `SubscribeOk` is dequeued.
    pub fn hold(&mut self, k: u32) {
        self.pending.insert(k);
    }

    /// Whether `k` is held.
    pub fn is_held(&self, k: u32) -> bool {
        self.pending.contains(&k)
    }

    fn assign_sequence(&mut self, key: (u32, StreamQueueVerb)) -> u64 {
        let next = self.sequences.entry(key).or_insert(1);
        let seq = *next;
        *next += 1;
        seq
    }

    fn note_queued(&mut self, item: &StreamQueueItem) {
        if let Some(k) = item.key() {
            *self.queued.entry(k).or_insert(0) += 1;
        }
    }

    fn note_unqueued(&mut self, k: u32) {
        if let Some(n) = self.queued.get_mut(&k) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                self.queued.remove(&k);
            }
        }
    }

    /// Free a RELEASED key once nothing queued mentions it. Called after the work that might have
    /// needed its ledger (a dequeue), never from inside `unlink`.
    fn sweep(&mut self, k: u32) {
        if self.released.contains(&k) && !self.queued.contains_key(&k) {
            self.free_key(k);
        }
    }

    /// Remove an entry and every index that points at it. The one place the books are balanced.
    fn unlink(&mut self, stamp: u64) -> Option<Entry> {
        let entry = self.entries.remove(&stamp)?;
        match &entry.item {
            StreamQueueItem::Control(_) => self.control_frames -= 1,
            StreamQueueItem::Data { k, payload, .. } => {
                self.data_frames -= 1;
                self.data_bytes -= entry.cost;
                let key = (*k, payload.verb());
                if entry.lossless {
                    self.lossless_stamps.remove(&stamp);
                    if let Some(d) = self.lossless.get_mut(&key) {
                        if d.front() == Some(&stamp) {
                            d.pop_front();
                        } else {
                            d.retain(|s| *s != stamp);
                        }
                        if d.is_empty() {
                            self.lossless.remove(&key);
                        }
                    }
                } else {
                    self.slot_release(key, payload.slot_dimension(), stamp);
                }
            }
        }
        if let Some(k) = entry.item.key() {
            self.note_unqueued(k);
        }
        Some(entry)
    }

    /// Point the conflatable slot `(key, dimension)` at `stamp`; the stamp it held before, if any.
    fn slot_replace(
        &mut self,
        key: (u32, StreamQueueVerb),
        dimension: &str,
        stamp: u64,
    ) -> Option<u64> {
        let slots = self.slots.entry(key).or_default();
        match slots.get_mut(dimension) {
            Some(held) => Some(std::mem::replace(held, stamp)),
            None => {
                slots.insert(dimension.to_string(), stamp);
                None
            }
        }
    }

    /// Forget the slot `(key, dimension)` if it still names `stamp` (a newer frame may have taken it).
    fn slot_release(&mut self, key: (u32, StreamQueueVerb), dimension: &str, stamp: u64) {
        let Some(slots) = self.slots.get_mut(&key) else { return };
        if slots.get(dimension) == Some(&stamp) {
            slots.remove(dimension);
        }
        if slots.is_empty() {
            self.slots.remove(&key);
        }
    }

    /// Queue one data frame of key `k`. Never blocks and never fails: it either queues, supersedes,
    /// evicts, or refuses a row that cannot cross (and discloses that).
    pub fn push_data(&mut self, k: u32, payload: StreamQueuePayload) -> StreamQueuePush {
        let finite = payload.is_finite();
        self.push_data_checked(k, payload, finite)
    }

    /// [`StreamQueue::push_data`] with the finite scan already done: `finite` MUST be
    /// `payload.is_finite()`. The scan of a folded `Book` walks every level, so the sink does it
    /// BEFORE it takes the session lock that guards this queue and passes the answer in.
    pub fn push_data_checked(
        &mut self,
        k: u32,
        payload: StreamQueuePayload,
        finite: bool,
    ) -> StreamQueuePush {
        let verb = payload.verb();
        let key = (k, verb);
        let seq = self.assign_sequence(key);
        let lossless = self.is_lossless(key);
        // The frame's time, for the ledger of a lossless verb: where a hole it falls into opens.
        let ts_ms = if lossless { payload.stamp().unwrap_or_else(vike_model::now_ms) } else { 0 };
        if !finite {
            self.stats.nonfinite += 1;
            if lossless {
                self.add_hole(key, seq, payload.stream(), ts_ms);
            }
            return StreamQueuePush::Refused;
        }
        let cost = payload.cost();
        if cost > self.limits.bytes {
            // Bigger than the whole bound: it could only ever evict itself AND everything else, so it
            // is refused alone. A lossless frame owes the disclosure; a conflatable one is latest-wins.
            if lossless {
                self.add_hole(key, seq, payload.stream(), ts_ms);
                self.stats.lossless_evicted += 1;
                return StreamQueuePush::Evicted(1);
            }
            self.stats.conflatable_evicted += 1;
            return StreamQueuePush::Refused;
        }
        let stamp = self.next_stamp();
        let replaced = if lossless {
            self.lossless.entry(key).or_default().push_back(stamp);
            self.lossless_stamps.insert(stamp);
            None
        } else {
            // The slot is `(k, verb)` and, for a forming bar, its interval (`slot_dimension`).
            self.slot_replace(key, payload.slot_dimension(), stamp)
        };
        let item = StreamQueueItem::Data { k, sequence: seq, payload };
        self.note_queued(&item);
        let mut superseded = false;
        if let Some(old) = replaced {
            // Latest-wins: the older unsent frame of the slot goes, and the survivor takes the
            // position of its own emission. (`unlink` leaves the new slot mapping alone because it
            // only removes a mapping that still names the stamp being unlinked.)
            self.unlink(old);
            superseded = true;
            self.stats.superseded += 1;
        }
        self.entries.insert(stamp, Entry { item, cost, timestamp: ts_ms, lossless });
        self.data_frames += 1;
        self.data_bytes += cost;
        let evicted = self.enforce_bounds(k);
        if evicted > 0 {
            StreamQueuePush::Evicted(evicted)
        } else if superseded {
            StreamQueuePush::Superseded
        } else {
            StreamQueuePush::Queued
        }
    }

    /// Queue one control frame. `false` when the reserved lane is full: the caller ends the session.
    pub fn push_control(&mut self, control: StreamQueueControl) -> bool {
        if self.control_frames >= self.limits.control_frames {
            return false;
        }
        let stamp = self.next_stamp();
        let item = StreamQueueItem::Control(control);
        self.note_queued(&item);
        self.entries.insert(stamp, Entry { item, cost: 0, timestamp: 0, lossless: false });
        self.control_frames += 1;
        true
    }

    /// The class `key` runs at right now: the verb's own, or the subscriber's floor if that is stronger.
    pub fn class_of(&self, key: (u32, StreamQueueVerb)) -> DataClientSinkDelivery {
        stronger(
            key.1.base(),
            self.floors.get(&key).copied().unwrap_or(DataClientSinkDelivery::Conflatable),
        )
    }

    /// Whether a frame of `key` pushed now is queued under the LOSSLESS rule: evictable only with a
    /// disclosed hole, numbered into the ledger. A data verb at [`DataClientSinkDelivery::Control`] is queued like a
    /// lossless one (the reserved control lane is for the wire's own frames; a data frame under it
    /// would be unbounded).
    pub fn is_lossless(&self, key: (u32, StreamQueueVerb)) -> bool {
        self.class_of(key) >= DataClientSinkDelivery::Lossless
    }

    /// Set the class FLOOR of `lane` on key `k` to `floor` (the weakest, [`DataClientSinkDelivery::Conflatable`],
    /// clears it). The floor applies to every verb of the lane (`vike_data::delivery::lane_verbs`) that
    /// has a wire name ([`StreamQueueVerb::gap_verb`]) and whose own class is weaker; every other verb keeps its
    /// own, so a floor is never a weakening and a lane of already-lossless verbs is unchanged.
    ///
    /// Returns the lane's class from now on (the strongest over its verbs) when it CHANGED, else
    /// `None`: the caller announces a change ([`StreamQueueControl::Delivery`]) and says nothing about a no-op.
    /// Frames already queued keep the rule they entered under (`Entry::lossless`).
    pub fn set_floor(
        &mut self,
        k: u32,
        lane: StreamLabel,
        floor: DataClientSinkDelivery,
    ) -> Option<DataClientSinkDelivery> {
        let mut before = DataClientSinkDelivery::Conflatable;
        let mut after = DataClientSinkDelivery::Conflatable;
        for sink_verb in lane_verbs(lane) {
            let Some(verb) = StreamQueueVerb::from_sink_verb(*sink_verb) else { continue };
            let key = (k, verb);
            before = stronger(before, self.class_of(key));
            if verb.gap_verb().is_some() && floor > verb.base() {
                self.floors.insert(key, floor);
            } else {
                self.floors.remove(&key);
            }
            after = stronger(after, self.class_of(key));
        }
        (after != before).then_some(after)
    }

    /// The floor `lane` of key `k` runs under: the strongest over its verbs, [`DataClientSinkDelivery::Conflatable`]
    /// when none is raised.
    pub fn floor_of(&self, k: u32, lane: StreamLabel) -> DataClientSinkDelivery {
        lane_verbs(lane)
            .iter()
            .filter_map(|v| StreamQueueVerb::from_sink_verb(*v))
            .filter_map(|verb| self.floors.get(&(k, verb)).copied())
            .fold(DataClientSinkDelivery::Conflatable, stronger)
    }

    fn add_hole(&mut self, key: (u32, StreamQueueVerb), seq: u64, stream: StreamWordWire, ts: i64) {
        self.ledger.entry(key).or_default().add_hole(seq, stream, ts);
        self.holey.insert(key);
    }

    /// How many frames of lane `(k, verb)` have been lost (evicted, refused or unsendable) since the
    /// key was bound: a figure that only grows while the key lives, for a sender that must know its
    /// receiver's state has diverged (`crate::sink::DataClientSinkStream::book_losses`).
    pub fn lost(&self, k: u32, verb: StreamQueueVerb) -> u64 {
        self.ledger.get(&(k, verb)).map_or(0, |l| l.lost)
    }

    fn oldest_lossless_of(&self, k: u32) -> Option<u64> {
        StreamQueueVerb::GAPPABLE
            .iter()
            .filter_map(|v| self.lossless.get(&(k, *v)).and_then(|d| d.front().copied()))
            .min()
    }

    fn enforce_bounds(&mut self, pusher: u32) -> usize {
        let mut evicted = 0;
        while self.data_bytes > self.limits.bytes || self.data_frames > self.limits.frames {
            // The key that is FILLING the queue pays: with older frames of its own queued, its oldest
            // goes. A key whose only queued frame is the one it just pushed (a quiet key into a queue
            // others filled) must not lose that frame to its neighbours' burst, so the globally oldest
            // lossless frame goes instead.
            let own: usize = StreamQueueVerb::GAPPABLE
                .iter()
                .map(|v| self.lossless.get(&(pusher, *v)).map_or(0, VecDeque::len))
                .sum();
            let victim = if own > 1 {
                self.oldest_lossless_of(pusher)
            } else {
                self.lossless_stamps.first().copied()
            };
            if let Some(stamp) = victim {
                match self.unlink(stamp) {
                    Some(Entry {
                        item: StreamQueueItem::Data { k, sequence: seq, payload },
                        timestamp: ts,
                        ..
                    }) => {
                        let key = (k, payload.verb());
                        self.add_hole(key, seq, payload.stream(), ts);
                        self.stats.lossless_evicted += 1;
                        evicted += 1;
                        self.sweep(k);
                    }
                    // An index that named a frame the map does not hold: unreachable by the books'
                    // construction, but a loop that made no progress would spin the pump's thread, so
                    // the stale stamp is dropped and the loop goes on.
                    _ => {
                        debug_assert!(false, "a lossless index named a missing entry");
                        self.lossless_stamps.remove(&stamp);
                        for d in self.lossless.values_mut() {
                            d.retain(|s| *s != stamp);
                        }
                    }
                }
                continue;
            }
            // Nothing lossless left to give. Conflatable frames are bounded by their slots, so this is
            // reached only if a single frame outgrows the bound: shed the oldest, counted.
            let oldest_data = self
                .entries
                .iter()
                .find(|(_, e)| matches!(e.item, StreamQueueItem::Data { .. }))
                .map(|(s, _)| *s);
            match oldest_data {
                Some(stamp) => {
                    self.unlink(stamp);
                    self.stats.conflatable_evicted += 1;
                }
                None => break,
            }
        }
        evicted
    }

    fn first_deliverable(&self) -> Option<u64> {
        if self.pending.is_empty() {
            return self.entries.keys().next().copied();
        }
        self.entries
            .iter()
            .find(|(_, e)| e.item.key().is_none_or(|k| !self.pending.contains(&k)))
            .map(|(s, _)| *s)
    }

    /// The next frame to write, with the `Gap`s that precede it.
    pub fn take(&mut self) -> Option<StreamQueueTake> {
        let stamp = self.first_deliverable()?;
        let entry = self.unlink(stamp)?;
        let frame_ts = entry.timestamp;
        let lossless = entry.lossless;
        match entry.item {
            StreamQueueItem::Control(control) => {
                if let StreamQueueControl::SubscribeOk { k, .. } = &control {
                    self.pending.remove(k);
                }
                if let StreamQueueControl::Status { k, .. }
                | StreamQueueControl::Delay { k, .. }
                | StreamQueueControl::Delivery { k, .. } = &control
                {
                    self.sweep(*k);
                }
                Some(StreamQueueTake {
                    gaps: Vec::new(),
                    item: StreamQueueItem::Control(control),
                    undo: None,
                })
            }
            StreamQueueItem::Data { k, sequence: seq, payload } => {
                let verb = payload.verb();
                let key = (k, verb);
                let mut gaps = Vec::new();
                let mut undo = None;
                if lossless {
                    let stream = payload.stream();
                    let ledger = self.ledger.entry(key).or_default();
                    let previous = ledger.last_delivered;
                    let previous_timestamp = ledger.last_delivered_timestamp;
                    let holes = ledger.take_before(seq);
                    if ledger.holes.is_empty() {
                        self.holey.remove(&key);
                    }
                    ledger.last_delivered = Some(seq);
                    ledger.last_delivered_timestamp = Some(frame_ts);
                    if !holes.is_empty() {
                        let opens = opens_at(previous_timestamp, &holes);
                        gaps = gaps_for(k, verb, previous.unwrap_or(0), seq, opens, holes);
                    }
                    undo = Some(StreamQueueUndo {
                        k,
                        verb,
                        sequence: seq,
                        previous,
                        previous_timestamp,
                        stream,
                        timestamp: frame_ts,
                    });
                }
                self.sweep(k);
                Some(StreamQueueTake {
                    gaps,
                    item: StreamQueueItem::Data { k, sequence: seq, payload },
                    undo,
                })
            }
        }
    }

    /// `Gap`s for every hole that no queued frame will carry: the newest frames of a `(k, verb)` were
    /// the ones lost, so the writer discloses the loss now, naming the number the NEXT frame will
    /// carry as `to_sequence`. Held keys wait for their `SubscribeOk`.
    pub fn tail_gaps(&mut self) -> Vec<StreamDown> {
        if self.holey.is_empty() {
            return Vec::new();
        }
        let due: Vec<(u32, StreamQueueVerb)> = self
            .holey
            .iter()
            .filter(|key| {
                !self.pending.contains(&key.0)
                    && self.lossless.get(key).is_none_or(VecDeque::is_empty)
            })
            .copied()
            .collect();
        let mut out = Vec::new();
        for key in due {
            let to = self.sequences.get(&key).copied().unwrap_or(1);
            if let Some(ledger) = self.ledger.get_mut(&key) {
                let holes = std::mem::take(&mut ledger.holes);
                let opens = opens_at(ledger.last_delivered_timestamp, &holes);
                let from = ledger.last_delivered.unwrap_or(0);
                out.extend(gaps_for(key.0, key.1, from, to, opens, holes));
            }
            self.holey.remove(&key);
        }
        out
    }

    /// Take back a lossless frame the writer could not send: it never reached the peer, so it is a
    /// hole (disclosed with the next survivor or at the tail) and not a delivery.
    pub fn note_unsendable(&mut self, undo: StreamQueueUndo) {
        let StreamQueueUndo {
            k,
            verb,
            sequence: seq,
            previous,
            previous_timestamp,
            stream,
            timestamp: ts,
        } = undo;
        let key = (k, verb);
        self.stats.unsendable += 1;
        // A key that was freed while the frame was in the writer's hands has no reader left to tell.
        if !self.sequences.contains_key(&key) {
            return;
        }
        if let Some(ledger) = self.ledger.get_mut(&key) {
            ledger.last_delivered = previous;
            ledger.last_delivered_timestamp = previous_timestamp;
        }
        self.add_hole(key, seq, stream, ts);
    }

    /// Forget a key whose subscription failed to start: its held frames, its numbers and its holes.
    /// The client never learned the key, so nothing about it may reach the wire.
    pub fn purge(&mut self, k: u32) {
        self.pending.remove(&k);
        let stamps: Vec<u64> =
            self.entries.iter().filter(|(_, e)| e.item.key() == Some(k)).map(|(s, _)| *s).collect();
        for stamp in stamps {
            self.unlink(stamp);
        }
        self.free_key(k);
    }

    /// The key is no longer bound (its last subscription ended): free its numbers and holes once
    /// nothing queued mentions it.
    pub fn release_key(&mut self, k: u32) {
        self.released.insert(k);
        self.sweep(k);
    }

    fn free_key(&mut self, k: u32) {
        self.sequences.retain(|key, _| key.0 != k);
        self.ledger.retain(|key, _| key.0 != k);
        self.holey.retain(|key| key.0 != k);
        self.slots.retain(|key, _| key.0 != k);
        self.floors.retain(|key, _| key.0 != k);
        self.released.remove(&k);
    }
}

#[cfg(test)]
#[path = "queue_tests.rs"]
pub(crate) mod queue_tests;
