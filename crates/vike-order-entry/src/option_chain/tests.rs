//! The chain grid's unit tests: the cell-click order, the column layout, the strike window and the
//! paint's colours.

use vike_ui_theme::components::ThemeStatus;
use vike_ui_theme::maps;

use super::grid::{leg, quote_order};
use super::layout::{compute_edges, weight};
use super::toolbar::next_strike_window;
use super::*;

fn quote_with(
    bid: Option<f64>,
    ask: Option<f64>,
    name: Option<&str>,
) -> vike_marketdata::OptionQuote {
    let mut q = vike_marketdata::OptionQuote::new(30000.0, vike_marketdata::OptionKind::Call);
    q.bid = bid;
    q.ask = ask;
    q.instrument_name = name.map(str::to_string);
    q
}

#[test]
fn quote_order_ask_buys_at_ask() {
    let q = quote_with(Some(0.01), Some(0.02), Some("BTC-30000-C"));
    let o = quote_order(&q, true, OptionKind::Call, 30000.0).expect("ask cell is tradeable");
    assert_eq!(o.side, vike_model::OrderSide::Long, "an ASK click is a BUY");
    assert_eq!(o.price, 0.02, "priced at the ask");
    assert_eq!(o.instrument, "BTC-30000-C");
    assert_eq!(o.kind, OptionKind::Call);
    assert_eq!(o.strike, 30000.0);
}

#[test]
fn quote_order_bid_sells_at_bid() {
    let q = quote_with(Some(0.01), Some(0.02), Some("BTC-30000-P"));
    let o = quote_order(&q, false, OptionKind::Put, 30000.0).expect("bid cell is tradeable");
    assert_eq!(o.side, vike_model::OrderSide::Short, "a BID click is a SELL");
    assert_eq!(o.price, 0.01, "priced at the bid");
    assert_eq!(o.kind, OptionKind::Put);
}

#[test]
fn quote_order_none_without_instrument_or_price() {
    // no instrument_name → not tradeable
    assert!(
        quote_order(&quote_with(Some(0.01), Some(0.02), None), true, OptionKind::Call, 1.0)
            .is_none()
    );
    // ask absent → ask cell not tradeable
    assert!(
        quote_order(&quote_with(Some(0.01), None, Some("X")), true, OptionKind::Call, 1.0)
            .is_none()
    );
    // bid absent → bid cell not tradeable
    assert!(
        quote_order(&quote_with(None, Some(0.02), Some("X")), false, OptionKind::Call, 1.0)
            .is_none()
    );
}

#[test]
fn weight_known_fields() {
    for f in ["bidpct", "askpct", "reldist"] {
        assert_eq!(weight(f), 0.95, "{f} should be the narrow-pct weight");
    }
    for f in ["theor", "bid", "ask", "distance"] {
        assert_eq!(weight(f), 1.05, "{f} should be the wide-numeric weight");
    }
    assert_eq!(weight("volume"), 0.8, "volume carries a magnitude bar, gets less text room");
    assert_eq!(weight("spread"), 1.0, "an unmatched field falls back to the default weight");
}

#[test]
fn strike_window_cycles_through_presets() {
    assert_eq!(next_strike_window(6), 12);
    assert_eq!(next_strike_window(12), 20);
    assert_eq!(next_strike_window(20), 30);
    assert_eq!(next_strike_window(30), 6, "wraps back to the first preset");
    assert_eq!(next_strike_window(999), 12, "an off-preset value snaps to 12");
}

#[test]
fn the_grid_shows_exactly_the_strikes_the_ticker_streams() {
    // The grid once showed a balanced 2k+1 rows (ATM ± k), so its top row ATM + n was a strike
    // the 100 ms focus set (n below, n at/above) never subscribed: no live bid/ask on it.
    use vike_marketdata::{OptionChain, OptionChainExpiry, OptionChainSet, OptionChainStrike};
    let quote = |k: f64, kind: OptionKind, tag: &str| vike_marketdata::OptionQuote {
        instrument_name: Some(format!("BTC-{k}-{tag}")),
        ..vike_marketdata::OptionQuote::new(k, kind)
    };
    let expiry = OptionChainExpiry { date: "2026-06-27".into(), dte: 25, label: "27 Jun".into() };
    // strikes 100..=190, step 10
    let rows: Vec<OptionChainStrike> = (0..10)
        .map(|i| {
            let k = 100.0 + f64::from(i) * 10.0;
            OptionChainStrike {
                strike: k,
                call: Some(quote(k, OptionKind::Call, "C")),
                put: Some(quote(k, OptionKind::Put, "P")),
            }
        })
        .collect();
    let ids = |rows: &[OptionChainStrike]| -> Vec<String> {
        rows.iter()
            .flat_map(|r| [&r.call, &r.put])
            .filter_map(|q| q.as_ref()?.instrument_name.clone())
            .collect()
    };
    // between strikes, on a strike, at both ends, below and above every strike
    for spot in [145.0, 150.0, 100.0, 190.0, 50.0, 500.0] {
        let chain = OptionChain {
            underlying: "BTC".into(),
            underlying_kind: vike_marketdata::OptionUnderlyingKind::Crypto,
            underlying_price: Some(spot),
            expiry: expiry.clone(),
            asof_ms: 0,
            source: "deribit".into(),
            rows: rows.clone(),
        };
        let by_underlying = std::collections::BTreeMap::from([(
            "BTC".to_string(),
            OptionChainSet {
                default_expiry: expiry.date.clone(),
                expiries: vec![expiry.clone()],
                chains: std::collections::BTreeMap::from([(expiry.date.clone(), chain)]),
            },
        )]);
        for n in [1, 2, 3, 6, 50] {
            assert_eq!(
                ids(strike_window_rows(&rows, Some(spot), n)),
                vike_options::front_expiry_focus_instruments(&by_underlying, &["BTC"], n),
                "spot {spot}, ±{n}"
            );
        }
    }
    // no spot: nothing to centre on, every row
    assert_eq!(strike_window_rows(&rows, None, 2).len(), rows.len());
}

#[test]
fn compute_edges_cumulative_offsets() {
    let weights = [1.0_f32, 1.0, 2.0];
    let edges = compute_edges(&weights, &[0.0; 3], 400.0);
    assert_eq!(edges, vec![0.0, 100.0, 200.0, 400.0]);
}

/// Room beyond the floors is shared by weight; a width short of the floors is not honoured, so
/// no column ever gets less than its widest text (the row overflows and the body scrolls).
#[test]
fn compute_edges_never_squeezes_a_column_under_its_floor() {
    let weights = [1.0_f32, 1.0, 2.0];
    let floors = [60.0, 20.0, 40.0];
    assert_eq!(compute_edges(&weights, &floors, 200.0), vec![0.0, 80.0, 120.0, 200.0]);
    assert_eq!(compute_edges(&weights, &floors, 120.0), vec![0.0, 60.0, 80.0, 120.0]);
    assert_eq!(
        compute_edges(&weights, &floors, 50.0),
        vec![0.0, 60.0, 80.0, 120.0],
        "a pane narrower than the floors gets a row wider than itself, never narrower columns"
    );
}

#[test]
fn compute_edges_returns_one_more_than_weights() {
    let weights = [1.0_f32; 5];
    let edges = compute_edges(&weights, &[0.0; 5], 500.0);
    assert_eq!(edges.len(), weights.len() + 1);
    assert_eq!(edges[0], 0.0);
    assert_eq!(*edges.last().unwrap(), 500.0);
}

#[test]
fn strike_spine_lands_at_chain_fields_len() {
    // Mirror `draw`'s column layout exactly: 9 reversed call fields, Strike, IV, then 9
    // forward put fields — `strike_col` must land at CHAIN_FIELDS.len() (today: 9), the left
    // edge of the 2-column centre spine (Strike | IV).
    let fields = columns::CHAIN_FIELDS;
    let n = fields.len();
    let mut weights: Vec<f32> = Vec::with_capacity(2 * n + 2);
    for &f in fields.iter().rev() {
        weights.push(weight(f));
    }
    weights.push(1.1); // Strike
    weights.push(0.85); // IV
    for &f in fields.iter() {
        weights.push(weight(f));
    }
    let strike_col = n;
    let edges = compute_edges(&weights, &vec![0.0; weights.len()], 2000.0);

    assert_eq!(strike_col, 9, "CHAIN_FIELDS today has 9 fields/side");
    assert_eq!(edges.len(), weights.len() + 1);
    // The spine spans exactly 2 columns (Strike then IV), both non-degenerate.
    assert!(edges[strike_col] < edges[strike_col + 1], "Strike column must have positive width");
    assert!(edges[strike_col + 1] < edges[strike_col + 2], "IV column must have positive width");
    // Calls (0..strike_col) and puts (strike_col+2..end) use the SAME per-field weight()
    // lookup, just in reversed order, so their total pixel widths must match exactly.
    let calls_w = edges[strike_col] - edges[0];
    let puts_w = edges[weights.len()] - edges[strike_col + 2];
    assert!(
        (calls_w - puts_w).abs() < 1e-3,
        "mirrored calls/puts sides must be equal width: {calls_w} vs {puts_w}"
    );
}

/// The call and put rows are TODAY's colours on every theme and market set: the calls' series blue
/// (`(87, 165, 255)`, the info status's value) as a graphic and as text, and the market's down
/// for the puts, as a graphic and as text. Changing the table changes this named test.
#[test]
fn the_callput_rows_are_todays_colours() {
    use vike_ui_theme::appearance::ThemeAppearance;
    use vike_ui_theme::market::ThemeMarketColorsId;
    let series_blue = egui::Color32::from_rgb(87, 165, 255);
    use vike_ui_theme::theme::ThemePaletteId;
    for theme in ThemePaletteId::ALL {
        for market in ThemeMarketColorsId::ALL {
            let t = ThemeTokens::from_appearance(&ThemeAppearance {
                theme,
                market,
                ..ThemeAppearance::default()
            });
            let at = format!("{theme:?} {market:?}");
            assert_eq!(maps::callput::CALL.colour.resolve(&t), series_blue, "{at}: call");
            assert_eq!(maps::callput::CALL.text.resolve(&t), series_blue, "{at}: CALLS");
            assert_eq!(maps::callput::CALL.colour.resolve(&t), ThemeStatus::Info.color(), "{at}");
            assert_eq!(maps::callput::PUT.colour.resolve(&t), t.market.down, "{at}: put");
            assert_eq!(maps::callput::PUT.text.resolve(&t), t.market.down_text, "{at}: PUTS");
        }
    }
}

/// A side of the chain is drawn by the row of its name, and every row of the map is one side's.
#[test]
fn a_side_of_the_chain_is_drawn_by_its_own_row_and_every_row_has_a_side() {
    assert_eq!(leg(OptionKind::Call).key, "CALL");
    assert_eq!(leg(OptionKind::Put).key, "PUT");
    assert_ne!(leg(OptionKind::Call), leg(OptionKind::Put));
    let sides = [OptionKind::Call, OptionKind::Put];
    assert!(maps::callput::ALL.iter().all(|r| sides.iter().any(|k| leg(*k) == *r)));
}
