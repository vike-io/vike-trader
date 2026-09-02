//! Live smoke for the `markprice.options` streaming feed — connects to REAL Deribit mainnet (public,
//! keyless, not geo-blocked, read-only: no auth, no order, no account). `#[ignore]`d so CI never
//! dials the venue; run it explicitly to prove the feed connects → subscribes → decodes → folds:
//!
//! ```sh
//! cargo test -p vike-deribit --test markprice_options_live_smoke -- --ignored --nocapture
//! ```
//!
//! Asserts the feed delivers a chain-wide batch within a few seconds and that the wire units hold
//! on live data (IV a plausible decimal fraction, mark a positive coin premium) — the runtime twin
//! of the fixture-pinned unit tests in `options_feed.rs`.

use std::sync::mpsc;
use std::time::Duration;

use vike_deribit::options_feed::{spawn_deribit_markprice_options_feed, MarkPriceRow, MAINNET_WS};

#[test]
#[ignore = "hits real Deribit mainnet WS; run explicitly with --ignored"]
fn markprice_options_streams_all_three_underlyings() {
    let (tx, rx) = mpsc::channel::<Vec<MarkPriceRow>>();
    // Subscribe the three index books the app wires: BTC/ETH (coin-settled) + SOL (USDC book). SOL's
    // index really is `sol_usdc` — this smoke proves all three deliver live.
    let feed = spawn_deribit_markprice_options_feed(
        MAINNET_WS.to_string(),
        vec!["btc_usd".to_string(), "eth_usd".to_string(), "sol_usdc".to_string()],
        move |rows| {
            // A dead receiver (test already asserted) just drops the batch — never panics the feed.
            let _ = tx.send(rows);
        },
    );

    // The first frame per channel is the full chain snapshot (hundreds of options). Collect batches
    // until every underlying prefix has appeared (or the window elapses).
    let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    let mut max_iv = 0.0f64;
    let deadline = std::time::Instant::now() + Duration::from_secs(25);
    while std::time::Instant::now() < deadline {
        let Ok(batch) = rx.recv_timeout(Duration::from_secs(20)) else { break };
        for row in &batch {
            let prefix = if row.instrument_name.starts_with("BTC-") {
                "BTC"
            } else if row.instrument_name.starts_with("ETH-") {
                "ETH"
            } else if row.instrument_name.starts_with("SOL_USDC-") {
                "SOL"
            } else {
                panic!("unexpected instrument {}", row.instrument_name);
            };
            // A mark is a NON-NEGATIVE premium — deep-OTM options legitimately mark at 0.0 (the REST
            // path produces `Some(0.0)` there too, and the fold handles it), so `>= 0`, not `> 0`.
            assert!(
                row.mark_price.is_finite() && row.mark_price >= 0.0,
                "mark {} is not a non-negative premium",
                row.mark_price
            );
            assert!(row.iv.is_finite() && row.iv >= 0.0, "iv {} not finite / non-negative", row.iv);
            max_iv = max_iv.max(row.iv);
            *counts.entry(prefix).or_default() += 1;
        }
        if ["BTC", "ETH", "SOL"].iter().all(|u| counts.contains_key(u)) {
            break;
        }
    }
    println!("markprice.options live rows by underlying: {counts:?}, max IV {max_iv:.4}");
    for u in ["BTC", "ETH", "SOL"] {
        assert!(counts.get(u).copied().unwrap_or(0) >= 10, "{u} chain should stream many strikes");
    }
    // The channel's load-bearing unit contract: IV is a DECIMAL fraction, NOT a percent. A percent
    // encoding would put a normal ~66% option at 66.0; decimals keep even the extreme short-dated
    // wings under ~10 — so a chain-wide max IV ≥ 10 would mean the percent/decimal contract broke.
    assert!(max_iv < 10.0, "max IV {max_iv} looks percent-encoded — the decimal contract broke");

    feed.shutdown();
}
