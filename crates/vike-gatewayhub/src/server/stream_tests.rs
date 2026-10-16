use super::*;

fn bars(symbol: &str, interval: &str) -> SubOp {
    SubOp::Bars { symbol: symbol.to_string(), interval: interval.to_string() }
}

#[test]
fn a_blank_overlong_or_control_spelling_is_refused_before_the_venue_is_asked() {
    assert!(refuse_spelling(&bars("BTCUSDT", "1m")).is_none());
    assert!(
        refuse_spelling(&SubOp::Quotes { symbol: "   ".into() })
            .is_some_and(|w| w.contains("blank"))
    );
    assert!(refuse_spelling(&bars("BTCUSDT", "")).is_some_and(|w| w.contains("interval")));
    let long = "x".repeat(GW_MAX_SYMBOL_BYTES + 1);
    assert!(refuse_spelling(&SubOp::Trades { symbol: long }).is_some_and(|w| w.contains("limit")));
    assert!(
        refuse_spelling(&SubOp::Book { symbol: "A\nB".into() })
            .is_some_and(|w| w.contains("control character"))
    );
    // The edge of the limit is accepted.
    let edge = "x".repeat(GW_MAX_SYMBOL_BYTES);
    assert!(refuse_spelling(&SubOp::Depth { symbol: edge }).is_none());
}

#[test]
fn the_writer_thread_name_is_ascii_and_fits_what_linux_keeps() {
    assert_eq!(writer_thread_name("binance"), "gwh-wr-binance");
    let name = writer_thread_name("a-very-long-venue-name");
    assert!(name.len() <= 15, "{name}");
    assert!(name.starts_with("gwh-wr-"));
    assert!(name.is_ascii());
}
