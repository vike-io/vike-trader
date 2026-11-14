//! **What the gatewayhub asks of okx to admit a symbol the engine was not started on** (decision 0173, decision 0232,
//! decision 0258's okx amendment): the venue's half of the list-answered symbol facts ([`SymbolPropertiesListSpecOkx`]):
//! which network's list answers (demo and mainnet share one host; the demo is a header, so a list of its own), which
//! asks are point reads, and what each class of row is (`okx_class_facts`: the grid in BASE units for a linear
//! USDT contract, `Cash` for a USDT spot pair, `Inverse` on the CONTRACTS grid for a coin-margined one, a future
//! refused inside its last day at this crate's own [`EXPIRY_GUARD_MS`]). The listing, its bounds, the grid checks
//! every venue shares and the wire are the gatewayhub's (`crates/vike-gatewayhub/src/trade/adapters/symbol_facts.rs`).
//! The base-unit grid is held equal to the mounted path's `crate::execution::rest::properties_in_base` by
//! `symbol_properties_tests.rs`.

use vike_bridge_core::symbol_properties::{
    SymbolPropertiesListAnswer, SymbolPropertiesListSpec, SymbolPropertiesWords,
};
use vike_model::{Contract, ContractValue, Instrument, Margining, SymbolProperties};

use crate::instrument::EXPIRY_GUARD_MS;

/// **The listing key of OKX's DEMO network**: demo and mainnet share one host and the demo is the
/// `x-simulated-trading: 1` header, which changes the list the public endpoint serves (2026-10-31: 188 SWAP rows
/// against 532, and three USDT-linear dated futures, `QTUM-USDT-261106` among them, that mainnet answers `51001`),
/// so a `demo` account asked of the mainnet list could admit no demo-only future and could admit a swap its network
/// does not list. Keyed apart from mainnet's; the SPOT and dated one-symbol reads are asked under this key too, so
/// the two networks keep separate slots.
pub const OKX_DEMO_LIST: &str = "okx@demo";

/// **okx's symbol facts** (decision 0232): the symbol is the venue's own instId (`ETH-USDT-SWAP`). A linear
/// USDT-settled SWAP or dated future with its grid converted from CONTRACTS to BASE units (`okx_in_base`), a
/// USDT-quoted SPOT pair answered `Cash` with a margin rate of 1 (`okx_class_facts`), or a coin-margined SWAP or dated
/// future answered `Inverse(ctVal x ctMult)` on its CONTRACTS grid (decision 0258's okx amendment,
/// `okx_inverse_facts`); an option and a USDC or USD settled one are refused by name. A `demo` account is asked of
/// the simulated-trading list ([`OKX_DEMO_LIST`]), which holds the demo-only dated futures, a `live` one of mainnet's.
#[derive(Debug, Clone, Copy, Default)]
pub struct SymbolPropertiesListSpecOkx;

impl SymbolPropertiesListSpec for SymbolPropertiesListSpecOkx {
    fn venue(&self) -> &'static str {
        "okx"
    }

    fn list_key(&self, network: &str) -> &'static str {
        if network == "testnet" { OKX_DEMO_LIST } else { "okx" }
    }

    fn covers(&self, list_key: &str, _account_listed: bool) -> Option<&'static str> {
        Some(if list_key == OKX_DEMO_LIST {
            "USDT-quoted spot pairs, linear USDT-settled SWAP perpetuals and dated futures of OKX's \
             simulated-trading network, the network this account trades on; that list is not mainnet's"
        } else {
            "USDT-quoted spot pairs, linear USDT-settled SWAP perpetuals and dated futures"
        })
    }

    /// A SPOT pair and a dated future (decision 0232): one point read each (`?instType=&instId=`); the SWAP list
    /// stays one bulk read, kept per network and shared by every swap ask.
    fn one_at_a_time(&self, key: &str) -> bool {
        !key.ends_with("-SWAP")
    }

    /// The lister is told which NETWORK is meant, by the listing key.
    fn lister_key<'k>(&self, list_key: &'k str) -> &'k str {
        list_key
    }

    /// Each class has its own rules (decision 0232), judged in `okx_class_facts`.
    fn row_answer(
        &self,
        symbol: &str,
        _key: &str,
        row: &Instrument,
        now_ms: i64,
        _words: &SymbolPropertiesWords,
    ) -> Result<SymbolPropertiesListAnswer, String> {
        okx_class_facts(symbol, row, now_ms)
    }
}

/// **Which okx rows are admitted, and as what** (decision 0232): the grid in BASE units and the margin mode and rate
/// a SPOT pair states (`Cash`, 1.0: fully funded, never liquidated, the shape Hyperliquid's spot pairs are admitted
/// in, decision 0218); a coin-margined contract's grid in CONTRACTS and its `Inverse` contract value (decision 0258's
/// okx amendment). A SWAP and a dated future must be linear and USDT-settled, or coin-margined ([`okx_inverse_facts`],
/// which the node admits only into an engine mounted on a contract of the same coin); a spot pair must be quoted in
/// USDT (the engine's ledger is USDT, and a pair quoted in another asset would be valued at par). Refused by name: an
/// option, a contract settled in USDC or USD, a future inside its last day ([`EXPIRY_GUARD_MS`]: the same day the
/// bridge stops taking an OPENING order).
fn okx_class_facts(
    symbol: &str,
    row: &Instrument,
    now_ms: i64,
) -> Result<SymbolPropertiesListAnswer, String> {
    let facts = |properties| SymbolPropertiesListAnswer {
        properties: Some(properties),
        ..SymbolPropertiesListAnswer::default()
    };
    let inverse = row.contract.margining() == Some(Margining::Inverse);
    match &row.contract {
        Contract::Perp(_) if inverse => okx_inverse_facts(symbol, row),
        Contract::Perp(_) => {
            okx_linear(symbol, row)?;
            Ok(facts(okx_in_base(symbol, row)?))
        }
        Contract::Future(spec) => {
            if !inverse {
                okx_linear(symbol, row)?;
            }
            let left = spec.expiry_ms - now_ms;
            if left < EXPIRY_GUARD_MS {
                return Err(format!(
                    "okx: {symbol} is delivered {} (08:00 UTC of the date in its name); the venue settles a \
                     position at the delivery and the engine books no settlement, so a future is admitted \
                     only until its last day. Trade the next expiry",
                    if left <= 0 {
                        "already".to_string()
                    } else {
                        format!("in {} h", left / 3_600_000)
                    }
                ));
            }
            if inverse {
                return okx_inverse_facts(symbol, row);
            }
            Ok(facts(okx_in_base(symbol, row)?))
        }
        Contract::CryptoSpot => {
            if !row.quote.eq_ignore_ascii_case("USDT") {
                return Err(format!(
                    "okx: {symbol} is quoted in {}, and the engine's ledger is USDT, so only a USDT-quoted \
                     pair is admitted (a pair quoted in another asset would be valued at par)",
                    row.quote
                ));
            }
            // a spot row counts size in the BASE coin already: nothing to scale. The venue states its
            // maximum market size in the QUOTE currency, which the catalog leaves absent.
            Ok(SymbolPropertiesListAnswer {
                properties: Some(row.properties),
                margin_mode: Some(vike_model::MarginMode::Cash),
                margin_rate: Some(1.0),
                ..SymbolPropertiesListAnswer::default()
            })
        }
        Contract::Option(_) => Err(format!(
            "okx: {symbol} is an option, which the gatewayhub does not admit: its premium is quoted in the \
             underlying coin (tick 0.0001 BTC on the demo's BTC-USD family) and a contract is 0.01 of it, so \
             the engine's quantity times price would value it in the wrong money, and the engine has no \
             option payoff or short-option margin model. The venue also wants options trading activated by \
             hand on its app or web before it takes an order (code 51198)"
        )),
        _ => Err(format!(
            "okx: {symbol} is a {} instrument, which the gatewayhub does not trade (spot pairs, SWAPs and \
             dated futures only)",
            row.asset_class().sql_word()
        )),
    }
}

/// A contract that is neither linear nor coin-margined (a quanto: the catalog drops such a row today) is
/// refused by name; a coin-margined one is [`okx_inverse_facts`]'s.
fn okx_linear(symbol: &str, row: &Instrument) -> Result<(), String> {
    if row.contract.margining() == Some(Margining::Linear) {
        return Ok(());
    }
    Err(format!(
        "okx: {symbol} is {}, neither a linear nor a coin-margined perpetual or future, so its notional is \
         not valued",
        row.contract_type.as_deref().unwrap_or("a contract of no stated kind")
    ))
}

/// **A coin-margined okx SWAP or dated future** (decision 0258's okx amendment): one unit of quantity is ONE
/// CONTRACT, worth `ctVal x ctMult` US dollars (the catalog row's `contract_size`: 100 on `BTC-USD-SWAP`), and
/// the money is the settle coin. So the facts are the venue's grid IN CONTRACTS, as listed (nothing scaled:
/// the mounted path passes contracts through, `crate::execution::mount_money`), with `contract_size` 0 (the
/// unit rides the value, never a second multiplier) and the value `Inverse` of that worth. The node admits it
/// only into an engine of that kind and coin (`symbol_admission`'s `contract_kind_refusal` and
/// `settle_coin_refusal`), and the bridge holds the coin again. Refused: a contract counted in anything but
/// US dollars, or of no usable size.
fn okx_inverse_facts(symbol: &str, row: &Instrument) -> Result<SymbolPropertiesListAnswer, String> {
    let value_ccy = match &row.contract {
        Contract::Perp(spec) => spec.value_currency.as_deref(),
        Contract::Future(spec) => spec.value_currency.as_deref(),
        _ => None,
    };
    if !value_ccy.is_some_and(|currency| currency.eq_ignore_ascii_case("USD")) {
        return Err(format!(
            "okx: {symbol} is coin-margined and its contract is counted in {}, not in US dollars, so its \
             notional is not valued",
            value_ccy.unwrap_or("no stated currency")
        ));
    }
    let unit = row.properties.contract_size;
    if !unit.is_finite() || unit <= 0.0 {
        return Err(format!(
            "okx: instrument {symbol}: the venue's instrument list carries no contract size \
             (missing or zero)"
        ));
    }
    Ok(SymbolPropertiesListAnswer {
        properties: Some(SymbolProperties { contract_size: 0.0, ..row.properties }),
        contract_value: Some(ContractValue::inverse(unit)),
        ..SymbolPropertiesListAnswer::default()
    })
}

/// **An okx SWAP's or dated future's grid in BASE units**, from its catalog row. OKX counts size in CONTRACTS (`lotSz`,
/// `minSz` and `maxMktSz` are contract counts, `ctVal` base units per contract) and the catalog row
/// carries `ctVal x ctMult` as `contract_size`; the engine's orders and fills are in BASE, so the three
/// quantity fields scale by it and nothing else does (a tick is quote per base, `min_notional` is
/// quote). The result must equal `crate::execution::rest::properties_in_base` of the mounted path's exec row
/// (`symbol_properties_tests.rs`' `okx_facts_equal_the_mounted_paths_base_unit_grid` holds it), and its
/// `contract_size` is 0 (absent): the scaling is DONE, so setting it too would multiply a notional
/// twice.
///
/// Refused, in words: a contract with no usable size, a contract counted in something other than the
/// base asset (the conversion would be meaningless), and a swap settled in anything but USDT (first
/// version: how the engine's cash treats a second settle currency is not checked).
///
/// ⚠ The catalog row has the PRODUCT `ctVal x ctMult`, so a `ctMult` other than 1 cannot be seen here:
/// the adapter refuses such an instrument itself when it builds the client (`crates/bridges/okx/src/execution/mod.rs`'s
/// `foreign_rest`), so the order is rejected, never mis-sized.
fn okx_in_base(symbol: &str, row: &Instrument) -> Result<SymbolProperties, String> {
    let (settle, value_ccy) = match &row.contract {
        Contract::Perp(spec) => (spec.settle.as_str(), spec.value_currency.as_deref()),
        Contract::Future(spec) => (spec.settle.as_str(), spec.value_currency.as_deref()),
        _ => return Err(format!("okx: {symbol} is not a SWAP perpetual or a dated future")),
    };
    if !settle.eq_ignore_ascii_case("USDT") {
        return Err(format!(
            "okx: {symbol} settles in {settle}; the gatewayhub admits USDT-settled swaps and futures \
             only for now"
        ));
    }
    if let Some(currency) = value_ccy
        && !currency.eq_ignore_ascii_case(&row.base)
    {
        return Err(format!(
            "okx: {symbol}: a contract is counted in {currency}, not in the base asset {}",
            row.base
        ));
    }
    let ct = row.properties.contract_size;
    if !ct.is_finite() || ct <= 0.0 {
        return Err(format!(
            "okx: instrument {symbol}: the venue's instrument list carries no contract size \
             (missing or zero)"
        ));
    }
    let p = row.properties;
    Ok(SymbolProperties {
        step_size: p.step_size * ct,
        min_qty: p.min_qty * ct,
        max_qty: p.max_qty * ct,
        contract_size: 0.0,
        ..p
    })
}

#[cfg(all(test, feature = "execution"))]
#[path = "symbol_properties_tests.rs"]
mod symbol_properties_tests;
