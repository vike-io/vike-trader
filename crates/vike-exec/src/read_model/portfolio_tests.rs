use super::*;
use crate::{BalanceMode, PositionView, TradingState, VenueBlock};

fn vb(venue: &str, equity: f64, positions: Vec<PositionView>) -> VenueBlock {
    VenueBlock {
        venue: venue.into(),
        account: None,
        route_key: venue.into(),
        symbol: String::new(),
        extra_symbols: Vec::new(),
        mode: None,
        balance: 0.0,
        realized_pnl: 1.5,
        fees_paid: 0.0,
        funding_paid: 0.0,
        balance_mode: BalanceMode::Delta,
        equity,
        unrealized: 2.5,
        missing_prices: 0,
        margin_used: 42.0,
        free_bp: 0.0,
        margin_ratio: 0.0,
        fee_schedule: None,
        trading_state: TradingState::Active,
        multipliers: Default::default(),
        multiplier_default: 1.0,
        positions,
    }
}

fn pos(venue: &str, symbol: &str, size: f64) -> PositionView {
    PositionView {
        venue: venue.into(),
        symbol: symbol.into(),
        position_side: "BOTH".into(),
        size,
        avg_px: 0.0,
        unrealized: 0.0,
        mark_source: None,
        leverage: 0.0,
        liq_price: 0.0,
        margin_mode: vike_model::MarginMode::Cross,
        isolated_margin: None,
    }
}

/// ⚠ **The the CI box shape, measured 2026-08-17.** Nine paper mounts seeded 1000 each, still
/// `Delta`, plus one bybit mount that `VIKE_RECONCILE=1` flipped to `Authoritative` by adopting
/// the SHARED UNIFIED account's 53647 USDT `walletBalance`. `equity_total` sums the two into
/// 62647.106..., which is neither the account's cash nor the daemon's book — so the partition
/// has to keep them apart, and each side has to be reachable on its own.
#[test]
fn the_wallet_half_and_the_book_half_are_separately_reachable() {
    let mut paper = vb("binance", 1_000.0, vec![]);
    paper.balance_mode = BalanceMode::Delta;
    let mut wallet = vb("bybit", 53_647.10600813, vec![]);
    wallet.balance_mode = BalanceMode::Authoritative;
    let mut venues = vec![paper];
    for v in ["okx", "hyperliquid", "aster", "deribit", "alpaca", "ctrader", "ig", "oanda"] {
        let mut b = vb(v, 1_000.0, vec![]);
        b.balance_mode = BalanceMode::Delta;
        venues.push(b);
    }
    venues.push(wallet);
    let pf = Portfolio { venues, ..Default::default() };

    assert_eq!(pf.equity_book_total(), 9_000.0, "nine paper mounts at 1000 seed each");
    assert_eq!(
        pf.equity_wallet_total(),
        53_647.10600813,
        "the venue-attested half is bybit's whole-account wallet, alone"
    );
    assert_eq!(pf.wallet_venues(), vec!["bybit"], "and the report can name whose wallet it is");
    // The two halves partition the sum — which is the point: the sum EXISTS, it just may not
    // be printed as one figure. Tolerance, not `to_bits`: two py_sums over subsequences are
    // not required to reassociate to the whole one.
    let total = vike_model::py_sum(pf.venues.iter().map(|v| v.equity));
    assert!((pf.equity_book_total() + pf.equity_wallet_total() - total).abs() < 1e-9);
}

#[test]
fn a_pure_paper_core_has_no_wallet_half_and_a_fully_live_one_has_no_book_half() {
    let mut paper = Portfolio {
        venues: vec![vb("binance", 1_000.0, vec![]), vb("okx", 2_000.0, vec![])],
        ..Default::default()
    };
    assert_eq!(paper.equity_book_total(), 3_000.0);
    assert_eq!(paper.equity_wallet_total(), 0.0, "nothing has ever attested a balance");
    assert!(paper.wallet_venues().is_empty());

    for v in paper.venues.iter_mut() {
        v.balance_mode = BalanceMode::Authoritative;
    }
    assert_eq!(paper.equity_book_total(), 0.0, "no book-kept block left");
    assert_eq!(paper.equity_wallet_total(), 3_000.0);
    assert_eq!(paper.wallet_venues(), vec!["binance", "okx"], "registration order, both named");
}

#[test]
fn per_venue_accessors_read_the_right_block() {
    let pf = Portfolio {
        venues: vec![
            vb("binance", 10_000.0, vec![pos("binance", "BTCUSDT", 2.0)]),
            vb("bybit", 5_000.0, vec![pos("bybit", "BTCUSDT", -0.5)]),
        ],
        ..Default::default()
    };
    assert_eq!(pf.equity("binance"), 10_000.0);
    assert_eq!(pf.equity("bybit"), 5_000.0);
    assert_eq!(pf.unrealized_pnl("binance"), 2.5);
    assert_eq!(pf.realized_pnl("bybit"), 1.5);
    assert_eq!(pf.margin_used("binance"), 42.0);
    assert!(pf.venue("binance").is_some());
    // Unknown venue reads as absent, never panics.
    assert!(pf.venue("okx").is_none());
    assert_eq!(pf.equity("okx"), 0.0);
    assert_eq!(pf.unrealized_pnl("okx"), 0.0);
}

#[test]
fn net_position_sums_signed_size_across_venues() {
    let pf = Portfolio {
        venues: vec![
            vb("binance", 0.0, vec![pos("binance", "BTCUSDT", 2.0)]),
            vb("bybit", 0.0, vec![pos("bybit", "BTCUSDT", -0.5), pos("bybit", "ETHUSDT", 3.0)]),
        ],
        ..Default::default()
    };
    // BTCUSDT: +2.0 (binance) + -0.5 (bybit) = +1.5 → net long
    assert_eq!(pf.net_position("BTCUSDT"), 1.5);
    assert!(pf.is_net_long("BTCUSDT"));
    assert!(!pf.is_net_short("BTCUSDT"));
    assert!(!pf.is_flat("BTCUSDT"));
    // ETHUSDT: only bybit +3.0 → long
    assert!(pf.is_net_long("ETHUSDT"));
    // Unknown symbol nets flat.
    assert_eq!(pf.net_position("NOPE"), 0.0);
    assert!(pf.is_flat("NOPE"));
}

#[test]
fn missing_price_instruments_names_only_open_unpriceable_positions() {
    let priced = PositionView {
        mark_source: Some(crate::PriceSource::Mark),
        ..pos("binance", "BTCUSDT", 1.0)
    };
    let unpriced_open = pos("binance", "WEIRDCOIN", 5.0); // mark_source None (see `pos`)
    let unpriced_flat = pos("bybit", "CLOSEDCOIN", 0.0);
    let unpriced_open_other_venue = pos("bybit", "ILLIQ", -2.0);
    let pf = Portfolio {
        venues: vec![
            vb("binance", 0.0, vec![priced, unpriced_open]),
            vb("bybit", 0.0, vec![unpriced_flat, unpriced_open_other_venue]),
        ],
        ..Default::default()
    };
    assert_eq!(
        pf.missing_price_instruments(),
        vec![
            ("binance".to_string(), "WEIRDCOIN".to_string()),
            ("bybit".to_string(), "ILLIQ".to_string()),
        ],
        "priced rows and FLAT leftovers are both excluded; venue-block order is preserved"
    );
}

#[test]
fn missing_price_instruments_is_empty_when_everything_prices() {
    let pf = Portfolio {
        venues: vec![vb(
            "binance",
            0.0,
            vec![PositionView {
                mark_source: Some(crate::PriceSource::Mark),
                ..pos("binance", "BTCUSDT", 1.0)
            }],
        )],
        ..Default::default()
    };
    assert!(pf.missing_price_instruments().is_empty());
}

#[test]
fn perfectly_hedged_symbol_is_flat() {
    let pf = Portfolio {
        venues: vec![
            vb("binance", 0.0, vec![pos("binance", "BTCUSDT", 1.0)]),
            vb("bybit", 0.0, vec![pos("bybit", "BTCUSDT", -1.0)]),
        ],
        ..Default::default()
    };
    assert_eq!(pf.net_position("BTCUSDT"), 0.0);
    assert!(pf.is_flat("BTCUSDT"));
    assert!(!pf.is_net_long("BTCUSDT"));
    assert!(!pf.is_net_short("BTCUSDT"));
}

#[test]
fn gross_and_venue_net_position() {
    let pf = Portfolio {
        venues: vec![
            vb("binance", 0.0, vec![pos("binance", "BTCUSDT", 2.0)]),
            vb("bybit", 0.0, vec![pos("bybit", "BTCUSDT", -0.5), pos("bybit", "ETHUSDT", 3.0)]),
        ],
        ..Default::default()
    };
    // gross never nets long against short: |2| + |-0.5| = 2.5 for BTC across venues.
    assert_eq!(pf.gross_position("BTCUSDT"), 2.5);
    assert_eq!(pf.gross_position("ETHUSDT"), 3.0);
    // per-venue slice of net_position.
    assert_eq!(pf.venue_net_position("binance", "BTCUSDT"), 2.0);
    assert_eq!(pf.venue_net_position("bybit", "BTCUSDT"), -0.5);
    // unknown venue / symbol → 0.0 (never a panic).
    assert_eq!(pf.venue_net_position("okx", "BTCUSDT"), 0.0);
    assert_eq!(pf.gross_position("NOPE"), 0.0);
}
