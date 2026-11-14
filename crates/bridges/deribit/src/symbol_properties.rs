//! **What the gatewayhub asks of deribit to admit a symbol the engine was not started on** (decision 0173, decision
//! 0233): the venue's half of the list-answered symbol facts ([`SymbolPropertiesListSpecDeribit`]): an option read
//! one at a time with its case kept, and what a row of the TESTNET book is (the money model of a perpetual or dated
//! future, a USDC option margined at its premium; a coin-settled option, a spot pair and a quanto row refused by
//! name). The listing (`crate::execution::fetch_testnet_futures`, `fetch_testnet_instrument`, read by the gatewayhub's
//! composition root), its bounds, the grid checks every venue shares and the wire are the gatewayhub's
//! (`crates/vike-gatewayhub/src/trade/adapters/symbol_facts.rs`).

use vike_bridge_core::symbol_properties::{
    SymbolPropertiesListAnswer, SymbolPropertiesListSpec, SymbolPropertiesWords,
};
use vike_model::{Contract, ContractValue, Instrument, Margining, NotionalKind};

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

    /// An OPTION (`BASE-DDMMMYY-STRIKE-C`, four parts; decision 0233), by `public/get_instrument`. The testnet lists
    /// about 4,600 of them in one multi-megabyte body; the perpetuals and dated futures stay one futures-kind list,
    /// kept for every ask.
    fn one_at_a_time(&self, key: &str) -> bool {
        key.split('-').count() == 4
    }

    /// A PERPETUAL, a DATED future or a USDC-settled option (an inverse future counts its amount in dollars, a
    /// linear one in the base coin; the answer says which), and `contract_size` is the LOT, never a multiplier
    /// (decision 0170). An option is priced `qty * premium` in USDC, margined at the premium (rate 1: the adapter
    /// holds an added option long only, decision 0233); a coin-settled option and a spot pair are refused.
    fn row_answer(
        &self,
        symbol: &str,
        _key: &str,
        row: &Instrument,
        _now_ms: i64,
        _words: &SymbolPropertiesWords,
    ) -> Result<SymbolPropertiesListAnswer, String> {
        let mut margin_rate = None;
        match &row.contract {
            Contract::Perp(_) | Contract::Future(_) => {}
            Contract::Option(spec) if spec.margining == Margining::Linear => {
                margin_rate = Some(1.0);
            }
            Contract::Option(spec) => {
                return Err(format!(
                    "deribit: {symbol} is a coin-settled option: its premium is quoted in {}, so its \
                     notional and PnL are coin amounts that an account's one-currency ledger would \
                     book as dollars. Trade the USDC-settled options of the same underlying",
                    spec.settle.to_ascii_uppercase()
                ));
            }
            _ => {
                return Err(format!(
                    "deribit: {symbol} is {}, not a perpetual, a dated future or a USDC option; a spot \
                     pair has no position at the venue (a holding is only a currency balance), so the \
                     gatewayhub does not admit one",
                    row.asset_class().sql_word()
                ));
            }
        }
        let Some(value) = ContractValue::of(&row.contract, 1.0) else {
            return Err(format!(
                "deribit: {symbol} is a quanto contract, whose notional is not valued"
            ));
        };
        Ok(SymbolPropertiesListAnswer {
            contract_value: (value.kind != NotionalKind::Linear).then_some(value),
            margin_rate,
            ..SymbolPropertiesListAnswer::default()
        })
    }
}
