//! `BacktestProfile` parse and validate tests: the profile fixtures several topics share.

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
