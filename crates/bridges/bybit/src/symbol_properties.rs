//! **What the gatewayhub asks of bybit to admit a symbol the engine was not started on** (decision 0173, decision
//! 0231): the venue's half of the list-answered symbol facts ([`SymbolPropertiesListSpecBybit`]): how an engine
//! spelling names a book and its catalog row, every symbol read one at a time, and what a row is (`bybit_answer`: a
//! linear or coin-margined perpetual or dated future, a spot pair, nothing else). The listing, its bounds, the grid
//! checks every venue shares and the wire are the gatewayhub's (`crates/vike-gatewayhub/src/trade/adapters/symbol_facts.rs`).

use vike_bridge_core::symbol_properties::{
    SymbolPropertiesListAnswer, SymbolPropertiesListSpec, SymbolPropertiesWords,
};
use vike_model::{Contract, ContractValue, Instrument, NotionalKind};

/// **How long before its delivery a bybit dated future is no longer admitted** (decision 0231): the venue closes
/// every position of a dated contract at its delivery time (the `deliveryTime` of its row, 08:00 UTC of the expiry
/// day) and this adapter does not fold that settlement (an `execType` other than `Trade` is dropped by
/// `crates/bridges/bybit/src/execution/event_mapper.rs`'s `map_execution`), so a position opened close to it would reach the
/// delivery as a position the engine still holds. A day keeps every first order a full session clear of it.
pub const BYBIT_DELIVERY_GUARD_MS: i64 = 24 * 3_600_000;

/// **bybit's symbol facts** (decision 0231): every symbol read one at a time (`instruments-info?symbol=`). The engine
/// symbol names the book: a spot pair is `PAIR.S` (its catalog row is the bare pair, asked of the lister under the
/// engine's name so the pair and the perpetual of one name keep two slots), every other name a derivative: a
/// perpetual's catalog row is `<SYMBOL>.P`, a dated contract's its own name (`ETHUSDT-27NOV26`), whichever the venue
/// lists under it (the two never share a name). What the ROW is decides the answer (`bybit_answer`).
#[derive(Debug, Clone, Copy, Default)]
pub struct SymbolPropertiesListSpecBybit;

impl SymbolPropertiesListSpec for SymbolPropertiesListSpecBybit {
    fn venue(&self) -> &'static str {
        "bybit"
    }

    fn covers(&self, _list_key: &str, _account_listed: bool) -> Option<&'static str> {
        Some("linear perpetuals and dated linear futures")
    }

    fn key(&self, symbol: &str) -> String {
        match crate::instrument::class::split_spot(symbol) {
            (pair, true) => pair.to_string(),
            (_, false) => format!("{}.P", symbol.strip_suffix(".P").unwrap_or(symbol)),
        }
    }

    fn one_at_a_time(&self, _key: &str) -> bool {
        true
    }

    /// The spot pair under the engine's name (`PAIR.S`), a derivative under its bare venue symbol.
    fn wanted(&self, symbol: &str, key: &str) -> String {
        if crate::instrument::class::split_spot(symbol).1 {
            symbol
        } else {
            key.strip_suffix(".P").unwrap_or(key)
        }
        .to_string()
    }

    /// A dated contract is keyed by its own name, not `<SYMBOL>.P`.
    fn row_fallback<'k>(&self, key: &'k str) -> Option<&'k str> {
        key.strip_suffix(".P")
    }

    fn row_answer(
        &self,
        symbol: &str,
        _key: &str,
        row: &Instrument,
        now_ms: i64,
        _words: &SymbolPropertiesWords,
    ) -> Result<SymbolPropertiesListAnswer, String> {
        bybit_answer(row, symbol, crate::instrument::class::split_spot(symbol).1, now_ms)
    }
}

/// **What a bybit catalog row is to the engine that is admitted it** (decision 0231), or the refusal in words:
///
/// * a linear PERPETUAL: the facts as before;
/// * a coin-margined PERPETUAL: the facts and the contract value `Inverse` (one unit of quantity is one USD of
///   notional, `crates/vike-model/src/money/contract_value.rs`'s `ContractValue`), which the engine books per
///   symbol (its fee in the settle coin enters at the fill price, its PnL in the quote);
/// * a DATED future, linear or coin-margined, the same two ways, refused within [`BYBIT_DELIVERY_GUARD_MS`] of its
///   delivery;
/// * a SPOT pair (spelled `PAIR.S`), `Cash` at a margin rate of 1: fully funded, never liquidated, as a Hyperliquid
///   spot pair is (decision 0218). Its grid is the spot row's;
/// * anything else (an option, a row the venue added) is refused by name.
///
/// The spelling and the row must agree: the spot spelling asks the spot list, so a derivative row under it (and a
/// spot row under a bare name) is the lister's fault and is refused, never admitted as the other book. The answer is
/// whole (the shared `.P` linear rule does not apply: a coin-margined row is answered, not refused); the grid checks
/// follow. `now_ms` is a parameter so a test drives the clock.
fn bybit_answer(
    row: &Instrument,
    symbol: &str,
    spot_spelling: bool,
    now_ms: i64,
) -> Result<SymbolPropertiesListAnswer, String> {
    let word = row.contract_type.as_deref().unwrap_or("a contract");
    let (contract_value, margin_mode, margin_rate) = match (&row.contract, spot_spelling) {
        (Contract::CryptoSpot, true) => (None, Some(vike_model::MarginMode::Cash), Some(1.0)),
        (Contract::Perp(_) | Contract::Future(_), false) => {
            if let Contract::Future(spec) = &row.contract {
                let left = spec.expiry_ms - now_ms;
                if left < BYBIT_DELIVERY_GUARD_MS {
                    return Err(format!(
                        "bybit: {symbol} delivers in {} h (at {} ms): the venue closes every position at \
                         delivery and this adapter does not fold that settlement, so no order opens on a \
                         contract within {} h of it; trade the next one",
                        left.max(0) / 3_600_000,
                        spec.expiry_ms,
                        BYBIT_DELIVERY_GUARD_MS / 3_600_000
                    ));
                }
            }
            let Some(value) = ContractValue::of(&row.contract, 1.0) else {
                return Err(format!(
                    "bybit: {symbol} is a quanto contract, whose notional is not valued"
                ));
            };
            ((value.kind != NotionalKind::Linear).then_some(value), None, None)
        }
        _ => {
            return Err(format!(
                "bybit: {symbol} is {word}, which this gatewayhub does not trade (it trades linear and \
                 coin-margined perpetuals, dated futures of either, and spot pairs; an option needs a payoff and \
                 short-margin model the engine does not have)"
            ));
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
