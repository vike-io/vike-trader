//! What THIS crate proves of the shared trades stack: that Aster's own `env`-keyed URL table came
//! through the family extraction unchanged. The family's decode, splice, earliest-id and backfill
//! tests call `vike_binance::family::trades` directly, not an Aster wrapper, so they live once, in
//! `crates/bridges/binance/src/trades_tests.rs`.

use super::*;
use vike_binance::family::trades::TRADES_SEED_LIMIT;

/// The `.P` perp split for the trades feed: `agg_trades_ws_url`/`agg_trades_rest_url` route a
/// perp (`is_perp = true`, api_symbol already `.P`-stripped) to the USDⓈ-M fapi/fstream hosts
/// and spot to sapi/sstream, lower-casing the WS path symbol either way; `env` further splits
/// testnet vs mainnet. Mirrors `data.rs`'s `rest_klines_base_selects_fapi_for_perp_and_testnet_vs_mainnet`.
/// Also the rung-2 guard that Aster's hosts came through the [`UrlTable`] extraction unchanged.
#[test]
fn agg_trades_urls_split_perp_vs_spot_and_testnet_vs_mainnet() {
    // Spot testnet: exchange symbol == series label, is_perp = false.
    assert_eq!(
        agg_trades_ws_url("BTCUSDT", false, AccountTier::Demo),
        "wss://sstream.asterdex-testnet.com/ws/btcusdt@aggTrade"
    );
    assert_eq!(
        agg_trades_rest_url("BTCUSDT", false, TRADES_SEED_LIMIT, None, AccountTier::Demo),
        "https://sapi.asterdex-testnet.com/api/v3/aggTrades?symbol=BTCUSDT&limit=1000"
    );
    // Perp testnet: `.P` already stripped to `api_symbol` by the caller; routes to fapi/fstream.
    assert_eq!(
        agg_trades_ws_url("BTCUSDT", true, AccountTier::Demo),
        "wss://fstream.asterdex-testnet.com/ws/btcusdt@aggTrade"
    );
    assert_eq!(
        agg_trades_rest_url("BTCUSDT", true, TRADES_SEED_LIMIT, None, AccountTier::Demo),
        "https://fapi.asterdex-testnet.com/fapi/v3/aggTrades?symbol=BTCUSDT&limit=1000"
    );
    // Mainnet (Live): same split, different hosts.
    assert_eq!(
        agg_trades_ws_url("BTCUSDT", false, AccountTier::Live),
        "wss://sstream.asterdex.com/ws/btcusdt@aggTrade"
    );
    assert_eq!(
        agg_trades_rest_url("BTCUSDT", true, TRADES_SEED_LIMIT, None, AccountTier::Live),
        "https://fapi.asterdex.com/fapi/v3/aggTrades?symbol=BTCUSDT&limit=1000"
    );
    // fromId backfill query (Some(id)):
    assert_eq!(
        agg_trades_rest_url("BTCUSDT", false, 1000, Some(42), AccountTier::Demo),
        "https://sapi.asterdex-testnet.com/api/v3/aggTrades?symbol=BTCUSDT&limit=1000&fromId=42"
    );
}
