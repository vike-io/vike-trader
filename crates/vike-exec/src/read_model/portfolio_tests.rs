use super::*;
use crate::{BalanceMode, PositionView, VenueBlock};
use vike_model::TradingState;

fn vb(venue: &str, equity: f64, positions: Vec<PositionView>) -> VenueBlock {
    VenueBlock {
        venue: venue.into(),
        account: None,
        route_key: venue.into(),
        symbol: String::new(),
        extra_symbols: Default::default(),
        tier: None,
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
        contract_values: Default::default(),
        contract_value_default: vike_model::ContractValue::default(),
        admits_symbols: false,
        quote_rates: Vec::new(),
        quote_rates_in: Default::default(),
        ledger_ccy: Default::default(),
        colliding_fills_refused: 0,
        fees_unconverted: 0,
        fees_at_stale_rate: 0,
        positions,
    }
}

fn pos(venue: &str, symbol: &str, size: f64) -> PositionView {
    PositionView {
        venue: venue.into(),
        symbol: symbol.into(),
        position_side: vike_model::events::PositionSide::Both,
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

// ---- currency groups (decision 0183, item 13): a total adds one currency's figures only ----------

/// A block in currency group `ccy` ("" = the dollar group) with un-round figures, so a fold-law or a
/// grouping difference shows in the bits.
fn ccy_block(venue: &str, ccy: &str, equity: f64, pnl_seed: f64) -> VenueBlock {
    let mut b = vb(venue, equity, vec![]);
    b.ledger_ccy = ccy.into();
    b.realized_pnl = pnl_seed * 1.37;
    b.fees_paid = pnl_seed * 0.011;
    b.funding_paid = -pnl_seed * 0.0073;
    b.unrealized = pnl_seed * 0.29;
    b.margin_used = pnl_seed * 3.1;
    b
}

fn bits(x: f64) -> u64 {
    x.to_bits()
}

/// Every total and every `*_in` of a group, as bits, for a comparison that cannot be a tolerance.
fn all_totals(pf: &Portfolio) -> [u64; 8] {
    [
        bits(pf.realized_pnl_total()),
        bits(pf.fees_paid_total()),
        bits(pf.funding_paid_total()),
        bits(pf.equity_book_total()),
        bits(pf.equity_wallet_total()),
        bits(pf.pnl_total()),
        bits(pf.drawdown_curve()),
        bits(pf.ledger_totals()[0].margin_used),
    ]
}

/// ⚠ **A EUR block is never added to a dollar total.** USD primary, one EUR engine beside it: every
/// `*_total` is the USD block's figure alone, bit for bit what a node WITHOUT the EUR engine reports.
#[test]
fn a_eur_block_is_never_added_to_a_usd_total() {
    let usd = ccy_block("binance", "", 1_000.7, 11.3);
    let eur = ccy_block("oanda", "EUR", 5_000.9, 70.1);
    let alone =
        Portfolio { venues: vec![usd.clone()], capital_base: 1_000.0, ..Default::default() };
    let mixed = Portfolio { venues: vec![usd, eur], capital_base: 1_000.0, ..Default::default() };

    assert_eq!(
        all_totals(&mixed),
        all_totals(&alone),
        "the EUR engine moved a dollar total: it was added across currencies"
    );
    assert!(mixed.is_multi_currency());
    assert!(!alone.is_multi_currency());
    assert_eq!(mixed.primary_ccy(), "");
    assert_eq!(mixed.wallet_venues(), alone.wallet_venues());
}

/// The EUR group's own totals are reachable, in the group's own currency, and the primary row of
/// `ledger_totals` equals the scalar totals bit for bit.
#[test]
fn each_currency_group_has_its_own_row_and_the_primary_row_is_the_scalar_totals() {
    let a = ccy_block("binance", "", 1_000.7, 11.3);
    let b = ccy_block("oanda", "EUR", 5_000.9, 70.1);
    let c = ccy_block("ig", "EUR", 2_222.2, 5.9);
    let pf = Portfolio { venues: vec![a.clone(), b.clone(), c.clone()], ..Default::default() };

    let rows = pf.ledger_totals();
    assert_eq!(rows.len(), 2, "USD and EUR");
    assert_eq!(rows[0].label(), "USD");
    assert_eq!(rows[1].label(), "EUR");
    assert_eq!(bits(rows[0].realized_pnl), bits(pf.realized_pnl_total()));
    assert_eq!(bits(rows[0].pnl), bits(pf.pnl_total()));
    assert_eq!(bits(rows[0].fees_paid), bits(pf.fees_paid_total()));
    assert_eq!(bits(rows[0].equity_book), bits(pf.equity_book_total()));
    // The EUR row folds the two EUR blocks, in registration order, with the same law.
    let eur_only = Portfolio { venues: vec![b, c], ..Default::default() };
    assert_eq!(bits(rows[1].equity), bits(vike_model::py_sum([5_000.9, 2_222.2])));
    assert_eq!(bits(rows[1].realized_pnl), bits(eur_only.realized_pnl_total()));
    assert_eq!(bits(rows[1].pnl), bits(eur_only.pnl_total()));
    assert_eq!(bits(rows[1].margin_used), bits(eur_only.ledger_totals()[0].margin_used));
    assert_ne!(bits(rows[0].realized_pnl), bits(rows[1].realized_pnl));
}

/// Three groups, first-seen order, each folded alone; a group's blocks need not be adjacent.
#[test]
fn three_currency_groups_keep_first_seen_order_and_never_mix() {
    let pf = Portfolio {
        venues: vec![
            ccy_block("a", "", 100.0, 1.0),
            ccy_block("b", "EUR", 200.0, 2.0),
            ccy_block("c", "JPY", 300.0, 3.0),
            ccy_block("d", "", 400.0, 4.0),
            ccy_block("e", "EUR", 500.0, 5.0),
        ],
        ..Default::default()
    };
    let rows = pf.ledger_totals();
    let order: Vec<&str> = rows.iter().map(|r| r.label()).collect();
    assert_eq!(order, ["USD", "EUR", "JPY"]);
    assert_eq!(rows[0].equity, 500.0, "100 + 400");
    assert_eq!(rows[1].equity, 700.0, "200 + 500");
    assert_eq!(rows[2].equity, 300.0);
    assert_eq!(pf.ledger_ccys().len(), 3);
}

/// The PRIMARY is the first block's group, whatever it is: a EUR node's totals are EUR.
#[test]
fn a_eur_primary_makes_the_totals_eur() {
    let pf = Portfolio {
        venues: vec![
            ccy_block("oanda", "EUR", 5_000.9, 70.1),
            ccy_block("binance", "", 1_000.7, 11.3),
        ],
        ..Default::default()
    };
    assert_eq!(pf.primary_ccy(), "EUR");
    let eur_alone = Portfolio { venues: vec![pf.venues[0].clone()], ..Default::default() };
    assert_eq!(all_totals(&pf), all_totals(&eur_alone));
}

/// A STOPPED EUR engine (its block stays in the list, beside the one that replaced it) stays in the EUR
/// group: it is still a fact in euros and must not leak into the dollar total, or vanish from the EUR one.
#[test]
fn a_retired_eur_block_stays_in_the_eur_group() {
    let usd = ccy_block("binance", "", 1_000.7, 11.3);
    let stopped = ccy_block("oanda", "EUR", 5_000.9, 70.1);
    let restarted = ccy_block("oanda", "EUR", 4_000.0, 9.0);
    let pf = Portfolio {
        venues: vec![usd.clone(), stopped.clone(), restarted.clone()],
        ..Default::default()
    };

    let usd_alone = Portfolio { venues: vec![usd], ..Default::default() };
    assert_eq!(bits(pf.pnl_total()), bits(usd_alone.pnl_total()), "no EUR term in the dollar pnl");
    let eur = pf.ledger_totals()[1];
    let want = vike_model::py_sum([
        stopped.realized_pnl - stopped.fees_paid + stopped.funding_paid + stopped.unrealized,
        restarted.realized_pnl - restarted.fees_paid
            + restarted.funding_paid
            + restarted.unrealized,
    ]);
    assert_eq!(bits(eur.pnl), bits(want), "the stopped block still counts, in EUR");
}

/// One group, however many blocks: `ledger_totals` has one row and every total is the plain fold over
/// all blocks (the expression a node of one currency always had).
#[test]
fn one_currency_is_the_plain_fold_over_every_block() {
    let blocks = vec![
        ccy_block("a", "", 100.3, 1.7),
        ccy_block("b", "", 200.9, 2.9),
        ccy_block("c", "", 300.1, 3.1),
    ];
    let pf = Portfolio { venues: blocks.clone(), ..Default::default() };
    assert_eq!(pf.ledger_totals().len(), 1);
    assert_eq!(
        bits(pf.realized_pnl_total()),
        bits(vike_model::py_sum(blocks.iter().map(|b| b.realized_pnl)))
    );
    assert_eq!(
        bits(pf.pnl_total()),
        bits(vike_model::py_sum(
            blocks.iter().map(|b| b.realized_pnl - b.fees_paid + b.funding_paid + b.unrealized)
        ))
    );
    assert_eq!(
        bits(pf.ledger_totals()[0].equity),
        bits(vike_model::py_sum(blocks.iter().map(|b| b.equity)))
    );
    assert!(Portfolio::default().ledger_totals().is_empty(), "no block yet: no row");
    assert_eq!(Portfolio::default().primary_ccy(), "");
}
