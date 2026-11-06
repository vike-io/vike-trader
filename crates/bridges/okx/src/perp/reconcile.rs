//! `OkxPerpRest`'s reconcile reads: USDT balance, resting orders, positions snapshot, last price.

use serde_json::json;

use vike_bridge_core::json::json_num;
use vike_bridge_core::transport::VenueApiError;
use vike_exec::ReconcileSnapshot;
use vike_model::ManagedOrder;
use vike_model::events::PositionSide;

use super::{
    OkxPerpRest, PATH_ACCOUNT, PATH_ORDERS_PENDING, PATH_POSITIONS, PATH_TICKER, map_okx_open_order,
};
use crate::transport::{OkxTransport, unwrap_okx};

impl<T: OkxTransport> OkxPerpRest<T> {
    /// USDT TOTAL cash (`cashBal`, free + frozen — matches Bybit walletBalance / Binance
    /// balance). Default-safe: any failure → 0.0.
    fn fetch_usdt_balance(&self) -> f64 {
        let Ok(result) = self.call(PATH_ACCOUNT, "GET", &[]) else {
            return 0.0;
        };
        for acct in result.as_array().unwrap_or(&vec![]) {
            for d in acct.get("details").and_then(|d| d.as_array()).unwrap_or(&vec![]) {
                if d.get("ccy").and_then(|c| c.as_str()) == Some("USDT") {
                    return d.get("cashBal").and_then(json_num).unwrap_or(0.0);
                }
            }
        }
        0.0
    }

    /// Signed GET /api/v5/trade/orders-pending {instType:SWAP, instId} → the venue's resting orders
    /// as reconcile-seeded [`ManagedOrder`]s (`sz` CONTRACTS → base via `ct_val`, so the seeded qty
    /// matches how the order was submitted). Perps previously returned an EMPTY `open_orders`, so
    /// `apply_snapshot`'s stale-order reap could never fire on a perp; this closes that. Best-effort
    /// by contract: a REST hiccup returns `Vec::new()` (skip the order-fetch, KEEP the position
    /// snapshot) rather than failing the whole reconcile — the "REST hiccup ⇒ skip tick" convention.
    /// Reconcile path only, already off the fold.
    fn fetch_open_orders(&self) -> Vec<ManagedOrder> {
        let Ok(data) = self.call(
            PATH_ORDERS_PENDING,
            "GET",
            &[("instType", json!("SWAP")), ("instId", json!(self.symbol))],
        ) else {
            return Vec::new();
        };
        data.as_array()
            .map(|rows| rows.iter().map(|o| map_okx_open_order(o, self.ct_val)).collect())
            .unwrap_or_default()
    }

    /// GET positions: `pos` is signed CONTRACTS → base via ct_val. net → one BOTH leg;
    /// hedge long/short rows carry sides. Flat → zero row.
    pub fn reconcile_positions(&self) -> Result<ReconcileSnapshot, VenueApiError> {
        let data = self.call(
            PATH_POSITIONS,
            "GET",
            &[("instType", json!("SWAP")), ("instId", json!(self.symbol))],
        )?;
        let bal = self.fetch_usdt_balance();
        // Best-effort resting-order fetch (empty on a REST hiccup) — feeds apply_snapshot's
        // stale-order reap. Fetched once; moved into whichever return path fires.
        let open_orders = self.fetch_open_orders();
        // (base_qty, avg, mark, side, reported margin mode + isolated margin balance)
        type Leg = (f64, f64, f64, PositionSide, (vike_model::MarginMode, Option<f64>));
        let mut legs: Vec<Leg> = Vec::new();
        for p in data.as_array().unwrap_or(&vec![]) {
            let contracts = p.get("pos").and_then(json_num).unwrap_or(0.0); // already signed
            if contracts == 0.0 {
                continue;
            }
            legs.push((
                self.to_base(contracts),
                p.get("avgPx").and_then(json_num).unwrap_or(0.0),
                p.get("markPx").and_then(json_num).unwrap_or(0.0),
                crate::event_mapper::pside(p),
                // `mgnMode`/`margin` off the SAME row — the shared read
                // `recon_client::parse_mgn_mode` fixture-tests. The REQUEST side honors
                // `OrderRequest.margin_mode` via `swap_td_mode` (unset = "cross").
                crate::recon_client::parse_mgn_mode(p),
            ));
        }
        if legs.is_empty() {
            // Flat: no margin info reported — empty `position_margin` = carry priors forward.
            return Ok(ReconcileSnapshot {
                positions: vec![(self.symbol.clone(), 0.0)],
                open_orders,
                position_avg_px: vec![(self.symbol.clone(), 0.0)],
                position_mark_px: vec![(self.symbol.clone(), 0.0)],
                position_sides: Vec::new(),
                balance: bal,
                position_margin: Vec::new(),
            });
        }
        let hedge = legs.iter().any(|(_, _, _, sd, _)| *sd != PositionSide::Both);
        Ok(ReconcileSnapshot {
            positions: legs.iter().map(|(q, ..)| (self.symbol.clone(), *q)).collect(),
            open_orders,
            position_avg_px: legs.iter().map(|(_, a, ..)| (self.symbol.clone(), *a)).collect(),
            position_mark_px: legs.iter().map(|(_, _, m, ..)| (self.symbol.clone(), *m)).collect(),
            position_sides: if hedge {
                legs.iter().map(|(_, _, _, sd, _)| (self.symbol.clone(), *sd)).collect()
            } else {
                Vec::new()
            },
            balance: bal,
            position_margin: legs
                .iter()
                .map(|(.., (mode, iso))| (self.symbol.clone(), *mode, *iso))
                .collect(),
        })
    }

    pub fn last_price(&self) -> Result<f64, VenueApiError> {
        let data = self.transport.public(
            &self.base_url,
            PATH_TICKER,
            &[("instId", self.symbol.clone())],
        )?;
        let data = unwrap_okx(data)?;
        Ok(data
            .as_array()
            .and_then(|d| d.first())
            .and_then(|t| t.get("last"))
            .and_then(json_num)
            .unwrap_or(0.0))
    }
}
