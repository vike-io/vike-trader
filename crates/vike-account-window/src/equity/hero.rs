//! The Account window's equity HEADER (the owner, 2026-11-17): one group per tier, each with its
//! account count and one line per currency, and the one cross-venue hero for a node that reports no
//! account's tier (`equity_header`); and the hero's stat strip sums ([`hero_totals`]), a named, pure
//! function with unit tests. It draws `super`'s `AccountWindowEquityModel` and owns no state.
//!
//! ⚠ Up and down are the chosen market-colour set since the design system's PR 4 — text in the
//! set's text colours, fills in its graphic colours.

use crate::accounts::tier_mark;
use crate::equity;
use vike_ui_theme::components::{ON_FILL, ThemeStatus, ThemeTokens, role_px};
use vike_ui_theme::icons;
use vike_ui_theme::metrics::{RADIUS, space};
use vike_ui_theme::side::ThemeSidePair;
use vike_ui_theme::type_scale::ThemeTextRole;

/// The cross-venue Realized / Fees / Funding aggregates the equity hero's stat strip shows —
/// plain naive folds over the panel rows, in row order (matching every other display sum here).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct AccountWindowEquityHeroTotals {
    pub realized: f64,
    pub fees: f64,
    pub funding: f64,
}

/// Sum the per-venue rows into the hero's [`AccountWindowEquityHeroTotals`]. Pure; empty rows ⇒ all-zero (which the
/// hero renders as a `+0.0000` P&L chip, not as a blank).
///
/// ONE currency: the rows of the `primary` group (the node's, as the headline `equity_total` is:
/// `AccountWindowEquityModel::primary`, which the frame states and which is NOT the first row's after a primary
/// restart); a row in another currency is not added to it (decision 0183, item 13) and shows as its own
/// [`equity::AccountWindowEquityLedger`]. With every row in one group, which is every node today, it adds
/// every row.
pub fn hero_totals(
    rows: &[equity::AccountWindowAccountsRow],
    primary: vike_execution::Interned,
) -> AccountWindowEquityHeroTotals {
    let group = || rows.iter().filter(move |r| r.currency == primary);
    AccountWindowEquityHeroTotals {
        realized: group().map(|r| r.realized).sum(),
        fees: group().map(|r| r.fees).sum(),
        funding: group().map(|r| r.funding).sum(),
    }
}

/// The cross-venue equity HERO, now drawn only for a node that reports no account's tier ([`equity_header`]):
/// big `equity_total`, a P&L chip (summed venue realized), the unpriced badge, and a Realized/Fees/Funding
/// stat strip. Replaces the old four account cards, which duplicated the primary venue's table row.
fn equity_hero(
    ui: &mut egui::Ui,
    model: &equity::AccountWindowEquityModel,
    margin_used_total: f64,
) {
    use egui::RichText;

    let t = ThemeTokens::of(ui.ctx());
    let AccountWindowEquityHeroTotals { realized, fees, funding } =
        hero_totals(&model.rows, model.primary);

    // A node whose engines are in several currencies says which one the headline is (the primary's);
    // a node of one currency keeps the title it always had.
    let title = if model.other_ledgers.is_empty() {
        "CROSS-VENUE EQUITY".to_string()
    } else {
        format!("CROSS-VENUE EQUITY ({})", model.currency)
    };
    ui.label(
        RichText::new(title)
            .size(role_px(ui.ctx(), ThemeTextRole::Caption))
            .color(t.theme.text_secondary)
            .strong(),
    );
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!("{:.2}", model.equity_total))
                .size(role_px(ui.ctx(), ThemeTextRole::Display))
                .color(t.theme.text),
        );
        let (pc, ptxt) = if realized >= 0.0 {
            (ThemeSidePair::GainLoss.text(true, &t), format!("P&L +{realized:.2}"))
        } else {
            (ThemeSidePair::GainLoss.text(false, &t), format!("P&L {realized:.2}"))
        };
        ui.label(
            RichText::new(ptxt).size(role_px(ui.ctx(), ThemeTextRole::Body)).color(pc).monospace(),
        );
        unpriced_total(ui, model.missing_prices_total);
    });

    // stat strip: cross-venue Realized · Fees · Funding aggregates
    ui.add_space(space::LG);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::XL3;
        stat(
            ui,
            &t,
            "Realized",
            format!("{realized:+.4}"),
            ThemeSidePair::GainLoss.text(realized >= 0.0, &t),
        );
        stat(ui, &t, "Fees", format!("{fees:.4}"), t.theme.text_secondary);
        stat(
            ui,
            &t,
            "Funding",
            format!("{funding:+.4}"),
            ThemeSidePair::GainLoss.text(funding >= 0.0, &t),
        );
        if margin_used_total > 0.0 {
            stat(ui, &t, "Margin used", format!("{margin_used_total:.2}"), t.theme.text_secondary);
        }
    });

    // Every OTHER currency group, beside the headline and never added to it (decision 0183, item 13):
    // one line each, in the group's own code. Nothing is drawn for a node of one currency.
    for other in &model.other_ledgers {
        ui.add_space(space::MD);
        ledger_line(ui, &t, other, None);
    }
}

/// The hero's badge for `n` positions with no price; nothing for none.
fn unpriced_total(ui: &mut egui::Ui, n: u32) {
    if n > 0 {
        ui.label(
            icons::WARNING.before(
                ui.style(),
                egui::RichText::new(format!("{n} unpriced"))
                    .size(role_px(ui.ctx(), ThemeTextRole::Body))
                    .color(ON_FILL)
                    .background_color(ThemeStatus::Warning.color()),
            ),
        );
    }
}

/// One figure of a stat strip: its caption over its value.
fn stat(ui: &mut egui::Ui, t: &ThemeTokens, k: &str, v: String, col: egui::Color32) {
    use egui::RichText;
    ui.vertical(|ui| {
        ui.label(
            RichText::new(k)
                .size(role_px(ui.ctx(), ThemeTextRole::Caption))
                .color(t.theme.text_secondary),
        );
        ui.label(
            RichText::new(v).size(role_px(ui.ctx(), ThemeTextRole::Title)).color(col).monospace(),
        );
    });
}

/// One currency group's line: its code and equity, then its Realized, Fees, Funding and margin in use. A tier
/// group's first line carries the group's chip and account count over the equity, as a stat's caption sits over
/// its value, so a group costs no row of its own.
fn ledger_line(
    ui: &mut egui::Ui,
    t: &ThemeTokens,
    l: &equity::AccountWindowEquityLedger,
    group: Option<&equity::AccountWindowEquityTier>,
) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::XL3;
        let equity = egui::RichText::new(format!("{} {:.2}", l.currency, l.equity_total))
            .size(role_px(ui.ctx(), ThemeTextRole::Title))
            .color(t.theme.text)
            .monospace();
        match group {
            None => {
                ui.label(equity);
            }
            Some(g) => {
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = space::SM;
                        tier_mark(ui, g.tier);
                        ui.label(
                            egui::RichText::new(accounts_word(g.accounts))
                                .size(role_px(ui.ctx(), ThemeTextRole::Caption))
                                .color(t.theme.text_secondary),
                        );
                        unpriced_total(ui, g.missing_prices);
                    });
                    ui.label(equity);
                });
            }
        }
        let gain = |v: f64| ThemeSidePair::GainLoss.text(v >= 0.0, t);
        stat(ui, t, "Realized", format!("{:+.4}", l.realized), gain(l.realized));
        stat(ui, t, "Fees", format!("{:.4}", l.fees), t.theme.text_secondary);
        stat(ui, t, "Funding", format!("{:+.4}", l.funding), gain(l.funding));
        if l.margin_used > 0.0 {
            stat(ui, t, "Margin used", format!("{:.2}", l.margin_used), t.theme.text_secondary);
        }
    });
}

/// What the header says under the one total of a node that reports no account's tier.
const OLDER_SERVER: &str = "This server is older and does not report each account's tier, so this total \
                            adds live, demo and paper money together.";

/// How many accounts a tier group adds, in words.
fn accounts_word(n: usize) -> String {
    if n == 1 { "1 account".to_string() } else { format!("{n} accounts") }
}

/// The top of the Account window (the owner, 2026-11-17): one group per tier, LIVE, DEMO, PAPER, each with its
/// account count and one line per currency, and no figure that adds one tier to another. LIVE sits on its
/// chip's own wash so real money reads apart at a glance. A node that reports no tier gets the one total it
/// always had, with [`OLDER_SERVER`] under it.
pub(crate) fn equity_header(
    ui: &mut egui::Ui,
    model: &equity::AccountWindowEquityModel,
    margin_used_total: f64,
) {
    let t = ThemeTokens::of(ui.ctx());
    if model.tiers.is_empty() {
        equity_hero(ui, model, margin_used_total);
        if !model.rows.is_empty() {
            ui.label(
                egui::RichText::new(OLDER_SERVER)
                    .size(role_px(ui.ctx(), ThemeTextRole::Body))
                    .color(ThemeStatus::Warning.color()),
            );
        }
        return;
    }
    for (i, group) in model.tiers.iter().enumerate() {
        if i > 0 {
            ui.add_space(space::SM);
        }
        let wash = group.tier.map_or(t.theme.background, |tier| {
            vike_ui_theme::components::chip::tier_wash(tier, &t)
        });
        egui::Frame::new()
            .fill(wash)
            .corner_radius(egui::CornerRadius::same(RADIUS))
            .inner_margin(egui::Margin::symmetric(t.metrics.padding as i8, space::SM as i8))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                for (n, line) in group.ledgers.iter().enumerate() {
                    ledger_line(ui, &t, line, (n == 0).then_some(group));
                }
            });
    }
}

#[path = "hero_tests.rs"]
#[cfg(test)]
mod hero_tests;
