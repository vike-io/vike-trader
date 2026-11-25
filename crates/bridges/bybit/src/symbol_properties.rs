//! **What the gatewayhub asks of bybit to admit a symbol the engine was not started on** (decision 0173, decision
//! 0231): the venue's half of the list-answered symbol facts ([`SymbolPropertiesListSpecBybit`]): how an engine
//! spelling names a book and its catalog row, every symbol read one at a time, and what a row is (`bybit_answer`: a
//! linear or coin-margined perpetual or dated future, a spot pair, nothing else). The listing, its bounds, the grid
//! checks every venue shares and the wire are the gatewayhub's (`crates/vike-gatewayhub/src/trade/adapters/symbol_facts.rs`).

use vike_bridge_core::symbol_properties::{
    SymbolPropertiesError, SymbolPropertiesListAnswer, SymbolPropertiesListSpec,
    SymbolPropertiesWords,
};
use vike_model::{Contract, ContractValue, Instrument, NotionalKind};
use vike_venues::venue_delivery_guard::{DeliveryGuardVerdict, delivery_guard_for};

use crate::refusals::BybitSymbolRefusal;

/// **bybit's symbol facts** (decisions 0231 and 0290): every symbol read one at a time (`instruments-info?symbol=`).
/// The engine's symbol is a USER name, and the catalog row it names is its KEPT row's key: the pair `BTCUSDT` is the
/// spot row `BTCUSDT`, the perpetual `BTCUSDT.P` the row `BTCUSDT.P`, `BTCUSDC.P` the row `BTCPERP.P`, a dated
/// contract `ETHUSDT_261106` the row `ETHUSDT-06NOV26`. The lister is asked for the user name and reads its book and
/// venue name off the same kept row (the gatewayhub's `with_symbol_lister`), so a name with no kept row is refused
/// there in the translator's words. What the ROW is decides the answer (`bybit_answer`).
#[derive(Debug, Clone, Copy, Default)]
pub struct SymbolPropertiesListSpecBybit;

impl SymbolPropertiesListSpec for SymbolPropertiesListSpecBybit {
    fn venue(&self) -> &'static str {
        "bybit"
    }

    fn covers(&self, _list_key: &str, _account_listed: bool) -> Option<&'static str> {
        Some("linear perpetuals and dated linear futures")
    }

    /// The catalog's key of the row: the user name's kept row's own (`BTCUSDT.P`, `BTCPERP.P`, `ETHUSDT-06NOV26`, the
    /// pair `BTCUSDT`), the key the lister's rows are parsed under (`crate::catalog`). With no kept row the name
    /// itself, which keys a row only where the catalog spells the instrument so (a perpetual's `X.P`, a pair), and is
    /// otherwise the venue's list's "no instrument" refusal.
    fn key(&self, symbol: &str) -> String {
        let rows = vike_catalog::kept_list("bybit");
        let rows = rows.as_deref().map_or(&[][..], Vec::as_slice);
        vike_catalog::kept_row_of_user_name(rows, "bybit", symbol)
            .map_or_else(|_| symbol.to_string(), |row| row.raw_symbol.clone())
    }

    fn one_at_a_time(&self, _key: &str) -> bool {
        true
    }

    /// The USER name: the lister reads its book and venue name off the same kept row.
    fn wanted(&self, symbol: &str, _key: &str) -> String {
        symbol.to_string()
    }

    fn row_answer(
        &self,
        symbol: &str,
        _key: &str,
        row: &Instrument,
        now_ms: i64,
        _words: &SymbolPropertiesWords,
    ) -> Result<SymbolPropertiesListAnswer, SymbolPropertiesError> {
        bybit_answer(row, symbol, now_ms)
            .map_err(|refusal| SymbolPropertiesError::Refused(refusal.carried()))
    }
}

/// **What a bybit catalog row is to the engine that is admitted it** (decision 0231), or the refusal in words:
///
/// * a linear PERPETUAL: the facts as before;
/// * a coin-margined PERPETUAL: the facts and the contract value `Inverse` (one unit of quantity is one USD of
///   notional, `crates/vike-model/src/money/contract_value.rs`'s `ContractValue`), which the engine books per
///   symbol (its fee in the settle coin enters at the fill price, its PnL in the quote);
/// * a DATED future, linear or coin-margined, the same two ways, refused once delivered: the venue closes every
///   position of a dated contract at its delivery and this adapter does not fold that settlement (an `execType` other
///   than `Trade` is dropped by `crates/bridges/bybit/src/execution/event_mapper.rs`'s `map_execution`). Inside its
///   last day (`vike_venues::venue_delivery_guard`'s window, at the delivery day its user name states) it is admitted
///   close-only: each opening order is refused at send (`crate::execution::rest`'s `VenueRestBybit`);
/// * a SPOT pair (the user name `BTCUSDT`), `Cash` at a margin rate of 1: fully funded, never liquidated, as a
///   Hyperliquid spot pair is (decision 0218). Its grid is the spot row's;
/// * anything else (an option, a row the venue added) is refused by name.
///
/// The ROW says which: a pair and a derivative are two rows under two keys (`BTCUSDT`, `BTCUSDT.P`), so the row a
/// user name keys is the instrument it names. The answer is whole (the shared `.P` linear rule does not apply: a
/// coin-margined row is answered, not refused); the grid checks follow. `now_ms` is a parameter so a test drives the
/// clock.
fn bybit_answer(
    row: &Instrument,
    symbol: &str,
    now_ms: i64,
) -> Result<SymbolPropertiesListAnswer, BybitSymbolRefusal> {
    let word = row.contract_type.as_deref().unwrap_or("a contract");
    let (contract_value, margin_mode, margin_rate) = match &row.contract {
        Contract::CryptoSpot => (None, Some(vike_model::MarginMode::Cash), Some(1.0)),
        Contract::Perp(_) | Contract::Future(_) => {
            let guard = delivery_guard_for("bybit");
            if let Some(expiry) = guard.expiry_of(symbol)
                && guard.verdict(expiry, now_ms, true) == DeliveryGuardVerdict::Delivered
            {
                return Err(BybitSymbolRefusal::Delivered {
                    symbol: symbol.to_string(),
                    expiry,
                    window_hours: guard.window_ms.unwrap_or_default() / vike_model::MS_PER_HOUR,
                });
            }
            let Some(value) = ContractValue::of(&row.contract, 1.0) else {
                return Err(BybitSymbolRefusal::Quanto { symbol: symbol.to_string() });
            };
            ((value.kind != NotionalKind::Linear).then_some(value), None, None)
        }
        _ => {
            return Err(BybitSymbolRefusal::NotTraded {
                symbol: symbol.to_string(),
                word: word.to_string(),
            });
        }
    };
    Ok(SymbolPropertiesListAnswer {
        properties: None,
        contract_value,
        margin_mode,
        margin_rate,
        whole: true,
    })
}
