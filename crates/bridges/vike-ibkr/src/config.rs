//! `IbkrConfig` — the resolved connection parameters + the paper/live gate. Resolved from a
//! CALLER-SUPPLIED credential map (the binary owns the store read) under
//! `IBKR_{DEMO|LIVE}_{ACCOUNT|CLIENT_ID|DATA_CLIENT_ID|MKTDATA_TYPE}`, and from the venue's
//! `venue_setting` rows for the GATEWAY — `venue.ibkr.<tier>.{backend,cpapi_url,host,port}`, one per
//! machine and tier (ruling 10; decision 0095, whose Task 7 retired the credential-map fold that
//! carried them as `IBKR_{TIER}_{…}` — `cpapi_url` joined the catalog in that task, because the fold
//! had rendered a stored `cpapi_url` row too).
//! ABSENT ACCOUNT (or unknown backend) → `None` → the app root keeps the venue paper. IBKR's
//! socket API has no in-crate auth — the Gateway holds the login — so the only "credential" here
//! is which account to trade, on which gateway.

use std::collections::HashMap;
use vike_bridge_core::credentials::{Environment, TierKeys, account_var};
use vike_model::accounts::account_keys::{AccountLabel, account_key};
use vike_secrets::venue_setting::VenueSettings;

/// Which transport surface the bridge speaks (Phase 1 wires only `Socket`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum IbkrBackend {
    #[default]
    Socket,
    Cpapi, // Phase 2
    Oauth, // Phase 2
}

impl IbkrBackend {
    fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "socket" => Some(IbkrBackend::Socket),
            "cpapi" => Some(IbkrBackend::Cpapi),
            "oauth" => Some(IbkrBackend::Oauth),
            _ => None,
        }
    }

    /// Public parse for binaries/mounts (the internal `parse` stays private to config loading).
    pub fn parse_public(s: &str) -> Option<Self> {
        Self::parse(s)
    }
}

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
    pub env: Environment,
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
}

/// Default socket port per environment: TWS paper 7497 / live 7496 (Gateway 4002/4001 — set
/// `venue.ibkr.<tier>.port` explicitly for Gateway).
fn default_port(env: Environment) -> u16 {
    match env {
        Environment::Live => 7496,
        _ => 7497,
    }
}

/// The variable name for one suffix at one tier — the single composition site, read by `var` (the
/// loader) and by [`tier_keys`] (the report of what a half-written set lacks).
fn key_name(env: Environment, suffix: &str) -> String {
    format!("IBKR_{}_{}", env.as_str(), suffix)
}

fn var<'a>(
    vars: &'a HashMap<String, String>,
    env: Environment,
    label: &AccountLabel,
    suffix: &str,
) -> Option<&'a str> {
    account_var(vars, &key_name(env, suffix), label)
}

/// Resolve config from a var map and the venue's settings. `None` when `account` is absent (the
/// paper gate), the backend string is unrecognized, or the port does not parse.
pub fn load_ibkr_config_from(
    env: Environment,
    vars: &HashMap<String, String>,
    settings: &VenueSettings,
) -> Option<IbkrConfig> {
    load_ibkr_config_for_account(env, &AccountLabel::Default, vars, settings)
}

/// [`load_ibkr_config_from`] for ONE NAMED ACCOUNT — `IBKR_{TIER}_{SUFFIX}__{LABEL}`.
///
/// Everything [`load_ibkr_config_from`] documents holds word for word, every default included; the
/// ONLY difference is the credential NAMES read, composed by
/// `vike_bridge_core::credentials::account_var`, which appends the label after the WHOLE of today's
/// key. `IBKR_DEMO_DATA_CLIENT_ID` — a three-word suffix — therefore needs no entry in any table.
///
/// ⚠ **[`AccountLabel::Default`] is byte-identically [`load_ibkr_config_from`]**, reached through
/// it.
///
/// ⚠ **No fallback to the unlabelled key**, for either half of what makes an IBKR mount distinct.
/// `_ACCOUNT` selects the `DU…`/`U…` account every order is placed in, so borrowing it would trade
/// the FIRST account; and `_CLIENT_ID`/`_DATA_CLIENT_ID` must DIFFER between two live TWS
/// connections — a second account silently inheriting the first's ids would have its socket
/// evicted by the gateway rather than mounting beside it. Both are the operator's to write.
///
/// ⚠ **The GATEWAY is the tier's, never the account's** —
/// `venue.ibkr.<tier>.{backend,cpapi_url,host,port}` from `settings`, so a labelled account dials
/// the gateway every account of its tier dials. That is ruling 10 (one gateway per machine and
/// tier); until decision 0095's Task 7 retired the credential-map fold, each account read its own
/// `IBKR_{TIER}_{HOST,PORT,BACKEND,CPAPI_URL}__{LABEL}`, and a credential row under one of those
/// names is refused at boot now.
pub fn load_ibkr_config_for_account(
    env: Environment,
    label: &AccountLabel,
    vars: &HashMap<String, String>,
    settings: &VenueSettings,
) -> Option<IbkrConfig> {
    let var = |suffix: &str| var(vars, env, label, suffix);
    let account = var("ACCOUNT")?.to_string();
    // The gateway is a MACHINE fact, one per tier (ruling 10):
    // `venue.ibkr.<tier>.{backend,cpapi_url,host,port}` rows, never a credential and never per
    // account (decision 0095; the credential-map fold that carried them as `IBKR_{TIER}_{…}` is
    // retired). EXACTLY the tier's row — a machine-scoped (`any`) row of a tier-scoped field is not
    // read, as `vike-cli config show` labels it — trimmed, and a blank row reads as no row, as the
    // credential-map read did (`config set` and the move store a value as written).
    let setting = |field: &str| {
        settings.get_exact(env.setting_tier(), field).map(str::trim).filter(|v| !v.is_empty())
    };
    let backend = IbkrBackend::parse(setting("backend").unwrap_or("socket"))?;
    let host = setting("host").unwrap_or("127.0.0.1").to_string();
    let port = match setting("port") {
        Some(p) => p.parse().ok()?,
        None => default_port(env),
    };
    let client_id = match var("CLIENT_ID") {
        Some(c) => c.parse().ok()?,
        None => 1,
    };
    let cpapi_url = setting("cpapi_url").unwrap_or("https://127.0.0.1:5000").to_string();
    let mktdata_type = MktDataType::parse(var("MKTDATA_TYPE").unwrap_or("delayed"))?;
    let data_client_id = match var("DATA_CLIENT_ID") {
        Some(c) => c.parse().ok()?,
        None => client_id + 1, // distinct from the exec connection
    };
    Some(IbkrConfig {
        env,
        backend,
        host,
        port,
        client_id,
        account,
        cpapi_url,
        mktdata_type,
        data_client_id,
    })
}

/// Whether the store names an `_ACCOUNT` at the LIVE tier for this account and NONE at the demo
/// tier — the "live tier alone" fact, which is the whole of what the demo-only mount may say about a
/// live tier it will never select. An IBKR credential IS the account (the Gateway holds the login),
/// read through the same private `var` and the same `account_var` as
/// [`load_ibkr_config_for_account`], so the key name and the account scoping cannot drift from it.
///
/// Deliberately NOT built from `load_ibkr_config_for_account`: that loader also parses the tier's
/// GATEWAY rows (`venue.ibkr.<tier>.{backend,port,…}`), and a row nobody can parse would turn "a
/// LIVE account is stored" back into "the store holds nothing" (live side), or "a demo account is
/// stored" into "no demo account" (demo side) — two different lies about the store. And a demo
/// `_ACCOUNT` that is present but whose gateway rows the loader refuses is NOT "live tier alone":
/// its fault is a setting, and the live account is not what is keeping the venue on paper.
#[must_use]
pub fn live_tier_account_alone(label: &AccountLabel, vars: &HashMap<String, String>) -> bool {
    live_tier_account_present(label, vars)
        && var(vars, Environment::Demo, label, "ACCOUNT").is_none()
}

/// Whether the store names an `_ACCOUNT` at the LIVE tier for this account, whatever the demo tier
/// holds — the "live tier BESIDE a demo one" fact, which [`live_tier_account_alone`] is the
/// no-demo-account half of. The same key, through the same `var`, for the same reason: an IBKR
/// credential IS the account, and the gateway rows are settings that may be unparseable without the
/// live account becoming any less present.
#[must_use]
pub fn live_tier_account_present(label: &AccountLabel, vars: &HashMap<String, String>) -> bool {
    var(vars, Environment::Live, label, "ACCOUNT").is_some()
}

/// The names ONE account's key set at `env` is written under, for a report of which are missing
/// from a half-written one ([`vike_bridge_core::credentials::TierKeys`]). `_ACCOUNT` is the one
/// REQUIRED key (see [`load_ibkr_config_for_account`]); the optional `_CLIENT_ID`, `_DATA_CLIENT_ID`
/// and `_MKTDATA_TYPE` belong to the tier too, so a store that wrote them and forgot the account is
/// a half-written set. The gateway is the tier's SETTINGS, not credentials, so it is not here.
/// Label-composed.
#[must_use]
pub fn tier_keys(env: Environment, label: &AccountLabel) -> Vec<TierKeys> {
    let name = |suffix: &str| account_key(&key_name(env, suffix), label);
    vec![TierKeys {
        required: vec![name("ACCOUNT")],
        tier_named: vec![
            name("ACCOUNT"),
            name("CLIENT_ID"),
            name("DATA_CLIENT_ID"),
            name("MKTDATA_TYPE"),
        ],
    }]
}

// There is deliberately NO `load_ibkr_config(env)` convenience here any more. It was a two-line
// wrapper that opened the workspace credential store itself and fed this function — a LIBRARY
// reading global configuration state its caller can neither see nor substitute, which is the class
// `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `CREDENTIAL_STORE_PIN` ratchets down. Every caller
// was a binary or a test, so each now loads the map and passes it — and, since decision 0095's
// Task 7, the venue's `venue_setting` rows beside it (`vike_secrets::venue_setting::
// load_venue_settings` over the same settings directory). (This crate re-exported that loader for
// two mount binaries with no vike-bridge-core edge; both are deleted, and so is the re-export —
// `lib.rs` says so.)

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use vike_bridge_core::credentials::Environment;
    use vike_secrets::venue_setting::VenueSettings;

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    /// The venue's `venue.ibkr.<tier>.<field>` rows — the shape the composition root reads out of
    /// the settings database (decision 0095).
    fn settings(tier: &str, rows: &[(&str, &str)]) -> VenueSettings {
        let rows: Vec<vike_secrets::VenueSettingRow> = rows
            .iter()
            .map(|(field, value)| vike_secrets::VenueSettingRow {
                venue: "ibkr".to_string(),
                tier: Some(tier.to_string()),
                field: field.to_ascii_uppercase(),
                value: (*value).to_string(),
            })
            .collect();
        VenueSettings::from_rows("ibkr", &rows)
    }

    fn none() -> VenueSettings {
        VenueSettings::default()
    }

    #[test]
    fn demo_defaults_to_paper_socket_port() {
        let v = vars(&[("IBKR_DEMO_CLIENT_ID", "7"), ("IBKR_DEMO_ACCOUNT", "DUQ186573")]);
        let cfg = load_ibkr_config_from(Environment::Demo, &v, &none()).expect("demo config");
        assert_eq!(cfg.backend, IbkrBackend::Socket);
        assert_eq!(cfg.host, "127.0.0.1");
        assert_eq!(cfg.port, 7497); // TWS paper default
        assert_eq!(cfg.client_id, 7);
        assert_eq!(cfg.account, "DUQ186573");
    }

    #[test]
    fn live_defaults_to_live_port() {
        let v = vars(&[("IBKR_LIVE_ACCOUNT", "U13112916")]);
        let cfg = load_ibkr_config_from(Environment::Live, &v, &none()).expect("live config");
        assert_eq!(cfg.port, 7496); // TWS live default
        assert_eq!(cfg.client_id, 1); // default client id
    }

    /// Decision 0095, Task 7: the gateway is read from the TIER's `venue_setting` rows — host,
    /// port and backend — while the account stays a credential.
    #[test]
    fn the_gateway_comes_from_the_tiers_settings() {
        let v = vars(&[("IBKR_DEMO_ACCOUNT", "DUQ186573")]);
        let s = settings("demo", &[("host", "<host>"), ("port", "4002"), ("backend", "cpapi")]);
        let cfg = load_ibkr_config_from(Environment::Demo, &v, &s).unwrap();
        assert_eq!(cfg.host, "<host>");
        assert_eq!(cfg.port, 4002); // Gateway paper
        assert_eq!(cfg.backend, IbkrBackend::Cpapi);
        // …and another tier's rows are not this tier's.
        let live_only = settings("live", &[("port", "4001")]);
        assert_eq!(load_ibkr_config_from(Environment::Demo, &v, &live_only).unwrap().port, 7497);
        // A padded row reads trimmed — `vike-cli config set` and the move copy a value as written —
        // and a blank one reads as no row, so the defaults apply instead of a paper demotion.
        let padded = settings("demo", &[("host", " <host> "), ("port", " 4002 ")]);
        let cfg = load_ibkr_config_from(Environment::Demo, &v, &padded).expect("padded rows read");
        assert_eq!((cfg.host.as_str(), cfg.port), ("<host>", 4002));
        let blank = settings("demo", &[("host", ""), ("port", "  ")]);
        let cfg = load_ibkr_config_from(Environment::Demo, &v, &blank).expect("blank rows default");
        assert_eq!((cfg.host.as_str(), cfg.port), ("127.0.0.1", 7497));
    }

    /// The gateway is TIER-scoped, so a machine-scoped (`any`) row of one of its fields is not this
    /// tier's value: `vike-cli config show` labels such a row "not read as that field's value", and
    /// the reader keeps that true rather than falling back to it.
    #[test]
    fn a_machine_scoped_row_of_a_gateway_field_is_not_read() {
        let v = vars(&[("IBKR_DEMO_ACCOUNT", "DUQ186573")]);
        let any = VenueSettings::from_rows(
            "ibkr",
            &[vike_secrets::VenueSettingRow {
                venue: "ibkr".to_string(),
                tier: None,
                field: "PORT".to_string(),
                value: "4002".to_string(),
            }],
        );
        assert_eq!(load_ibkr_config_from(Environment::Demo, &v, &any).unwrap().port, 7497);
    }

    /// A gateway setting left in the CREDENTIAL map under its legacy name is read by nothing — the
    /// boot refuses such a store (`vike_config::refuse_stranded_venue_settings`); the loader never
    /// looks there. Composed rather than spelled, so the settings registry's literal sweep does not
    /// read a variable here.
    #[test]
    fn a_legacy_gateway_name_in_the_credential_map_is_ignored() {
        let v = vars(&[
            ("IBKR_DEMO_ACCOUNT", "DUQ186573"),
            (concat!("IBKR", "_DEMO_HOST"), "<host>"),
            (concat!("IBKR", "_DEMO_PORT"), "4002"),
            (concat!("IBKR", "_DEMO_BACKEND"), "grpc"),
        ]);
        let cfg = load_ibkr_config_from(Environment::Demo, &v, &none()).expect("defaults apply");
        assert_eq!((cfg.host.as_str(), cfg.port), ("127.0.0.1", 7497));
        assert_eq!(cfg.backend, IbkrBackend::Socket);
    }

    /// The task map's behaviour change: a LABELLED account reads the tier-level gateway settings —
    /// the gateway is one per machine and tier (ruling 10) — while its account and client ids stay
    /// its own.
    #[test]
    fn a_labelled_account_reads_the_tier_gateway() {
        let hedge = AccountLabel::parse("HEDGE").expect("a legal label");
        let account = vike_model::accounts::account_keys::account_key("IBKR_DEMO_ACCOUNT", &hedge);
        let client = vike_model::accounts::account_keys::account_key("IBKR_DEMO_CLIENT_ID", &hedge);
        let v = vars(&[(account.as_str(), "DU0000001"), (client.as_str(), "11")]);
        let s = settings("demo", &[("host", "<host>"), ("port", "4002")]);
        let cfg = load_ibkr_config_for_account(Environment::Demo, &hedge, &v, &s).unwrap();
        assert_eq!((cfg.host.as_str(), cfg.port), ("<host>", 4002));
        assert_eq!((cfg.account.as_str(), cfg.client_id), ("DU0000001", 11));
    }

    #[test]
    fn absent_account_is_the_paper_gate() {
        assert!(load_ibkr_config_from(Environment::Demo, &vars(&[]), &none()).is_none());
    }

    #[test]
    fn cpapi_url_defaults_and_overrides() {
        let v = vars(&[("IBKR_DEMO_ACCOUNT", "DUQ186573")]);
        let cpapi = settings("demo", &[("backend", "cpapi")]);
        let cfg = load_ibkr_config_from(Environment::Demo, &v, &cpapi).expect("cpapi cfg");
        assert_eq!(cfg.backend, IbkrBackend::Cpapi);
        assert_eq!(cfg.cpapi_url, "https://127.0.0.1:5000");
        // The CP Gateway's URL is the gateway's, so it is the TIER's `venue.ibkr.<tier>.cpapi_url`
        // row like host/port/backend — and a labelled account dials the same one.
        let at_5555 =
            settings("demo", &[("backend", "cpapi"), ("cpapi_url", "https://127.0.0.1:5555")]);
        assert_eq!(
            load_ibkr_config_from(Environment::Demo, &v, &at_5555).unwrap().cpapi_url,
            "https://127.0.0.1:5555"
        );
        let alt = AccountLabel::parse("ALT").expect("a legal label");
        let alt_account =
            vike_model::accounts::account_keys::account_key("IBKR_DEMO_ACCOUNT", &alt);
        let labelled = vars(&[(alt_account.as_str(), "DUQ186574")]);
        assert_eq!(
            load_ibkr_config_for_account(Environment::Demo, &alt, &labelled, &at_5555)
                .unwrap()
                .cpapi_url,
            "https://127.0.0.1:5555"
        );
        // …and the credential-map name it was read under until then is read by nothing (the boot
        // refuses a store holding it). Composed, so the registry's literal sweep reads no variable.
        let v2 = vars(&[
            ("IBKR_DEMO_ACCOUNT", "DUQ186573"),
            (concat!("IBKR", "_DEMO_CPAPI_URL"), "https://127.0.0.1:5555"),
        ]);
        assert_eq!(
            load_ibkr_config_from(Environment::Demo, &v2, &cpapi).unwrap().cpapi_url,
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
        let v = vars(&[("IBKR_DEMO_ACCOUNT", "DUQ186573")]);
        let padded =
            settings("demo", &[("backend", " cpapi "), ("cpapi_url", " https://127.0.0.1:5555 ")]);
        let cfg = load_ibkr_config_from(Environment::Demo, &v, &padded).expect("padded rows read");
        assert_eq!(
            (cfg.backend, cfg.cpapi_url.as_str()),
            (IbkrBackend::Cpapi, "https://127.0.0.1:5555")
        );
        let blank = settings("demo", &[("backend", ""), ("cpapi_url", "   ")]);
        let cfg = load_ibkr_config_from(Environment::Demo, &v, &blank).expect("blank rows default");
        assert_eq!(
            (cfg.backend, cfg.cpapi_url.as_str()),
            (IbkrBackend::Socket, "https://127.0.0.1:5000")
        );
    }

    #[test]
    fn unknown_backend_is_none() {
        let v = vars(&[("IBKR_DEMO_ACCOUNT", "DUQ186573")]);
        let grpc = settings("demo", &[("backend", "grpc")]);
        assert!(load_ibkr_config_from(Environment::Demo, &v, &grpc).is_none());
    }

    #[test]
    fn an_unparseable_port_is_none() {
        let v = vars(&[("IBKR_DEMO_ACCOUNT", "DUQ186573")]);
        let bad = settings("demo", &[("port", "not-a-port")]);
        assert!(load_ibkr_config_from(Environment::Demo, &v, &bad).is_none());
    }

    #[test]
    fn mktdata_type_defaults_to_delayed_and_parses() {
        let v = vars(&[("IBKR_DEMO_ACCOUNT", "DUQ186573"), ("IBKR_DEMO_CLIENT_ID", "7")]);
        let cfg = load_ibkr_config_from(Environment::Demo, &v, &none()).unwrap();
        assert_eq!(cfg.mktdata_type, MktDataType::Delayed); // default
        assert_eq!(cfg.data_client_id, 8); // exec client_id (7) + 1
        let v2 = vars(&[
            ("IBKR_DEMO_ACCOUNT", "DUQ186573"),
            ("IBKR_DEMO_CLIENT_ID", "7"),
            ("IBKR_DEMO_MKTDATA_TYPE", "realtime"),
            ("IBKR_DEMO_DATA_CLIENT_ID", "20"),
        ]);
        let cfg2 = load_ibkr_config_from(Environment::Demo, &v2, &none()).unwrap();
        assert_eq!(cfg2.mktdata_type, MktDataType::Realtime);
        assert_eq!(cfg2.data_client_id, 20);
    }
}
