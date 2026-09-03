//! OFFLINE diagnostic → REGRESSION GUARD: the A-S maker now QUOTES a BTC-SCALE asset (~$64k mid)
//! when mounted via [`vike_run::MakerMountConfig::crypto`] — the price-domain generalization (the
//! crypto lift) landing. `mount_scripted.rs` proves it quotes at a 0–1 Polymarket mid 0.50; this
//! feeds the SAME production mount $64k quotes (venue=hyperliquid, symbol=BTC, tick_size=$1) and
//! checks the maker posts a resting two-sided quote straddling the mid, on the $1 grid. No network,
//! no creds.
//!
//! History: BEFORE the generalization the A-S core clamped every quote into `[tick, 1−tick]` and
//! required `s < ask`, so at a mid `s ≈ 64767` the ask clamped BELOW `s` → `None` → no quote (proven
//! live: `vike-tradehub`'s hyperliquid/BTC mount posted 0 orders in 5 min while the feed delivered
//! ~19 quotes/25s). [`vike_run::MakerMountConfig::crypto`] sets [`vike_model::PriceDomain::Unbounded`]
//! (no wall clamp) + [`vike_model::VarianceMode::RawLocal`] + a `min_half_spread_ticks` floor (the
//! sub-tick $-scale spread guard), so the SAME maker now prices. This test FLIPPED from asserting
//! `!quoted` (the old [0,1]-domain characterization) to asserting a real two-sided quote.

use std::time::{Duration, Instant};

use vike_data::LiveDataSink;
use vike_model::QuoteTick;
use vike_run::{build_paper_maker_core, MakerMountConfig, MakerSink};

const INTERVAL_MS: i64 = 60_000;

fn wait_until(secs: u64, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn maker_quotes_on_btc_scale_prices_via_crypto_mount() {
    vike_log::test_init();

    // The crypto-domain mount for Hyperliquid BTC: unbounded $-scale A-S (RawLocal / ConstantTau /
    // Unbounded / 2-tick half-spread floor), with venue+symbol+tick+qty threaded in.
    let cfg = MakerMountConfig::crypto("hyperliquid", "BTC", 1.0, 0.005);
    let mount = build_paper_maker_core(&cfg);
    let sink =
        MakerSink::new(&mount.handle, "hyperliquid", "BTC", cfg.interval.clone(), INTERVAL_MS);

    // Feed BTC-scale quotes (~$64,767 mid, $2 spread) exactly like the live HL feed emits
    // (proven: `quote:hyperliquid:BTC:64766/64768`). A handful of ticks drove the 0–1 maker in
    // mount_scripted; give this the same.
    for i in 0..12 {
        let ts = 1_000 + i * 1_000;
        sink.quote(
            "hyperliquid",
            "BTC",
            QuoteTick {
                ts,
                local_ts: 0,
                bid: 64_766.0,
                ask: 64_768.0,
                bid_size: 1.0,
                ask_size: 1.0,
                symbol: "BTC".into(),
            },
        );
    }

    // The generalization landing: the A-S maker now posts a resting TWO-SIDED quote at a $64k mid,
    // where the [0,1]-domain core returned `None`. (Before the crypto lift this asserted `!quoted`.)
    let quoted = wait_until(5, || {
        mount.handle.snapshot().orders.iter().filter(|o| o.order_type == "limit").count() >= 2
    });
    let snap = mount.handle.snapshot();
    let limits: Vec<_> = snap.orders.iter().filter(|o| o.order_type == "limit").collect();
    println!("BTC-scale HL crypto mount: {} resting limit(s) after 12 quotes", limits.len());
    for o in limits.iter().take(4) {
        println!("   side={} type={} price={:?}", o.side, o.order_type, o.price);
    }
    assert!(
        quoted,
        "the A-S maker must post a two-sided quote on a $64k asset via the Unbounded crypto domain"
    );

    // Both sides, on the $1 grid, straddling the ~64767 mid — and, crucially, with NO [0,1] wall
    // clamp (the ask sits well above the old `1−tick` ceiling).
    let bid = snap
        .orders
        .iter()
        .find(|o| o.side > 0 && o.order_type == "limit")
        .expect("a resting BID quote");
    let ask = snap
        .orders
        .iter()
        .find(|o| o.side < 0 && o.order_type == "limit")
        .expect("a resting ASK quote");
    let (bid_px, ask_px) = (bid.price.unwrap(), ask.price.unwrap());
    assert!(bid_px < ask_px, "bid {bid_px} must be below ask {ask_px}");
    assert!(bid_px < 64_767.0 && 64_767.0 < ask_px, "straddle ~64767 mid: {bid_px}/{ask_px}");
    assert!(ask_px > 1.0, "a $-scale ask clears the old [0,1] ceiling: {ask_px}");
    // on the $1 tick grid (integer prices)
    assert!((bid_px - bid_px.round()).abs() < 1e-9, "bid on the $1 grid: {bid_px}");
    assert!((ask_px - ask_px.round()).abs() < 1e-9, "ask on the $1 grid: {ask_px}");

    mount.handle.shutdown_and_join();
}
