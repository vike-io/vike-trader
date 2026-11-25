use super::*;

#[test]
fn a_position_is_its_per_unit_greeks_times_its_signed_quantity() {
    let (s, k, t, iv, r) = (104_000.0, 100_000.0, 0.07, 0.625, 0.01);
    let (d, g, th, v) = black_scholes_greeks(s, k, t, iv, OptionKind::Call, r).unwrap();
    let second = second_order_greeks(s, k, t, iv, OptionKind::Call, r).unwrap();
    let short = PositionGreeks::of_option(-3.0, s, k, t, iv, OptionKind::Call, r).unwrap();
    for (got, unit) in [
        (short.delta, d),
        (short.gamma, g),
        (short.theta, th),
        (short.vega, v),
        (short.vanna, second.vanna),
        (short.vomma, second.vomma),
        (short.charm, second.charm),
        (short.veta, second.veta),
        (short.color, second.color),
    ] {
        assert_eq!(got.to_bits(), (unit * -3.0).to_bits(), "bit for bit");
    }
    assert!(short.delta < 0.0 && short.gamma < 0.0 && short.vega < 0.0, "a short flips them");
    assert_eq!(PositionGreeks::of_option(1.0, s, k, 0.0, iv, OptionKind::Call, r), None, "expired");
    assert_eq!(PositionGreeks::of_option(1.0, s, k, t, 0.0, OptionKind::Put, r), None, "no vol");
}

#[test]
fn a_net_is_the_left_fold_in_position_order() {
    let call =
        PositionGreeks::of_option(2.0, 100.0, 100.0, 0.25, 0.5, OptionKind::Call, 0.0).unwrap();
    let put =
        PositionGreeks::of_option(-1.0, 100.0, 95.0, 0.25, 0.55, OptionKind::Put, 0.0).unwrap();
    let mut net = PositionGreeks::default();
    net.add(&call);
    net.add(&put);
    assert_eq!(net.delta.to_bits(), (call.delta + put.delta).to_bits());
    assert_eq!(net.vega.to_bits(), (call.vega + put.vega).to_bits());
    assert_eq!(net.color.to_bits(), (call.color + put.color).to_bits());
}

/// A coin-settled book by hand: two long BTC calls at the money (S = K = 50,000, r = 0,
/// σ√T = 0.4 × √0.25 = 0.2, so d1 = 0.1 and the delta is N(0.1) = 0.539827837277029), hedged by a
/// short perpetual of −0.5 coin, and one ETH call. BTC's delta is 2 × 0.5398 − 0.5 = 0.5797 coin,
/// 28,982.78 USD at 50,000; an underlying with no price is left out of the USD read.
#[test]
fn a_book_sums_its_delta_per_underlying_then_in_usd() {
    let btc = PositionGreeks::of_option(2.0, 50_000.0, 50_000.0, 0.25, 0.4, OptionKind::Call, 0.0)
        .unwrap();
    let eth =
        PositionGreeks::of_option(1.0, 3_000.0, 3_200.0, 0.25, 0.7, OptionKind::Call, 0.0).unwrap();
    let mut book = PortfolioGreeks::default();
    book.add_option("BTC", &btc);
    book.add_delta_one("BTC", -0.5);
    book.add_option("ETH", &eth);
    book.add_delta_one("SOL", 10.0);
    assert!((btc.delta - 2.0 * 0.539_827_837_277_029).abs() < 1e-12, "{}", btc.delta);
    assert_eq!(book.delta_units["BTC"].to_bits(), (btc.delta - 0.5).to_bits());
    assert_eq!(book.options.vega.to_bits(), (btc.vega + eth.vega).to_bits());

    let price = |u: &str| match u {
        "BTC" => Some(50_000.0),
        "ETH" => Some(3_000.0),
        _ => None,
    };
    let net = book.net_usd(price, vec!["SOL-PERPETUAL".into()]);
    let btc_usd = &net.per_underlying["BTC"];
    assert!((btc_usd.delta_usd - 28_982.783_727_702_9).abs() < 1e-6, "{btc_usd:?}");
    assert_eq!(btc_usd.spot, 50_000.0);
    assert_eq!(net.per_underlying["ETH"].delta_usd, eth.delta * 3_000.0);
    assert!(!net.per_underlying.contains_key("SOL"), "no price, no USD read");
    assert_eq!(net.net_delta_usd, btc_usd.delta_usd + eth.delta * 3_000.0, "BTreeMap order");
    assert_eq!((net.net_vega, net.net_theta), (book.options.vega, book.options.theta));
    assert_eq!(net.unpriced, vec!["SOL-PERPETUAL".to_string()]);
}
