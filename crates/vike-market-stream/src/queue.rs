//! The per-session BOUNDED outgoing queue, and the overflow policy that makes a laggard's losses honest.
//!
//! # Why this is not the md mailbox
//!
//! `crates/vike-datahub/src/md/mailbox.rs`'s `Mailbox` is the model. It was re-implemented here, not
//! imported, because both crates were layer 65 and `crates/vike-ops/tests/architecture/layer_gate.rs`
//! refuses an edge between siblings; the queue now sits below both (the data-plane tier, decision 0167),
//! and the mailbox moving onto it is that record's later verdict.
//! The shape is borrowed (bytes first and frames second, a reserved control lane that is never
//! evicted, supersede only where the contract says it is not a loss, a gap synthesized by the WRITER
//! immediately before the next post-hole frame) and the policy is this wire's own: the
//! lossless / conflatable / control split the sink's documented contracts draw
//! (`crates/vike-data/src/live.rs`'s `LiveDataSink`).
//!
//! # The three classes, and what each does on overflow
//!
//! | class | verbs | on overflow |
//! |---|---|---|
//! | lossless | `Seed`, `BarClose`, `Quote`, `Trade`, `Book`, `BookDelta` (and `Depth` on a lane a subscriber raised) | **evict the OLDEST queued frame of the pushing key** (the key that is filling the queue pays) when it has older frames queued, else the globally oldest lossless frame; a frame bigger than the whole bound is refused alone; every eviction is counted and OWES a `Gap` |
//! | conflatable | `Forming`, `Mark`, `BarCloseTick`, `Depth` (unless raised) | **supersede the unsent frame of that slot** - by the sink's own contract latest-wins is not a loss. The slot is `(k, verb)` and, for `Forming`, the bar INTERVAL as well: a forming bar is latest-wins per `(symbol, interval)`, and one symbol key carries every interval, so a 5m snapshot must never supersede an unsent 1m one |
//! | control | `SubOk`, `SubErr`, `Key`, `Status`, `Delay`, `Delivery` | **never evicted**; a separate bound; if even it cannot be filled the session is closed (`ControlLaneOverflow`) |
//!
//! `Gap` and `Heartbeat` and `Bye` are not queued at all: the writer synthesizes them.
//!
//! # A subscriber can RAISE a class (`Depth`), and a raise is the lossless rule whole
//!
//! The table above is each verb's OWN class (`vike_data::delivery`'s, read through [`Verb::base`]).
//! A subscriber that cannot take a superseded frame (a recorder writing `l2_snapshot` rows into a
//! tape) declares a FLOOR for a key's lane ([`Queue::set_floor`]); the class a `(k, verb)` runs at is
//! the stronger of its own and the floor, and only a verb with a wire name ([`Verb::gap_verb`]) can be
//! raised. Today that is `Depth`: with its floor at lossless a depth frame is queued, evicted, numbered
//! and disclosed exactly as a `Quote` is. ⚠ **Not "stop superseding"**: a depth frame that survives but
//! is still conflatable by position would make room by evicting lossless quotes and trades, so the
//! raised verb joins the lossless index, the ledger and the `Gap` machinery in full.
//!
//! The class is decided PER FRAME when it is pushed and travels with the entry (`Entry::lossless`), so a
//! change never re-files a frame already queued: a conflatable depth frame queued before a raise is
//! still superseded by a later conflatable one and still counted as such, a lossless one queued before
//! a relax is still evictable with a `Gap`. The change itself is announced in the control lane's
//! emission order ([`Control::Delivery`]), which is what lets the receiver restart its `seq` check at
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
//! never ahead of queued frames that precede the hole (the `CTRL`-lane defect the md mailbox's module doc
//! rejects). A hole with NO surviving frame after it (the newest frames were the ones dropped) is
//! disclosed as soon as the queue has nothing else for that `(k, verb)`: `Queue::tail_gaps` hands the
//! writer a `Gap` whose `to_seq` is the number the NEXT frame will carry, so the consumer learns of the
//! loss when the writer goes idle and not only when more data happens to arrive.
//!
//! # A `Gap` carries WHEN the hole opens
//!
//! The consumer files its `GapStart` under a time, and replay orders rows by `(ts, seq)`
//! (`crates/vike-store/src/archive_store.rs`'s `scan_book_updates_impl`; the recorder's marker has `seq`
//! 0 and lands under `at_ts_ms`). A marker stamped with the CLIENT's arrival clock would sort after
//! every survivor still queued behind a slow reader and un-anchor the replayed book too late. So the
//! ledger remembers the stamp of every frame it delivers and the first lost frame's stamp, and a `Gap`
//! names `from_ts_ms`: the last delivered frame's stamp, or with none delivered the first lost
//! frame's. A payload with no stamp of its own (`Payload::Book`) is stamped with the daemon's clock
//! when it is pushed.
//!
//! # One ordered sequence, and a held key
//!
//! The classes share one emission order (a stamp per entry), so a `Status` is never delivered ahead of
//! the data queued before it and a superseding frame takes the POSITION of its own emission (a
//! `Forming` snapshot of bar t+1 never overtakes the `BarClose` of bar t). The one exception is the
//! HELD key: a key a `Sub` is still starting ([`Queue::hold`]) has its frames queued but not
//! delivered until the writer dequeues that key's `SubOk`, which is what makes "`SubOk` before any frame
//! carrying that `k`" true even when a venue client emits a seed from inside `subscribe_*`.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;

use vike_data::delivery::{Delivery, SinkVerb, lane_verbs, stronger};
use vike_marketdata::{Bar, BookLevel, BookUpdate, L2Book, QuoteTick, TradeTick};
use vike_model::{DataDelay, StreamStatus};

use crate::wire::{GapVerb, StreamDown, SubErrKind, WireDelivery, WireLane, WireStream};

/// The ten sequenced data verbs of a stream (the twelve sink verbs less `stream_status` and
/// `data_delay`, which ride the control lane).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Verb {
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
    /// `l2_snapshot`. Conflatable, and lossless on a lane a subscriber raised ([`Queue::set_floor`]).
    Depth,
    /// `quote`. Lossless.
    Quote,
    /// `trade`. Lossless.
    Trade,
    /// `book`. Lossless.
    Book,
    /// `book_update`. Lossless.
    BookDelta,
}

impl Verb {
    /// Every verb that CAN be lossless: the six whose own class is, and `Depth`, which a subscriber may
    /// raise. Exactly the verbs [`Verb::gap_verb`] names.
    pub const GAPPABLE: [Verb; 7] = [
        Verb::Seed,
        Verb::BarClose,
        Verb::Quote,
        Verb::Trade,
        Verb::Book,
        Verb::BookDelta,
        Verb::Depth,
    ];

    /// The sink verb this queue verb is a frame of. Exhaustive: a queue verb is a role in the
    /// `vike_data::delivery` table, and the table is where its class is read from.
    pub const fn sink_verb(self) -> SinkVerb {
        match self {
            Verb::Seed => SinkVerb::SeedBars,
            Verb::BarClose => SinkVerb::CloseBar,
            Verb::Forming => SinkVerb::FormingBar,
            Verb::Mark => SinkVerb::MarkTick,
            Verb::BarCloseTick => SinkVerb::BarCloseTick,
            Verb::Depth => SinkVerb::L2Snapshot,
            Verb::Quote => SinkVerb::Quote,
            Verb::Trade => SinkVerb::Trade,
            Verb::Book => SinkVerb::Book,
            Verb::BookDelta => SinkVerb::BookUpdate,
        }
    }

    /// The queue verb of a sink verb, `None` for the two control verbs (`stream_status`, `data_delay`),
    /// which are not sequenced data frames. Exhaustive.
    pub const fn from_sink_verb(verb: SinkVerb) -> Option<Verb> {
        match verb {
            SinkVerb::SeedBars => Some(Verb::Seed),
            SinkVerb::CloseBar => Some(Verb::BarClose),
            SinkVerb::FormingBar => Some(Verb::Forming),
            SinkVerb::MarkTick => Some(Verb::Mark),
            SinkVerb::BarCloseTick => Some(Verb::BarCloseTick),
            SinkVerb::L2Snapshot => Some(Verb::Depth),
            SinkVerb::Quote => Some(Verb::Quote),
            SinkVerb::Trade => Some(Verb::Trade),
            SinkVerb::Book => Some(Verb::Book),
            SinkVerb::BookUpdate => Some(Verb::BookDelta),
            SinkVerb::StreamStatus | SinkVerb::DataDelay => None,
        }
    }

    /// The verb's own class, before any subscriber's floor: `vike_data::delivery`'s table, the one
    /// every tube reads.
    pub const fn base(self) -> Delivery {
        self.sink_verb().base()
    }

    /// The wire's name for a verb that can be lossless, `None` for one that never is. Exhaustive.
    /// ⚠ `Some` does not say the verb IS lossless on a given key (`Depth` is not, until a subscriber
    /// raises it): the class of a key is [`Queue::is_lossless`].
    pub const fn gap_verb(self) -> Option<GapVerb> {
        match self {
            Verb::Seed => Some(GapVerb::Seed),
            Verb::BarClose => Some(GapVerb::BarClose),
            Verb::Quote => Some(GapVerb::Quote),
            Verb::Trade => Some(GapVerb::Trade),
            Verb::Book => Some(GapVerb::Book),
            Verb::BookDelta => Some(GapVerb::BookDelta),
            Verb::Depth => Some(GapVerb::Depth),
            Verb::Forming | Verb::Mark | Verb::BarCloseTick => None,
        }
    }
}

/// What a data frame carries, kept TYPED in the queue so the writer thread (not the venue pump)
/// builds and serializes the frame.
#[derive(Debug)]
pub enum Payload {
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
        px: f64,
        /// The stamp, epoch-ms.
        ts: i64,
    },
    /// `bar_close_tick`.
    BarCloseTick {
        /// The candle-close price.
        px: f64,
        /// The stamp, epoch-ms.
        ts: i64,
    },
    /// `l2_snapshot`.
    Depth {
        /// The feed's price grid, `0.0` when unknown.
        tick_size: f64,
        /// Bids, best first.
        bids: Vec<BookLevel>,
        /// Asks, best first.
        asks: Vec<BookLevel>,
        /// The venue/receipt stamp, epoch-ms.
        ts: i64,
    },
    /// `quote`.
    Quote(QuoteTick),
    /// `trade`.
    Trade(TradeTick),
    /// `book`. Held behind the feed's own `Arc` until the writer clones it into the frame, so the
    /// copy is the writer thread's cost and not the pump's.
    Book(Arc<L2Book>),
    /// `book_update`.
    BookDelta(BookUpdate),
}

/// A rough per-frame fixed cost (tags, the key, the sequence number, queue bookkeeping) added to every
/// payload's size. A bound, not a measurement.
const FRAME_OVERHEAD: usize = 96;

impl Payload {
    /// The verb this payload is a frame of.
    pub fn verb(&self) -> Verb {
        match self {
            Payload::Seed { .. } => Verb::Seed,
            Payload::BarClose { .. } => Verb::BarClose,
            Payload::Forming { .. } => Verb::Forming,
            Payload::Mark { .. } => Verb::Mark,
            Payload::BarCloseTick { .. } => Verb::BarCloseTick,
            Payload::Depth { .. } => Verb::Depth,
            Payload::Quote(_) => Verb::Quote,
            Payload::Trade(_) => Verb::Trade,
            Payload::Book(_) => Verb::Book,
            Payload::BookDelta(_) => Verb::BookDelta,
        }
    }

    /// What the queue's byte bound counts this frame as: its in-memory size, not its JSON size.
    pub fn cost(&self) -> usize {
        use std::mem::size_of;
        FRAME_OVERHEAD
            + match self {
                Payload::Seed { interval, bars } => interval.len() + bars.len() * size_of::<Bar>(),
                Payload::BarClose { interval, .. } | Payload::Forming { interval, .. } => {
                    interval.len() + size_of::<Bar>()
                }
                Payload::Mark { .. } | Payload::BarCloseTick { .. } => 0,
                Payload::Depth { bids, asks, .. } => {
                    (bids.len() + asks.len()) * size_of::<BookLevel>()
                }
                Payload::Quote(q) => size_of::<QuoteTick>() + q.symbol.len(),
                Payload::Trade(t) => size_of::<TradeTick>() + t.symbol.len(),
                Payload::Book(b) => {
                    (b.bid_levels() + b.ask_levels()) * (size_of::<BookLevel>() + 16)
                }
                Payload::BookDelta(u) => {
                    size_of::<BookUpdate>()
                        + (u.bids.len() + u.asks.len()) * size_of::<BookLevel>()
                        + u.symbol.len()
                }
            }
    }

    /// The stream word a `Gap` over this payload's verb names: the interval for a bar verb, the lane
    /// for a tick verb. (Meaningful for the six lossless verbs.)
    pub fn stream(&self) -> WireStream {
        match self {
            Payload::Seed { interval, .. }
            | Payload::BarClose { interval, .. }
            | Payload::Forming { interval, .. } => WireStream::Bar(interval.clone()),
            Payload::Quote(_) => WireStream::Lane(WireLane::Quotes),
            Payload::Trade(_) => WireStream::Lane(WireLane::Trades),
            Payload::Book(_) | Payload::BookDelta(_) => WireStream::Lane(WireLane::Book),
            Payload::Depth { .. } => WireStream::Lane(WireLane::Depth),
            Payload::Mark { .. } | Payload::BarCloseTick { .. } => WireStream::Link,
        }
    }

    /// The stamp (epoch-ms) the frame's own payload carries, `None` for the folded `Book` (an
    /// `L2Book` has no time). A seed is stamped by its newest bar; one with no bars has none.
    pub fn stamp(&self) -> Option<i64> {
        match self {
            Payload::Seed { bars, .. } => bars.last().map(|b| b.ts),
            Payload::BarClose { bar, .. } | Payload::Forming { bar, .. } => Some(bar.ts),
            Payload::Mark { ts, .. }
            | Payload::BarCloseTick { ts, .. }
            | Payload::Depth { ts, .. } => Some(*ts),
            Payload::Quote(q) => Some(q.ts),
            Payload::Trade(t) => Some(t.ts),
            Payload::Book(_) => None,
            Payload::BookDelta(u) => Some(u.ts),
        }
    }

    /// The sub-dimension of this payload's conflation slot: the bar interval for a `Forming` snapshot
    /// (latest-wins per `(symbol, interval)`), `""` for every verb whose slot is just `(k, verb)`.
    /// Exhaustive: a new conflatable verb must say whether its key carries a dimension.
    fn slot_dimension(&self) -> &str {
        match self {
            Payload::Forming { interval, .. } => interval,
            Payload::Seed { .. }
            | Payload::BarClose { .. }
            | Payload::Mark { .. }
            | Payload::BarCloseTick { .. }
            | Payload::Depth { .. }
            | Payload::Quote(_)
            | Payload::Trade(_)
            | Payload::Book(_)
            | Payload::BookDelta(_) => "",
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
            |l: &[BookLevel]| l.iter().all(|x| x.price.is_finite() && x.qty.is_finite());
        match self {
            Payload::Seed { bars, .. } => bars.iter().all(bar_ok),
            Payload::BarClose { bar, .. } | Payload::Forming { bar, .. } => bar_ok(bar),
            Payload::Mark { px, .. } | Payload::BarCloseTick { px, .. } => px.is_finite(),
            Payload::Depth { tick_size, bids, asks, .. } => {
                tick_size.is_finite() && levels_ok(bids) && levels_ok(asks)
            }
            Payload::Quote(q) => {
                q.bid.is_finite()
                    && q.ask.is_finite()
                    && q.bid_size.is_finite()
                    && q.ask_size.is_finite()
            }
            Payload::Trade(t) => t.price.is_finite() && t.size.is_finite(),
            // The whole folded state: the grid AND every resting quantity, read through `top_n` over
            // every level (the book's maps are private to `vike-marketdata`, whose published surface
            // this daemon does not widen: it sits in the latency-measured closure). One non-finite
            // level would serialize as `null`, and a client that cannot decode a frame ends its
            // session, so the book is refused as a hole instead and the NEXT book (the next fold)
            // crosses.
            Payload::Book(b) => {
                let (bids, asks) = b.top_n(b.bid_levels().max(b.ask_levels()));
                b.tick_size.is_finite() && levels_ok(&bids) && levels_ok(&asks)
            }
            Payload::BookDelta(u) => {
                u.tick_size.is_finite() && levels_ok(&u.bids) && levels_ok(&u.asks)
            }
        }
    }
}

/// A control frame: carried in order, never evicted.
#[derive(Debug)]
pub enum Control {
    /// The stream started. Dequeuing it RELEASES a held key.
    SubOk {
        /// The client's subscription id.
        sid: u64,
        /// The key the symbol is bound to.
        k: u32,
    },
    /// The stream did not start.
    SubErr {
        /// The client's subscription id.
        sid: u64,
        /// Why.
        err: SubErrKind,
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
        stream: WireStream,
        /// The transition, verbatim.
        status: StreamStatus,
    },
    /// `data_delay`.
    Delay {
        /// The key.
        k: u32,
        /// Which lane.
        stream: WireStream,
        /// The label, verbatim.
        delay: DataDelay,
    },
    /// The class of a key's lane changed (a subscriber raised or relaxed it): queued in the control
    /// lane's emission order, so it lands between the frames queued under the old class and the ones
    /// queued under the new ([`StreamDown::Delivery`]).
    Delivery {
        /// The key.
        k: u32,
        /// The lane whose class changed.
        lane: WireLane,
        /// The lane's class from here on.
        class: WireDelivery,
    },
}

/// One queued frame.
#[derive(Debug)]
pub enum Item {
    /// A sequenced data frame.
    Data {
        /// The key.
        k: u32,
        /// The `(k, verb)` sequence number.
        seq: u64,
        /// The payload.
        payload: Payload,
    },
    /// A control frame.
    Ctrl(Control),
}

impl Item {
    /// The key a frame carries, for the held-key rule and a purge. `SubOk`, `SubErr` and `Key` are
    /// not "frames carrying a key" in that sense: the first releases it and the others bind it.
    fn key(&self) -> Option<u32> {
        match self {
            Item::Data { k, .. } => Some(*k),
            Item::Ctrl(
                Control::Status { k, .. } | Control::Delay { k, .. } | Control::Delivery { k, .. },
            ) => Some(*k),
            Item::Ctrl(Control::SubOk { .. } | Control::SubErr { .. } | Control::Key { .. }) => {
                None
            }
        }
    }

    /// The wire frame. Done by the WRITER, outside the queue's lock.
    pub fn into_frame(self) -> StreamDown {
        match self {
            Item::Data { k, seq, payload } => match payload {
                Payload::Seed { interval, bars } => StreamDown::Seed { k, seq, interval, bars },
                Payload::BarClose { interval, bar } => {
                    StreamDown::BarClose { k, seq, interval, bar }
                }
                Payload::Forming { interval, bar } => StreamDown::Forming { k, seq, interval, bar },
                Payload::Mark { px, ts } => StreamDown::Mark { k, seq, px, ts },
                Payload::BarCloseTick { px, ts } => StreamDown::BarCloseTick { k, seq, px, ts },
                Payload::Depth { tick_size, bids, asks, ts } => {
                    StreamDown::Depth { k, seq, tick_size, bids, asks, ts }
                }
                Payload::Quote(quote) => StreamDown::Quote { k, seq, quote },
                Payload::Trade(trade) => StreamDown::Trade { k, seq, trade },
                Payload::Book(book) => StreamDown::Book { k, seq, book: Box::new((*book).clone()) },
                Payload::BookDelta(update) => {
                    StreamDown::BookDelta { k, seq, update: Box::new(update) }
                }
            },
            Item::Ctrl(Control::SubOk { sid, k }) => StreamDown::SubOk { sid, k },
            Item::Ctrl(Control::SubErr { sid, err }) => StreamDown::SubErr { sid, err },
            Item::Ctrl(Control::Key { k, symbol }) => StreamDown::Key { k, symbol },
            Item::Ctrl(Control::Status { k, stream, status }) => {
                StreamDown::Status { k, stream, status }
            }
            Item::Ctrl(Control::Delay { k, stream, delay }) => {
                StreamDown::Delay { k, stream, delay }
            }
            Item::Ctrl(Control::Delivery { k, lane, class }) => {
                StreamDown::Delivery { k, lane, class }
            }
        }
    }
}

/// The bounds of one queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueLimits {
    /// Bytes of data frames (the first bound).
    pub bytes: usize,
    /// Data frames (the second).
    pub frames: usize,
    /// Control frames (the reserved lane).
    pub ctrl_frames: usize,
}

/// What the queue has done, for the session's closing log line and the tests.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct QueueStats {
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
pub enum Pushed {
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
    stream: WireStream,
    /// The stamp of the FIRST frame of the run (epoch-ms): where the hole opens when nothing was
    /// delivered before it.
    first_ts: i64,
}

#[derive(Debug, Default)]
struct Ledger {
    /// The `seq` of the last frame HANDED to the writer, `None` before the first.
    last_delivered: Option<u64>,
    /// The stamp (epoch-ms) of that frame: where the next hole opens.
    last_delivered_ts: Option<i64>,
    /// Holes in ascending `seq` order.
    holes: Vec<Hole>,
    /// Every frame this lane has LOST since the key was bound, delivered holes included: never reset
    /// by a disclosure, so a poller that remembers the last figure it saw learns of a loss the `Gap`
    /// already carried to the wire ([`Queue::lost`]).
    lost: u64,
}

impl Ledger {
    fn add_hole(&mut self, seq: u64, stream: WireStream, ts: i64) {
        self.lost += 1;
        if let Some(last) = self.holes.last_mut()
            && last.last_missing + 1 == seq
            && last.stream == stream
        {
            last.last_missing = seq;
            last.dropped += 1;
            return;
        }
        self.holes.push(Hole { last_missing: seq, dropped: 1, stream, first_ts: ts });
    }

    /// Drain the holes that lie before a surviving frame of `seq`.
    fn take_before(&mut self, seq: u64) -> Vec<Hole> {
        let n = self.holes.partition_point(|h| h.last_missing < seq);
        self.holes.drain(..n).collect()
    }
}

/// Where a run of `holes` opens: the stamp of the last frame delivered before them, else the first
/// lost frame's (`from_ts_ms` on the wire). `holes` are in ascending `seq` order and never empty here.
fn opens_at(last_delivered_ts: Option<i64>, holes: &[Hole]) -> i64 {
    last_delivered_ts.or_else(|| holes.first().map(|h| h.first_ts)).unwrap_or_default()
}

/// The `Gap` frames for `holes` (one per distinct stream), spanning `from` (the last frame delivered
/// before them, `0` when none) to `to` (the number of the first frame after them), opening at `from_ts`.
fn gaps_for(
    k: u32,
    verb: Verb,
    from: u64,
    to: u64,
    from_ts: i64,
    holes: Vec<Hole>,
) -> Vec<StreamDown> {
    let Some(gap_verb) = verb.gap_verb() else { return Vec::new() };
    let mut groups: Vec<(WireStream, u64)> = Vec::new();
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
            from_seq: from,
            to_seq: to,
            from_ts_ms: from_ts,
        })
        .collect()
}

struct Entry {
    item: Item,
    cost: usize,
    /// The frame's stamp (epoch-ms): its payload's own, else the push clock. Meaningful for a
    /// lossless data frame, which is what the ledger tracks.
    ts: i64,
    /// Whether the frame was queued under the LOSSLESS rule: its verb's class and the key's floor AT
    /// THE TIME OF THE PUSH (`Queue::is_lossless`). Decided once and carried, so the books (the
    /// lossless index or the conflatable slot) are balanced by the rule the frame entered under even
    /// when the key's floor changed while it waited.
    lossless: bool,
}

/// What the writer needs to take back a frame it turned out unable to send.
#[derive(Debug)]
pub struct Undo {
    k: u32,
    verb: Verb,
    seq: u64,
    previous: Option<u64>,
    previous_ts: Option<i64>,
    stream: WireStream,
    ts: i64,
}

/// One dequeued frame, with the gaps that precede it.
#[derive(Debug)]
pub struct Taken {
    /// `Gap` frames to write BEFORE `item` (empty almost always).
    pub gaps: Vec<StreamDown>,
    /// The frame.
    pub item: Item,
    /// Present for a lossless data frame: how to un-deliver it if it cannot be written.
    pub undo: Option<Undo>,
}

/// The bounded queue. See the module doc. Not synchronized: the sink wraps it in its own lock.
pub struct Queue {
    limits: QueueLimits,
    stamp: u64,
    entries: BTreeMap<u64, Entry>,
    data_bytes: usize,
    data_frames: usize,
    ctrl_frames: usize,
    /// `(k, verb)` -> `slot dimension` (the bar interval for `Forming`, `""` otherwise) -> the stamp
    /// of that conflatable slot's unsent frame. Nested so the hot lookup borrows the interval
    /// instead of allocating a key.
    slots: HashMap<(u32, Verb), HashMap<String, u64>>,
    /// `(k, verb)` -> the stamps of its queued lossless frames, oldest first.
    lossless: HashMap<(u32, Verb), VecDeque<u64>>,
    /// Every queued lossless stamp, for the globally-oldest eviction.
    lossless_stamps: BTreeSet<u64>,
    /// `(k, verb)` -> the subscriber's class FLOOR where it is stronger than the verb's own (only a
    /// verb with a wire name is ever raised: [`Queue::set_floor`]). Absent = the verb's own class.
    floors: HashMap<(u32, Verb), Delivery>,
    /// `(k, verb)` -> the next `seq` to assign.
    seqs: HashMap<(u32, Verb), u64>,
    ledger: HashMap<(u32, Verb), Ledger>,
    /// The `(k, verb)` pairs whose ledger holds a hole.
    holey: HashSet<(u32, Verb)>,
    pending: HashSet<u32>,
    /// How many queued entries mention each key (so a released key is freed once it drains).
    queued: HashMap<u32, usize>,
    released: HashSet<u32>,
    stats: QueueStats,
}

impl Queue {
    /// An empty queue at `limits`.
    pub fn new(limits: QueueLimits) -> Self {
        Self {
            limits,
            stamp: 0,
            entries: BTreeMap::new(),
            data_bytes: 0,
            data_frames: 0,
            ctrl_frames: 0,
            slots: HashMap::new(),
            lossless: HashMap::new(),
            lossless_stamps: BTreeSet::new(),
            floors: HashMap::new(),
            seqs: HashMap::new(),
            ledger: HashMap::new(),
            holey: HashSet::new(),
            pending: HashSet::new(),
            queued: HashMap::new(),
            released: HashSet::new(),
            stats: QueueStats::default(),
        }
    }

    /// What the queue has done so far.
    pub fn stats(&self) -> QueueStats {
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
    pub fn ctrl_frames(&self) -> usize {
        self.ctrl_frames
    }

    /// Whether nothing at all is queued.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn next_stamp(&mut self) -> u64 {
        self.stamp += 1;
        self.stamp
    }

    /// Hold `k`: its frames queue but are not delivered until its `SubOk` is dequeued.
    pub fn hold(&mut self, k: u32) {
        self.pending.insert(k);
    }

    /// Whether `k` is held.
    pub fn is_held(&self, k: u32) -> bool {
        self.pending.contains(&k)
    }

    fn assign_seq(&mut self, key: (u32, Verb)) -> u64 {
        let next = self.seqs.entry(key).or_insert(1);
        let seq = *next;
        *next += 1;
        seq
    }

    fn note_queued(&mut self, item: &Item) {
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
            Item::Ctrl(_) => self.ctrl_frames -= 1,
            Item::Data { k, payload, .. } => {
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
    fn slot_replace(&mut self, key: (u32, Verb), dimension: &str, stamp: u64) -> Option<u64> {
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
    fn slot_release(&mut self, key: (u32, Verb), dimension: &str, stamp: u64) {
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
    pub fn push_data(&mut self, k: u32, payload: Payload) -> Pushed {
        let finite = payload.is_finite();
        self.push_data_checked(k, payload, finite)
    }

    /// [`Queue::push_data`] with the finite scan already done: `finite` MUST be
    /// `payload.is_finite()`. The scan of a folded `Book` walks every level, so the sink does it
    /// BEFORE it takes the session lock that guards this queue and passes the answer in.
    pub fn push_data_checked(&mut self, k: u32, payload: Payload, finite: bool) -> Pushed {
        let verb = payload.verb();
        let key = (k, verb);
        let seq = self.assign_seq(key);
        let lossless = self.is_lossless(key);
        // The frame's time, for the ledger of a lossless verb: where a hole it falls into opens.
        let ts_ms = if lossless { payload.stamp().unwrap_or_else(vike_model::now_ms) } else { 0 };
        if !finite {
            self.stats.nonfinite += 1;
            if lossless {
                self.add_hole(key, seq, payload.stream(), ts_ms);
            }
            return Pushed::Refused;
        }
        let cost = payload.cost();
        if cost > self.limits.bytes {
            // Bigger than the whole bound: it could only ever evict itself AND everything else, so it
            // is refused alone. A lossless frame owes the disclosure; a conflatable one is latest-wins.
            if lossless {
                self.add_hole(key, seq, payload.stream(), ts_ms);
                self.stats.lossless_evicted += 1;
                return Pushed::Evicted(1);
            }
            self.stats.conflatable_evicted += 1;
            return Pushed::Refused;
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
        let item = Item::Data { k, seq, payload };
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
        self.entries.insert(stamp, Entry { item, cost, ts: ts_ms, lossless });
        self.data_frames += 1;
        self.data_bytes += cost;
        let evicted = self.enforce_bounds(k);
        if evicted > 0 {
            Pushed::Evicted(evicted)
        } else if superseded {
            Pushed::Superseded
        } else {
            Pushed::Queued
        }
    }

    /// Queue one control frame. `false` when the reserved lane is full: the caller ends the session.
    pub fn push_ctrl(&mut self, control: Control) -> bool {
        if self.ctrl_frames >= self.limits.ctrl_frames {
            return false;
        }
        let stamp = self.next_stamp();
        let item = Item::Ctrl(control);
        self.note_queued(&item);
        self.entries.insert(stamp, Entry { item, cost: 0, ts: 0, lossless: false });
        self.ctrl_frames += 1;
        true
    }

    /// The class `key` runs at right now: the verb's own, or the subscriber's floor if that is stronger.
    pub fn class_of(&self, key: (u32, Verb)) -> Delivery {
        stronger(key.1.base(), self.floors.get(&key).copied().unwrap_or(Delivery::Conflatable))
    }

    /// Whether a frame of `key` pushed now is queued under the LOSSLESS rule: evictable only with a
    /// disclosed hole, numbered into the ledger. A data verb at [`Delivery::Control`] is queued like a
    /// lossless one (the reserved control lane is for the wire's own frames; a data frame under it
    /// would be unbounded).
    pub fn is_lossless(&self, key: (u32, Verb)) -> bool {
        self.class_of(key) >= Delivery::Lossless
    }

    /// Set the class FLOOR of `lane` on key `k` to `floor` (the weakest, [`Delivery::Conflatable`],
    /// clears it). The floor applies to every verb of the lane (`vike_data::delivery::lane_verbs`) that
    /// has a wire name ([`Verb::gap_verb`]) and whose own class is weaker; every other verb keeps its
    /// own, so a floor is never a weakening and a lane of already-lossless verbs is unchanged.
    ///
    /// Returns the lane's class from now on (the strongest over its verbs) when it CHANGED, else
    /// `None`: the caller announces a change ([`Control::Delivery`]) and says nothing about a no-op.
    /// Frames already queued keep the rule they entered under (`Entry::lossless`).
    pub fn set_floor(&mut self, k: u32, lane: WireLane, floor: Delivery) -> Option<Delivery> {
        let mut before = Delivery::Conflatable;
        let mut after = Delivery::Conflatable;
        for sink_verb in lane_verbs(lane.stream_label()) {
            let Some(verb) = Verb::from_sink_verb(*sink_verb) else { continue };
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

    /// The floor `lane` of key `k` runs under: the strongest over its verbs, [`Delivery::Conflatable`]
    /// when none is raised.
    pub fn floor_of(&self, k: u32, lane: WireLane) -> Delivery {
        lane_verbs(lane.stream_label())
            .iter()
            .filter_map(|v| Verb::from_sink_verb(*v))
            .filter_map(|verb| self.floors.get(&(k, verb)).copied())
            .fold(Delivery::Conflatable, stronger)
    }

    fn add_hole(&mut self, key: (u32, Verb), seq: u64, stream: WireStream, ts: i64) {
        self.ledger.entry(key).or_default().add_hole(seq, stream, ts);
        self.holey.insert(key);
    }

    /// How many frames of lane `(k, verb)` have been lost (evicted, refused or unsendable) since the
    /// key was bound: a figure that only grows while the key lives, for a sender that must know its
    /// receiver's state has diverged (`crate::sink::WireSink::book_losses`).
    pub fn lost(&self, k: u32, verb: Verb) -> u64 {
        self.ledger.get(&(k, verb)).map_or(0, |l| l.lost)
    }

    fn oldest_lossless_of(&self, k: u32) -> Option<u64> {
        Verb::GAPPABLE
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
            let own: usize = Verb::GAPPABLE
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
                    Some(Entry { item: Item::Data { k, seq, payload }, ts, .. }) => {
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
                .find(|(_, e)| matches!(e.item, Item::Data { .. }))
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
    pub fn take(&mut self) -> Option<Taken> {
        let stamp = self.first_deliverable()?;
        let entry = self.unlink(stamp)?;
        let frame_ts = entry.ts;
        let lossless = entry.lossless;
        match entry.item {
            Item::Ctrl(control) => {
                if let Control::SubOk { k, .. } = &control {
                    self.pending.remove(k);
                }
                if let Control::Status { k, .. }
                | Control::Delay { k, .. }
                | Control::Delivery { k, .. } = &control
                {
                    self.sweep(*k);
                }
                Some(Taken { gaps: Vec::new(), item: Item::Ctrl(control), undo: None })
            }
            Item::Data { k, seq, payload } => {
                let verb = payload.verb();
                let key = (k, verb);
                let mut gaps = Vec::new();
                let mut undo = None;
                if lossless {
                    let stream = payload.stream();
                    let ledger = self.ledger.entry(key).or_default();
                    let previous = ledger.last_delivered;
                    let previous_ts = ledger.last_delivered_ts;
                    let holes = ledger.take_before(seq);
                    if ledger.holes.is_empty() {
                        self.holey.remove(&key);
                    }
                    ledger.last_delivered = Some(seq);
                    ledger.last_delivered_ts = Some(frame_ts);
                    if !holes.is_empty() {
                        let opens = opens_at(previous_ts, &holes);
                        gaps = gaps_for(k, verb, previous.unwrap_or(0), seq, opens, holes);
                    }
                    undo = Some(Undo { k, verb, seq, previous, previous_ts, stream, ts: frame_ts });
                }
                self.sweep(k);
                Some(Taken { gaps, item: Item::Data { k, seq, payload }, undo })
            }
        }
    }

    /// `Gap`s for every hole that no queued frame will carry: the newest frames of a `(k, verb)` were
    /// the ones lost, so the writer discloses the loss now, naming the number the NEXT frame will
    /// carry as `to_seq`. Held keys wait for their `SubOk`.
    pub fn tail_gaps(&mut self) -> Vec<StreamDown> {
        if self.holey.is_empty() {
            return Vec::new();
        }
        let due: Vec<(u32, Verb)> = self
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
            let to = self.seqs.get(&key).copied().unwrap_or(1);
            if let Some(ledger) = self.ledger.get_mut(&key) {
                let holes = std::mem::take(&mut ledger.holes);
                let opens = opens_at(ledger.last_delivered_ts, &holes);
                let from = ledger.last_delivered.unwrap_or(0);
                out.extend(gaps_for(key.0, key.1, from, to, opens, holes));
            }
            self.holey.remove(&key);
        }
        out
    }

    /// Take back a lossless frame the writer could not send: it never reached the peer, so it is a
    /// hole (disclosed with the next survivor or at the tail) and not a delivery.
    pub fn note_unsendable(&mut self, undo: Undo) {
        let Undo { k, verb, seq, previous, previous_ts, stream, ts } = undo;
        let key = (k, verb);
        self.stats.unsendable += 1;
        // A key that was freed while the frame was in the writer's hands has no reader left to tell.
        if !self.seqs.contains_key(&key) {
            return;
        }
        if let Some(ledger) = self.ledger.get_mut(&key) {
            ledger.last_delivered = previous;
            ledger.last_delivered_ts = previous_ts;
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
        self.seqs.retain(|key, _| key.0 != k);
        self.ledger.retain(|key, _| key.0 != k);
        self.holey.retain(|key| key.0 != k);
        self.slots.retain(|key, _| key.0 != k);
        self.floors.retain(|key, _| key.0 != k);
        self.released.remove(&k);
    }
}

#[cfg(test)]
#[path = "queue_tests.rs"]
mod queue_tests;
