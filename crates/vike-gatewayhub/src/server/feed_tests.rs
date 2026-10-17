use super::*;

/// A queue bounded in frames only: the byte bound is the subject of its own tests.
const NO_BYTE_BOUND: usize = usize::MAX;

fn row(ts: i64) -> OptionTicker {
    OptionTicker::new(ts, "BTC-31JUL26-67000-C")
}

#[test]
fn a_full_queue_drops_its_oldest_rows_and_counts_them() {
    let queue = FeedQueue::new(3, NO_BYTE_BOUND);
    let sink = QueueSink(Arc::clone(&queue));
    for ts in 0..8 {
        sink.ticker(row(ts));
    }
    assert_eq!(queue.dropped(), 5);
    let mut got = Vec::new();
    let mut lapsed_flags = Vec::new();
    while let FeedNext::Frame(frame, lapsed) = queue.next(Duration::from_millis(1)) {
        if let OptionsFeedDown::Ticker(t) = frame {
            got.push(t.ts);
        }
        lapsed_flags.push(lapsed);
    }
    assert_eq!(got, vec![5, 6, 7], "the NEWEST rows survive");
    assert_eq!(lapsed_flags, vec![true, false, false], "one drop episode is one lapse");
}

#[test]
fn a_row_with_a_non_finite_float_is_refused_and_counted() {
    let queue = FeedQueue::new(8, NO_BYTE_BOUND);
    let sink = QueueSink(Arc::clone(&queue));
    let mut bad = row(1);
    bad.mark = Some(f64::NAN);
    sink.ticker(bad);
    let mut batch = vec![row(2), row(3)];
    batch[1].mark_iv = Some(f64::INFINITY);
    sink.marks(batch);
    sink.marks(vec![row(4)]);
    assert_eq!(queue.refused(), 2);
    let frames: Vec<_> = std::iter::from_fn(|| match queue.next(Duration::from_millis(1)) {
        FeedNext::Frame(f, _) => Some(f),
        _ => None,
    })
    .collect();
    assert!(
        matches!(frames.as_slice(), [OptionsFeedDown::Marks(rows)] if rows.len() == 1),
        "{frames:?}"
    );
}

#[test]
fn finish_discards_the_backlog_and_the_first_reason_stands() {
    let queue = FeedQueue::new(8, NO_BYTE_BOUND);
    let sink = QueueSink(Arc::clone(&queue));
    sink.ticker(row(1));
    queue.finish(Some(ByeReason::ServerStopping));
    queue.finish(None);
    sink.ticker(row(2)); // ignored once ended
    assert!(queue.is_ended());
    assert!(matches!(
        queue.next(Duration::from_millis(1)),
        FeedNext::End(Some(ByeReason::ServerStopping))
    ));
}

#[test]
fn an_idle_queue_reports_idle_after_the_wait() {
    let queue = FeedQueue::new(8, NO_BYTE_BOUND);
    let started = Instant::now();
    assert!(matches!(queue.next(Duration::from_millis(30)), FeedNext::Idle));
    assert!(started.elapsed() >= Duration::from_millis(25));
}

#[test]
fn a_push_wakes_a_waiting_writer() {
    let queue = FeedQueue::new(8, NO_BYTE_BOUND);
    let waiter = {
        let queue = Arc::clone(&queue);
        std::thread::Builder::new()
            .name("feed-test-wr".into())
            .spawn(move || matches!(queue.next(Duration::from_secs(5)), FeedNext::Frame(..)))
            .expect("spawn")
    };
    std::thread::sleep(Duration::from_millis(50));
    QueueSink(Arc::clone(&queue)).ticker(row(1));
    assert!(waiter.join().expect("join"), "the writer slept through a push");
}

/// A batch of `n` rows, the shape of a `markprice.options` frame.
fn batch(first_ts: i64, n: usize) -> Vec<OptionTicker> {
    (0..n).map(|i| row(first_ts + i as i64)).collect()
}

fn queued_ts(queue: &FeedQueue) -> Vec<i64> {
    std::iter::from_fn(|| match queue.next(Duration::from_millis(1)) {
        FeedNext::Frame(OptionsFeedDown::Marks(rows), _) => Some(rows[0].ts),
        FeedNext::Frame(OptionsFeedDown::Ticker(t), _) => Some(t.ts),
        _ => None,
    })
    .collect()
}

/// **The queue is bounded in BYTES as well as frames.** A `Marks` frame is a whole venue batch, so a
/// frame count of 4,096 let a parked consumer hold 4,096 chains. With room for about three 100-row
/// batches, a hundred pushes leave the newest three and count the rest as dropped, however large the
/// frame bound is.
#[test]
fn a_parked_consumer_holds_a_byte_bound_of_batches_not_a_frame_count_of_them() {
    let one = frame_cost(&OptionsFeedDown::Marks(batch(0, 100)));
    let queue = FeedQueue::new(4_096, one * 3 + one / 2);
    let sink = QueueSink(Arc::clone(&queue));
    for ts in 0..100 {
        sink.marks(batch(ts * 1_000, 100));
    }
    assert_eq!(queue.dropped(), 97, "everything past the byte bound is dropped, and counted");
    assert_eq!(queued_ts(&queue), vec![97_000, 98_000, 99_000], "the NEWEST batches survive");
}

/// A frame bigger than the whole byte bound is admitted ALONE (everything older goes): refusing it
/// would be a silent loss, and the writer's frame ceiling already bounds how large it can be.
#[test]
fn a_frame_over_the_whole_byte_bound_is_admitted_alone() {
    let queue = FeedQueue::new(4_096, 1_000);
    let sink = QueueSink(Arc::clone(&queue));
    sink.ticker(row(1));
    sink.marks(batch(10, 50));
    assert_eq!(queue.dropped(), 1, "the older row made way for the oversized batch");
    sink.marks(batch(20, 50));
    assert_eq!(queue.dropped(), 2, "and an oversized batch replaces an oversized batch");
    assert_eq!(queued_ts(&queue), vec![20]);
}

/// The count is returned as frames leave: after a drain the queue accepts a full bound again.
#[test]
fn draining_the_queue_gives_its_bytes_back() {
    let one = frame_cost(&OptionsFeedDown::Marks(batch(0, 20)));
    let queue = FeedQueue::new(4_096, one * 2);
    let sink = QueueSink(Arc::clone(&queue));
    for round in 0..5 {
        sink.marks(batch(round * 100, 20));
        sink.marks(batch(round * 100 + 50, 20));
        assert_eq!(queued_ts(&queue).len(), 2, "round {round}: both batches fit again");
    }
    assert_eq!(queue.dropped(), 0);
}
