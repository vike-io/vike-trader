//! The Account window's Orders tab: the working orders (each with its delay badge, its ✕ and its
//! order id's friendly words) and the orders the node emulates itself below them. The ✕ writes the
//! one cancel the window sends; [`delay_badge`] and [`order_client_id_words`] are read by the Trade
//! window too.

use crate::{number_cell, text_cell, text_w};
use vike_ui_theme::components::{ThemeStatus, ThemeTokens, role_px};
use vike_ui_theme::icons;
use vike_ui_theme::metrics::space;
use vike_ui_theme::side::ThemeSidePair;
use vike_ui_theme::type_scale::ThemeTextRole;
use vike_ui_theme::value::account;
use vike_utility::number_text::format_number;

/// The order row's DELAY badge (decision 0140): `DELAYED` / `FROZEN` when the node recorded that the
/// order was priced off data that was not real-time, `None` for a real-time or unlabelled order (no
/// badge: the ordinary case says nothing). Information, never a gate: the row and its ✕ are the
/// same either way. The trade window's status line reads the same words.
pub fn delay_badge(delay: Option<vike_model::StreamDelay>) -> Option<&'static str> {
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
pub fn order_client_id_words(
    id: &str,
    snapshot: &vike_execution::CoreSnapshot,
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
        .and_then(|code| snapshot.portfolio.venues.iter().find(|b| b.account_code == Some(code)))
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
/// their whole text on hover, so the ✕ before them stays on screen however narrow the window is. A
/// click on a ✕ writes that order's client id into `cancel`.
pub(crate) fn orders_section(
    ui: &mut egui::Ui,
    snapshot: &vike_execution::CoreSnapshot,
    mounts: &[vike_tradehub_client::wire::MountViewWire],
    cancel: &mut Option<String>,
) {
    use egui::{Label, RichText};

    let t = ThemeTokens::of(ui.ctx());
    if snapshot.orders.is_empty() {
        ui.label(
            RichText::new("No working orders.")
                .size(role_px(ui.ctx(), ThemeTextRole::Strong))
                .color(t.theme.text_tertiary),
        );
        return;
    }
    // The text cells of an order's row, in column order, and the qty (a number) beside them.
    let body = role_px(ui.ctx(), ThemeTextRole::Body);
    let words = |o: &vike_execution::OrderView| {
        let sc = ThemeSidePair::BuySell.text(o.side > 0, &t);
        let side = vike_model::side_word(o.side);
        [
            RichText::new(order_venue(o)).size(body).color(t.theme.text),
            RichText::new(&o.symbol).size(body).color(t.theme.text),
            RichText::new(side).size(body).color(sc).strong(),
            RichText::new(o.order_type.as_str()).size(body).color(t.theme.text_secondary),
            RichText::new(o.status.as_str()).size(body).color(t.theme.text_secondary),
        ]
    };
    // venue, symbol, side, qty, type, status: each at its token width or the widest thing it holds.
    let mut w = [
        account::WIDTH_VENUE,
        0.0,
        account::ORDER_WIDTH_SIDE,
        account::ORDER_WIDTH_QUANTITY,
        account::ORDER_WIDTH_TYPE,
        account::ORDER_WIDTH_STATUS,
    ];
    let strong = role_px(ui.ctx(), ThemeTextRole::Strong);
    for o in &snapshot.orders {
        let [venue, symbol, side, kind, status] = words(o);
        for (at, rt) in [(0, venue), (1, symbol), (2, side), (4, kind), (5, status)] {
            w[at] = w[at].max(text_w(ui, rt));
        }
        let quantity = RichText::new(format_number(o.qty)).monospace().size(strong);
        w[3] = w[3].max(text_w(ui, quantity));
    }
    let list_h = account::ORDERS_MAX_HEIGHT;
    egui::ScrollArea::vertical().max_height(list_h).auto_shrink([false, false]).show(ui, |ui| {
        for o in &snapshot.orders {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = space::LG;
                let [venue, symbol, side, kind, status] = words(o);
                text_cell(ui, w[0], venue);
                text_cell(ui, w[1], symbol);
                text_cell(ui, w[2], side);
                number_cell(ui, w[3], format_number(o.qty), t.theme.text);
                text_cell(ui, w[4], kind);
                text_cell(ui, w[5], status);
                // Priced off delayed or frozen data (decision 0140): shown, never a gate.
                if let Some(word) = delay_badge(o.data_delay) {
                    vike_ui_theme::components::chip::badge(ui, word, ThemeStatus::Warning);
                }
                // A working order keeps its ✕: `is_live` is false for exactly the five terminals and
                // LIQUIDATED, the set this table has always treated as done.
                if o.status.is_live()
                    && icons::named(ui.small_button(icons::CANCEL), "Cancel order").clicked()
                {
                    *cancel = Some(o.order_client_id.clone());
                }
                // The order client id, last: it names the order to the venue, the journal and the CLI.
                ui.add(
                    Label::new(
                        RichText::new(&o.order_client_id)
                            .monospace()
                            .size(role_px(ui.ctx(), ThemeTextRole::Caption))
                            .color(t.theme.text_tertiary),
                    )
                    .truncate(),
                )
                .on_hover_text(&o.order_client_id);
                if let Some(words) = order_client_id_words(&o.order_client_id, snapshot, mounts) {
                    ui.add(
                        Label::new(
                            RichText::new(&words)
                                .size(role_px(ui.ctx(), ThemeTextRole::Caption))
                                .color(t.theme.text_tertiary),
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
pub(crate) fn emulated_section(ui: &mut egui::Ui, snapshot: &vike_execution::CoreSnapshot) {
    use egui::{Label, RichText};

    if snapshot.emulated_orders.is_empty() {
        return;
    }
    let t = ThemeTokens::of(ui.ctx());
    ui.add_space(space::LG);
    ui.label(
        RichText::new(format!("Emulated by the node ({})", snapshot.emulated_orders.len()))
            .size(role_px(ui.ctx(), ThemeTextRole::Strong))
            .color(t.theme.text)
            .strong(),
    );
    ui.add_space(space::SM);
    let body = role_px(ui.ctx(), ThemeTextRole::Body);
    let caption = role_px(ui.ctx(), ThemeTextRole::Caption);
    for e in &snapshot.emulated_orders {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = space::LG;
            vike_ui_theme::components::chip::badge(ui, e.kind, ThemeStatus::Info);
            let venue = match e.account.as_ref().and_then(|a| a.text()) {
                Some(label) => format!("{}#{label}", e.venue),
                None => e.venue.clone(),
            };
            let side = vike_model::side_word(e.side);
            for (text, color) in [
                (venue, t.theme.text),
                (e.symbol.clone(), t.theme.text),
                (side.to_string(), ThemeSidePair::BuySell.text(e.side > 0, &t)),
                (format_number(e.qty), t.theme.text),
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
            ui.add(
                Label::new(RichText::new(&tail).size(caption).color(t.theme.text_tertiary))
                    .truncate(),
            )
            .on_hover_text(&tail);
        });
    }
}

#[path = "orders_tests.rs"]
#[cfg(test)]
mod orders_tests;
