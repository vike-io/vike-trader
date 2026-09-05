//! The venue-facing report seam (`ReconClient`) — the normalized replacement for the per-venue
//! ad-hoc `connect`/`reconcile_positions`. Impure (REST I/O) in real venues; `FakeReconClient` is
//! the offline test double. `run_pass` composes fetch → diff → resolve so a golden fixture and the
//! live `ReconManager` exercise the identical path.

use vike_model::{FeeSchedule, FillReport, OrderStatusReport, PositionStatusReport};

use super::diff::diff;
use super::journal_view::JournalView;
use super::resolve::{AdoptContext, PitFn, resolve};
use super::types::{LocalView, Recon, ReconPolicy};

/// An atomic, point-in-time bundle of all three reconcile report kinds — the Nautilus
/// `ExecutionMassStatus` graft. A venue whose API exposes a genuine single-call snapshot can
/// override [`ReconClient::fetch_mass_status`] to return orders + positions + fills read together,
/// giving the reconcile pass cross-report consistency (no per-report skew from three separate
/// round-trips). The DEFAULT trait impl composes the same three fetches the driver ran before this
/// seam existed, so the bundle is byte-identical to today's behavior for every current venue.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MassStatus {
    pub orders: Vec<OrderStatusReport>,
    pub positions: Vec<PositionStatusReport>,
    pub fills: Vec<FillReport>,
}

/// Blocking REST report fetch. Called on the reconcile thread, never the fold thread.
pub trait ReconClient: Send {
    fn fetch_order_status_reports(&self, since: i64) -> Result<Vec<OrderStatusReport>, String>;
    fn fetch_fill_reports(&self, since: i64) -> Result<Vec<FillReport>, String>;
    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String>;

    /// Absolute account balance/cash truth (venue quote currency). Default `Ok(None)` = venue
    /// doesn't surface it / not wired — keeps the trait back-compatible and any un-wired client inert.
    fn fetch_balance(&self) -> Result<Option<f64>, String> {
        Ok(None)
    }

    /// The venue's live account-actual fee schedule (fee model 3/5) — the maker/taker rate the
    /// account is really charged (deribit: the mounted instrument's rate), fetched over the same
    /// signed transport. Default `Ok(None)` = not wired for this venue → the caller keeps the
    /// static [`vike_model::fee_schedule_for`] default (fail-soft). Overridden by
    /// binance/bybit/okx/deribit; the enrichment hook (`vike_mount::resolve_fee_schedule`) prefers
    /// a live `Some` over the static default.
    fn fetch_fee_rates(&self) -> Result<Option<FeeSchedule>, String> {
        Ok(None)
    }

    /// A single atomic snapshot of orders + positions + fills as of `since` — the Nautilus
    /// `ExecutionMassStatus` graft. The DEFAULT impl COMPOSES the three existing fetches in the
    /// exact order the driver fetches them today (orders → fills → positions), so a venue with no
    /// atomic endpoint keeps behavior byte-identical: the returned [`MassStatus`] carries the same
    /// reports, and the first fetch error short-circuits with the same `Err` a separate fetch would
    /// have surfaced. A venue whose API has a real point-in-time bulk endpoint MAY override this to
    /// read all three together for cross-report consistency; no current venue does, so overriding is
    /// purely additive. Never widen this to fetch in a different order — the compose order is what
    /// makes the default provably equivalent to the pre-seam driver path.
    fn fetch_mass_status(&self, since: i64) -> Result<MassStatus, String> {
        let orders = self.fetch_order_status_reports(since)?;
        let fills = self.fetch_fill_reports(since)?;
        let positions = self.fetch_position_status_reports()?;
        Ok(MassStatus { orders, positions, fills })
    }
}

/// One reconcile pass: fetch all three report kinds, diff against local (+ optional journal
/// three-way cross-check), resolve per policy. `generate_missing_orders` becomes [`resolve`]'s
/// [`AdoptContext`] — built HERE because this composition point holds both halves the context
/// needs (this pass's fill reports + the local seen-trade-id set); `false` passes `None`, byte-
/// identical to before this parameter existed.
#[allow(clippy::too_many_arguments)]
pub fn run_pass(
    client: &dyn ReconClient,
    since: i64,
    local: &LocalView,
    journal: Option<&JournalView>,
    policy: &ReconPolicy,
    pit: Option<PitFn>,
    generate_missing_orders: bool,
) -> Result<Recon, String> {
    let orders = client.fetch_order_status_reports(since)?;
    let fills = client.fetch_fill_reports(since)?;
    let positions = client.fetch_position_status_reports()?;
    let fill_order_ids: std::collections::HashSet<String> =
        fills.iter().map(|f| f.venue_order_id.to_string()).collect();
    let adopt = generate_missing_orders.then_some(AdoptContext {
        pass_fill_order_ids: &fill_order_ids,
        seen_trade_ids: local.seen_trade_ids,
    });
    Ok(resolve(diff(&orders, &fills, &positions, local, journal), policy, pit, adopt))
}

/// Offline test double — returns canned reports, records the `since` it was asked for.
#[derive(Default)]
pub struct FakeReconClient {
    pub orders: Vec<OrderStatusReport>,
    pub fills: Vec<FillReport>,
    pub positions: Vec<PositionStatusReport>,
    pub balance: Option<f64>,
}

impl ReconClient for FakeReconClient {
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
        Ok(self.orders.clone())
    }
    fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<FillReport>, String> {
        Ok(self.fills.clone())
    }
    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        Ok(self.positions.clone())
    }
    fn fetch_balance(&self) -> Result<Option<f64>, String> {
        Ok(self.balance)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use vike_model::events::LiquiditySide;

    fn ext_fill(trade_id: &'static str) -> FillReport {
        FillReport {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            trade_id: trade_id.into(),
            venue_order_id: "v9".into(),
            client_order_id: None,
            side: 1,
            last_qty: 1.0,
            last_px: 100.0,
            commission: 0.0,
            commission_asset: "USDT".into(),
            liquidity_side: LiquiditySide::Taker,
            ts: 5,
        }
    }

    #[test]
    fn run_pass_composes_fetch_diff_resolve() {
        let client = FakeReconClient { fills: vec![ext_fill("t1")], ..Default::default() };
        let orders = indexmap::IndexMap::new();
        let seen = HashSet::new();
        let pos = indexmap::IndexMap::new();
        let local = LocalView {
            venue: "binance",
            orders: &orders,
            seen_trade_ids: &seen,
            positions: &pos,
            qty_tol: 1e-9,
            cash: crate::recon::LocalCash::default(),
        };
        let recon =
            run_pass(&client, 0, &local, None, &ReconPolicy::default(), None, false).unwrap();
        assert_eq!(recon.events.len(), 2);
    }

    #[test]
    fn run_pass_propagates_fetch_error() {
        struct FailingClient;
        impl ReconClient for FailingClient {
            fn fetch_order_status_reports(
                &self,
                _since: i64,
            ) -> Result<Vec<OrderStatusReport>, String> {
                Err("boom".to_string())
            }
            fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<FillReport>, String> {
                Ok(Vec::new())
            }
            fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
                Ok(Vec::new())
            }
        }
        let orders = indexmap::IndexMap::new();
        let seen = HashSet::new();
        let pos = indexmap::IndexMap::new();
        let local = LocalView {
            venue: "binance",
            orders: &orders,
            seen_trade_ids: &seen,
            positions: &pos,
            qty_tol: 1e-9,
            cash: crate::recon::LocalCash::default(),
        };
        let err = run_pass(&FailingClient, 0, &local, None, &ReconPolicy::default(), None, false)
            .unwrap_err();
        assert_eq!(err, "boom");
    }

    #[test]
    fn fake_client_reports_balance() {
        let c = FakeReconClient { balance: Some(1234.5), ..Default::default() };
        assert_eq!(c.fetch_balance().unwrap(), Some(1234.5));
    }

    #[test]
    fn default_fetch_balance_is_none() {
        // a trait object with no override returns None
        let c = FakeReconClient::default();
        assert_eq!(c.fetch_balance().unwrap(), None);
    }

    #[test]
    fn default_fetch_fee_rates_is_none() {
        // an un-wired client is fee-inert → the caller keeps the static default (fail-soft).
        let c = FakeReconClient::default();
        assert_eq!(c.fetch_fee_rates().unwrap(), None);
    }

    fn ext_order(coid: &str) -> OrderStatusReport {
        OrderStatusReport {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            venue_order_id: "v1".into(),
            client_order_id: Some(coid.into()),
            side: 1,
            order_type: "LIMIT".into(),
            qty: 1.0,
            filled_qty: 0.0,
            avg_px: 0.0,
            status: "NEW".into(),
            ts: 1,
        }
    }

    fn ext_position() -> PositionStatusReport {
        PositionStatusReport {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            position_side: vike_model::events::PositionSide::Both,
            qty: 2.0,
            avg_px: 100.0,
            ts: 3,
            margin_mode: Default::default(),
            isolated_margin: None,
            delta: None,
        }
    }

    /// The default `fetch_mass_status` bundles EXACTLY the reports the three separate fetches
    /// return (the inert-by-default guarantee: composing the same three reads).
    #[test]
    fn default_mass_status_composes_the_three_fetches() {
        let client = FakeReconClient {
            orders: vec![ext_order("c1")],
            fills: vec![ext_fill("t1")],
            positions: vec![ext_position()],
            balance: None,
        };
        let mass = client.fetch_mass_status(0).unwrap();
        assert_eq!(mass.orders, client.fetch_order_status_reports(0).unwrap());
        assert_eq!(mass.fills, client.fetch_fill_reports(0).unwrap());
        assert_eq!(mass.positions, client.fetch_position_status_reports().unwrap());
    }

    /// A fetch error short-circuits the composed default with the same `Err` a separate fetch
    /// would surface (order report failure here).
    #[test]
    fn default_mass_status_propagates_fetch_error() {
        struct FailingOrders;
        impl ReconClient for FailingOrders {
            fn fetch_order_status_reports(
                &self,
                _since: i64,
            ) -> Result<Vec<OrderStatusReport>, String> {
                Err("boom".to_string())
            }
            fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<FillReport>, String> {
                Ok(Vec::new())
            }
            fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
                Ok(Vec::new())
            }
        }
        assert_eq!(FailingOrders.fetch_mass_status(0).unwrap_err(), "boom");
    }

    /// A venue that OVERRIDES `fetch_mass_status` (a real atomic snapshot endpoint) is used
    /// verbatim — the override wins over the default composition.
    #[test]
    fn overridden_mass_status_is_used() {
        struct AtomicVenue;
        impl ReconClient for AtomicVenue {
            // The three separate fetches return EMPTY — proving the override, not the compose
            // path, is what supplies the bundle.
            fn fetch_order_status_reports(
                &self,
                _since: i64,
            ) -> Result<Vec<OrderStatusReport>, String> {
                Ok(Vec::new())
            }
            fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<FillReport>, String> {
                Ok(Vec::new())
            }
            fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
                Ok(Vec::new())
            }
            fn fetch_mass_status(&self, _since: i64) -> Result<MassStatus, String> {
                Ok(MassStatus {
                    orders: vec![ext_order("atomic")],
                    positions: vec![ext_position()],
                    fills: vec![ext_fill("atomic")],
                })
            }
        }
        let mass = AtomicVenue.fetch_mass_status(0).unwrap();
        assert_eq!(mass.orders.len(), 1);
        assert_eq!(mass.fills.len(), 1);
        assert_eq!(mass.positions.len(), 1);
        assert_eq!(mass.orders[0].client_order_id.as_deref(), Some("atomic"));
        // The default composition would have returned an empty bundle.
        assert!(AtomicVenue.fetch_order_status_reports(0).unwrap().is_empty());
    }
}
