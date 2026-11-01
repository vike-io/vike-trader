//! The Fills tab: every loaded fill, newest first, in the kit's data table.

use vike_data::ExecFillRow;
use vike_ui_theme::components::table::{self, Column};
use vike_ui_theme::value::history;

use super::fmt_ts;

/// The Fills table's columns, in the order the cells are written below.
pub(super) const FILL_COLUMNS: [Column<'static>; 10] = [
    Column {
        title: "Time",
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
        title: "Account",
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
    Column { title: "Qty", weight: history::NUM_WEIGHT, min_w: history::NUM_MIN_W, numeric: true },
    Column {
        title: "Price",
        weight: history::NUM_WEIGHT,
        min_w: history::NUM_MIN_W,
        numeric: true,
    },
    Column { title: "Fee", weight: history::NUM_WEIGHT, min_w: history::NUM_MIN_W, numeric: true },
    Column {
        title: "Liq",
        weight: history::FLAG_WEIGHT,
        min_w: history::FLAG_MIN_W,
        numeric: false,
    },
    Column { title: "Order", weight: history::ID_WEIGHT, min_w: history::ID_MIN_W, numeric: false },
];

/// Draw the fills (already newest first).
pub(super) fn fills_table(ui: &mut egui::Ui, fills: &[ExecFillRow], tz: vike_chart::DisplayTz) {
    table::data_table(ui, "hist_fills", &FILL_COLUMNS, fills.len(), None, |r, c| {
        fill_cell(&fills[r], c, tz)
    });
}

/// One Fills cell, by column index of [`FILL_COLUMNS`].
pub(super) fn fill_cell(f: &ExecFillRow, col: usize, tz: vike_chart::DisplayTz) -> String {
    match col {
        0 => fmt_ts(f.ts, tz),
        1 => f.venue.clone(),
        2 => f.symbol.clone(),
        3 => f.account.clone().unwrap_or_else(|| "—".to_string()),
        4 => side_word(f.side).to_string(),
        5 => f.qty.to_string(),
        6 => f.px.to_string(),
        7 => match f.commission_asset.as_str() {
            "" => f.commission.to_string(),
            asset => format!("{} {asset}", f.commission),
        },
        8 => match f.liquidity_side.as_str() {
            "maker" => "M".to_string(),
            "taker" => "T".to_string(),
            _ => "—".to_string(),
        },
        _ => order_ref(&f.client_order_id, f.venue_order_id.as_deref()),
    }
}

/// `BUY` / `SELL` — the Account window's own words for a side.
pub(super) fn side_word(side: i32) -> &'static str {
    if side > 0 { "BUY" } else { "SELL" }
}

/// The order a row belongs to: OUR client order id when there is one; otherwise the venue's own id,
/// marked as the venue's (an import from a trade-history export carries only that); otherwise `—`.
pub(super) fn order_ref(client_order_id: &str, venue_order_id: Option<&str>) -> String {
    match (client_order_id, venue_order_id) {
        ("", Some(v)) => format!("venue {v}"),
        ("", None) => "—".to_string(),
        (coid, _) => coid.to_string(),
    }
}
