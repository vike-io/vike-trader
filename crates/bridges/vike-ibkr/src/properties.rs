//! Live-`RiskGate` pre-fetch: a dedicated, best-effort `contractDetails` round-trip that resolves
//! the engine symbol's tick/size grid into `vike_model::SymbolProperties`, for
//! `IbkrVenueAdapter`'s start (`crates/bridges/vike-ibkr/src/adapter.rs`) — the sibling of every
//! other bridge's `fetch_{bybit,binance,alpaca,…}_properties(&config, symbol)`.
//!
//! IBKR has NO keyless public grid endpoint (the crypto venues' `exchangeInfo` analog): a
//! contract's tick/size grid lives behind a live `contractDetails` request. The exec connection's
//! transport is consumed by the `ExecActor` thread (no synchronous handle), so this opens its OWN
//! throwaway ibapi socket connection — mirroring [`crate::HistoricalFetcher::connect`] — on the
//! dedicated `data_client_id`, issues one `contract_details`, and drops the connection. Best-effort:
//! `None` on ANY failure (unparseable symbol, cpapi backend, unreachable Gateway, empty reply), so
//! the start keeps the permissive default limits — byte-identical to a failed pre-fetch on the
//! crypto arms. Behind `ibkr-socket` (where ibapi exists). Ports nothing.

use std::time::Duration;

use ibapi::client::blocking::Client;

use crate::config::IbkrConfig;
use crate::contract::{
    IbkrContract, contract_details_to_properties, parse_simplified, resolve_multiplier,
};
use crate::market_feed::pump_contract;
use crate::transport::gateway_gate::gate_for;
use crate::transport::socket::OPEN_DEADLINE;
use vike_model::SymbolProperties;
use vike_venues::IbkrBackend;

/// Best-effort `contractDetails` → `SymbolProperties` for the live-`RiskGate` start. Returns `None`
/// (⇒ the caller keeps the permissive default limits) when:
/// - the canonical `SYMBOL.EXCHANGE.CURRENCY` symbol is unparseable;
/// - the backend is `cpapi` — that path has no socket listener on `cfg.port`, so a socket connect
///   would only fast-fail; its contract-info REST grid is a separate follow-up;
/// - the Gateway/TWS is unreachable, or IB returns no details for the contract.
///
/// NEVER panics. The `data_client_id` connection is a transient one-shot: it connects → fetches →
/// drops synchronously at start time, fully torn down before a live market-data feed could claim
/// that same id, so it never contends the feed. (That feed was wired post-mount by the GUI shell,
/// `vike-app`, as this said until 2026-09-28; no root wires an IBKR market-data feed today —
/// `crate::market_feed` is started by nothing outside this crate.)
pub fn fetch_ibkr_properties(cfg: &IbkrConfig, symbol: &str) -> Option<SymbolProperties> {
    fetch_within(cfg, symbol, OPEN_DEADLINE)
}

/// [`fetch_ibkr_properties`] with the network half under a `within` bound (`GatewayGate::dial`): a gateway that
/// takes the connection and never answers leaves the permissive default standing, it does not hold the start.
pub(crate) fn fetch_within(
    cfg: &IbkrConfig,
    symbol: &str,
    within: Duration,
) -> Option<SymbolProperties> {
    // cpapi speaks REST, not the socket protocol — skip the doomed connect and keep permissive.
    if cfg.backend == IbkrBackend::Cpapi {
        return None;
    }
    let contract = match parse_simplified(symbol) {
        Ok(c) => c,
        Err(why) => {
            tracing::warn!(%symbol, reason = %why, "ibkr properties: unusable symbol");
            return None;
        }
    };
    let cfg = cfg.clone();
    // One dial at a time per gateway, spaced after the previous one (`GatewayGate::dial`).
    let gate = gate_for(&format!("{}:{}", cfg.host, cfg.port));
    match gate.dial("grid pre-fetch", within, move || Ok(fetch_blocking(&cfg, &contract))).0 {
        Ok(props) => props,
        Err(e) => {
            tracing::warn!(%symbol, error = %e, "ibkr properties: pre-fetch abandoned");
            None
        }
    }
}

/// The blocking `ibapi` round trip of [`fetch_within`].
fn fetch_blocking(cfg: &IbkrConfig, contract: &IbkrContract) -> Option<SymbolProperties> {
    let addr = format!("{}:{}", cfg.host, cfg.port);
    let client = Client::connect(&addr, cfg.data_client_id).ok()?;
    let ib = pump_contract(contract);
    let details = client.contract_details(&ib).ok()?;
    // The multiplier is read across EVERY row before the first is taken: the reply is a ladder
    // (`resolve_multiplier`'s doc), and only a unanimous one — or the operator's claim — names
    // the started contract's size.
    let multiplier = resolve_multiplier(
        contract.multiplier.as_deref(),
        details.iter().map(|d| d.contract.multiplier.as_str()),
    );
    // A SMART-routed equity can return one detail row per exchange; the first carries the
    // consolidated tick/size grid the RiskGate needs (best-effort).
    let first = details.into_iter().next()?;
    // The class comes from the contract WE asked about, not from the reply: `contractDetails`
    // answers a grid, and a grid does not say what it is a grid for. `parse_simplified` already
    // resolved the operator's canonical to a `SecType` above, and that is IBKR's own word.
    Some(contract_details_to_properties(
        &contract.sec_type,
        first.min_tick,
        // ibapi 4 reports an unsent lot grid / minimum as `None`; 3.x said `0.0`. `0.0` IS the inert
        // case `contract_details_to_properties` documents (`nz_step` folds it to unconstrained).
        first.size_increment.unwrap_or(0.0),
        first.min_size.unwrap_or(0.0),
        multiplier,
    ))
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::transport::socket::socket_tests::{QUICK, TEST_BOUND, silent_gateway};

    /// A gateway that takes the connection and never answers leaves the permissive default standing,
    /// within the bound, instead of holding the start.
    #[test]
    fn the_grid_prefetch_gives_up_on_a_silent_gateway() {
        let (_gateway, cfg) = silent_gateway();
        let began = Instant::now();
        assert!(fetch_within(&cfg, "AAPL.SMART.USD", TEST_BOUND).is_none());
        assert!(began.elapsed() < QUICK, "waited {:?}", began.elapsed());
    }
}
