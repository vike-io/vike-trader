//! The two order vocabularies: every id is unique, `order.`-dotted and read back exactly; the gate's
//! words are the bytes journals and results already hold; the HALT file's literal is the reason the
//! halt has always written.

use super::{OrderDenial, OrderDenialId, OrderRefusal, OrderRefusalId};

#[test]
fn every_order_id_is_unique_dotted_and_reads_back() {
    let ids: Vec<&str> =
        OrderDenialId::WORDS.iter().chain(OrderRefusalId::WORDS.iter()).copied().collect();
    let mut seen = std::collections::HashSet::new();
    for id in &ids {
        assert!(id.starts_with("order."), "{id}");
        assert!(id.bytes().all(|b| b.is_ascii_lowercase() || b == b'.' || b == b'_'), "{id}");
        assert!(seen.insert(*id), "{id} is declared twice");
    }
    for id in OrderDenialId::ALL {
        assert_eq!(OrderDenialId::from_word(id.word()), Some(id));
    }
    for id in OrderRefusalId::ALL {
        assert_eq!(OrderRefusalId::from_word(id.word()), Some(id));
    }
    assert_eq!(OrderRefusalId::from_word("order.some_later_id"), None);
}

/// The gate's words, byte for byte: they are the `reason` of every `OrderDenied` a journal holds, the
/// `OrderDrop.reason` of every results JSON, and the key `vike_analytics`' zero-trade report groups by.
#[test]
fn the_denial_words_are_the_bytes_the_gate_always_wrote() {
    let words: Vec<String> = [
        OrderDenial::InvalidSide,
        OrderDenial::Halted,
        OrderDenial::ReduceOnly,
        OrderDenial::ReduceOnlyOvershoot,
        OrderDenial::NonPositiveSize,
        OrderDenial::QuoteRateStale,
        OrderDenial::NoMarkForMountedSymbol,
        OrderDenial::PriceCollar,
        OrderDenial::BelowMinQty,
        OrderDenial::NoReferencePrice,
        OrderDenial::BelowMinNotional,
        OrderDenial::OverMaxNotional,
        OrderDenial::OverMaxExposure,
        OrderDenial::AccountExposureUnpriced,
        OrderDenial::OverAccountExposure,
        OrderDenial::InsufficientMargin,
        OrderDenial::ImpactOverSlippageBudget,
        OrderDenial::ImpactNotFillable,
        OrderDenial::RateLimited,
        OrderDenial::NotACombo,
        OrderDenial::RepeatedLegSymbol,
        OrderDenial::LegZeroRatio,
        OrderDenial::LegNoMark,
        OrderDenial::LegOffGridQty,
        OrderDenial::SymbolNotAdmitted,
        OrderDenial::CashOversell,
        OrderDenial::CashOverbuy,
        OrderDenial::OverMountLeverage,
        OrderDenial::VolumeCap,
        OrderDenial::LatencyReject,
    ]
    .iter()
    .map(ToString::to_string)
    .collect();
    assert_eq!(
        words,
        [
            "invalid-side",
            "halted",
            "reduce-only",
            "reduce-only-overshoot",
            "non-positive-size",
            "quote-rate-stale",
            "no-mark-for-mounted-symbol",
            "price-collar",
            "below-min-qty",
            "no-reference-price",
            "below-min-notional",
            "over-max-notional",
            "over-max-exposure",
            "account-exposure-unpriced",
            "over-account-exposure",
            "insufficient-margin",
            "impact-over-slippage-budget",
            "impact-not-fillable",
            "rate-limited",
            "not-a-combo",
            "repeated-leg-symbol",
            "zero-ratio",
            "no-mark",
            "off-grid-qty",
            "symbol-not-admitted",
            "a spot account cannot sell more than it holds",
            "a spot account cannot buy more than its free cash pays for, the fee included",
            "over-mount-leverage",
            "volume_cap",
            "latency_reject",
        ]
    );
    assert_eq!(OrderDenialId::ALL.len(), words.len(), "every denial is pinned above");
}

#[test]
fn the_halt_file_literal_is_the_reason_the_halt_writes() {
    assert_eq!(OrderRefusal::HaltFile.to_string(), "halted: HALT file present");
    assert_eq!(OrderRefusal::HaltFile.id().word(), "order.halted");
}
