//! A LIVE window is unmistakable, and the strip says why the ticket sends nothing (FW6).

use super::*;

/// Every rectangle the last frame painted, and its fill.
fn filled(h: &Harness<'_, Fixture>) -> Vec<(egui::Rect, egui::Color32)> {
    flat_shapes(h)
        .into_iter()
        .filter_map(|s| match s {
            egui::Shape::Rect(r) => Some((r.rect, r.fill)),
            _ => None,
        })
        .collect()
}

/// The fill of the chip the instrument bar painted `word` on: the smallest filled rectangle under
/// that word, the chip's own pill (the bar's background lies under it too). `None` where the bar
/// paints no such word.
fn chip_fill(h: &Harness<'_, Fixture>, word: &str) -> Option<egui::Color32> {
    let bar = region_rect(h, "Instrument bar");
    let at = painted(h).into_iter().find(|p| p.text == word && bar.contains(p.rect.center()))?;
    filled(h)
        .into_iter()
        .filter(|(r, fill)| *fill != egui::Color32::TRANSPARENT && r.contains(at.rect.center()))
        .min_by(|a, b| a.0.area().total_cmp(&b.0.area()))
        .map(|(_, fill)| fill)
}

/// A window the render check drew: what it is, its size, where its ticket sits, whether its ladder
/// is shown.
type Window = (&'static str, egui::Vec2, TradeWindowTicketPlace, bool);

/// The three windows the second render check drew: the default one (the ladder beside the full
/// ticket), the bottom-panel one (the compact ticket under the ladder) and the ticket alone.
const WINDOWS: [Window; 3] = [
    ("the 600 pt window", trade::BESIDE_SIZE, TradeWindowTicketPlace::Beside, true),
    ("the 320 pt window", trade::UNDER_SIZE, TradeWindowTicketPlace::Under, true),
    ("the 280 pt ticket alone", layout::TICKET_ONLY_SIZE, TradeWindowTicketPlace::Beside, false),
];

/// `scene` drawn in `window`, settled, with nothing emitted yet.
fn in_window(scene: Scene, (_, size, panel, ladder): Window) -> Harness<'static, Fixture> {
    let mut h = harness_at(scene, size, panel);
    h.state_mut().state.view.ladder = ladder;
    h.run();
    h.state_mut().emitted.clear();
    h
}

/// Move a drawn window to `window`, as a trader resizing it and choosing its view would.
fn move_to(h: &mut Harness<'static, Fixture>, (_, size, panel, ladder): Window) {
    h.state_mut().state.view = trade_window::TradeWindowPanes { chart: false, ladder, panel };
    h.set_size(size);
    h.run();
}

/// Every look the fit sweep draws: each density at each text size.
fn looks() -> Vec<ThemeAppearance> {
    use vike_ui_theme::metrics::ThemeDensity;
    use vike_ui_theme::type_scale::ThemeTextSize;
    ThemeDensity::ALL
        .into_iter()
        .flat_map(|density| {
            ThemeTextSize::ALL.map(|text_size| ThemeAppearance {
                density,
                text_size,
                ..ThemeAppearance::default()
            })
        })
        .collect()
}

/// The owner's ruling B (2026-10-03): the LIVE chip is filled in the theme's ACCENT, the design
/// system's "LIVE is an accent shape" (spec §2), and not in the danger red (`ThemeStatus::Error`) the
/// build had given it (FW6, I1). No other mode's chip is filled in the accent: the CONTROLS, DEMO,
/// PAPER and an unknown mode, in whose bars nothing at all is filled in it.
#[test]
fn a_live_window_fills_its_mode_chip_in_the_accent_and_no_other_does() {
    use vike_ui_theme::components::{ThemeStatus, ThemeTokens};
    let danger = ThemeStatus::Error.color();
    for (mode, word) in [
        (Some(AccountTier::Live), "LIVE"),
        (Some(AccountTier::Demo), "DEMO"),
        (Some(AccountTier::Paper), "PAPER"),
        (None, "MODE ?"),
    ] {
        let h = settled(Scene { mode, ..demo() });
        let accent = ThemeTokens::of(&h.ctx).theme.accent;
        assert_ne!(danger, accent, "CONTROL: the accent is not the danger red");
        let bar = region_rect(&h, "Instrument bar");
        let chip = painted(&h)
            .into_iter()
            .find(|p| p.text == word && bar.contains(p.rect.center()))
            .unwrap_or_else(|| panic!("{word} is painted in the bar"));
        let under: Vec<egui::Color32> = filled(&h)
            .into_iter()
            .filter(|(r, _)| r.contains(chip.rect.center()))
            .map(|(_, fill)| fill)
            .collect();
        if mode == Some(AccountTier::Live) {
            assert!(under.contains(&accent), "LIVE sits on the accent fill: {under:?}");
            assert!(!under.contains(&danger), "and not on the danger red: {under:?}");
        } else {
            let lit = filled(&h).into_iter().any(|(r, fill)| fill == accent && bar.intersects(r));
            assert!(!lit, "{word}: CONTROL: nothing in the bar is filled in the accent");
        }
    }
}

/// FW6, I1: an order held for a confirm on a LIVE account no longer looks like one on a DEMO
/// account. Its prompt's region is WASHED in the LIVE chip's own wash (`chip::tier_wash`: a fill,
/// which takes no width from the words) and it names the account LIVE in the colour the bar's
/// LIVE chip is PAINTED in, read off the chip as drawn — one source, so a change to the chip's
/// fill moves the prompt's marks with it. That the chip's fill IS the accent (the owner's ruling
/// B, 2026-10-03) is the chip tests' to hold:
/// `a_live_window_fills_its_mode_chip_in_the_accent_and_no_other_does` here, and the kit's
/// `live_is_the_accent_demo_the_info_paper_the_border`. An account whose mode is not known is
/// treated as LIVE wherever safety is at stake: its prompt is washed too, and never CALLED live.
/// CONTROL: a DEMO prompt has neither.
///
/// The word MOVES NOTHING (the look at 320 pt caught a first version giving it room by turning the
/// answers into ✓ and ✕): in all three windows at every look the fit sweep draws, a LIVE prompt and
/// an unknown mode's are drawn beside the same answers as the DEMO one — the words Place and
/// Cancel, or ✓ and ✕ — in a strip of the same height. The word heads the first line where that
/// line still fits beside those answers, else the second; at the owner's looks (Normal and
/// Comfortable density, Standard text) it is there in every window, for the render check's prompt
/// (a five-digit price, reduce only), whose first line leaves no room for it at 280 pt.
#[test]
fn a_live_prompt_is_washed_in_the_accent_says_live_and_moves_nothing() {
    use vike_ui_theme::components::{ThemeTokens, chip};
    use vike_ui_theme::metrics::ThemeDensity;
    use vike_ui_theme::type_scale::ThemeTextSize;
    for look in looks() {
        for window in WINDOWS {
            let mut demo_form = None;
            // DEMO first: the control the other two are measured against.
            for mode in [Some(AccountTier::Demo), Some(AccountTier::Live), None] {
                let what =
                    format!("{mode:?} in {}, {:?}/{:?}", window.0, look.density, look.text_size);
                let mut h = in_window(Scene { mode, look, ..demo() }, window);
                set(&mut h, |s| {
                    s.one_click = false;
                    s.reduce_only = true;
                    s.price = "84606.9".to_string();
                });
                let buy = label_of(&h, "Buy 0.010 ", "");
                click(&mut h, &buy);
                assert!(h.state().state.held.is_some(), "{what}: CONTROL: held for a confirm");
                let wash = chip::tier_wash(AccountTier::Live, &ThemeTokens::of(&h.ctx));
                let strip = strip_rect(&h);
                let words = in_strip(&h);
                let first = words
                    .iter()
                    .find(|p| p.text.contains("Buy 0.010"))
                    .unwrap_or_else(|| panic!("{what}: the prompt is painted: {words:?}"));
                let washed = filled(&h).into_iter().any(|(r, fill)| {
                    fill == wash
                        && strip.expand(0.6).contains_rect(r)
                        && r.contains(first.rect.center())
                });
                let live: Vec<&(String, egui::Color32)> = words
                    .iter()
                    .flat_map(|p| &p.sections)
                    .filter(|(s, _)| s.contains("LIVE"))
                    .collect();
                let form = (h.state().strip_h, words.iter().any(|p| p.text == "Place"));
                match mode {
                    Some(AccountTier::Demo) => {
                        assert!(!washed, "{what}: CONTROL: a DEMO prompt is not washed");
                        assert!(live.is_empty(), "{what}: CONTROL: nor called LIVE: {words:?}");
                        demo_form = Some(form);
                        continue;
                    }
                    Some(AccountTier::Live) => {
                        assert!(washed, "{what}: the prompt is washed in the LIVE chip's wash");
                        let owners = look.text_size == ThemeTextSize::Small
                            && matches!(
                                look.density,
                                ThemeDensity::Normal | ThemeDensity::Comfortable
                            );
                        if owners {
                            assert!(!live.is_empty(), "{what}: the word fits here: {words:?}");
                        }
                        if !live.is_empty() {
                            // It heads a line of the prompt (painted top down, before the
                            // answers): the first, else the one after it.
                            let at = words.iter().position(|p| p.text.starts_with("LIVE"));
                            assert_matches!(
                                at,
                                Some(0 | 1),
                                "{what}: LIVE heads the first or second line: {words:?}"
                            );
                            let chip = chip_fill(&h, "LIVE");
                            assert!(
                                live.iter().all(|(_, c)| Some(*c) == chip),
                                "{what}: LIVE is said in the chip's own fill {chip:?}: {live:?}"
                            );
                        }
                    }
                    _ => {
                        assert!(washed, "{what}: treated as LIVE, the prompt is washed");
                        assert!(live.is_empty(), "{what}: and never called LIVE: {words:?}");
                    }
                }
                assert_eq!(
                    Some(form),
                    demo_form,
                    "{what}: (strip height, words for answers) as on DEMO: {words:?}"
                );
            }
        }
    }
}

/// FW6, I2: a held Stop whose trigger is on the wrong side of the market says "it triggers at once"
/// in the WARNING colour in both of the prompt's forms — on a line of its own where the prompt's
/// lines fit beside the answers, and among the wrapped words where they do not (at 320 and 280 pt
/// they wrap, and every wrapped word was painted in the text colour). In the compact ticket, which
/// has no trigger field, the strip is the only place the warning shows. Read off the painted
/// sections, in the three windows at every look the fit sweep draws: the words painted in the
/// warning colour are the warning, word for word, and nothing else; and BOTH forms were drawn, so
/// the test cannot pass on one alone.
#[test]
fn a_held_wrong_side_stop_keeps_its_warning_colour_in_both_forms() {
    let warning = "Stop BUY 98.0 is below the market 100.0: it triggers at once.";
    let amber = vike_ui_theme::components::ThemeStatus::Warning.color();
    let stop = TradeWindowAction::Place {
        side: 1,
        order_type: TradeWindowTicketKind::Stop,
        price: Some(98.0),
        quantity: 0.01,
        reduce_only: false,
        post_only: false,
        exits: None,
        origin: TradeWindowOrigin::Ticket,
    };
    let (mut whole, mut wrapped) = (Vec::new(), Vec::new());
    for look in looks() {
        for window in WINDOWS {
            let what = format!("{}, {:?}/{:?}", window.0, look.density, look.text_size);
            let mut h = in_window(Scene { look, ..live() }, window);
            h.state_mut().state.held = Some(stop.clone());
            h.run();
            assert!(h.state().state.held.is_some(), "{what}: CONTROL: it waits for a confirm");
            let words = in_strip(&h);
            let warned: Vec<&str> = words
                .iter()
                .flat_map(|p| &p.sections)
                .filter(|(_, c)| *c == amber)
                .map(|(s, _)| s.as_str())
                .collect();
            let said = warned.join(" ").split_whitespace().collect::<Vec<_>>().join(" ");
            assert_eq!(said, warning, "{what}: the words in the warning colour: {words:?}");
            if words.iter().any(|p| p.text == warning) {
                whole.push(what);
            } else {
                wrapped.push(what);
            }
        }
    }
    assert!(
        !whole.is_empty() && !wrapped.is_empty(),
        "both forms are drawn: whole in {whole:?}, wrapped in {wrapped:?}"
    );
}

/// The glue's words for TP/SL on an engine whose lane cannot hold the stop-loss (test words: this
/// crate cannot see the glue's `TAKE_PROFIT_STOP_LOSS_LANE_WHY`).
const LANE_WHY: &str = "TP/SL is not available on this account yet: its engine trades the spot \
                        market, where the exchange cannot hold the stop-loss.";

/// FW6, I3 and I4: with no order waiting and no line from the app, the strip says WHY the ticket's
/// Buy and Sell send nothing — the very sentence the form says it in — and "Ready" only when they
/// can send: a ticked TP/SL the account cannot carry, a TP/SL with a leg left blank, an instrument
/// with no lot size, and an account that trades other symbols (the shortened list included, I4).
/// A Stop entry refuses a ticked TP/SL in the FULL ticket only: the compact one sends at market,
/// which carries one, so under the ladder the strip is Ready (the control on which ticket it reads).
/// In the three windows, at every look the fit sweep draws, and never cut short: where two lines
/// do not hold the sentence it wraps on, and the strip grows to hold it.
#[test]
fn the_strip_says_why_the_ticket_sends_nothing_and_ready_only_when_it_can() {
    let five: Vec<String> =
        ["ETH-PERPETUAL", "SOL-PERPETUAL", "XRP-PERPETUAL", "DOGE-PERPETUAL", "ADA-PERPETUAL"]
            .map(String::from)
            .to_vec();
    let all = |why: String| [Some(why.clone()), Some(why.clone()), Some(why)];
    let ticked: Tweak = |s| s.take_profit_stop_loss = true;
    let cases: Vec<(&str, Scene, Tweak, [Option<String>; 3])> = vec![
        (
            "TP/SL ticked where the account's lane cannot carry it",
            Scene { bracket_why: Some(LANE_WHY), ..demo() },
            ticked,
            all(ticket::take_profit_stop_loss_refusal(LANE_WHY)),
        ),
        (
            "TP/SL ticked with its stop-loss left blank",
            demo(),
            |s| {
                s.take_profit_stop_loss = true;
                s.stop_loss_text.clear();
            },
            all(ticket::TAKE_PROFIT_STOP_LOSS_LEGS_WHY.to_string()),
        ),
        (
            "no lot size",
            Scene { grid: NO_LOT, ..demo() },
            |_| {},
            all(ticket::no_lot_reason("BTCUSDT")),
        ),
        (
            "an account that trades another symbol",
            Scene { tradable: false, ..demo() },
            |_| {},
            all("This account does not trade BTCUSDT. It trades ETHUSDT.".to_string()),
        ),
        (
            "an account that trades five other symbols",
            Scene { tradable: false, trades: five, ..demo() },
            |_| {},
            all("This account does not trade BTCUSDT. It trades ETH-PERPETUAL, SOL-PERPETUAL and \
                 3 more."
                .to_string()),
        ),
        (
            "TP/SL ticked on a Stop entry",
            demo(),
            |s| {
                s.take_profit_stop_loss = true;
                s.order_type = TradeWindowTicketKind::Stop;
            },
            {
                let stop = Some(ticket::take_profit_stop_loss_refusal(
                    ticket::TAKE_PROFIT_STOP_LOSS_STOP_WHY,
                ));
                [stop.clone(), None, stop]
            },
        ),
        ("CONTROL: nothing refused", demo(), |_| {}, [None, None, None]),
    ];
    let mut found = Vec::new();
    for look in looks() {
        for (what, scene, tweak, want) in &cases {
            let mut h = harness_at(
                Scene { look, ..scene.clone() },
                trade::BESIDE_SIZE,
                TradeWindowTicketPlace::Beside,
            );
            h.run();
            set(&mut h, *tweak);
            for (window, want) in WINDOWS.into_iter().zip(want) {
                move_to(&mut h, window);
                let at = format!("{what}, {}, {:?}/{:?}", window.0, look.density, look.text_size);
                let strip = labels_in(&h, "Status strip");
                let cut: Vec<String> =
                    in_strip(&h).into_iter().filter(|p| p.elided).map(|p| p.text).collect();
                if !cut.is_empty() {
                    found.push(format!("{at}: cut short: {cut:?}"));
                }
                let ready = strip.iter().any(|l| l.starts_with("Ready"));
                match want {
                    Some(why) if !strip.contains(why) || ready => {
                        found.push(format!("{at}: the strip should say {why:?}: {strip:?}"));
                    }
                    None if !ready => found.push(format!("{at}: CONTROL: Ready: {strip:?}")),
                    _ => {}
                }
            }
        }
    }
    assert!(found.is_empty(), "{} finding(s):\n{}", found.len(), found.join("\n"));
}
