//! The ladder's unit tests: the row keys and the grouping, the sizes, the columns and the markers;
//! the screen tests over a drawn ladder are `screen`'s, the no-book classifier's `absence`'s.

use super::absence::absence_icon;
use super::columns::{
    COLS, TradeWindowLadderColumn, TradeWindowLadderColumns, click_type, side_of,
};
use super::markers::{
    TradeWindowLadderMarkerFace, TradeWindowLadderMarkerKind, TradeWindowLadderMarkerStyle,
    marker_face, marker_room, marker_words, pill_style,
};
use super::rows::{
    UNKNOWN_LOT_DECIMALS, drag_to_reprice_allowed, fmt_size, group_book, key_price, lot_decimals,
    next_group, previous_group, row_key, row_tick, size_galley,
};
use crate::trade_window::{TradeWindowLadderOrder, TradeWindowTicketKind};
use crate::trade_window::{TradeWindowPanes, grid_value, layout};
use egui::Color32;
use egui::Pos2;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use vike_marketdata::L2Book;
use vike_model::feed_status::DataClientConnectionState;
use vike_ui_theme::appearance::ThemeAppearance;
use vike_ui_theme::color::with_alpha;
use vike_ui_theme::components::ThemeStatus;
use vike_ui_theme::icons;
use vike_ui_theme::maps;
use vike_ui_theme::market::ThemeMarketColorsId;
use vike_ui_theme::metrics::ThemeDensity;
use vike_ui_theme::roles::ThemeColourRole;
use vike_ui_theme::theme::ThemePaletteId;
use vike_ui_theme::type_scale::ThemeTextSize;
use vike_ui_theme::value::trade;
use vike_venues::VenueCapabilities;

use super::*;

/// The shapes the columns take: beside the ticket with the Vol column off and on, and
/// under it.
fn shapes() -> [(TradeWindowTicketPlace, bool, Vec<TradeWindowLadderColumn>); 3] {
    let five = vec![
        TradeWindowLadderColumn::Buy,
        TradeWindowLadderColumn::Bid,
        TradeWindowLadderColumn::Price,
        TradeWindowLadderColumn::Ask,
        TradeWindowLadderColumn::Sell,
    ];
    let six = [five.clone(), vec![TradeWindowLadderColumn::Volume]].concat();
    [
        (TradeWindowTicketPlace::Beside, false, five.clone()),
        (TradeWindowTicketPlace::Beside, true, six),
        (TradeWindowTicketPlace::Under, true, five),
    ]
}

#[test]
fn the_columns_split_the_width_in_order_and_volume_is_only_there_when_asked_for() {
    for (panel, volume, want) in shapes() {
        let cols = TradeWindowLadderColumns::new(panel, volume, 340.0, 0.0);
        let mut order: Vec<TradeWindowLadderColumn> =
            (0..100).map(|x| cols.at(x as f32 * 3.4, 0.0)).collect();
        order.dedup();
        assert_eq!(order, want, "{panel:?} / volume {volume}");
        // A pointer past either edge (a drag that left the ladder) is the edge column, not none.
        assert_eq!(cols.at(-50.0, 0.0), TradeWindowLadderColumn::Buy);
        assert_eq!(cols.at(9_999.0, 0.0), *want.last().unwrap());
        assert_eq!(
            cols.shown(TradeWindowLadderColumn::Volume),
            want.contains(&TradeWindowLadderColumn::Volume),
            "{panel:?} / volume {volume}"
        );
    }
}

/// The rect a column is drawn in is the span [`TradeWindowLadderColumns::at`] hit-tests it over: what a trader sees
/// in a cell is what a click there trades. And the columns fill the row, whatever its width.
#[test]
fn each_column_is_drawn_where_it_is_hit_tested() {
    for width in [200.0, 323.0, 340.0, 600.0] {
        let row = Rect::from_min_size(pos2(10.0, 0.0), vec2(width, 18.0));
        for (panel, volume, shown) in shapes() {
            let cols = TradeWindowLadderColumns::new(panel, volume, width, 0.0);
            for col in shown {
                let r = cols.rect(col, row);
                for x in [r.min.x + 0.5, r.center().x, r.max.x - 0.5] {
                    assert_eq!(cols.at(x, row.min.x), col, "{col:?} at {x} of {width}");
                }
            }
            let last = if volume && panel == TradeWindowTicketPlace::Beside {
                TradeWindowLadderColumn::Volume
            } else {
                TradeWindowLadderColumn::Sell
            };
            assert!((cols.rect(last, row).max.x - row.max.x).abs() < 1e-3, "they fill it");
        }
    }
}

/// The design's own widths at the design's own width: a 323 pt ladder, and its 322 pt of
/// columns scaled by a hundred and fiftieth of a point. ⚠ A reader who changes a width here
/// is changing the owner's design, not a test.
#[test]
fn the_columns_are_the_designs_at_its_own_width() {
    let widths = |cols: &TradeWindowLadderColumns| -> Vec<f32> {
        COLS.iter().map(|c| cols.edge[*c as usize + 1] - cols.edge[*c as usize]).collect()
    };
    let near = |got: Vec<f32>, want: [f32; 6]| {
        for (g, w) in got.iter().zip(want) {
            assert!((g - w).abs() < 0.2, "{got:?} against {want:?}");
        }
    };
    near(
        widths(&TradeWindowLadderColumns::new(TradeWindowTicketPlace::Beside, true, 322.0, 0.0)),
        [44.0, 64.0, 76.0, 64.0, 44.0, 30.0],
    );
    near(
        widths(&TradeWindowLadderColumns::new(TradeWindowTicketPlace::Beside, false, 322.0, 0.0)),
        [44.0, 64.0, 106.0, 64.0, 44.0, 0.0],
    );
    near(
        widths(&TradeWindowLadderColumns::new(TradeWindowTicketPlace::Under, true, 300.0, 0.0)),
        [48.0, 64.0, 76.0, 64.0, 48.0, 0.0],
    );
}

/// The Price column is never narrower than the widest price in view needs, and what it takes
/// comes from the other columns: the row stays the width it was given.
#[test]
fn a_wide_price_widens_its_column_and_the_row_stays_as_wide() {
    let plain = TradeWindowLadderColumns::new(TradeWindowTicketPlace::Beside, true, 323.0, 0.0);
    let wide = TradeWindowLadderColumns::new(TradeWindowTicketPlace::Beside, true, 323.0, 90.0);
    let price = |c: &TradeWindowLadderColumns| {
        c.edge[TradeWindowLadderColumn::Price as usize + 1]
            - c.edge[TradeWindowLadderColumn::Price as usize]
    };
    assert!(price(&plain) < 90.0 && (price(&wide) - 90.0).abs() < 1e-3);
    assert!((wide.width() - 323.0).abs() < 1e-3);
    assert!(
        wide.edge[TradeWindowLadderColumn::Price as usize]
            < plain.edge[TradeWindowLadderColumn::Price as usize],
        "Buy and Bid gave"
    );
}

#[test]
fn buy_and_bid_buy_ask_and_sell_sell_price_and_volume_are_inert() {
    assert_eq!(side_of(TradeWindowLadderColumn::Buy), Some(1));
    assert_eq!(side_of(TradeWindowLadderColumn::Bid), Some(1));
    assert_eq!(side_of(TradeWindowLadderColumn::Price), None);
    assert_eq!(side_of(TradeWindowLadderColumn::Ask), Some(-1));
    assert_eq!(side_of(TradeWindowLadderColumn::Sell), Some(-1));
    assert_eq!(side_of(TradeWindowLadderColumn::Volume), None);
}

/// A buy above the best ask, or a sell below the best bid, is a stop; at or inside the market,
/// a limit (spec §3.4). No modifier key forces a stop any more.
#[test]
fn a_click_resolves_limit_or_stop_by_the_market() {
    assert_eq!(click_type(1, 101.0, 99.0, 100.0), TradeWindowTicketKind::Stop);
    assert_eq!(click_type(1, 100.0, 99.0, 100.0), TradeWindowTicketKind::Limit);
    assert_eq!(click_type(-1, 98.0, 99.0, 100.0), TradeWindowTicketKind::Stop);
    assert_eq!(click_type(-1, 99.0, 99.0, 100.0), TradeWindowTicketKind::Limit);
}

/// Prices print exactly on the tick at both ends of the range (Review Focus 3): a seven-digit
/// price grouped, a sub-satoshi tick to its last digit.
#[test]
fn prices_print_on_the_tick_with_thousands_grouped() {
    assert_eq!(fmt_price(65_432.5, Some(0.1)), "65,432.5");
    assert_eq!(fmt_price(0.5231, Some(0.0001)), "0.5231");
    assert_eq!(fmt_price(0.000_012_34, Some(1e-8)), "0.00001234");
    assert_eq!(fmt_price(123.0, Some(1.0)), "123");
    assert_eq!(fmt_price(1_234_567.0, Some(0.5)), "1,234,567.0");
    assert_eq!(fmt_price(0.000_000_000_12, Some(1e-11)), "0.00000000012");
    for price in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(fmt_price(price, Some(0.1)), NO_PRICE, "{price}");
    }
}

#[test]
fn row_keys_group_ticks_and_invert() {
    assert_eq!(row_key(100.3, 0.1, 5), row_key(100.1, 0.1, 5));
    let k = row_key(100.3, 0.1, 5);
    assert!((key_price(k, 0.1, 5) - 100.0).abs() < 1e-9);
    // A price between two ticks keys on the NEAREST one.
    assert_eq!(row_key(100.04, 0.1, 1), row_key(100.0, 0.1, 1));
    assert_eq!(row_key(100.06, 0.1, 1), row_key(100.1, 0.1, 1));
}

/// A row names the grid's own price, the one the book reads its level at
/// (`vike_marketdata::TickFraction`): 35 ticks of 0.01 is 0.35, never the
/// 0.35000000000000003 of `35 × 0.01` a ladder click used to send.
#[test]
fn a_row_names_the_books_own_grid_price() {
    assert_eq!(key_price(35, 0.01, 1), 0.35);
    assert_eq!(key_price(7, 0.01, 5), 0.35);
    assert_eq!(key_price(1_001, 0.1, 1), 100.1);
    let mut book = L2Book::new(0.01);
    book.apply_snapshot(0, &[L2BookLevel::new(0.35, 1.0)], &[]);
    let level = book.best_bid().expect("one bid").price;
    assert_eq!(key_price(row_key(level, 0.01, 1), 0.01, 1), level);
}

#[test]
fn grouping_steps_up_and_down_the_roster() {
    assert_eq!(next_group(1), 2);
    assert_eq!(next_group(5), 10);
    assert_eq!(next_group(25), 50);
    assert_eq!(next_group(50), 50);
    assert_eq!(previous_group(10), 5);
    assert_eq!(previous_group(5), 2);
    assert_eq!(previous_group(1), 1);
}

/// The rows key on the catalog's tick, else on the book's own; a grid that knows no tick never
/// divides a price by zero (pre-flight: a zero tick saturates every key and overflows the row
/// arithmetic).
#[test]
fn the_rows_key_on_the_catalogs_tick_else_the_books() {
    assert_eq!(row_tick(Some(0.1), 0.5), 0.1);
    assert_eq!(row_tick(None, 0.5), 0.5);
    assert_eq!(row_tick(grid_value(f64::NAN), 0.5), 0.5);
    assert_eq!(row_tick(None, 0.0), 1.0);
}

// ---- MOVED from the DOM's tests with the items they pin (pre-flight Minor 8) ----

#[test]
fn group_book_sums_quantity_into_rows() {
    let tick = 1.0;
    let levels: Vec<L2BookLevel> = vec![
        L2BookLevel::new(100.0, 1.0),
        L2BookLevel::new(101.0, 2.0),
        L2BookLevel::new(105.0, 4.0),
        L2BookLevel::new(109.0, 8.0),
    ];
    // group 10 → all four collapse into one row summing to 15
    let m = group_book(&levels, tick, 10);
    assert_eq!(m.len(), 1);
    let key = row_key(100.0, tick, 10);
    assert_eq!(m.get(&key).copied(), Some(15.0));
    // group 5 → {100,101} and {105,109} split into two rows
    let m5 = group_book(&levels, tick, 5);
    assert_eq!(m5.len(), 2);
    assert_eq!(m5.get(&row_key(100.0, tick, 5)).copied(), Some(3.0));
    assert_eq!(m5.get(&row_key(105.0, tick, 5)).copied(), Some(12.0));
}

/// Pins the rendered strings. A size prints to the LOT's decimals, whatever its size — `1.262`
/// on a lot of 0.001, not the `1.26` and `0.7050` the size tiers printed — and plain digits,
/// never a K/M compaction (`vike_ui_theme::fmt::format_compact` would print `1.23K`). A lot nobody
/// knows prints the design's three.
#[test]
fn sizes_print_to_the_lots_decimals() {
    assert_eq!(fmt_size(1.262, Some(0.001)), "1.262");
    assert_eq!(fmt_size(0.705, Some(0.001)), "0.705");
    assert_eq!(fmt_size(2.5, Some(0.001)), "2.500");
    assert_eq!(fmt_size(0.00001, Some(0.00001)), "0.00001");
    assert_eq!(fmt_size(1234.567, Some(0.001)), "1234.567");
    assert_eq!(fmt_size(1234.0, Some(1.0)), "1234");
    assert_eq!(fmt_size(0.7, Some(0.1)), "0.7");
    for raw in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        let unknown = grid_value(raw);
        assert_eq!(lot_decimals(unknown), UNKNOWN_LOT_DECIMALS, "{raw}");
        assert_eq!(fmt_size(1.262, unknown), "1.262", "{raw}");
        // A tiny size of a spot book prints a digit, never `0.000`.
        assert_eq!(fmt_size(0.0002, unknown), "0.00020", "{raw}");
        assert_eq!(fmt_size(0.0, unknown), "0.000", "{raw}");
        assert_eq!(fmt_size(0.05, unknown), "0.050", "{raw}");
    }
    assert_eq!(
        fmt_size(0.0002, Some(0.001)),
        "0.000",
        "a known lot is the lot's: it cannot hold less"
    );
}

/// A size too wide for its column loses its LAST digits, never its first: `1234.567` in a
/// room that holds seven characters is `1234.57`, and in one that holds four, `1235`. The
/// first frame's fonts are egui's, so the measure waits for the app's type.
#[test]
fn a_size_that_does_not_fit_is_printed_to_fewer_decimals_never_cut() {
    let mut h = Harness::builder().build_ui_state(
        |ui, got: &mut Vec<String>| {
            if !vike_ui_theme::harness::type_ready(ui.ctx()) {
                return;
            }
            let t = ThemeTokens::of(ui.ctx());
            let font = t.mono(ThemeTextRole::Body);
            let one = ui.painter().layout_no_wrap("0".to_string(), font.clone(), t.theme.text);
            let digit = one.size().x;
            got.clear();
            for chars in [8.0, 7.0, 6.0, 4.0] {
                let room = chars * digit;
                let g = size_galley(ui.painter(), &font, t.theme.text, 1234.567, 3, room);
                got.push(g.text().to_string());
            }
        },
        Vec::new(),
    );
    h.run();
    assert_eq!(h.state(), &["1234.567", "1234.57", "1234.6", "1235"]);
}

/// The drag-to-reprice gate (audit br6): the ladder offers the drag ONLY when the venue's
/// declared caps wire a native modify. A modify-less venue (or the unknown/default caps) blocks
/// it — the exact condition that stops an unsupported `Command::Modify`.
#[test]
fn drag_to_reprice_gate_follows_capabilities() {
    assert!(drag_to_reprice_allowed(&vike_venues::venue_caps::BINANCE));
    assert!(drag_to_reprice_allowed(&vike_venues::venue_caps::BYBIT));
    assert!(drag_to_reprice_allowed(&vike_venues::venue_caps::OKX));
    assert!(drag_to_reprice_allowed(&vike_venues::venue_caps::OANDA));
    // Dukascopy (setOpenPrice/setRequestedAmount) and IBKR (re-place under the same id) amend a resting
    // order now, and FXCM edits a resting entry's price (EditOrder), so the drag is offered on all three.
    assert!(drag_to_reprice_allowed(&vike_venues::venue_caps::FXCM));
    assert!(drag_to_reprice_allowed(&vike_venues::venue_caps::DUKASCOPY));
    assert!(drag_to_reprice_allowed(&vike_venues::venue_caps::IBKR));
    assert!(!drag_to_reprice_allowed(&vike_venues::venue_caps::POLYMARKET));
    assert!(!drag_to_reprice_allowed(&VenueCapabilities::UNSUPPORTED));
}

/// The design's pill (`span.pill`): a LIMIT is its side's colour at a fifth of its strength,
/// edged in the same at half of it; a STOP is hollow and its edge DASHED, in the side's whole
/// colour (owner decision 2); the words are the side's TEXT colour either way. A venue that
/// cannot reprice greys the edge of both kinds (audit br6).
#[test]
fn a_limit_pill_is_tinted_and_a_stop_pill_is_hollow_and_dashed() {
    let t = ThemeTokens::from_appearance(&ThemeAppearance::default());
    let limit = pill_style(1, false, true, &t);
    assert_eq!(limit.fill, with_alpha(t.market.up, trade::PILL_FILL_ALPHA));
    assert_eq!(limit.edge, with_alpha(t.market.up, trade::PILL_EDGE_ALPHA));
    assert_eq!((limit.ink, limit.dashed), (t.market.up_text, false));
    let stop = pill_style(-1, true, true, &t);
    assert_eq!(stop.fill, Color32::TRANSPARENT, "hollow");
    assert_eq!((stop.edge, stop.ink, stop.dashed), (t.market.down, t.market.down_text, true));
    for is_stop in [false, true] {
        let s = pill_style(1, is_stop, false, &t);
        assert_eq!(s.edge, t.theme.text_tertiary, "a venue that cannot reprice: {is_stop}");
        assert_eq!(s.dashed, is_stop, "{is_stop}");
    }
}

/// Every pill's fill, edge, words and dash are what the ladder painted before they moved to
/// `ui-theme.toml`'s `order_pill` map, on every theme and market set, for a buy and a sell (a
/// side that is not positive is a sell), a limit and a stop, on a venue that can reprice and on one
/// that cannot. Changing a row of the table changes a pill, and this test is what says so by name.
#[test]
fn a_pill_wears_the_fill_edge_words_and_dash_the_order_pill_map_gives_it() {
    for id in ThemePaletteId::ALL {
        for m in ThemeMarketColorsId::ALL {
            let t = ThemeTokens::from_appearance(&ThemeAppearance {
                theme: id,
                market: m,
                ..ThemeAppearance::default()
            });
            for side in [1, 0, -1] {
                let (graphic, ink) = if side > 0 {
                    (t.market.up, t.market.up_text)
                } else {
                    (t.market.down, t.market.down_text)
                };
                for is_stop in [false, true] {
                    for draggable in [true, false] {
                        let fill = if is_stop {
                            Color32::TRANSPARENT
                        } else {
                            with_alpha(graphic, trade::PILL_FILL_ALPHA)
                        };
                        let edge = match (draggable, is_stop) {
                            (false, _) => t.theme.text_tertiary,
                            (true, true) => graphic,
                            (true, false) => with_alpha(graphic, trade::PILL_EDGE_ALPHA),
                        };
                        assert_eq!(
                            pill_style(side, is_stop, draggable, &t),
                            TradeWindowLadderMarkerStyle { fill, edge, ink, dashed: is_stop },
                            "{id:?} {m:?} side {side} stop {is_stop} draggable {draggable}"
                        );
                    }
                }
            }
        }
    }
}

/// The map is the ladder's own: every pill reads the row of its own name (`BuyLimit` is
/// `BUY_LIMIT`), no two share one, and the one row of the `order_pill` map that is no pill's is
/// the edge of a pill that cannot be dragged.
#[test]
fn every_pill_has_its_own_row_and_the_one_row_no_pill_reads_is_the_undraggable_edge() {
    let all = [
        TradeWindowLadderMarkerKind::BuyLimit,
        TradeWindowLadderMarkerKind::BuyStop,
        TradeWindowLadderMarkerKind::SellLimit,
        TradeWindowLadderMarkerKind::SellStop,
    ];
    for pill in all {
        let mut key = String::new();
        for (i, c) in format!("{pill:?}").chars().enumerate() {
            if i > 0 && c.is_ascii_uppercase() {
                key.push('_');
            }
            key.push(c.to_ascii_uppercase());
        }
        assert_eq!(pill.row().key, key, "{pill:?} reads another row");
        assert_eq!(all.iter().filter(|o| o.row() == pill.row()).count(), 1, "{pill:?}'s row");
    }
    let mut unread = Vec::new();
    for row in maps::order_pill::ALL {
        if !all.iter().any(|p| p.row() == *row) {
            unread.push(row.key);
        }
    }
    assert_eq!(unread, ["NOT_DRAGGABLE"], "a row of the order_pill map is no pill's");
}

/// An order is the pill of its side and type, and the code reads two things off a row that must
/// say the same: a stop is HOLLOW (its `fill` is `none`, so its edge stays whole) exactly when
/// it is DASHED (its `flag`). A row edited to disagree would draw a dashed tinted edge or a
/// hollow solid one, a pill the design does not have.
#[test]
fn an_order_is_the_pill_of_its_side_and_type_and_a_stop_is_hollow_exactly_when_dashed() {
    for (side, is_stop, pill) in [
        (1, false, TradeWindowLadderMarkerKind::BuyLimit),
        (1, true, TradeWindowLadderMarkerKind::BuyStop),
        (-1, false, TradeWindowLadderMarkerKind::SellLimit),
        (-1, true, TradeWindowLadderMarkerKind::SellStop),
        (0, false, TradeWindowLadderMarkerKind::SellLimit),
    ] {
        assert_eq!(
            TradeWindowLadderMarkerKind::of(side, is_stop),
            pill,
            "side {side} stop {is_stop}"
        );
        let row = pill.row();
        assert_eq!(row.fill == ThemeColourRole::None, is_stop, "{pill:?}: hollow only a stop");
        assert_eq!(row.flag, Some(is_stop), "{pill:?}: dashed only a stop");
        assert_ne!(row.stroke, ThemeColourRole::None, "{pill:?}: a pill with no edge");
        assert_ne!(row.text, ThemeColourRole::None, "{pill:?}: a pill with no words");
    }
}

/// The three bookless renderings (owner decision 5, spec §4.2): nothing while connecting — a
/// bookless ladder does not move, so no spinner — the tray when the link is up or not known,
/// the unreachable cloud in the warning colour when it is down.
#[test]
fn a_bookless_ladder_shows_no_icon_while_connecting_the_tray_when_up_and_the_cloud_when_down() {
    let t = ThemeTokens::from_appearance(&ThemeAppearance::default());
    assert_eq!(absence_icon(DataClientConnectionState::Connecting, &t), None);
    for up in [DataClientConnectionState::Connected, DataClientConnectionState::Unknown] {
        assert_eq!(
            absence_icon(up, &t).map(|(i, c, _)| (i, c)),
            Some((icons::EMPTY, t.theme.text_tertiary))
        );
    }
    for down in [DataClientConnectionState::Disconnected, DataClientConnectionState::Error] {
        assert_eq!(
            absence_icon(down, &t).map(|(i, c, _)| (i, c)),
            Some((icons::UNREACHABLE, ThemeStatus::Warning.color()))
        );
    }
}

/// A bookless ladder is drawn from the `empty_pane` map's rows, on every theme: the tray (its caption grey)
/// and the words "empty" for a link that is up or not known, the cloud (the warning amber) and "unreachable"
/// for one that is down, nothing while it connects. Pinned to the icons, colours and words the ladder
/// chose before they moved to `ui-theme.toml`.
#[test]
fn a_bookless_ladder_is_drawn_from_the_empty_pane_rows_on_every_theme() {
    for id in ThemePaletteId::ALL {
        let t = ThemeTokens::from_appearance(&ThemeAppearance {
            theme: id,
            ..ThemeAppearance::default()
        });
        let tray = Some((icons::EMPTY, t.theme.text_tertiary, "empty"));
        let cloud = Some((icons::UNREACHABLE, ThemeStatus::Warning.color(), "unreachable"));
        for (link, want) in [
            (DataClientConnectionState::Connected, tray),
            (DataClientConnectionState::Connecting, None),
            (DataClientConnectionState::Disconnected, cloud),
            (DataClientConnectionState::Error, cloud),
            (DataClientConnectionState::Unknown, tray),
        ] {
            assert_eq!(absence_icon(link, &t), want, "{id:?} {link:?}");
        }
    }
}

// ---- the markers ----

fn order(
    order_client_id: &str,
    side: i32,
    price: f64,
    quantity: f64,
    is_stop: bool,
) -> TradeWindowLadderOrder {
    TradeWindowLadderOrder {
        order_client_id: order_client_id.to_string(),
        side,
        price,
        quantity,
        is_stop,
        words: None,
    }
}

fn face(full: &str, count: &str, stop: bool) -> TradeWindowLadderMarkerFace {
    TradeWindowLadderMarkerFace { full: full.to_string(), count: count.to_string(), stop }
}

/// One marker stands for every own order on its row and side: the count FIRST, then the summed
/// size to the lot's decimals. It is hollow only when every order it stands for is a stop.
#[test]
fn one_marker_says_how_many_orders_it_stands_for_then_their_size() {
    let (a, b, c) = (
        order("a", 1, 98.0, 0.01, false),
        order("b", 1, 98.0, 0.02, true),
        order("c", 1, 98.0, 0.01, true),
    );
    assert_eq!(marker_face(&[&a], Some(0.001)), face("0.010", "×1", false));
    assert_eq!(marker_face(&[&a, &b], Some(0.001)), face("×2 0.030", "×2", false), "mixed: filled");
    assert_eq!(marker_face(&[&b, &c], Some(0.001)), face("×2 0.030", "×2", true), "stops: hollow");
    // Review fix 4: the DOM's fixed four decimals printed this order as `0.0000`.
    let tiny = order("t", 1, 98.0, 0.000_01, false);
    assert_eq!(marker_face(&[&tiny], Some(0.000_01)).full, "0.00001");
    // The lot not known yet: the size's own decimals.
    assert_eq!(marker_face(&[&a, &b], None).full, "×2 0.03");
}

/// Where the size does not fit, the marker keeps the COUNT — a click cancels every order it
/// stands for — and where not even the count fits it says nothing rather than half of it.
#[test]
fn a_marker_that_cannot_hold_its_size_keeps_its_count() {
    let f = face("×2 0.030", "×2", false);
    let glyphs = |w: &str| w.chars().count() as f32 * 6.0;
    assert_eq!(marker_words(&f, 48.0, glyphs), Some("×2 0.030"));
    assert_eq!(marker_words(&f, 47.0, glyphs), Some("×2"));
    assert_eq!(marker_words(&f, 11.0, glyphs), None);
    let one = face("123.456", "×1", false);
    assert_eq!(marker_words(&one, 30.0, glyphs), Some("×1"), "one order's count, too");
}

/// The window's frame and body margin across the window, a bound above the desktop's
/// (`crates/vike-app-core/src/ui/window/state/mod.rs`'s `TOOL_BODY_MARGIN` is 8 a side).
const WINDOW_SIDES: f32 = 20.0;

/// Review fix 4, measured: at the sizes the window opens at, in every look up to the widest
/// (Large text; Comfortable's wider gap narrows the ladder beside the ticket), the Buy column
/// holds a two-digit count (`×99`) and one order's size (`0.010`) in the mono caption the
/// marker is drawn in. Where a size does not fit beside its count, [`marker_words`] keeps the
/// count.
#[test]
fn a_markers_count_and_a_single_size_fit_the_order_column_at_the_window_sizes() {
    const WORDS: [&str; 2] = ["×99", "0.010"];
    let mut h = Harness::builder().build_ui_state(
        |ui, widths: &mut Vec<(ThemeTextSize, &'static str, f32)>| {
            // Measured in the app's type: kittest's first frame has egui's default fonts.
            if !vike_ui_theme::harness::type_ready(ui.ctx()) {
                return;
            }
            widths.clear();
            for text_size in ThemeTextSize::ALL {
                let a = ThemeAppearance { text_size, ..ThemeAppearance::default() };
                let t = ThemeTokens::from_appearance(&a);
                for words in WORDS {
                    let font = t.mono(ThemeTextRole::Caption);
                    let g = ui.painter().layout_no_wrap(words.to_string(), font, t.theme.text);
                    widths.push((text_size, words, g.size().x));
                }
            }
        },
        Vec::new(),
    );
    h.run();
    let widths = h.state().clone();
    assert_eq!(widths.len(), ThemeTextSize::ALL.len() * WORDS.len(), "measured: {widths:?}");
    for density in ThemeDensity::ALL {
        let m = density.metrics();
        for (panel, size) in [
            (TradeWindowTicketPlace::Beside, trade::BESIDE_SIZE),
            (TradeWindowTicketPlace::Under, trade::UNDER_SIZE),
        ] {
            let view = TradeWindowPanes { chart: false, ladder: true, panel };
            for (text_size, words, w) in &widths {
                // The ticket grows with the text, so the ladder beside it is narrower at the
                // bigger sizes: the order column is measured at each size's own ladder.
                let look = ThemeAppearance {
                    density,
                    text_size: *text_size,
                    ..ThemeAppearance::default()
                };
                let ticket_w = layout::ticket_w(&ThemeTokens::from_appearance(&look));
                // A window opens as much wider as the ticket grew: the ladder keeps its width.
                let body = Rect::from_min_size(
                    Pos2::ZERO,
                    layout::spec_size(view, ticket_w).max(size) - vec2(WINDOW_SIDES, 0.0),
                );
                let ladder =
                    layout::split(body, view, &m, ticket_w).ladder.expect("a ladder at this size");
                let buy = TradeWindowLadderColumns::new(panel, false, ladder.width(), 0.0)
                    .rect(TradeWindowLadderColumn::Buy, ladder);
                let room = marker_room(buy);
                assert!(
                    *w <= room,
                    "{density:?} / {text_size:?} / {panel:?}: {words:?} is {w} wide, the \
                     order column holds {room}"
                );
            }
        }
    }
}

#[cfg(test)]
mod absence;
#[cfg(test)]
mod screen;
