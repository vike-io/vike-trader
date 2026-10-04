//! The instrument bar (spec §3.3): the symbol and venue · account pickers, the mode chip, the last
//! price and spread, and the FEED badge that names the window's own depth link (Ruling R1 — the
//! DOM's "depth" strip, moved).
//!
//! The bar takes one, two or three rows, by width ([`rows`]): everything on one; the pickers and
//! the chip, then the price group, on two; and on three, the symbol picker and the chip, the account
//! picker, then the price group — two pickers and a chip are wider than a 320 pt window. [`height`]
//! is the band the window gives the bar, so whatever it holds, the ladder starts below it.
//!
//! Two honesty rules live here:
//! - an account whose mode the backend did not report is drawn `MODE ?`, never as PAPER — in the
//!   bar AND in the account picker, the list a trader picks where an order goes from (spec §4.3);
//! - the FEED badge reads the depth link through [`link_of`], so a link nobody reported on says
//!   `FEED ?` rather than `FEED DOWN`.

use std::collections::HashMap;
use std::hash::Hash;

use egui::RichText;
use vike_model::feed_status::{ConnectionState, parse_feed_status};
use vike_ui_theme::components::button::ActionButton;
use vike_ui_theme::components::{Status, Tokens, chip, input};
use vike_ui_theme::fmt::fmt_thousands_prec;
use vike_ui_theme::icons;
use vike_ui_theme::metrics::Density;
use vike_ui_theme::type_scale::{TextRole, TextSize};

use super::{AccountMode, TradeAction, TradeInputs, TradeState, keyed, ladder, section};

// The two thresholds are the bar's WIDEST content, MEASURED 2026-10-03 at Normal density and
// Standard text by `tests::print_bar_widths` over the Trade window's fit scene (a swap's
// thirteen-character symbol, a 27-character venue · account label, a price of 65,432.1) in its
// widest state: an unknown mode's `MODE ?` chip and a dialling link's `FEED DIALLING`.
// `tests::the_bar_fits_every_width_in_every_look` sweeps every look and width, so a control that
// grows fails there instead of overlapping here.

/// Below this width the bar takes two rows: the pickers and the chip, then the price group. The
/// widest one-row bar measured 602.1 pt (551.8 with a LIVE chip and `FEED UP`), plus 8.4 pt — one
/// Title-size mono digit — for the thousands comma the ladder's price format prints and the
/// measurement's price did not. Re-measured 2026-10-03 with that format printing it: 610.1 pt
/// (559.8 with a LIVE chip and `FEED UP`), inside the budget. [`rows`] grows it for a looser or
/// larger look.
pub const TWO_ROWS_BELOW: f32 = 612.0;

/// Below this width the bar takes three rows: the symbol picker and the chip, the account picker,
/// the price group. The widest first row of two — the two pickers and the chip — measured 384.7 pt.
pub const THREE_ROWS_BELOW: f32 = 388.0;

/// The symbol picker's width: its search field and its rows take all of it.
const SYMBOL_PICKER_W: f32 = 280.0;
/// The account picker's narrowest width.
const ACCOUNT_PICKER_W: f32 = 240.0;
/// How tall the symbol picker's list grows before it scrolls.
const MATCHES_H: f32 = 260.0;

/// The mode chip's word for an account whose mode the backend did not report.
const MODE_UNKNOWN: &str = "MODE ?";
/// ...and what hovering it says, in the bar and in the account picker alike.
const MODE_UNKNOWN_WHY: &str = "The backend does not say whether this account is paper, demo or \
                                live: it predates that report, or names a mode this app does not \
                                know. This window treats the account as live.";

/// How much wider than Normal density at Standard text a look's bar is, at most: the largest of
/// the ratios its control height, its padding and its text grow by (never below 1, so a denser
/// look keeps the thresholds and gives a row up a little early, never late). The DOM's strips were
/// scaled the same way.
fn scale(t: &Tokens) -> f32 {
    let base = Density::Normal.metrics();
    let m = &t.metrics;
    let text = t.text.px(TextRole::Caption) / TextSize::Standard.px(TextRole::Caption);
    (m.control_h / base.control_h).max(m.pad / base.pad).max(text).max(1.0)
}

/// How many rows the bar takes in a band `width` wide under `t`: one, two below
/// [`TWO_ROWS_BELOW`], three below [`THREE_ROWS_BELOW`], each threshold grown by the largest of
/// the ratios the look's control height, padding and text exceed Normal density at Standard text
/// by.
pub fn rows(width: f32, t: &Tokens) -> usize {
    let k = scale(t);
    if width >= TWO_ROWS_BELOW * k {
        1
    } else if width >= THREE_ROWS_BELOW * k {
        2
    } else {
        3
    }
}

/// The bar's height at `width`: its rows, a gap between each two, and a gap above and below.
pub fn height(width: f32, t: &Tokens) -> f32 {
    let n = rows(width, t) as f32;
    n * t.metrics.control_h + (n - 1.0) * t.metrics.gap + 2.0 * t.metrics.gap
}

/// The state of the window's own depth link, read off [`TradeInputs::source`]. An EMPTY source is
/// a link nobody reported on, so it is `Unknown`: `parse_feed_status` alone calls it
/// `Disconnected`, and the badge would say `FEED DOWN` about a link it knows nothing of. Whatever
/// names the window's link reads it here, so no two places can disagree about it.
pub fn link_of(source: &str) -> ConnectionState {
    if source.trim().is_empty() { ConnectionState::Unknown } else { parse_feed_status(source) }
}

/// The FEED badge's word for the window's own depth link. Deliberately not the LIVE/PAPER words the
/// mode chip uses.
pub fn feed_word(state: ConnectionState) -> &'static str {
    use ConnectionState as C;
    match state {
        C::Connected => "FEED UP",
        C::Connecting => "FEED DIALLING",
        C::Disconnected => "FEED DOWN",
        C::Error => "FEED FAULT",
        C::Unknown => "FEED ?",
    }
}

/// The badge's colour: fixed, the same in every theme. A dead link is the ERROR red, where the status
/// bar's wordless dots use a muted grey (owner decision 4, 2026-09-29).
pub fn feed_status(state: ConnectionState) -> Status {
    use ConnectionState as C;
    match state {
        C::Connected => Status::Ok,
        C::Connecting => Status::Warning,
        C::Disconnected | C::Error => Status::Error,
        C::Unknown => Status::Muted,
    }
}

/// Draw the bar into `ui`, the band [`height`] sized for this width. Public so it is live code
/// before the window's `draw` calls it.
pub fn bar(
    ui: &mut egui::Ui,
    t: &Tokens,
    state: &mut TradeState,
    inputs: &TradeInputs<'_>,
    actions: &mut Vec<TradeAction>,
) {
    let n = rows(ui.available_width(), t);
    // Rows a gap apart, the gap [`height`] counts: no other spacing between them.
    ui.spacing_mut().item_spacing.y = t.metrics.gap;
    ui.add_space(t.metrics.gap);
    // Each control under an id of its own (`section`), never one it takes from what the bar drew
    // before it: the account picker sits on the first row or the second by the window's width, the
    // mode is a chip or a muted badge, and the spread comes and goes with the book and the room.
    let key = ui.id();
    let symbol = |ui: &mut egui::Ui, state: &mut TradeState, actions: &mut Vec<TradeAction>| {
        section(ui, key, "bar_symbol", |ui| symbol_picker(ui, t, state, inputs, actions));
    };
    let account = |ui: &mut egui::Ui, actions: &mut Vec<TradeAction>| {
        section(ui, key, "bar_account", |ui| account_picker(ui, t, inputs, actions));
    };
    let mode = |ui: &mut egui::Ui| {
        section(ui, key, "bar_mode", |ui| {
            mode_mark(ui, inputs.mode);
        });
    };
    let prices = |ui: &mut egui::Ui| {
        section(ui, key, "bar_prices", |ui| prices_on_the_right(ui, t, inputs));
    };
    match n {
        1 => line(ui, t, |ui| {
            symbol(ui, state, actions);
            account(ui, actions);
            mode(ui);
            prices(ui);
        }),
        2 => {
            line(ui, t, |ui| {
                symbol(ui, state, actions);
                account(ui, actions);
                mode(ui);
            });
            line(ui, t, prices);
        }
        _ => {
            line(ui, t, |ui| {
                symbol(ui, state, actions);
                mode(ui);
            });
            line(ui, t, |ui| account(ui, actions));
            line(ui, t, prices);
        }
    }
}

/// The salt for each item of a list, in order: what the item IS (`id`), and how many items before
/// it are the same, so two rows that look alike never share an id. A row keyed this way keeps its
/// id, and a press held on it keeps its meaning, when rows appear, go or re-rank around it (the F
/// wave's id audit: a picker's list changes under a held press when the node's reply arrives).
/// One pass, each item's `id` read once: a picker works its list's salts out once per frame, not a
/// scan of the rows before every row.
fn row_salts<T, K: Copy + Eq + Hash>(items: &[T], id: impl Fn(&T) -> K) -> Vec<(K, usize)> {
    let mut seen: HashMap<K, usize> = HashMap::with_capacity(items.len());
    items
        .iter()
        .map(|item| {
            let me = id(item);
            let before = seen.entry(me).or_insert(0);
            *before += 1;
            (me, *before - 1)
        })
        .collect()
}

/// One row of the bar, its controls a gap apart.
fn line(ui: &mut egui::Ui, t: &Tokens, add: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = t.metrics.gap;
        add(ui);
    });
}

/// The kit's chip for the account's mode, or `MODE ?` in the muted outline when the backend did not
/// report one. The bar's chip and the account picker's rows both draw it here, so neither can show
/// an unreported mode as PAPER.
fn mode_mark(ui: &mut egui::Ui, mode: AccountMode) -> egui::Response {
    match kit_mode(mode) {
        Some(m) => chip::mode(ui, m),
        None => chip::badge(ui, MODE_UNKNOWN, Status::Muted).on_hover_text(MODE_UNKNOWN_WHY),
    }
}

/// The kit's chip for a REPORTED mode; `None` for one nobody reported.
fn kit_mode(mode: AccountMode) -> Option<chip::Mode> {
    match mode {
        AccountMode::Paper => Some(chip::Mode::Paper),
        AccountMode::Demo => Some(chip::Mode::Demo),
        AccountMode::Live => Some(chip::Mode::Live),
        AccountMode::Unknown => None,
    }
}

/// The price group, against the row's right edge.
fn prices_on_the_right(ui: &mut egui::Ui, t: &Tokens, inputs: &TradeInputs<'_>) {
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        price_group(ui, t, inputs)
    });
}

/// Right to left: the FEED badge, the spread, the price (the book's mid). The spread is the one
/// that gives way where the three do not fit the row (MEASURED by the bar's sweep: 2 pt short in
/// a 260 pt window at Comfortable density and Large text, a dialling link's `FEED DIALLING`): the
/// badge names the window's link and the price is the price, while the spread is on the ladder too.
fn price_group(ui: &mut egui::Ui, t: &Tokens, inputs: &TradeInputs<'_>) {
    let link = link_of(inputs.source);
    let source = inputs.source.trim();
    let detail = if source.is_empty() { "source unknown" } else { source };
    let (txt, col) = match inputs.last {
        Some(p) => (ladder::fmt_px(p, inputs.grid.tick), t.theme.text),
        None => ("—".to_string(), t.theme.text3),
    };
    let spread = match (inputs.book.best_bid(), inputs.book.best_ask()) {
        (Some(b), Some(a)) => {
            Some(format!("Spread {}", ladder::fmt_px(a.price - b.price, inputs.grid.tick)))
        }
        _ => None,
    };
    chip::badge(ui, feed_word(link), feed_status(link)).on_hover_text(format!(
        "This window's depth arrives on the DATAHUB market-data link, not the backend connection \
         the status bar reports: {detail}"
    ));
    // Measured once the badge holds its place: what is left must take the spread, a gap, the price.
    let text_w = |text: &str, font: egui::FontId| {
        ui.painter().layout_no_wrap(text.to_string(), font, t.theme.text).size().x
    };
    let price_w = text_w(&txt, t.mono(TextRole::Title));
    let spread = spread.filter(|s| {
        text_w(s, t.font(TextRole::Caption)) + t.metrics.gap + price_w <= ui.available_width()
    });
    if let Some(s) = spread {
        ui.label(RichText::new(s).font(t.font(TextRole::Caption)).color(t.theme.text3));
    }
    // The window's price is the book's MID: no last trade price reaches the desktop (a snapshot
    // from the node carries no marks; the FW2 review), so the hover says what it is.
    ui.label(RichText::new(txt).font(t.mono(TextRole::Title)).color(col)).on_hover_text(
        if inputs.last.is_some() {
            "Mid price: halfway between this window's best bid and best ask."
        } else {
            "No price: this window's book has no bid and ask to take the mid of."
        },
    );
}

/// A match's price. No tick comes with it, so it prints six significant digits, grouped, with
/// trailing zeros dropped: `65,432.1`, `0.5231`, `0.0000123`. Not a number prints a dash.
fn mark_text(p: f64) -> String {
    if !p.is_finite() {
        return "—".to_string();
    }
    let a = p.abs();
    let magnitude = if a > 0.0 { a.log10().floor() as i32 } else { 0 };
    let decimals = (5 - magnitude).clamp(0, 12) as usize;
    let s = fmt_thousands_prec(p, decimals);
    if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s }
}

/// The venue's own spelling of `venue`, from whichever row carries one (each is the settings
/// database's `venue.title`): the window's, an account's, a match's — else the key itself. The
/// widget never spells a venue of its own.
fn venue_label_of<'a>(inputs: &TradeInputs<'a>, venue: &'a str) -> &'a str {
    if venue == inputs.venue {
        return inputs.venue_label;
    }
    inputs
        .accounts
        .iter()
        .find(|a| a.venue == venue)
        .map(|a| a.venue_label)
        .or_else(|| inputs.matches.iter().find(|m| m.venue == venue).map(|m| m.venue_label))
        .unwrap_or(venue)
}

/// Move the window to `symbol` on `venue`, and leave the picker clean for next time.
fn pick_symbol(state: &mut TradeState, actions: &mut Vec<TradeAction>, venue: &str, symbol: &str) {
    actions.push(TradeAction::PickSymbol { venue: venue.to_string(), symbol: symbol.to_string() });
    state.query.clear();
    state.hot = 0;
}

fn symbol_picker(
    ui: &mut egui::Ui,
    t: &Tokens,
    state: &mut TradeState,
    inputs: &TradeInputs<'_>,
    actions: &mut Vec<TradeAction>,
) {
    let button = ui.add(ActionButton::secondary((inputs.symbol, icons::DISCLOSE_OPEN)));
    egui::Popup::menu(&button)
        .id(ui.id().with("trade_symbol_picker"))
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            // The search field, each recent pick and each match under an id of its own (`section`,
            // `keyed`): the Recent row and the matches come and go while the trader types, and a
            // field or a row that took its id from them would lose the focus or a held press.
            let key = ui.id();
            ui.set_min_width(SYMBOL_PICKER_W);
            // The search field takes the picker's width, not egui's 280 pt default.
            ui.spacing_mut().text_edit_width = SYMBOL_PICKER_W;
            let entry = section(ui, key, "search", |ui| {
                input::text(
                    ui,
                    &mut state.query,
                    input::Field { hint: "Search a symbol or a coin", ..Default::default() },
                )
            });
            if ui.memory(|m| m.focused().is_none()) {
                entry.request_focus();
            }
            if entry.changed() {
                state.hot = 0;
            }
            if !inputs.recent.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new("Recent")
                            .font(t.font(TextRole::Caption))
                            .color(t.theme.text3),
                    );
                    // A symbol is an address only with its venue: a recent pick moves the window
                    // to its venue too.
                    let salts = row_salts(inputs.recent, |r| *r);
                    for (&(venue, symbol), salt) in inputs.recent.iter().zip(salts) {
                        let words = format!("{symbol} · {}", venue_label_of(inputs, venue));
                        let salt = ("recent", salt);
                        if keyed(ui, key.with(salt), ActionButton::secondary(words)).clicked() {
                            pick_symbol(state, actions, venue, symbol);
                            ui.close();
                        }
                    }
                });
            }
            let n = inputs.matches.len();
            let (down, up, enter) = ui.input(|i| {
                (
                    i.key_pressed(egui::Key::ArrowDown),
                    i.key_pressed(egui::Key::ArrowUp),
                    i.key_pressed(egui::Key::Enter),
                )
            });
            if n > 0 {
                if down {
                    state.hot = (state.hot + 1) % n;
                }
                if up {
                    state.hot = (state.hot + n - 1) % n;
                }
                state.hot = state.hot.min(n - 1);
            }
            let mut pick = None;
            let list = egui::ScrollArea::vertical().id_salt("trade_symbol_matches");
            list.max_height(MATCHES_H).show(ui, |ui| {
                if n == 0 {
                    ui.label(
                        RichText::new(format!(
                            "No symbol matches \u{201C}{}\u{201D} on the venues you trade.",
                            state.query.trim()
                        ))
                        .font(t.font(TextRole::Body))
                        .color(t.theme.text2),
                    );
                }
                let salts = row_salts(inputs.matches, |m| (m.venue, m.symbol));
                for ((i, m), salt) in inputs.matches.iter().enumerate().zip(salts) {
                    let price = m.last.map_or_else(|| "—".to_string(), mark_text);
                    let mut words = format!("{}  {}  {price}  {}", m.symbol, m.name, m.venue_label);
                    if !m.tradable {
                        words.push_str("  · view only");
                    }
                    let row = if i == state.hot {
                        ActionButton::primary(words)
                    } else {
                        ActionButton::secondary(words)
                    };
                    if keyed(ui, key.with(("match", salt)), row).clicked() {
                        pick = Some(i);
                    }
                }
            });
            if enter && n > 0 {
                pick = Some(state.hot);
            }
            if let Some(m) = pick.map(|i| inputs.matches[i]) {
                pick_symbol(state, actions, m.venue, m.symbol);
                ui.close();
            }
            ui.label(
                RichText::new("↑ ↓ to move · Enter to open · Esc to close")
                    .font(t.font(TextRole::Caption))
                    .color(t.theme.text3),
            );
        });
}

fn account_picker(
    ui: &mut egui::Ui,
    t: &Tokens,
    inputs: &TradeInputs<'_>,
    actions: &mut Vec<TradeAction>,
) {
    // The window's own row names the account as the list does; without one, its label or "main".
    let name = inputs
        .accounts
        .iter()
        .find(|a| a.venue == inputs.venue && a.account == inputs.account)
        .map_or(inputs.account.unwrap_or("main"), |a| a.name);
    let words = format!("{} · {name}", inputs.venue_label);
    let button = ui.add(ActionButton::secondary((words.as_str(), icons::DISCLOSE_OPEN)));
    egui::Popup::menu(&button).id(ui.id().with("trade_account_picker")).show(|ui| {
        // Each row under an id of its own (`section`, `keyed`): the list changes under a held press
        // — the node's directory reply arrives, an account starts running — and a row that took
        // its id from its place in the list was released as a pick of the row that took that place.
        let key = ui.id();
        ui.set_min_width(ACCOUNT_PICKER_W);
        ui.label(
            RichText::new(format!("{} is listed on", inputs.symbol))
                .font(t.font(TextRole::Caption))
                .color(t.theme.text3),
        );
        // Why the list is not the server's whole list, as the app states it (owner, 10-03, item 5).
        if let Some(why) = inputs.accounts_why {
            ui.label(RichText::new(why).font(t.font(TextRole::Caption)).color(t.theme.text2));
        }
        let salts = row_salts(inputs.accounts, |r| (r.venue, r.account, r.name));
        for (a, salt) in inputs.accounts.iter().zip(salts) {
            let chosen = a.venue == inputs.venue && a.account == inputs.account;
            let label = format!("{} · {}", a.venue_label, a.name);
            // The row opens its OWN venue's spelling of the instrument (`AccountRow::symbol`).
            let pick = || TradeAction::PickAccount {
                venue: a.venue.to_string(),
                account: a.account.map(str::to_string),
                symbol: a.symbol.to_string(),
            };
            section(ui, key, ("account", salt), |ui| match (a.why_not, kit_mode(a.mode)) {
                (None, Some(mode)) => {
                    if chip::account(ui, &label, mode, chosen).clicked() && !chosen {
                        actions.push(pick());
                        ui.close();
                    }
                }
                // A mode nobody reported: the `MODE ?` mark beside the account, never the PAPER
                // chip `chip::account` would need. The chosen one is a label, as the kit's is.
                (None, None) => {
                    ui.horizontal(|ui| {
                        mode_mark(ui, a.mode);
                        if chosen {
                            ui.label(
                                RichText::new(label.as_str())
                                    .font(t.font(TextRole::Body))
                                    .color(t.theme.text),
                            );
                        } else if ui.add(ActionButton::secondary(label.as_str())).clicked() {
                            actions.push(pick());
                            ui.close();
                        }
                    });
                }
                // An account the database holds but the server does not run: greyed, with the
                // reason on hover, and no click (spec §3.3).
                (Some(why), _) => {
                    ui.horizontal(|ui| {
                        mode_mark(ui, a.mode);
                        ui.label(
                            RichText::new(label.as_str())
                                .font(t.font(TextRole::Body))
                                .color(t.theme.text3),
                        )
                        .on_hover_text(why);
                    });
                }
            });
        }
        let salts = row_salts(inputs.unconnected, |u| u.venue_label);
        for (u, salt) in inputs.unconnected.iter().zip(salts) {
            section(ui, key, ("connect", salt), |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("{} · No account connected", u.venue_label))
                            .font(t.font(TextRole::Body))
                            .color(t.theme.text3),
                    );
                    // Every row's button reads "Connect", so its NAME carries the venue: without
                    // it a screen reader hears the same button once per venue.
                    let name = format!("Connect {}", u.venue_label);
                    if icons::named(ui.add(ActionButton::secondary("Connect")), &name).clicked() {
                        actions.push(TradeAction::OpenConnections);
                        ui.close();
                    }
                });
            });
        }
        let manage = ActionButton::secondary("Manage accounts in Connections");
        if keyed(ui, key.with("manage"), manage).clicked() {
            actions.push(TradeAction::OpenConnections);
            ui.close();
        }
    });
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use egui::accesskit::Role;
    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT, Queryable};
    use vike_model::feed_status::ConnectionState as C;
    use vike_model::{BookLevel, L2Book, VenueCaps};
    use vike_ui_theme::appearance::Appearance;

    use super::*;
    use crate::trade::{AccountRow, Grid, SymbolMatch, Tradable, Unconnected};

    /// The FEED badge never reuses the trading-mode words: the mode chip says where an ORDER goes,
    /// the badge where DEPTH comes from, and the two fail independently (moved from the DOM's
    /// source strip, Ruling R1).
    #[test]
    fn the_feed_words_are_not_the_mode_words() {
        for s in [C::Connected, C::Connecting, C::Disconnected, C::Error, C::Unknown] {
            let w = feed_word(s);
            assert!(w.starts_with("FEED "), "{w}");
            for mode in ["LIVE", "PAPER", "DEMO"] {
                assert!(!w.contains(mode), "{w}");
            }
        }
    }

    /// A dead depth link is the alarm: red, like the DOM's strip (owner decision 4, 2026-09-29).
    #[test]
    fn a_dead_link_is_red_and_an_unknown_one_is_muted() {
        assert_eq!(feed_status(C::Disconnected), Status::Error);
        assert_eq!(feed_status(C::Error), Status::Error);
        assert_eq!(feed_status(C::Unknown), Status::Muted);
        assert_eq!(feed_status(C::Connected), Status::Ok);
    }

    #[test]
    fn the_bar_takes_two_rows_below_its_threshold() {
        let t = Tokens::of(&egui::Context::default());
        assert!(height(TWO_ROWS_BELOW - 1.0, &t) > height(TWO_ROWS_BELOW, &t));
    }

    /// Two pickers and a chip are wider than a 320 pt window, so the bar has a third row: the
    /// symbol picker and the chip, the account picker, the price group. A 320 pt window — the
    /// ticket-under view — leaves the bar 304 pt, which is three rows in every look.
    #[test]
    fn the_bar_takes_three_rows_below_its_second_threshold() {
        let t = Tokens::of(&egui::Context::default());
        assert!(height(THREE_ROWS_BELOW - 1.0, &t) > height(THREE_ROWS_BELOW, &t));
        assert_eq!(
            [TWO_ROWS_BELOW, THREE_ROWS_BELOW - 1.0].map(|w| rows(w, &t)),
            [1, 3],
            "at Normal density and Standard text the thresholds are the widths themselves"
        );
        for density in Density::ALL {
            for text_size in TextSize::ALL {
                let a = Appearance { density, text_size, ..Appearance::default() };
                assert_eq!(
                    rows(304.0, &Tokens::from_appearance(&a)),
                    3,
                    "{density:?} {text_size:?}"
                );
            }
        }
    }

    /// An EMPTY source is a link nobody reported on, so the badge says `FEED ?`. Read through
    /// `parse_feed_status` alone it would be `Disconnected`, and the badge would say `FEED DOWN`
    /// about a link it knows nothing of.
    #[test]
    fn an_empty_source_is_an_unknown_link_not_a_dead_one() {
        assert_eq!(link_of(""), C::Unknown);
        assert_eq!(link_of("   "), C::Unknown);
        assert_eq!(feed_word(link_of("")), "FEED ?");
        assert_eq!(link_of(LIVE_SOURCE), C::Connected);
        assert_eq!(link_of(DIALLING_SOURCE), C::Connecting);
        assert_eq!(link_of("disconnected"), C::Disconnected);
    }

    /// A match's price has no tick beside it, so it prints six significant digits, grouped, with
    /// trailing zeros dropped.
    #[test]
    fn a_matchs_price_prints_six_significant_digits() {
        assert_eq!(mark_text(65_432.1), "65,432.1");
        assert_eq!(mark_text(123_456.7), "123,457");
        assert_eq!(mark_text(100.0), "100");
        assert_eq!(mark_text(0.5231), "0.5231");
        assert_eq!(mark_text(0.000_012_3), "0.0000123");
        assert_eq!(mark_text(f64::NAN), "—");
    }

    /// A list's salts are each row's key and the number of rows before it with the same key, as
    /// the per-row scan gave them, so every id a held press is keyed on is unchanged and two rows
    /// that look alike still never share one. They are worked out in ONE pass, each row's key read
    /// once: the per-row scan read the rows before every row, every frame, while the picker was
    /// open (n·(n+1)/2 reads: 100,000 at 450 accounts).
    #[test]
    fn a_lists_salts_are_each_rows_key_and_its_earlier_twins_read_in_one_pass() {
        let items = ["a", "b", "a", "c", "a", "b"];
        let reads = std::cell::Cell::new(0);
        let salts = row_salts(&items, |s| {
            reads.set(reads.get() + 1);
            *s
        });
        assert_eq!(salts, [("a", 0), ("b", 0), ("a", 1), ("c", 0), ("a", 2), ("b", 1)]);
        // CONTROL: the definition, worked out the slow way here.
        for (i, salt) in salts.iter().enumerate() {
            let twins = items[..i].iter().filter(|x| **x == items[i]).count();
            assert_eq!(*salt, (items[i], twins), "row {i}");
        }
        let distinct: std::collections::HashSet<_> = salts.iter().collect();
        assert_eq!(distinct.len(), items.len(), "two rows share a salt: {salts:?}");
        assert_eq!(reads.get(), items.len(), "each row's key is read once, not once per later row");
        assert!(row_salts(&[] as &[&str], |s| *s).is_empty(), "an empty list has no salts");
    }

    const BTC: Grid = Grid { tick: 0.1, lot: 0.001, min_qty: 0.001 };
    const LIVE_SOURCE: &str = "datahub 127.0.0.1:7878 — 1/1 stream(s) live";
    const DIALLING_SOURCE: &str = "datahub 127.0.0.1:7878 — connecting";
    const NOT_RUNNING: &str = "Not running on the server.";

    /// One account row: its label is its name, else "main".
    fn row(
        venue: &'static str,
        venue_label: &'static str,
        account: Option<&'static str>,
        symbol: &'static str,
        mode: AccountMode,
    ) -> AccountRow<'static> {
        AccountRow {
            venue,
            venue_label,
            account,
            symbol,
            name: account.unwrap_or("main"),
            mode,
            why_not: None,
        }
    }

    /// What one harness draws. Owned, so the harness can lend it to `TradeInputs` every frame.
    #[derive(Clone)]
    struct Scene {
        venue: &'static str,
        venue_label: &'static str,
        account: Option<&'static str>,
        symbol: &'static str,
        mode: AccountMode,
        source: &'static str,
        accounts: Vec<AccountRow<'static>>,
        recent: Vec<(&'static str, &'static str)>,
        matches: Vec<SymbolMatch<'static>>,
        unconnected: Vec<Unconnected<'static>>,
        /// Whether the book has both sides, so the bar shows a spread.
        spread: bool,
    }

    /// A window on okx's BTC swap, on the venue's default account, PAPER, with a live depth link.
    fn okx() -> Scene {
        Scene {
            venue: "okx",
            venue_label: "OKX",
            account: None,
            symbol: "BTC-USDT-SWAP",
            mode: AccountMode::Paper,
            source: LIVE_SOURCE,
            accounts: vec![row("okx", "OKX", None, "BTC-USDT-SWAP", AccountMode::Paper)],
            recent: Vec::new(),
            matches: Vec::new(),
            unconnected: Vec::new(),
            spread: true,
        }
    }

    /// The widest bar the window draws (the Trade window's fit scene): a long swap symbol, a long
    /// venue and account, and whichever mode chip and FEED word it is given.
    fn widest(mode: AccountMode, source: &'static str) -> Scene {
        Scene {
            venue: "hyperliquid",
            venue_label: "Hyperliquid",
            account: Some("sub-account-2"),
            symbol: "BTC-USDT-SWAP",
            mode,
            source,
            accounts: vec![row(
                "hyperliquid",
                "Hyperliquid",
                Some("sub-account-2"),
                "BTC-USDT-SWAP",
                mode,
            )],
            recent: Vec::new(),
            matches: Vec::new(),
            unconnected: Vec::new(),
            spread: true,
        }
    }

    /// The two looks the widest bar is measured in: a LIVE account on a live link, and an account
    /// of unknown mode on a link that is dialling — the widest chip and the widest FEED word.
    const WIDEST: [(AccountMode, &str); 2] =
        [(AccountMode::Live, LIVE_SOURCE), (AccountMode::Unknown, DIALLING_SOURCE)];

    struct Fixture {
        state: TradeState,
        scene: Scene,
        emitted: Vec<TradeAction>,
        /// The band the bar was drawn into, as the window's `draw` sizes it.
        band: egui::Rect,
    }

    /// A one-level book, 65,432.0 / 65,432.5: a spread of exactly 0.5.
    fn book() -> L2Book {
        let mut b = L2Book::new(0.1);
        b.apply_snapshot(1, &[BookLevel::new(65_432.0, 1.0)], &[BookLevel::new(65_432.5, 1.0)]);
        b
    }

    /// The same book with its asks gone: no spread to show.
    fn bids_only() -> L2Book {
        let mut b = L2Book::new(0.1);
        b.apply_snapshot(1, &[BookLevel::new(65_432.0, 1.0)], &[]);
        b
    }

    /// The bar alone, drawn as the window draws it: into a clipped band [`height`] high across the
    /// width, under the appearance `look`. Tooltips show at once: egui's 0.5 s delay outlasts
    /// `Harness::run`.
    fn harness(scene: Scene, look: Appearance, size: egui::Vec2) -> Harness<'static, Fixture> {
        let (two_sided, one_sided) = (book(), bids_only());
        Harness::builder().with_size(size).build_ui_state(
            move |ui, f: &mut Fixture| {
                if !vike_ui_theme::harness::appearance_ready(ui.ctx(), &look) {
                    return;
                }
                ui.ctx().all_styles_mut(|s| s.interaction.tooltip_delay = 0.0);
                let s = &f.scene;
                let book = if s.spread { &two_sided } else { &one_sided };
                let inputs = TradeInputs {
                    venue: s.venue,
                    venue_label: s.venue_label,
                    account: s.account,
                    symbol: s.symbol,
                    base: "BTC",
                    quote: "USDT",
                    mode: s.mode,
                    tradable: Tradable::Yes,
                    grid: BTC,
                    book,
                    last: Some(65_432.1),
                    stale: false,
                    source: s.source,
                    absence: None,
                    orders: &[],
                    orders_why: None,
                    position: None,
                    buying_power: None,
                    caps: VenueCaps::UNSUPPORTED,
                    bracket_why: s.account.map(|_| crate::trade::ticket::TPSL_ACCOUNT_WHY),
                    bracket_wire: false,
                    matches: &s.matches,
                    recent: &s.recent,
                    accounts: &s.accounts,
                    accounts_why: None,
                    unconnected: &s.unconnected,
                    status: None,
                };
                let t = Tokens::of(ui.ctx());
                let full = ui.available_rect_before_wrap();
                let band = egui::Rect::from_min_size(
                    full.min,
                    egui::vec2(full.width(), height(full.width(), &t)),
                );
                let mut region = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(band)
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                region.shrink_clip_rect(band);
                bar(&mut region, &t, &mut f.state, &inputs, &mut f.emitted);
                f.band = band;
            },
            Fixture {
                state: TradeState::default(),
                scene,
                emitted: Vec::new(),
                band: egui::Rect::NOTHING,
            },
        )
    }

    /// Every word the last frame painted.
    fn painted(h: &Harness<'_, Fixture>) -> Vec<String> {
        fn walk(s: &egui::Shape, out: &mut Vec<String>) {
            match s {
                egui::Shape::Text(t) => out.push(t.galley.text().to_string()),
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let mut out = Vec::new();
        for c in &h.output().shapes {
            walk(&c.shape, &mut out);
        }
        out
    }

    /// Open the account picker: the bar's button names the window's venue and account.
    fn open_accounts(h: &mut Harness<'static, Fixture>, venue_and_account: &str) {
        h.run();
        h.get_by_label_contains(venue_and_account).click();
        h.run();
    }

    /// An account whose mode the backend did not report reads `MODE ?` in the bar AND in the
    /// account picker — the list a trader picks where an order goes from — and never PAPER (spec
    /// §4.3). An older node reports no mode for any account, so every row is unknown, and such a row
    /// still picks like any other.
    #[test]
    fn an_unknown_mode_reads_mode_unknown_and_never_paper() {
        let mut scene = okx();
        scene.mode = AccountMode::Unknown;
        scene.accounts = vec![
            row("okx", "OKX", None, "BTC-USDT-SWAP", AccountMode::Unknown),
            row("bybit", "Bybit", None, "BTCUSDT", AccountMode::Unknown),
        ];
        let mut h = harness(scene, Appearance::default(), egui::vec2(600.0, 420.0));
        open_accounts(&mut h, "OKX · main");
        let words = painted(&h);
        assert_eq!(
            words.iter().filter(|w| *w == "MODE ?").count(),
            3,
            "the bar's chip and both rows: {words:?}"
        );
        assert!(!words.iter().any(|w| w.contains("PAPER")), "{words:?}");
        h.get_by_label("Bybit · main").click();
        h.run();
        assert_eq!(
            h.state().emitted,
            [TradeAction::PickAccount {
                venue: "bybit".to_string(),
                account: None,
                symbol: "BTCUSDT".to_string(),
            }]
        );
    }

    /// A venue spells one instrument its own way, so picking an account on ANOTHER venue opens that
    /// venue's own symbol, never the window's; and a recent instrument is a (venue, symbol) pair, so
    /// picking one moves the window to its venue too.
    #[test]
    fn a_pick_on_another_venue_carries_that_venues_own_symbol() {
        let mut scene = okx();
        scene.accounts.push(row("binance", "Binance", None, "BTCUSDT", AccountMode::Demo));
        scene.recent = vec![("bybit", "BTCUSDT")];
        let mut h = harness(scene, Appearance::default(), egui::vec2(600.0, 420.0));
        open_accounts(&mut h, "OKX · main");
        h.get_by_label("Binance · main").click();
        h.run();
        assert_eq!(
            h.state().emitted,
            [TradeAction::PickAccount {
                venue: "binance".to_string(),
                account: None,
                symbol: "BTCUSDT".to_string(),
            }]
        );
        h.state_mut().emitted.clear();
        h.get_by_label_contains("BTC-USDT-SWAP").click();
        h.run();
        h.get_by_label_contains("BTCUSDT · bybit").click();
        h.run();
        assert_eq!(
            h.state().emitted,
            [TradeAction::PickSymbol { venue: "bybit".to_string(), symbol: "BTCUSDT".to_string() }]
        );
    }

    /// An account the database holds but the server does not run is listed greyed, says why on
    /// hover, and takes no click (spec §3.3). Hover needs the zero tooltip delay `harness` sets.
    #[test]
    fn a_greyed_account_says_why_and_takes_no_pick() {
        let mut scene = okx();
        scene.accounts.push(AccountRow {
            why_not: Some(NOT_RUNNING),
            ..row("okx", "OKX", Some("HEDGE"), "BTC-USDT-SWAP", AccountMode::Live)
        });
        let mut h = harness(scene, Appearance::default(), egui::vec2(600.0, 420.0));
        open_accounts(&mut h, "OKX · main");
        h.get_by_label("OKX · HEDGE").hover();
        h.run();
        assert!(h.query_by_label(NOT_RUNNING).is_some(), "the reason shows on hover");
        h.get_by_label("OKX · HEDGE").click();
        h.run();
        assert!(h.state().emitted.is_empty(), "{:?}", h.state().emitted);
    }

    /// An empty depth source shows `FEED ?` in the bar, not `FEED DOWN`.
    #[test]
    fn an_empty_source_shows_feed_unknown_in_the_bar() {
        let mut scene = okx();
        scene.source = "";
        let mut h = harness(scene, Appearance::default(), egui::vec2(600.0, 200.0));
        h.run();
        assert!(h.query_by_label("FEED ?").is_some());
        assert!(h.query_by_label("FEED DOWN").is_none());
    }

    /// The FW2 review: the bar's price is the book's MID on the desktop, because no last trade
    /// price reaches it (a snapshot from the node carries no marks), so its hover leads with that
    /// and never calls it the last price.
    #[test]
    fn the_bars_price_says_it_is_the_books_mid() {
        let mut h = harness(okx(), Appearance::default(), egui::vec2(600.0, 200.0));
        h.run();
        h.get_by_label("65,432.1").hover();
        h.run();
        assert!(h.query_by_label_contains("Mid price").is_some(), "the hover names the mid");
        assert!(h.query_by_label_contains("Last price").is_none(), "and no last price");
    }

    /// The symbol picker's DRAWN rows (W2 review minor 4): each match shows its symbol, its name,
    /// its price (a dash for none) and its venue, and one the account does not trade says "view
    /// only"; the arrows move the highlight, wrapping, Enter opens the highlighted match, and a
    /// click opens the one clicked — each on its OWN venue.
    #[test]
    fn the_symbol_pickers_rows_show_their_facts_and_pick_by_key_and_by_click() {
        let mut scene = okx();
        scene.matches = vec![
            SymbolMatch {
                venue: "binance",
                venue_label: "Binance",
                symbol: "BTCUSDT",
                name: "Bitcoin",
                last: Some(65_432.1),
                tradable: true,
            },
            SymbolMatch {
                venue: "okx",
                venue_label: "OKX",
                symbol: "ETH-USDT-SWAP",
                name: "Ether",
                last: None,
                tradable: false,
            },
        ];
        let first = "BTCUSDT  Bitcoin  65,432.1  Binance";
        let second = "ETH-USDT-SWAP  Ether  —  OKX  · view only";
        let mut h = harness(scene, Appearance::default(), egui::vec2(600.0, 480.0));
        h.run();
        h.get_by_label_contains("BTC-USDT-SWAP").click();
        h.run();
        assert!(h.query_by_label(first).is_some(), "the first match's row");
        assert!(h.query_by_label(second).is_some(), "the second match's row");
        for (key, hot) in
            [(egui::Key::ArrowDown, 1), (egui::Key::ArrowDown, 0), (egui::Key::ArrowUp, 1)]
        {
            h.key_press(key);
            h.run();
            assert_eq!(h.state().state.hot, hot, "{key:?}");
        }
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(
            h.state().emitted,
            [TradeAction::PickSymbol {
                venue: "okx".to_string(),
                symbol: "ETH-USDT-SWAP".to_string()
            }]
        );
        h.state_mut().emitted.clear();
        h.get_by_label_contains("BTC-USDT-SWAP").click();
        h.run();
        h.get_by_label(first).click();
        h.run();
        assert_eq!(
            h.state().emitted,
            [TradeAction::PickSymbol {
                venue: "binance".to_string(),
                symbol: "BTCUSDT".to_string()
            }]
        );
    }

    /// Every unconnected venue's button reads "Connect"; its accessible NAME carries the venue, so
    /// a screen reader (and a test) can tell one from another without the label beside it (W2
    /// review minor 7). The venue is the row's own label, never a name spelled here.
    #[test]
    fn each_connect_button_names_the_venue_it_connects() {
        let mut scene = okx();
        scene.unconnected =
            vec![Unconnected { venue_label: "Bybit" }, Unconnected { venue_label: "Deribit" }];
        let mut h = harness(scene, Appearance::default(), egui::vec2(600.0, 420.0));
        open_accounts(&mut h, "OKX · main");
        assert!(h.query_by_label("Connect Bybit").is_some(), "the first venue's button");
        h.get_by_label("Connect Deribit").click();
        h.run();
        assert_eq!(h.state().emitted, [TradeAction::OpenConnections]);
    }

    /// The bar's widget-id audit (W4 fix round 2's parked minor): the symbol search keeps the
    /// trader's focus — every keystroke — while what the bar shows changes under it: the mode chip
    /// (a mode becoming unknown and known again), the spread (a book losing a side and getting it
    /// back), the FEED badge (the link going unreported and live again), and in the picker itself
    /// the Recent row and the matches coming and going. Each change is ONE frame and the next
    /// keystroke the frame after it: egui drops the focus of a field whose id did not come back at
    /// the end of the frame that moved it, so the keystroke after it is lost (the field asks for
    /// the focus again only once it has none).
    #[test]
    fn a_focused_symbol_search_keeps_every_keystroke_while_the_bar_changes() {
        let mut h = harness(okx(), Appearance::default(), egui::vec2(600.0, 480.0));
        h.run();
        h.get_by_label_contains("BTC-USDT-SWAP").click();
        h.run();
        h.event(egui::Event::Text("B".to_string()));
        h.run();
        assert_eq!(h.state().state.query, "B", "CONTROL: the search has the focus");
        let eth = SymbolMatch {
            venue: "okx",
            venue_label: "OKX",
            symbol: "ETH-USDT-SWAP",
            name: "Ether",
            last: None,
            tradable: true,
        };
        let changes = [
            "the mode becomes unknown",
            "the spread goes",
            "the link goes unreported",
            "a recent pick appears",
            "a match appears",
            "the mode is known again",
            "the spread comes back",
            "the link is live again",
            "the recent pick goes",
            "the match goes",
        ];
        let change = |s: &mut Scene, what: &str| match what {
            "the mode becomes unknown" => s.mode = AccountMode::Unknown,
            "the spread goes" => s.spread = false,
            "the link goes unreported" => s.source = "",
            "a recent pick appears" => s.recent = vec![("bybit", "BTCUSDT")],
            "a match appears" => s.matches = vec![eth],
            "the mode is known again" => s.mode = AccountMode::Paper,
            "the spread comes back" => s.spread = true,
            "the link is live again" => s.source = LIVE_SOURCE,
            "the recent pick goes" => s.recent.clear(),
            _ => s.matches.clear(),
        };
        let mut typed = String::from("B");
        for (what, key) in changes.into_iter().zip("TCUSDTSWAP".chars()) {
            change(&mut h.state_mut().scene, what);
            h.step();
            h.event(egui::Event::Text(key.to_string()));
            h.step();
            h.run();
            typed.push(key);
            assert_eq!(h.state().state.query, typed, "{what}: every keystroke kept");
        }
    }

    /// The bar's id audit, the account picker's half: its rows are a LIST, and a list changes under
    /// a held press (the node's directory reply arrives, an account starts running). A press held
    /// on an account's row while a row appears ABOVE it is released on THAT account — never on the
    /// row that took its place, which would move the window, and every order sent from it after, to
    /// another account. CONTROL: a plain click on the row picks it.
    #[test]
    fn a_press_on_an_account_row_held_while_a_row_appears_above_it_picks_that_account() {
        let binance = || TradeAction::PickAccount {
            venue: "binance".to_string(),
            account: None,
            symbol: "BTCUSDT".to_string(),
        };
        let mut scene = okx();
        scene.accounts.push(row("binance", "Binance", None, "BTCUSDT", AccountMode::Demo));
        let mut h = harness(scene, Appearance::default(), egui::vec2(600.0, 420.0));
        open_accounts(&mut h, "OKX · main");
        h.get_by_label("Binance · main").click();
        h.run();
        assert_eq!(h.state().emitted, [binance()], "CONTROL: a click picks the row");
        h.state_mut().emitted.clear();
        open_accounts(&mut h, "OKX · main");
        let at = h.get_by_label("Binance · main").rect().center();
        let button = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        h.event(egui::Event::PointerMoved(at));
        h.event(button(true));
        h.step();
        let bybit = row("bybit", "Bybit", None, "BTCUSDT", AccountMode::Paper);
        h.state_mut().scene.accounts.insert(1, bybit);
        h.step();
        h.step();
        let moved = h.get_by_label("Binance · main").rect().center();
        assert!(moved.y > at.y, "CONTROL: the row moved down under the press: {at:?} -> {moved:?}");
        h.event(button(false));
        h.run();
        assert_eq!(h.state().emitted, [binance()], "the account pressed, and nothing else");
    }

    /// Press the primary button on the control named `label`, let `change` move the scene under
    /// the held button (each a frame of its own), then release where it went down. Answers where
    /// `label` was, and where it is just before the release (a pick closes the picker).
    fn press_change_release(
        h: &mut Harness<'static, Fixture>,
        label: &str,
        change: impl FnOnce(&mut Scene),
    ) -> (egui::Pos2, egui::Pos2) {
        let at = h.get_by_label(label).rect().center();
        let button = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        h.event(egui::Event::PointerMoved(at));
        h.event(button(true));
        h.step();
        change(&mut h.state_mut().scene);
        h.step();
        h.step();
        let moved = h.get_by_label(label).rect().center();
        h.event(button(false));
        h.run();
        (at, moved)
    }

    /// The F wave's review, parked minor 6: the symbol picker's half of the id audit. Its Recent
    /// row is a LIST, and it re-ranks under a held press (the window moved, so a pick went to the
    /// front). A press held on a recent pick while another appears before it is released on THAT
    /// pick — never on the one that took its place, which would move the window to another venue
    /// or symbol. CONTROLS: a plain click picks it, and the pick did move under the press.
    #[test]
    fn a_press_on_a_recent_pick_held_while_the_picks_re_rank_picks_that_pick() {
        let eth = || TradeAction::PickSymbol {
            venue: "binance".to_string(),
            symbol: "ETHUSDT".to_string(),
        };
        let mut scene = okx();
        scene.recent = vec![("bybit", "BTCUSDT"), ("binance", "ETHUSDT")];
        let mut h = harness(scene, Appearance::default(), egui::vec2(600.0, 480.0));
        let open = |h: &mut Harness<'static, Fixture>| {
            h.run();
            h.get_by_label_contains("BTC-USDT-SWAP").click();
            h.run();
        };
        open(&mut h);
        h.get_by_label("ETHUSDT · binance").click();
        h.run();
        assert_eq!(h.state().emitted, [eth()], "CONTROL: a click picks it");
        h.state_mut().emitted.clear();
        open(&mut h);
        let (at, moved) = press_change_release(&mut h, "ETHUSDT · binance", |s| {
            s.recent.insert(0, ("okx", "SOLUSDT"));
        });
        assert_ne!(moved, at, "CONTROL: the pick moved under the press");
        assert_eq!(h.state().emitted, [eth()], "the pick pressed, and nothing else");
    }

    /// ...and the matches' half: a press held on a match while a match appears ABOVE it (the
    /// catalog's answer to what the trader typed arriving) is released on THAT match. CONTROLS: a
    /// plain click picks it, and the row did move down under the press.
    #[test]
    fn a_press_on_a_match_held_while_a_match_appears_above_it_picks_that_match() {
        let matching = |venue, venue_label, symbol| SymbolMatch {
            venue,
            venue_label,
            symbol,
            name: "Coin",
            last: None,
            tradable: true,
        };
        let eth = || TradeAction::PickSymbol {
            venue: "okx".to_string(),
            symbol: "ETH-USDT-SWAP".to_string(),
        };
        let mut scene = okx();
        scene.matches = vec![
            matching("binance", "Binance", "BTCUSDT"),
            matching("okx", "OKX", "ETH-USDT-SWAP"),
        ];
        let row = "ETH-USDT-SWAP  Coin  —  OKX";
        let mut h = harness(scene, Appearance::default(), egui::vec2(600.0, 480.0));
        let open = |h: &mut Harness<'static, Fixture>| {
            h.run();
            h.get_by_label_contains("BTC-USDT-SWAP").click();
            h.run();
        };
        open(&mut h);
        h.get_by_label(row).click();
        h.run();
        assert_eq!(h.state().emitted, [eth()], "CONTROL: a click picks it");
        h.state_mut().emitted.clear();
        open(&mut h);
        let (at, moved) = press_change_release(&mut h, row, |s| {
            s.matches.insert(0, matching("bybit", "Bybit", "SOLUSDT"));
        });
        assert!(moved.y > at.y, "CONTROL: the row moved down under the press: {at:?} -> {moved:?}");
        assert_eq!(h.state().emitted, [eth()], "the match pressed, and nothing else");
    }

    /// Half a point: the tolerance for the rounding egui does to a rect's edges.
    const EPS: f32 = 0.6;

    /// One control's accessible name and where it landed.
    struct Control {
        text: String,
        rect: egui::Rect,
    }

    /// Every button and label the last frame drew.
    fn controls(h: &Harness<'_, Fixture>) -> Vec<Control> {
        h.root()
            .children_recursive()
            .filter_map(|n| {
                let node = n.accesskit_node();
                if !matches!(node.role(), Role::Button | Role::Label) {
                    return None;
                }
                let text: String = node.label().or_else(|| node.value()).unwrap_or_default();
                (!text.is_empty()).then(|| Control { text, rect: n.rect() })
            })
            .collect()
    }

    /// The bar fits every width a window gives it, in every look: every control lies inside the band
    /// `height` gave it, and none lies on another. Swept over windows 260 to 900 pt wide (the bar
    /// is 16 pt narrower: `egui_kittest`'s central panel keeps an 8 pt margin) — from under the
    /// ticket-alone window's 280 (W2 review minor 3) — every density and text size, with the widest
    /// content the bar holds. The pickers, the mode, the FEED badge and the price are drawn at every
    /// width; the spread where it fits (`price_group` gives it up first). Every violation is
    /// collected and reported together, so one run says where a threshold is wrong and by how much.
    #[test]
    fn the_bar_fits_every_width_in_every_look() {
        let always = ["BTC-USDT-SWAP", "Hyperliquid · sub-account-2", "FEED ", "65,432.1"];
        let mut found: BTreeMap<String, Vec<u32>> = BTreeMap::new();
        for density in Density::ALL {
            for text_size in TextSize::ALL {
                let look = Appearance { density, text_size, ..Appearance::default() };
                for (mode, source) in WIDEST {
                    let mut h = harness(widest(mode, source), look, egui::vec2(900.0, 200.0));
                    for w in (260..=900).step_by(10) {
                        h.set_size(egui::vec2(w as f32, 200.0));
                        h.run();
                        let band = h.state().band;
                        let cs = controls(&h);
                        let mut fail = |what: String| {
                            let key = format!("{density:?}/{text_size:?}/{mode:?}: {what}");
                            found.entry(key).or_default().push(w);
                        };
                        let chip = if mode == AccountMode::Live { "LIVE" } else { MODE_UNKNOWN };
                        for want in always.iter().chain([&chip]) {
                            if !cs.iter().any(|c| c.text.starts_with(want)) {
                                fail(format!("{want:?} is not drawn"));
                            }
                        }
                        // W4 fix round 1, M4: the spread gives way only where it does not fit: a
                        // bar with room for everything on one row draws it.
                        if w == 900 && !cs.iter().any(|c| c.text.starts_with("Spread ")) {
                            fail("the spread is not drawn with room to spare".to_string());
                        }
                        for c in &cs {
                            if !band.expand(EPS).contains_rect(c.rect) {
                                fail(format!("{:?} lies outside the bar's band", c.text));
                            }
                        }
                        for (i, a) in cs.iter().enumerate() {
                            for b in &cs[i + 1..] {
                                let o = a.rect.intersect(b.rect);
                                if o.width() > EPS && o.height() > EPS {
                                    fail(format!("{:?} lies on {:?}", a.text, b.text));
                                }
                            }
                        }
                    }
                }
            }
        }
        let report: Vec<String> =
            found.iter().map(|(what, widths)| format!("{what} at {widths:?}")).collect();
        assert!(report.is_empty(), "{}", report.join("\n"));
    }

    /// Re-measures what [`TWO_ROWS_BELOW`] and [`THREE_ROWS_BELOW`] are measured from: the widest
    /// bar, drawn on one row in a window wide enough for anything, in every look. Run it with
    /// `cargo test -p vike-panels --lib print_bar_widths -- --ignored --nocapture`.
    #[test]
    #[ignore = "a measurement, not a check: it re-derives the bar's two thresholds"]
    fn print_bar_widths() {
        let (mut one_row, mut first_of_two) = (0.0_f32, 0.0_f32);
        for density in Density::ALL {
            for text_size in TextSize::ALL {
                let look = Appearance { density, text_size, ..Appearance::default() };
                let t = Tokens::from_appearance(&look);
                for (mode, source) in WIDEST {
                    let mut h = harness(widest(mode, source), look, egui::vec2(2000.0, 200.0));
                    h.run();
                    let cs = controls(&h);
                    let width_of = |prefix: &str| {
                        cs.iter()
                            .find(|c| c.text.starts_with(prefix))
                            .map_or(f32::NAN, |c| c.rect.width())
                    };
                    let span = |left: bool| {
                        let side: Vec<&Control> =
                            cs.iter().filter(|c| (c.rect.center().x < 1000.0) == left).collect();
                        let min = side.iter().map(|c| c.rect.min.x).fold(f32::MAX, f32::min);
                        let max = side.iter().map(|c| c.rect.max.x).fold(f32::MIN, f32::max);
                        max - min
                    };
                    let (pickers, prices) = (span(true), span(false));
                    let whole = pickers + t.metrics.gap + prices;
                    let k = scale(&t);
                    one_row = one_row.max(whole / k);
                    first_of_two = first_of_two.max(pickers / k);
                    println!(
                        "{density:?}/{text_size:?}/{mode:?}: one row {whole:.1} (/{k:.3} = \
                         {:.1}); pickers and chip {pickers:.1} (/{k:.3} = {:.1}); symbol {:.1}, \
                         account {:.1}, chip {:.1}, prices {prices:.1}",
                        whole / k,
                        pickers / k,
                        width_of("BTC-USDT-SWAP"),
                        width_of("Hyperliquid"),
                        width_of(if mode == AccountMode::Live { "LIVE" } else { "MODE ?" }),
                    );
                }
            }
        }
        println!("measured: TWO_ROWS_BELOW >= {one_row:.1}, THREE_ROWS_BELOW >= {first_of_two:.1}");
    }
}
