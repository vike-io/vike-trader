//! LIVE Binance-spot aggTrades backfill smoke (network, NO creds — public REST):
//!     cargo test -p vike-binance --test binance_backfill_smoke -- --ignored --nocapture
//!
//! Proves SP3's `vike_binance::agg_trades_backfill_reported` end-to-end against the REAL Binance
//! public `GET /api/v3/aggTrades` endpoint (no key required, same as
//! `binance_market_data_smoke.rs`'s public WS streams): fetches the CURRENT latest BTCUSDT
//! aggTrade via `latest_agg_trade`, then backward-pages far enough to cover the last 5 minutes,
//! and asserts the walk returns real, correctly-ordered historical trades — not just what the
//! scripted-fetch unit tests in `vike_binance::trades` already cover.
//!
//! Asserts the entry point's RETURNED stop reason, not just its ticks: a mid-walk REST error used
//! to be indistinguishable from end-of-history, so this smoke would have passed on a
//! silently-truncated walk (it only checks that >100 trades came back). The `stop` assertion below
//! is what makes that failure mode visible here. `Capped` is tolerated — 20 pages may or may not
//! span 5 real minutes of BTCUSDT — but `Failed` is not.

use vike_model::TradeTick;

const SYMBOL: &str = "BTCUSDT";
const FIVE_MIN_MS: i64 = 5 * 60 * 1000;
/// Small slack on the `earliest_ts` floor assertion — the pager's page-stop test is
/// `page_min_ts <= earliest_ts` (see `backfill_agg_trades_backward`'s doc comment in
/// vike-binance), which by construction should never emit a trade below `earliest_ts`; this is
/// just defensive margin against real-data edge cases a scripted test can't exercise.
const SLACK_MS: i64 = 2_000;

#[test]
#[ignore = "network — public Binance REST, run manually (see module doc)"]
fn binance_backfill_pages_real_agg_trades() {
    vike_log::test_init();

    let (latest_id, latest_ts) = vike_binance::latest_agg_trade(SYMBOL)
        .expect("latest_agg_trade: fetch the current latest BTCUSDT aggTrade");
    let earliest_ts = latest_ts - FIVE_MIN_MS;

    let mut collected: Vec<TradeTick> = Vec::new();
    let outcome = vike_binance::agg_trades_backfill_reported(
        SYMBOL,
        latest_id,
        earliest_ts,
        20,
        &mut |tick| collected.push(tick),
    );

    assert!(!collected.is_empty(), "backfill returned no trades — real Binance data expected");
    assert!(
        !outcome.stop.is_retryable(),
        "the walk was cut short by a REST error, not by data: {:?} after {} pages",
        outcome.stop,
        outcome.pages
    );

    for t in &collected {
        assert!(
            t.ts >= earliest_ts - SLACK_MS,
            "trade ts {} is older than earliest_ts {} (- {SLACK_MS}ms slack)",
            t.ts,
            earliest_ts
        );
    }

    assert!(
        collected.windows(2).all(|w| w[0].ts <= w[1].ts),
        "backfill emission order is not ascending by ts (must be oldest-first)"
    );

    // Sanity floor: BTCUSDT trades many times per second even in quiet markets, so 5 real
    // minutes of aggTrades should comfortably clear this bar.
    assert!(
        collected.len() > 100,
        "only {} trades for 5 minutes of BTCUSDT — implausibly few for real market data",
        collected.len()
    );

    let first = collected.first().expect("non-empty (checked above)");
    let last = collected.last().expect("non-empty (checked above)");
    tracing::info!(
        target: "vike_binance",
        "backfill smoke: {} trades over {} pages, stop={:?}, ts span [{}, {}] (~{:.1} min), price first={} last={}",
        collected.len(),
        outcome.pages,
        outcome.stop,
        first.ts,
        last.ts,
        (last.ts - first.ts) as f64 / 60_000.0,
        first.price,
        last.price
    );
}
