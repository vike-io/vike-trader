//! The Orders tab: one row per order — its LATEST snapshot in the loaded rows, collapsed by
//! `vike_data::execution_index::latest_order_snapshots`, the one rule reconciliation collapses by too —
//! and, under the table, the clicked order's whole lifecycle and its fills.
//!
//! ⚠ A non-terminal last snapshot is an order still open — or one whose later events the datahub
//! has not stored yet. Its status keeps the Account window's word and is marked as LIVE THERE, never given a
//! "live" state of this window's own: what is working now is the Account window's fact.

use vike_data::ExecOrderRow;
use vike_data::execution_index::latest_order_snapshots;
use vike_ui_theme::components::table::{self, ThemeTableColumn};
use vike_ui_theme::components::{ThemeTokens, role_px};
use vike_ui_theme::metrics::space;
use vike_ui_theme::type_scale::ThemeTextRole;
use vike_ui_theme::value::history;

use super::fills::{FILL_COLUMNS, fill_cell, order_ref};
use super::fmt_timestamp;
use super::page::ExecutionHistoryPage;

/// The Orders table's columns, in the order the cells are written below.
pub(super) const ORDER_COLUMNS: [ThemeTableColumn<'static>; 11] = [
    ThemeTableColumn {
        title: "Updated",
        weight: history::TIME_WEIGHT,
        min_w: history::TIME_MIN_WIDTH,
        numeric: false,
    },
    ThemeTableColumn {
        title: "Venue",
        weight: history::TEXT_WEIGHT,
        min_w: history::TEXT_MIN_WIDTH,
        numeric: false,
    },
    ThemeTableColumn {
        title: "Symbol",
        weight: history::TEXT_WEIGHT,
        min_w: history::TEXT_MIN_WIDTH,
        numeric: false,
    },
    ThemeTableColumn {
        title: "Side",
        weight: history::FLAG_WEIGHT,
        min_w: history::FLAG_MIN_WIDTH,
        numeric: false,
    },
    ThemeTableColumn {
        title: "Type",
        weight: history::TEXT_WEIGHT,
        min_w: history::TEXT_MIN_WIDTH,
        numeric: false,
    },
    ThemeTableColumn {
        title: "Qty",
        weight: history::NUMBER_WEIGHT,
        min_w: history::NUMBER_MIN_WIDTH,
        numeric: true,
    },
    ThemeTableColumn {
        title: "Price",
        weight: history::NUMBER_WEIGHT,
        min_w: history::NUMBER_MIN_WIDTH,
        numeric: true,
    },
    ThemeTableColumn {
        title: "Filled",
        weight: history::NUMBER_WEIGHT,
        min_w: history::NUMBER_MIN_WIDTH,
        numeric: true,
    },
    ThemeTableColumn {
        title: "Avg px",
        weight: history::NUMBER_WEIGHT,
        min_w: history::NUMBER_MIN_WIDTH,
        numeric: true,
    },
    ThemeTableColumn {
        title: "Status",
        weight: history::TEXT_WEIGHT,
        min_w: history::TEXT_MIN_WIDTH,
        numeric: false,
    },
    ThemeTableColumn {
        title: "Order",
        weight: history::ID_WEIGHT,
        min_w: history::ID_MIN_WIDTH,
        numeric: false,
    },
];

/// The lifecycle table's columns: one row per stored snapshot of the expanded order.
const LIFECYCLE_COLUMNS: [ThemeTableColumn<'static>; 5] = [
    ThemeTableColumn {
        title: "Time",
        weight: history::TIME_WEIGHT,
        min_w: history::TIME_MIN_WIDTH,
        numeric: false,
    },
    ThemeTableColumn {
        title: "Status",
        weight: history::TEXT_WEIGHT,
        min_w: history::TEXT_MIN_WIDTH,
        numeric: false,
    },
    ThemeTableColumn {
        title: "Filled",
        weight: history::NUMBER_WEIGHT,
        min_w: history::NUMBER_MIN_WIDTH,
        numeric: true,
    },
    ThemeTableColumn {
        title: "Avg px",
        weight: history::NUMBER_WEIGHT,
        min_w: history::NUMBER_MIN_WIDTH,
        numeric: true,
    },
    ThemeTableColumn {
        title: "Price",
        weight: history::NUMBER_WEIGHT,
        min_w: history::NUMBER_MIN_WIDTH,
        numeric: true,
    },
];

/// Draw one row per order (newest update first), and the clicked one's lifecycle under it.
pub(super) fn orders_table(
    ui: &mut egui::Ui,
    page: &ExecutionHistoryPage,
    expanded: &mut Option<String>,
    time_zone: vike_ui_theme::time_zone::ThemeDisplayTimeZone,
) {
    use egui::RichText;
    let mut latest = latest_order_snapshots(page.orders.iter().cloned());
    latest.reverse();
    let selected =
        expanded.as_ref().and_then(|c| latest.iter().position(|o| o.order_client_id == *c));
    let seen =
        table::data_table(ui, "history_orders", &ORDER_COLUMNS, latest.len(), selected, |r, c| {
            order_cell(&latest[r], c, time_zone)
        });
    if let Some(i) = seen.clicked {
        *expanded = Some(latest[i].order_client_id.clone());
    }
    let Some(order_client_id) = expanded.as_deref() else { return };
    let t = ThemeTokens::of(ui.ctx());
    ui.add_space(space::MD);
    ui.label(
        RichText::new(format!("Lifecycle · {order_client_id}"))
            .size(role_px(ui.ctx(), ThemeTextRole::Strong))
            .color(t.theme.text)
            .strong(),
    );
    let mut steps: Vec<&ExecOrderRow> =
        page.orders.iter().filter(|o| o.order_client_id == order_client_id).collect();
    steps.sort_by_key(|o| o.ts);
    table::data_table(ui, "history_life", &LIFECYCLE_COLUMNS, steps.len(), None, |r, c| {
        let o = steps[r];
        match c {
            0 => fmt_timestamp(o.ts, time_zone),
            1 => o.status.clone(),
            2 => o.filled_qty.to_string(),
            3 => o.avg_fill_px.to_string(),
            _ => price_cell(o),
        }
    });
    let fills: Vec<_> =
        page.fills.iter().filter(|f| f.order_client_id == order_client_id).collect();
    if !fills.is_empty() {
        ui.add_space(space::SM);
        table::data_table(ui, "history_life_fills", &FILL_COLUMNS, fills.len(), None, |r, c| {
            fill_cell(fills[r], c, time_zone)
        });
    }
}

/// One Orders cell, by column index of [`ORDER_COLUMNS`].
pub(super) fn order_cell(
    o: &ExecOrderRow,
    col: usize,
    time_zone: vike_ui_theme::time_zone::ThemeDisplayTimeZone,
) -> String {
    match col {
        0 => fmt_timestamp(o.ts, time_zone),
        1 => o.venue.clone(),
        2 => o.symbol.clone(),
        3 => vike_model::side_word(o.side).to_string(),
        4 => o.order_type.clone(),
        5 => o.qty.to_string(),
        6 => price_cell(o),
        7 => format!("{}/{}", o.filled_qty, o.qty),
        8 => o.avg_fill_px.to_string(),
        9 => status_word(o),
        _ => order_ref(&o.order_client_id, o.venue_order_id.as_deref()),
    }
}

/// The status as the Account window spells it (the stored `OrderStatus::as_str` word); one that is
/// not done (`OrderStatus::is_live`, the Account window's own test), or not a status word at all
/// (`ExecOrderRow::order_status` is `None`), is marked as live THERE.
pub(super) fn status_word(o: &ExecOrderRow) -> String {
    if o.order_status().is_some_and(|s| !s.is_live()) {
        o.status.clone()
    } else {
        format!("{} (live state: Account window)", o.status)
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
    use super::{ExecOrderRow, status_word};

    fn row(status: &str) -> ExecOrderRow {
        ExecOrderRow {
            ts: 1,
            order_client_id: "c1".to_string(),
            venue: "binance".to_string(),
            symbol: "BTCUSDT".to_string(),
            side: 1,
            qty: 1.0,
            order_type: "limit".to_string(),
            status: status.to_string(),
            price: Some(1.0),
            trigger_price: None,
            venue_order_id: None,
            filled_qty: 0.0,
            avg_fill_px: 0.0,
            account: None,
            mount_id: None,
            ownership: None,
            journal_sequence: None,
            journal_generation: None,
            reason: None,
        }
    }

    /// The terminal set the Account window has always treated as done stays bare: the five FSM
    /// terminals and LIQUIDATED.
    #[test]
    fn a_done_status_is_its_bare_word() {
        for s in ["FILLED", "CANCELED", "REJECTED", "DENIED", "EXPIRED", "LIQUIDATED"] {
            assert_eq!(status_word(&row(s)), s, "{s} is done");
        }
    }

    /// A working status, or a word that is not one of the FSM's, points at the Account window.
    #[test]
    fn a_working_or_unknown_status_points_at_the_account_window() {
        for s in
            ["ACCEPTED", "PARTIALLY_FILLED", "SUBMITTED", "PENDING_CANCEL", "NEW", "", "filled"]
        {
            assert_eq!(status_word(&row(s)), format!("{s} (live state: Account window)"), "{s:?}");
        }
    }
}
