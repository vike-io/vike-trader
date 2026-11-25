use super::*;
use crate::test_support::venue_block;
use vike_execution::{CoreSnapshot, VenueBlock};
use vike_model::AccountTier;

fn row(venue: &str, realized: f64, fees: f64, funding: f64) -> equity::AccountWindowAccountsRow {
    equity::AccountWindowAccountsRow {
        venue: venue.to_string(),
        account: "main".to_string(),
        route_key: venue.to_string(),
        tier: None,
        equity: 0.0,
        realized,
        unrealized: 0.0,
        fees,
        funding,
        mode: "\u{0394}".to_string(),
        missing_prices: 0,
        currency: Default::default(),
    }
}

#[test]
fn hero_totals_sums_every_row() {
    let rows = [row("binance", 1.5, 0.25, -0.5), row("bybit", -0.5, 0.75, 0.25)];
    let t = hero_totals(&rows, Default::default());
    assert_eq!(t.realized, 1.0);
    assert_eq!(t.fees, 1.0);
    assert_eq!(t.funding, -0.25);
}

#[test]
fn hero_totals_of_no_venues_is_all_zero() {
    assert_eq!(hero_totals(&[], Default::default()), AccountWindowEquityHeroTotals::default());
}

/// ⚠ A row in another currency is never added to the hero's strip (decision 0183, item 13): the
/// strip is the `primary` group's, here dollars, WHERE THE ROW LIES.
#[test]
fn hero_totals_adds_one_currency_only() {
    let mut eur = row("oanda", -300.0, 4.5, 0.25);
    eur.currency = "EUR".into();
    let rows = [row("binance", 1.5, 0.25, -0.5), eur.clone(), row("bybit", -0.5, 0.75, 0.25)];
    let t = hero_totals(&rows, Default::default());
    assert_eq!((t.realized, t.fees, t.funding), (1.0, 1.0, -0.25), "the two dollar rows");
    // ...and a EUR primary makes the strip EUR.
    let t = hero_totals(&[eur, row("binance", 1.5, 0.25, -0.5)], "EUR".into());
    assert_eq!((t.realized, t.fees, t.funding), (-300.0, 4.5, 0.25));
}

/// ⚠ **The strip's group is the node's primary, not the first row's** (the primary restarted, so the
/// frame lists `[oanda EUR, bybit, binance']`): the dollar rows are the strip, the EUR row is not.
#[test]
fn hero_totals_follow_the_stated_primary_not_the_first_row() {
    let mut eur = row("oanda", -300.0, 4.5, 0.25);
    eur.currency = "EUR".into();
    let rows = [eur, row("bybit", -0.5, 0.75, 0.25), row("binance", 1.5, 0.25, -0.5)];
    let t = hero_totals(&rows, Default::default());
    assert_eq!((t.realized, t.fees, t.funding), (1.0, 1.0, -0.25), "the dollar group");
}

/// The P&L chip's colour, read off a real frame of the equity hero under appearance `a`.
fn pnl_colour(a: vike_ui_theme::appearance::ThemeAppearance, realized: f64) -> egui::Color32 {
    let context = egui::Context::default();
    vike_ui_theme::appearance::install(&context, &a);
    let model = equity::AccountWindowEquityModel {
        equity_total: 1000.0,
        missing_prices_total: 0,
        rows: vec![equity::AccountWindowAccountsRow {
            venue: "bybit".to_string(),
            account: "main".to_string(),
            route_key: "bybit".to_string(),
            tier: Some(AccountTier::Demo),
            equity: 1000.0,
            realized,
            unrealized: 0.0,
            fees: 0.0,
            funding: 0.0,
            mode: "auth".to_string(),
            missing_prices: 0,
            currency: Default::default(),
        }],
        currency: "USD",
        primary: Default::default(),
        other_ledgers: Vec::new(),
        rates: Vec::new(),
        tiers: Vec::new(),
    };
    let out = context.run_ui(egui::RawInput::default(), |ui| equity_hero(ui, &model, 0.0));
    let colour = out.shapes.iter().find_map(|c| match &c.shape {
        egui::Shape::Text(t) if t.galley.text().starts_with("P&L") => {
            Some(t.galley.job.sections[0].format.color)
        }
        _ => None,
    });
    out.drop_without_applying_deltas();
    colour.expect("the hero paints a P&L chip")
}

/// Every text one frame of the header paints over `snapshot`, with its height on the page.
fn header_texts(snapshot: &CoreSnapshot) -> Vec<(String, f32)> {
    let context = egui::Context::default();
    vike_ui_theme::appearance::install(
        &context,
        &vike_ui_theme::appearance::ThemeAppearance::default(),
    );
    let model = equity::AccountWindowEquityModel::from_snapshot(snapshot);
    let out = context.run_ui(egui::RawInput::default(), |ui| equity_header(ui, &model, 0.0));
    let texts = out
        .shapes
        .iter()
        .filter_map(|c| match &c.shape {
            egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.pos.y)),
            _ => None,
        })
        .collect();
    out.drop_without_applying_deltas();
    texts
}

/// One account of `tier` in `currency` (empty = dollars) holding `equity`.
fn tiered(key: &str, tier: Option<AccountTier>, currency: &str, equity: f64) -> VenueBlock {
    VenueBlock {
        route_key: key.to_string(),
        tier,
        ledger_currency: currency.into(),
        equity,
        ..venue_block("bybit", Vec::new())
    }
}

/// 2 live, 3 demo and 4 paper accounts over dollars and euros.
fn nine_accounts() -> CoreSnapshot {
    let (l, d, p) = (Some(AccountTier::Live), Some(AccountTier::Demo), Some(AccountTier::Paper));
    let mut snapshot = CoreSnapshot::empty("bybit", "BTCUSDT");
    snapshot.portfolio.venues = vec![
        tiered("bybit#LIVE", l, "", 1_000.0),
        tiered("bybit#D1", d, "", 5_000.0),
        tiered("bybit#P1", p, "", 100_000.0),
        tiered("bybit#L2", l, "EUR", 200.0),
        tiered("bybit#D2", d, "", 3_000.0),
        tiered("bybit#P2", p, "", 100_000.0),
        tiered("bybit#D3", d, "EUR", 7_000.0),
        tiered("bybit#P3", p, "", 100_000.0),
        tiered("bybit#P4", p, "EUR", 100_000.0),
    ];
    snapshot
}

/// ⚠ **The header draws LIVE, DEMO and PAPER apart, in that order, each with its count and its own line
/// per currency, and no figure that adds one tier to another** (the owner, 2026-11-17).
#[test]
fn the_header_draws_each_tier_apart_and_no_total_across_them() {
    let texts = header_texts(&nine_accounts());
    let at =
        |w: &str| texts.iter().find(|(s, _)| s == w).unwrap_or_else(|| panic!("{w}: {texts:?}")).1;
    let (live, demo, paper) = (at("LIVE"), at("DEMO"), at("PAPER"));
    assert!(live < demo && demo < paper, "{texts:?}");
    for said in ["2 accounts", "3 accounts", "4 accounts"] {
        at(said);
    }
    let lines = ["USD 1000.00", "EUR 200.00", "USD 8000.00", "EUR 7000.00", "USD 300000.00"];
    for (said, below) in lines.into_iter().zip([live, live, demo, demo, paper]) {
        assert!(at(said) > below, "{said} sits in its group: {texts:?}");
    }
    assert!(at("EUR 100000.00") > paper);
    for mixed in ["309000.00", "USD 309000.00", "EUR 107200.00", "CROSS-VENUE EQUITY", OLDER_SERVER]
    {
        assert!(!texts.iter().any(|(s, _)| s.contains(mixed)), "{mixed}: {texts:?}");
    }
}

/// A tier with no account draws no group.
#[test]
fn a_tier_with_no_account_is_not_drawn() {
    let mut snapshot = nine_accounts();
    snapshot.portfolio.venues.retain(|v| v.tier != Some(AccountTier::Demo));
    let texts = header_texts(&snapshot);
    assert!(texts.iter().any(|(s, _)| s == "LIVE") && texts.iter().any(|(s, _)| s == "PAPER"));
    assert!(!texts.iter().any(|(s, _)| s == "DEMO" || s == "3 accounts"), "{texts:?}");
}

/// A node older than the tier report gets the one total it always had, and a note that says why it mixes
/// tiers; no group is drawn for it. No account at all is no note.
#[test]
fn an_older_server_gets_the_one_total_and_a_note() {
    let mut snapshot = nine_accounts();
    snapshot.portfolio.venues.iter_mut().for_each(|v| v.tier = None);
    let texts = header_texts(&snapshot);
    for said in ["CROSS-VENUE EQUITY (USD)", OLDER_SERVER] {
        assert!(texts.iter().any(|(s, _)| s == said), "{said}: {texts:?}");
    }
    assert!(!texts.iter().any(|(s, _)| ["LIVE", "DEMO", "PAPER"].contains(&s.as_str())));
    let empty = header_texts(&CoreSnapshot::empty("bybit", "BTCUSDT"));
    assert!(!empty.iter().any(|(s, _)| s == OLDER_SERVER), "{empty:?}");
}

#[test]
fn the_pnl_chip_is_the_chosen_market_sets_text_colour() {
    use vike_ui_theme::appearance::ThemeAppearance;
    use vike_ui_theme::market::{ThemeMarketColors, ThemeMarketColorsId};
    for m in ThemeMarketColorsId::ALL {
        let c = ThemeMarketColors::of(m);
        let a = ThemeAppearance { market: m, ..ThemeAppearance::default() };
        assert_eq!(pnl_colour(a, 12.5), c.up_text, "{m:?} profit");
        assert_eq!(pnl_colour(a, -12.5), c.down_text, "{m:?} loss");
    }
}

/// Review Focus 5: the default appearance paints exactly what the old Trade window (this
/// window, before it lost its ticket) painted before.
#[test]
fn the_default_trade_colours_are_todays() {
    use vike_ui_theme::appearance::ThemeAppearance;
    use vike_ui_theme::market::{ThemeMarketColors, ThemeMarketColorsId};
    let classic = ThemeMarketColors::of(ThemeMarketColorsId::Classic);
    assert_eq!(pnl_colour(ThemeAppearance::default(), 12.5), classic.up_text);
    assert_eq!(pnl_colour(ThemeAppearance::default(), -12.5), classic.down_text);
}
