//! The mount's one informational line per delayed stream (decision 0140).
use super::*;

/// One line per not-real-time stream, naming the symbol; a repeat, a flip back to real-time and a
/// real-time stream log nothing; another stream of the same symbol is its own stream.
#[tracing_test::traced_test]
#[test]
fn a_delayed_stream_is_announced_once_and_a_real_time_one_never() {
    let a = DelayAnnouncer::default();
    assert!(!a.note("ibkr", "EUR.USD.IDEALPRO", "quotes", DataDelay::RealTime));
    assert!(a.note("ibkr", "AAPL.SMART.USD", "quotes", DataDelay::DELAYED));
    assert!(!a.note("ibkr", "AAPL.SMART.USD", "quotes", DataDelay::DELAYED));
    assert!(!a.note("ibkr", "AAPL.SMART.USD", "quotes", DataDelay::RealTime));
    assert!(!a.note("ibkr", "AAPL.SMART.USD", "quotes", DataDelay::Frozen));
    assert!(a.note("ibkr", "AAPL.SMART.USD", "1m", DataDelay::Frozen));
    assert!(logs_contain("ibkr AAPL.SMART.USD quotes: market data is delayed"));
    assert!(logs_contain("ibkr AAPL.SMART.USD 1m: market data is frozen"));
    assert!(!logs_contain("EUR.USD.IDEALPRO"), "a real-time stream is never announced");
}
