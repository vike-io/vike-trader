//! **What the gatewayhub asks of okx to admit a symbol the engine was not started on** (decision 0173, decision 0232,
//! decision 0258's okx amendment): the venue's half of the list-answered symbol facts ([`SymbolPropertiesListSpecOkx`]):
//! which network's list answers (demo and mainnet share one host; the demo is a header, so a list of its own), which
//! asks are point reads, and what each class of row is (`okx_class_facts`: the grid in BASE units for a linear
//! USDT contract, `Cash` for a USDT spot pair, `Inverse` on the CONTRACTS grid for a coin-margined one, a future
//! refused once delivered, `vike_venues::venue_delivery_guard`'s `delivery_verdict`). The listing, its bounds, the grid checks
//! every venue shares and the wire are the gatewayhub's (`crates/vike-gatewayhub/src/trade/adapters/symbol_facts.rs`).
//! The base-unit grid is the mounted path's own conversion, `vike_catalog::okx_grid_in_base`, held equal to it by
//! `symbol_properties_tests.rs`.

use vike_bridge_core::symbol_properties::{
    SymbolPropertiesError, SymbolPropertiesListAnswer, SymbolPropertiesListSpec,
    SymbolPropertiesWords,
};
use vike_model::{Contract, ContractValue, Instrument, Margining, SymbolProperties};

use vike_venues::venue_delivery_guard::{DeliveryGuardVerdict, delivery_verdict};

use crate::instrument::InstrumentType;
use crate::refusals::OkxPropertiesRefusal;

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
        if network == vike_model::VenueNetwork::Testnet.word() { OKX_DEMO_LIST } else { "okx" }
    }

    fn covers(&self, list_key: &str, _account_listed: bool) -> Option<&'static str> {
        Some(if list_key == OKX_DEMO_LIST {
            "USDT-quoted spot pairs, linear USDT-settled SWAP perpetuals and dated futures of OKX's \
             simulated-trading network, the network this account trades on; that list is not mainnet's"
        } else {
            "USDT-quoted spot pairs, linear USDT-settled SWAP perpetuals and dated futures"
        })
    }

    /// The USER name's KEPT row's instId (decision 0290: `BTCUSDT.P` is listed as `BTC-USDT-SWAP`); a name with no row
    /// keys as written, which no listing holds (the not-found refusal).
    fn key(&self, symbol: &str) -> String {
        crate::instrument::inst_id_of(symbol).unwrap_or_else(|_| symbol.to_string())
    }

    /// A SPOT pair and a dated future (decision 0232): one point read each (`?instType=&instId=`); the SWAP list
    /// stays one bulk read, kept per network and shared by every swap ask.
    fn one_at_a_time(&self, key: &str) -> bool {
        InstrumentType::of_inst_id(key) != Some(InstrumentType::Swap)
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
    ) -> Result<SymbolPropertiesListAnswer, SymbolPropertiesError> {
        okx_class_facts(symbol, row, now_ms)
            .map_err(|refusal| SymbolPropertiesError::Refused(refusal.carried()))
    }
}

/// **Which okx rows are admitted, and as what** (decision 0232): the grid in BASE units and the margin mode and rate
/// a SPOT pair states (`Cash`, 1.0: fully funded, never liquidated, the shape Hyperliquid's spot pairs are admitted
/// in, decision 0218); a coin-margined contract's grid in CONTRACTS and its `Inverse` contract value (decision 0258's
/// okx amendment). A SWAP and a dated future must be linear and USDT-settled, or coin-margined ([`okx_inverse_facts`],
/// which the node admits only into an engine mounted on a contract of the same coin); a spot pair must be quoted in
/// USDT (the engine's ledger is USDT, and a pair quoted in another asset would be valued at par). Refused by name: an
/// option, a contract settled in USDC or USD, a future already delivered. Inside its last day a future is admitted
/// close-only (`vike_venues::venue_delivery_guard`'s `delivery_verdict`, okx's rule for every venue): the bridge
/// refuses each OPENING order at send.
fn okx_class_facts(
    symbol: &str,
    row: &Instrument,
    now_ms: i64,
) -> Result<SymbolPropertiesListAnswer, OkxPropertiesRefusal> {
    let symbol_owned = || symbol.to_string();
    let facts = |properties| SymbolPropertiesListAnswer {
        properties: Some(properties),
        ..SymbolPropertiesListAnswer::default()
    };
    let inverse = row.contract.is_inverse();
    match &row.contract {
        Contract::Perp(_) if inverse => okx_inverse_facts(symbol, row),
        Contract::Perp(_) => {
            okx_linear(symbol, row)?;
            Ok(facts(okx_in_base(symbol, row)?))
        }
        Contract::Future(_) => {
            if !inverse {
                okx_linear(symbol, row)?;
            }
            // Its last day is close-only: each opening order is refused at send (`crate::instrument::expiry_refusal`).
            if delivery_verdict("okx", symbol, now_ms, true) == DeliveryGuardVerdict::Delivered {
                return Err(OkxPropertiesRefusal::FutureDelivered { symbol: symbol_owned() });
            }
            if inverse {
                return okx_inverse_facts(symbol, row);
            }
            Ok(facts(okx_in_base(symbol, row)?))
        }
        Contract::CryptoSpot => {
            if !row.quote.eq_ignore_ascii_case("USDT") {
                return Err(OkxPropertiesRefusal::SpotNotUsdtQuoted {
                    symbol: symbol_owned(),
                    quote: row.quote.clone(),
                });
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
        Contract::Option(_) => {
            Err(OkxPropertiesRefusal::OptionNotAdmitted { symbol: symbol_owned() })
        }
        _ => Err(OkxPropertiesRefusal::ClassNotTraded {
            symbol: symbol_owned(),
            class: row.asset_class().sql_word(),
        }),
    }
}

/// A contract that is neither linear nor coin-margined (a quanto: the catalog drops such a row today) is
/// refused by name; a coin-margined one is [`okx_inverse_facts`]'s.
fn okx_linear(symbol: &str, row: &Instrument) -> Result<(), OkxPropertiesRefusal> {
    if row.contract.margining() == Some(Margining::Linear) {
        return Ok(());
    }
    Err(OkxPropertiesRefusal::NotLinear {
        symbol: symbol.to_string(),
        kind: row.contract_type.as_deref().unwrap_or("a contract of no stated kind").to_string(),
    })
}

/// **A coin-margined okx SWAP or dated future** (decision 0258's okx amendment): one unit of quantity is ONE
/// CONTRACT, worth `ctVal x ctMult` US dollars (the catalog row's `contract_size`: 100 on `BTC-USD-SWAP`), and
/// the money is the settle coin. So the facts are the venue's grid IN CONTRACTS, as listed (nothing scaled:
/// the mounted path passes contracts through, `crate::execution::mount_money`), with `contract_size` 0 (the
/// unit rides the value, never a second multiplier) and the value `Inverse` of that worth. The node admits it
/// only into an engine of that kind and coin (`symbol_admission`'s `contract_kind_refusal` and
/// `settle_coin_refusal`), and the bridge holds the coin again. Refused: a contract counted in anything but
/// US dollars, or of no usable size.
fn okx_inverse_facts(
    symbol: &str,
    row: &Instrument,
) -> Result<SymbolPropertiesListAnswer, OkxPropertiesRefusal> {
    let value_ccy = match &row.contract {
        Contract::Perp(spec) => spec.value_currency.as_deref(),
        Contract::Future(spec) => spec.value_currency.as_deref(),
        _ => None,
    };
    if !value_ccy.is_some_and(|currency| currency.eq_ignore_ascii_case("USD")) {
        return Err(OkxPropertiesRefusal::InverseNotUsd {
            symbol: symbol.to_string(),
            currency: value_ccy.unwrap_or("no stated currency").to_string(),
        });
    }
    let unit = row.properties.contract_size;
    if !unit.is_finite() || unit <= 0.0 {
        return Err(OkxPropertiesRefusal::NoContractSize { symbol: symbol.to_string() });
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
/// quote), by the one conversion the mounted path's exec row is scaled by (`vike_catalog::okx_grid_in_base`). The
/// result must equal that of the exec row
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
fn okx_in_base(symbol: &str, row: &Instrument) -> Result<SymbolProperties, OkxPropertiesRefusal> {
    let symbol_owned = || symbol.to_string();
    let (settle, value_ccy) = match &row.contract {
        Contract::Perp(spec) => (spec.settle.as_str(), spec.value_currency.as_deref()),
        Contract::Future(spec) => (spec.settle.as_str(), spec.value_currency.as_deref()),
        _ => return Err(OkxPropertiesRefusal::NotSwapOrFuture { symbol: symbol_owned() }),
    };
    if !settle.eq_ignore_ascii_case("USDT") {
        return Err(OkxPropertiesRefusal::SettleNotUsdt {
            symbol: symbol_owned(),
            settle: settle.to_string(),
        });
    }
    if let Some(currency) = value_ccy
        && !currency.eq_ignore_ascii_case(&row.base)
    {
        return Err(OkxPropertiesRefusal::NotCountedInBase {
            symbol: symbol_owned(),
            currency: currency.to_string(),
            base: row.base.clone(),
        });
    }
    let ct = row.properties.contract_size;
    if !ct.is_finite() || ct <= 0.0 {
        return Err(OkxPropertiesRefusal::NoContractSize { symbol: symbol_owned() });
    }
    Ok(vike_catalog::okx_grid_in_base(&row.properties, ct))
}

#[cfg(all(test, feature = "execution"))]
#[path = "symbol_properties_tests.rs"]
mod symbol_properties_tests;
