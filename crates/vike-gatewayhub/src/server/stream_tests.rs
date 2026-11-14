use super::*;

fn bars(symbol: &str, interval: &str) -> SubscriptionOperation {
    SubscriptionOperation::Bars { symbol: symbol.to_string(), interval: interval.to_string() }
}

#[test]
fn a_blank_overlong_or_control_spelling_is_refused_before_the_venue_is_asked() {
    let refuse_spelling = |op: &SubscriptionOperation| op.refuse_spelling(GATEWAY_MAX_SYMBOL_BYTES);
    assert!(refuse_spelling(&bars("BTCUSDT", "1m")).is_none());
    assert!(
        refuse_spelling(&SubscriptionOperation::Quotes { symbol: "   ".into() })
            .is_some_and(|w| w.contains("blank"))
    );
    assert!(refuse_spelling(&bars("BTCUSDT", "")).is_some_and(|w| w.contains("interval")));
    let long = "x".repeat(GATEWAY_MAX_SYMBOL_BYTES + 1);
    assert!(
        refuse_spelling(&SubscriptionOperation::Trades { symbol: long })
            .is_some_and(|w| w.contains("limit"))
    );
    assert!(
        refuse_spelling(&SubscriptionOperation::Book { symbol: "A\nB".into() })
            .is_some_and(|w| w.contains("control character"))
    );
    // The edge of the limit is accepted.
    let edge = "x".repeat(GATEWAY_MAX_SYMBOL_BYTES);
    assert!(refuse_spelling(&SubscriptionOperation::Depth { symbol: edge }).is_none());
}

#[test]
fn the_writer_thread_name_is_ascii_and_fits_what_linux_keeps() {
    use vike_market_stream::market::venue_thread_name;
    assert_eq!(venue_thread_name("gw-wr-", "binance"), "gw-wr-binance");
    // The reader thread of the gateway client's tube takes the same rule under its own prefix (15 bytes:
    // it fits since P4.5c's `gwh-` became `gw-`); a venue that does not fit is cut at the 15 bytes Linux keeps.
    assert_eq!(venue_thread_name("gw-data-", "binance"), "gw-data-binance");
    let name = venue_thread_name("gw-wr-", "a-very-long-venue-name");
    assert!(name.len() <= 15, "{name}");
    assert!(name.starts_with("gw-wr-"));
    assert!(name.is_ascii());
}
