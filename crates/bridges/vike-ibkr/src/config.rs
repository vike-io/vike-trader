//! `IbkrConfig` — the resolved connection parameters + the paper/live gate. Resolved from the
//! STARTED ACCOUNT's credential FIELDS (`vike_secrets::account_credentials::AccountCredentials`;
//! the binary owns the store read) — `ACCOUNT`, `CLIENT_ID`, `DATA_CLIENT_ID`, `RECON_CLIENT_ID`,
//! `MKTDATA_TYPE`, the key schema's ibkr row (`vike_venues::credential_schema`) — and, for the
//! GATEWAY, one per machine and tier (ruling 10): its ADDRESSES, the venue's `venue_url` rows
//! ([`gateway_addresses`]: `ibkr <type> trade socket gateway` for the socket API, `host:port`, and
//! `ibkr <type> trade rest cpapi` for the Client Portal base; decision 0265, which retired the
//! `venue.ibkr.<tier>.{host,port,cpapi_url}` settings), and its `venue_setting` rows
//! `venue.ibkr.<tier>.backend` and the order switch `venue.ibkr.<tier>.extended_hours`
//! ([`IbkrConfig::extended_hours`]).
//! NO STORED CREDENTIAL ([`account_is_stored`]; or unknown backend, or a refused address) → `None` →
//! the account does NOT RUN. IBKR's socket API has no in-crate auth — the Gateway holds the login — so
//! the only "credential" here is which account to trade, on which gateway.
//!
//! **On the socket backend the account number is LEARNED, not typed.** The gateway tells every API
//! client its login's accounts at connect (`managedAccounts`), so an account that holds the gateway's
//! login pair ([`GATEWAY_LOGIN_FIELDS`]) and no `ACCOUNT` loads with an EMPTY [`IbkrConfig::account`],
//! and the exec connect takes the login's ONE account ([`account_from_gateway`]); a stored `ACCOUNT`
//! must be one of the login's. The Client Portal backend still needs the field: its order paths carry
//! the number before anything is asked. ⚠ So the missing field is no longer a live gate: a `live`
//! account holding only its login pair, which did NOT RUN for want of `ACCOUNT`, STARTS the live
//! engine once its `account` row is active (`account.active` is the switch, as the root `CLAUDE.md`
//! says it should be).
//!
//! **A credential belongs to its ACCOUNT, and the account has ONE tier** (decision 0156 item 6).
//! The loader reads the fields of the one account it is handed and never composes a name; the tier
//! it is handed is that account's, and picks the gateway rows and addresses and nothing else. The
//! labelled twin `load_ibkr_config_for_account` and the per-tier name composers went with the names
//! they composed: a labelled account IS its credentials, so [`load_ibkr_config_from`] is the one
//! loader.

use vike_model::AccountTier;
use vike_secrets::account_credentials::AccountCredentials;
use vike_secrets::venue_setting::{SettingTier, VenueSettings};
use vike_venues::IbkrBackend;
use vike_venues::credential_schema::{KeyFieldOwner, VenueKeySchema, schema_for};
use vike_venues::venue_fields::{BACKEND, EXTENDED_HOURS, VenueFieldName};
use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlSocket, UrlType};

use crate::refusals::IbkrConfigRefusal;

/// IBKR market-data type (`reqMarketDataType`): delayed-by-default so the feed works with no paid
/// subscriptions. `to_ibapi` maps to the `ibapi` 3.2.1 enum at the connect boundary (behind the
/// `ibkr-socket` feature, where ibapi exists).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MarketDataType {
    Realtime,
    Frozen,
    #[default]
    Delayed,
    DelayedFrozen,
}

impl MarketDataType {
    fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "delayed" => Some(MarketDataType::Delayed),
            "realtime" | "live" => Some(MarketDataType::Realtime),
            "frozen" => Some(MarketDataType::Frozen),
            "delayed-frozen" | "delayed_frozen" => Some(MarketDataType::DelayedFrozen),
            _ => None,
        }
    }
    #[cfg(feature = "ibkr-socket")]
    pub fn to_ibapi(self) -> ibapi::market_data::MarketDataType {
        use ibapi::market_data::MarketDataType as M;
        match self {
            MarketDataType::Realtime => M::Realtime,
            MarketDataType::Frozen => M::Frozen,
            MarketDataType::Delayed => M::Delayed,
            MarketDataType::DelayedFrozen => M::DelayedFrozen,
        }
    }
}

/// Resolved IBKR connection config. `account` is the paper `DU…` or live `U…` account number.
#[derive(Clone, Debug)]
pub struct IbkrConfig {
    /// The tier the config was loaded at — the started account's own (`AdapterInputs::tier`): it
    /// chose the gateway rows and the default port.
    pub tier: AccountTier,
    pub backend: IbkrBackend,
    pub host: String,
    pub port: u16,
    pub client_id: i32,
    /// The account's `ACCOUNT` field, or EMPTY when it stores none (socket only, [`account_is_stored`]):
    /// the exec connect then fills it from the gateway's login ([`account_from_gateway`]) before anything
    /// else of the start reads it.
    pub account: String,
    /// Client Portal Web API base URL (cpapi backend): the tier's `ibkr <type> trade rest cpapi`
    /// row ([`gateway_addresses`]). Inert for the socket backend.
    pub cpapi_url: String,
    /// `reqMarketDataType` selector for the Phase-3 realtime feed (delayed by default).
    pub market_data_type: MarketDataType,
    /// Client id for the DEDICATED market-data connection (distinct from `client_id`, the exec
    /// connection's).
    pub data_client_id: i32,
    /// Client id for the socket backend's DEDICATED reconcile connection
    /// (`crate::reconcile::socket::ReconcileClientIbkrSocket`), on the SAME gateway as the exec: one more API
    /// client id on one login, never a second session (decision 0140). Defaults to `client_id + 2`,
    /// so it differs from the exec's and from the defaulted `data_client_id`. Inert for cpapi, whose
    /// reconcile client speaks REST to the CP Gateway and has no client id.
    ///
    /// ⚠ **Every client id on one gateway must be unique across ALL accounts started on it** —
    /// exec (`client_id`), data (`data_client_id`) and reconcile (`reconcile_client_id`) of every
    /// labelled account alike: IB refuses a second connection under an id in use. The defaults
    /// (`client_id`, `+1`, `+2`) collide for two accounts left at `client_id = 1`, and a set
    /// `DATA_CLIENT_ID` can land on a neighbour's recon id; only the exec/data/recon clash WITHIN
    /// one config is refused (`crate::reconcile::socket::require_distinct_reconcile_id`). The rest is the
    /// operator's to write.
    pub reconcile_client_id: i32,
    /// The tier's `venue.ibkr.<tier>.extended_hours` switch, OFF unless the row says `1`: the socket
    /// transport then sends every US-stock LIMIT order with IBKR's `outsideRth` and `includeOvernight`
    /// flags (`crate::transport::IbkrTransportSocket`), so it is eligible pre-market, after-hours and in the
    /// overnight session. Market and stop orders never carry them. Only the socket backend honours it:
    /// the cpapi backend cannot, and `crate::connect_execution_client` refuses the start.
    pub extended_hours: bool,
}

/// **The tier's gateway ADDRESSES**: the socket API's `host:port`, the `ibkr <type> trade socket
/// gateway` row, and the Client Portal base, the `ibkr <type> trade rest cpapi` row (the release
/// seeds `127.0.0.1:4101` live / `127.0.0.1:4102` demo, what the box's IB Gateways listen on, and
/// `https://127.0.0.1:5000` for both). `paper` reads as `demo` does. Both are asked whatever the
/// backend, as both were always filled. The socket row is split by the rule its write is judged by
/// (`UrlSocket::parse`).
///
/// # Errors
/// The refusal naming the row that is missing, is the live one on a demo tier, or (the socket row) is
/// not `host:port`: the account does NOT RUN.
pub fn gateway_addresses(
    tier: AccountTier,
    settings: &VenueSettings,
) -> Result<(UrlSocket, &str), IbkrConfigRefusal> {
    let ty = UrlType::of_tier(tier);
    let url = |protocol, product| {
        settings
            .urls()
            .url(ty, UrlChannel::Trade, protocol, product)
            .map_err(|refusal| IbkrConfigRefusal::AddressRow { refusal })
    };
    let socket = url(UrlProtocol::Socket, "gateway")?;
    let socket = UrlSocket::parse(socket).map_err(|_| IbkrConfigRefusal::SocketRowNotHostPort {
        tier: ty.to_string(),
        socket: socket.to_string(),
    })?;
    Ok((socket, url(UrlProtocol::Rest, "cpapi")?))
}

impl IbkrConfig {
    /// **`native_time_release`: whether this account's bridge has the VENUE hold a time release** (decision 0267's
    /// 2.6, the coordinator's ruling: decided per account by what the bridge can do, never by the venue row alone).
    /// The socket backend sends a time-only release as IB's `goodAfterTime` and IB holds it (`crate::execution::order`'s module
    /// doc); `oauth` dials the socket too (`crate::connect_execution_client`). The Client Portal ticket has no such
    /// field, so there the core keeps holding every release, as it always did, and one that reaches the bridge anyway
    /// is refused by name (`transport/cpapi/mod.rs`'s `order_json`). An account with [`IbkrConfig::extended_hours`]
    /// on is `false` too: IB refuses `goodAfterTime` on the extended-hours stock limit the socket sends there, so the
    /// core keeps holding the release and sends a plain order at its time, as it always did on that account. The
    /// gatewayhub publishes it per account beside the start's other facts, from the config it resolves for the account.
    #[must_use]
    pub fn native_time_release(&self) -> bool {
        matches!(self.backend, IbkrBackend::Socket | IbkrBackend::Oauth) && !self.extended_hours
    }
}

/// The key schema's ibkr row: `vike_venues::credential_schema` states every FIELD this crate reads,
/// so the GUI form, the status grid and this loader cannot spell one differently.
fn key_schema() -> &'static VenueKeySchema {
    schema_for("ibkr").expect("the credential key schema carries an ibkr row")
}

/// The account's TRIMMED value of ONE field (blank reads as absent). `word` must be an ACCOUNT
/// field of the key schema's ibkr row: a word the row does not carry is a programming error here,
/// not an absent value, so this loader and the schema cannot drift apart. [`required_fields`]
/// reads the same row.
fn account_field<'a>(creds: &AccountCredentials<'a>, word: &str) -> Option<&'a str> {
    let known = key_schema().field(word).is_some_and(|f| f.owner == KeyFieldOwner::Account);
    assert!(known, "the ibkr key schema has no account field {word}");
    creds.get(word)
}

/// The fields a complete ibkr credential set needs — the key schema's REQUIRED account fields,
/// `ACCOUNT` alone; on the socket backend the gateway's login pair stands in for it
/// ([`account_is_stored`]). The optional `CLIENT_ID`, `DATA_CLIENT_ID`, `RECON_CLIENT_ID` and
/// `MKTDATA_TYPE` belong to the account too, so an account that holds them and neither is a
/// HALF-WRITTEN set, which the start names by its missing fields
/// (`vike_secrets::account_credentials::AccountCredentials::missing`). The gateway is the tier's
/// SETTINGS, not credentials, so it is not here.
#[must_use]
pub fn required_fields() -> Vec<&'static str> {
    key_schema().required(KeyFieldOwner::Account)
}

/// **The IB Gateway's login pair as an account row holds it** — what the launcher logs the gateway in
/// with (`vike-cli secrets ibc-start`). This crate reads its PRESENCE only, never a value, so it is not
/// the key schema's (whose rows are the fields a loader reads the VALUE of): an account holding it is
/// the gateway's own login, so the socket connect can ask the gateway which account it is.
pub const GATEWAY_LOGIN_FIELDS: [&str; 2] = ["USERNAME", "PASSWORD"];

/// **Whether the account holds its ibkr credential** — the live gate, judged before the gateway rows
/// are (so a gateway row nobody can parse refuses a STORED account by name, never as "the store holds
/// nothing"): its `ACCOUNT`, or, on any backend but `cpapi` (whose order paths need the number up
/// front), the gateway's login pair ([`GATEWAY_LOGIN_FIELDS`]), whose account is learned at connect
/// ([`account_from_gateway`]). An account that holds neither does not run.
#[must_use]
pub fn account_is_stored(
    tier: AccountTier,
    creds: &AccountCredentials<'_>,
    settings: &VenueSettings,
) -> bool {
    let backend = IbkrBackend::parse(tier_setting(tier, settings, &BACKEND).unwrap_or("socket"));
    account_field(creds, "ACCOUNT").is_some()
        || (creds.missing(&GATEWAY_LOGIN_FIELDS).is_empty() && backend != Some(IbkrBackend::Cpapi))
}

/// **The account a socket session trades, out of the accounts its login named at the handshake**
/// (`managedAccounts`): a stored `ACCOUNT` (`stored`, blank when none) must be one of them, the
/// wrong-account guard; with none stored, the login's ONE account. A login that manages none or
/// several (an advisor login) is refused rather than guessed from. The refusal names how many
/// accounts the login manages, never their numbers.
///
/// # Errors
/// The refusal: the account does NOT RUN.
pub fn account_from_gateway(stored: &str, managed: &[String]) -> Result<String, IbkrConfigRefusal> {
    let held: Vec<&str> = managed.iter().map(|m| m.trim()).filter(|m| !m.is_empty()).collect();
    match (stored.trim(), held.as_slice()) {
        ("", [only]) => Ok((*only).to_string()),
        ("", held) => Err(IbkrConfigRefusal::NoAccountStored { count: held.len() }),
        (stored, held) if held.contains(&stored) => Ok(stored.to_string()),
        (_, held) => Err(IbkrConfigRefusal::AccountNotManaged { count: held.len() }),
    }
}

/// The tier's `venue.ibkr.<tier>.<field>` row: the gateway is a MACHINE fact, one per tier (ruling
/// 10), never a credential and never per account (decision 0095; the credential-map fold that
/// carried them is retired). EXACTLY the tier's row — a machine-scoped (`any`) row of a tier-scoped
/// field is not read, as `vike-cli config show` labels it — trimmed, and a blank row reads as no row,
/// as the credential-map read did (`config set` and the move store a value as written).
fn tier_setting<'s>(
    tier: AccountTier,
    settings: &'s VenueSettings,
    field: &VenueFieldName,
) -> Option<&'s str> {
    settings.get_exact(SettingTier::Tier(tier), field).map(str::trim).filter(|v| !v.is_empty())
}

/// The exec client id of an account whose `CLIENT_ID` field is unset.
pub const DEFAULT_CLIENT_ID: i32 = 1;
/// How far past the exec client id an unset `DATA_CLIENT_ID` sits: distinct from the exec's.
pub const DATA_CLIENT_ID_OFFSET: i32 = 1;
/// How far past the exec client id an unset `RECON_CLIENT_ID` sits: distinct from the exec's and
/// from the defaulted data id.
pub const RECONCILE_CLIENT_ID_OFFSET: i32 = 2;

/// Resolve config from ONE account's credential fields and the venue's settings, at `tier` — the
/// account's own tier. `None` when the account holds no credential ([`account_is_stored`]: the live
/// gate), the backend string is unrecognized, a gateway address is refused ([`gateway_addresses`];
/// the start logs why) or the socket row is not `host:port`, or a client id does not parse. An
/// account that stores no `ACCOUNT` loads with an empty [`IbkrConfig::account`].
///
/// ⚠ **Nothing is borrowed from another account**, for either half of what makes an IBKR start
/// distinct. `ACCOUNT` selects the `DU…`/`U…` account every order is placed in, so borrowing it
/// would trade another account; and `CLIENT_ID`/`DATA_CLIENT_ID`/`RECON_CLIENT_ID` must DIFFER
/// between two live TWS connections — a second account silently inheriting the first's ids would
/// have its socket evicted by the gateway rather than starting beside it. Both are the operator's
/// to write, on each account; `creds` is the one account's fields, so there is no other to fall
/// back to.
///
/// ⚠ **The GATEWAY is the tier's, never the account's** — its addresses from `settings.urls()`
/// ([`gateway_addresses`]) and `venue.ibkr.<tier>.backend` from `settings`, so every account of a
/// tier dials the gateway that tier's rows name. That is ruling 10 (one gateway per machine and
/// tier); until decision 0095's Task 7 retired the credential-map fold, each account read its own
/// gateway credentials, and a credential row under one of those legacy names is refused at boot
/// now. A gateway word held as an account FIELD is read by nothing.
pub fn load_ibkr_config_from(
    tier: AccountTier,
    creds: &AccountCredentials<'_>,
    settings: &VenueSettings,
) -> Option<IbkrConfig> {
    if !account_is_stored(tier, creds, settings) {
        return None;
    }
    let field = |word: &str| account_field(creds, word);
    let account = field("ACCOUNT").unwrap_or_default().to_string();
    let setting = |field: &VenueFieldName| tier_setting(tier, settings, field);
    let backend = IbkrBackend::parse(setting(&BACKEND).unwrap_or("socket"))?;
    let (UrlSocket { host, port }, cpapi_url) = gateway_addresses(tier, settings).ok()?;
    let client_id = match field("CLIENT_ID") {
        Some(c) => c.parse().ok()?,
        None => DEFAULT_CLIENT_ID,
    };
    let cpapi_url = cpapi_url.to_string();
    let market_data_type = MarketDataType::parse(field("MKTDATA_TYPE").unwrap_or("delayed"))?;
    let data_client_id = match field("DATA_CLIENT_ID") {
        Some(c) => c.parse().ok()?,
        None => client_id + DATA_CLIENT_ID_OFFSET,
    };
    let reconcile_client_id = match field("RECON_CLIENT_ID") {
        Some(c) => c.parse().ok()?,
        None => client_id + RECONCILE_CLIENT_ID_OFFSET,
    };
    // The tier's order switch, an exact-`1` row (`venue.ibkr.<tier>.extended_hours`): off unless it says
    // `1`. A word `config set` cannot write refuses the config like an unparseable port, never a guess.
    let extended_hours = match setting(&EXTENDED_HOURS) {
        None | Some("0") => false,
        Some("1") => true,
        Some(_) => return None,
    };
    Some(IbkrConfig {
        tier,
        backend,
        host,
        port,
        client_id,
        account,
        cpapi_url,
        market_data_type,
        data_client_id,
        reconcile_client_id,
        extended_hours,
    })
}

// There is deliberately NO `load_ibkr_config(env)` convenience here any more. It was a two-line
// wrapper that opened the workspace credential store itself and fed this function — a LIBRARY
// reading global configuration state its caller can neither see nor substitute, which is the class
// `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `CREDENTIAL_STORE_PIN` ratchets down. Every caller
// was a binary or a test, so each now reads the store and passes the started account's fields —
// and, since decision 0095's Task 7, the venue's `venue_setting` rows beside them
// (`vike_secrets::venue_setting::load_venue_settings` over the same settings directory). (This
// crate re-exported that loader for two start binaries with no vike-bridge-core edge; both are
// deleted, and so is the re-export — `lib.rs` says so.)

#[cfg(test)]
mod tests {
    use super::*;
    use vike_secrets::account_credentials::CredentialFields;
    use vike_venues::venue_urls::{UrlRefusal, VenueUrls};

    /// Load at `tier` from ONE account holding `fields` (`(FIELD, value)` pairs).
    fn load(
        tier: AccountTier,
        fields: &[(&str, &str)],
        settings: &VenueSettings,
    ) -> Option<IbkrConfig> {
        let fields = CredentialFields::from_pairs(fields.iter().copied());
        let creds = AccountCredentials::new(Some(1), Some(&fields), None);
        load_ibkr_config_from(tier, &creds, settings)
    }

    /// The venue's `venue.ibkr.<tier>.<field>` rows — the shape the composition root reads out of
    /// the settings database (decision 0095) — beside the release's addresses.
    fn settings(tier: &str, rows: &[(&str, &str)]) -> VenueSettings {
        vike_bridge_core::venue_adapter_fixture::venue_settings("ibkr", tier, rows)
            .with_urls(VenueUrls::seed("ibkr"))
    }

    /// No setting row, the release's addresses: what a fresh box hands the bridge.
    fn seeded() -> VenueSettings {
        VenueSettings::default().with_urls(VenueUrls::seed("ibkr"))
    }

    /// No setting row and no ibkr address: every address lookup is refused, naming the venue.
    fn none() -> VenueSettings {
        VenueSettings::default().with_urls(VenueUrls::from_rows("ibkr", std::iter::empty()))
    }

    /// The seed's url of an ibkr `trade` row — what a fresh box's table holds.
    fn seed_url(ty: UrlType, protocol: UrlProtocol, product: &str) -> &'static str {
        vike_venues::venue_urls::seed_url("ibkr", ty, UrlChannel::Trade, protocol, product)
            .expect("a seed row")
    }

    /// The demo tier's two addresses as an operator's `venue_url` rows, and the live tier's seed.
    fn demo_at(socket: &'static str, cpapi: &'static str) -> VenueSettings {
        let (sock, rest) = (UrlProtocol::Socket, UrlProtocol::Rest);
        let rows = [
            (UrlType::Live, sock, "gateway", seed_url(UrlType::Live, sock, "gateway")),
            (UrlType::Live, rest, "cpapi", seed_url(UrlType::Live, rest, "cpapi")),
            (UrlType::Demo, sock, "gateway", socket),
            (UrlType::Demo, rest, "cpapi", cpapi),
        ];
        let rows =
            rows.map(|(ty, p, product, url)| ("ibkr", ty, UrlChannel::Trade, p, product, url));
        VenueSettings::default().with_urls(VenueUrls::from_rows("ibkr", rows))
    }

    const PAPER_ACCOUNT: (&str, &str) = ("ACCOUNT", "DUQ186573");

    /// The owner's ruling (decision 0265): the demo gateway is the seed's `127.0.0.1:4102`, the
    /// port the box's paper IB Gateway listens on (the code said the TWS port 7497 before).
    #[test]
    fn demo_dials_the_seeded_demo_gateway() {
        let config = load(AccountTier::Demo, &[("CLIENT_ID", "7"), PAPER_ACCOUNT], &seeded())
            .expect("demo config");
        assert_eq!(config.tier, AccountTier::Demo);
        assert_eq!(config.backend, IbkrBackend::Socket);
        assert_eq!(
            format!("{}:{}", config.host, config.port),
            seed_url(UrlType::Demo, UrlProtocol::Socket, "gateway")
        );
        assert_eq!((config.host.as_str(), config.port), ("127.0.0.1", 4102));
        assert_eq!(config.client_id, 7);
        assert_eq!(config.account, "DUQ186573");
    }

    /// **`native_time_release` is the backend's**: the socket (and oauth, which dials it) sends a time release as IB's
    /// `goodAfterTime`; the Client Portal has no field for one, so the core keeps holding it there.
    #[test]
    fn a_time_release_is_held_at_the_venue_on_the_socket_only() {
        for (backend, native) in [("socket", true), ("oauth", true), ("cpapi", false)] {
            let s = settings("demo", &[("backend", backend)]);
            let config = load(AccountTier::Demo, &[PAPER_ACCOUNT], &s).unwrap();
            assert_eq!(config.native_time_release(), native, "{backend}");
        }
        // No backend row, the release's addresses: the socket.
        let default = load(AccountTier::Demo, &[PAPER_ACCOUNT], &seeded()).unwrap();
        assert!(default.native_time_release(), "no backend row is the socket");
        // Extended hours on: the core holds, as before the attributes wave (IB refuses GAT on that order).
        let ext = settings("demo", &[("backend", "socket"), ("extended_hours", "1")]);
        let config = load(AccountTier::Demo, &[PAPER_ACCOUNT], &ext).unwrap();
        assert!(config.extended_hours && !config.native_time_release());
    }

    /// …and the live gateway is the seed's `127.0.0.1:4101` (the code said TWS's 7496).
    #[test]
    fn live_dials_the_seeded_live_gateway() {
        let config =
            load(AccountTier::Live, &[("ACCOUNT", "U13112916")], &seeded()).expect("live config");
        assert_eq!(config.tier, AccountTier::Live);
        assert_eq!(
            format!("{}:{}", config.host, config.port),
            seed_url(UrlType::Live, UrlProtocol::Socket, "gateway")
        );
        assert_eq!(config.port, 4101);
        assert_eq!(config.client_id, 1); // default client id
    }

    /// Decision 0265: the gateway's address is the TIER's `venue_url` rows and its backend the
    /// tier's `venue_setting` row, while the account stays a credential.
    #[test]
    fn the_gateway_comes_from_the_tiers_rows() {
        let urls = demo_at("<host>:4002", "https://127.0.0.1:5000").urls().clone();
        let s = vike_bridge_core::venue_adapter_fixture::venue_settings(
            "ibkr",
            "demo",
            &[("backend", "cpapi")],
        )
        .with_urls(urls);
        let config = load(AccountTier::Demo, &[PAPER_ACCOUNT], &s).unwrap();
        assert_eq!(config.host, "<host>");
        assert_eq!(config.port, 4002);
        assert_eq!(config.backend, IbkrBackend::Cpapi);
        // …and another tier's rows are not this tier's.
        assert_eq!(load(AccountTier::Live, &[PAPER_ACCOUNT], &s).unwrap().port, 4101);
    }

    /// **A retired gateway row is read by nothing** (decision 0265: no migration, a retired name
    /// just stops being read). A box that still holds `venue.ibkr.<tier>.{host,port,cpapi_url}`
    /// rows, machine-scoped or tier-scoped, dials the `venue_url` addresses.
    #[test]
    fn a_retired_gateway_row_is_not_read() {
        let row = |tier, field: &str, value: &str| vike_secrets::VenueSettingRow {
            venue: "ibkr".to_string(),
            tier,
            field: field.to_string(),
            value: value.to_string(),
        };
        let retired = VenueSettings::from_rows(
            "ibkr",
            &[
                row(None, "PORT", "4002"),
                row(Some(AccountTier::Demo), "HOST", "<host>"),
                row(Some(AccountTier::Demo), "PORT", "4002"),
                row(Some(AccountTier::Demo), "CPAPI_URL", "https://127.0.0.1:5555"),
            ],
        )
        .with_urls(VenueUrls::seed("ibkr"));
        let config = load(AccountTier::Demo, &[PAPER_ACCOUNT], &retired).unwrap();
        assert_eq!((config.host.as_str(), config.port), ("127.0.0.1", 4102));
        assert_eq!(config.cpapi_url, seed_url(UrlType::Demo, UrlProtocol::Rest, "cpapi"));
    }

    /// No address row is no gateway: the loader refuses, and [`gateway_addresses`] names the row.
    #[test]
    fn a_refused_address_is_none_and_names_the_row() {
        assert!(load(AccountTier::Demo, &[PAPER_ACCOUNT], &none()).is_none());
        let why = gateway_addresses(AccountTier::Demo, &none()).unwrap_err();
        assert!(
            matches!(
                &why,
                IbkrConfigRefusal::AddressRow { refusal: UrlRefusal::Missing { ty: UrlType::Demo, product, .. } }
                    if product == "gateway"
            ),
            "{why}"
        );
        // A demo socket that IS the live one is refused by the guard, not dialled.
        let live = seed_url(UrlType::Live, UrlProtocol::Socket, "gateway");
        let s = demo_at(live, "https://127.0.0.1:5000");
        assert!(load(AccountTier::Demo, &[PAPER_ACCOUNT], &s).is_none());
        let why = gateway_addresses(AccountTier::Demo, &s).unwrap_err();
        assert!(
            matches!(why, IbkrConfigRefusal::AddressRow { refusal: UrlRefusal::DemoIsLive { .. } }),
            "{why}"
        );
        // A socket row that is not `host:port` is named like a missing one (drift U-D2).
        let s = demo_at("127.0.0.1", "https://127.0.0.1:5000");
        let why = gateway_addresses(AccountTier::Demo, &s).unwrap_err();
        assert_eq!(
            why,
            IbkrConfigRefusal::SocketRowNotHostPort {
                tier: "demo".into(),
                socket: "127.0.0.1".into()
            }
        );
    }

    /// A gateway word held as one of the ACCOUNT's credential fields is read by nothing: the loader
    /// reads the gateway from the tier's rows only, and never asks the account for it.
    #[test]
    fn a_gateway_field_held_by_the_account_is_ignored() {
        let config = load(
            AccountTier::Demo,
            &[PAPER_ACCOUNT, ("HOST", "<host>"), ("PORT", "4002"), ("BACKEND", "grpc")],
            &seeded(),
        )
        .expect("defaults apply");
        assert_eq!((config.host.as_str(), config.port), ("127.0.0.1", 4102));
        assert_eq!(config.backend, IbkrBackend::Socket);
    }

    /// Two accounts of one tier dial the tier's ONE gateway — the gateway is one per machine and
    /// tier (ruling 10) — while each account's number and client ids stay its own.
    #[test]
    fn two_accounts_of_one_tier_dial_the_tier_gateway_with_their_own_ids() {
        let s = demo_at("<host>:4002", "https://127.0.0.1:5000");
        let first = load(AccountTier::Demo, &[PAPER_ACCOUNT], &s).unwrap();
        let second =
            load(AccountTier::Demo, &[("ACCOUNT", "DU0000001"), ("CLIENT_ID", "11")], &s).unwrap();
        for config in [&first, &second] {
            assert_eq!((config.host.as_str(), config.port), ("<host>", 4002));
        }
        assert_eq!((first.account.as_str(), first.client_id), ("DUQ186573", 1));
        assert_eq!((second.account.as_str(), second.client_id), ("DU0000001", 11));
    }

    /// **The fields are the key schema's, word for word.** The ibkr row's account fields are the
    /// five this loader reads, in its order; the venue holds none; `ACCOUNT` alone is required
    /// ([`required_fields`]); and each field planted is READ — a complete set moves every slot off
    /// its default.
    #[test]
    fn the_fields_are_the_key_schemas() {
        let schema = key_schema();
        let words: Vec<&str> = schema.account_fields().map(|f| f.field).collect();
        assert_eq!(
            words,
            ["ACCOUNT", "CLIENT_ID", "DATA_CLIENT_ID", "RECON_CLIENT_ID", "MKTDATA_TYPE"]
        );
        assert!(schema.venue_fields().next().is_none(), "ibkr has no application credential");
        assert_eq!(required_fields(), vec!["ACCOUNT"]);
        let config = load(
            AccountTier::Demo,
            &[
                ("ACCOUNT", "DU1"),
                ("CLIENT_ID", "5"),
                ("DATA_CLIENT_ID", "11"),
                ("RECON_CLIENT_ID", "12"),
                ("MKTDATA_TYPE", "realtime"),
            ],
            &seeded(),
        )
        .expect("a complete set");
        assert_eq!(
            (
                config.account.as_str(),
                config.client_id,
                config.data_client_id,
                config.reconcile_client_id,
                config.market_data_type
            ),
            ("DU1", 5, 11, 12, MarketDataType::Realtime)
        );
    }

    /// A word the key schema's ibkr row does not carry as an ACCOUNT field is a programming error
    /// in this loader, never a quiet absence.
    #[test]
    #[should_panic(expected = "the ibkr key schema has no account field HOST")]
    fn a_field_the_schema_does_not_carry_is_refused_loudly() {
        let _ = account_field(&AccountCredentials::none(), "HOST");
    }

    /// No `ACCOUNT` — nothing stored, a blank value, or a half-written set holding only the
    /// optional fields — is the paper gate.
    #[test]
    fn absent_account_is_the_paper_gate() {
        assert!(load(AccountTier::Demo, &[], &seeded()).is_none());
        assert!(load(AccountTier::Demo, &[("ACCOUNT", "   ")], &seeded()).is_none());
        assert!(load(AccountTier::Live, &[("CLIENT_ID", "7")], &seeded()).is_none());
        assert!(
            load_ibkr_config_from(AccountTier::Demo, &AccountCredentials::none(), &seeded())
                .is_none()
        );
    }

    const LOGIN: [(&str, &str); 2] = [("USERNAME", "gw-user"), ("PASSWORD", "gw-pass")];

    /// **An account holding the gateway's login pair and no `ACCOUNT` loads on the socket backend**,
    /// with an EMPTY account the exec connect fills from the gateway, at either tier; the Client
    /// Portal backend still needs the field, and half a pair is no login.
    #[test]
    fn the_gateway_login_stands_in_for_the_account_on_the_socket_only() {
        for tier in [AccountTier::Demo, AccountTier::Live] {
            let config = load(tier, &LOGIN, &seeded()).expect("the login pair loads on the socket");
            assert_eq!(
                (config.backend, config.account.as_str()),
                (IbkrBackend::Socket, ""),
                "{tier}"
            );
            assert!(!account_is_stored(tier, &AccountCredentials::none(), &seeded()), "{tier}");
        }
        let oauth = settings("live", &[("backend", "oauth")]);
        assert!(load(AccountTier::Live, &LOGIN, &oauth).is_some(), "oauth dials the socket");
        let cpapi = settings("live", &[("backend", "cpapi")]);
        assert!(load(AccountTier::Live, &LOGIN, &cpapi).is_none(), "cpapi needs ACCOUNT");
        assert!(load(AccountTier::Live, &LOGIN[..1], &seeded()).is_none(), "half a pair");
        // A stored `ACCOUNT` is read as it always was, beside the pair or not, on either backend.
        let both = [LOGIN[0], LOGIN[1], PAPER_ACCOUNT];
        for s in [&cpapi, &seeded()] {
            let account = load(AccountTier::Live, &both, s).map(|c| c.account);
            assert_eq!(account.as_deref(), Some("DUQ186573"));
        }
    }

    fn managed(accounts: &[&str]) -> Vec<String> {
        accounts.iter().map(|a| (*a).to_string()).collect()
    }

    /// **No `ACCOUNT` stored: the login's ONE account**, and a login of none or several (an advisor
    /// login) is refused in words that count the accounts and name the field that picks one.
    #[test]
    fn with_no_account_stored_the_logins_one_account_is_traded() {
        assert_eq!(account_from_gateway("", &managed(&["U1310001"])), Ok("U1310001".to_string()));
        assert_eq!(account_from_gateway("", &managed(&[" DU1 ", ""])), Ok("DU1".to_string()));
        for (list, count) in [(&["DU1", "DU2"][..], 2), (&[][..], 0)] {
            let why = account_from_gateway("", &managed(list)).unwrap_err();
            assert_eq!(
                why,
                IbkrConfigRefusal::NoAccountStored { count },
                "it counts the accounts, never names one"
            );
        }
    }

    /// **A stored `ACCOUNT` must be one the login manages** (the wrong-account guard), and one that is
    /// is traded exactly as before, whatever else the login manages.
    #[test]
    fn a_stored_account_must_be_one_the_login_manages() {
        let two = managed(&["DU1111111", " DU2222222 "]);
        assert_eq!(account_from_gateway("DU2222222", &two), Ok("DU2222222".to_string()));
        let why = account_from_gateway("DU9999999", &two).unwrap_err();
        assert_eq!(why, IbkrConfigRefusal::AccountNotManaged { count: 2 }, "no number");
        assert!(account_from_gateway("DU1111111", &[]).is_err(), "a login naming none");
    }

    #[test]
    fn cpapi_url_is_the_tiers_row() {
        let cpapi = settings("demo", &[("backend", "cpapi")]);
        let config = load(AccountTier::Demo, &[PAPER_ACCOUNT], &cpapi).expect("cpapi cfg");
        assert_eq!(config.backend, IbkrBackend::Cpapi);
        assert_eq!(config.cpapi_url, seed_url(UrlType::Demo, UrlProtocol::Rest, "cpapi"));
        assert_eq!(config.cpapi_url, "https://127.0.0.1:5000");
        // The CP Gateway's URL is the gateway's, so it is the TIER's `ibkr demo trade rest cpapi`
        // row like the socket's — and another account of the tier dials the same one.
        let at_5555 = vike_bridge_core::venue_adapter_fixture::venue_settings(
            "ibkr",
            "demo",
            &[("backend", "cpapi")],
        )
        .with_urls(demo_at("127.0.0.1:4102", "https://127.0.0.1:5555").urls().clone());
        assert_eq!(
            load(AccountTier::Demo, &[PAPER_ACCOUNT], &at_5555).unwrap().cpapi_url,
            "https://127.0.0.1:5555"
        );
        assert_eq!(
            load(AccountTier::Demo, &[("ACCOUNT", "DUQ186574")], &at_5555).unwrap().cpapi_url,
            "https://127.0.0.1:5555"
        );
        // …and a `CPAPI_URL` held as an account field is read by nothing.
        assert_eq!(
            load(
                AccountTier::Demo,
                &[PAPER_ACCOUNT, ("CPAPI_URL", "https://127.0.0.1:5555")],
                &cpapi
            )
            .unwrap()
            .cpapi_url,
            "https://127.0.0.1:5000"
        );
    }

    /// `config set` and the move store a value as written, so a padded `backend` must still read
    /// and a blank one must take the default rather than demote the start to paper.
    #[test]
    fn padded_and_blank_backend_rows() {
        let padded = settings("demo", &[("backend", " cpapi ")]);
        let config = load(AccountTier::Demo, &[PAPER_ACCOUNT], &padded).expect("padded rows read");
        assert_eq!(config.backend, IbkrBackend::Cpapi);
        let blank = settings("demo", &[("backend", "")]);
        let config = load(AccountTier::Demo, &[PAPER_ACCOUNT], &blank).expect("blank rows default");
        assert_eq!(config.backend, IbkrBackend::Socket);
    }

    #[test]
    fn unknown_backend_is_none() {
        let grpc = settings("demo", &[("backend", "grpc")]);
        assert!(load(AccountTier::Demo, &[PAPER_ACCOUNT], &grpc).is_none());
    }

    /// A socket row that is not `host:port` (one written past `config url set`'s check) is no
    /// gateway, like an unparseable port row was: by the rule the write judges a row by
    /// (`UrlSocket::parse`), so an empty host, port 0 and a scheme are refused too.
    #[test]
    fn a_socket_row_that_is_not_host_port_is_none() {
        for bad in ["127.0.0.1", "127.0.0.1:not-a-port", ":4102", "127.0.0.1:0", "tcp://h:4102"] {
            let s = demo_at(bad, "https://127.0.0.1:5000");
            assert!(load(AccountTier::Demo, &[PAPER_ACCOUNT], &s).is_none(), "{bad}");
        }
    }

    #[test]
    fn market_data_type_defaults_to_delayed_and_parses() {
        let config =
            load(AccountTier::Demo, &[PAPER_ACCOUNT, ("CLIENT_ID", "7")], &seeded()).unwrap();
        assert_eq!(config.market_data_type, MarketDataType::Delayed); // default
        assert_eq!(config.data_client_id, 8); // exec client_id (7) + 1
        let config2 = load(
            AccountTier::Demo,
            &[
                PAPER_ACCOUNT,
                ("CLIENT_ID", "7"),
                ("MKTDATA_TYPE", "realtime"),
                ("DATA_CLIENT_ID", "20"),
            ],
            &seeded(),
        )
        .unwrap();
        assert_eq!(config2.market_data_type, MarketDataType::Realtime);
        assert_eq!(config2.data_client_id, 20);
    }

    /// **`venue.ibkr.<tier>.extended_hours` is OFF unless the TIER's row says `1`**: no row, `0` and a
    /// blank row read off; a padded `1` reads on (`config set` stores a value as written); another
    /// tier's row and a machine-scoped (`any`) row are not this tier's; a word that is neither `1` nor `0`
    /// (nothing `config set` writes) refuses the config, like an unparseable port, rather than guess.
    #[test]
    fn extended_hours_is_off_unless_the_tiers_row_says_one() {
        let on = |rows: &[(&str, &str)]| {
            load(AccountTier::Demo, &[PAPER_ACCOUNT], &settings("demo", rows))
                .map(|config| config.extended_hours)
        };
        assert!(!load(AccountTier::Demo, &[PAPER_ACCOUNT], &seeded()).unwrap().extended_hours);
        assert_eq!(on(&[("extended_hours", "1")]), Some(true));
        assert_eq!(on(&[("extended_hours", " 1 ")]), Some(true));
        assert_eq!(on(&[("extended_hours", "0")]), Some(false));
        assert_eq!(on(&[("extended_hours", "  ")]), Some(false), "blank reads as no row");
        assert_eq!(on(&[("extended_hours", "yes")]), None, "not a word `config set` writes");
        // Another tier's row is not this tier's: the live row does not switch the demo account on.
        let live_only = settings("live", &[("extended_hours", "1")]);
        let config = load(AccountTier::Demo, &[PAPER_ACCOUNT], &live_only).unwrap();
        assert!(!config.extended_hours);
        assert!(!load(AccountTier::Live, &[PAPER_ACCOUNT], &seeded()).unwrap().extended_hours);
        // …and a machine-scoped (`any`) row is not read as a tier-scoped field's value.
        let any = VenueSettings::from_rows(
            "ibkr",
            &[vike_secrets::VenueSettingRow {
                venue: "ibkr".to_string(),
                tier: None,
                field: "EXTENDED_HOURS".to_string(),
                value: "1".to_string(),
            }],
        )
        .with_urls(VenueUrls::seed("ibkr"));
        assert!(!load(AccountTier::Demo, &[PAPER_ACCOUNT], &any).unwrap().extended_hours);
    }

    /// The socket reconcile connection's client id (decision 0140): `client_id + 2` by default, so
    /// it differs from the exec's and the defaulted data connection's on the one gateway; its own
    /// field overrides it; an unparseable value refuses the whole config, like every client id.
    #[test]
    fn reconcile_client_id_defaults_past_the_data_id_and_parses() {
        let config =
            load(AccountTier::Demo, &[PAPER_ACCOUNT, ("CLIENT_ID", "7")], &seeded()).unwrap();
        assert_eq!(
            (config.client_id, config.data_client_id, config.reconcile_client_id),
            (7, 8, 9)
        );
        let set = [PAPER_ACCOUNT, ("CLIENT_ID", "7"), ("RECON_CLIENT_ID", "30")];
        assert_eq!(load(AccountTier::Demo, &set, &seeded()).unwrap().reconcile_client_id, 30);
        let bad = [PAPER_ACCOUNT, ("RECON_CLIENT_ID", "x")];
        assert!(load(AccountTier::Demo, &bad, &seeded()).is_none());
    }
}
