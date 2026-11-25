//! `closed_archive`: written once off the fold, read once, a torn frame and a duplicate dropped.

use std::io::Write;

use super::*;
use crate::testutil::tmp_dir;

fn row(n: u64) -> TradeClosedRow {
    TradeClosedRow {
        symbol: "BTCUSDT".into(),
        tag: String::new(),
        side: 1,
        entry_timestamp: 1_000 * n as i64,
        entry_bar: n as usize,
        entry_price: 100.0 + n as f64,
        exit_timestamp: 1_000 * n as i64 + 500,
        exit_bar: n as usize + 1,
        exit_price: 101.0 + n as f64,
        qty: 1.0,
        pnl: 1.0,
        commission: 0.25,
        net: 0.75,
    }
}

/// **The trades a writer queued are on disk once `durable` reaches them, and read back by mount and number.**
#[test]
fn queued_trades_are_durable_and_read_back_by_mount_and_number() {
    let dir = tmp_dir("archive-write");
    let mut a = TradeClosedArchive::open(&dir).unwrap();
    let sequences: Vec<u64> = (0..3).map(|n| a.enqueue("m1", n, row(n))).collect();
    assert_eq!(sequences, vec![1, 2, 3], "one sequence number per trade, in order");
    a.enqueue("m2", 0, row(7));
    a.close();
    assert_eq!(a.durable(), 4, "the thread wrote and synced them");
    drop(a);

    let read = read_closed_archive(&dir).unwrap();
    assert_eq!(read.trades["m1"].len(), 3);
    assert_eq!(read.trades["m1"][&2], row(2));
    assert_eq!(read.trades["m2"][&0], row(7));
    assert_eq!((read.torn, read.duplicates), (0, 0));
    assert_eq!(
        read_closed_archive(&tmp_dir("archive-none")).unwrap(),
        TradeClosedArchiveRead::default()
    );
}

/// **A crash mid-append, then a restart that writes again**: the torn frame the dead writer left is dropped and the
/// frames the next writer appended after it are read; the number it wrote again stands in its LAST copy.
#[test]
fn a_torn_frame_and_a_duplicate_are_dropped_and_nothing_after_them_is_lost() {
    let dir = tmp_dir("archive-torn");
    let mut a = TradeClosedArchive::open(&dir).unwrap();
    (0..2).for_each(|n| {
        a.enqueue("m1", n, row(n));
    });
    drop(a);
    // The dead writer's half frame: a length, a check and the first part of the payload.
    let mut whole = Vec::new();
    push_frame(&mut whole, &TradeClosedArchiveEntry { mount_id: "m1".into(), n: 2, row: row(2) });
    let mut f = OpenOptions::new().append(true).open(dir.join(ARCHIVE_FILE)).unwrap();
    f.write_all(&whole[..whole.len() / 2]).unwrap();
    drop(f);

    // The next process writes number 1 again (rebuilt from its Snapshot and tail, a bar index moved) and 2 and 3.
    let mut b = TradeClosedArchive::open(&dir).unwrap();
    let again = TradeClosedRow { exit_bar: 99, ..row(1) };
    b.enqueue("m1", 1, again.clone());
    b.enqueue("m1", 2, row(2));
    b.enqueue("m1", 3, row(3));
    drop(b);

    let read = read_closed_archive(&dir).unwrap();
    let m1 = &read.trades["m1"];
    assert_eq!(
        m1.keys().copied().collect::<Vec<_>>(),
        vec![0, 1, 2, 3],
        "nothing after the torn frame is lost"
    );
    assert_eq!(m1[&1], again, "the last copy of a number stands");
    assert_eq!((read.torn, read.duplicates), (1, 1));
}
