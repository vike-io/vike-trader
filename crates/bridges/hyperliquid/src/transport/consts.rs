//! Hyperliquid endpoints + protocol constants (mainnet/testnet host pairs, the two EIP-712 chain
//! ids, per-product price max-decimals, min notional, asset-id offsets). Verified against the
//! official docs — see `docs/research/2026-07-16-hyperliquid-adapters/README.md` §5, §9.

/// Canonical venue string (the `capabilities_for`/report/`feeds`-map key).
pub const VENUE: &str = "hyperliquid";

/// The read endpoint's path below a network's `rest` `venue_url` row (`crate::config`'s
/// `base_url`). The hosts are rows; the paths stay here.
pub const INFO_PATH: &str = "/info";
/// The signed-action endpoint's path below the same row.
pub const EXCHANGE_PATH: &str = "/exchange";
/// The websocket's path below a network's `ws` row.
pub const WEBSOCKET_PATH: &str = "/ws";

/// EIP-712 domain `chainId` for L1 (phantom-agent) actions — a FIXED `1337`, independent of the
/// wallet's chain. (Getting this wrong yields opaque signature rejections.)
pub const EXCHANGE_CHAIN_ID: u64 = 1337;
/// `signatureChainId` for user-signed actions (transfers/approvals; deferred) — `0x66eee`
/// (421614, Arbitrum Sepolia).
pub const USER_SIGNED_CHAIN_ID: u64 = 0x0006_6eee;
/// [`USER_SIGNED_CHAIN_ID`] as the action body's `signatureChainId` hex STRING, the value the Python SDK
/// hardcodes for BOTH mainnet and testnet (`int("0x66eee", 16)` == [`USER_SIGNED_CHAIN_ID`]).
pub const SIGNATURE_CHAIN_ID: &str = "0x66eee";

/// Price rule: ≤ 5 significant figures AND ≤ `MAX_DECIMALS - szDecimals` decimals; integer prices
/// always valid. `MAX_DECIMALS` = 6 (perp) / 8 (spot).
pub const PERP_MAX_DECIMALS: u32 = 6;
pub const SPOT_MAX_DECIMALS: u32 = 8;
/// Prices carry at most this many significant figures (non-integer).
pub const PRICE_MAX_SIG_FIGS: u32 = 5;

/// Minimum order notional in USD (venue rejects below this with "Order must have minimum value of
/// $10.").
pub const MIN_NOTIONAL_USD: f64 = 10.0;

/// Spot order asset id = `SPOT_ASSET_OFFSET + spotUniverseIndex` (PURR/USDC = 10000). Perp order
/// asset id = the `universe` array index directly.
pub const SPOT_ASSET_OFFSET: u32 = 10_000;

/// HIP-3 builder-deployed perp order asset id (official HL "Asset IDs" doc):
/// `PERP_DEX_ASSET_BASE + perp_dex_index * PERP_DEX_ASSET_STRIDE + index_in_dex_meta`, where
/// `perp_dex_index` is the entry's POSITION in the `perpDexs` array (index 0 = the core perp dex,
/// returned as `null`; builder dexs start at 1) and `index_in_dex_meta` is the market's position in
/// that dex's own `meta.universe`. Worked example from the docs: `test:ABC` at `perp_dex_index` 1,
/// meta index 0 → `100000 + 1*10000 + 0` = **110000**. Distinct from the core (`0..N`) and spot
/// (`10000+`) ranges, so a HIP-3 market can never collide with a core asset id.
pub const PERP_DEX_ASSET_BASE: u32 = 100_000;
pub const PERP_DEX_ASSET_STRIDE: u32 = 10_000;

/// Every Hyperliquid perp funds HOURLY: one protocol-wide cadence, not a per-asset `meta` field, so
/// the catalog states it as this constant rather than reading it. The account's payment poll looks
/// back five minutes against it (`crates/vike-bridge-core/src/exec_actor/funding_poll.rs`'s
/// `FUNDING_LOOKBACK_MS`), and the `fundingHistory` rows its tests pin are one hour apart. It is the
/// `ContractPerp::funding_interval_ms` of every
/// catalog perp row, core and HIP-3 alike (a builder dex sets its own funding multiplier, not its
/// interval).
pub const FUNDING_INTERVAL_MS: u64 = 3_600_000;

/// WS keepalive: send `{"method":"ping"}` at least this often (server drops a connection idle > 60s).
///
/// The market pump's row (`crates/vike-bridge-core/src/websocket_session/spec.rs`'s `row`) declares
/// this 30 s as a literal and cites this constant; the user-data pump no longer reads it and takes
/// its ping from the exec lane of `crates/vike-venues/src/venue_timings.rs`'s `HYPERLIQUID`.
/// The three are held equal by `crates/bridges/hyperliquid/src/execution/user_data_tests.rs`'s
/// `websocket_ping_secs_is_the_execution_lanes_keepalive_and_the_market_pump_rows`.
pub const WEBSOCKET_PING_SECS: u64 = 30;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_chain_id_string_matches_the_numeric_const() {
        // The action's hex string and the domain's numeric chainId are two views of ONE value.
        assert_eq!(SIGNATURE_CHAIN_ID, format!("0x{USER_SIGNED_CHAIN_ID:x}"));
        assert_eq!(USER_SIGNED_CHAIN_ID, 421_614);
    }
}
