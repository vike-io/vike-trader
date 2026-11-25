use super::*;

use crate::instrument::contract::Margining;
use crate::test_support::instruments::perp;

fn model(kind: NotionalKind, settle: &str) -> MoneyModel {
    MoneyModel { kind, settle: settle.into() }
}

/// The model is the contract's kind and its settle coin in the engine's spelling; a quanto, a cash
/// class and a contract naming no coin have none.
#[test]
fn a_contract_states_its_money_model() {
    assert_eq!(
        MoneyModel::of_contract(&perp(Margining::Inverse, " btc ")),
        Some(model(NotionalKind::Inverse, "BTC"))
    );
    assert_eq!(
        MoneyModel::of_contract(&perp(Margining::Linear, "USDT")),
        Some(model(NotionalKind::Linear, "USDT"))
    );
    for none in [perp(Margining::Quanto, "BTC"), perp(Margining::Linear, " "), Contract::CryptoSpot]
    {
        assert_eq!(MoneyModel::of_contract(&none), None, "{none:?}");
    }
}

/// The same kind and coin is admitted (any case); the other kind is refused as a KIND even when the
/// coin agrees, and before the coin is looked at; the same kind in another coin is a SETTLE COIN
/// refusal, linear included (the caller judges which refusals it holds). Red on a mutant that swaps
/// the two checks or compares the coin case-sensitively.
#[test]
fn an_engine_admits_only_its_own_kind_and_coin() {
    use NotionalKind::{Inverse, Linear};
    assert_eq!(model(Inverse, "BTC").admits(&model(Inverse, "btc")), Ok(()));
    assert_eq!(model(Linear, "USDC").admits(&model(Linear, "USDC")), Ok(()));
    assert_eq!(
        model(Inverse, "BTC").admits(&model(Inverse, "ETH")),
        Err(MoneyModelRefusal::SettleCoin)
    );
    assert_eq!(
        model(Linear, "USDC").admits(&model(Linear, "USDT")),
        Err(MoneyModelRefusal::SettleCoin)
    );
    assert_eq!(model(Inverse, "BTC").admits(&model(Linear, "BTC")), Err(MoneyModelRefusal::Kind));
    assert_eq!(model(Linear, "USDT").admits(&model(Inverse, "ETH")), Err(MoneyModelRefusal::Kind));
}
