//! The instrument bar (spec §3.3): the symbol and venue · account pickers, the mode chip, the last
//! price and spread, and the FEED badge that names the window's own depth link (Ruling R1 — the
//! DOM's "depth" strip, moved).
//!
//! The bar takes one, two or three rows, by width ([`rows`]): everything on one, the price and spread
//! at the right (the owner's v3 design); the pickers and the chip, then the price at the left and the
//! spread at the right, on two; and on three, the symbol picker and the chip, the account picker,
//! then the price and spread — two pickers and a chip are wider than a 262 pt window. [`height`] is
//! the band the window gives the bar, so whatever it holds, the ladder starts below it.
//!
//! Two honesty rules live here:
//! - an account whose mode the backend did not report is drawn `MODE ?`, never as PAPER — in the
//!   bar AND in the account picker, the list a trader picks where an order goes from (spec §4.3);
//! - the FEED badge reads the depth link through [`link_of`], so a link nobody reported on says
//!   `FEED ?` rather than `FEED DOWN`.

use std::collections::HashMap;
use std::hash::Hash;

use egui::RichText;
use vike_model::AccountTier;
use vike_model::feed_status::{DataClientConnectionState, DataClientStatus};
use vike_ui_theme::components::account_menu::{
    ThemeAccountMenuMark, ThemeAccountMenuRow, account_row, connect_row,
};
use vike_ui_theme::components::button::ThemeButtonAction;

use vike_ui_theme::components::venue_chip::{ThemeVenueChipFlatRow, chip_pad, flat_row};
use vike_ui_theme::components::{ThemeStatus, ThemeTokens, chip, input};
use vike_ui_theme::icons;
use vike_ui_theme::maps::{self, ThemeMapRow};
use vike_ui_theme::metrics::{ThemeDensity, space, stroke};
use vike_ui_theme::type_scale::ThemeTextRole;
use vike_ui_theme::value::trade;

use super::{TradeWindowAction, TradeWindowInputs, TradeWindowState, keyed, ladder, section};

// The two thresholds are the bar's WIDEST content, MEASURED 2026-10-03 at Normal density and
// Standard text by `instrument_bar_tests::screen::print_bar_widths` over the Trade window's fit
// scene (a swap's thirteen-character symbol, a 27-character venue · account label, a price of
// 65,432.1) in its widest state: an unknown mode's `MODE ?` chip and a dialling link's
// `FEED DIALLING`. `screen::the_bar_fits_every_width_in_every_look` sweeps every look and width,
// so a control that grows fails there instead of overlapping here.

// `trade::TWO_ROWS_BELOW`: below this width the bar takes two rows, the pickers and the chip, then the
// price group. The widest one-row bar measured 602.1 pt (551.8 with a LIVE chip and `FEED UP`), plus
// 8.4 pt — one Title-size mono digit — for the thousands comma the ladder's price format prints and
// the measurement's price did not. Re-measured 2026-10-03 with that format printing it: 610.1 pt
// (559.8 with a LIVE chip and `FEED UP`), inside the budget. [`rows`] grows it for a looser or
// larger look.
//
// `trade::THREE_ROWS_BELOW`: below this width the bar takes three rows, the symbol picker and the
// chip, the account picker, the price group. The widest first row of two — the two pickers and the
// chip — measured 384.7 pt.
//
// `trade::SYMBOL_PICKER_WIDTH`: the symbol picker's width, its search field and its lines taking all of
// it. Wider than the Trade window can be: the picker is a popup, and a line holds a chip for every
// venue. `trade::ACCOUNT_PICKER_WIDTH` is the account picker's narrowest width and `trade::MATCHES_HEIGHT`
// how tall the symbol picker's list grows before it scrolls.

/// How many lines the symbol picker draws before it stops (the scroll area holds the rest back).
const ROWS_SHOWN: usize = 60;

/// The mode chip's word for an account whose mode the backend did not report.
const MODE_UNKNOWN: &str = "MODE ?";
/// ...and what hovering it says, in the bar and in the account picker alike.
const MODE_UNKNOWN_WHY: &str = "The backend does not say whether this account is paper, demo or \
                                live: it predates that report, or names a mode this app does not \
                                know. This window treats the account as live.";

/// How much wider than Normal density at Small text a look's bar is, at most: the largest of
/// the ratios its control height, its padding and its text grow by (never below 1, so a denser
/// look keeps the thresholds and gives a row up a little early, never late). The DOM's strips were
/// scaled the same way.
fn scale(t: &ThemeTokens) -> f32 {
    let base = ThemeDensity::Normal.metrics();
    let m = &t.metrics;
    let text = super::layout::text_scale(t);
    (m.control_height / base.control_height).max(m.padding / base.padding).max(text).max(1.0)
}

/// How many rows the bar takes in a band `width` wide under `t`: one, two below
/// [`trade::TWO_ROWS_BELOW`], three below [`trade::THREE_ROWS_BELOW`], each threshold grown by the largest of
/// the ratios the look's control height, padding and text exceed Normal density at Small text
/// by.
pub fn rows(width: f32, t: &ThemeTokens) -> usize {
    let k = scale(t);
    if width >= trade::TWO_ROWS_BELOW * k {
        1
    } else if width >= trade::THREE_ROWS_BELOW * k {
        2
    } else {
        3
    }
}

/// The bar's height at `width` for the WIDEST content it can hold: its rows, a gap between each two,
/// and a gap above and below. What a window opens at ([`super::layout::window_size`], which has no
/// instrument yet) and what the fit sweep, which draws the widest content, counts; the bar of a window
/// draws [`height_of`] the rows [`rows_for`] gives it.
pub fn height(width: f32, t: &ThemeTokens) -> f32 {
    height_of(rows(width, t), t)
}

/// The height of a bar of `n` rows: the rows, a gap between each two and above and below, and
/// the rule under them, which is the bar's own last row and `stroke::HAIRLINE` tall (the design's
/// band is 6 + 24 + 6 + 1).
pub fn height_of(n: usize, t: &ThemeTokens) -> f32 {
    let n = n as f32;
    n * t.metrics.control_height
        + (n - 1.0) * t.metrics.gap
        + 2.0 * t.metrics.gap
        + stroke::HAIRLINE
}

/// How many rows THIS window's bar takes in a band `width` wide: what its own words need, never more
/// than [`rows`] gives the widest content. The thresholds are for the widest symbol and venue ·
/// account label the bar can hold, and a window with ordinary ones (`BTCUSDT`, `Binance Perp ·
/// main`) fits the pickers and the chip on one row at 320 pt, as the owner's v3 design draws it.
/// Measured with the fonts the bar draws in, with a few points to spare, so a control that does not
/// fit moves to the next row rather than lying on another.
pub fn rows_for(
    context: &egui::Context,
    t: &ThemeTokens,
    inputs: &TradeWindowInputs<'_>,
    width: f32,
) -> usize {
    let caret = super::text_w(
        context,
        icons::DISCLOSE_OPEN.rich().text(),
        &egui::FontId::new(t.text.px(ThemeTextRole::Strong), icons::family()),
        t,
    );
    let picker = |words: &str| super::button_w(context, t, words) + t.metrics.gap + caret;
    let account = account_words(inputs);
    let chip_word = match inputs.mode {
        Some(m) => chip::tier_label(m),
        None => MODE_UNKNOWN,
    };
    let chip =
        super::text_w(context, chip_word, &t.mono(ThemeTextRole::Caption), t) + 2.0 * chip_pad(t);
    let price = match inputs.last {
        Some(p) => ladder::fmt_price(p, inputs.grid.tick_size),
        None => "—".to_string(),
    };
    let price_w = super::text_w(context, &price, &t.mono(ThemeTextRole::Title), t);
    let gap = t.metrics.gap;
    // `trade::SPARE` is what the measurement leaves unspent, in points.
    let pickers = picker(inputs.symbol) + gap + picker(&account) + gap + chip + trade::SPARE;
    let fit = if width >= pickers + gap + price_w {
        1
    } else if width >= pickers {
        2
    } else {
        3
    };
    fit.min(rows(width, t))
}

/// The state of the window's own depth link, as its producer set it on
/// [`TradeWindowInputs::source`], never guessed from the line's words. A venue with NO line is a
/// link nobody reported on, so it is `Unknown` and the badge says `FEED ?`, never `FEED DOWN` about
/// a link it knows nothing of. Whatever draws the window's link reads it here, so no two places can
/// disagree about it: the badge, and the bookless ladder's icon.
pub fn link_of(source: Option<&DataClientStatus>) -> DataClientConnectionState {
    source.map_or(DataClientConnectionState::Unknown, |s| s.state)
}

/// This link state's row of `ui-theme.toml`'s `feed_badge` map: `word` the badge's words, `stroke` its
/// outline and `text` its ink. Fixed, the same in every theme. A dead link is the ERROR red, where the
/// status bar's wordless dots use a muted grey (owner decision 4, 2026-09-29), so the badge is a map of its
/// own and not the status dot's `connection`. Exhaustive: a state without a row does not compile.
fn feed_row(state: DataClientConnectionState) -> &'static ThemeMapRow {
    match state {
        DataClientConnectionState::Connected => &maps::feed_badge::CONNECTED,
        DataClientConnectionState::Connecting => &maps::feed_badge::CONNECTING,
        DataClientConnectionState::Disconnected => &maps::feed_badge::DISCONNECTED,
        DataClientConnectionState::Error => &maps::feed_badge::ERROR,
        DataClientConnectionState::Unknown => &maps::feed_badge::UNKNOWN,
    }
}

/// The FEED badge's word for the window's own depth link: its row's `word`. Deliberately not the
/// LIVE/PAPER words the mode chip uses.
pub fn feed_word(state: DataClientConnectionState) -> &'static str {
    feed_row(state).word.unwrap_or("")
}

/// Draw the bar into `ui`, the band [`height`] sized for this width. Public so it is live code
/// before the window's `draw` calls it.
pub fn bar(
    ui: &mut egui::Ui,
    t: &ThemeTokens,
    state: &mut TradeWindowState,
    inputs: &TradeWindowInputs<'_>,
    actions: &mut Vec<TradeWindowAction>,
) {
    let n = rows_for(ui.ctx(), t, inputs, ui.available_width());
    // Rows a gap apart, the gap [`height`] counts: no other spacing between them.
    ui.spacing_mut().item_spacing.y = t.metrics.gap;
    ui.add_space(t.metrics.gap);
    // Each control under an id of its own (`section`), never one it takes from what the bar drew
    // before it: the account picker sits on the first row or the second by the window's width, the
    // mode is a chip or a muted badge, and the spread comes and goes with the book and the room.
    let key = ui.scope_id();
    let symbol =
        |ui: &mut egui::Ui, state: &mut TradeWindowState, actions: &mut Vec<TradeWindowAction>| {
            section(ui, key, "bar_symbol", |ui| symbol_picker(ui, t, state, inputs, actions));
        };
    let account = |ui: &mut egui::Ui, actions: &mut Vec<TradeWindowAction>| {
        section(ui, key, "bar_account", |ui| account_picker(ui, t, inputs, actions));
    };
    let mode = |ui: &mut egui::Ui| {
        section(ui, key, "bar_mode", |ui| {
            mode_mark(ui, inputs.mode);
        });
    };
    // One row: the price and the spread against the right edge. Under that: the price at the left
    // and the spread at the right, as the v3 design's narrow window draws them.
    let prices = |ui: &mut egui::Ui, split: bool, feed: bool| {
        section(ui, key, "bar_prices", |ui| price_row(ui, t, inputs, split, feed));
    };
    match n {
        1 => line(ui, t, |ui| {
            symbol(ui, state, actions);
            account(ui, actions);
            mode(ui);
            prices(ui, false, false);
        }),
        2 => {
            line(ui, t, |ui| {
                symbol(ui, state, actions);
                account(ui, actions);
                mode(ui);
            });
            line(ui, t, |ui| prices(ui, true, false));
        }
        _ => {
            line(ui, t, |ui| {
                symbol(ui, state, actions);
                mode(ui);
            });
            line(ui, t, |ui| account(ui, actions));
            line(ui, t, |ui| prices(ui, true, true));
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
fn line(ui: &mut egui::Ui, t: &ThemeTokens, add: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = t.metrics.gap;
        add(ui);
    });
}

/// The kit's chip for the account's tier, or `MODE ?` in the muted outline when the backend did not
/// report one. The bar's chip and the account picker's rows both draw it here, so neither can show
/// an unreported mode as PAPER.
fn mode_mark(ui: &mut egui::Ui, mode: Option<AccountTier>) -> egui::Response {
    match mode {
        Some(m) => chip::tier(ui, m),
        None => chip::badge(ui, MODE_UNKNOWN, ThemeStatus::Muted).on_hover_text(MODE_UNKNOWN_WHY),
    }
}

/// Whether the FEED word sits at the right end of the status strip (a window wide enough for the bar
/// to take one or two rows) or in the bar's last row (the narrow window, whose strip would have to cut
/// its words to make room).
pub(super) fn feeds_in_the_strip(
    context: &egui::Context,
    t: &ThemeTokens,
    inputs: &TradeWindowInputs<'_>,
    width: f32,
) -> bool {
    rows_for(context, t, inputs, width) < 3
}

/// The FEED badge: the window's own depth link, named in the status strip's right end (the v3
/// design's bar carries only the price and the spread, and the link must still be named, rule 3 of
/// the module doc of [`super`]). What it measures: [`feed_badge_w`].
pub(super) fn feed_badge(ui: &mut egui::Ui, inputs: &TradeWindowInputs<'_>) -> egui::Response {
    let link = link_of(inputs.source);
    let (row, t) = (feed_row(link), ThemeTokens::of(ui.ctx()));
    let (edge, ink) = (row.stroke.resolve(&t), row.text.resolve(&t));
    let source = inputs.source.map_or("", |s| s.text.trim());
    let detail = if source.is_empty() { "source unknown" } else { source };
    chip::badge_coloured(ui, &t, feed_word(link), edge, ink).on_hover_text(format!(
        "This window's depth arrives on the DATAHUB market-data link, not the backend connection          the status bar reports: {detail}"
    ))
}

/// How wide [`feed_badge`] is drawn: its word in the mono caption and the chip's padding, as
/// `chip::badge` lays it out.
pub(super) fn feed_badge_w(
    context: &egui::Context,
    t: &ThemeTokens,
    inputs: &TradeWindowInputs<'_>,
) -> f32 {
    let word = feed_word(link_of(inputs.source));
    super::text_w(context, word, &t.mono(ThemeTextRole::Caption), t) + 2.0 * chip_pad(t)
}

/// The window's price and the spread, as the v3 design draws them: the price in the Title mono, then
/// "Spread" in the caption grey beside its number in the mono. `split` puts the price at the row's
/// left edge and the spread at its right (the narrow windows' second row); else both sit against the
/// right edge, the spread first. `feed` adds the FEED word after the price: a bar of three rows is the
/// narrow window, whose strip has no room for it ([`feeds_in_the_strip`]).
///
/// The window's price is the book's MID: no last trade price reaches the desktop (a snapshot from the
/// node carries no marks; the FW2 review), so the price's hover says what it is. The spread is the
/// one that gives way where the two do not fit the row.
fn price_row(
    ui: &mut egui::Ui,
    t: &ThemeTokens,
    inputs: &TradeWindowInputs<'_>,
    split: bool,
    feed: bool,
) {
    let (txt, col) = match inputs.last {
        Some(p) => (ladder::fmt_price(p, inputs.grid.tick_size), t.theme.text),
        None => ("—".to_string(), t.theme.text_tertiary),
    };
    let spread = match super::ticket::best(inputs) {
        (Some(b), Some(a)) => Some(ladder::fmt_price(a - b, inputs.grid.tick_size)),
        _ => None,
    };
    let price = |ui: &mut egui::Ui| {
        ui.label(RichText::new(&txt).font(t.mono_semibold(ThemeTextRole::Title)).color(col))
            .on_hover_text(if inputs.last.is_some() {
                "Mid price: halfway between this window's best bid and best ask."
            } else {
                "No price: this window's book has no bid and ask to take the mid of."
            });
    };
    let spread_w = |ui: &egui::Ui, n: &str| {
        let w = |text: &str, font: egui::FontId| {
            ui.painter().layout_no_wrap(text.to_string(), font, t.theme.text).size().x
        };
        w("Spread", t.font(ThemeTextRole::Caption))
            + t.metrics.gap / 2.0
            + w(n, t.mono(ThemeTextRole::Caption))
    };
    let draw_spread = |ui: &mut egui::Ui, n: &str| {
        ui.spacing_mut().item_spacing.x = t.metrics.gap / 2.0;
        // Right to left, so the number is laid first: the words come out before it.
        ui.label(
            RichText::new(n).font(t.mono(ThemeTextRole::Caption)).color(t.theme.text_secondary),
        );
        ui.label(
            RichText::new("Spread")
                .font(t.font(ThemeTextRole::Caption))
                .color(t.theme.text_tertiary),
        );
    };
    if split {
        price(ui);
        if feed {
            feed_badge(ui, inputs);
        }
        // What is left of the row: the spread takes it where it fits and gives way where it does
        // not (a seven-digit price at Large text leaves none).
        if let Some(n) = spread.as_deref()
            && spread_w(ui, n) + t.metrics.gap <= ui.available_width()
        {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                draw_spread(ui, n)
            });
        }
    } else {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // Right to left: the spread is laid first, against the edge, and the price lands to its
            // left; the spread is the one that gives way where the two do not fit.
            let price_w = ui
                .painter()
                .layout_no_wrap(txt.clone(), t.mono(ThemeTextRole::Title), col)
                .size()
                .x;
            if let Some(n) = spread.as_deref()
                && spread_w(ui, n) + t.metrics.gap + price_w <= ui.available_width()
            {
                draw_spread(ui, n);
                ui.add_space(t.metrics.gap);
            }
            price(ui);
        });
    }
}

/// The venue's own spelling of `venue`, from whichever row carries one (each is the settings
/// database's `venue.title`): the window's, an account's, a match's — else the key itself. The
/// widget never spells a venue of its own.
fn venue_label_of<'a>(inputs: &TradeWindowInputs<'a>, venue: &'a str) -> &'a str {
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
fn pick_symbol(
    state: &mut TradeWindowState,
    actions: &mut Vec<TradeWindowAction>,
    venue: &str,
    symbol: &str,
) {
    actions.push(TradeWindowAction::PickSymbol {
        venue: venue.to_string(),
        symbol: symbol.to_string(),
    });
    state.query.clear();
    state.hot = 0;
}

fn symbol_picker(
    ui: &mut egui::Ui,
    t: &ThemeTokens,
    state: &mut TradeWindowState,
    inputs: &TradeWindowInputs<'_>,
    actions: &mut Vec<TradeWindowAction>,
) {
    let symbol = RichText::new(inputs.symbol).font(t.font_semibold(ThemeTextRole::Strong));
    let button = ui.add(ThemeButtonAction::secondary((symbol, icons::DISCLOSE_OPEN)));
    egui::Popup::menu(&button)
        .id(ui.scope_id().with("trade_symbol_picker"))
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            // The search field, each recent pick and each row under an id of its own (`section`,
            // `keyed`, the row's own key): the Recent row and the rows come and go while the trader
            // types, and a field or a row that took its id from them would lose the focus or a held
            // press.
            let key = ui.scope_id();
            ui.set_width(trade::SYMBOL_PICKER_WIDTH);
            // The search field takes the picker's width, not egui's 280 pt default.
            ui.spacing_mut().text_edit_width = trade::SYMBOL_PICKER_WIDTH;
            let entry = section(ui, key, "search", |ui| {
                input::text(
                    ui,
                    &mut state.query,
                    input::ThemeInputField {
                        hint: "Search a symbol or a coin",
                        ..Default::default()
                    },
                )
            });
            if ui.memory(|m| m.focused().is_none()) {
                entry.request_focus();
            }
            // The design's field names its way out: a small `Esc` at the right end.
            let esc_room = entry.rect.shrink2(egui::vec2(t.metrics.padding, space::NONE));
            let mut esc = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(esc_room)
                    .layout(egui::Layout::right_to_left(egui::Align::Center)),
            );
            chip::badge(&mut esc, "Esc", ThemeStatus::Muted);
            if entry.changed() {
                state.hot = 0;
            }
            if !inputs.recent.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    // A button here MOVES to the next line when it does not fit: with egui's default
                    // wrapping it kept the width it had left and broke its WORDS instead, so the
                    // fifth recent pick drew as a column of single letters (owner's screenshot,
                    // 2026-10-04).
                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                    ui.label(
                        RichText::new("Recent")
                            .font(t.font(ThemeTextRole::Caption))
                            .color(t.theme.text_tertiary),
                    );
                    // A symbol is an address only with its venue: a recent pick moves the window
                    // to its venue too.
                    let salts = row_salts(inputs.recent, |r| *r);
                    for (&(venue, symbol), salt) in inputs.recent.iter().zip(salts) {
                        let words = format!("{symbol} · {}", venue_label_of(inputs, venue));
                        let salt = ("recent", salt);
                        if keyed(ui, key.with(salt), ThemeButtonAction::secondary(words)).clicked()
                        {
                            pick_symbol(state, actions, venue, symbol);
                            ui.close();
                        }
                    }
                });
            }
            let rows = &inputs.matches[..inputs.matches.len().min(ROWS_SHOWN)];
            let n = rows.len();
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
            list.max_height(trade::MATCHES_HEIGHT).show(ui, |ui| {
                if n == 0 {
                    ui.label(
                        RichText::new(no_match_words(&state.query))
                            .font(t.font(ThemeTextRole::Body))
                            .color(t.theme.text_secondary),
                    );
                }
                let salts = row_salts(rows, |r| (r.venue, r.symbol));
                for ((i, r), salt) in rows.iter().enumerate().zip(salts) {
                    let row = ThemeVenueChipFlatRow {
                        symbol: r.symbol,
                        pair: &r.pair,
                        venue: r.venue_label,
                        hot: i == state.hot,
                        current: r.current,
                    };
                    if flat_row(ui, (key, "row", salt), &row).clicked() {
                        pick = Some(i);
                    }
                }
            });
            // Enter opens the hot row, but only while the search field has the focus.
            if enter && n > 0 && (entry.has_focus() || entry.lost_focus()) {
                pick = Some(state.hot);
            }
            if let Some(r) = pick.map(|i| &rows[i]) {
                pick_symbol(state, actions, r.venue, r.symbol);
                ui.close();
            }
            ui.label(
                RichText::new("↑ ↓ to move · Enter to open · Esc to close")
                    .font(t.font(ThemeTextRole::Caption))
                    .color(t.theme.text_tertiary),
            );
        });
}

/// What the list says when it is empty: what to do on an empty search, else what was not found.
fn no_match_words(query: &str) -> String {
    let q = query.trim();
    if q.is_empty() {
        "Type a symbol or a coin.".to_string()
    } else {
        format!("No symbol matches \u{201C}{q}\u{201D}.")
    }
}

/// A venue and, where the catalog says, the product the instrument is on it: `Binance Perp`.
fn venue_and_product(venue_label: &str, product: &str) -> String {
    if product.is_empty() { venue_label.to_string() } else { format!("{venue_label} {product}") }
}

/// What the account button says: the venue and its product, then the account's name — the window's
/// own row names the account as the list does; without one, its label or "main".
fn account_words(inputs: &TradeWindowInputs<'_>) -> String {
    let name = inputs
        .accounts
        .iter()
        .find(|a| a.venue == inputs.venue && a.account == inputs.account)
        .map_or(
            inputs.account.unwrap_or(vike_model::accounts::account_keys::UNLABELLED_ACCOUNT_NAME),
            |a| a.name,
        );
    format!("{} · {name}", venue_and_product(inputs.venue_label, inputs.product))
}

fn account_picker(
    ui: &mut egui::Ui,
    t: &ThemeTokens,
    inputs: &TradeWindowInputs<'_>,
    actions: &mut Vec<TradeWindowAction>,
) {
    let words = account_words(inputs);
    // The v3 design's account button reads in the secondary ink: the symbol beside it is the bar's
    // one bright word.
    let words =
        RichText::new(words).font(t.font(ThemeTextRole::Body)).color(t.theme.text_secondary);
    let button = ui.add(ThemeButtonAction::secondary((words, icons::DISCLOSE_OPEN)));
    egui::Popup::menu(&button).id(ui.scope_id().with("trade_account_picker")).show(|ui| {
        // Each row under an id of its own (`section`, `keyed`): the list changes under a held press
        // — the node's directory reply arrives, an account starts running — and a row that took
        // its id from its place in the list was released as a pick of the row that took that place.
        let key = ui.scope_id();
        ui.set_width(trade::ACCOUNT_PICKER_WIDTH);
        ui.spacing_mut().item_spacing.y = space::NONE;
        ui.horizontal(|ui| {
            ui.add_space(t.metrics.padding - ui.spacing().item_spacing.x);
            ui.label(
                RichText::new(format!("{} is listed on", inputs.symbol))
                    .font(t.font(ThemeTextRole::Caption))
                    .color(t.theme.text_tertiary),
            );
        });
        ui.add_space(t.metrics.gap);
        // Why the list is not the server's whole list, as the app states it (owner, 10-03, item 5).
        if let Some(why) = inputs.accounts_why {
            ui.label(
                RichText::new(why)
                    .font(t.font(ThemeTextRole::Caption))
                    .color(t.theme.text_secondary),
            );
        }
        let salts = row_salts(inputs.accounts, |r| (r.venue, r.account, r.name));
        for (a, salt) in inputs.accounts.iter().zip(salts) {
            let chosen = a.venue == inputs.venue && a.account == inputs.account;
            let title = venue_and_product(a.venue_label, a.product);
            let mark = match a.mode {
                Some(m) => ThemeAccountMenuMark::Tier(m),
                None => ThemeAccountMenuMark::Unknown { word: MODE_UNKNOWN, why: MODE_UNKNOWN_WHY },
            };
            let row = ThemeAccountMenuRow {
                title: &title,
                account: a.name,
                opens: a.symbol,
                mark,
                current: chosen,
                why_not: a.why_not,
                lost: a.lost,
            };
            // The row opens its OWN venue's spelling of the instrument (`TradeWindowAccountPickerRow::symbol`).
            let pick = || TradeWindowAction::PickAccount {
                venue: a.venue.to_string(),
                account: a.account.map(str::to_string),
                symbol: a.symbol.to_string(),
            };
            section(ui, key, ("account", salt), |ui| {
                if account_row(ui, "row", &row).clicked() {
                    actions.push(pick());
                    ui.close();
                }
            });
        }
        let salts = row_salts(inputs.unconnected, |u| u.venue_label);
        for (u, salt) in inputs.unconnected.iter().zip(salts) {
            section(ui, key, ("connect", salt), |ui| {
                let title = venue_and_product(u.venue_label, u.product);
                // Every row's button reads "Connect", so its NAME carries the venue: without it a
                // screen reader hears the same button once per venue.
                let name = format!("Connect {}", u.venue_label);
                if connect_row(ui, "row", &title, &name).clicked() {
                    actions.push(TradeWindowAction::OpenConnections);
                    ui.close();
                }
            });
        }
        ui.add_space(t.metrics.gap);
        ui.separator();
        let manage = egui::Button::new(
            RichText::new("Manage accounts in Connections")
                .font(t.font(ThemeTextRole::Strong))
                .color(t.theme.text_secondary),
        )
        .frame(false);
        if keyed(ui, key.with("manage"), manage).clicked() {
            actions.push(TradeWindowAction::OpenConnections);
            ui.close();
        }
    });
}

#[path = "instrument_bar_tests.rs"]
#[cfg(test)]
mod instrument_bar_tests;
