//! The Orders tab: one row per order — its LATEST snapshot in the loaded rows, collapsed by
//! `vike_data::exec_index::latest_order_snapshots`, the one rule reconciliation collapses by too —
//! and, under the table, the clicked order's whole lifecycle and its fills.
//!
//! ⚠ A non-terminal last snapshot is an order still open — or one the materializer has not caught
//! up with. Its status keeps the Account window's word and is marked as LIVE THERE, never given a
//! "live" state of this window's own: what is working now is the Account window's fact.

use vike_data::ExecOrderRow;
use vike_data::exec_index::latest_order_snapshots;
use vike_ui_theme::components::table::{self, Column};
use vike_ui_theme::components::{Tokens, role_px};
use vike_ui_theme::metrics::space;
use vike_ui_theme::type_scale::TextRole;
use vike_ui_theme::value::history;

use super::fills::{FILL_COLUMNS, fill_cell, order_ref, side_word};
use super::fmt_ts;
use crate::data::exec_history::HistoryPage;

/// The Orders table's columns, in the order the cells are written below.
pub(super) const ORDER_COLUMNS: [Column<'static>; 11] = [
    Column {
        title: "Updated",
        weight: history::TIME_WEIGHT,
        min_w: history::TIME_MIN_W,
        numeric: false,
    },
    Column {
        title: "Venue",
        weight: history::TEXT_WEIGHT,
        min_w: history::TEXT_MIN_W,
        numeric: false,
    },
    Column {
        title: "Symbol",
        weight: history::TEXT_WEIGHT,
        min_w: history::TEXT_MIN_W,
        numeric: false,
    },
    Column {
        title: "Side",
        weight: history::FLAG_WEIGHT,
        min_w: history::FLAG_MIN_W,
        numeric: false,
    },
    Column {
        title: "Type",
        weight: history::TEXT_WEIGHT,
        min_w: history::TEXT_MIN_W,
        numeric: false,
    },
    Column { title: "Qty", weight: history::NUM_WEIGHT, min_w: history::NUM_MIN_W, numeric: true },
    Column {
        title: "Price",
        weight: history::NUM_WEIGHT,
        min_w: history::NUM_MIN_W,
        numeric: true,
    },
    Column {
        title: "Filled",
        weight: history::NUM_WEIGHT,
        min_w: history::NUM_MIN_W,
        numeric: true,
    },
    Column {
        title: "Avg px",
        weight: history::NUM_WEIGHT,
        min_w: history::NUM_MIN_W,
        numeric: true,
    },
    Column {
        title: "Status",
        weight: history::TEXT_WEIGHT,
        min_w: history::TEXT_MIN_W,
        numeric: false,
    },
    Column { title: "Order", weight: history::ID_WEIGHT, min_w: history::ID_MIN_W, numeric: false },
];

/// The lifecycle table's columns: one row per materialized snapshot of the expanded order.
const LIFECYCLE_COLUMNS: [Column<'static>; 5] = [
    Column {
        title: "Time",
        weight: history::TIME_WEIGHT,
        min_w: history::TIME_MIN_W,
        numeric: false,
    },
    Column {
        title: "Status",
        weight: history::TEXT_WEIGHT,
        min_w: history::TEXT_MIN_W,
        numeric: false,
    },
    Column {
        title: "Filled",
        weight: history::NUM_WEIGHT,
        min_w: history::NUM_MIN_W,
        numeric: true,
    },
    Column {
        title: "Avg px",
        weight: history::NUM_WEIGHT,
        min_w: history::NUM_MIN_W,
        numeric: true,
    },
    Column {
        title: "Price",
        weight: history::NUM_WEIGHT,
        min_w: history::NUM_MIN_W,
        numeric: true,
    },
];

/// Draw one row per order (newest update first), and the clicked one's lifecycle under it.
pub(super) fn orders_table(
    ui: &mut egui::Ui,
    page: &HistoryPage,
    expanded: &mut Option<String>,
    tz: vike_chart::DisplayTz,
) {
    use egui::RichText;
    let mut latest = latest_order_snapshots(page.orders.iter().cloned());
    latest.reverse();
    let selected =
        expanded.as_ref().and_then(|c| latest.iter().position(|o| o.client_order_id == *c));
    let seen =
        table::data_table(ui, "hist_orders", &ORDER_COLUMNS, latest.len(), selected, |r, c| {
            order_cell(&latest[r], c, tz)
        });
    if let Some(i) = seen.clicked {
        *expanded = Some(latest[i].client_order_id.clone());
    }
    let Some(coid) = expanded.as_deref() else { return };
    let t = Tokens::of(ui.ctx());
    ui.add_space(space::MD);
    ui.label(
        RichText::new(format!("Lifecycle · {coid}"))
            .size(role_px(ui.ctx(), TextRole::Strong))
            .color(t.theme.text)
            .strong(),
    );
    let mut steps: Vec<&ExecOrderRow> =
        page.orders.iter().filter(|o| o.client_order_id == coid).collect();
    steps.sort_by_key(|o| o.ts);
    table::data_table(ui, "hist_life", &LIFECYCLE_COLUMNS, steps.len(), None, |r, c| {
        let o = steps[r];
        match c {
            0 => fmt_ts(o.ts, tz),
            1 => o.status.clone(),
            2 => o.filled_qty.to_string(),
            3 => o.avg_fill_px.to_string(),
            _ => price_cell(o),
        }
    });
    let fills: Vec<_> = page.fills.iter().filter(|f| f.client_order_id == coid).collect();
    if !fills.is_empty() {
        ui.add_space(space::SM);
        table::data_table(ui, "hist_life_fills", &FILL_COLUMNS, fills.len(), None, |r, c| {
            fill_cell(fills[r], c, tz)
        });
    }
}

/// One Orders cell, by column index of [`ORDER_COLUMNS`].
pub(super) fn order_cell(o: &ExecOrderRow, col: usize, tz: vike_chart::DisplayTz) -> String {
    match col {
        0 => fmt_ts(o.ts, tz),
        1 => o.venue.clone(),
        2 => o.symbol.clone(),
        3 => side_word(o.side).to_string(),
        4 => o.order_type.clone(),
        5 => o.qty.to_string(),
        6 => price_cell(o),
        7 => format!("{}/{}", o.filled_qty, o.qty),
        8 => o.avg_fill_px.to_string(),
        9 => status_word(&o.status),
        _ => order_ref(&o.client_order_id, o.venue_order_id.as_deref()),
    }
}

/// The status as the Account window spells it (the stored `OrderStatus::as_str` word); one that is
/// not done (`OrderStatus::is_live`, the Account window's own test), or not a status word at all, is
/// marked as live THERE.
pub(super) fn status_word(status: &str) -> String {
    if vike_model::OrderStatus::parse(status).is_some_and(|s| !s.is_live()) {
        status.to_string()
    } else {
        format!("{status} (live state: Account window)")
    }
}

/// The limit price, the trigger after it when there is one, `market` when neither.
fn price_cell(o: &ExecOrderRow) -> String {
    match (o.price, o.trigger_price) {
        (Some(p), Some(trig)) => format!("{p} @ {trig}"),
        (Some(p), None) => p.to_string(),
        (None, Some(trig)) => format!("@ {trig}"),
        (None, None) => "market".to_string(),
    }
}

#[cfg(test)]
mod status_word_tests {
    use super::status_word;

    /// The terminal set the Account window has always treated as done stays bare: the five FSM
    /// terminals and LIQUIDATED.
    #[test]
    fn a_done_status_is_its_bare_word() {
        for s in ["FILLED", "CANCELED", "REJECTED", "DENIED", "EXPIRED", "LIQUIDATED"] {
            assert_eq!(status_word(s), s, "{s} is done");
        }
    }

    /// A working status, or a word that is not one of the FSM's, points at the Account window.
    #[test]
    fn a_working_or_unknown_status_points_at_the_account_window() {
        for s in
            ["ACCEPTED", "PARTIALLY_FILLED", "SUBMITTED", "PENDING_CANCEL", "NEW", "", "filled"]
        {
            assert_eq!(status_word(s), format!("{s} (live state: Account window)"), "{s:?}");
        }
    }
}
