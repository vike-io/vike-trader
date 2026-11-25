//! The Account window's Strategies tab: what the node runs, one row per mount, with the account it
//! trades and the tier that account runs at now. It has no type of its own.

use crate::accounts::{LINK_NOT_RUNNING, tier_mark};
use crate::{text_cell, text_w};
use vike_tradehub_client::node_words::BackendNodeWords;
use vike_ui_theme::components::{ThemeStatus, ThemeTokens, role_px};
use vike_ui_theme::metrics::space;
use vike_ui_theme::type_scale::ThemeTextRole;

/// What an empty Strategies tab says.
const NO_STRATEGIES: &str = "No strategies.";

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
/// With no row it says so, as the Orders tab does.
pub(crate) fn strategies_section(ui: &mut egui::Ui, words: &BackendNodeWords) {
    use egui::RichText;

    let t = ThemeTokens::of(ui.ctx());
    let strong = role_px(ui.ctx(), ThemeTextRole::Strong);
    if words.mounts.is_empty() {
        ui.label(RichText::new(NO_STRATEGIES).size(strong).color(t.theme.text_tertiary));
        return;
    }
    let body = role_px(ui.ctx(), ThemeTextRole::Body);
    let cells = |m: &vike_tradehub_client::wire::MountViewWire| {
        [
            RichText::new(&m.strategy).size(body).color(t.theme.text),
            RichText::new(mount_book(m)).size(body).color(t.theme.text_secondary),
            RichText::new(format!("{} {}", m.symbol, m.interval))
                .size(body)
                .color(t.theme.text_secondary),
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
            tier_mark(ui, vike_tradehub_client::observe_decode::map_tier(m.tier.as_deref()));
            if m.not_running {
                vike_ui_theme::components::chip::badge(ui, LINK_NOT_RUNNING, ThemeStatus::Error)
                    .on_hover_text(MOUNT_NOT_RUNNING_WHY);
            }
            if m.latched {
                vike_ui_theme::components::chip::badge(ui, "BUDGET SPENT", ThemeStatus::Warning)
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

#[path = "strategies_tests.rs"]
#[cfg(test)]
mod strategies_tests;
