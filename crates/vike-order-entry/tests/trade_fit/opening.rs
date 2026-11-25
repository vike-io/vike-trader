//! The window as it opens: the whole form at the size a window opens at, the divider of a form that
//! scrolls, the compact ticket's one height and the grown refusal strip.

use super::*;

/// What egui_kittest's central panel keeps each side (rule 1): a harness this much larger than a
/// body, each way, hands `draw` that body.
const HARNESS_MARGIN: f32 = 8.0;

/// The tokens the last frame drew with.
fn tokens(h: &Harness<'static, Fixture>) -> ThemeTokens {
    h.state().drawn.expect("the window drew").1
}

/// Draw `h` as the desktop draws a window `size` large: `draw` is handed the window's body, its
/// size less what the window puts around it (`layout::chrome`). The sweeps above draw the harness
/// AS the window, which hands `draw` a rect up to 33 pt taller than the desktop does.
fn as_window(h: &mut Harness<'static, Fixture>, size: egui::Vec2) {
    let body = size - layout::chrome();
    h.set_size(body + egui::Vec2::splat(2.0 * HARNESS_MARGIN));
    h.run();
    let drawn = h.state().drawn.expect("the window drew").0.size();
    assert!((drawn - body).length() <= EPS, "the harness drew a {drawn:?} body for {body:?}");
}

/// Ruling B1 of the render check (2026-10-03): the window a trader opens shows the full ticket's
/// WHOLE form, down to TP/SL, without a scroll, in every look. The render check measured TP/SL
/// 61 pt below the fold of the 600 × 560 window at Comfortable density, Reduce only half cut, and
/// nothing on screen saying the form went on.
///
/// Drawn as the desktop draws the window ([`as_window`]), in the state a trader fills the ticket
/// from (a limit typed, TP/SL off: [`Ticket::Resting`]) and in every content, the spot lot whose
/// quick sizes are the widest among them: every control the form draws lies inside the form's
/// visible rect, so not one of them is a scroll away.
fn the_whole_form_shows_at_the_size_a_window_opens_at(view: trade_window::TradeWindowPanes) {
    let mut found = Vec::new();
    for density in ThemeDensity::ALL {
        for text_size in ThemeTextSize::ALL {
            let look = ThemeAppearance { density, text_size, ..ThemeAppearance::default() };
            for content in CONTENTS {
                let mut h = window(look, content, Ticket::Resting);
                h.state_mut().state.view = view;
                let size = layout::window_size(view, &h.ctx);
                if matches!(content, Content::Swap) {
                    println!("{view:?} {density:?}/{text_size:?}: a window opens at {size:?}");
                }
                as_window(&mut h, size);
                let at = format!("{content:?}/{density:?}/{text_size:?} at {size:?}");
                let (cs, regions) = drawn(&h);
                let Some(form) = regions.get(FORM).copied() else {
                    found.push(format!("{at}: no order form"));
                    continue;
                };
                let mut take_profit_stop_loss = false;
                let mut last = form.min.y;
                for c in cs.iter().filter(|c| c.region.as_deref() == Some(FORM)) {
                    take_profit_stop_loss |= c.text == "TP/SL";
                    last = last.max(c.rect.max.y);
                    let below = c.rect.max.y - form.max.y;
                    if below > EPS {
                        found.push(format!("{at}: {:?} ends {below:.1} pt below the form", c.text));
                    }
                }
                // The room the form does not use (the second quick-size row a coarse lot leaves
                // empty, and the opening height's rounding up): recorded per look in spec §9.1.
                println!("{at}: {:.1} pt free under the form's last row", form.max.y - last);
                if !take_profit_stop_loss {
                    found.push(format!("{at}: the form draws no TP/SL"));
                }
            }
        }
    }
    assert!(found.is_empty(), "{} control(s) a scroll away:\n{}", found.len(), found.join("\n"));
}

#[test]
fn the_whole_form_shows_beside_the_ladder_at_the_size_a_window_opens_at() {
    the_whole_form_shows_at_the_size_a_window_opens_at(BESIDE);
}

#[test]
fn the_whole_form_shows_with_the_ticket_alone_at_the_size_a_window_opens_at() {
    the_whole_form_shows_at_the_size_a_window_opens_at(ALONE);
}

/// The dividers the last frame painted under the full ticket's form: horizontal lines in the
/// theme's border colour, across the whole ticket, in the gap between the form's visible bottom
/// and the rows pinned under it.
fn dividers(h: &Harness<'static, Fixture>) -> usize {
    let t = tokens(h);
    let (_, regions) = drawn(h);
    let (form, ticket) = (regions[FORM], regions[TICKET]);
    h.output()
        .shapes
        .iter()
        .filter(|c| match &c.shape {
            egui::Shape::LineSegment { points: [a, b], stroke } => {
                stroke.color == t.theme.border
                    && (a.y - b.y).abs() <= EPS
                    && a.y > form.max.y
                    && a.y < form.max.y + t.metrics.gap
                    && a.x.min(b.x) <= ticket.min.x + EPS
                    && a.x.max(b.x) >= ticket.max.x - EPS
            }
            _ => false,
        })
        .count()
}

/// Ruling B2: a form that goes on below its visible bottom SAYS so, with a divider in the gap
/// between the form and the rows pinned under it. A window the trader made shorter shows it; the
/// window as it opens, where nothing scrolls, shows none. The divider is painted, never laid out:
/// Buy starts a gap under the form with it or without it.
#[test]
fn a_form_that_scrolls_says_so_with_a_divider_and_one_that_does_not_draws_none() {
    for density in ThemeDensity::ALL {
        for text_size in ThemeTextSize::ALL {
            let look = ThemeAppearance { density, text_size, ..ThemeAppearance::default() };
            let mut h = window(look, Content::Swap, Ticket::Resting);
            h.state_mut().state.view = BESIDE;
            let at = format!("{density:?}/{text_size:?}");
            for (size, want, what) in [
                (layout::window_size(BESIDE, &h.ctx), 0, "the window as it opens scrolls nothing"),
                (vec2(trade::BESIDE_SIZE.x, 340.0), 1, "a window made shorter scrolls its form"),
            ] {
                as_window(&mut h, size);
                assert_eq!(dividers(&h), want, "{at} at {size:?}: {what}");
                let (cs, regions) = drawn(&h);
                let buy = cs.iter().find(|c| c.text.starts_with("Buy ")).expect("a Buy button");
                let gap = tokens(&h).metrics.gap;
                let under = buy.rect.min.y - regions[FORM].max.y;
                assert!((under - gap).abs() <= EPS, "{at} at {size:?}: Buy starts {under} under");
            }
        }
    }
}

/// The compact ticket's band under its cancel row (the ticket's bottom less the cancel row's), and
/// whether Buy and Sell are stacked, one to a row.
fn compact_band(h: &Harness<'static, Fixture>) -> (f32, bool) {
    let (cs, regions) = drawn(h);
    let at = |prefix: &str| button(&cs, prefix).map(|(_, rect)| rect);
    let (buy, sell) = (at("Buy ").expect("Buy"), at("Sell ").expect("Sell"));
    let cancel = at("Cancel all").expect("Cancel all");
    (regions[TICKET].max.y - cancel.max.y, (buy.min.y - sell.min.y).abs() > EPS)
}

/// Item C of the render check (2026-10-03), as the v3 design's compact ticket keeps it: the ticket's
/// height is fixed before it is drawn (`layout::under_ticket_h`) so that Buy and Sell can stack, one
/// to a row, without moving the ladder above it: a label NAMES the order and is never cut, so where
/// both do not fit side by side (a long size, a narrow window, a loose look) they stack and what is
/// under them moves down or gives up a row, inside the same ticket. A height that followed the
/// labels would move every ladder row whenever they stacked, and a quote-sized order's labels
/// change with every tick of the book. So: at the 320 × 680 window, side by side and stacked (a long
/// size), the ticket is the same rect and the cancel row, the last thing in it, lies inside it. The
/// band left under the cancel row is printed per look.
///
/// (This asserted that the band side by side was a whole row, `control_height`, while the ticket's room
/// was seven rows. The v3 ticket's rows are not equal — a position strip, tall Buy and Sell — and
/// the room it is given is `layout::under_ticket_h`'s; a band's height is not the property.)
#[test]
fn the_compact_ticket_is_one_height_whether_buy_and_sell_stack_or_not() {
    for density in ThemeDensity::ALL {
        for text_size in ThemeTextSize::ALL {
            let look = ThemeAppearance { density, text_size, ..ThemeAppearance::default() };
            let at = format!("{density:?}/{text_size:?}");
            let mut h = window(look, Content::Swap, Ticket::Resting);
            h.state_mut().state.view = UNDER;
            as_window(&mut h, trade::UNDER_SIZE);
            let (band, stacked) = compact_band(&h);
            assert!(!stacked, "{at}: at 320 × 680 Buy and Sell sit side by side");
            assert!(band >= -EPS, "{at}: the cancel row lies inside the ticket ({band})");
            println!("{at}: side by side, the band under the cancel row is {band:.2} pt");
            let ticket = drawn(&h).1[TICKET];
            // A long size: "Buy 1234567890.123 MKT" and its Sell do not fit side by side.
            h.state_mut().state.size = "1234567890.123".to_string();
            h.run();
            let (band, stacked) = compact_band(&h);
            assert!(stacked, "{at}: labels this long stack");
            assert!(band >= -EPS, "{at}: stacked, the cancel row lies inside the ticket ({band})");
            println!("{at}: stacked, the band under the cancel row is {band:.2} pt");
            assert_eq!(drawn(&h).1[TICKET], ticket, "{at}: the ticket is the same rect stacked");
        }
    }
}

/// FW6, I3 (the review's minor 4): with nothing held and no line from the app, the strip says why
/// the ticket sends nothing, and grows for a sentence two Body lines cannot hold — a strip no other
/// scene draws, for they all carry the app's line. In every view, at Large text in every density,
/// at every point from 260 to 319 pt, with the longest refusal the ticket says (TP/SL ticked on an
/// engine whose lane cannot hold its stop-loss): the seven rules hold for the grown strip and nothing
/// in it is cut. CONTROLS: the strip says the sentence at every width, and it grew at some.
#[test]
fn the_grown_refusal_strip_fits_from_260_to_319_pt_at_large_text() {
    let refusal = ticket::take_profit_stop_loss_refusal(LANE_WHY);
    let mut found: BTreeMap<String, Vec<u32>> = BTreeMap::new();
    let mut grew = false;
    for density in ThemeDensity::ALL {
        let look = ThemeAppearance {
            density,
            text_size: ThemeTextSize::Standard,
            ..ThemeAppearance::default()
        };
        for (view, height) in [(BESIDE, 560.0), (UNDER, 680.0), (ALONE, 560.0)] {
            let mut h = window(look, Content::Swap, Ticket::Refused);
            h.state_mut().state.view = view;
            for w in 260..=319 {
                h.set_size(egui::vec2(w as f32, height));
                h.run();
                let mut seen = violations(&h);
                let (controls, _) = drawn(&h);
                if !controls.iter().any(|c| c.region.as_deref() == Some(STRIP) && c.text == refusal)
                {
                    seen.push("CONTROL: the strip does not say the refusal".to_string());
                }
                grew |= h.state().strip_h > least_strip_h(&h) + EPS;
                for v in seen {
                    found.entry(format!("{view:?}/{density:?}: {v}")).or_default().push(w);
                }
            }
        }
    }
    assert!(grew, "CONTROL: the refusal never grew the strip, so the sweep proved nothing");
    assert_none(found);
}

/// The strip's least height in the look the last frame drew, as `status_line::idle_height` (the window
/// module's own) counts it: one control row or two lines of Body text, whichever is taller, and a
/// point above and below.
fn least_strip_h(h: &Harness<'static, Fixture>) -> f32 {
    let t = tokens(h);
    let body = t.font(vike_ui_theme::type_scale::ThemeTextRole::Body);
    let row = h.ctx.fonts_mut(|f| f.row_height(&body));
    t.metrics.control_height.max(2.0 * row) + 2.0
}

/// Where the button whose name starts with `prefix` is, with its name.
fn button(cs: &[Control], prefix: &str) -> Option<(String, Rect)> {
    cs.iter()
        .find(|c| c.role == Role::Button && c.text.starts_with(prefix))
        .map(|c| (c.text.clone(), c.rect))
}

/// FW5 and FW6 together (the final merge, 2026-10-03): the window AS IT OPENS
/// (`layout::window_size`, drawn as the desktop draws it, [`as_window`]) with TP/SL then ticked and
/// refused on the account's lane ([`Ticket::Refused`]). The opening height counts the strip at its
/// least, and this is the state that grows it (by some 20 pt in the 280 pt ticket alone). In every
/// look, every content and each view: the seven rules hold, so the strip's reason is said whole;
/// where the full ticket's form goes on more than a point below what it shows, its divider says so,
/// and where it ends more than a point above, there is none (between the two egui's rounding
/// decides, so neither is asserted); Buy starts a gap under the form; and a click on the refused
/// Buy holds nothing (the account is LIVE, so an order that got through would wait in the strip)
/// and moves neither Buy nor Sell. CONTROLS: the strip says the refusal in every one, and it grew
/// in the ticket alone in some look.
#[test]
fn a_refused_take_profit_stop_loss_in_the_window_as_it_opens_is_said_whole_and_a_form_it_scrolls_says_so()
 {
    let refusal = ticket::take_profit_stop_loss_refusal(LANE_WHY);
    let mut found = Vec::new();
    let mut grew_alone = false;
    for density in ThemeDensity::ALL {
        for text_size in ThemeTextSize::ALL {
            let look = ThemeAppearance { density, text_size, ..ThemeAppearance::default() };
            for content in CONTENTS {
                let mut h = window(look, content, Ticket::Refused);
                for view in [BESIDE, UNDER, ALONE] {
                    h.state_mut().state.view = view;
                    let size = layout::window_size(view, &h.ctx);
                    as_window(&mut h, size);
                    let at = format!("{view:?} {content:?}/{density:?}/{text_size:?} at {size:?}");
                    found.extend(violations(&h).into_iter().map(|v| format!("{at}: {v}")));
                    let grew = h.state().strip_h - least_strip_h(&h);
                    grew_alone |= view == ALONE && grew > 1.0;
                    let (cs, regions) = drawn(&h);
                    if !cs.iter().any(|c| c.region.as_deref() == Some(STRIP) && c.text == refusal) {
                        found.push(format!("{at}: CONTROL: the strip does not say the refusal"));
                    }
                    let (Some((buy, buy_at)), Some((_, sell_at))) =
                        (button(&cs, "Buy "), button(&cs, "Sell "))
                    else {
                        found.push(format!("{at}: no Buy or no Sell"));
                        continue;
                    };
                    if let Some(form) = regions.get(FORM).copied() {
                        let over = cs
                            .iter()
                            .filter(|c| c.region.as_deref() == Some(FORM))
                            .map(|c| c.rect.max.y - form.max.y)
                            .fold(f32::MIN, f32::max);
                        let shown = dividers(&h);
                        if (over > 1.0 && shown != 1) || (over < -1.0 && shown != 0) {
                            found.push(format!(
                                "{at}: the form ends {over:.1} pt past what it shows, with {shown} \
                                 divider(s)"
                            ));
                        }
                        let under = buy_at.min.y - form.max.y;
                        if (under - tokens(&h).metrics.gap).abs() > EPS {
                            found.push(format!("{at}: Buy starts {under:.1} pt under the form"));
                        }
                        println!(
                            "{at}: the strip grew {grew:.1} pt; the form ends {over:.1} pt past"
                        );
                    } else {
                        println!("{at}: the strip grew {grew:.1} pt (the compact ticket)");
                    }
                    h.get_by_label(&buy).click();
                    h.run();
                    // The pointer leaves, so no hover text is left open over the next view.
                    h.hover_at(pos2(1.0, 1.0));
                    h.run();
                    let (cs, _) = drawn(&h);
                    if h.state().state.held.is_some() {
                        found.push(format!("{at}: a click on the refused {buy:?} held an order"));
                    }
                    let moved = button(&cs, "Buy ").map(|b| b.1) != Some(buy_at)
                        || button(&cs, "Sell ").map(|s| s.1) != Some(sell_at);
                    if moved {
                        found.push(format!(
                            "{at}: a click on the refused {buy:?} moved Buy or Sell"
                        ));
                    }
                }
            }
        }
    }
    assert!(grew_alone, "CONTROL: the refusal never grew the ticket alone's strip");
    assert!(found.is_empty(), "{} finding(s):\n{}", found.len(), found.join("\n"));
}
