//! The raw demo-fapi venue the perp ALGO drills share (`perp_algo_smoke.rs` and
//! `binance_perp_algo_restore_smoke.rs`; the modes and classes-exec drills take its client too):
//! the signed client, its position and price reads, the
//! leave-flat `Drop` guard and the order builder, on the one drill symbol. The `binance_smokes` binary
//! declares it ONCE by a plain `mod perp_algo_venue;` and each drill reads it as
//! `crate::perp_algo_venue`; a drill-specific read stays in the drill as its own `impl Venue`.

use serde_json::Value;
use vike_binance::perp::{DEMO_FAPI_REST, PATH_POSITIONS};
use vike_bridge_core::signer::BinanceHmacSigner;
use vike_bridge_core::transport::{RestTransport, UreqTransport, VenueApiError};
use vike_model::{OrderRequest, OrderType, now_ms};
use vike_secrets::credentials::Credentials;

/// The drill's symbol: not BTCUSDT, which the lane twin and the market-maker smokes mount.
pub const SYMBOL: &str = "ETHUSDT";

pub struct Venue {
    pub transport: UreqTransport,
    signer: BinanceHmacSigner,
}

impl Venue {
    pub fn new(creds: &Credentials) -> Self {
        // DEMO ONLY: the one base URL this file ever signs against.
        assert_eq!(DEMO_FAPI_REST, "https://demo-fapi.binance.com", "demo host constant moved");
        Self {
            transport: UreqTransport::new("binance"),
            signer: BinanceHmacSigner::new(creds, now_ms),
        }
    }

    pub fn call(
        &self,
        method: &str,
        path: &str,
        params: &[(&str, String)],
    ) -> Result<Value, VenueApiError> {
        self.transport.signed(DEMO_FAPI_REST, path, method, params, &self.signer)
    }

    pub fn symbol(&self) -> (&'static str, String) {
        ("symbol", SYMBOL.to_string())
    }

    /// The signed position amount on `symbol` (0.0 when flat), or the venue's error.
    pub fn try_position(&self, symbol: &str) -> Result<f64, VenueApiError> {
        let rows = self.call("GET", PATH_POSITIONS, &[("symbol", symbol.to_string())])?;
        Ok(rows
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| r.get("positionAmt")?.as_str()?.parse::<f64>().ok())
            .sum())
    }

    /// [`Self::try_position`] for the drill's body, where a failed read is a failed drill.
    pub fn position(&self, symbol: &str) -> f64 {
        self.try_position(symbol).expect("positionRisk")
    }

    pub fn last_price(&self, symbol: &str) -> f64 {
        let t = self
            .transport
            .public(DEMO_FAPI_REST, "/fapi/v1/ticker/price", &[("symbol", symbol.to_string())])
            .expect("ticker/price");
        t["price"].as_str().and_then(|p| p.parse().ok()).expect("a ticker price")
    }
}

/// Leaves the symbol flat and clean whatever happened (a `Drop` runs on a panicking test too).
pub struct LeaveFlat {
    pub creds: Credentials,
}

impl Drop for LeaveFlat {
    fn drop(&mut self) {
        let v = Venue::new(&self.creds);
        let _ = v.call("DELETE", "/fapi/v1/algoOpenOrders", &[v.symbol()]);
        let _ = v.call("DELETE", "/fapi/v1/allOpenOrders", &[v.symbol()]);
        // ⚠ NEVER panic in here: this runs while a failed assertion unwinds, and a second panic aborts the
        // process before the position is flattened. Flatten first (retrying the read once), report after.
        let amt = v.try_position(SYMBOL).or_else(|_| v.try_position(SYMBOL));
        match amt {
            Ok(amt) if amt != 0.0 => {
                let params = vec![
                    v.symbol(),
                    ("side", if amt > 0.0 { "SELL" } else { "BUY" }.to_string()),
                    ("type", "MARKET".to_string()),
                    ("quantity", format!("{}", amt.abs())),
                    ("reduceOnly", "true".to_string()),
                ];
                let r = v.call("POST", "/fapi/v1/order", &params);
                println!(
                    "CLEANUP flatten {amt}: {}",
                    r.map_or_else(|e| e.to_string(), |_| "sent".into())
                );
            }
            Ok(_) => {}
            Err(e) => {
                println!("CLEANUP could not read the {SYMBOL} position ({e}): CHECK IT BY HAND")
            }
        }
    }
}

/// One perp order on `SYMBOL`; a trigger makes it a reduce-only conditional.
pub fn order(
    coid: &str,
    side: i32,
    qty: f64,
    kind: OrderType,
    trigger: Option<f64>,
) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.to_string(),
        venue: "binance".to_string(),
        symbol: format!("{SYMBOL}.P"),
        side,
        qty,
        order_type: kind,
        trigger_price: trigger,
        reduce_only: trigger.is_some(),
        ts: now_ms(),
        ..Default::default()
    }
}
