//! The helpers more than one `trade` member shares, compiled once in the group binary (`tests/trade.rs`).

/// The adapter start a member sends when it restarts the adapter: BTCUSDT with recon, no trigger and a
/// route key the fake peer ignores.
pub(crate) fn start_args() -> vike_gatewayhub_client::proto::AdapterStart {
    vike_gatewayhub_client::proto::AdapterStart {
        rid: 0,
        symbol: "BTCUSDT".into(),
        declared_legs: Vec::new(),
        recon_enabled: true,
        takes_trigger: false,
        risk: None,
        market_slippage: None,
        halt_admit: vike_model::HaltAdmit::Admit,
        withhold: None,
        account_leverage: None,
        read_account_ccy: false,
        route_key: "ignored".into(),
    }
}
