//! The Account window (`docs/superpowers/specs/2026-09-30-trade-window-design.md` §3.10): the old
//! Trade window without its ticket. One column, top to bottom: the cross-venue equity hero, the
//! per-venue Accounts table and the working-orders table, whose ✕ leaves through
//! `ToolView::account_cancel`. Order ENTRY lives in the Trade window now (`trade.rs` here, the
//! widget in `vike_panels::trade`).
//!
//! History: these sections moved here verbatim from `vike-app`'s CI-excluded `main.rs` (tool-view
//! extraction batch 3, audit F1), and the decision helpers the render code used to inline
//! ([`hero_totals`], [`unpriced_tooltip`]) became named, pure functions with unit tests, the POINT
//! of that move. The read-only `snap` arrives through [`ToolContext`]; `&mut ToolView` stays a separate
//! param so the body keeps writing its OUT intent.
//!
//! ⚠ Up and down are the chosen market-colour set since the design system's PR 4 — text in the
//! set's text colours, fills in its graphic colours.

use super::ToolContext;
use crate::backend::node_words::{AccountLink, NodeWords};
use crate::{orders::equity_panel, tools};
use vike_model::AccountTier;
use vike_ui_theme::components::table::{Column, column_widths};
use vike_ui_theme::components::venue_chip::chip_pad;
use vike_ui_theme::components::{ON_FILL, Status, Tokens, role_px};
use vike_ui_theme::icons;
use vike_ui_theme::metrics::{space, stroke};
use vike_ui_theme::side::Pair;
use vike_ui_theme::type_scale::TextRole;
use vike_ui_theme::value::account;
use vike_utility::math::fmt_number;

/// The cross-venue Realized / Fees / Funding aggregates the equity hero's stat strip shows —
/// plain naive folds over the panel rows, in row order (matching every other display sum here).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct HeroTotals {
    pub realized: f64,
    pub fees: f64,
    pub funding: f64,
}

/// Sum the per-venue rows into the hero's [`HeroTotals`]. Pure; empty rows ⇒ all-zero (which the
/// hero renders as a `+0.0000` P&L chip, not as a blank).
///
/// ONE currency: the rows of the `primary` group (the node's, as the headline `equity_total` is:
/// `EquityPanelModel::primary`, which the frame states and which is NOT the first row's after a primary
/// restart); a row in another currency is not added to it (decision 0183, item 13) and shows as its own
/// [`equity_panel::LedgerHeadline`]. With every row in one group, which is every node today, it adds
/// every row.
pub fn hero_totals(
    rows: &[equity_panel::EquityRow],
    primary: vike_execution::Interned,
) -> HeroTotals {
    let group = || rows.iter().filter(move |r| r.currency == primary);
    HeroTotals {
        realized: group().map(|r| r.realized).sum(),
        fees: group().map(|r| r.fees).sum(),
        funding: group().map(|r| r.funding).sum(),
    }
}

/// The Accounts row's unpriced-badge hover text: the comma-joined symbols of the positions of the
/// engine `route_key` (`EquityRow::route_key`) that carry no `mark_source`. Derived here because
/// `EquityRow` carries only the COUNT (the T4 model is display-thin). Falls back to a generic label
/// when no block has the key or every position is priced.
///
/// ⚠ The key, never the venue name: a demo and a LIVE bybit are two blocks named `bybit`, and a lookup
/// by name gave the second row the first account's positions.
pub fn unpriced_tooltip(snap: &vike_execution::CoreSnapshot, route_key: &str) -> String {
    snap.portfolio
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

/// Right-aligned fixed-width monospace numeric cell — the aligned-table idiom (mirrors the
/// Calendar table's `allocate_ui_with_layout` columns). Used by the Account window's venue +
/// orders tables so numbers line up on the decimal.
fn number_cell(ui: &mut egui::Ui, w: f32, txt: String, col: egui::Color32) {
    use egui::{Align, Label, Layout, RichText, vec2};
    let cell = vec2(w, account::CELL_H);
    ui.allocate_ui_with_layout(cell, Layout::right_to_left(Align::Center), |ui| {
        ui.set_min_width(w);
        ui.add(
            Label::new(
                RichText::new(txt).monospace().size(role_px(ui.ctx(), TextRole::Strong)).color(col),
            )
            .truncate(),
        );
    });
}

/// Left-aligned fixed-width text cell (venue-name / side columns of the Account tables).
fn text_cell(ui: &mut egui::Ui, w: f32, rt: egui::RichText) {
    use egui::{Align, Label, Layout, vec2};
    let cell = vec2(w, account::CELL_H);
    ui.allocate_ui_with_layout(cell, Layout::left_to_right(Align::Center), |ui| {
        ui.set_min_width(w);
        ui.add(Label::new(rt).truncate());
    });
}

/// How wide `rt` lays out unwrapped, the way [`text_cell`] and [`number_cell`] draw it, plus a point so a
/// cell sized from it never elides it (the layout rounds glyph edges up). The Account tables' columns
/// are sized from this, never from a guess about the longest word: a fixed cell cut `1006910.45` to
/// `1006910…` and `hyperliquid` to `hyperliq…`.
fn text_w(ui: &egui::Ui, rt: egui::RichText) -> f32 {
    egui::WidgetText::from(rt)
        .into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Body)
        .size()
        .x
        .ceil()
        + 1.0
}

/// The word an account's chip says when the node did not say its tier, and what hovering it says:
/// the Trade window's own (`crates/vike-panels/src/trade/instrument.rs`'s `MODE_UNKNOWN`), so the one
/// account reads the same in both windows. Never PAPER: an unknown tier may be real money.
const TIER_UNKNOWN: &str = "MODE ?";
const TIER_UNKNOWN_WHY: &str = "The backend does not say whether this account is paper, demo or \
                                live. This window treats the account as live.";

/// An account's tier as the theme's own chip (LIVE filled in the accent, DEMO outlined in the info
/// blue, PAPER in the border grey), or [`TIER_UNKNOWN`] in the muted outline.
fn tier_mark(ui: &mut egui::Ui, tier: Option<AccountTier>) {
    match tier {
        Some(tier) => {
            vike_ui_theme::components::chip::tier(ui, tier);
        }
        None => {
            vike_ui_theme::components::chip::badge(ui, TIER_UNKNOWN, Status::Muted)
                .on_hover_text(TIER_UNKNOWN_WHY);
        }
    }
}

/// What the Link column's badge says for an account that runs, in words the frame does not carry.
const LINK_NOT_RUNNING: &str = "NOT RUNNING";
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
/// beside the block's OLD one); then the frame's links ([`NodeWords::link_of`]). A paper account has
/// no link to lose.
fn link_cell<'a>(
    snap: &vike_execution::CoreSnapshot,
    words: &'a NodeWords,
    route_key: &str,
    tier: Option<AccountTier>,
) -> (&'a str, Status, String) {
    if let Some(n) = snap.not_running.iter().find(|n| n.route_key() == route_key) {
        return (LINK_NOT_RUNNING, Status::Error, super::trade::not_running_because(&n.reason));
    }
    if tier == Some(AccountTier::Paper) {
        return (LINK_NONE, Status::Muted, LINK_NONE_WHY.to_string());
    }
    match words.link_of(route_key) {
        AccountLink::Lost(lost) => (lost.word, Status::Error, lost.why.clone()),
        AccountLink::Up => (LINK_UP, Status::Ok, LINK_UP_WHY.to_string()),
        AccountLink::NotKnown => (LINK_UNKNOWN, Status::Muted, LINK_UNKNOWN_WHY.to_string()),
    }
}

/// The accounts the node says it does not run and publishes no engine for: nothing else in the table
/// would name them, and an account it lists but no row shows reads as one that does not exist.
fn engineless(
    snap: &vike_execution::CoreSnapshot,
) -> impl Iterator<Item = &vike_execution::ArmingNotRunningView> {
    snap.not_running.iter().filter(move |n| {
        let key = n.route_key();
        !snap.portfolio.venues.iter().any(|b| b.route_key == key)
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
            .size(role_px(ui.ctx(), TextRole::Body))
            .color(ON_FILL)
            .background_color(Status::Warning.color()),
    )
}

/// The four numbers of an Accounts row, in column order, each with the colour it is drawn in.
fn account_numbers(row: &equity_panel::EquityRow, t: &Tokens) -> [(String, egui::Color32); 4] {
    [
        (format!("{:.2}", row.equity), t.theme.text),
        (format!("{:+.4}", row.unrealized), Pair::GainLoss.text(row.unrealized >= 0.0, t)),
        (format!("{:.4}", row.fees), t.theme.text2),
        (format!("{:+.4}", row.funding), Pair::GainLoss.text(row.funding >= 0.0, t)),
    ]
}

/// The cross-venue equity HERO (full-width, top of the Account window): big `equity_total`, a P&L
/// chip (summed venue realized), the unpriced badge, and a Realized/Fees/Funding stat strip.
/// Replaces the old four account cards, which duplicated the primary venue's table row.
fn equity_hero(ui: &mut egui::Ui, model: &equity_panel::EquityPanelModel, margin_used_total: f64) {
    use egui::{Color32, RichText};

    let t = Tokens::of(ui.ctx());
    let HeroTotals { realized, fees, funding } = hero_totals(&model.rows, model.primary);

    // A node whose engines are in several currencies says which one the headline is (the primary's);
    // a node of one currency keeps the title it always had.
    let title = if model.other_ledgers.is_empty() {
        "CROSS-VENUE EQUITY".to_string()
    } else {
        format!("CROSS-VENUE EQUITY ({})", model.currency)
    };
    ui.label(
        RichText::new(title)
            .size(role_px(ui.ctx(), TextRole::Caption))
            .color(t.theme.text2)
            .strong(),
    );
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!("{:.2}", model.equity_total))
                .size(role_px(ui.ctx(), TextRole::Display))
                .color(t.theme.text),
        );
        let (pc, ptxt) = if realized >= 0.0 {
            (Pair::GainLoss.text(true, &t), format!("P&L +{realized:.2}"))
        } else {
            (Pair::GainLoss.text(false, &t), format!("P&L {realized:.2}"))
        };
        ui.label(RichText::new(ptxt).size(role_px(ui.ctx(), TextRole::Body)).color(pc).monospace());
        if model.missing_prices_total > 0 {
            ui.label(
                icons::WARNING.before(
                    ui.style(),
                    RichText::new(format!("{} unpriced", model.missing_prices_total))
                        .size(role_px(ui.ctx(), TextRole::Body))
                        .color(ON_FILL)
                        .background_color(Status::Warning.color()),
                ),
            );
        }
    });

    // stat strip: cross-venue Realized · Fees · Funding aggregates
    ui.add_space(space::LG);
    let stat = |ui: &mut egui::Ui, k: &str, v: String, col: Color32| {
        ui.vertical(|ui| {
            ui.label(
                RichText::new(k).size(role_px(ui.ctx(), TextRole::Caption)).color(t.theme.text2),
            );
            ui.label(
                RichText::new(v).size(role_px(ui.ctx(), TextRole::Title)).color(col).monospace(),
            );
        });
    };
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::XL3;
        stat(ui, "Realized", format!("{realized:+.4}"), Pair::GainLoss.text(realized >= 0.0, &t));
        stat(ui, "Fees", format!("{fees:.4}"), t.theme.text2);
        stat(ui, "Funding", format!("{funding:+.4}"), Pair::GainLoss.text(funding >= 0.0, &t));
        if margin_used_total > 0.0 {
            stat(ui, "Margin used", format!("{margin_used_total:.2}"), t.theme.text2);
        }
    });

    // Every OTHER currency group, beside the headline and never added to it (decision 0183, item 13):
    // one line each, in the group's own code. Nothing is drawn for a node of one currency.
    for other in &model.other_ledgers {
        ui.add_space(space::MD);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = space::XL3;
            ui.label(
                RichText::new(format!("{} {:.2}", other.currency, other.equity_total))
                    .size(role_px(ui.ctx(), TextRole::Title))
                    .color(t.theme.text)
                    .monospace(),
            );
            stat(
                ui,
                "Realized",
                format!("{:+.4}", other.realized),
                Pair::GainLoss.text(other.realized >= 0.0, &t),
            );
            stat(ui, "Fees", format!("{:.4}", other.fees), t.theme.text2);
            stat(
                ui,
                "Funding",
                format!("{:+.4}", other.funding),
                Pair::GainLoss.text(other.funding >= 0.0, &t),
            );
            if other.margin_used > 0.0 {
                stat(ui, "Margin used", format!("{:.2}", other.margin_used), t.theme.text2);
            }
        });
    }
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
/// window's spare width, up to the cap `account_tool_content` sets, is shared by weight
/// (`vike_ui_theme::components::table::column_widths`). A table whose columns need more than the
/// window has is wider than it, never cut.
fn accounts_section(
    ui: &mut egui::Ui,
    snap: &vike_execution::CoreSnapshot,
    model: &equity_panel::EquityPanelModel,
    words: &NodeWords,
) {
    use egui::{Align, Label, Layout, RichText, vec2};

    let t = Tokens::of(ui.ctx());
    let (strong, caption) =
        (role_px(ui.ctx(), TextRole::Strong), role_px(ui.ctx(), TextRole::Caption));
    ui.label(RichText::new("Accounts").size(strong).color(t.theme.text).strong());
    ui.add_space(space::SM);

    let head_text = |h: &str| RichText::new(h).size(caption).color(t.theme.text3);
    // Each column's least width: its token width, or the widest thing it holds, the header too.
    let mut least = [
        account::WV,
        0.0,
        0.0,
        account::WM,
        account::WN,
        account::WN,
        account::WN,
        account::WN,
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
    for n in engineless(snap) {
        least[0] = least[0].max(text_w(ui, RichText::new(engineless_venue(n)).size(strong)));
        least[9] = least[9].max(badge_w(ui, LINK_NOT_RUNNING));
    }
    for row in &model.rows {
        let (word, _, _) = link_cell(snap, words, &row.route_key, row.tier);
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
    let columns: Vec<Column<'_>> = ACCOUNT_HEADS
        .into_iter()
        .zip(least)
        .zip(weights)
        .enumerate()
        .map(|(i, ((title, min_w), weight))| Column {
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
                    vec2(*w, account::HEAD_CELL_H),
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

    if model.rows.is_empty() && engineless(snap).next().is_none() {
        ui.label(RichText::new("no venues").color(t.theme.text2));
    }
    // The Link cell of a row, the column's last.
    let link = |ui: &mut egui::Ui, w: f32, (word, status, why): (&str, Status, String)| {
        ui.allocate_ui_with_layout(
            vec2(w, account::CELL_H),
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
            text_cell(ui, widths[1], RichText::new(&row.account).size(strong).color(t.theme.text2));
            ui.allocate_ui_with_layout(
                vec2(widths[2], account::CELL_H),
                Layout::left_to_right(Align::Center),
                |ui| {
                    // A cell takes the room its CONTENT used unless told a width: a chip is narrower
                    // than the column, and every cell after it would slide left with it.
                    ui.set_min_width(widths[2]);
                    tier_mark(ui, row.tier);
                },
            );
            text_cell(ui, widths[3], RichText::new(&row.mode).size(caption).color(t.theme.text3));
            for ((txt, col), w) in account_numbers(row, &t).into_iter().zip(&widths[4..8]) {
                number_cell(ui, *w, txt, col);
            }
            // Allocated with or without a badge, so the Link column after it stays in register.
            ui.allocate_ui_with_layout(
                vec2(widths[8], account::CELL_H),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.set_min_width(widths[8]);
                    if row.missing_prices > 0 {
                        ui.label(unpriced_badge(ui, row.missing_prices))
                            .on_hover_text(unpriced_tooltip(snap, &row.route_key));
                    }
                },
            );
            link(ui, widths[9], link_cell(snap, words, &row.route_key, row.tier));
        });
    }
    for n in engineless(snap) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = space::MD;
            let venue = engineless_venue(n);
            text_cell(ui, widths[0], RichText::new(venue).size(strong).color(t.theme.text3));
            let account = n.account.label_text().unwrap_or("main");
            text_cell(ui, widths[1], RichText::new(account).size(strong).color(t.theme.text3));
            ui.allocate_ui_with_layout(
                vec2(widths[2], account::CELL_H),
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
            link(ui, widths[9], link_cell(snap, words, &n.route_key(), n.tier));
        });
    }

    if !snap.portfolio.balances_by_asset.is_empty() {
        ui.add_space(space::MD);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = space::XL;
            for (asset, qty) in &snap.portfolio.balances_by_asset {
                ui.label(
                    RichText::new(format!("{asset} {qty:.4}"))
                        .size(role_px(ui.ctx(), TextRole::Body))
                        .color(t.theme.text2)
                        .monospace(),
                );
            }
        });
    }
}

/// The order row's DELAY badge (decision 0140): `DELAYED` / `FROZEN` when the node recorded that the
/// order was priced off data that was not real-time, `None` for a real-time or unlabelled order (no
/// badge: the ordinary case says nothing). Information, never a gate: the row and its ✕ are the
/// same either way. The trade window's status line reads the same words.
pub(crate) fn delay_badge(delay: Option<vike_model::StreamDelay>) -> Option<&'static str> {
    match delay? {
        vike_model::StreamDelay::RealTime => None,
        vike_model::StreamDelay::Delayed { .. } => Some("DELAYED"),
        vike_model::StreamDelay::Frozen => Some("FROZEN"),
    }
}

/// The venue an order rests at, with its account when the node named one (`bybit#LIVE`, the routing
/// key's own spelling). ⚠ An order with NO label is printed as the bare venue and never as `main`:
/// `OrderView::account` reads `None` for the default account AND for a node that predates the
/// label, so "main" would claim an account the node did not say.
fn order_venue(o: &vike_execution::OrderView) -> String {
    match o.account.as_ref().and_then(|a| a.text()) {
        Some(label) => format!("{}#{label}", o.venue),
        None => o.venue.clone(),
    }
}

/// The friendly words of a v2 order id (decision 0270's 3.6, `describe_order_client_id`), with the names this
/// frame holds: the account's label off the block whose account code the id carries (`(no label)` for
/// the default account), the strategy's name off the mount whose code it carries. `None` for a v1 or a
/// foreign id, which shows as it always did, alone. The Trade window's ladder hover reads it too.
pub(crate) fn order_client_id_words(
    id: &str,
    snap: &vike_execution::CoreSnapshot,
    mounts: &[vike_tradehub_client::wire::MountViewWire],
) -> Option<String> {
    use vike_model::orders::client_id::{
        OrderClientIdReading, describe_order_client_id, parse_order_client_id,
    };
    let parsed = parse_order_client_id(id)?;
    let OrderClientIdReading::V2(parts) = parsed else { return None };
    let mut words = describe_order_client_id(&parsed);
    words.account_label = parts
        .account
        .and_then(|code| snap.portfolio.venues.iter().find(|b| b.account_code == Some(code)))
        .map(|b| b.account.as_ref().and_then(|l| l.text()).unwrap_or("(no label)").to_string());
    words.strategy_name = words
        .strat
        .and_then(|code| {
            mounts.iter().find(|m| m.order_client_id_code.as_deref() == Some(code.as_str()))
        })
        .map(|m| m.strategy.clone());
    Some(words.to_string())
}

/// The working-orders section: an aligned table (Venue · Symbol · Side · Qty · Type · Status · delay
/// badge · ✕ · order id · its words) with a colored side chip and cancel on non-terminal orders.
///
/// The columns are sized from what they hold ([`text_w`]); the order id, then its friendly words
/// ([`order_client_id_words`], a v2 id only), are free-running labels that take what is left of the row and carry
/// their whole text on hover, so the ✕ before them stays on screen however narrow the window is.
fn orders_section(
    ui: &mut egui::Ui,
    snap: &vike_execution::CoreSnapshot,
    mounts: &[vike_tradehub_client::wire::MountViewWire],
    tv: &mut tools::ToolView,
) {
    use egui::{Label, RichText};

    let t = Tokens::of(ui.ctx());
    ui.label(
        RichText::new(format!("Orders ({})", snap.orders.len()))
            .size(role_px(ui.ctx(), TextRole::Strong))
            .color(t.theme.text)
            .strong(),
    );
    ui.add_space(space::SM);
    if snap.orders.is_empty() {
        ui.label(
            RichText::new("No working orders.")
                .size(role_px(ui.ctx(), TextRole::Strong))
                .color(t.theme.text3),
        );
        return;
    }
    // The text cells of an order's row, in column order, and the qty (a number) beside them.
    let body = role_px(ui.ctx(), TextRole::Body);
    let words = |o: &vike_execution::OrderView| {
        let sc = Pair::BuySell.text(o.side > 0, &t);
        let side = if o.side > 0 { "BUY" } else { "SELL" };
        [
            RichText::new(order_venue(o)).size(body).color(t.theme.text),
            RichText::new(&o.symbol).size(body).color(t.theme.text),
            RichText::new(side).size(body).color(sc).strong(),
            RichText::new(o.order_type.as_str()).size(body).color(t.theme.text2),
            RichText::new(o.status.as_str()).size(body).color(t.theme.text2),
        ]
    };
    // venue, symbol, side, qty, type, status: each at its token width or the widest thing it holds.
    let mut w = [
        account::WV,
        0.0,
        account::ORDER_W_SIDE,
        account::ORDER_W_QTY,
        account::ORDER_W_TYPE,
        account::ORDER_W_STATUS,
    ];
    let strong = role_px(ui.ctx(), TextRole::Strong);
    for o in &snap.orders {
        let [venue, symbol, side, kind, status] = words(o);
        for (at, rt) in [(0, venue), (1, symbol), (2, side), (4, kind), (5, status)] {
            w[at] = w[at].max(text_w(ui, rt));
        }
        let qty = RichText::new(fmt_number(o.qty)).monospace().size(strong);
        w[3] = w[3].max(text_w(ui, qty));
    }
    let list_h = account::ORDERS_MAX_H;
    egui::ScrollArea::vertical().max_height(list_h).auto_shrink([false, false]).show(ui, |ui| {
        for o in &snap.orders {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = space::LG;
                let [venue, symbol, side, kind, status] = words(o);
                text_cell(ui, w[0], venue);
                text_cell(ui, w[1], symbol);
                text_cell(ui, w[2], side);
                number_cell(ui, w[3], fmt_number(o.qty), t.theme.text);
                text_cell(ui, w[4], kind);
                text_cell(ui, w[5], status);
                // Priced off delayed or frozen data (decision 0140): shown, never a gate.
                if let Some(word) = delay_badge(o.data_delay) {
                    vike_ui_theme::components::chip::badge(ui, word, Status::Warning);
                }
                // A working order keeps its ✕: `is_live` is false for exactly the five terminals and
                // LIQUIDATED, the set this table has always treated as done.
                if o.status.is_live()
                    && icons::named(ui.small_button(icons::CANCEL), "Cancel order").clicked()
                {
                    tv.account_cancel = Some(o.order_client_id.clone());
                }
                // The order client id, last: it names the order to the venue, the journal and the CLI.
                ui.add(
                    Label::new(
                        RichText::new(&o.order_client_id)
                            .monospace()
                            .size(role_px(ui.ctx(), TextRole::Caption))
                            .color(t.theme.text3),
                    )
                    .truncate(),
                )
                .on_hover_text(&o.order_client_id);
                if let Some(words) = order_client_id_words(&o.order_client_id, snap, mounts) {
                    ui.add(
                        Label::new(
                            RichText::new(&words)
                                .size(role_px(ui.ctx(), TextRole::Caption))
                                .color(t.theme.text3),
                        )
                        .truncate(),
                    )
                    .on_hover_text(&words);
                }
            });
        }
    });
}

/// The EMULATED orders the node holds or works itself (`vike_execution::CoreSnapshot::emulated_orders`, decision 0275),
/// each ONCE, with its kind as a badge: kind · venue · symbol · side · qty · id · what it waits on. A working kind's
/// live child is a row of the orders above. Draws nothing for a node that emulates none, so that window is as before.
fn emulated_section(ui: &mut egui::Ui, snap: &vike_execution::CoreSnapshot) {
    use egui::{Label, RichText};

    if snap.emulated_orders.is_empty() {
        return;
    }
    let t = Tokens::of(ui.ctx());
    ui.add_space(space::LG);
    ui.label(
        RichText::new(format!("Emulated by the node ({})", snap.emulated_orders.len()))
            .size(role_px(ui.ctx(), TextRole::Strong))
            .color(t.theme.text)
            .strong(),
    );
    ui.add_space(space::SM);
    let body = role_px(ui.ctx(), TextRole::Body);
    let caption = role_px(ui.ctx(), TextRole::Caption);
    for e in &snap.emulated_orders {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = space::LG;
            vike_ui_theme::components::chip::badge(ui, e.kind, Status::Info);
            let venue = match e.account.as_ref().and_then(|a| a.text()) {
                Some(label) => format!("{}#{label}", e.venue),
                None => e.venue.clone(),
            };
            let side = if e.side > 0 { "BUY" } else { "SELL" };
            for (text, color) in [
                (venue, t.theme.text),
                (e.symbol.clone(), t.theme.text),
                (side.to_string(), Pair::BuySell.text(e.side > 0, &t)),
                (fmt_number(e.qty), t.theme.text),
            ] {
                ui.label(RichText::new(text).size(body).color(color));
            }
            // The id `cancel` names, then why it acts on nothing now when the node says.
            let mut tail = match &e.waiting {
                Some(why) => format!("{}, waiting: {why}", e.id),
                None => e.id.clone(),
            };
            // HELD is the core's own state (`OrderEmulationView::held`), never read off the waiting words.
            if e.held {
                tail.push_str(", HELD until its account's first reconcile pass");
            }
            if e.rearms > 0 {
                tail.push_str(&format!(", re-armed {}x", e.rearms));
            }
            ui.add(Label::new(RichText::new(&tail).size(caption).color(t.theme.text3)).truncate())
                .on_hover_text(&tail);
        });
    }
}

/// The Account window: cross-venue equity, then per-venue accounts, then working orders, in one
/// column. Reads the core's lossy `CoreSnapshot` and writes a working order's ✕ into
/// `tv.account_cancel`, which the shell hands to the dispatcher.
pub fn account_tool_content(ui: &mut egui::Ui, ctx: &ToolContext<'_>, tv: &mut tools::ToolView) {
    let snap = ctx.snap;
    // cap the composed content width so a maximized window doesn't stretch the tables across the
    // whole screen; a normal-sized Account window (< cap) is unaffected.
    ui.set_max_width(ui.available_width().min(account::MAX_W));

    let model = equity_panel::EquityPanelModel::from_snapshot(snap);

    equity_hero(ui, &model, snap.portfolio.margin_used_total);
    ui.add_space(space::XL);
    accounts_section(ui, snap, &model, ctx.node_words);
    strategies_section(ui, ctx.node_words);
    rates_section(ui, &model);
    ui.add_space(space::XL2);
    orders_section(ui, snap, &ctx.node_words.mounts, tv);
    emulated_section(ui, snap);
}

/// The account a mount trades, as the book an operator types; the bare venue from a node that predates
/// the field (`MountViewWire::route_key`), which names no account and is not read as the default one.
fn mount_book(m: &vike_tradehub_client::wire::MountViewWire) -> String {
    if m.route_key.is_empty() {
        m.venue.clone()
    } else {
        vike_tradehub_client::wire::book_of_route_key(&m.venue, &m.route_key)
    }
}

/// The Strategies section: what the node runs, one row per mount: its name, the account it trades, what
/// on, and the tier that account runs at NOW (`MountViewWire::tier`). ⚠ Never `MountViewWire::live`, which
/// reads `true` for a demo account: from a node that does not say the tier, the chip says `MODE ?`.
/// Draws nothing while the node has answered with no row, so a window with none is what it was.
fn strategies_section(ui: &mut egui::Ui, words: &NodeWords) {
    use egui::RichText;

    if words.mounts.is_empty() {
        return;
    }
    let t = Tokens::of(ui.ctx());
    let strong = role_px(ui.ctx(), TextRole::Strong);
    let body = role_px(ui.ctx(), TextRole::Body);
    ui.add_space(space::LG);
    ui.label(RichText::new("Strategies").size(strong).color(t.theme.text).strong());
    ui.add_space(space::SM);
    let cells = |m: &vike_tradehub_client::wire::MountViewWire| {
        [
            RichText::new(&m.strategy).size(body).color(t.theme.text),
            RichText::new(mount_book(m)).size(body).color(t.theme.text2),
            RichText::new(format!("{} {}", m.symbol, m.interval)).size(body).color(t.theme.text2),
        ]
    };
    let mut w = [0.0_f32; 3];
    for m in &words.mounts {
        for (at, cell) in w.iter_mut().zip(cells(m)) {
            *at = at.max(text_w(ui, cell));
        }
    }
    for m in &words.mounts {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = space::LG;
            for (at, cell) in w.iter().zip(cells(m)) {
                text_cell(ui, *at, cell);
            }
            tier_mark(ui, crate::backend::observe_bridge::map_tier(m.tier.as_deref()));
            if m.not_running {
                vike_ui_theme::components::chip::badge(ui, LINK_NOT_RUNNING, Status::Error)
                    .on_hover_text(MOUNT_NOT_RUNNING_WHY);
            }
            if m.latched {
                vike_ui_theme::components::chip::badge(ui, "BUDGET SPENT", Status::Warning)
                    .on_hover_text(MOUNT_LATCHED_WHY);
            }
        });
    }
}

/// Why a mount's row says NOT RUNNING: the Accounts table above gives the account's own reason.
const MOUNT_NOT_RUNNING_WHY: &str =
    "The node does not run this strategy's account, so every order of it is refused.";
/// Why a mount's row says BUDGET SPENT (`MountViewWire::latched`).
const MOUNT_LATCHED_WHY: &str = "This strategy breached its budget: its orders were cancelled and new \
                                 ones are discarded until `vike-cli trade strategy reset-budget` clears it.";

/// The conversion rates behind the Accounts table's figures (decision 0183), one line per rate in the model's
/// wording; a STALE one is drawn in the warning colour, since the account is valuing money at the last rate it
/// has. Draws nothing for a node with no account that holds a symbol quoted in another currency, so such a
/// window is what it was.
fn rates_section(ui: &mut egui::Ui, model: &equity_panel::EquityPanelModel) {
    use egui::RichText;

    if model.rates.is_empty() {
        return;
    }
    let t = Tokens::of(ui.ctx());
    ui.add_space(space::LG);
    ui.label(
        RichText::new("Conversion rates")
            .size(role_px(ui.ctx(), TextRole::Strong))
            .color(t.theme.text)
            .strong(),
    );
    ui.add_space(space::SM);
    for line in &model.rates {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = space::LG;
            ui.label(
                RichText::new(&line.account)
                    .size(role_px(ui.ctx(), TextRole::Body))
                    .color(t.theme.text2),
            );
            let colour = if line.stale { Status::Warning.color() } else { t.theme.text };
            ui.label(
                RichText::new(&line.phrase)
                    .size(role_px(ui.ctx(), TextRole::Body))
                    .color(colour)
                    .monospace(),
            );
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_execution::{BalanceMode, CoreSnapshot, PositionView, PriceSource, VenueBlock};
    use vike_model::TradingState;

    /// **A NOT RUNNING row of a venue this build's roster does not have is its own line, `unknown venue kalshi`**
    /// (owner ruling 2026-11-12, variant 2B): kept as the node named it, never dropped and never a venue this desktop
    /// trades. A known venue reads as itself, and its label as the node sent it.
    #[test]
    fn an_engineless_row_of_a_venue_this_build_does_not_know_says_so() {
        let row = |venue: &str, label: Option<&str>| vike_execution::ArmingNotRunningView {
            account: vike_model::accounts::account_keys::AccountNamed::from_wire(venue, label),
            tier: None,
            reason: "no credentials".to_string(),
            block: None,
        };
        let kalshi = row("kalshi", Some("main"));
        assert_eq!(engineless_venue(&kalshi), "unknown venue kalshi");
        assert_eq!(kalshi.account.label_text(), Some("main"));
        assert_eq!(engineless_venue(&row("okx", None)), "okx");
        let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
        snap.not_running = vec![kalshi];
        assert_eq!(engineless(&snap).count(), 1, "no engine of it, so it is listed");
    }

    fn row(venue: &str, realized: f64, fees: f64, funding: f64) -> equity_panel::EquityRow {
        equity_panel::EquityRow {
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

    fn position(symbol: &str, mark_source: Option<PriceSource>) -> PositionView {
        PositionView {
            venue: "binance".into(),
            symbol: symbol.into(),
            position_side: vike_model::events::PositionSide::Both,
            size: 1.0,
            average_price: 100.0,
            unrealized: 0.0,
            mark_source,
            leverage: 0.0,
            liquidation_price: 0.0,
            margin_mode: vike_model::MarginMode::Cross,
            isolated_margin: None,
        }
    }

    fn venue_block(venue: &str, positions: Vec<PositionView>) -> VenueBlock {
        VenueBlock {
            account_code: None,
            order_client_id_shape: None,
            venue: venue.to_string(),
            account: None,
            route_key: venue.to_string(),
            symbol: String::new(),
            extra_symbols: Default::default(),
            tier: None,
            balance: 0.0,
            realized_pnl: 0.0,
            fees_paid: 0.0,
            funding_paid: 0.0,
            balance_mode: BalanceMode::Delta,
            contract_values: Default::default(),
            contract_value_default: vike_model::ContractValue::default(),
            admits_symbols: false,
            quote_rates: Vec::new(),
            quote_rates_in: Default::default(),
            ledger_currency: Default::default(),
            currency_assumed: false,
            colliding_fills_refused: 0,
            fees_unconverted: 0,
            fees_at_stale_rate: 0,
            equity: 0.0,
            unrealized: 0.0,
            missing_prices: 0,
            margin_used: 0.0,
            free_buying_power: 0.0,
            margin_ratio: 0.0,
            fee_schedule: None,
            trading_state: TradingState::Active,
            positions,
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
        assert_eq!(hero_totals(&[], Default::default()), HeroTotals::default());
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

    #[test]
    fn unpriced_tooltip_lists_only_the_positions_with_no_mark_source() {
        let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
        snap.portfolio.venues = vec![venue_block(
            "binance",
            vec![
                position("BTCUSDT", Some(PriceSource::LastTrade)),
                position("PEPEUSDT", None),
                position("WIFUSDT", None),
            ],
        )];
        assert_eq!(unpriced_tooltip(&snap, "binance"), "PEPEUSDT, WIFUSDT");
    }

    /// ⚠ Two blocks named `bybit` (a demo and a LIVE account): each key answers with its own block's
    /// positions, the second one included.
    #[test]
    fn unpriced_tooltip_follows_the_route_key_not_the_venue_name() {
        let mut snap = CoreSnapshot::empty("bybit", "BTCUSDT");
        let live = VenueBlock {
            route_key: "bybit#LIVE".to_string(),
            ..venue_block("bybit", vec![position("WIFUSDT", None)])
        };
        snap.portfolio.venues = vec![venue_block("bybit", vec![position("DOGEUSDT", None)]), live];
        assert_eq!(unpriced_tooltip(&snap, "bybit"), "DOGEUSDT");
        assert_eq!(unpriced_tooltip(&snap, "bybit#LIVE"), "WIFUSDT");
    }

    #[test]
    fn unpriced_tooltip_falls_back_when_everything_is_priced_or_the_venue_is_absent() {
        let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
        snap.portfolio.venues =
            vec![venue_block("binance", vec![position("BTCUSDT", Some(PriceSource::LastTrade))])];
        assert_eq!(unpriced_tooltip(&snap, "binance"), "unpriced positions");
        assert_eq!(unpriced_tooltip(&snap, "okx"), "unpriced positions");
    }

    /// ⚠ Each account's way to its venue reads in its own row: lost and adapter-less accounts by name,
    /// one the bridge says is up as up, one nothing speaks for as unknown and never as up, a stopped
    /// engine as NOT RUNNING whatever the frame's links say, and an account the node lists with no
    /// engine at all gets a row of its own.
    #[test]
    fn the_accounts_table_says_how_each_accounts_link_stands() {
        use crate::backend::node_words::LostAccount;
        let ctx = egui::Context::default();
        vike_ui_theme::appearance::install(&ctx, &vike_ui_theme::appearance::Appearance::default());
        let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
        snap.portfolio.venues =
            Vec::from(["binance", "bybit", "okx", "polymarket"].map(|v| VenueBlock {
                tier: Some(AccountTier::Demo),
                ..venue_block(v, Vec::new())
            }));
        snap.not_running =
            Vec::from(["okx", "deribit"].map(|v| vike_execution::ArmingNotRunningView {
                account: vike_model::accounts::account_keys::AccountNamed::of(
                    v,
                    vike_model::accounts::account_keys::AccountLabel::Default,
                ),
                tier: None,
                reason: "no credentials".to_string(),
                block: Some(vike_model::ArmingBlock::NoCredentials),
            }));
        // A tier change: okx is listed under its NEW tier beside its block's OLD one, still NOT RUNNING.
        snap.not_running[0].tier = Some(AccountTier::Live);
        let words = NodeWords {
            lost: vec![LostAccount {
                route_key: "bybit".to_string(),
                word: "NO ADAPTER",
                why: "It has none.".to_string(),
            }],
            path_up: vec!["binance".to_string()],
            ..NodeWords::default()
        };
        let model = equity_panel::EquityPanelModel::from_snapshot(&snap);
        let out =
            ctx.run_ui(egui::RawInput::default(), |ui| accounts_section(ui, &snap, &model, &words));
        let texts: Vec<(String, f32)> = out
            .shapes
            .iter()
            .filter_map(|c| match &c.shape {
                egui::Shape::Text(t) => {
                    let at = t.galley.rect.translate(t.pos.to_vec2()).center().y;
                    Some((t.galley.text().to_string(), at))
                }
                _ => None,
            })
            .collect();
        out.drop_without_applying_deltas();
        let row_of = |venue: &str| -> Vec<&str> {
            let y = texts.iter().find(|(s, _)| s == venue).expect(venue).1;
            texts.iter().filter(|(_, at)| (at - y).abs() < 4.0).map(|(s, _)| s.as_str()).collect()
        };
        assert!(row_of("binance").contains(&LINK_UP), "{texts:?}");
        assert!(row_of("bybit").contains(&"NO ADAPTER"), "{texts:?}");
        assert!(row_of("okx").contains(&LINK_NOT_RUNNING), "a stopped engine keeps its block");
        assert!(row_of("deribit").contains(&LINK_NOT_RUNNING), "no engine, and still a row");
        assert!(row_of("polymarket").contains(&LINK_UNKNOWN), "never UP without the node's word");
        assert!(!row_of("polymarket").contains(&LINK_UP));
    }

    fn mount(route_key: &str, tier: Option<&str>) -> vike_tradehub_client::wire::MountViewWire {
        vike_tradehub_client::wire::MountViewWire {
            order_client_id_code: None,
            strategy: "spread_maker".to_string(),
            params: String::new(),
            // `live` reads `true` for a demo account: it must never be what a row says.
            live: true,
            venue: "bybit".to_string(),
            symbol: "BTCUSDT".to_string(),
            interval: "1m".to_string(),
            mount_id: String::new(),
            typed_params: None,
            asset_class: None,
            latched: false,
            route_key: route_key.to_string(),
            tier: tier.map(str::to_string),
            not_running: false,
        }
    }

    /// Every text a frame of the Strategies section paints.
    fn strategy_texts(words: &NodeWords) -> Vec<String> {
        let ctx = egui::Context::default();
        vike_ui_theme::appearance::install(&ctx, &vike_ui_theme::appearance::Appearance::default());
        let out = ctx.run_ui(egui::RawInput::default(), |ui| strategies_section(ui, words));
        let texts = out
            .shapes
            .iter()
            .filter_map(|c| match &c.shape {
                egui::Shape::Text(t) => Some(t.galley.text().to_string()),
                _ => None,
            })
            .collect();
        out.drop_without_applying_deltas();
        texts
    }

    /// ⚠ A demo account's strategy never reads LIVE, whatever its `live` flag says: a row names the
    /// book and the tier the account runs at, and NOT RUNNING and a spent budget where they stand.
    #[test]
    fn a_mount_row_names_its_account_and_real_tier_never_a_bare_live() {
        let mut stopped = mount("bybit", Some("live"));
        stopped.not_running = true;
        let mut spent = mount("bybit#SUB", Some("demo"));
        spent.latched = true;
        let words =
            NodeWords { mounts: vec![mount("bybit#DEMO", Some("demo"))], ..NodeWords::default() };
        let texts = strategy_texts(&words);
        for said in ["Strategies", "spread_maker", "bybit/DEMO", "BTCUSDT 1m", "DEMO"] {
            assert!(texts.iter().any(|t| t == said), "{said}: {texts:?}");
        }
        assert!(!texts.iter().any(|t| t == "LIVE"), "{texts:?}");
        let texts =
            strategy_texts(&NodeWords { mounts: vec![stopped, spent], ..NodeWords::default() });
        for said in ["bybit/DEFAULT", "LIVE", LINK_NOT_RUNNING, "bybit/SUB", "BUDGET SPENT"] {
            assert!(texts.iter().any(|t| t == said), "{said}: {texts:?}");
        }
    }

    /// A node that predates the fields says no tier and no account: unknown, and the bare venue, never
    /// the `live` flag read as LIVE and never `DEFAULT`, which it did not say.
    #[test]
    fn an_older_nodes_mount_row_says_what_it_does_not_know() {
        let words = NodeWords { mounts: vec![mount("", None)], ..NodeWords::default() };
        let texts = strategy_texts(&words);
        for said in ["bybit", TIER_UNKNOWN] {
            assert!(texts.iter().any(|t| t == said), "{said}: {texts:?}");
        }
        assert!(!texts.iter().any(|t| t == "LIVE" || t.contains("DEFAULT")), "{texts:?}");
        assert!(strategy_texts(NodeWords::NONE).is_empty(), "no row, no section");
    }

    /// The P&L chip's colour, read off a real frame of the equity hero under appearance `a`.
    fn pnl_colour(a: vike_ui_theme::appearance::Appearance, realized: f64) -> egui::Color32 {
        let ctx = egui::Context::default();
        vike_ui_theme::appearance::install(&ctx, &a);
        let model = equity_panel::EquityPanelModel {
            equity_total: 1000.0,
            missing_prices_total: 0,
            rows: vec![equity_panel::EquityRow {
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
        };
        let out = ctx.run_ui(egui::RawInput::default(), |ui| equity_hero(ui, &model, 0.0));
        let colour = out.shapes.iter().find_map(|c| match &c.shape {
            egui::Shape::Text(t) if t.galley.text().starts_with("P&L") => {
                Some(t.galley.job.sections[0].format.color)
            }
            _ => None,
        });
        out.drop_without_applying_deltas();
        colour.expect("the hero paints a P&L chip")
    }

    #[test]
    fn the_pnl_chip_is_the_chosen_market_sets_text_colour() {
        use vike_ui_theme::appearance::Appearance;
        use vike_ui_theme::market::{MarketColors, MarketId};
        for m in MarketId::ALL {
            let c = MarketColors::of(m);
            let a = Appearance { market: m, ..Appearance::default() };
            assert_eq!(pnl_colour(a, 12.5), c.up_text, "{m:?} profit");
            assert_eq!(pnl_colour(a, -12.5), c.down_text, "{m:?} loss");
        }
    }

    fn order_view(
        order_client_id: &str,
        side: i32,
        delay: Option<vike_model::StreamDelay>,
    ) -> vike_execution::OrderView {
        vike_execution::OrderView {
            order_client_id: order_client_id.into(),
            venue: "ibkr".into(),
            account: None,
            symbol: "AAPL.SMART.USD".into(),
            side,
            qty: 1.0,
            order_type: vike_model::OrderType::Limit,
            price: Some(190.0),
            trigger_price: None,
            status: vike_model::OrderStatus::Accepted,
            venue_order_id: None,
            filled_qty: 0.0,
            average_fill_price: 0.0,
            reason: None,
            pending_price: None,
            trail: None,
            limit_offset: None,
            attributes: None,
            trail_percent: None,
            adjust: None,
            release: None,
            display_quantity: None,
            data_delay: delay,
        }
    }

    #[test]
    fn only_a_delayed_or_frozen_order_gets_a_badge() {
        use vike_model::StreamDelay;
        assert_eq!(delay_badge(None), None);
        assert_eq!(delay_badge(Some(StreamDelay::RealTime)), None);
        assert_eq!(delay_badge(Some(StreamDelay::DELAYED)), Some("DELAYED"));
        let stated = StreamDelay::Delayed { lag_ms: Some(900_000) };
        assert_eq!(delay_badge(Some(stated)), Some("DELAYED"));
        assert_eq!(delay_badge(Some(StreamDelay::Frozen)), Some("FROZEN"));
    }

    /// The order-row LAYOUT (decision 0140), read off a real frame: the delayed order's row carries
    /// ONE `DELAYED` badge, on its own row (nearer that row than the other), right of its status;
    /// the real-time order's row carries none.
    #[test]
    fn the_delay_badge_sits_on_the_delayed_orders_row_right_of_its_status() {
        let ctx = egui::Context::default();
        vike_ui_theme::appearance::install(&ctx, &vike_ui_theme::appearance::Appearance::default());
        let mut snap = CoreSnapshot::empty("ibkr", "AAPL.SMART.USD");
        snap.orders = vec![
            order_view("d", 1, Some(vike_model::StreamDelay::DELAYED)),
            order_view("r", -1, Some(vike_model::StreamDelay::RealTime)),
        ];
        let mut tv = tools::ToolView::default();
        let out =
            ctx.run_ui(egui::RawInput::default(), |ui| orders_section(ui, &snap, &[], &mut tv));
        let texts: Vec<(String, egui::Rect)> = out
            .shapes
            .iter()
            .filter_map(|c| match &c.shape {
                egui::Shape::Text(t) => {
                    Some((t.galley.text().to_string(), t.galley.rect.translate(t.pos.to_vec2())))
                }
                _ => None,
            })
            .collect();
        out.drop_without_applying_deltas();
        let find = |w: &str| -> Vec<egui::Rect> {
            texts.iter().filter(|(s, _)| s == w).map(|(_, r)| *r).collect()
        };
        let badges = find("DELAYED");
        assert_eq!(badges.len(), 1, "one badge, for the one delayed order: {texts:?}");
        let (buy, sell) = (find("BUY")[0], find("SELL")[0]);
        assert!(buy.center().y < sell.center().y, "the delayed order is the first row");
        let dy = |a: egui::Rect, b: egui::Rect| (a.center().y - b.center().y).abs();
        let statuses = find("ACCEPTED");
        assert_eq!(statuses.len(), 2, "{texts:?}");
        let buy_status = statuses
            .iter()
            .copied()
            .min_by(|a, b| dy(buy, *a).total_cmp(&dy(buy, *b)))
            .expect("the delayed row's status");
        assert!(dy(buy, badges[0]) < dy(sell, badges[0]), "the badge is on the delayed row");
        assert!(badges[0].left() >= buy_status.right(), "right of the status");
    }

    /// Review Focus 5: the default appearance paints exactly what the old Trade window (this
    /// window, before it lost its ticket) painted before.
    #[test]
    fn the_default_trade_colours_are_todays() {
        use vike_ui_theme::appearance::Appearance;
        use vike_ui_theme::market::{MarketColors, MarketId};
        let classic = MarketColors::of(MarketId::Classic);
        assert_eq!(pnl_colour(Appearance::default(), 12.5), classic.up_text);
        assert_eq!(pnl_colour(Appearance::default(), -12.5), classic.down_text);
    }

    /// A v2 id's friendly words (decision 0270) are named from the frame: the account's label off
    /// the block with its code (`(no label)` for the default account), the strategy's name off the
    /// mount with its code. A v1 or a foreign id has none.
    #[test]
    fn a_v2_order_ids_words_name_the_account_and_the_strategy() {
        use vike_model::orders::client_id::OrderClientIdAccountCode;
        let mut snap = CoreSnapshot::empty("ibkr", "AAPL.SMART.USD");
        let demo = vike_model::accounts::account_keys::AccountLabel::parse("DEMO").ok();
        let code = |id| Some(OrderClientIdAccountCode::of_id(id));
        snap.portfolio.venues = vec![
            VenueBlock { account: demo, account_code: code(7), ..venue_block("ibkr", Vec::new()) },
            VenueBlock { account_code: code(4), ..venue_block("bybit", Vec::new()) },
        ];
        let mut maker = mount("bybit", Some("demo"));
        maker.order_client_id_code = Some("00A3".into());
        let mounts = [maker];
        assert_eq!(
            order_client_id_words("S00A307k3x9q2m7d1p4CSSLR2", &snap, &mounts).as_deref(),
            Some(
                "strategy (auto) · strategy 00A3 spread_maker · account #7 DEMO · closes short \
                 (cannot open) · stop of group k3x9q2m7d1p4 · replacement 2"
            )
        );
        assert_eq!(
            order_client_id_words("G000004k3x9q2m7d1p4OL", &snap, &[]).as_deref(),
            Some("GUI (by hand) · no strategy · account #4 (no label) · opens or adds long")
        );
        assert_eq!(
            order_client_id_words("G00000Bk3x9q2m7d1p4FL", &snap, &[]).as_deref(),
            Some(
                "GUI (by hand) · no strategy · account #11 · closes long with a plain order \
                 (may open a short)"
            ),
            "an account this frame has no block for is its number alone"
        );
        for alone in ["deadbeef17", "uicafef00d3", "ui-3-0", "EXT-1", "my hedge"] {
            assert_eq!(order_client_id_words(alone, &snap, &mounts), None, "{alone}");
        }
    }

    /// The words sit on the order's row, right of its id, and a v1 id's row carries none.
    #[test]
    fn the_words_sit_right_of_a_v2_id_and_a_v1_id_shows_alone() {
        let ctx = egui::Context::default();
        vike_ui_theme::appearance::install(&ctx, &vike_ui_theme::appearance::Appearance::default());
        let mut snap = CoreSnapshot::empty("ibkr", "AAPL.SMART.USD");
        let g = "G000004k3x9q2m7d1p4OL";
        snap.orders = vec![order_view(g, 1, None), order_view("deadbeef17", -1, None)];
        let mut tv = tools::ToolView::default();
        let out =
            ctx.run_ui(egui::RawInput::default(), |ui| orders_section(ui, &snap, &[], &mut tv));
        let texts: Vec<(String, egui::Rect)> = out
            .shapes
            .iter()
            .filter_map(|c| match &c.shape {
                egui::Shape::Text(t) => {
                    Some((t.galley.text().to_string(), t.galley.rect.translate(t.pos.to_vec2())))
                }
                _ => None,
            })
            .collect();
        out.drop_without_applying_deltas();
        let at = |w: &str| texts.iter().find(|(s, _)| s == w).map(|(_, r)| *r);
        let words = "GUI (by hand) · no strategy · account #4 · opens or adds long";
        let (id, said) = (at(g).expect("the v2 id"), at(words).expect("its words"));
        assert!(said.left() >= id.right() && (said.center().y - id.center().y).abs() < 1.0);
        assert!(at("deadbeef17").is_some(), "{texts:?}");
        assert_eq!(texts.iter().filter(|(s, _)| s.contains(" · ")).count(), 1, "{texts:?}");
    }
}
