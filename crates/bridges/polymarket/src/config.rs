//! Polymarket CLOB hosts + (later) auth config.
//!
//! DATA reads are UNAUTHENTICATED — no creds needed. The EXEC half will add an Ethereum L1
//! private key (`POLY_PRIVATE_KEY`) that derives the L2 api creds (apiKey/secret/passphrase) via
//! EIP-712 — a new credential shape kept out of Debug/logs, added with the exec slice.

/// Central Limit Order Book REST + WS (chainId 137 for all EIP-712).
pub const CLOB_BASE: &str = "https://clob.polymarket.com";
/// Market catalog (conditions/markets/slugs).
pub const GAMMA_BASE: &str = "https://gamma-api.polymarket.com";
/// Positions / trades data API.
pub const DATA_API_BASE: &str = "https://data-api.polymarket.com";
/// Public market book/trade websocket channel (unauthenticated).
pub const WS_MARKET: &str = "wss://ws-subscriptions-clob.polymarket.com/ws/market";
/// Authenticated CLOB user channel (fills/orders). Same host as [`WS_MARKET`], `/user` path.
// Read only by the `polymarket`-gated user-data pump; still DECLARED unconditionally (it is this
// module's URL grid, and a cfg'd const would fracture the grid) — the #1381 `exchange_url` idiom.
#[cfg_attr(not(feature = "polymarket"), allow(dead_code))]
pub const WS_USER: &str = "wss://ws-subscriptions-clob.polymarket.com/ws/user";
/// Real-Time Data Service (RTDS) websocket — the keyless feed carrying the UNDERLYING reference
/// prices Polymarket's crypto/equity binary markets are struck on (see [`crate::rtds`]). Distinct
/// from [`WS_MARKET`], which carries the CLOB's own book/trade frames for outcome tokens.
///
/// **VERIFIED live 2026-07-22** (probed through the Dublin host): this host is correct and KEYLESS
/// for the `crypto_prices`, `crypto_prices_chainlink` and `equity_prices` topics. It stays a
/// DEFAULT rather than a hard pin — [`crate::rtds::RtdsConfig::url`] is a plain `String` a caller
/// overrides without touching this crate. Nothing connects to it unless a caller opts in by
/// constructing an `RtdsFeed`.
pub const RTDS_WS: &str = "wss://ws-live-data.polymarket.com";

use std::collections::HashMap;
use vike_bridge_core::credentials::{Environment, account_var};
use vike_model::account_keys::AccountLabel;

/// The first whitespace-separated token of a `.env` value (`""` when there is none).
///
/// `vike_bridge_core::credentials::parse_dotenv` trims and strips surrounding quotes but does NOT
/// strip a trailing inline `#` comment — and the real workspace `.env` writes exactly that style,
/// e.g. `POLY_SIGNATURE_TYPE=3   # POLY_1271 (deposit wallet)` and
/// `POLY_PROXY_PORT=11080   # dev-box arbdub tunnel`, whose parsed values carry the whole comment.
/// Every Polymarket setting that goes through this helper — the proxy url/host/port/enable flags
/// ([`crate::exec`]) and the signature type + reconcile gate ([`crate::recon_client`]) — is a
/// SINGLE-token value, so reading the first token makes an annotated line behave the way its author
/// obviously meant. The shared parser is deliberately left alone: changing it would alter every
/// other consumer's behavior at once.
///
/// This was found the hard way — a `.env`-declared tunnel port with a trailing comment produced
/// `bad port in "127.0.0.1:11080   # dev-box arbdub tunnel"` and the run silently egressed direct.
pub(crate) fn first_token(v: &str) -> &str {
    v.split_whitespace().next().unwrap_or("")
}

/// Polymarket credentials. `private_key` is the Ethereum L1 key (root) that DERIVES the L2 api
/// creds (api_key/secret/passphrase) via EIP-712; the L2 trio may also be supplied directly. The
/// private key + L2 secret never reach Debug/Display/logs.
#[derive(Clone, Default)]
pub struct PolymarketCreds {
    /// eth L1 private key (hex, `0x…`) — the root; derives L2 + signs orders.
    pub private_key: String,
    /// funder/maker address (`0x…`).
    pub address: String,
    /// L2 api key (derived or supplied).
    pub api_key: String,
    /// L2 secret (base64url).
    pub secret: String,
    /// L2 passphrase.
    pub passphrase: String,
    /// Relayer API key (deposit-wallet / gasless order flow) — secret, kept out of Debug.
    pub relayer_key: String,
    /// Relayer API key address (the authorized EOA that owns the deposit wallet).
    pub relayer_address: String,
}

impl std::fmt::Debug for PolymarketCreds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // never leak private_key / secret
        write!(
            f,
            "PolymarketCreds(address={}, api_key_set={}, l2={}, relayer={})",
            self.address,
            !self.api_key.is_empty(),
            if self.secret.is_empty() { "unset" } else { "set" },
            if self.relayer_key.is_empty() { "unset" } else { "set" }
        )
    }
}

/// Env-var names for Polymarket at `env` (private_key, address, api_key, secret, passphrase).
pub fn poly_env_var_names(env: Environment) -> [String; 5] {
    let p = format!("POLY_{}", env.as_str());
    [
        format!("{p}_PRIVATE_KEY"),
        format!("{p}_ADDRESS"),
        format!("{p}_API_KEY"),
        format!("{p}_SECRET"),
        format!("{p}_PASSPHRASE"),
    ]
}

/// Read Polymarket creds from a var map. `None` when the L1 private key is unset — the live gate.
/// The L2 trio may be blank (then derived at connect via EIP-712).
pub fn load_polymarket_creds_from(
    env: Environment,
    vars: &HashMap<String, String>,
) -> Option<PolymarketCreds> {
    load_polymarket_creds_for_account(env, &AccountLabel::Default, vars)
}

/// [`load_polymarket_creds_from`] for ONE NAMED ACCOUNT — `POLY_{TIER}_{SUFFIX}__{LABEL}`, and
/// `POLY_{SUFFIX}__{LABEL}` for the un-suffixed spelling this venue's live keys also use.
///
/// Everything [`load_polymarket_creds_from`] documents holds word for word, BOTH fallback chains
/// included (the legacy `MAINNET` tier, and the tier-less `POLY_X` name); the ONLY difference is
/// the NAMES read, composed by `vike_bridge_core::credentials::account_var`, which appends the
/// label after the WHOLE of today's key.
///
/// ⚠ **The tier-less fallback stays a TIER fallback, not an ACCOUNT one.** `POLY_PRIVATE_KEY__ALT`
/// is what a labelled account falls back to — never `POLY_PRIVATE_KEY`. Both rungs are the SAME
/// account; nothing here ever reads another account's key.
///
/// ⚠ **[`AccountLabel::Default`] is byte-identically [`load_polymarket_creds_from`]**, reached
/// through it.
///
/// ⚠ This venue has NO testnet: every key this reads signs REAL MONEY on Polygon mainnet
/// (`crates/bridges/polymarket/CLAUDE.md`), so the no-fallback rule is doing its heaviest work
/// here. The `POLY_EXEC`/`POLY_RECONCILE` gates are deliberately NOT per-account — they are
/// deployment-wide opt-ins, and a second account arms only when the venue itself is already armed.
///
/// ⚠ **`vike_model::account_keys::account_ref_from_key` classifies NONE of these names**, because
/// no roster venue is spelled `POLY` — so a labelled polymarket account is not DISCOVERED from the
/// store the way a labelled oanda one is. It is named in `policy.toml`'s `[accounts]` table
/// instead, which is the other half of `vike_mount`'s `known_accounts_in`.
pub fn load_polymarket_creds_for_account(
    env: Environment,
    label: &AccountLabel,
    vars: &HashMap<String, String>,
) -> Option<PolymarketCreds> {
    // primary tier (POLY_LIVE_*), then the legacy tier (POLY_MAINNET_*) — pre-rename `.env`
    // files keep working (same contract as credentials::load_credentials_from).
    load_poly_tier(env.as_str(), label, vars)
        .or_else(|| env.legacy_str().and_then(|t| load_poly_tier(t, label, vars)))
}

fn load_poly_tier(
    tier: &str,
    label: &AccountLabel,
    vars: &HashMap<String, String>,
) -> Option<PolymarketCreds> {
    // Try the tier-suffixed name first (POLY_{TIER}_X), then the real no-suffix name (POLY_X) —
    // Polymarket is Polygon-mainnet-only and the live keys are stored un-suffixed. BOTH rungs are
    // account-scoped: the fallback is between TIER spellings of one account, never between
    // accounts.
    let prefix = format!("POLY_{tier}_");
    let get = |suffix: &str| {
        account_var(vars, &format!("{prefix}{suffix}"), label)
            .or_else(|| account_var(vars, &format!("POLY_{suffix}"), label))
            .map(str::to_string)
            .unwrap_or_default()
    };
    let private_key = get("PRIVATE_KEY");
    if private_key.is_empty() {
        return None;
    }
    // funder/deposit-wallet address: explicit ADDRESS, else POLY_FUNDER.
    let address = {
        let a = get("ADDRESS");
        if a.is_empty() { get("FUNDER") } else { a }
    };
    Some(PolymarketCreds {
        private_key,
        address,
        api_key: get("API_KEY"),
        secret: get("SECRET"),
        passphrase: get("PASSPHRASE"),
        relayer_key: get("RELAYER_API_KEY"),
        relayer_address: get("RELAYER_API_KEY_ADDRESS"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creds_gate_and_redaction() {
        let mut vars = HashMap::new();
        assert!(load_polymarket_creds_from(Environment::Live, &vars).is_none());
        vars.insert("POLY_MAINNET_PRIVATE_KEY".into(), "0xdeadbeef".into());
        vars.insert("POLY_MAINNET_ADDRESS".into(), "0xabc".into());
        vars.insert("POLY_MAINNET_SECRET".into(), "s3cr3t".into());
        let c = load_polymarket_creds_from(Environment::Live, &vars).unwrap();
        assert_eq!(c.address, "0xabc");
        let dbg = format!("{c:?}");
        assert!(!dbg.contains("deadbeef") && !dbg.contains("s3cr3t"));
    }
}
