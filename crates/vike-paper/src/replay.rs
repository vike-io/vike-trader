//! The R7 replay driver: `replay_bars` pushes a bar list through the live ingest lane.

use vike_model::Bar;

/// The BacktestDataClient (plan R7): replay a bar list through the SAME lossless ingest
/// lane a live feed uses. The core fills + strategy-steps each close deterministically.
///
/// Takes the lane itself rather than the core handle it came from. That is the whole of what this
/// function ever used (`CoreHandle::bar_sender()` is a cheap `Clone` of exactly this sender), and
/// [`vike_exec::BarSender`] is the producer-side type venue bridges already hold — so the R7 replay
/// driver no longer names a `vike_core` type, which is what lets vike-backtest keep vike-core as a
/// DEV-dependency instead of a real one (see this crate's Cargo.toml). Live callers pass
/// `&handle.bar_sender()`; a test may equally pass the `vike_exec::event_channel()` twin's sender.
pub fn replay_bars(
    sender: &vike_exec::BarSender,
    venue: &str,
    symbol: &str,
    interval: &str,
    bars: &[Bar],
) {
    for bar in bars {
        sender
            .close(vike_exec::BarUpdate {
                venue: venue.to_string(),
                symbol: symbol.to_string(),
                interval: interval.to_string(),
                bar: bar.clone(),
            })
            .expect("core alive during replay");
    }
}
