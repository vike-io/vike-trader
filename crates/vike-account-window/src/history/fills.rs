//! The Fills tab: every loaded fill, newest first, in the kit's data table.

use vike_data::ExecFillRow;
use vike_ui_theme::components::table::{self, ThemeTableColumn};
use vike_ui_theme::value::history;

use super::fmt_timestamp;

/// The Fills table's columns, in the order the cells are written below.
pub(super) const FILL_COLUMNS: [ThemeTableColumn<'static>; 10] = [
    ThemeTableColumn {
        title: "Time",
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
        title: "Account",
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
        title: "Fee",
        weight: history::NUMBER_WEIGHT,
        min_w: history::NUMBER_MIN_WIDTH,
        numeric: true,
    },
    ThemeTableColumn {
        title: "Liq",
        weight: history::FLAG_WEIGHT,
        min_w: history::FLAG_MIN_WIDTH,
        numeric: false,
    },
    ThemeTableColumn {
        title: "Order",
        weight: history::ID_WEIGHT,
        min_w: history::ID_MIN_WIDTH,
        numeric: false,
    },
];

/// The rows the table shows: each fill ONCE, with its final fee, through the Summary's own read
/// (`ExecFillRow::fill_events`, which folds the fee corrections), and no correction row of its own:
/// one moves no quantity and states no side, so drawn raw it read `SELL`, Qty 0. A row
/// `fill_events` cannot read (an empty trade id) is shown as stored.
pub(super) fn shown_fills(rows: &[ExecFillRow]) -> Vec<ExecFillRow> {
    // `fill_events` keeps `rows`' order, so each event is the next row whose trade id it carries.
    let mut events = ExecFillRow::fill_events(rows).into_iter().peekable();
    rows.iter()
        .filter_map(|r| {
            let event = events.next_if(|e| r.trade_id == e.trade_id.as_str());
            if r.corrects_trade_id.is_some() {
                return None;
            }
            let mut row = r.clone();
            if let Some(e) = event {
                row.commission = e.commission;
                row.commission_asset = e.commission_asset.to_string();
            }
            Some(row)
        })
        .collect()
}

/// Draw the fills (already newest first).
pub(super) fn fills_table(
    ui: &mut egui::Ui,
    fills: &[ExecFillRow],
    time_zone: vike_ui_theme::time_zone::ThemeDisplayTimeZone,
) {
    table::data_table(ui, "history_fills", &FILL_COLUMNS, fills.len(), None, |r, c| {
        fill_cell(&fills[r], c, time_zone)
    });
}

/// One Fills cell, by column index of [`FILL_COLUMNS`].
pub(super) fn fill_cell(
    f: &ExecFillRow,
    col: usize,
    time_zone: vike_ui_theme::time_zone::ThemeDisplayTimeZone,
) -> String {
    match col {
        0 => fmt_timestamp(f.ts, time_zone),
        1 => f.venue.clone(),
        2 => f.symbol.clone(),
        3 => f.account.clone().unwrap_or_else(|| "—".to_string()),
        4 => vike_model::side_word(f.side).to_string(),
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
        // Not this install's order (a trade on the venue's own site, another install's): the
        // account's all the same, and folded into its Summary, so it is marked rather than hidden.
        _ if f.is_foreign() => {
            format!("foreign · {}", order_ref(&f.order_client_id, f.venue_order_id.as_deref()))
        }
        _ => order_ref(&f.order_client_id, f.venue_order_id.as_deref()),
    }
}

/// The order a row belongs to: OUR order client id when there is one; otherwise the venue's own id,
/// marked as the venue's (an import from a trade-history export carries only that); otherwise `—`.
/// [`fill_cell`] puts `foreign · ` before it for a fill that is not this install's order.
pub(super) fn order_ref(order_client_id: &str, venue_order_id: Option<&str>) -> String {
    match (order_client_id, venue_order_id) {
        ("", Some(v)) => format!("venue {v}"),
        ("", None) => "—".to_string(),
        (order_client_id, _) => order_client_id.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::fill_cell;
    use vike_data::ExecFillRow;

    fn row(ownership: Option<&str>) -> ExecFillRow {
        ExecFillRow {
            ts: 1,
            trade_id: "t1".to_string(),
            order_client_id: "c1".to_string(),
            venue: "binance".to_string(),
            symbol: "BTCUSDT".to_string(),
            side: 1,
            qty: 1.0,
            px: 100.0,
            commission: 0.0,
            mark_price: None,
            liquidity_side: String::new(),
            commission_asset: String::new(),
            venue_order_id: Some("v1".to_string()),
            account: Some("MAIN".to_string()),
            position_side: None,
            corrects_trade_id: None,
            correction_seq: None,
            mount_id: None,
            received_ms: None,
            ownership: ownership.map(str::to_string),
            journal_sequence: None,
            journal_generation: None,
        }
    }

    /// **A foreign fill is marked in the Fills table** (the account's, folded into its Summary, but not
    /// this install's order); ours, and a row recorded before the stream, read as they did.
    #[test]
    fn a_foreign_fill_is_marked_in_the_order_cell() {
        let zone = vike_ui_theme::time_zone::ThemeDisplayTimeZone::default();
        assert_eq!(fill_cell(&row(Some("foreign")), 9, zone), "foreign · c1");
        assert_eq!(fill_cell(&row(Some("ours")), 9, zone), "c1");
        assert_eq!(fill_cell(&row(None), 9, zone), "c1");
    }
}
