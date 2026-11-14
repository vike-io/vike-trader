//! REGRESSION GUARD: the A-S maker QUOTES a BTC-SCALE asset (~$64k mid) when mounted on
//! the `crypto` preset ([`vike_market_making::SpreadMakerPreset::Crypto`]), its floor armed from the venue lane as the
//! daemon arms it. `mount_scripted.rs` covers a 0–1 Polymarket mid; this
//! feeds the SAME production mount $64k quotes (hyperliquid/BTC, $1 tick) and checks a resting
//! two-sided quote straddling the mid on the $1 grid. No network, no creds.
//!
//! Non-vacuous: a `[tick, 1−tick]` clamp puts the ask BELOW `s ≈ 64767`, so no quote (seen live:
//! `vike-tradehub`'s hyperliquid/BTC mount posted 0 orders in 5 min against ~19 quotes/25s).
//! `crypto` sets [`vike_model::PriceDomain::Unbounded`] + [`vike_model::VarianceMode::RawLocal`]
//! + a `min_half_spread_ticks` floor (the sub-tick $-scale spread guard).

use std::time::Duration;
use vike_data::DataClientSink;
use vike_log::poll::wait_until;
use vike_marketdata::TickQuote;
use vike_strategies_check::makers::spread_maker::SpreadMaker;
use vike_tradehub_core::{
    MountPlacement, StrategyCorePaperConfig, TickSink, build_paper_strategy_core_with,
};

const INTERVAL_MS: i64 = 60_000;

#[test]
fn maker_quotes_on_btc_scale_prices_via_crypto_mount() {
    vike_log::test_init();

    // RawLocal / ConstantTau / Unbounded / 2-tick half-spread floor.
    let mut params = vike_model::params::Params::new();
    params.insert("preset", vike_market_making::SpreadMakerPreset::Crypto.word());
    params.insert("tick_size", 1.0);
    params.insert("qty", 0.005);
    if let Some(rate) = vike_tradehub_core::maker_round_trip_fee_for("hyperliquid", "BTC") {
        params.insert("round_trip_fee_rate", rate);
    }
    let maker = SpreadMaker::from_params(&params).expect("the crypto preset reads");
    let spec = MountPlacement::new("hyperliquid", "BTC", 100_000.0, 1.0);
    let mount =
        build_paper_strategy_core_with(Box::new(maker), &spec, StrategyCorePaperConfig::default());
    let sink =
        TickSink::new(&mount.handle, "hyperliquid", "BTC", spec.interval.clone(), INTERVAL_MS);

    // BTC-scale quotes (~$64,767 mid, $2 spread) as the live HL feed emits them
    // (`quote:hyperliquid:BTC:64766/64768`).
    for i in 0..12 {
        let ts = 1_000 + i * 1_000;
        sink.quote(
            "hyperliquid",
            "BTC",
            TickQuote {
                timestamp: ts,
                local_timestamp: 0,
                bid: 64_766.0,
                ask: 64_768.0,
                bid_size: 1.0,
                ask_size: 1.0,
                symbol: "BTC".into(),
            },
        );
    }

    // A resting TWO-SIDED quote at a $64k mid, where a [0,1]-domain core returns `None`.
    let quoted = wait_until(Duration::from_secs(5), || {
        mount
            .handle
            .snapshot()
            .orders
            .iter()
            .filter(|o| o.order_type == vike_model::OrderType::Limit)
            .count()
            >= 2
    });
    let snap = mount.handle.snapshot();
    let limits: Vec<_> =
        snap.orders.iter().filter(|o| o.order_type == vike_model::OrderType::Limit).collect();
    println!("BTC-scale HL crypto mount: {} resting limit(s) after 12 quotes", limits.len());
    for o in limits.iter().take(4) {
        println!("   side={} type={} price={:?}", o.side, o.order_type, o.price);
    }
    assert!(
        quoted,
        "the A-S maker must post a two-sided quote on a $64k asset via the Unbounded crypto domain"
    );

    // Both sides, on the $1 grid, straddling the mid, with NO [0,1] wall clamp.
    let bid = snap
        .orders
        .iter()
        .find(|o| o.side > 0 && o.order_type == vike_model::OrderType::Limit)
        .expect("a resting BID quote");
    let ask = snap
        .orders
        .iter()
        .find(|o| o.side < 0 && o.order_type == vike_model::OrderType::Limit)
        .expect("a resting ASK quote");
    let (bid_price, ask_price) = (bid.price.unwrap(), ask.price.unwrap());
    assert!(bid_price < ask_price, "bid {bid_price} must be below ask {ask_price}");
    assert!(
        bid_price < 64_767.0 && 64_767.0 < ask_price,
        "straddle ~64767 mid: {bid_price}/{ask_price}"
    );
    assert!(ask_price > 1.0, "a $-scale ask clears the old [0,1] ceiling: {ask_price}");
    // on the $1 tick grid (integer prices)
    assert!((bid_price - bid_price.round()).abs() < 1e-9, "bid on the $1 grid: {bid_price}");
    assert!((ask_price - ask_price.round()).abs() < 1e-9, "ask on the $1 grid: {ask_price}");

    mount.handle.shutdown_and_join();
}
