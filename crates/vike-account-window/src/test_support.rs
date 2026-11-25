//! The fixtures the Account window's unit tests share: an empty venue block and a mount.

use vike_execution::{BalanceMode, PositionView, VenueBlock};
use vike_model::TradingState;

pub(crate) fn venue_block(venue: &str, positions: Vec<PositionView>) -> VenueBlock {
    VenueBlock {
        account_code: None,
        order_client_id_shape: None,
        venue: venue.to_string(),
        account: None,
        route_key: venue.to_string(),
        symbol: String::new(),
        extra_symbols: Default::default(),
        tier: None,
        balance: 0.0,
        realized_pnl: 0.0,
        fees_paid: 0.0,
        funding_paid: 0.0,
        balance_mode: BalanceMode::Delta,
        contract_values: Default::default(),
        contract_value_default: vike_model::ContractValue::default(),
        admits_symbols: false,
        quote_rates: Vec::new(),
        quote_rates_in: Default::default(),
        ledger_currency: Default::default(),
        currency_assumed: false,
        colliding_fills_refused: 0,
        fees_unconverted: 0,
        fees_at_stale_rate: 0,
        equity: 0.0,
        unrealized: 0.0,
        missing_prices: 0,
        margin_used: 0.0,
        free_buying_power: 0.0,
        margin_ratio: 0.0,
        fee_schedule: None,
        trading_state: TradingState::Active,
        positions,
    }
}

pub(crate) fn mount(
    route_key: &str,
    tier: Option<&str>,
) -> vike_tradehub_client::wire::MountViewWire {
    vike_tradehub_client::wire::MountViewWire {
        order_client_id_code: None,
        refusal_id: None,
        strategy: "spread_maker".to_string(),
        params: String::new(),
        // `live` reads `true` for a demo account: it must never be what a row says.
        live: true,
        venue: "bybit".to_string(),
        symbol: "BTCUSDT".to_string(),
        interval: "1m".to_string(),
        mount_id: String::new(),
        typed_params: None,
        asset_class: None,
        latched: false,
        route_key: route_key.to_string(),
        tier: tier.map(str::to_string),
        not_running: false,
    }
}
