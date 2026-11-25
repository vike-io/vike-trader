use super::*;
use vike_marketdata::OptionKind;

use crate::test_support::instruments::perp;

fn future(family: ContractFutureFamily) -> Contract {
    Contract::Future(ContractFuture {
        margining: Margining::Inverse,
        settle: "BTC".into(),
        value_currency: Some("USD".into()),
        expiry_ms: 1_793_000_000_000,
        tenor: Some(ContractFutureTenor::Month),
        delivery: ContractFutureDelivery::Cash,
        family,
    })
}

fn option() -> Contract {
    Contract::Option(ContractOption {
        right: OptionKind::Put,
        strike: 60_000.0,
        expiry_ms: 1_793_000_000_000,
        style: ContractOptionExerciseStyle::European,
        margining: Margining::Inverse,
        settle: "BTC".into(),
        value_currency: None,
        underlying: Some("BTC-30OCT26".into()),
    })
}

fn combo(settle: Option<&str>) -> Contract {
    Contract::Combo(ContractCombo {
        kind: ContractComboKind::FutureSpread,
        legs: vec![
            ContractComboLeg { symbol: "BTC-30OCT26".into(), ratio: 1 },
            ContractComboLeg { symbol: "BTC-27NOV26".into(), ratio: -1 },
        ],
        expiry_ms: Some(1_793_000_000_000),
        settle: settle.map(Into::into),
    })
}

fn cfd(expiry: ContractCfdExpiry) -> Contract {
    Contract::Cfd(ContractCfd {
        underlying_kind: ContractCfdUnderlying::Metal,
        underlying: None,
        product: ContractCfdProduct::SpreadBet,
        expiry,
        margin_rate: Some(0.05),
        swap_long: None,
        swap_short: Some(-1.25),
    })
}

fn prediction(neg_risk: Option<bool>) -> Contract {
    Contract::PredictionOutcome(ContractPredictionOutcome {
        condition_id: "0xabc".into(),
        outcome: "Yes".into(),
        token_id: "123".into(),
        resolution_ms: None,
        neg_risk,
    })
}

/// One value of EVERY variant, with the class it must project to. A new variant fails to compile
/// in `Contract::class` first, then is missing here — `every_class_is_reached` catches that.
fn every_variant() -> Vec<(Contract, AssetClass)> {
    vec![
        (Contract::Equity(ContractEquity::default()), AssetClass::Equity),
        (
            Contract::Etf(ContractEquity {
                exchange: Some("ARCA".into()),
                currency: Some("USD".into()),
            }),
            AssetClass::Etf,
        ),
        (Contract::Index, AssetClass::Index),
        (Contract::Bond, AssetClass::Bond),
        (Contract::Fund, AssetClass::Fund),
        (Contract::Warrant, AssetClass::Warrant),
        (Contract::Commodity, AssetClass::Commodity),
        (Contract::CryptoSpot, AssetClass::CryptoSpot),
        (
            Contract::Fx(ContractFx {
                pip_size: Some(0.0001),
                lot_size: Some(100_000.0),
                swap_long: None,
                swap_short: None,
            }),
            AssetClass::Fx,
        ),
        (cfd(ContractCfdExpiry::Rolling), AssetClass::Cfd),
        (perp(Margining::Linear, "USDT"), AssetClass::CryptoPerp),
        (future(ContractFutureFamily::Crypto), AssetClass::CryptoFuture),
        (future(ContractFutureFamily::Traditional), AssetClass::Future),
        (option(), AssetClass::Option),
        (combo(None), AssetClass::Combo),
        (prediction(Some(true)), AssetClass::PredictionMarket),
    ]
}

/// **A share and a CFD on a share are what a split moves; nothing else is** (decision 0289): the class, never the name.
#[test]
fn only_a_share_or_a_cfd_on_one_is_a_share() {
    for (contract, _) in every_variant() {
        let want = matches!(contract, Contract::Equity(_) | Contract::Etf(_));
        assert_eq!(contract.is_share(), want, "{contract:?}");
    }
    let Contract::Cfd(mut on_a_share) = cfd(ContractCfdExpiry::Rolling) else { unreachable!() };
    on_a_share.underlying_kind = ContractCfdUnderlying::Share;
    assert!(Contract::Cfd(on_a_share).is_share(), "a CFD on a share");
}

#[test]
fn class_projects_every_variant() {
    for (contract, class) in every_variant() {
        assert_eq!(contract.class(), class, "{contract:?}");
    }
}

/// Every [`AssetClass`] is the projection of SOME contract — the taxonomy holds no word a contract
/// cannot reach.
#[test]
fn every_class_is_reached() {
    let reached: Vec<AssetClass> = every_variant().iter().map(|(c, _)| c.class()).collect();
    for class in AssetClass::ALL {
        assert!(reached.contains(class), "no contract projects to {class:?}");
    }
}

#[test]
fn every_variant_round_trips_through_serde() {
    for (contract, _) in every_variant() {
        let json = serde_json::to_string(&contract).unwrap();
        let back: Contract = serde_json::from_str(&json).unwrap();
        assert_eq!(back, contract, "{json}");
    }
}

/// The tags are `snake_case`, unit variants a bare string — the spelling a cache file carries.
#[test]
fn serde_tags_are_snake_case() {
    assert_eq!(serde_json::to_string(&Contract::CryptoSpot).unwrap(), "\"crypto_spot\"");
    assert_eq!(serde_json::to_string(&Contract::Index).unwrap(), "\"index\"");
    let p = serde_json::to_value(perp(Margining::Inverse, "BTC")).unwrap();
    assert_eq!(p["perp"]["margining"], "inverse");
    let c = serde_json::to_value(cfd(ContractCfdExpiry::Date(5))).unwrap();
    assert_eq!(c["cfd"]["product"], "spread_bet");
    assert_eq!(c["cfd"]["expiry"]["date"], 5);
    assert_eq!(
        serde_json::to_value(prediction(None)).unwrap()["prediction_outcome"]["outcome"],
        "Yes"
    );
}

/// An absent optional field reads back as `None` — a payload that omits it is a payload that does
/// not know it.
#[test]
fn absent_optional_fields_deserialize_as_none() {
    let fx: Contract = serde_json::from_str(r#"{"fx":{}}"#).unwrap();
    assert_eq!(fx, Contract::Fx(ContractFx::default()));
    let eq: Contract = serde_json::from_str(r#"{"equity":{"exchange":"NYSE"}}"#).unwrap();
    assert_eq!(
        eq,
        Contract::Equity(ContractEquity { exchange: Some("NYSE".into()), currency: None })
    );
    let p: Contract =
        serde_json::from_str(r#"{"perp":{"margining":"linear","settle":"USDC"}}"#).unwrap();
    assert_eq!(p, perp(Margining::Linear, "USDC"));
}

/// Both halves of every optional field survive: `Some` and `None` alike.
#[test]
fn some_and_none_both_round_trip() {
    let full = Contract::Perp(ContractPerp {
        margining: Margining::Quanto,
        settle: "BTC".into(),
        value_currency: Some("USD".into()),
        funding_interval_ms: Some(28_800_000),
        settling_ms: Some(1_800_000_000_000),
    });
    for c in [full, perp(Margining::Quanto, "BTC")] {
        let back: Contract = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(back, c);
    }
    for e in [
        ContractCfdExpiry::Rolling,
        ContractCfdExpiry::Dfb,
        ContractCfdExpiry::Date(1),
        ContractCfdExpiry::Unknown,
    ] {
        let c = cfd(e);
        let back: Contract = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(back, c);
    }
}

/// ⚠ `neg_risk: None` is UNKNOWN and must never collapse into `Some(false)` on a round trip.
#[test]
fn neg_risk_unknown_is_not_false() {
    let unknown = prediction(None);
    let plain = prediction(Some(false));
    assert_ne!(unknown, plain);
    for c in [unknown, plain, prediction(Some(true))] {
        let back: Contract = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(back, c);
    }
    let absent: Contract = serde_json::from_str(
        r#"{"prediction_outcome":{"condition_id":"0xabc","outcome":"Yes","token_id":"123"}}"#,
    )
    .unwrap();
    assert_eq!(absent, prediction(None), "an absent key is unknown, not false");
}

#[test]
fn settle_asset_per_variant() {
    for (contract, _) in every_variant() {
        let want = match &contract {
            Contract::Perp(_) => Some("USDT"),
            Contract::Future(_) | Contract::Option(_) => Some("BTC"),
            _ => None,
        };
        assert_eq!(contract.settle_asset(), want, "{contract:?}");
    }
    assert_eq!(combo(Some("USDC")).settle_asset(), Some("USDC"));
    assert_eq!(combo(None).settle_asset(), None);
}

#[test]
fn margining_and_expiry_helpers() {
    assert_eq!(perp(Margining::Linear, "USDT").margining(), Some(Margining::Linear));
    assert_eq!(option().margining(), Some(Margining::Inverse));
    assert_eq!(Contract::CryptoSpot.margining(), None);

    assert_eq!(perp(Margining::Linear, "USDT").expiry_ms(), None, "a perp has no expiry");
    assert_eq!(future(ContractFutureFamily::Crypto).expiry_ms(), Some(1_793_000_000_000));
    assert_eq!(option().expiry_ms(), Some(1_793_000_000_000));
    assert_eq!(combo(None).expiry_ms(), Some(1_793_000_000_000));
    assert_eq!(cfd(ContractCfdExpiry::Date(7)).expiry_ms(), Some(7));
    assert_eq!(cfd(ContractCfdExpiry::Rolling).expiry_ms(), None);
    assert_eq!(prediction(None).expiry_ms(), None);
}

/// Inverse is a perpetual's or a dated future's `Inverse` margining and nothing else: a coin option,
/// a combo and every cash class are not, so they keep no coin books. The coin is the settle asset in
/// the engine's spelling, and an inverse contract naming no coin has none.
#[test]
fn only_an_inverse_perp_or_future_keeps_coin_books() {
    let inverse_perp = perp(Margining::Inverse, " btc ");
    assert!(inverse_perp.is_inverse());
    assert_eq!(inverse_perp.settle_coin_if_inverse().as_deref(), Some("BTC"));
    let inverse_future = future(ContractFutureFamily::Crypto);
    assert!(inverse_future.is_inverse());
    assert_eq!(inverse_future.settle_coin_if_inverse().as_deref(), Some("BTC"));
    assert_eq!(perp(Margining::Inverse, "  ").settle_coin_if_inverse(), None);
    for (contract, _) in every_variant() {
        let inverse = matches!(contract, Contract::Future(_));
        assert_eq!(contract.is_inverse(), inverse, "{contract:?}");
        assert_eq!(contract.settle_coin_if_inverse().is_some(), inverse, "{contract:?}");
    }
    for not_inverse in [
        perp(Margining::Linear, "USDT"),
        perp(Margining::Quanto, "BTC"),
        option(),
        combo(Some("BTC")),
    ] {
        assert!(!not_inverse.is_inverse(), "{not_inverse:?}");
        assert_eq!(not_inverse.settle_coin_if_inverse(), None, "{not_inverse:?}");
    }
}
