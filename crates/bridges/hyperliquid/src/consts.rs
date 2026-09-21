//! Hyperliquid endpoints + protocol constants (mainnet/testnet host pairs, the two EIP-712 chain
//! ids, per-product price max-decimals, min notional, asset-id offsets). Verified against the
//! official docs — see `docs/research/2026-07-16-hyperliquid-adapters/README.md` §5, §9.

/// Canonical venue string (the `caps_for`/report/`feeds`-map key).
pub const VENUE: &str = "hyperliquid";

pub const MAINNET_INFO: &str = "https://api.hyperliquid.xyz/info";
pub const MAINNET_EXCHANGE: &str = "https://api.hyperliquid.xyz/exchange";
pub const MAINNET_WS: &str = "wss://api.hyperliquid.xyz/ws";

pub const TESTNET_INFO: &str = "https://api.hyperliquid-testnet.xyz/info";
pub const TESTNET_EXCHANGE: &str = "https://api.hyperliquid-testnet.xyz/exchange";
pub const TESTNET_WS: &str = "wss://api.hyperliquid-testnet.xyz/ws";

/// EIP-712 domain `chainId` for L1 (phantom-agent) actions — a FIXED `1337`, independent of the
/// wallet's chain. (Getting this wrong yields opaque signature rejections.)
pub const EXCHANGE_CHAIN_ID: u64 = 1337;
/// `signatureChainId` for user-signed actions (transfers/approvals; deferred) — `0x66eee`
/// (421614, Arbitrum Sepolia).
pub const USER_SIGNED_CHAIN_ID: u64 = 0x0006_6eee;

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

/// Opt-in gate (process env, the EXACT string `"1"` — the `VIKE_RECONCILE` idiom, not a fuzzy
/// truthy parse; default OFF) for enumerating HIP-3 builder-deployed perp dexs at instrument-load
/// time. Unset ⇒ only the core `meta`/`spotMeta` are fetched (byte-identical to the pre-HIP-3
/// universe); `HYPERLIQUID_HIP3=1` ⇒ `perpDexs` + each builder dex's `meta` (with the `dex` param)
/// are additionally folded in.
pub const HIP3_ENV: &str = "HYPERLIQUID_HIP3";

/// WS keepalive: send `{"method":"ping"}` at least this often (server drops a connection idle > 60s).
pub const WS_PING_SECS: u64 = 30;
