//! Polymarket CLOB hosts + (later) auth config.
//!
//! DATA reads are UNAUTHENTICATED — no creds needed. The EXEC half reads the mounted ACCOUNT's
//! Ethereum L1 private key (its `PRIVATE_KEY` field, [`load_polymarket_creds_from`]) that derives
//! the L2 api creds (apiKey/secret/passphrase) via EIP-712 — a credential shape kept out of
//! Debug/logs. A credential belongs to its account and carries no tier (decision 0156 item 6): this
//! venue is live-only, and its fields are read as fields, never through a composed name.
//!
//! ⚠ **That authenticated/keyless split is also where the DUBLIN EGRESS rule is drawn**, and it is
//! narrower than "all Polymarket traffic": the owner ruled on 2026-09-22 that Dublin is needed for
//! TRADING — signing and orders — exactly and only, so the exec plane must leave via Dublin and the
//! TAPE need not. The rule, its argument and its measured cost live in ONE place,
//! `crates/bridges/polymarket/src/egress.rs`'s `check_expected_egress`; the per-constant notes
//! below only say which side of it each host sits on, because a reader who reconstructs the rule
//! host-by-host from this grid gets it wrong — [`CLOB_BASE`] straddles.

/// Central Limit Order Book REST + WS (chainId 137 for all EIP-712).
///
/// ⚠ **This host STRADDLES the egress rule, which is why that rule is drawn per CALL and not per
/// host.** Its keyless book/midpoint/markets reads are tape; `/auth/derive-api-key`, order submit
/// and order cancel are the exec plane that must leave via Dublin
/// (`crates/bridges/polymarket/src/egress.rs`'s `check_expected_egress`).
pub const CLOB_BASE: &str = "https://clob.polymarket.com";
/// Market catalog (conditions/markets/slugs) — keyless reads, so the TAPE side of the egress rule
/// (`crates/bridges/polymarket/src/egress.rs`'s `check_expected_egress`).
pub const GAMMA_BASE: &str = "https://gamma-api.polymarket.com";
/// Positions / trades data API.
pub const DATA_API_BASE: &str = "https://data-api.polymarket.com";
/// Public market book/trade websocket channel (unauthenticated) — the TAPE, and the plainest case
/// of the half that does NOT require the Dublin egress
/// (`crates/bridges/polymarket/src/egress.rs`'s `check_expected_egress`).
pub const WS_MARKET: &str = "wss://ws-subscriptions-clob.polymarket.com/ws/market";
/// Authenticated CLOB user channel (fills/orders). Same host as [`WS_MARKET`], `/user` path — and
/// the OPPOSITE side of the egress rule for exactly that reason: it authenticates as the account,
/// so it belongs to the exec plane that IS required to leave via Dublin
/// (`crates/bridges/polymarket/src/egress.rs`'s `check_expected_egress`).
// Read only by the `polymarket`-gated user-data pump; still DECLARED unconditionally (it is this
// module's URL grid, and a cfg'd const would fracture the grid) — the #1381 `exchange_url` idiom.
// `unreachable_pub` joins the list for the same feeds-only build: `config` is a private module there
// and the crate-root re-export that makes it reachable is `polymarket`-gated (not converted to
// `pub(crate)`: that re-export and `tests/polymarket_user_smoke.rs` need the `pub`).
#[cfg_attr(
    not(feature = "polymarket"),
    allow(dead_code, unreachable_pub, clippy::allow_attributes)
)]
pub const WS_USER: &str = "wss://ws-subscriptions-clob.polymarket.com/ws/user";
/// Real-Time Data Service (RTDS) websocket — the keyless feed carrying the UNDERLYING reference
/// prices Polymarket's crypto/equity binary markets are struck on (see [`crate::rtds`]). Distinct
/// from [`WS_MARKET`], which carries the CLOB's own book/trade frames for outcome tokens.
///
/// **VERIFIED live 2026-07-22**: this host is correct and KEYLESS for the `crypto_prices`,
/// `crypto_prices_chainlink` and `equity_prices` topics. ⚠ That verification was PROBED through
/// the Dublin host, which is how it was MEASURED and not a requirement of this feed — RTDS is
/// keyless tape, and the standing Dublin constraint covers the exec plane only
/// (`crates/bridges/polymarket/src/egress.rs`'s `check_expected_egress`). It stays a
/// DEFAULT rather than a hard pin — [`crate::rtds::RtdsConfig::url`] is a plain `String` a caller
/// overrides without touching this crate. Nothing connects to it unless a caller opts in by
/// constructing an `RtdsFeed`.
pub const RTDS_WS: &str = "wss://ws-live-data.polymarket.com";

use vike_secrets::account_credentials::AccountCredentials;

// The account FIELDS the loader reads — the words of `vike_venues::credential_schema`'s polymarket
// row, held to it by this module's `the_fields_are_the_key_schemas`.
/// The account field holding the Ethereum L1 private key — the root. Required: its absence is the
/// live gate.
pub(crate) const PRIVATE_KEY: &str = "PRIVATE_KEY";
/// The account field holding the funder/maker (deposit-wallet) address.
pub(crate) const ADDRESS: &str = "ADDRESS";
/// The account field [`load_polymarket_creds_from`] reads for the address when [`ADDRESS`] is
/// absent.
pub(crate) const FUNDER: &str = "FUNDER";
// Every field the loader reads beside the key and the two address fields, in its slot order: the
// L2 trio and the relayer pair.
const API_KEY: &str = "API_KEY";
const SECRET: &str = "SECRET";
const PASSPHRASE: &str = "PASSPHRASE";
const RELAYER_API_KEY: &str = "RELAYER_API_KEY";
const RELAYER_API_KEY_ADDRESS: &str = "RELAYER_API_KEY_ADDRESS";

/// The first whitespace-separated token of a `.env` value (`""` when there is none).
///
/// A value written in the `.env` style may carry a trailing inline `#` comment, e.g. an account's
/// `SIGNATURE_TYPE` field stored as `3   # POLY_1271 (deposit wallet)` and
/// `POLY_PROXY_PORT=11080   # dev-box arbdub tunnel`, and a value is taken verbatim, comment included.
/// Every Polymarket setting that goes through this helper — the proxy url/host/port/enable flags
/// ([`crate::egress`], which took them from `exec` at the feeds/exec split) and the signature type
/// plus reconcile gate ([`crate::exec_plane::recon_client`]) — is a SINGLE-token value, so reading the first
/// token makes an annotated line behave the way its author obviously meant.
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

/// Read Polymarket creds from the mounted ACCOUNT's fields. `None` when the L1 private key
/// (`PRIVATE_KEY`) is unset or blank — the live gate. The L2 trio may be blank (then derived at
/// connect via EIP-712). The funder/deposit-wallet address is the account's `ADDRESS`, else its
/// `FUNDER`.
///
/// One read per field, and no rung: the account's fields carry no tier (the account HAS one, and
/// this venue mounts only `live`), so there is neither a tier-suffixed spelling nor a tier-less one
/// to fall back between, and nothing here can read another account's key.
///
/// ⚠ This venue has NO testnet: every key this reads signs REAL MONEY on Polygon mainnet
/// (`crates/bridges/polymarket/CLAUDE.md`). The `POLY_EXEC`/`POLY_RECONCILE` gates are
/// deliberately NOT per-account — they are deployment-wide opt-ins, and a second account arms only
/// when the venue itself is already armed.
pub fn load_polymarket_creds_from(creds: &AccountCredentials<'_>) -> Option<PolymarketCreds> {
    let get = |field: &str| creds.get(field).unwrap_or_default().to_string();
    let private_key = get(PRIVATE_KEY);
    if private_key.is_empty() {
        return None;
    }
    // funder/deposit-wallet address: explicit ADDRESS, else FUNDER.
    let address = {
        let a = get(ADDRESS);
        if a.is_empty() { get(FUNDER) } else { a }
    };
    Some(PolymarketCreds {
        private_key,
        address,
        api_key: get(API_KEY),
        secret: get(SECRET),
        passphrase: get(PASSPHRASE),
        relayer_key: get(RELAYER_API_KEY),
        relayer_address: get(RELAYER_API_KEY_ADDRESS),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_secrets::account_credentials::CredentialFields;

    /// The account holding `fields`, as a mount is handed it.
    fn holding(fields: &CredentialFields) -> AccountCredentials<'_> {
        AccountCredentials::new(Some(1), Some(fields), None)
    }

    #[test]
    fn creds_gate_and_redaction() {
        assert!(load_polymarket_creds_from(&AccountCredentials::none()).is_none());
        let no_key = CredentialFields::from_pairs([(ADDRESS, "0xabc"), (SECRET, "s3cr3t")]);
        assert!(load_polymarket_creds_from(&holding(&no_key)).is_none(), "no key, no creds");
        let blank_key = CredentialFields::from_pairs([(PRIVATE_KEY, "  ")]);
        assert!(load_polymarket_creds_from(&holding(&blank_key)).is_none(), "blank is absent");
        let fields = CredentialFields::from_pairs([
            (PRIVATE_KEY, "0xdeadbeef"),
            (ADDRESS, "0xabc"),
            (SECRET, "s3cr3t"),
        ]);
        let c = load_polymarket_creds_from(&holding(&fields)).unwrap();
        assert_eq!(c.address, "0xabc");
        let dbg = format!("{c:?}");
        assert!(!dbg.contains("deadbeef") && !dbg.contains("s3cr3t"));
    }

    /// **The fields the loader reads are `vike_venues::credential_schema`'s polymarket row, word for
    /// word**: every account field of the row lands in its slot (`SIGNATURE_TYPE` is the recon
    /// client's — `crate::exec_plane::recon_client`'s `signature_type_for_account` — and `FUNDER`
    /// stands in for an absent `ADDRESS`), the private key is the one required field, and the
    /// venue holds no application field.
    #[test]
    fn the_fields_are_the_key_schemas() {
        use std::collections::BTreeSet;
        use vike_venues::credential_schema::{FieldOwner, schema_for};
        let schema = schema_for(crate::market_feed::VENUE).expect("a polymarket schema row");
        let ours: BTreeSet<&str> = [
            PRIVATE_KEY,
            ADDRESS,
            FUNDER,
            API_KEY,
            SECRET,
            PASSPHRASE,
            RELAYER_API_KEY,
            RELAYER_API_KEY_ADDRESS,
            "SIGNATURE_TYPE",
        ]
        .into_iter()
        .collect();
        let theirs: BTreeSet<&str> = schema.account_fields().map(|f| f.field).collect();
        assert_eq!(ours, theirs, "the loader's fields are the schema row's account fields");
        assert!(schema.venue_fields().next().is_none(), "polymarket holds no venue field");
        assert_eq!(schema.required(FieldOwner::Account), [PRIVATE_KEY]);

        let every: Vec<(&str, String)> =
            theirs.iter().filter(|f| **f != FUNDER).map(|f| (*f, format!("v-{f}"))).collect();
        let fields = CredentialFields::from_pairs(every);
        let c = load_polymarket_creds_from(&holding(&fields)).expect("the key is present");
        assert_eq!(
            [
                c.private_key.as_str(),
                &c.address,
                &c.api_key,
                &c.secret,
                &c.passphrase,
                &c.relayer_key,
                &c.relayer_address,
            ],
            [
                "v-PRIVATE_KEY",
                "v-ADDRESS",
                "v-API_KEY",
                "v-SECRET",
                "v-PASSPHRASE",
                "v-RELAYER_API_KEY",
                "v-RELAYER_API_KEY_ADDRESS",
            ]
        );
    }

    /// `FUNDER` stands in for an absent (or blank) `ADDRESS`, and never overrides a present one.
    #[test]
    fn funder_is_the_address_fallback() {
        let funder = CredentialFields::from_pairs([(PRIVATE_KEY, "0xk"), (FUNDER, "0xfunder")]);
        assert_eq!(load_polymarket_creds_from(&holding(&funder)).unwrap().address, "0xfunder");
        let blank = CredentialFields::from_pairs([
            (PRIVATE_KEY, "0xk"),
            (ADDRESS, " "),
            (FUNDER, "0xfunder"),
        ]);
        assert_eq!(load_polymarket_creds_from(&holding(&blank)).unwrap().address, "0xfunder");
        let both = CredentialFields::from_pairs([
            (PRIVATE_KEY, "0xk"),
            (ADDRESS, "0xaddress"),
            (FUNDER, "0xfunder"),
        ]);
        assert_eq!(load_polymarket_creds_from(&holding(&both)).unwrap().address, "0xaddress");
        let bare = CredentialFields::from_pairs([(PRIVATE_KEY, "0xk")]);
        assert_eq!(load_polymarket_creds_from(&holding(&bare)).unwrap().address, "");
    }
}
