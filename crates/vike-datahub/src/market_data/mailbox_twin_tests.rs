//! The TWIN test that decision 0167's verdict 5 asks for before this mailbox may move onto the tube:
//! [`MarketDataMailbox`] and `vike_market_stream::queue::StreamQueue`, fed the SAME stream (overflow
//! included), must hand the writer identical frames.
//!
//! Both sides are read in one vocabulary: `status S`, `book S n`, `trades S n`, `gap S dropped from..to`.
//! The book lane is fed to the tube's conflatable `Depth` (latest-wins), the tape to its lossless `Trade`
//! (one print per batch, so a gap counts the same unit on both sides) and a status to its control lane.
//! A frame is named by its payload's stamp; the tube's own `seq` agrees with the hub's because both count
//! 1, 2, 3 per key.
//!
//! ⚠ **Every case DIFFERS, and that is why the mailbox stays its own queue** (owner, 2026-11-15; the
//! reopener "A subscriber whose loss rule no class and override can express" in
//! `docs/decisions/0167-one-live-data-tube-vike-market-stream-on-a-data-plane-tier.md`'s "What would reopen this").
//! Each case pins what EACH queue delivers, so a change to either queue's order shows here first.

use super::*;
use vike_market_stream::queue::{
    StreamQueue, StreamQueueControl, StreamQueueLimits, StreamQueuePayload,
};
use vike_market_stream::wire::{StreamDown, StreamWordWire};
use vike_marketdata::TickTrade;
use vike_model::{StreamLabel, StreamStatus};

/// One step of the shared stream.
#[derive(Clone, Copy)]
enum Step {
    /// The `n`th book the hub built for symbol `S`.
    Book(&'static str, u64),
    /// A one-print tape batch of `S` at hub `seq` `n`.
    Trades(&'static str, u64),
    /// A status transition of `S`.
    Status(&'static str),
    /// The writer takes everything it can.
    Drain,
}

const SYMBOLS: [&str; 2] = ["A", "B"];

fn k_of(symbol_name: &str) -> u32 {
    SYMBOLS.iter().position(|x| *x == symbol_name).expect("a known symbol") as u32
}

fn key(symbol_name: &str, lane: StreamLabel) -> MarketDataKey {
    MarketDataKey { venue: "binance".into(), symbol: symbol_name.into(), lane }
}

fn outgoing(step: Step) -> MarketDataMailboxOutgoing {
    let (key, lane, text, sequence, ticks) = match step {
        Step::Book(s, n) => (
            key(s, StreamLabel::Depth),
            MarketDataMailboxLaneClass::Book,
            format!("book {s} {n}"),
            n,
            0,
        ),
        Step::Trades(s, n) => (
            key(s, StreamLabel::Trades),
            MarketDataMailboxLaneClass::Tape,
            format!("trades {s} {n}"),
            n,
            1,
        ),
        Step::Status(s) => (
            key(s, StreamLabel::Depth),
            MarketDataMailboxLaneClass::Control,
            format!("status {s}"),
            0,
            0,
        ),
        Step::Drain => unreachable!("a drain is not a frame"),
    };
    MarketDataMailboxOutgoing {
        key,
        lane,
        bytes: Arc::new(text.into_bytes()),
        sequence,
        ticks,
        hub_dropped: 0,
    }
}

fn drain_mailbox(mailbox: &MarketDataMailbox, out: &mut Vec<String>) {
    while let MarketDataMailboxReceive::Frame { bytes, owed_gap } =
        mailbox.recv_timeout(Duration::ZERO)
    {
        if let Some((key, dropped, from, to)) = owed_gap {
            out.push(format!("gap {} {dropped} {from}..{to}", key.symbol));
        }
        out.push(String::from_utf8((*bytes).clone()).expect("the twin writes text"));
    }
}

fn push_stream(queue: &mut StreamQueue, step: Step) {
    match step {
        Step::Book(s, n) => {
            let payload = StreamQueuePayload::Depth {
                tick_size: 0.0,
                bids: Vec::new(),
                asks: Vec::new(),
                timestamp: n as i64,
            };
            queue.push_data(k_of(s), payload);
        }
        Step::Trades(s, n) => {
            let trade = TickTrade {
                timestamp: n as i64,
                local_timestamp: 0,
                price: 1.0,
                size: 1.0,
                is_buyer_maker: false,
                symbol: Default::default(),
            };
            queue.push_data(k_of(s), StreamQueuePayload::Trade(trade));
        }
        Step::Status(s) => {
            let status = StreamQueueControl::Status {
                k: k_of(s),
                stream: StreamWordWire::Lane(StreamLabel::Depth),
                status: StreamStatus::GapStart { at_timestamp_ms: 0 },
            };
            assert!(queue.push_control(status), "the control lane has room");
        }
        Step::Drain => unreachable!("a drain is not a frame"),
    }
}

fn read_stream(frame: StreamDown) -> String {
    match frame {
        StreamDown::Depth { k, timestamp, .. } => {
            format!("book {} {timestamp}", SYMBOLS[k as usize])
        }
        StreamDown::Trade { k, trade, .. } => {
            format!("trades {} {}", SYMBOLS[k as usize], trade.timestamp)
        }
        StreamDown::Status { k, .. } => format!("status {}", SYMBOLS[k as usize]),
        StreamDown::Gap { k, dropped, from_sequence, to_sequence, .. } => {
            format!("gap {} {dropped} {from_sequence}..{to_sequence}", SYMBOLS[k as usize])
        }
        other => panic!("the twin feeds no such frame: {other:?}"),
    }
}

/// The writer's order (`vike_market_stream::sink::DataClientSinkStream::next`): a frame with its gaps,
/// and the tail gaps only once the queue has nothing else.
fn drain_stream(queue: &mut StreamQueue, out: &mut Vec<String>) {
    loop {
        if let Some(taken) = queue.take() {
            out.extend(taken.gaps.into_iter().map(read_stream));
            out.push(read_stream(taken.item.into_frame()));
            continue;
        }
        let gaps = queue.tail_gaps();
        if gaps.is_empty() {
            return;
        }
        out.extend(gaps.into_iter().map(read_stream));
    }
}

/// Feed `steps` to both queues at a frame bound of `frames` (bytes never bind) and read what each hands
/// its writer: `(the mailbox's, the tube's)`.
fn twin(steps: &[Step], frames: usize) -> (Vec<String>, Vec<String>) {
    let mailbox = MarketDataMailbox::with_bounds(frames, 1 << 30);
    let mut queue = StreamQueue::new(StreamQueueLimits {
        bytes: 1 << 30,
        frames,
        control_frames: MARKET_DATA_MAILBOX_CONTROL,
    });
    let (mut old, mut new) = (Vec::new(), Vec::new());
    for step in steps {
        match step {
            Step::Drain => {
                drain_mailbox(&mailbox, &mut old);
                drain_stream(&mut queue, &mut new);
            }
            frame => {
                mailbox.push(outgoing(*frame));
                push_stream(&mut queue, *frame);
            }
        }
    }
    (old, new)
}

/// The mailbox drains its control lane FIRST; the tube keeps one emission order, so a status waits
/// behind the data queued before it.
#[test]
fn twin_a_status_jumps_the_queue_in_the_mailbox_and_waits_its_turn_on_the_tube() {
    let (old, new) =
        twin(&[Step::Book("A", 1), Step::Trades("A", 1), Step::Status("A"), Step::Drain], 64);
    assert_eq!(old, ["status A", "book A 1", "trades A 1"]);
    assert_eq!(new, ["book A 1", "trades A 1", "status A"]);
}

/// A superseding book keeps its key's PLACE in the mailbox; on the tube it takes the place of its own
/// emission.
#[test]
fn twin_a_superseding_book_keeps_its_place_in_the_mailbox_and_moves_back_on_the_tube() {
    let (old, new) =
        twin(&[Step::Book("A", 1), Step::Book("B", 1), Step::Book("A", 2), Step::Drain], 64);
    assert_eq!(old, ["book A 2", "book B 1"]);
    assert_eq!(new, ["book B 1", "book A 2"]);
}

/// The mailbox drains every pending book before any tape batch; the tube keeps emission order.
#[test]
fn twin_books_go_before_the_tape_in_the_mailbox_only() {
    let (old, new) = twin(&[Step::Trades("A", 1), Step::Book("B", 1), Step::Drain], 64);
    assert_eq!(old, ["book B 1", "trades A 1"]);
    assert_eq!(new, ["trades A 1", "book B 1"]);
}

/// A full tape: the mailbox DISCARDS the newest batch and discloses it on the next post-hole batch; the
/// tube evicts the pushing key's OLDEST frame and discloses it ahead of the first survivor.
#[test]
fn twin_a_full_tape_loses_the_newest_print_in_the_mailbox_and_the_oldest_on_the_tube() {
    let steps = [
        Step::Trades("A", 1),
        Step::Trades("A", 2),
        Step::Trades("A", 3),
        Step::Trades("A", 4),
        Step::Drain,
        Step::Trades("A", 5),
        Step::Drain,
    ];
    let (old, new) = twin(&steps, 3);
    assert_eq!(old, ["trades A 1", "trades A 2", "trades A 3", "gap A 1 3..5", "trades A 5"]);
    assert_eq!(new, ["gap A 1 0..2", "trades A 2", "trades A 3", "trades A 4", "trades A 5"]);
}
