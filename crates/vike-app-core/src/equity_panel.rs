//! Qt-free view-model for the cross-venue equity panel (PR-2 T4). `vike-app` renders it; this
//! module is CI-tested because `vike-app` itself is not (see the crate-level doc).
//!
//! Pure mapping only: one [`EquityRow`] per `CoreSnapshot::portfolio` venue entry, in order;
//! `equity_total`/`missing_prices_total` copied straight from the snapshot. No egui, no I/O —
//! the GUI owns all number formatting, the only rendering choice made here is the short `mode`
//! tag.

use vike_core::CoreSnapshot;
use vike_exec::BalanceMode;

/// One venue's row in the cross-venue equity panel — a flattened, display-ready slice of a
/// `VenueBlock`.
#[derive(Debug, Clone, PartialEq)]
pub struct EquityRow {
    pub venue: String,
    pub equity: f64,
    pub realized: f64,
    pub unrealized: f64,
    pub fees: f64,
    pub funding: f64,
    /// short display tag for `BalanceMode`: "\u{0394}" (Delta) / "auth" (Authoritative).
    pub mode: String,
    pub missing_prices: u32,
}

/// The whole cross-venue equity panel: the headline (`equity_total`/`missing_prices_total`)
/// plus one [`EquityRow`] per venue, in `CoreSnapshot::venues` order.
#[derive(Debug, Clone, PartialEq)]
pub struct EquityPanelModel {
    pub equity_total: f64,
    pub missing_prices_total: u32,
    pub rows: Vec<EquityRow>,
}

impl EquityPanelModel {
    /// Shape a `CoreSnapshot` into the panel model: one `EquityRow` per `VenueBlock` in order;
    /// `equity_total`/`missing_prices_total` copied straight from the snapshot. Pure — no I/O,
    /// no egui, no formatting beyond the `mode` tag (the GUI formats every number).
    pub fn from_snapshot(snap: &CoreSnapshot) -> Self {
        let rows = snap
            .portfolio
            .venues
            .iter()
            .map(|v| EquityRow {
                venue: v.venue.clone(),
                equity: v.equity,
                realized: v.realized_pnl,
                unrealized: v.unrealized,
                fees: v.fees_paid,
                funding: v.funding_paid,
                mode: match v.balance_mode {
                    BalanceMode::Delta => "\u{0394}".to_string(),
                    BalanceMode::Authoritative => "auth".to_string(),
                },
                missing_prices: v.missing_prices,
            })
            .collect();
        EquityPanelModel {
            equity_total: snap.portfolio.equity_total,
            missing_prices_total: snap.portfolio.missing_prices_total,
            rows,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_core::{CoreSnapshot, VenueBlock};
    use vike_exec::{BalanceMode, TradingState};

    /// A zero-valued `VenueBlock` for `venue`, Delta mode, no positions — struct-update base
    /// for tests that only care about a subset of fields.
    fn base_venue_block(venue: &str) -> VenueBlock {
        VenueBlock {
            venue: venue.to_string(),
            balance: 0.0,
            realized_pnl: 0.0,
            fees_paid: 0.0,
            funding_paid: 0.0,
            balance_mode: BalanceMode::Delta,
            multipliers: Default::default(),
            multiplier_default: 1.0,
            equity: 0.0,
            unrealized: 0.0,
            missing_prices: 0,
            margin_used: 0.0,
            free_bp: 0.0,
            margin_ratio: 0.0,
            fee_schedule: None,
            trading_state: TradingState::Active,
            positions: Vec::new(),
        }
    }

    #[test]
    fn panel_model_maps_venues_in_order() {
        let mut snap = CoreSnapshot::empty("binance", "BTC");
        snap.portfolio.equity_total = 12_345.678;
        snap.portfolio.missing_prices_total = 3;
        snap.portfolio.venues = vec![
            VenueBlock { equity: 1_100.0, unrealized: 100.0, ..base_venue_block("binance") },
            VenueBlock {
                equity: 2_200.0,
                unrealized: 200.0,
                missing_prices: 2,
                balance_mode: BalanceMode::Authoritative,
                ..base_venue_block("bybit")
            },
        ];

        let m = EquityPanelModel::from_snapshot(&snap);

        assert_eq!(m.rows.len(), 2);
        assert_eq!(m.rows[0].venue, "binance");
        assert_eq!(m.equity_total.to_bits(), snap.portfolio.equity_total.to_bits());
        assert_eq!(m.missing_prices_total, snap.portfolio.missing_prices_total);
        assert_eq!(m.rows[1].unrealized, snap.portfolio.venues[1].unrealized);
    }

    #[test]
    fn equity_row_maps_every_field_without_transposition() {
        // Distinct values per field so a swapped mapping (e.g. fees<->funding) fails loudly.
        let mut snap = CoreSnapshot::empty("okx", "BTC-PERP");
        snap.portfolio.venues = vec![VenueBlock {
            venue: "okx".to_string(),
            balance: 500.0,
            realized_pnl: 7.0,
            fees_paid: 3.0,
            funding_paid: 4.0,
            balance_mode: BalanceMode::Delta,
            multipliers: Default::default(),
            multiplier_default: 1.0,
            equity: 9_000.0,
            unrealized: 8.0,
            missing_prices: 5,
            margin_used: 0.0,
            free_bp: 0.0,
            margin_ratio: 0.0,
            fee_schedule: None,
            trading_state: TradingState::Active,
            positions: Vec::new(),
        }];

        let row = &EquityPanelModel::from_snapshot(&snap).rows[0];

        assert_eq!(row.venue, "okx");
        assert_eq!(row.equity, 9_000.0);
        assert_eq!(row.realized, 7.0);
        assert_eq!(row.unrealized, 8.0);
        assert_eq!(row.fees, 3.0);
        assert_eq!(row.funding, 4.0);
        assert_eq!(row.missing_prices, 5);
    }

    #[test]
    fn mode_tag_renders_short_strings() {
        let mut delta_snap = CoreSnapshot::empty("binance", "BTC");
        delta_snap.portfolio.venues =
            vec![VenueBlock { balance_mode: BalanceMode::Delta, ..base_venue_block("binance") }];
        let mut auth_snap = CoreSnapshot::empty("bybit", "BTC");
        auth_snap.portfolio.venues = vec![VenueBlock {
            balance_mode: BalanceMode::Authoritative,
            ..base_venue_block("bybit")
        }];

        assert_eq!(EquityPanelModel::from_snapshot(&delta_snap).rows[0].mode, "\u{0394}");
        assert_eq!(EquityPanelModel::from_snapshot(&auth_snap).rows[0].mode, "auth");
    }

    #[test]
    fn empty_venues_yields_empty_rows_but_keeps_totals() {
        let mut snap = CoreSnapshot::empty("binance", "BTC");
        snap.portfolio.equity_total = 42.0;
        snap.portfolio.missing_prices_total = 7;

        let m = EquityPanelModel::from_snapshot(&snap);

        assert!(m.rows.is_empty());
        assert_eq!(m.equity_total, 42.0);
        assert_eq!(m.missing_prices_total, 7);
    }
}
