//! `IbkrConfig` — the resolved connection parameters + the paper/live gate. Resolved from the
//! MOUNTED ACCOUNT's credential FIELDS (`vike_secrets::account_credentials::AccountCredentials`;
//! the binary owns the store read) — `ACCOUNT`, `CLIENT_ID`, `DATA_CLIENT_ID`, `RECON_CLIENT_ID`,
//! `MKTDATA_TYPE`, the key schema's ibkr row (`vike_venues::credential_schema`) — and from the
//! venue's `venue_setting` rows for the GATEWAY — `venue.ibkr.<tier>.{backend,cpapi_url,host,port}`,
//! one per machine and tier (ruling 10; decision 0095, whose Task 7 retired the credential-map fold
//! that carried them as credentials — `cpapi_url` joined the catalog in that task, because the fold
//! had rendered a stored `cpapi_url` row too).
//! ABSENT ACCOUNT (or unknown backend) → `None` → the app root keeps the venue paper. IBKR's
//! socket API has no in-crate auth — the Gateway holds the login — so the only "credential" here
//! is which account to trade, on which gateway.
//!
//! **A credential belongs to its ACCOUNT, and the account has ONE tier** (decision 0156 item 6).
//! The loader reads the fields of the one account it is handed and never composes a name; the tier
//! it is handed is that account's, and picks the gateway rows and the default port and nothing
//! else. The labelled twin `load_ibkr_config_for_account` and the per-tier name composers went with
//! the names they composed: a labelled account IS its credentials, so [`load_ibkr_config_from`] is
//! the one loader.

use vike_model::AccountTier;
use vike_secrets::account_credentials::AccountCredentials;
use vike_secrets::venue_setting::{SettingTier, VenueSettings};
use vike_venues::IbkrBackend;
use vike_venues::credential_schema::{FieldOwner, VenueKeySchema, schema_for};

/// IBKR market-data type (`reqMarketDataType`): delayed-by-default so the feed works with no paid
/// subscriptions. `to_ibapi` maps to the `ibapi` 3.2.1 enum at the connect boundary (behind the
/// `ibkr-socket` feature, where ibapi exists).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MktDataType {
    Realtime,
    Frozen,
    #[default]
    Delayed,
    DelayedFrozen,
}

impl MktDataType {
    fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "delayed" => Some(MktDataType::Delayed),
            "realtime" | "live" => Some(MktDataType::Realtime),
            "frozen" => Some(MktDataType::Frozen),
            "delayed-frozen" | "delayed_frozen" => Some(MktDataType::DelayedFrozen),
            _ => None,
        }
    }
    #[cfg(feature = "ibkr-socket")]
    pub fn to_ibapi(self) -> ibapi::market_data::MarketDataType {
        use ibapi::market_data::MarketDataType as M;
        match self {
            MktDataType::Realtime => M::Realtime,
            MktDataType::Frozen => M::Frozen,
            MktDataType::Delayed => M::Delayed,
            MktDataType::DelayedFrozen => M::DelayedFrozen,
        }
    }
}

/// Resolved IBKR connection config. `account` is the paper `DU…` or live `U…` account number.
#[derive(Clone, Debug)]
pub struct IbkrConfig {
    /// The tier the config was loaded at — the mounted account's own (`MountInputs::tier`): it
    /// chose the gateway rows and the default port.
    pub tier: AccountTier,
    pub backend: IbkrBackend,
    pub host: String,
    pub port: u16,
    pub client_id: i32,
    pub account: String,
    /// Client Portal Web API base URL (cpapi backend), default `https://127.0.0.1:5000`. Inert for
    /// the socket backend.
    pub cpapi_url: String,
    /// `reqMarketDataType` selector for the Phase-3 realtime feed (delayed by default).
    pub mktdata_type: MktDataType,
    /// Client id for the DEDICATED market-data connection (distinct from `client_id`, the exec
    /// connection's).
    pub data_client_id: i32,
    /// Client id for the socket backend's DEDICATED reconcile connection
    /// (`crate::recon::socket::IbkrSocketReconClient`), on the SAME gateway as the exec: one more API
    /// client id on one login, never a second session (decision 0140). Defaults to `client_id + 2`,
    /// so it differs from the exec's and from the defaulted `data_client_id`. Inert for cpapi, whose
    /// reconcile client speaks REST to the CP Gateway and has no client id.
    ///
    /// ⚠ **Every client id on one gateway must be unique across ALL accounts mounted on it** —
    /// exec (`client_id`), data (`data_client_id`) and reconcile (`recon_client_id`) of every
    /// labelled account alike: IB refuses a second connection under an id in use. The defaults
    /// (`client_id`, `+1`, `+2`) collide for two accounts left at `client_id = 1`, and a set
    /// `DATA_CLIENT_ID` can land on a neighbour's recon id; only the exec/data/recon clash WITHIN
    /// one config is refused (`crate::recon::socket::require_distinct_recon_id`). The rest is the
    /// operator's to write.
    pub recon_client_id: i32,
}

/// Default socket port per tier: TWS paper 7497 / live 7496 (Gateway 4002/4001 — set
/// `venue.ibkr.<tier>.port` explicitly for Gateway; a deployment that runs the LIVE gateway on its
/// own port writes the `venue.ibkr.live.port` row, this default is not a deployment's answer).
/// `paper` reads as `demo` does.
fn default_port(tier: AccountTier) -> u16 {
    match tier {
        AccountTier::Live => 7496,
        AccountTier::Demo | AccountTier::Paper => 7497,
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
    let known = key_schema().field(word).is_some_and(|f| f.owner == FieldOwner::Account);
    assert!(known, "the ibkr key schema has no account field {word}");
    creds.get(word)
}

/// The fields a complete ibkr credential set needs — the key schema's REQUIRED account fields,
/// `ACCOUNT` alone ([`load_ibkr_config_from`]: the Gateway holds the login, so the account number
/// is the whole live gate). The optional `CLIENT_ID`, `DATA_CLIENT_ID`, `RECON_CLIENT_ID` and
/// `MKTDATA_TYPE` belong to the account too, so an account that holds them and lacks one of these
/// is a HALF-WRITTEN set, which the mount names by its missing fields
/// (`vike_secrets::account_credentials::AccountCredentials::missing`). The gateway is the tier's
/// SETTINGS, not credentials, so it is not here.
#[must_use]
pub fn required_fields() -> Vec<&'static str> {
    key_schema().required(FieldOwner::Account)
}

/// Resolve config from ONE account's credential fields and the venue's settings, at `tier` — the
/// account's own tier. `None` when `ACCOUNT` is absent (the paper gate), the backend string is
/// unrecognized, the port does not parse, or a client id does not parse.
///
/// ⚠ **Nothing is borrowed from another account**, for either half of what makes an IBKR mount
/// distinct. `ACCOUNT` selects the `DU…`/`U…` account every order is placed in, so borrowing it
/// would trade another account; and `CLIENT_ID`/`DATA_CLIENT_ID`/`RECON_CLIENT_ID` must DIFFER
/// between two live TWS connections — a second account silently inheriting the first's ids would
/// have its socket evicted by the gateway rather than mounting beside it. Both are the operator's
/// to write, on each account; `creds` is the one account's fields, so there is no other to fall
/// back to.
///
/// ⚠ **The GATEWAY is the tier's, never the account's** —
/// `venue.ibkr.<tier>.{backend,cpapi_url,host,port}` from `settings`, so every account of a tier
/// dials the gateway that tier's rows name. That is ruling 10 (one gateway per machine and tier);
/// until decision 0095's Task 7 retired the credential-map fold, each account read its own gateway
/// credentials, and a credential row under one of those legacy names is refused at boot now. A
/// gateway word held as an account FIELD is read by nothing.
pub fn load_ibkr_config_from(
    tier: AccountTier,
    creds: &AccountCredentials<'_>,
    settings: &VenueSettings,
) -> Option<IbkrConfig> {
    let field = |word: &str| account_field(creds, word);
    let account = field("ACCOUNT")?.to_string();
    // The gateway is a MACHINE fact, one per tier (ruling 10):
    // `venue.ibkr.<tier>.{backend,cpapi_url,host,port}` rows, never a credential and never per
    // account (decision 0095; the credential-map fold that carried them is retired). EXACTLY the
    // tier's row — a machine-scoped (`any`) row of a tier-scoped field is not read, as `vike-cli
    // config show` labels it — trimmed, and a blank row reads as no row, as the credential-map read
    // did (`config set` and the move store a value as written).
    let setting = |field: &str| {
        settings.get_exact(SettingTier::Tier(tier), field).map(str::trim).filter(|v| !v.is_empty())
    };
    let backend = IbkrBackend::parse(setting("backend").unwrap_or("socket"))?;
    let host = setting("host").unwrap_or("127.0.0.1").to_string();
    let port = match setting("port") {
        Some(p) => p.parse().ok()?,
        None => default_port(tier),
    };
    let client_id = match field("CLIENT_ID") {
        Some(c) => c.parse().ok()?,
        None => 1,
    };
    let cpapi_url = setting("cpapi_url").unwrap_or("https://127.0.0.1:5000").to_string();
    let mktdata_type = MktDataType::parse(field("MKTDATA_TYPE").unwrap_or("delayed"))?;
    let data_client_id = match field("DATA_CLIENT_ID") {
        Some(c) => c.parse().ok()?,
        None => client_id + 1, // distinct from the exec connection
    };
    let recon_client_id = match field("RECON_CLIENT_ID") {
        Some(c) => c.parse().ok()?,
        None => client_id + 2, // distinct from the exec and the defaulted data connection
    };
    Some(IbkrConfig {
        tier,
        backend,
        host,
        port,
        client_id,
        account,
        cpapi_url,
        mktdata_type,
        data_client_id,
        recon_client_id,
    })
}

// There is deliberately NO `load_ibkr_config(env)` convenience here any more. It was a two-line
// wrapper that opened the workspace credential store itself and fed this function — a LIBRARY
// reading global configuration state its caller can neither see nor substitute, which is the class
// `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `CREDENTIAL_STORE_PIN` ratchets down. Every caller
// was a binary or a test, so each now reads the store and passes the mounted account's fields —
// and, since decision 0095's Task 7, the venue's `venue_setting` rows beside them
// (`vike_secrets::venue_setting::load_venue_settings` over the same settings directory). (This
// crate re-exported that loader for two mount binaries with no vike-bridge-core edge; both are
// deleted, and so is the re-export — `lib.rs` says so.)

#[cfg(test)]
mod tests {
    use super::*;
    use vike_secrets::account_credentials::CredentialFields;

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
    /// the settings database (decision 0095).
    fn settings(tier: &str, rows: &[(&str, &str)]) -> VenueSettings {
        let rows: Vec<vike_secrets::VenueSettingRow> = rows
            .iter()
            .map(|(field, value)| vike_secrets::VenueSettingRow {
                venue: "ibkr".to_string(),
                tier: Some(tier.parse().expect("a tier")),
                field: field.to_ascii_uppercase(),
                value: (*value).to_string(),
            })
            .collect();
        VenueSettings::from_rows("ibkr", &rows)
    }

    fn none() -> VenueSettings {
        VenueSettings::default()
    }

    const PAPER_ACCOUNT: (&str, &str) = ("ACCOUNT", "DUQ186573");

    #[test]
    fn demo_defaults_to_paper_socket_port() {
        let cfg = load(AccountTier::Demo, &[("CLIENT_ID", "7"), PAPER_ACCOUNT], &none())
            .expect("demo config");
        assert_eq!(cfg.tier, AccountTier::Demo);
        assert_eq!(cfg.backend, IbkrBackend::Socket);
        assert_eq!(cfg.host, "127.0.0.1");
        assert_eq!(cfg.port, 7497); // TWS paper default
        assert_eq!(cfg.client_id, 7);
        assert_eq!(cfg.account, "DUQ186573");
    }

    #[test]
    fn live_defaults_to_live_port() {
        let cfg =
            load(AccountTier::Live, &[("ACCOUNT", "U13112916")], &none()).expect("live config");
        assert_eq!(cfg.tier, AccountTier::Live);
        assert_eq!(cfg.port, 7496); // TWS live default
        assert_eq!(cfg.client_id, 1); // default client id
    }

    /// Decision 0095, Task 7: the gateway is read from the TIER's `venue_setting` rows — host,
    /// port and backend — while the account stays a credential.
    #[test]
    fn the_gateway_comes_from_the_tiers_settings() {
        let s = settings("demo", &[("host", "<host>"), ("port", "4002"), ("backend", "cpapi")]);
        let cfg = load(AccountTier::Demo, &[PAPER_ACCOUNT], &s).unwrap();
        assert_eq!(cfg.host, "<host>");
        assert_eq!(cfg.port, 4002); // Gateway paper
        assert_eq!(cfg.backend, IbkrBackend::Cpapi);
        // …and another tier's rows are not this tier's.
        let live_only = settings("live", &[("port", "4001")]);
        assert_eq!(load(AccountTier::Demo, &[PAPER_ACCOUNT], &live_only).unwrap().port, 7497);
        // A padded row reads trimmed — `vike-cli config set` and the move copy a value as written —
        // and a blank one reads as no row, so the defaults apply instead of a paper demotion.
        let padded = settings("demo", &[("host", " <host> "), ("port", " 4002 ")]);
        let cfg = load(AccountTier::Demo, &[PAPER_ACCOUNT], &padded).expect("padded rows read");
        assert_eq!((cfg.host.as_str(), cfg.port), ("<host>", 4002));
        let blank = settings("demo", &[("host", ""), ("port", "  ")]);
        let cfg = load(AccountTier::Demo, &[PAPER_ACCOUNT], &blank).expect("blank rows default");
        assert_eq!((cfg.host.as_str(), cfg.port), ("127.0.0.1", 7497));
    }

    /// The gateway is TIER-scoped, so a machine-scoped (`any`) row of one of its fields is not this
    /// tier's value: `vike-cli config show` labels such a row "not read as that field's value", and
    /// the reader keeps that true rather than falling back to it.
    #[test]
    fn a_machine_scoped_row_of_a_gateway_field_is_not_read() {
        let any = VenueSettings::from_rows(
            "ibkr",
            &[vike_secrets::VenueSettingRow {
                venue: "ibkr".to_string(),
                tier: None,
                field: "PORT".to_string(),
                value: "4002".to_string(),
            }],
        );
        assert_eq!(load(AccountTier::Demo, &[PAPER_ACCOUNT], &any).unwrap().port, 7497);
    }

    /// A gateway word held as one of the ACCOUNT's credential fields is read by nothing: the loader
    /// reads the gateway from the tier's settings rows only, and never asks the account for it.
    #[test]
    fn a_gateway_field_held_by_the_account_is_ignored() {
        let cfg = load(
            AccountTier::Demo,
            &[PAPER_ACCOUNT, ("HOST", "<host>"), ("PORT", "4002"), ("BACKEND", "grpc")],
            &none(),
        )
        .expect("defaults apply");
        assert_eq!((cfg.host.as_str(), cfg.port), ("127.0.0.1", 7497));
        assert_eq!(cfg.backend, IbkrBackend::Socket);
    }

    /// Two accounts of one tier dial the tier's ONE gateway — the gateway is one per machine and
    /// tier (ruling 10) — while each account's number and client ids stay its own.
    #[test]
    fn two_accounts_of_one_tier_dial_the_tier_gateway_with_their_own_ids() {
        let s = settings("demo", &[("host", "<host>"), ("port", "4002")]);
        let first = load(AccountTier::Demo, &[PAPER_ACCOUNT], &s).unwrap();
        let second =
            load(AccountTier::Demo, &[("ACCOUNT", "DU0000001"), ("CLIENT_ID", "11")], &s).unwrap();
        for cfg in [&first, &second] {
            assert_eq!((cfg.host.as_str(), cfg.port), ("<host>", 4002));
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
        let cfg = load(
            AccountTier::Demo,
            &[
                ("ACCOUNT", "DU1"),
                ("CLIENT_ID", "5"),
                ("DATA_CLIENT_ID", "11"),
                ("RECON_CLIENT_ID", "12"),
                ("MKTDATA_TYPE", "realtime"),
            ],
            &none(),
        )
        .expect("a complete set");
        assert_eq!(
            (
                cfg.account.as_str(),
                cfg.client_id,
                cfg.data_client_id,
                cfg.recon_client_id,
                cfg.mktdata_type
            ),
            ("DU1", 5, 11, 12, MktDataType::Realtime)
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
        assert!(load(AccountTier::Demo, &[], &none()).is_none());
        assert!(load(AccountTier::Demo, &[("ACCOUNT", "   ")], &none()).is_none());
        assert!(load(AccountTier::Live, &[("CLIENT_ID", "7")], &none()).is_none());
        assert!(
            load_ibkr_config_from(AccountTier::Demo, &AccountCredentials::none(), &none())
                .is_none()
        );
    }

    #[test]
    fn cpapi_url_defaults_and_overrides() {
        let cpapi = settings("demo", &[("backend", "cpapi")]);
        let cfg = load(AccountTier::Demo, &[PAPER_ACCOUNT], &cpapi).expect("cpapi cfg");
        assert_eq!(cfg.backend, IbkrBackend::Cpapi);
        assert_eq!(cfg.cpapi_url, "https://127.0.0.1:5000");
        // The CP Gateway's URL is the gateway's, so it is the TIER's `venue.ibkr.<tier>.cpapi_url`
        // row like host/port/backend — and another account of the tier dials the same one.
        let at_5555 =
            settings("demo", &[("backend", "cpapi"), ("cpapi_url", "https://127.0.0.1:5555")]);
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

    /// The same trim / blank-is-unset rule the host and port rows get
    /// (`the_gateway_comes_from_the_tiers_settings`), for the two gateway fields it did not cover:
    /// `config set` and the move store a value as written, so a padded `cpapi_url` must still dial
    /// the URL and a blank `backend` or `cpapi_url` must take the default rather than demote the
    /// mount to paper.
    #[test]
    fn padded_and_blank_cpapi_url_and_backend_rows() {
        let padded =
            settings("demo", &[("backend", " cpapi "), ("cpapi_url", " https://127.0.0.1:5555 ")]);
        let cfg = load(AccountTier::Demo, &[PAPER_ACCOUNT], &padded).expect("padded rows read");
        assert_eq!(
            (cfg.backend, cfg.cpapi_url.as_str()),
            (IbkrBackend::Cpapi, "https://127.0.0.1:5555")
        );
        let blank = settings("demo", &[("backend", ""), ("cpapi_url", "   ")]);
        let cfg = load(AccountTier::Demo, &[PAPER_ACCOUNT], &blank).expect("blank rows default");
        assert_eq!(
            (cfg.backend, cfg.cpapi_url.as_str()),
            (IbkrBackend::Socket, "https://127.0.0.1:5000")
        );
    }

    #[test]
    fn unknown_backend_is_none() {
        let grpc = settings("demo", &[("backend", "grpc")]);
        assert!(load(AccountTier::Demo, &[PAPER_ACCOUNT], &grpc).is_none());
    }

    #[test]
    fn an_unparseable_port_is_none() {
        let bad = settings("demo", &[("port", "not-a-port")]);
        assert!(load(AccountTier::Demo, &[PAPER_ACCOUNT], &bad).is_none());
    }

    #[test]
    fn mktdata_type_defaults_to_delayed_and_parses() {
        let cfg = load(AccountTier::Demo, &[PAPER_ACCOUNT, ("CLIENT_ID", "7")], &none()).unwrap();
        assert_eq!(cfg.mktdata_type, MktDataType::Delayed); // default
        assert_eq!(cfg.data_client_id, 8); // exec client_id (7) + 1
        let cfg2 = load(
            AccountTier::Demo,
            &[
                PAPER_ACCOUNT,
                ("CLIENT_ID", "7"),
                ("MKTDATA_TYPE", "realtime"),
                ("DATA_CLIENT_ID", "20"),
            ],
            &none(),
        )
        .unwrap();
        assert_eq!(cfg2.mktdata_type, MktDataType::Realtime);
        assert_eq!(cfg2.data_client_id, 20);
    }

    /// The socket reconcile connection's client id (decision 0140): `client_id + 2` by default, so
    /// it differs from the exec's and the defaulted data connection's on the one gateway; its own
    /// field overrides it; an unparseable value refuses the whole config, like every client id.
    #[test]
    fn recon_client_id_defaults_past_the_data_id_and_parses() {
        let cfg = load(AccountTier::Demo, &[PAPER_ACCOUNT, ("CLIENT_ID", "7")], &none()).unwrap();
        assert_eq!((cfg.client_id, cfg.data_client_id, cfg.recon_client_id), (7, 8, 9));
        let set = [PAPER_ACCOUNT, ("CLIENT_ID", "7"), ("RECON_CLIENT_ID", "30")];
        assert_eq!(load(AccountTier::Demo, &set, &none()).unwrap().recon_client_id, 30);
        let bad = [PAPER_ACCOUNT, ("RECON_CLIENT_ID", "x")];
        assert!(load(AccountTier::Demo, &bad, &none()).is_none());
    }
}
