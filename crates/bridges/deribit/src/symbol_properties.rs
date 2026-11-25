//! **What the gatewayhub asks of deribit to admit a symbol the engine was not started on** (decision 0173, decision
//! 0233): the venue's half of the list-answered symbol facts ([`SymbolPropertiesListSpecDeribit`]): an option read
//! one at a time with its case kept, and what a row of the TESTNET book is (the money model of a perpetual or dated
//! future, a USDC option margined at its premium; a coin-settled option, a spot pair and a quanto row refused by
//! name). The listing (`crate::execution::fetch_testnet_futures`, `fetch_testnet_instrument`, read by the gatewayhub's
//! composition root), its bounds, the grid checks every venue shares and the wire are the gatewayhub's
//! (`crates/vike-gatewayhub/src/trade/adapters/symbol_facts.rs`).

use vike_bridge_core::symbol_properties::{
    SymbolPropertiesError, SymbolPropertiesListAnswer, SymbolPropertiesListSpec,
    SymbolPropertiesWords,
};
use vike_model::{Contract, ContractValue, Instrument, Margining, NotionalKind};
use vike_venues::venue_delivery_guard::{DeliveryGuardVerdict, delivery_verdict};

use crate::refusals::DeribitPropertiesRefusal;

/// **deribit's symbol facts** (perpetuals, dated futures and USDC-settled options, rows from the TESTNET book the
/// adapter signs on, not the public catalog's mainnet): `Some(Inverse, 1.0)` for a coin-margined future and `None` for
/// a linear one, so the node refuses the model that is not the engine's, and the adapter again
/// (`docs/ops/deribit-any-symbol-plan.md`). A USDC option is read ONE at a time (`public/get_instrument`: the venue
/// lists thousands of them) and answers a margin rate of 1, because the adapter holds an added option long only and a
/// long's margin is its premium (decision 0233). A coin-settled option, a spot pair and a combo are refused by name.
#[derive(Debug, Clone, Copy, Default)]
pub struct SymbolPropertiesListSpecDeribit;

impl SymbolPropertiesListSpec for SymbolPropertiesListSpecDeribit {
    fn venue(&self) -> &'static str {
        "deribit"
    }

    fn covers(&self, _list_key: &str, _account_listed: bool) -> Option<&'static str> {
        Some("perpetuals, dated futures and USDC options of the testnet book")
    }

    /// An option arrives in the door's spelling: upper case but for a strike's decimal `d`.
    fn case_kept(&self, symbol: &str) -> bool {
        self.one_at_a_time(symbol)
    }

    /// The USER name's KEPT row's instrument name (decision 0290: `BTCUSD.P` is listed as `BTC-PERPETUAL`; an option
    /// is its own name); a name with no row keys as written, which no listing holds (the not-found refusal).
    fn key(&self, symbol: &str) -> String {
        crate::names::venue_name_of(symbol).unwrap_or_else(|_| symbol.to_string())
    }

    /// An OPTION (`BASE-DDMMMYY-STRIKE-C`, four parts; decision 0233), by `public/get_instrument`. The testnet lists
    /// about 4,600 of them in one multi-megabyte body; the perpetuals and dated futures stay one futures-kind list,
    /// kept for every ask.
    fn one_at_a_time(&self, key: &str) -> bool {
        key.split('-').count() == 4
    }

    /// A PERPETUAL, a DATED future or a USDC-settled option (an inverse future counts its amount in dollars, a
    /// linear one in the base coin; the answer says which), and `contract_size` is the LOT, never a multiplier
    /// (decision 0170). An option is priced `qty * premium` in USDC, margined at the premium (rate 1: the adapter
    /// holds an added option long only, decision 0233); a coin-settled option and a spot pair are refused. A dated
    /// future or option whose delivery has passed is refused by the delivery guard (decision 0233 as amended on
    /// 2026-11-22: `vike_venues::venue_delivery_guard::delivery_verdict`, judged as a reduce-only order, so one inside
    /// its last 24 h is admitted close-only and the send refuses its opening orders).
    fn row_answer(
        &self,
        symbol: &str,
        _key: &str,
        row: &Instrument,
        now_ms: i64,
        words: &SymbolPropertiesWords,
    ) -> Result<SymbolPropertiesListAnswer, SymbolPropertiesError> {
        let mut margin_rate = None;
        match &row.contract {
            Contract::Perp(_) | Contract::Future(_) => {}
            Contract::Option(spec) if spec.margining == Margining::Linear => {
                margin_rate = Some(1.0);
            }
            Contract::Option(spec) => {
                return Err(SymbolPropertiesError::Refused(
                    DeribitPropertiesRefusal::CoinSettledOption {
                        symbol: symbol.to_string(),
                        settle: spec.settle.to_ascii_uppercase(),
                    }
                    .carried(),
                ));
            }
            _ => {
                return Err(SymbolPropertiesError::Refused(
                    DeribitPropertiesRefusal::NotADerivative {
                        symbol: symbol.to_string(),
                        class: row.asset_class().sql_word().to_string(),
                    }
                    .carried(),
                ));
            }
        }
        if delivery_verdict("deribit", symbol, now_ms, true) == DeliveryGuardVerdict::Delivered {
            return Err(SymbolPropertiesError::Expired(
                DeribitPropertiesRefusal::Delivered {
                    symbol: symbol.to_string(),
                    expired: words.expired.to_string(),
                }
                .carried(),
            ));
        }
        let Some(value) = ContractValue::of(&row.contract, 1.0) else {
            return Err(SymbolPropertiesError::Refused(
                DeribitPropertiesRefusal::Quanto { symbol: symbol.to_string() }.carried(),
            ));
        };
        Ok(SymbolPropertiesListAnswer {
            contract_value: (value.kind != NotionalKind::Linear).then_some(value),
            margin_rate,
            ..SymbolPropertiesListAnswer::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORDS: SymbolPropertiesWords = SymbolPropertiesWords {
        not_answered: "not answered",
        no_instrument: " has no instrument ",
        expired: "the contract has expired",
    };

    /// **A dated future is admitted close-only through its last day and refused once delivered** (the delivery guard,
    /// judged as a reduce-only order): an hour before `BTC-3NOV26`'s 08:00 UTC delivery it is admitted, an instant
    /// after it is refused in the expired words.
    #[test]
    fn a_dated_future_is_admitted_until_its_delivery_and_refused_after_it() {
        crate::names::hold_deribit_kept_rows();
        let rows = vike_catalog::kept_list("deribit").expect("the list is held");
        let row = rows.iter().find(|r| r.raw_symbol == "BTC-3NOV26").expect("the captured future");
        let user =
            vike_catalog::kept_user_name_of_raw("deribit", "BTC-3NOV26").expect("a user name");
        let delivery = row.contract.expiry_ms().expect("a dated future");
        let spec = SymbolPropertiesListSpecDeribit;
        let before = delivery - vike_model::MS_PER_HOUR;
        assert!(spec.row_answer(&user, "BTC-3NOV26", row, before, &WORDS).is_ok());
        let why =
            spec.row_answer(&user, "BTC-3NOV26", row, delivery + 1, &WORDS).expect_err("delivered");
        let delivered = DeribitPropertiesRefusal::Delivered {
            symbol: user.clone(),
            expired: WORDS.expired.to_string(),
        };
        assert_eq!(why, SymbolPropertiesError::Expired(delivered.carried()));
    }
}
