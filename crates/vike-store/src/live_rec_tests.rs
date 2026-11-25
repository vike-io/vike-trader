use super::*;
use crate::HistoryStoreDataFusion;
use std::time::Instant;
use vike_data::{HistoryStore, TimestampRange};
use vike_marketdata::{L2Book, L2BookUpdate, L2BookUpdateKind, TickQuote, TickTrade};
use vike_model::{EquitySample, StreamStatus};

fn quote(ts: i64, bid: f64) -> TickQuote {
    TickQuote {
        timestamp: ts,
        local_timestamp: 0,
        bid,
        ask: bid + 0.5,
        bid_size: 1.0,
        ask_size: 2.0,
        symbol: Default::default(),
    }
}

fn trade(ts: i64, price: f64) -> TickTrade {
    TickTrade {
        timestamp: ts,
        local_timestamp: 0,
        price,
        size: 1.0,
        is_buyer_maker: false,
        symbol: Default::default(),
    }
}

fn open_store() -> (tempfile::TempDir, Arc<HistoryStoreDataFusion>) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(HistoryStoreDataFusion::open(dir.path()).unwrap());
    (dir, store)
}

fn quote_series_dir(root: &std::path::Path, venue: &str, symbol: &str) -> std::path::PathBuf {
    root.join("kind=quote").join(format!("venue={venue}")).join(format!("symbol={symbol}"))
}

/// How long a test may wait for a [`DropReport`] to reach the observer before calling it
/// broken. Twin of the bound `a_permanently_failed_flush_discards_and_counts_the_rows` already
/// used, hoisted so the two cannot drift apart.
///
/// It bounds only how long an ACTUALLY BROKEN test takes to fail — the waits below poll every
/// 2ms and break the instant the condition holds — so it never slows the happy path.
///
/// **10s is ~8x the worst latency ever measured, and it is not papering over a lost edge.**
/// A report's latency is bounded by the writer thread's store commits, not by
/// `drop_report_every` (see [`RecorderConfig::drop_report_every`]): MEASURED on the the CI box CI
/// box, the first report of a ~19,000-row burst lands ~80ms after the burst when idle, p90
/// 176ms / max 350ms under a 4-way parallel test load, and max 1,305ms under an 8-way one.
/// Across 260 instrumented runs the report was late 6 times and **lost zero times** — which is
/// the discriminator that says a deadline is the right tool here at all. Where a report CAN be
/// permanently lost, no deadline is big enough and widening one is the bug (see
/// `crates/vike-bridge-core/src/websocket_session/user.rs`'s `run_user_resync`, whose 3s -> 30s
/// widening then failed at 30.083s because the edge was gone, not slow).
const REPORT_DEADLINE: Duration = Duration::from_secs(10);

#[path = "live_rec_tests/flush.rs"]
#[cfg(test)]
mod flush;

#[path = "live_rec_tests/loss_report.rs"]
#[cfg(test)]
mod loss_report;

#[path = "live_rec_tests/retry.rs"]
#[cfg(test)]
mod retry;
