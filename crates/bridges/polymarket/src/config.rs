//! Polymarket CLOB hosts + (later) auth config.
//!
//! DATA reads are UNAUTHENTICATED — no creds needed. The EXEC half reads the started ACCOUNT's
//! Ethereum L1 private key (its `PRIVATE_KEY` field, [`load_polymarket_credentials_from`]) that derives
//! the L2 api creds (apiKey/secret/passphrase) via EIP-712 — a credential shape kept out of
//! Debug/logs. A credential belongs to its account and carries no tier (decision 0156 item 6): this
//! venue is live-only, and its fields are read as fields, never through a composed name.
//!
//! ⚠ **That authenticated/keyless split is also where the DUBLIN EGRESS rule is drawn**, and it is
//! narrower than "all Polymarket traffic": the owner ruled on 2026-09-22 that Dublin is needed for
//! TRADING — signing and orders — exactly and only, so the exec plane must leave via Dublin and the
//! TAPE need not. The rule, its argument and its measured cost live in ONE place,
//! `crates/bridges/polymarket/src/transport/mod.rs`'s `check_expected_egress`; the per-constant notes
//! below only say which side of it each host sits on, because a reader who reconstructs the rule
//! host-by-host from this grid gets it wrong — [`clob_base`] straddles.
//!
//! **Every host is a `venue_url` row** (`vike_venues::venue_urls`, decision 0265): each function
//! below is ONE row's lookup, and the code appends the path. Every lookup asks type `live`, never
//! `UrlType::of_tier`: this venue has no demo network, a `demo` or `paper` account stays NOT RUNNING
//! through the adapter's `LiveOnlyArm`, and a demo lookup of a data row would fall back to live,
//! which must never become a demo exec path. A refusal's `Display` is the NOT RUNNING reason.

use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlRefusal, UrlType, VenueUrls};

/// Central Limit Order Book REST (chainId 137 for all EIP-712): row `live trade rest ""`, for EVERY
/// CLOB call, the keyless `/book`, `/midpoint`, `/markets`, `/tick-size`, `/neg-risk` and
/// `/clob-markets/` reads included (the seed holds no data row for this host).
///
/// ⚠ **This host STRADDLES the egress rule, which is why that rule is drawn per CALL and not per
/// host.** Its keyless book/midpoint/markets reads are tape; `/auth/derive-api-key`, order submit
/// and order cancel are the exec plane that must leave via Dublin
/// (`crates/bridges/polymarket/src/transport/mod.rs`'s `check_expected_egress`). Dialled with no account, so
/// `crates/vike-venues/src/venue_urls.rs`'s `KEYLESS_FEED_DIALS` names the row (as for [`market_websocket_url`]).
pub(crate) fn clob_base(urls: &VenueUrls) -> Result<&str, UrlRefusal> {
    urls.url(UrlType::Live, UrlChannel::Trade, UrlProtocol::Rest, "")
}
/// Market catalog (conditions/markets/slugs), row `live data rest gamma` — keyless reads, so the
/// TAPE side of the egress rule (`crates/bridges/polymarket/src/transport/mod.rs`'s `check_expected_egress`).
pub(crate) fn gamma_base(urls: &VenueUrls) -> Result<&str, UrlRefusal> {
    urls.url(UrlType::Live, UrlChannel::Data, UrlProtocol::Rest, "gamma")
}
/// Positions / trades data API, row `live data rest data` (`/v2/positions` in code).
#[cfg(feature = "polymarket")]
pub(crate) fn data_api_base(urls: &VenueUrls) -> Result<&str, UrlRefusal> {
    urls.url(UrlType::Live, UrlChannel::Data, UrlProtocol::Rest, "data")
}
/// Public market book/trade websocket channel (unauthenticated): row `live user ws ""` plus
/// `/ws/market` — the TAPE, and the plainest case of the half that does NOT require the Dublin
/// egress (`crates/bridges/polymarket/src/transport/mod.rs`'s `check_expected_egress`). It asks the `user`
/// row because the seed holds only that one for this host.
pub(crate) fn market_websocket_url(urls: &VenueUrls) -> Result<String, UrlRefusal> {
    Ok(format!(
        "{}/ws/market",
        urls.url(UrlType::Live, UrlChannel::User, UrlProtocol::Websocket, "")?
    ))
}
/// Authenticated CLOB user channel (fills/orders): the same row as [`market_websocket_url`] plus
/// `/ws/user` — and the OPPOSITE side of the egress rule for exactly that reason: it authenticates
/// as the account, so it belongs to the exec plane that IS required to leave via Dublin
/// (`crates/bridges/polymarket/src/transport/mod.rs`'s `check_expected_egress`).
#[cfg(feature = "polymarket")]
pub(crate) fn user_websocket_url(urls: &VenueUrls) -> Result<String, UrlRefusal> {
    Ok(format!(
        "{}/ws/user",
        urls.url(UrlType::Live, UrlChannel::User, UrlProtocol::Websocket, "")?
    ))
}
/// Real-Time Data Service (RTDS) websocket, row `live data ws live-data` — the keyless feed carrying
/// the UNDERLYING reference prices Polymarket's crypto/equity binary markets are struck on (see
/// [`crate::feed::rtds`]). Distinct from [`market_websocket_url`], which carries the CLOB's own book/trade frames
/// for outcome tokens.
///
/// **VERIFIED live 2026-07-22**: the seed's host is correct and KEYLESS for the `crypto_prices`,
/// `crypto_prices_chainlink` and `equity_prices` topics. ⚠ That verification was PROBED through
/// the Dublin host, which is how it was MEASURED and not a requirement of this feed — RTDS is
/// keyless tape, and the standing Dublin constraint covers the exec plane only
/// (`crates/bridges/polymarket/src/transport/mod.rs`'s `check_expected_egress`). Nothing connects to it
/// unless a caller opts in by constructing an `FeedRtds`.
pub(crate) fn rtds_url(urls: &VenueUrls) -> Result<&str, UrlRefusal> {
    urls.url(UrlType::Live, UrlChannel::Data, UrlProtocol::Websocket, "live-data")
}
/// The gasless relayer (deposit-wallet batches: approvals, redeem, split/merge), row `live trade
/// rest relayer` (`/nonce`, `/submit` in code). REAL funds.
#[cfg(feature = "polymarket")]
pub(crate) fn relayer_base(urls: &VenueUrls) -> Result<&str, UrlRefusal> {
    urls.url(UrlType::Live, UrlChannel::Trade, UrlProtocol::Rest, "relayer")
}
/// The keyless Polygon JSON-RPC default, row `live data rest rpc`. A keyed RPC url belongs in the
/// credential store, never in this table.
#[cfg(feature = "polymarket")]
pub(crate) fn rpc_url(urls: &VenueUrls) -> Result<&str, UrlRefusal> {
    urls.url(UrlType::Live, UrlChannel::Data, UrlProtocol::Rest, "rpc")
}
/// The web host serving the order-placement geoblock pre-flight, row `live login rest geoblock`
/// (`/api/geoblock` in code, `crates/bridges/polymarket/src/transport/mod.rs`'s `observe_geoblock`).
#[cfg(feature = "polymarket")]
pub(crate) fn geoblock_base(urls: &VenueUrls) -> Result<&str, UrlRefusal> {
    urls.url(UrlType::Live, UrlChannel::Login, UrlProtocol::Rest, "geoblock")
}

/// One row an exec-plane start dials, or `None` with the refusal's words at `error!`: `what` (the
/// thing that then does NOT RUN) is NOT RUNNING, and nothing is dialled in its place.
#[cfg(feature = "polymarket")]
pub(crate) fn row_or_not_running<T>(what: &str, row: Result<T, UrlRefusal>) -> Option<T> {
    let say = |e: &UrlRefusal| tracing::error!(target: "vike_polymarket::config", "{what} is NOT RUNNING: {e}");
    row.inspect_err(say).ok()
}

use vike_model::secret::Secret;
use vike_secrets::account_credentials::AccountCredentials;

// The account FIELDS the loader reads — the words of `vike_venues::credential_schema`'s polymarket
// row, held to it by this module's `the_fields_are_the_key_schemas`.
/// The account field holding the Ethereum L1 private key — the root. Required: its absence is the
/// live gate.
pub(crate) const PRIVATE_KEY: &str = "PRIVATE_KEY";
/// The account field holding the funder/maker (deposit-wallet) address.
pub(crate) const ADDRESS: &str = "ADDRESS";
/// The account field [`load_polymarket_credentials_from`] reads for the address when [`ADDRESS`] is
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
/// ([`crate::transport`], which took them from `exec` at the feeds/exec split) and the signature type
/// plus reconcile gate ([`crate::reconcile`]) — is a SINGLE-token value, so reading the first
/// token makes an annotated line behave the way its author obviously meant.
///
/// This was found the hard way — a `.env`-declared tunnel port with a trailing comment produced
/// `bad port in "127.0.0.1:11080   # dev-box arbdub tunnel"` and the run silently egressed direct.
pub(crate) fn first_token(v: &str) -> &str {
    v.split_whitespace().next().unwrap_or("")
}

/// Polymarket credentials. `private_key` is the Ethereum L1 key (root) that DERIVES the L2 api
/// creds (api_key/secret/passphrase) via EIP-712; the L2 trio may also be supplied directly. The
/// key, the L2 trio and the relayer key are [`Secret`]s, so none has a `Display`; a blank one is held
/// as an empty value (the L2 trio is then derived). The `Debug` below stays hand-written: it says
/// which of them is POPULATED, the one non-secret fact an operator debugging a start needs (the
/// same allowlist `crate::transport::sign::l1`'s `DerivedL2` prints), which a derived one cannot.
#[derive(Clone)]
pub struct PolymarketConfig {
    /// eth L1 private key (hex, `0x…`) — the root; derives L2 + signs orders.
    pub private_key: Secret<String>,
    /// funder/maker address (`0x…`).
    pub address: String,
    /// L2 api key (derived or supplied).
    pub api_key: Secret<String>,
    /// L2 secret (base64url).
    pub secret: Secret<String>,
    /// L2 passphrase.
    pub passphrase: Secret<String>,
    /// Relayer API key (deposit-wallet / gasless order flow).
    pub relayer_key: Secret<String>,
    /// Relayer API key address (the authorized EOA that owns the deposit wallet).
    pub relayer_address: String,
}

impl std::fmt::Debug for PolymarketConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // never leak private_key / secret
        write!(
            f,
            "PolymarketConfig(address={}, api_key_set={}, l2={}, relayer={})",
            self.address,
            !self.api_key.expose().is_empty(),
            if self.secret.expose().is_empty() { "unset" } else { "set" },
            if self.relayer_key.expose().is_empty() { "unset" } else { "set" }
        )
    }
}

/// Every field blank, as the derive this replaces gave it (`Secret` has no `Default`): what a test
/// spells a partial set from.
impl Default for PolymarketConfig {
    fn default() -> Self {
        let blank = || Secret::new(String::new());
        PolymarketConfig {
            private_key: blank(),
            address: String::new(),
            api_key: blank(),
            secret: blank(),
            passphrase: blank(),
            relayer_key: blank(),
            relayer_address: String::new(),
        }
    }
}

/// Read Polymarket creds from the started ACCOUNT's fields. `None` when the L1 private key
/// (`PRIVATE_KEY`) is unset or blank — the live gate. The L2 trio may be blank (then derived at
/// connect via EIP-712). The funder/deposit-wallet address is the account's `ADDRESS`, else its
/// `FUNDER`.
///
/// One read per field, and no rung: the account's fields carry no tier (the account HAS one, and
/// this venue starts only `live`), so there is neither a tier-suffixed spelling nor a tier-less one
/// to fall back between, and nothing here can read another account's key.
///
/// ⚠ This venue has NO testnet: every key this reads signs REAL MONEY on Polygon mainnet
/// (`crates/bridges/polymarket/CLAUDE.md`). The `POLY_EXEC`/`POLY_RECONCILE` gates are
/// deliberately NOT per-account — they are deployment-wide opt-ins, and a second account arms only
/// when the venue itself is already armed.
pub fn load_polymarket_credentials_from(
    creds: &AccountCredentials<'_>,
) -> Option<PolymarketConfig> {
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
    let secret = |field: &str| Secret::new(get(field));
    Some(PolymarketConfig {
        private_key: Secret::new(private_key),
        address,
        api_key: secret(API_KEY),
        secret: secret(SECRET),
        passphrase: secret(PASSPHRASE),
        relayer_key: secret(RELAYER_API_KEY),
        relayer_address: get(RELAYER_API_KEY_ADDRESS),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_secrets::account_credentials::CredentialFields;

    /// The account holding `fields`, as a start is handed it.
    fn holding(fields: &CredentialFields) -> AccountCredentials<'_> {
        AccountCredentials::new(Some(1), Some(fields), None)
    }

    #[test]
    fn credentials_gate_and_redaction() {
        assert!(load_polymarket_credentials_from(&AccountCredentials::none()).is_none());
        let no_key = CredentialFields::from_pairs([(ADDRESS, "0xabc"), (SECRET, "s3cr3t")]);
        assert!(load_polymarket_credentials_from(&holding(&no_key)).is_none(), "no key, no creds");
        let blank_key = CredentialFields::from_pairs([(PRIVATE_KEY, "  ")]);
        assert!(
            load_polymarket_credentials_from(&holding(&blank_key)).is_none(),
            "blank is absent"
        );
        let fields = CredentialFields::from_pairs([
            (PRIVATE_KEY, "0xdeadbeef"),
            (ADDRESS, "0xabc"),
            (SECRET, "s3cr3t"),
        ]);
        let c = load_polymarket_credentials_from(&holding(&fields)).unwrap();
        assert_eq!(c.address, "0xabc");
        let dbg = format!("{c:?}");
        assert!(!dbg.contains("deadbeef") && !dbg.contains("s3cr3t"));
    }

    /// **The fields the loader reads are `vike_venues::credential_schema`'s polymarket row, word for
    /// word**: every account field of the row lands in its slot (`SIGNATURE_TYPE` is the recon
    /// client's — `crate::reconcile`'s `signature_type_for_account` — and `FUNDER`
    /// stands in for an absent `ADDRESS`), the private key is the one required field, and the
    /// venue holds no application field.
    #[test]
    fn the_fields_are_the_key_schemas() {
        use std::collections::BTreeSet;
        use vike_venues::credential_schema::{KeyFieldOwner, schema_for};
        let schema = schema_for(crate::feed::VENUE).expect("a polymarket schema row");
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
        assert_eq!(schema.required(KeyFieldOwner::Account), [PRIVATE_KEY]);

        let every: Vec<(&str, String)> =
            theirs.iter().filter(|f| **f != FUNDER).map(|f| (*f, format!("v-{f}"))).collect();
        let fields = CredentialFields::from_pairs(every);
        let c = load_polymarket_credentials_from(&holding(&fields)).expect("the key is present");
        assert_eq!(
            [
                c.private_key.expose().as_str(),
                &c.address,
                c.api_key.expose(),
                c.secret.expose(),
                c.passphrase.expose(),
                c.relayer_key.expose(),
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

    /// **Every host is its `venue_url` seed row, at type `live`, plus the path the code appends** —
    /// the dialled address is what it was before the hosts moved into rows. A venue with no rows
    /// refuses every lookup.
    #[test]
    fn every_host_is_its_live_seed_row_plus_the_path_in_code() {
        let seed = VenueUrls::seed("polymarket");
        let row = |channel: UrlChannel, protocol: UrlProtocol, product: &str| {
            vike_venues::venue_urls::seed_url(
                "polymarket",
                UrlType::Live,
                channel,
                protocol,
                product,
            )
            .expect("the seed holds the row")
        };
        let (trade, user, data) = (UrlChannel::Trade, UrlChannel::User, UrlChannel::Data);
        let (rest, ws) = (UrlProtocol::Rest, UrlProtocol::Websocket);
        assert_eq!(clob_base(&seed), Ok(row(trade, rest, "")));
        assert_eq!(gamma_base(&seed), Ok(row(data, rest, "gamma")));
        assert_eq!(rtds_url(&seed), Ok(row(data, ws, "live-data")));
        assert_eq!(market_websocket_url(&seed), Ok(format!("{}/ws/market", row(user, ws, ""))));
        #[cfg(feature = "polymarket")]
        {
            assert_eq!(data_api_base(&seed), Ok(row(data, rest, "data")));
            assert_eq!(user_websocket_url(&seed), Ok(format!("{}/ws/user", row(user, ws, ""))));
            assert_eq!(relayer_base(&seed), Ok(row(trade, rest, "relayer")));
            assert_eq!(rpc_url(&seed), Ok(row(data, rest, "rpc")));
            assert_eq!(geoblock_base(&seed), Ok(row(UrlChannel::Login, rest, "geoblock")));
        }
        assert!(clob_base(&VenueUrls::default()).is_err(), "no rows, no address");
    }

    /// **The tape's `trade` and `user` rows are dialled with NO account**: [`clob_base`] (the keyless
    /// books) and [`market_websocket_url`] (the market socket) read them whether or not an account runs, so
    /// `crates/vike-venues/src/venue_urls.rs`'s `KEYLESS_FEED_DIALS` names them and the gatewayhub's
    /// address probe never reports them not in use (decision 0265's verdict 8). A row only the exec
    /// plane dials is not on that list.
    #[test]
    fn the_tapes_trade_and_user_rows_need_no_account() {
        use vike_venues::venue_urls::needs_no_account;
        let (trade, user, rest) = (UrlChannel::Trade, UrlChannel::User, UrlProtocol::Rest);
        assert!(needs_no_account("polymarket", trade, rest, ""), "clob_base");
        assert!(needs_no_account("polymarket", user, UrlProtocol::Websocket, ""), "market_ws_url");
        assert!(!needs_no_account("polymarket", trade, rest, "relayer"), "relayer_base: exec only");
        assert!(!needs_no_account("polymarket", UrlChannel::Login, rest, "geoblock"), "exec only");
    }

    /// `FUNDER` stands in for an absent (or blank) `ADDRESS`, and never overrides a present one.
    #[test]
    fn funder_is_the_address_fallback() {
        let funder = CredentialFields::from_pairs([(PRIVATE_KEY, "0xk"), (FUNDER, "0xfunder")]);
        assert_eq!(
            load_polymarket_credentials_from(&holding(&funder)).unwrap().address,
            "0xfunder"
        );
        let blank = CredentialFields::from_pairs([
            (PRIVATE_KEY, "0xk"),
            (ADDRESS, " "),
            (FUNDER, "0xfunder"),
        ]);
        assert_eq!(load_polymarket_credentials_from(&holding(&blank)).unwrap().address, "0xfunder");
        let both = CredentialFields::from_pairs([
            (PRIVATE_KEY, "0xk"),
            (ADDRESS, "0xaddress"),
            (FUNDER, "0xfunder"),
        ]);
        assert_eq!(load_polymarket_credentials_from(&holding(&both)).unwrap().address, "0xaddress");
        let bare = CredentialFields::from_pairs([(PRIVATE_KEY, "0xk")]);
        assert_eq!(load_polymarket_credentials_from(&holding(&bare)).unwrap().address, "");
    }
}
