//! Live-`RiskGate` pre-fetch: a dedicated, best-effort `contractDetails` round-trip that resolves
//! the mounted symbol's tick/size grid into `vike_model::SymbolProperties`, for
//! `vike_mount::make_engine`'s ibkr arm — the sibling of every other bridge's
//! `fetch_{bybit,binance,alpaca,…}_properties(&config, symbol)`.
//!
//! IBKR has NO keyless public grid endpoint (the crypto venues' `exchangeInfo` analog): a
//! contract's tick/size grid lives behind a live `contractDetails` request. The exec connection's
//! transport is consumed by the `ExecActor` thread (no synchronous handle), so this opens its OWN
//! throwaway ibapi socket connection — mirroring [`crate::HistoricalFetcher::connect`] — on the
//! dedicated `data_client_id`, issues one `contract_details`, and drops the connection. Best-effort:
//! `None` on ANY failure (unparseable symbol, cpapi backend, unreachable Gateway, empty reply), so
//! the mount keeps the permissive default limits — byte-identical to a failed pre-fetch on the
//! crypto arms. Behind `ibkr-socket` (where ibapi exists). Ports nothing.

use ibapi::client::blocking::Client;

use crate::config::{IbkrBackend, IbkrConfig};
use crate::contract::{contract_details_to_properties, parse_simplified};
use crate::market_feed::pump_contract;
use vike_model::SymbolProperties;

/// Best-effort `contractDetails` → `SymbolProperties` for the live-`RiskGate` mount. Returns `None`
/// (⇒ the caller keeps the permissive default limits) when:
/// - the canonical `SYMBOL.EXCHANGE.CURRENCY` symbol is unparseable;
/// - the backend is `cpapi` — that path has no socket listener on `cfg.port`, so a socket connect
///   would only fast-fail; its contract-info REST grid is a separate follow-up;
/// - the Gateway/TWS is unreachable, or IB returns no details for the contract.
///
/// NEVER panics. The `data_client_id` connection is a transient one-shot: it connects → fetches →
/// drops synchronously at mount time, fully torn down before the live market-data feed (wired
/// post-mount by vike-app) claims that same id, so it never contends the feed.
pub fn fetch_ibkr_properties(cfg: &IbkrConfig, symbol: &str) -> Option<SymbolProperties> {
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
    let addr = format!("{}:{}", cfg.host, cfg.port);
    let client = Client::connect(&addr, cfg.data_client_id).ok()?;
    let ib = pump_contract(&contract);
    let details = client.contract_details(&ib).ok()?;
    // A SMART-routed equity can return one detail row per exchange; the first carries the
    // consolidated tick/size grid the RiskGate needs (best-effort).
    let first = details.into_iter().next()?;
    // The class comes from the contract WE asked about, not from the reply: `contractDetails`
    // answers a grid, and a grid does not say what it is a grid for
    // (`docs/decisions/0061-an-instrument-names-its-kind.md`). `parse_simplified` already resolved
    // the operator's canonical to a `SecType` above, and that is IBKR's own word.
    Some(contract_details_to_properties(
        &contract.sec_type,
        first.min_tick,
        first.size_increment,
        first.min_size,
    ))
}
