//! The Account window's Accounts tab: one row per ACCOUNT (venue, label, tier chip, balance mode,
//! the four numbers, the unpriced badge and how its way to the venue stands), the rows of the
//! accounts the node lists but runs no engine for, and the conversion rates below the table. The
//! tier chip (`tier_mark`) and the NOT RUNNING words are read by the other tabs and the Trade
//! window too.

use crate::equity;
use crate::{number_cell, text_cell, text_w};
use vike_model::AccountTier;
use vike_tradehub_client::node_words::{BackendNodeWords, BackendNodeWordsAccountLink};
use vike_ui_theme::components::table::{ThemeTableColumn, column_widths};
use vike_ui_theme::components::venue_chip::chip_pad;
use vike_ui_theme::components::{ON_FILL, ThemeStatus, ThemeTokens, role_px};
use vike_ui_theme::icons;
use vike_ui_theme::metrics::{space, stroke};
use vike_ui_theme::side::ThemeSidePair;
use vike_ui_theme::type_scale::ThemeTextRole;
use vike_ui_theme::value::account;

/// The Accounts row's unpriced-badge hover text: the comma-joined symbols of the positions of the
/// engine `route_key` (`AccountWindowAccountsRow::route_key`) that carry no `mark_source`. Derived here because
/// `AccountWindowAccountsRow` carries only the COUNT (the T4 model is display-thin). Falls back to a generic label
/// when no block has the key or every position is priced.
///
/// ⚠ The key, never the venue name: a demo and a LIVE bybit are two blocks named `bybit`, and a lookup
/// by name gave the second row the first account's positions.
pub(crate) fn unpriced_tooltip(snapshot: &vike_execution::CoreSnapshot, route_key: &str) -> String {
    snapshot
        .portfolio
        .venues
        .iter()
        .find(|v| v.route_key == route_key)
        .map(|vb| {
            vb.positions
                .iter()
                .filter(|p| p.mark_source.is_none())
                .map(|p| p.symbol.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unpriced positions".to_string())
}

/// The word an account's chip says when the node did not say its tier, and what hovering it says:
/// the Trade window's own (`crates/vike-order-entry/src/trade_window/instrument_bar.rs`'s `MODE_UNKNOWN`), so the one
/// account reads the same in both windows. Never PAPER: an unknown tier may be real money.
pub(crate) const TIER_UNKNOWN: &str = "MODE ?";
const TIER_UNKNOWN_WHY: &str = "The backend does not say whether this account is paper, demo or \
                                live. This window treats the account as live.";

/// An account's tier as the theme's own chip (LIVE filled in the accent, DEMO outlined in the info
/// blue, PAPER in the border grey), or [`TIER_UNKNOWN`] in the muted outline.
pub(crate) fn tier_mark(ui: &mut egui::Ui, tier: Option<AccountTier>) {
    match tier {
        Some(tier) => {
            vike_ui_theme::components::chip::tier(ui, tier);
        }
        None => {
            vike_ui_theme::components::chip::badge(ui, TIER_UNKNOWN, ThemeStatus::Muted)
                .on_hover_text(TIER_UNKNOWN_WHY);
        }
    }
}

/// The Trade window's "Not running on the server." (vike-app-core's `NOT_RUNNING_WHY`) with the
/// node's own reason (`CoreSnapshot::not_running`, the `ArmingBlock` word: `no credentials`,
/// `flags.tradehub_live off`, …): an account runs at the tier its row states or not at all, and the
/// window says which stop holds it (decision 0156). The Link cell's hover says it; the Trade window
/// reads it from here.
pub fn not_running_because(reason: &str) -> String {
    format!("Not running on the server: {reason}.")
}

/// What the Link column's badge says for an account that runs, in words the frame does not carry.
pub(crate) const LINK_NOT_RUNNING: &str = "NOT RUNNING";
const LINK_UP: &str = "UP";
const LINK_UP_WHY: &str =
    "Its bridge said, on the node's last asks, that its path to the venue is up.";
const LINK_UNKNOWN: &str = "?";
const LINK_UNKNOWN_WHY: &str = "Nothing says whether this account's path to the venue is up: the \
                                bridge gave no fresh answer, or this venue's bridge reports no path. \
                                It is not reported down.";
const LINK_NONE: &str = "-";
const LINK_NONE_WHY: &str = "A paper account runs in the simulator: it has no path to a venue.";

/// One account's Link cell: the badge's word and status, and the sentence on hover. The node's list of
/// the accounts it does not run comes first, because a stopped engine keeps its block, and by route
/// key alone (`CoreSnapshotWire::stopped_row`'s rule: a tier change lists the account under its NEW tier
/// beside the block's OLD one); then the frame's links ([`BackendNodeWords::link_of`]). A paper account has
/// no link to lose.
fn link_cell<'a>(
    snapshot: &vike_execution::CoreSnapshot,
    words: &'a BackendNodeWords,
    route_key: &str,
    tier: Option<AccountTier>,
) -> (&'a str, ThemeStatus, String) {
    if let Some(n) = snapshot.not_running.iter().find(|n| n.route_key() == route_key) {
        return (LINK_NOT_RUNNING, ThemeStatus::Error, not_running_because(&n.reason));
    }
    if tier == Some(AccountTier::Paper) {
        return (LINK_NONE, ThemeStatus::Muted, LINK_NONE_WHY.to_string());
    }
    match words.link_of(route_key) {
        BackendNodeWordsAccountLink::Lost(lost) => {
            (lost.word, ThemeStatus::Error, lost.why.clone())
        }
        BackendNodeWordsAccountLink::Up => (LINK_UP, ThemeStatus::Ok, LINK_UP_WHY.to_string()),
        BackendNodeWordsAccountLink::NotKnown => {
            (LINK_UNKNOWN, ThemeStatus::Muted, LINK_UNKNOWN_WHY.to_string())
        }
    }
}

/// The accounts the node says it does not run and publishes no engine for: nothing else in the table
/// would name them, and an account it lists but no row shows reads as one that does not exist.
pub(crate) fn engineless(
    snapshot: &vike_execution::CoreSnapshot,
) -> impl Iterator<Item = &vike_execution::ArmingNotRunningView> {
    snapshot.not_running.iter().filter(move |n| {
        let key = n.route_key();
        !snapshot.portfolio.venues.iter().any(|b| b.route_key == key)
    })
}

/// The venue cell of an engineless row: the venue, or `unknown venue kalshi` for a venue this build's roster
/// does not have (a newer node's; owner ruling 2026-11-12), so the row is shown and not mistaken for one this
/// desktop can trade.
fn engineless_venue(n: &vike_execution::ArmingNotRunningView) -> String {
    let venue = n.account.venue();
    match vike_model::venues::roster_id(venue) {
        Some(_) => venue.to_string(),
        None => format!("unknown venue {venue}"),
    }
}

/// A row's unpriced badge: the warning icon and the count of positions with no price.
fn unpriced_badge(ui: &egui::Ui, n: u32) -> egui::text::LayoutJob {
    icons::WARNING.before(
        ui.style(),
        egui::RichText::new(n.to_string())
            .size(role_px(ui.ctx(), ThemeTextRole::Body))
            .color(ON_FILL)
            .background_color(ThemeStatus::Warning.color()),
    )
}

/// The four numbers of an Accounts row, in column order, each with the colour it is drawn in.
fn account_numbers(
    row: &equity::AccountWindowAccountsRow,
    t: &ThemeTokens,
) -> [(String, egui::Color32); 4] {
    [
        (format!("{:.2}", row.equity), t.theme.text),
        (format!("{:+.4}", row.unrealized), ThemeSidePair::GainLoss.text(row.unrealized >= 0.0, t)),
        (format!("{:.4}", row.fees), t.theme.text_secondary),
        (format!("{:+.4}", row.funding), ThemeSidePair::GainLoss.text(row.funding >= 0.0, t)),
    ]
}

/// The Accounts table's column headers, left to right: the three words that say WHICH account a row is
/// (venue, its label, its tier chip), the balance-mode tag (no header), the four numbers, the
/// slot of the unpriced badge (no header), and how the account's way to its venue stands.
const ACCOUNT_HEADS: [&str; 10] =
    ["Venue", "Account", "Tier", "", "Equity", "uPnL", "Fees", "Funding", "", "Link"];

/// The Accounts section: an aligned per-account table (Venue · Account · Tier · mode · Equity · uPnL ·
/// Fees · Funding) with right-aligned tabular numbers, the per-venue unpriced badge (+ hover), and a
/// per-asset balances line from `balances_by_asset`.
///
/// ⚠ A row is one ACCOUNT, not one venue: a node that runs a demo and a LIVE bybit lists two rows
/// named `bybit`, and the label and the tier chip are what tell them apart.
///
/// The columns are sized from what they hold ([`text_w`]) and never under their token width; the
/// window's spare width, up to the cap `account_window_body` sets, is shared by weight
/// (`vike_ui_theme::components::table::column_widths`). A table whose columns need more than the
/// window has is wider than it, never cut.
pub(crate) fn accounts_section(
    ui: &mut egui::Ui,
    snapshot: &vike_execution::CoreSnapshot,
    model: &equity::AccountWindowEquityModel,
    words: &BackendNodeWords,
) {
    use egui::{Align, Label, Layout, RichText, vec2};

    let t = ThemeTokens::of(ui.ctx());
    let (strong, caption) =
        (role_px(ui.ctx(), ThemeTextRole::Strong), role_px(ui.ctx(), ThemeTextRole::Caption));
    let head_text = |h: &str| RichText::new(h).size(caption).color(t.theme.text_tertiary);
    // Each column's least width: its token width, or the widest thing it holds, the header too.
    let mut least = [
        account::WIDTH_VENUE,
        0.0,
        0.0,
        account::WIDTH_MODE,
        account::WIDTH_NUMBER,
        account::WIDTH_NUMBER,
        account::WIDTH_NUMBER,
        account::WIDTH_NUMBER,
        0.0,
        0.0,
    ];
    for (l, h) in least.iter_mut().zip(ACCOUNT_HEADS) {
        *l = l.max(text_w(ui, head_text(h)));
    }
    // The chip is drawn by the theme: its widest word in the chip's mono caption face, and its
    // padding on each side, with room to spare, so it never reaches the next column.
    least[2] = least[2].max(
        text_w(ui, RichText::new(TIER_UNKNOWN).monospace().size(caption)) + 4.0 * chip_pad(&t),
    );
    // The Link badge is drawn by the theme too: as wide as its word, and its padding.
    let badge_w = |ui: &egui::Ui, word: &str| {
        text_w(ui, RichText::new(word).monospace().size(caption)) + 4.0 * chip_pad(&t)
    };
    for n in engineless(snapshot) {
        least[0] = least[0].max(text_w(ui, RichText::new(engineless_venue(n)).size(strong)));
        least[9] = least[9].max(badge_w(ui, LINK_NOT_RUNNING));
    }
    for row in &model.rows {
        let (word, _, _) = link_cell(snapshot, words, &row.route_key, row.tier);
        least[9] = least[9].max(badge_w(ui, word));
        least[0] = least[0].max(text_w(ui, RichText::new(&row.venue).size(strong)));
        least[1] = least[1].max(text_w(ui, RichText::new(&row.account).size(strong)));
        least[3] = least[3].max(text_w(ui, RichText::new(&row.mode).size(caption)));
        for (l, (txt, _)) in least[4..8].iter_mut().zip(account_numbers(row, &t)) {
            *l = l.max(text_w(ui, RichText::new(txt).monospace().size(strong)));
        }
    }
    if let Some(n) = model.rows.iter().map(|r| r.missing_prices).filter(|n| *n > 0).max() {
        least[8] = ui.painter().layout_job(unpriced_badge(ui, n)).size().x.ceil() + 1.0;
    }
    // The words take the spare width first; a number keeps its column right-aligned within its share.
    let weights = [2.0, 2.0, 1.0, 0.0, 2.0, 2.0, 2.0, 2.0, 0.0, 0.0];
    let columns: Vec<ThemeTableColumn<'_>> = ACCOUNT_HEADS
        .into_iter()
        .zip(least)
        .zip(weights)
        .enumerate()
        .map(|(i, ((title, min_w), weight))| ThemeTableColumn {
            title,
            weight,
            min_w,
            numeric: (4..8).contains(&i),
        })
        .collect();
    let gaps = space::MD * (columns.len() - 1) as f32;
    let widths = column_widths(ui.available_width() - gaps, &columns);

    // header row
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::MD;
        for (c, w) in columns.iter().zip(&widths) {
            if c.numeric {
                ui.allocate_ui_with_layout(
                    vec2(*w, account::HEADER_CELL_HEIGHT),
                    Layout::right_to_left(Align::Center),
                    |ui| {
                        ui.set_min_width(*w);
                        ui.add(Label::new(head_text(c.title)).truncate());
                    },
                );
            } else {
                text_cell(ui, *w, head_text(c.title));
            }
        }
    });
    ui.painter().hline(
        ui.min_rect().x_range(),
        ui.cursor().top(),
        egui::Stroke::new(stroke::HAIRLINE, t.theme.border),
    );
    ui.add_space(space::XS);

    if model.rows.is_empty() && engineless(snapshot).next().is_none() {
        ui.label(RichText::new("no venues").color(t.theme.text_secondary));
    }
    // The Link cell of a row, the column's last.
    let link = |ui: &mut egui::Ui, w: f32, (word, status, why): (&str, ThemeStatus, String)| {
        ui.allocate_ui_with_layout(
            vec2(w, account::CELL_HEIGHT),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.set_min_width(w);
                vike_ui_theme::components::chip::badge(ui, word, status).on_hover_text(why);
            },
        );
    };
    for row in &model.rows {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = space::MD;
            text_cell(
                ui,
                widths[0],
                RichText::new(&row.venue).size(strong).color(t.theme.text).strong(),
            );
            text_cell(
                ui,
                widths[1],
                RichText::new(&row.account).size(strong).color(t.theme.text_secondary),
            );
            ui.allocate_ui_with_layout(
                vec2(widths[2], account::CELL_HEIGHT),
                Layout::left_to_right(Align::Center),
                |ui| {
                    // A cell takes the room its CONTENT used unless told a width: a chip is narrower
                    // than the column, and every cell after it would slide left with it.
                    ui.set_min_width(widths[2]);
                    tier_mark(ui, row.tier);
                },
            );
            text_cell(
                ui,
                widths[3],
                RichText::new(&row.mode).size(caption).color(t.theme.text_tertiary),
            );
            for ((txt, col), w) in account_numbers(row, &t).into_iter().zip(&widths[4..8]) {
                number_cell(ui, *w, txt, col);
            }
            // Allocated with or without a badge, so the Link column after it stays in register.
            ui.allocate_ui_with_layout(
                vec2(widths[8], account::CELL_HEIGHT),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.set_min_width(widths[8]);
                    if row.missing_prices > 0 {
                        ui.label(unpriced_badge(ui, row.missing_prices))
                            .on_hover_text(unpriced_tooltip(snapshot, &row.route_key));
                    }
                },
            );
            link(ui, widths[9], link_cell(snapshot, words, &row.route_key, row.tier));
        });
    }
    for n in engineless(snapshot) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = space::MD;
            let venue = engineless_venue(n);
            text_cell(
                ui,
                widths[0],
                RichText::new(venue).size(strong).color(t.theme.text_tertiary),
            );
            let account = n
                .account
                .label_text()
                .unwrap_or(vike_model::accounts::account_keys::UNLABELLED_ACCOUNT_NAME);
            text_cell(
                ui,
                widths[1],
                RichText::new(account).size(strong).color(t.theme.text_tertiary),
            );
            ui.allocate_ui_with_layout(
                vec2(widths[2], account::CELL_HEIGHT),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.set_min_width(widths[2]);
                    tier_mark(ui, n.tier);
                },
            );
            // No engine, so no number: blank cells, in register with the rows above.
            for w in &widths[3..9] {
                text_cell(ui, *w, RichText::new(""));
            }
            link(ui, widths[9], link_cell(snapshot, words, &n.route_key(), n.tier));
        });
    }

    if !snapshot.portfolio.balances_by_asset.is_empty() {
        ui.add_space(space::MD);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = space::XL;
            for (asset, quantity) in &snapshot.portfolio.balances_by_asset {
                ui.label(
                    RichText::new(format!("{asset} {quantity:.4}"))
                        .size(role_px(ui.ctx(), ThemeTextRole::Body))
                        .color(t.theme.text_secondary)
                        .monospace(),
                );
            }
        });
    }
}

/// The conversion rates behind the Accounts table's figures (decision 0183), one line per rate in the model's
/// wording; a STALE one is drawn in the warning colour, since the account is valuing money at the last rate it
/// has. Draws nothing for a node with no account that holds a symbol quoted in another currency, so such a
/// window is what it was.
pub(crate) fn rates_section(ui: &mut egui::Ui, model: &equity::AccountWindowEquityModel) {
    use egui::RichText;

    if model.rates.is_empty() {
        return;
    }
    let t = ThemeTokens::of(ui.ctx());
    ui.add_space(space::LG);
    ui.label(
        RichText::new("Conversion rates")
            .size(role_px(ui.ctx(), ThemeTextRole::Strong))
            .color(t.theme.text)
            .strong(),
    );
    ui.add_space(space::SM);
    for line in &model.rates {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = space::LG;
            ui.label(
                RichText::new(&line.account)
                    .size(role_px(ui.ctx(), ThemeTextRole::Body))
                    .color(t.theme.text_secondary),
            );
            let colour = if line.stale { ThemeStatus::Warning.color() } else { t.theme.text };
            ui.label(
                RichText::new(&line.phrase)
                    .size(role_px(ui.ctx(), ThemeTextRole::Body))
                    .color(colour)
                    .monospace(),
            );
        });
    }
}

#[path = "accounts_tests.rs"]
#[cfg(test)]
mod accounts_tests;
