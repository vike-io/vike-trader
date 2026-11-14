//! Live-`RiskGate` pre-fetch: a dedicated, best-effort `contractDetails` round-trip that resolves
//! the engine symbol's tick/size grid into `vike_model::SymbolProperties`, for
//! `VenueAdapterIbkr`'s start (`crates/bridges/vike-ibkr/src/adapter.rs`) — the sibling of every
//! other bridge's `fetch_{bybit,binance,alpaca,…}_properties(&config, symbol)`.
//!
//! IBKR has NO keyless public grid endpoint (the crypto venues' `exchangeInfo` analog): a
//! contract's tick/size grid lives behind a live `contractDetails` request. The exec connection's
//! transport is consumed by the `ExecutionClientActor` thread (no synchronous handle), so this opens its OWN
//! throwaway ibapi socket connection — mirroring [`crate::HistoricalFetcher::connect`] — on the
//! dedicated `data_client_id`, issues one `contract_details`, and drops the connection. Best-effort:
//! `None` on ANY failure (unparseable symbol, cpapi backend, unreachable Gateway, empty reply), so
//! the start keeps the permissive default limits — byte-identical to a failed pre-fetch on the
//! crypto arms. Behind `ibkr-socket` (where ibapi exists). Ports nothing.
//!
//! **The tick is the band of the contract's price, as an admitted symbol's is** (decision 0220, the audit's G5): on the
//! same connection the pre-fetch reads the market rules the details name and a delayed or closing price (at most
//! `crate::instrument::any_symbol::socket::PRICE_READ_WAIT`), puts the tick on THE one grid rule ([`on_the_band`]) and notes the rules
//! for the exec's grid check (`crate::instrument::tick_grid`), so a mounted stock is rounded, and refused, exactly as the same stock
//! admitted by name. `minTick` alone ("the smallest of the different minimum tick sizes") rounded a SAP limit near 200 EUR
//! to 0.0001 where IB steps by 0.05, and IB refused it with `[110]`, which vike named nothing about. Once the details have
//! answered, a rule or price read that fails, or does not answer within the bound, keeps `minTick` with a WARN
//! ([`bounded_read`]): it never costs the mount its grid.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use ibapi::client::blocking::Client;
use ibapi::contracts::Contract;

use crate::config::IbkrConfig;
use crate::feed::price::session::session_for;
use crate::feed::socket::{apply_data_type, pump_contract};
use crate::instrument::any_symbol::socket::{band_price, rule_bands};
use crate::instrument::any_symbol::{IbkrPropertiesError, TickGrid, grid_tick_of};
use crate::instrument::contract::{
    IbkrContract, contract_details_to_properties, parse_simplified, resolve_multiplier,
};
use crate::transport::gateway_gate::gate_for;
use crate::transport::socket::OPEN_DEADLINE;
use vike_model::SymbolProperties;
use vike_venues::IbkrBackend;

/// Best-effort `contractDetails` → `SymbolProperties` for the live-`RiskGate` start. Returns `None`
/// (⇒ the caller keeps the permissive default limits) when:
/// - the canonical `SYMBOL.EXCHANGE.CURRENCY` symbol is unparseable;
/// - the backend is `cpapi` — that path has no socket listener on `config.port`, so a socket connect
///   would only fast-fail; its contract-info REST grid is a separate follow-up;
/// - the Gateway/TWS is unreachable, or IB returns no details for the contract.
///
/// NEVER panics. The `data_client_id` connection is a transient one-shot: it connects → fetches →
/// drops synchronously at start time. A data connection that is ALREADY up under that id (the gatewayhub's
/// price stream of an admitted stock, shared with the any-symbol lookup: `crate::feed::price::session`) is used
/// as it is instead, because a second client under the id is refused (error 326).
pub fn fetch_ibkr_properties(config: &IbkrConfig, symbol: &str) -> Option<SymbolProperties> {
    fetch_within(config, symbol, OPEN_DEADLINE)
}

/// [`fetch_ibkr_properties`] with the network half under a `within` bound (`GatewayGate::dial`): a gateway that
/// takes the connection and never answers leaves the permissive default standing, it does not hold the start.
pub(crate) fn fetch_within(
    config: &IbkrConfig,
    symbol: &str,
    within: Duration,
) -> Option<SymbolProperties> {
    // cpapi speaks REST, not the socket protocol — skip the doomed connect and keep permissive.
    if config.backend == IbkrBackend::Cpapi {
        return None;
    }
    let contract = match parse_simplified(symbol) {
        Ok(c) => c,
        Err(why) => {
            tracing::warn!(%symbol, reason = %why, "ibkr properties: unusable symbol");
            return None;
        }
    };
    let config = config.clone();
    let name = symbol.to_string();
    let gateway = format!("{}:{}", config.host, config.port);
    bounded_read(symbol, &gateway, within, move |details| {
        fetch_blocking(&config, &contract, &name, details)
    })
}

/// What the blocking read answers: the details' grid (`minTick`) and the band read off the rules and the price.
type Read = (SymbolProperties, Result<(f64, TickGrid), IbkrPropertiesError>);

/// **The pre-fetch's `read` under the gateway's turn and `within`** (`GatewayGate::dial`: one dial at a time per
/// gateway, spaced after the previous one), settled ON THIS THREAD by [`on_the_band`]. `read` keeps the details' grid in
/// its slot the moment `contractDetails` answers, so a rule or price read the bound runs out on is a band NOT READ, the
/// same as a rule the venue refused: the mount keeps `minTick`, with the same WARN. Only a read that ran out before the
/// details answered leaves the permissive default standing. The abandoned read's late answer is dropped unsettled, so it
/// notes nothing for the exec (the rules are noted here, never on the read's thread).
fn bounded_read(
    symbol: &str,
    gateway: &str,
    within: Duration,
    read: impl FnOnce(&Mutex<Option<SymbolProperties>>) -> Option<Read> + Send + 'static,
) -> Option<SymbolProperties> {
    let details = Arc::new(Mutex::new(None));
    let slot = Arc::clone(&details);
    match gate_for(gateway).dial("grid pre-fetch", within, move || Ok(read(&slot))).0 {
        Ok(answer) => answer.map(|(properties, band)| on_the_band(symbol, properties, band)),
        Err(e) => {
            let kept = *details.lock().unwrap_or_else(PoisonError::into_inner);
            match kept {
                Some(properties) => Some(on_the_band(
                    symbol,
                    properties,
                    Err(IbkrPropertiesError::Unavailable(format!(
                        "ibkr: the market rules or the price of {symbol} did not answer: {e}"
                    ))),
                )),
                None => {
                    tracing::warn!(%symbol, error = %e, "ibkr properties: pre-fetch abandoned");
                    None
                }
            }
        }
    }
}

/// The blocking `ibapi` round trip of [`fetch_within`]; `symbol` is the engine's name, the exec's key for the rules.
/// The details' grid goes into `details` as soon as it is known ([`bounded_read`]).
fn fetch_blocking(
    config: &IbkrConfig,
    contract: &IbkrContract,
    symbol: &str,
    details: &Mutex<Option<SymbolProperties>>,
) -> Option<Read> {
    // A data connection that is up under this id (the price stream's, shared with the any-symbol lookup:
    // `crate::feed::price::session`) is used as it is: a second client under the id is error 326 and would leave the
    // grid permissive. With none up, the throwaway connection of old.
    let live = session_for(config).live();
    let own;
    let client: &Client = match &live {
        Some(lease) => lease.client(),
        None => {
            let addr = format!("{}:{}", config.host, config.port);
            own = Client::connect(&addr, config.data_client_id).ok()?;
            &own
        }
    };
    let ibkr = pump_contract(contract);
    let rows = client.contract_details(&ibkr).ok()?;
    // The multiplier is read across EVERY row before the first is taken: the reply is a ladder
    // (`resolve_multiplier`'s doc), and only a unanimous one — or the operator's claim — names
    // the started contract's size.
    let multiplier = resolve_multiplier(
        contract.multiplier.as_deref(),
        rows.iter().map(|d| d.contract.multiplier.as_str()),
    );
    // A SMART-routed equity can return one detail row per exchange; the first carries the
    // consolidated tick/size grid the RiskGate needs (best-effort).
    let first = rows.into_iter().next()?;
    // The class comes from the contract WE asked about, not from the reply: `contractDetails`
    // answers a grid, and a grid does not say what it is a grid for. `parse_simplified` already
    // resolved the operator's canonical to a `SecType` above, and that is IBKR's own word.
    let properties = contract_details_to_properties(
        &contract.sec_type,
        first.min_tick,
        // ibapi 4 reports an unsent lot grid / minimum as `None`; 3.x said `0.0`. `0.0` IS the inert
        // case `contract_details_to_properties` documents (`nz_step` folds it to unconstrained).
        first.size_increment.unwrap_or(0.0),
        first.min_size.unwrap_or(0.0),
        multiplier,
    );
    *details.lock().unwrap_or_else(PoisonError::into_inner) = Some(properties);
    // A joined connection may be a session `ibapi` rebuilt, and a throwaway one was never told the type: a delayed-only
    // account would refuse the price read `[354]` (decision 0220: said before every subscribe).
    apply_data_type(client, config.market_data_type, "grid pre-fetch");
    // The price of the contract the details named (its conId), so a ticker listed twice is not ambiguous to the read.
    let price = band_price(client, &Contract { contract_id: first.contract.contract_id, ..ibkr });
    let band = grid_tick_of(
        &|id| rule_bands(client, id),
        properties.tick_size,
        &first.market_rule_ids,
        symbol,
        price,
    );
    Some((properties, band))
}

/// **A mount's tick on THE one grid rule of an admitted symbol**: `band` is what `crate::instrument::any_symbol`'s `grid_tick_of`
/// (decision 0220) read off the contract's rules at its price, the increment of the band the price falls in (the finest
/// band with no price). The tick moves to it and the rules are noted under the engine's name `symbol` for the exec's grid
/// check (`crate::instrument::tick_grid`'s `note`), so a mount is rounded, and refused, in the same words as the same stock admitted
/// by name. Rules the venue did not answer (in time or at all), or that do not nest, keep `properties`' `minTick` and note
/// nothing (the exec's check then stays fail-open for the symbol): the pre-fetch is best-effort and refuses no mount,
/// where the facts would have refused the name.
pub(crate) fn on_the_band(
    symbol: &str,
    properties: SymbolProperties,
    band: Result<(f64, TickGrid), IbkrPropertiesError>,
) -> SymbolProperties {
    match band {
        Ok((tick, grid)) => {
            crate::instrument::tick_grid::note(symbol, &grid.rules, grid.price);
            // Once per start: the line an operator (and the paper smoke) reads the mount's tick off.
            tracing::info!(
                %symbol,
                min_tick = properties.tick_size,
                tick,
                price = ?grid.price,
                rules = grid.rules.len(),
                "ibkr properties: the grid tick is the band of the contract's price"
            );
            SymbolProperties { tick_size: tick, ..properties }
        }
        Err(why) => {
            tracing::warn!(
                %symbol,
                min_tick = properties.tick_size,
                reason = ?why,
                "ibkr properties: the market rules were not read, so the grid keeps minTick and the exec does not \
                 hold a price to the venue's bands"
            );
            properties
        }
    }
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
        let (_gateway, config) = silent_gateway();
        let began = Instant::now();
        assert!(fetch_within(&config, "AAPL.SMART.USD", TEST_BOUND).is_none());
        assert!(began.elapsed() < QUICK, "waited {:?}", began.elapsed());
    }

    /// **A rule request that never answers is a band NOT read, the same as a rule the venue refused**: the details had
    /// answered, so within the bound the mount keeps `minTick` with the one WARN, never the permissive default (a lost
    /// grid); and the abandoned read's late answer notes nothing for the exec.
    #[test]
    fn a_rule_request_that_never_answers_keeps_min_tick_with_the_warn() {
        use std::sync::mpsc;

        use crate::instrument::any_symbol::PriceBand;
        use crate::instrument::contract::{SecType, contract_details_to_properties};
        const SYMBOL: &str = "HUNG.SMART.EUR";
        let properties = contract_details_to_properties(&SecType::Stk, 0.0001, 1.0, 1.0, 0.0);
        let (release, held) = mpsc::channel::<()>();
        let (done, finished) = mpsc::channel::<()>();
        let began = Instant::now();
        let (got, logs) = vike_log::capture::captured(|| {
            bounded_read(SYMBOL, "rule-never-answers.test:1", TEST_BOUND, move |details| {
                *details.lock().unwrap() = Some(properties);
                let band = grid_tick_of(
                    &|_| {
                        // Answered only once the bound has given up on the read.
                        let _ = held.recv();
                        Ok(vec![
                            PriceBand { low_edge: 0.0, increment: 0.0001 },
                            PriceBand { low_edge: 200.0, increment: 0.05 },
                        ])
                    },
                    properties.tick_size,
                    &["1874".to_string()],
                    SYMBOL,
                    Some(213.0),
                );
                let _ = done.send(());
                Some((properties, band))
            })
        });
        assert!(began.elapsed() < QUICK, "waited {:?}", began.elapsed());
        assert_eq!(got, Some(properties), "minTick, not the permissive default");
        let warned: Vec<_> = logs
            .iter()
            .filter(|e| e.level == tracing::Level::WARN && e.message.contains("keeps minTick"))
            .collect();
        assert_eq!(warned.len(), 1, "the WARN of an unreadable rule, once: {logs:?}");
        assert!(
            warned[0].field("reason").is_some_and(|r| r.contains("did not answer")),
            "{logs:?}"
        );
        // The late answer ends the read on its abandoned thread; nothing is noted, so the exec stays fail-open for it.
        release.send(()).expect("the read still waits");
        finished.recv_timeout(Duration::from_secs(5)).expect("the abandoned read ends");
        let order = vike_model::OrderRequest {
            symbol: SYMBOL.into(),
            side: 1,
            qty: 1.0,
            order_type: vike_model::OrderType::Limit,
            price: Some(213.37),
            ..Default::default()
        };
        assert_eq!(crate::instrument::tick_grid::refusal(&order), None, "no rules noted");
    }
}
