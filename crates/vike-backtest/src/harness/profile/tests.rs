//! `BacktestProfile` parse and validate tests: the profile fixtures several topics share, and
//! `refused_at_load`, the one load-time refusal check they call.

use super::*;

const BAR_TOML: &str = r#"
name = "bar-demo"

[data]
venue = "binance"
symbols = ["BTCUSDT", "ETHUSDT"]
kind = "bar"
interval = "1h"
from = "2026-01-01T00"
to = "2026-01-02T00"

[engine]
cash = 100000.0
fee_rate = 0.001

[strategy]
name = "sma_cross"
[strategy.params]
fast = 10
slow = 20
"#;

const TICK_TOML: &str = r#"
name = "demo"

[data]
venue = "polymarket"
symbols = ["0xTOK"]
kind = "tick"
from = "2026-04-13T19"
to = "2026-04-13T20"

[engine]
cash = 10000.0
snap_to_properties = true

[strategy]
name = "buy_hold"
[strategy.params]
size = 1.0
"#;

// --- G4: the cross-venue `[[data.series]]` slice ------------------------------------------

const CROSS_VENUE_TOML: &str = r#"
name = "cheap-np-window"

[data]
kind = "tick"
from = "1774999800000"
to = "1775002500000"

[[data.series]]
venue = "spot"
symbol = "BTCUSDT"
kind = "quote"

[[data.series]]
venue = "polymarket"
symbol = "btc-updown-5m-1775001600#0"
kind = "trade"

[[data.series]]
venue = "polymarket"
symbol = "btc-updown-5m-1775001600#1"
kind = "trade"

[engine]
cash = 1000.0

[engine.fee]
kind = "probability_scaled"
taker_rate = 0.072

[engine.resolution]
kind = "binary_outcome"
[engine.resolution.winners]
"btc-updown-5m-1775001600" = 0

[strategy]
name = "cheap_catch_updown_fair_value"
[strategy.params]
spot_symbol = "BTCUSDT"
"#;

// --- the one load-time refusal check the topic children share -----------------------------

/// Load `toml` and assert it is REFUSED at load, through the `arm` variant, with every needle in
/// the refusal SENTENCE (`HarnessError::message`, no `Display` prefix). `arm` is the variant's own
/// constructor (`HarnessError::Validation`, `HarnessError::Parse`): which door refused is part of
/// the contract (`crates/vike-backtest/src/harness/profile/tests/loading.rs`'s
/// `the_accumulating_door_does_not_change_the_error_variant`), so each call site names it rather
/// than inheriting a default. An empty `needles` checks the arm alone. Every failure prints the
/// profile text or the missing needle, so a call made once per input inside a loop stays
/// diagnosable without a per-input label.
#[track_caller]
fn refused_at_load(toml: &str, arm: fn(String) -> HarnessError, needles: &[&str]) {
    let Err(err) = BacktestProfile::from_toml_str(toml) else {
        panic!("this profile must be refused at load, and it loaded:\n{toml}");
    };
    assert_eq!(
        std::mem::discriminant(&err),
        std::mem::discriminant(&arm(String::new())),
        "refused through the wrong arm: {err:?}\nprofile:\n{toml}"
    );
    let text = err.message();
    for needle in needles {
        assert!(text.contains(needle), "missing needle {needle:?} in refusal: {text}");
    }
}

/// The helper's own guard: a refusal that does not carry the needle FAILS, so a test reduced to
/// one call cannot pass by asserting nothing.
#[test]
#[should_panic(expected = "missing needle")]
fn refused_at_load_fails_when_the_error_lacks_the_needle() {
    let toml = BAR_TOML.replace("cash = 100000.0", "cash = 0.0");
    refused_at_load(&toml, HarnessError::Validation, &["a phrase the error does not contain"]);
}

/// ...and a refusal through the OTHER arm fails too: an unparseable stamp is a `Parse`, and a
/// helper that read only the sentence would let it stand in for a `Validation`.
#[test]
#[should_panic(expected = "refused through the wrong arm")]
fn refused_at_load_fails_when_the_refusal_arrives_through_another_arm() {
    let toml = BAR_TOML.replace("from = \"2026-01-01T00\"", "from = \"not-a-timestamp\"");
    refused_at_load(&toml, HarnessError::Validation, &[]);
}

#[cfg(test)]
mod core_and_data;
#[cfg(test)]
mod engine;
#[cfg(test)]
mod fee;
#[cfg(test)]
mod loading;
#[cfg(test)]
mod resolution_risk;
#[cfg(test)]
mod sizer;
