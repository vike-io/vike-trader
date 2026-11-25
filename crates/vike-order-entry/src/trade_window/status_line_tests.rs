//! The status strip's unit and screen tests.

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use vike_marketdata::L2Book;
use vike_ui_theme::appearance::ThemeAppearance;
use vike_ui_theme::metrics::ThemeDensity;
use vike_ui_theme::theme::ThemePaletteId;
use vike_ui_theme::type_scale::ThemeTextSize;
use vike_venues::VenueCapabilities;

use super::*;
use crate::trade_window::{TradeWindowOrigin, TradeWindowTicketKind, TradeWindowTradable};
use vike_model::Grid;
use vike_model::feed_status::{DataClientConnectionState, DataClientStatus};

struct Fixture {
    state: TradeWindowState,
    emitted: Vec<TradeWindowAction>,
}

fn buy() -> TradeWindowAction {
    TradeWindowAction::Place {
        side: 1,
        order_type: TradeWindowTicketKind::Limit,
        price: Some(99.5),
        quantity: 0.01,
        reduce_only: false,
        post_only: false,
        exits: None,
        origin: TradeWindowOrigin::Ticket,
    }
}

/// The strip alone, a 320 pt window's width, holding `held` for a confirm.
fn harness(held: TradeWindowAction) -> Harness<'static, Fixture> {
    harness_in(ThemeAppearance::default(), held, 320.0)
}

/// [`harness`] under the appearance `look`, in a window `width` wide (the strip 16 pt narrower).
fn harness_in(
    look: ThemeAppearance,
    held: TradeWindowAction,
    width: f32,
) -> Harness<'static, Fixture> {
    harness_with(look, Some(held), width, None)
}

/// The strip under `look`, `width` wide, holding `held` (or nothing) with the depth link's line
/// `source`.
fn harness_with(
    look: ThemeAppearance,
    held: Option<TradeWindowAction>,
    width: f32,
    source: Option<DataClientStatus>,
) -> Harness<'static, Fixture> {
    let book = L2Book::new(0.1);
    let state = TradeWindowState { held, ..TradeWindowState::default() };
    Harness::builder().with_size(egui::vec2(width, 120.0)).build_ui_state(
        move |ui, f: &mut Fixture| {
            if !vike_ui_theme::harness::appearance_ready(ui.ctx(), &look) {
                return;
            }
            let inputs = TradeWindowInputs {
                venue: "binance",
                venue_label: "Binance",
                product: "",
                account: None,
                symbol: "BTCUSDT",
                base: "BTC",
                quote: "USDT",
                mode: Some(AccountTier::Live),
                tradable: TradeWindowTradable::Yes,
                grid: Grid {
                    tick_size: Some(0.1),
                    lot_size: Some(0.001),
                    min_notional: None,
                    min_qty: Some(0.001),
                },
                book: &book,
                last: Some(99.5),
                stale: false,
                source: source.as_ref(),
                absence: None,
                top: None,
                orders: &[],
                orders_why: None,
                position: None,
                buying_power: None,
                value: vike_model::ContractValue::default(),
                capabilities: VenueCapabilities::UNSUPPORTED,
                bracket_why: None,
                bracket_wire: false,
                post_only_wire: false,
                matches: &[],
                recent: &[],
                accounts: &[],
                accounts_why: None,
                unconnected: &[],
                tape: &[],
                status: None,
                held_says: None,
            };
            let t = ThemeTokens::of(ui.ctx());
            strip(ui, &t, &mut f.state, &inputs, &mut f.emitted);
        },
        Fixture { state, emitted: Vec::new() },
    )
}

/// The window names its own depth link at the right end of an idle strip (the v3 design's bar
/// carries only the price): a link nobody reported on reads `FEED ?`, never `FEED DOWN`, and a
/// live one `FEED UP`. A held order's prompt takes the strip's room instead (Place and Cancel
/// sit where the word would).
#[test]
fn an_idle_strip_names_the_depth_link_and_a_held_one_does_not() {
    let unknown = harness_with(ThemeAppearance::default(), None, 600.0, None);
    let mut unknown = unknown;
    unknown.run();
    assert!(unknown.query_by_label("FEED ?").is_some());
    assert!(unknown.query_by_label("FEED DOWN").is_none());
    let mut live = harness_with(
        ThemeAppearance::default(),
        None,
        600.0,
        Some(DataClientStatus::new(
            DataClientConnectionState::Connected,
            "datahub 127.0.0.1:7878 — 1/1 stream(s) live",
        )),
    );
    live.run();
    assert!(live.query_by_label("FEED UP").is_some());
    let mut held = harness_with(ThemeAppearance::default(), Some(buy()), 600.0, None);
    held.run();
    assert!(held.query_by_label_contains("FEED").is_none(), "the prompt has the room");
}

/// A strip too narrow to carry the word leaves it to the bar's last row ([`feeds_in_the_strip`]).
#[test]
fn a_narrow_strip_leaves_the_feed_word_to_the_bar() {
    let mut h = harness_with(ThemeAppearance::default(), None, 240.0, None);
    h.run();
    assert!(h.query_by_label_contains("FEED").is_none());
}

/// Place sends the held order exactly as it was held, and the strip lets go of it.
#[test]
fn place_sends_the_held_order_once() {
    let mut h = harness(buy());
    h.run();
    assert!(h.query_by_label_contains("Buy").is_some(), "the prompt names the order");
    h.get_by_label("Place").click();
    h.run();
    assert_eq!(h.state().emitted, [buy()]);
    assert!(h.state().state.held.is_none());
}

/// W2 review minor 2, the W3 fix-round re-review and W4 fix round 1 (I-2): at every strip a
/// supported window gives — 244 pt (the fit sweep's 260 pt window), 264 pt (the ticket-alone
/// window's 280) and 304 pt (the bottom-panel window's 320) — in every density and text size, a
/// held order's prompt is cut NOWHERE: not its side, size or price, and not what it carries
/// (`TP … / SL …`, a ticket order's protection, or `no TP/SL`, the one thing a ladder click's
/// prompt must say while TP/SL is ticked). Read off what is PAINTED: a galley the kit elided
/// carries `elided`, where the accessibility tree would still hold the whole text. And the
/// prompt is read BEFORE its answers.
#[test]
fn a_held_prompt_keeps_its_price_and_its_exits_at_every_strip_width() {
    let place = |price, exits| TradeWindowAction::Place {
        side: -1,
        order_type: TradeWindowTicketKind::Limit,
        price: Some(price),
        quantity: 0.012,
        reduce_only: false,
        post_only: false,
        exits,
        origin: TradeWindowOrigin::Ladder,
    };
    let exits = |take_profit, stop_loss| {
        Some(crate::trade_window::TradeWindowTicketExits { take_profit, stop_loss })
    };
    // The fit scene's price with its exits and without, and an index's eight-digit price with
    // its exits — the prompt two lines cannot hold whole in a narrow strip at Large text.
    let prompts = [
        (65_432.1, exits(65_105.9, 65_759.3), "65,432.1", "TP 65,105.9 / SL 65,759.3"),
        (65_432.1, None, "65,432.1", "no TP/SL"),
        (
            12_345_678.9,
            exits(12_283_950.5, 12_382_715.9),
            "12,345,678.9",
            "TP 12,283,950.5 / SL 12,382,715.9",
        ),
    ];
    let mut cut = Vec::new();
    for strip_w in [244.0, 264.0, 304.0] {
        for density in ThemeDensity::ALL {
            for text_size in ThemeTextSize::ALL {
                let look = ThemeAppearance { density, text_size, ..ThemeAppearance::default() };
                for (price, exits, priced, carries) in prompts {
                    let mut h = harness_in(look, place(price, exits), strip_w + 16.0);
                    h.run();
                    let what = format!("{strip_w} pt, {density:?}/{text_size:?}");
                    for c in &h.output().shapes {
                        if let egui::Shape::Text(t) = &c.shape
                            && t.galley.elided
                        {
                            cut.push(format!(
                                "{what}: {:?} elided at {:.1} pt",
                                t.galley.text(),
                                t.galley.rect.width()
                            ));
                        }
                    }
                    let order: Vec<String> = h
                        .root()
                        .children_recursive()
                        .filter_map(|n| {
                            let a = n.accesskit_node();
                            a.value()
                                .or_else(|| a.label())
                                .filter(|_| matches!(a.role(), Role::Label | Role::Button))
                        })
                        .collect();
                    // The prompt is every label read before Place, its lines joined as one text.
                    let place_at = order.iter().position(|w| w == "Place");
                    let cancel_at = order.iter().position(|w| w == "Cancel");
                    let asked = order[..place_at.unwrap_or(0)].join(" ");
                    assert!(
                        asked.contains(priced) && asked.contains(carries),
                        "{what}: the price and {carries:?} are read before Place: {order:?}"
                    );
                    assert!(place_at < cancel_at, "{what}: then Place, then Cancel: {order:?}");
                }
            }
        }
    }
    assert!(cut.is_empty(), "{}", cut.join("\n"));
}

/// A measurement for the report, not a check: at the narrowest strip (304 pt), in every look,
/// the room the prompt has left of Place and Cancel, how wide each of its two lines is, and how
/// wide the old ONE-line prompt was. Run it with
/// `cargo test -p vike-order-entry --lib print_prompt_room -- --ignored --nocapture`.
#[test]
#[ignore = "a measurement for the report: the held prompt's room at the narrowest strip"]
fn print_prompt_room() {
    const STRIP_W: f32 = 304.0;
    let place = |exits| TradeWindowAction::Place {
        side: 1,
        order_type: TradeWindowTicketKind::Limit,
        price: Some(65_432.1),
        quantity: 0.012,
        reduce_only: false,
        post_only: false,
        exits,
        origin: TradeWindowOrigin::Ladder,
    };
    let bracket = Some(crate::trade_window::TradeWindowTicketExits {
        take_profit: 65_759.6,
        stop_loss: 65_236.1,
    });
    let grid = Grid {
        tick_size: Some(0.1),
        lot_size: Some(0.001),
        min_notional: None,
        min_qty: Some(0.001),
    };
    let mut h = Harness::builder().build_ui_state(
        move |ui, out: &mut Vec<String>| {
            if !vike_ui_theme::harness::type_ready(ui.ctx()) {
                return;
            }
            out.clear();
            for density in ThemeDensity::ALL {
                for text_size in ThemeTextSize::ALL {
                    let a = ThemeAppearance { density, text_size, ..ThemeAppearance::default() };
                    let t = ThemeTokens::from_appearance(&a);
                    let w = |s: &str| {
                        let font = t.font(ThemeTextRole::Body);
                        ui.painter().layout_no_wrap(s.to_string(), font, t.theme.text).size().x
                    };
                    let answers =
                        button_w(ui.ctx(), &t, "Place") + button_w(ui.ctx(), &t, "Cancel");
                    let room = STRIP_W - answers - 2.0 * t.metrics.gap;
                    for exits in [bracket, None] {
                        let (order, carries) = describe_parts(&place(exits), grid, None);
                        let carries = carries.unwrap_or_default();
                        let one_line = format!("{order}, {carries}?");
                        out.push(format!(
                            "{density:?}/{text_size:?}: room {room:.1} pt; {:?} {:.1}, {:?} \
                             {:.1}; one line {:.1}",
                            format!("{order},"),
                            w(&format!("{order},")),
                            format!("{carries}?"),
                            w(&format!("{carries}?")),
                            w(&one_line)
                        ));
                    }
                }
            }
        },
        Vec::new(),
    );
    h.run();
    for line in h.state() {
        println!("{line}");
    }
}

/// W4 fix round 2, N4: where the words do not fit (a 244 pt strip at Comfortable density and
/// Large text), the answers are ✓ and ✕ — and they ANSWER: ✓ is named Place and sends the held
/// order, ✕ is named Cancel and sends nothing and says so.
#[test]
fn the_check_and_cross_answers_place_and_cancel() {
    let look = ThemeAppearance {
        density: ThemeDensity::Comfortable,
        text_size: ThemeTextSize::Standard,
        ..ThemeAppearance::default()
    };
    let held = TradeWindowAction::Place {
        side: -1,
        order_type: TradeWindowTicketKind::Limit,
        price: Some(65_432.1),
        quantity: 0.012,
        reduce_only: false,
        post_only: false,
        exits: Some(crate::trade_window::TradeWindowTicketExits {
            take_profit: 65_105.9,
            stop_loss: 65_759.3,
        }),
        origin: TradeWindowOrigin::Ladder,
    };
    let painted = |h: &Harness<'static, Fixture>| -> Vec<String> {
        let mut out = Vec::new();
        for c in &h.output().shapes {
            if let egui::Shape::Text(t) = &c.shape {
                out.push(t.galley.text().to_string());
            }
        }
        out
    };
    let mut h = harness_in(look, held.clone(), 260.0);
    h.run();
    let words = painted(&h);
    let check = icons::CHECK.rich().text().to_string();
    let cross = icons::CLOSE.rich().text().to_string();
    assert!(words.contains(&check) && words.contains(&cross), "{words:?}");
    assert!(!words.iter().any(|w| w == "Place" || w == "Cancel"), "icons, not words");
    // F2: each glyph is painted ON the button it answers with — the check on the one named
    // Place, the cross on the one named Cancel. A cross on the button that places would draw a
    // refusal on the one control that sends.
    let glyph_at = |h: &Harness<'static, Fixture>, glyph: &str| -> egui::Pos2 {
        let at: Vec<egui::Pos2> = h
            .output()
            .shapes
            .iter()
            .filter_map(|c| match &c.shape {
                egui::Shape::Text(t) if t.galley.text() == glyph => {
                    Some(t.visual_bounding_rect().center())
                }
                _ => None,
            })
            .collect();
        match at.as_slice() {
            [one] => *one,
            other => panic!("one {glyph:?} painted, found {other:?}"),
        }
    };
    let (place, cancel) = (h.get_by_label("Place").rect(), h.get_by_label("Cancel").rect());
    assert!(place.contains(glyph_at(&h, &check)), "the check is on Place: {place:?}");
    assert!(cancel.contains(glyph_at(&h, &cross)), "the cross is on Cancel: {cancel:?}");
    h.get_by_label("Place").click();
    h.run();
    assert_eq!(h.state().emitted, std::slice::from_ref(&held), "✓ places the held order");
    assert!(h.state().state.held.is_none());

    let mut h = harness_in(look, held, 260.0);
    h.run();
    h.get_by_label("Cancel").click();
    h.run();
    assert_eq!(
        h.state().emitted,
        [TradeWindowAction::Note {
            kind: TradeWindowStatusLineKind::Info,
            text: NOT_SENT.to_string()
        }],
        "✕ sends nothing and says so"
    );
    assert!(h.state().state.held.is_none());
}

/// Cancel sends nothing, lets go of the order, and says so.
#[test]
fn cancel_sends_nothing_and_says_so() {
    let mut h = harness(buy());
    h.run();
    h.get_by_label("Cancel").click();
    h.run();
    assert_eq!(
        h.state().emitted,
        [TradeWindowAction::Note {
            kind: TradeWindowStatusLineKind::Info,
            text: NOT_SENT.to_string()
        }]
    );
    assert!(h.state().state.held.is_none());
}

/// Which colours the strip's dot and words wear, pinned to what the strip painted before they moved to
/// `ui-theme.toml`'s `strip` map: Info a quiet note, Ok and Error their status colour in both, the ticket's
/// refusal an amber dot, Ready the quiet dot. Changing a row of the table changes the strip, and this
/// test is what says so by name.
#[test]
fn a_message_wears_the_dot_and_the_words_the_strip_map_gives_it() {
    for id in ThemePaletteId::ALL {
        let t = ThemeTokens::from_appearance(&ThemeAppearance {
            theme: id,
            ..ThemeAppearance::default()
        });
        let (text2, text3) = (t.theme.text_secondary, t.theme.text_tertiary);
        for (message, dot, words) in [
            (TradeWindowStatusLineMessage::Info, text3, text2),
            (TradeWindowStatusLineMessage::Ok, ThemeStatus::Ok.color(), ThemeStatus::Ok.color()),
            (
                TradeWindowStatusLineMessage::Error,
                ThemeStatus::Error.color(),
                ThemeStatus::Error.color(),
            ),
            (TradeWindowStatusLineMessage::Refusal, ThemeStatus::Warning.color(), text2),
            (TradeWindowStatusLineMessage::Ready, text3, text2),
        ] {
            assert_eq!(message.dot(&t), dot, "{id:?} {message:?}: the dot");
            assert_eq!(message.words(&t), words, "{id:?} {message:?}: the words");
        }
    }
}

/// The map is the strip's own: every message reads the row of its own name, no two share one, and no
/// row of the `strip` map is left without a message.
#[test]
fn every_message_has_its_own_row_and_every_row_of_the_strip_map_has_its_message() {
    let all = [
        TradeWindowStatusLineMessage::Info,
        TradeWindowStatusLineMessage::Ok,
        TradeWindowStatusLineMessage::Error,
        TradeWindowStatusLineMessage::Refusal,
        TradeWindowStatusLineMessage::Ready,
    ];
    for m in all {
        assert_eq!(m.row().key, format!("{m:?}").to_uppercase(), "{m:?} reads another row");
        assert_eq!(all.iter().filter(|o| o.row() == m.row()).count(), 1, "{m:?}'s row is shared");
    }
    for row in maps::strip::ALL {
        assert!(all.iter().any(|m| m.row() == *row), "{} is no message of the strip", row.key);
    }
}

/// The line the app composes keeps its kind: each `TradeWindowStatusLineKind` is the message of its own name.
#[test]
fn a_composed_line_is_the_message_of_its_kind() {
    for (kind, message) in [
        (TradeWindowStatusLineKind::Info, TradeWindowStatusLineMessage::Info),
        (TradeWindowStatusLineKind::Ok, TradeWindowStatusLineMessage::Ok),
        (TradeWindowStatusLineKind::Error, TradeWindowStatusLineMessage::Error),
    ] {
        assert_eq!(TradeWindowStatusLineMessage::from(kind), message);
    }
}
