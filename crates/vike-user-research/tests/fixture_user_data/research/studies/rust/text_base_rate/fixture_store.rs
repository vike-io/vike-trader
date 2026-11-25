//! The store `text_base_rate` is proven on, ONE copy for both binaries that run the study
//! (`crates/vike-user-research/tests/pipeline.rs`, directly and across the study file's boundary, and
//! `crates/vike-strategy-sandbox/tests/study_child.rs`, in the real jail). `#[cfg(test)]`: a built
//! study never compiles it.
//!
//! Twenty-five news items, one every ten minutes, of two kinds: "Spot ETF inflows lift bitcoin" is
//! followed by ten rising one-minute bars, "Exchange hack drains hot wallet" (every third item) by ten
//! falling ones. Each item has a stored all-MiniLM-L6-v2-shaped vector (two numbers here) close to its
//! kind's axis. The window opens at the sixth item, so every query has neighbours with known outcomes,
//! and each kind's neighbours predict its side: every scored query is a side hit.

use std::sync::Arc;

use vike_data::{HistoryStore, HistoryStoreMemory, TextRow, TextScoreRow, TimestampRange};
use vike_marketdata::Bar;

const MINUTE: i64 = 60_000;
const ITEMS: i64 = 25;

/// Whether the item at `i` is followed by rising bars.
fn rising(i: i64) -> bool {
    i % 3 != 2
}

/// The fixture store: the texts, their vectors and the bars.
pub fn store() -> Arc<dyn HistoryStore + Send + Sync> {
    let store = HistoryStoreMemory::new();
    let mut price = 100.0;
    let mut bars = Vec::new();
    for minute in 0..=10 * ITEMS {
        bars.push(Bar::ohlcv(minute * MINUTE, price, price, price, price, 1.0));
        price += if rising(minute / 10) { 0.1 } else { -0.1 };
    }
    // One commit key per append: a store holds a key once, whatever the series.
    let written = store.append_bars("binance", "BTCUSDT", "1m", &bars, Some("bars")).unwrap();
    assert_eq!(written, bars.len());

    let mut texts = Vec::new();
    let mut scores = Vec::new();
    for i in 0..ITEMS {
        let received_ms = 10 * i * MINUTE;
        let title = if rising(i) {
            "Spot ETF inflows lift bitcoin"
        } else {
            "Exchange hack drains hot wallet"
        };
        let text = TextRow {
            published_ms: received_ms - 30_000,
            title: title.to_string(),
            url: format!("https://news.test/{i}"),
            ..TextRow::new("news", "Wire", received_ms)
        };
        let lean = 0.05 * (i % 4) as f64;
        let vector = if rising(i) { [1.0, lean] } else { [lean, 1.0] };
        scores.push(TextScoreRow {
            received_ms,
            text_id: text.text_id(),
            chunk: 0,
            scored_ms: 0,
            model: "all-minilm-l6-v2@fixture".to_string(),
            values: TextScoreRow::joined_values(&vector),
        });
        texts.push(text);
    }
    assert_eq!(store.append_texts("news", "rss", &texts, Some("texts")).unwrap(), texts.len());
    let model = "all-minilm-l6-v2";
    assert_eq!(
        store.append_text_scores("news", model, &scores, Some("scores")).unwrap(),
        scores.len()
    );
    Arc::new(store)
}

/// The study window: from the sixth item to the end of the bars.
pub fn window() -> TimestampRange {
    TimestampRange::of(60 * MINUTE, 10 * ITEMS * MINUTE)
}

/// The recipe the proofs run: the bars above, five-bar outcomes, three neighbours, a day of history.
pub fn recipe() -> String {
    "venue = \"binance\"\nsymbol = \"BTCUSDT\"\ninterval = \"1m\"\nhorizon_bars = 5\n\
     neighbours = 3\nhistory_days = 1\n"
        .to_string()
}
