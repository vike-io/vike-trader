//! `eval_signal_rule` over the feed, fill-rate breaker and Polymarket resolution signals.

use super::*;
use crate::rule::{AlertRule, FeedState, RuleTrigger};

// ---- Feed / breaker / resolution signals ------------------------------------------------

#[test]
fn feed_signal_fires_on_the_requested_state_and_scope() {
    let degraded_rule = AlertRule::new(
        "fd",
        RuleTrigger::Feed { venue: Some("binance".into()), state: FeedState::Degraded },
    );
    // matching venue + degraded → fires.
    assert!(
        fire_signal(
            &degraded_rule,
            &AlertSignal::Feed { venue: "binance".into(), degraded: true },
            1
        )
        .is_some()
    );
    // a recovery signal must NOT fire a "degraded" rule.
    assert!(
        fire_signal(
            &degraded_rule,
            &AlertSignal::Feed { venue: "binance".into(), degraded: false },
            2
        )
        .is_none()
    );
    // wrong venue must not fire.
    assert!(
        fire_signal(&degraded_rule, &AlertSignal::Feed { venue: "okx".into(), degraded: true }, 3)
            .is_none()
    );
    // a "recovered" rule fires on the recovery signal.
    let recovered_rule =
        AlertRule::new("fr", RuleTrigger::Feed { venue: None, state: FeedState::Recovered });
    assert!(
        fire_signal(
            &recovered_rule,
            &AlertSignal::Feed { venue: "okx".into(), degraded: false },
            4
        )
        .is_some()
    );
}

#[test]
fn breaker_and_resolution_signals_fire_on_their_own_kind_only() {
    let breaker = AlertRule::new(
        "b",
        RuleTrigger::FillRateBreaker { venue: None, symbol: Some("BTCUSDT".into()) },
    );
    assert!(
        fire_signal(
            &breaker,
            &AlertSignal::FillRateBreaker { venue: "binance".into(), symbol: "BTCUSDT".into() },
            1
        )
        .is_some()
    );
    // symbol mismatch → no fire.
    assert!(
        fire_signal(
            &breaker,
            &AlertSignal::FillRateBreaker { venue: "binance".into(), symbol: "ETHUSDT".into() },
            2
        )
        .is_none()
    );
    // a feed signal never fires a breaker rule.
    assert!(
        fire_signal(&breaker, &AlertSignal::Feed { venue: "binance".into(), degraded: true }, 3)
            .is_none()
    );

    let resolution =
        AlertRule::new("pm", RuleTrigger::PolymarketResolution { token_id: Some("0xtok".into()) });
    assert!(
        fire_signal(
            &resolution,
            &AlertSignal::PolymarketResolution { token_id: "0xtok".into() },
            4
        )
        .is_some()
    );
    assert!(
        fire_signal(
            &resolution,
            &AlertSignal::PolymarketResolution { token_id: "0xOTHER".into() },
            5
        )
        .is_none()
    );
}
