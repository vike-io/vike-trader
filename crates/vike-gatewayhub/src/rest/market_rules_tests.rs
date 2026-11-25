use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

/// A source that answers one rules row per call and counts the calls.
struct Counted(AtomicUsize);

impl MarketRulesSource for Counted {
    fn venue(&self) -> &str {
        MARKET_RULES_VENUE
    }

    fn closed_ending(&self, start_ms: i64, _: i64) -> Result<Vec<TextRow>, SourceError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(vec![TextRow::new("polymarket_rules", "Polymarket", start_ms)])
    }

    fn closed_of_series(
        &self,
        _: &str,
        start_ms: i64,
        _: i64,
    ) -> Result<Vec<TextRow>, SourceError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(vec![TextRow::new("polymarket_rules", "Polymarket", start_ms)])
    }

    fn of_token(&self, _: &str) -> Result<Vec<TextRow>, SourceError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Vec::new())
    }
}

/// The served venue is asked once; another venue, and a fetch cancelled before the call, ask nothing.
#[test]
fn the_rules_lane_asks_its_venue_once_and_nothing_else() {
    let source = Counted(AtomicUsize::new(0));
    let ctx = FetchContext::new();
    let rows = fetch_market_rules(&source, "polymarket", &ctx, |s| s.closed_ending(7, 9)).unwrap();
    assert_eq!(rows[0].received_ms, 7);
    assert_eq!(source.0.load(Ordering::SeqCst), 1);

    let Err(SourceError::Refused(why)) =
        fetch_market_rules(&source, "kalshi", &ctx, |s| s.of_token("1"))
    else {
        panic!("another venue is refused")
    };
    assert!(why.text().contains("[polymarket]"), "{why}");
    ctx.cancel();
    assert!(fetch_market_rules(&source, "polymarket", &ctx, |s| s.of_token("1")).is_err());
    assert_eq!(source.0.load(Ordering::SeqCst), 1, "neither refusal reached the venue");
}

/// No Gamma row: the source still answers for its venue and refuses every fetch, naming why.
#[test]
fn a_missing_gamma_row_is_a_source_that_refuses_every_fetch() {
    let source = market_rules_source(&BTreeMap::new());
    assert_eq!(source.venue(), "polymarket");
    let Err(SourceError::Fetch(why)) = source.closed_ending(0, 1) else {
        panic!("an unbuilt source fails the fetch")
    };
    assert!(why.text().contains("NOT RUNNING"), "{why}");
    assert!(source.of_token("1").is_err());
    assert!(matches!(source.closed_of_series("btc-updown-5m", 0, 1), Err(SourceError::Fetch(_))));
}
