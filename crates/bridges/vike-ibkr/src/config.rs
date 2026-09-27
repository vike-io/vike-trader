//! `IbkrConfig` — the resolved connection parameters + the paper/live gate. Resolved from a
//! CALLER-SUPPLIED credential map (the binary owns the store read) under
//! `IBKR_{DEMO|LIVE}_{HOST|PORT|CLIENT_ID|ACCOUNT|BACKEND}`.
//! ABSENT ACCOUNT (or unknown backend) → `None` → the app root keeps the venue paper. IBKR's
//! socket API has no in-crate auth — the Gateway holds the login — so the only "credential" here
//! is which account/host/port to connect the socket to.

use std::collections::HashMap;
use vike_bridge_core::credentials::{Environment, account_var};
use vike_model::account_keys::AccountLabel;

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
/// subscriptions. `to_ibapi` maps to the vendored ibapi enum at the connect boundary (behind the
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
/// `IBKR_{env}_PORT` explicitly for Gateway).
fn default_port(env: Environment) -> u16 {
    match env {
        Environment::Live => 7496,
        _ => 7497,
    }
}

fn var<'a>(
    vars: &'a HashMap<String, String>,
    env: Environment,
    label: &AccountLabel,
    suffix: &str,
) -> Option<&'a str> {
    account_var(vars, &format!("IBKR_{}_{}", env.as_str(), suffix), label)
}

/// Resolve config from a var map. `None` when `account` is absent (the paper gate) or the backend
/// string is unrecognized.
pub fn load_ibkr_config_from(
    env: Environment,
    vars: &HashMap<String, String>,
) -> Option<IbkrConfig> {
    load_ibkr_config_for_account(env, &AccountLabel::Default, vars)
}

/// [`load_ibkr_config_from`] for ONE NAMED ACCOUNT — `IBKR_{TIER}_{SUFFIX}__{LABEL}`.
///
/// Everything [`load_ibkr_config_from`] documents holds word for word, every default included; the
/// ONLY difference is the NAMES read, composed by `vike_bridge_core::credentials::account_var`,
/// which appends the label after the WHOLE of today's key. `IBKR_DEMO_DATA_CLIENT_ID` — a
/// three-word suffix — therefore needs no entry in any table.
///
/// ⚠ **[`AccountLabel::Default`] is byte-identically [`load_ibkr_config_from`]**, reached through
/// it.
///
/// ⚠ **No fallback to the unlabelled key**, for either half of what makes an IBKR mount distinct.
/// `_ACCOUNT` selects the `DU…`/`U…` account every order is placed in, so borrowing it would trade
/// the FIRST account; and `_CLIENT_ID`/`_DATA_CLIENT_ID` must DIFFER between two live TWS
/// connections — a second account silently inheriting the first's ids would have its socket
/// evicted by the gateway rather than mounting beside it. Both are the operator's to write.
pub fn load_ibkr_config_for_account(
    env: Environment,
    label: &AccountLabel,
    vars: &HashMap<String, String>,
) -> Option<IbkrConfig> {
    let var = |suffix: &str| var(vars, env, label, suffix);
    let account = var("ACCOUNT")?.to_string();
    let backend = IbkrBackend::parse(var("BACKEND").unwrap_or("socket"))?;
    let host = var("HOST").unwrap_or("127.0.0.1").to_string();
    let port = match var("PORT") {
        Some(p) => p.parse().ok()?,
        None => default_port(env),
    };
    let client_id = match var("CLIENT_ID") {
        Some(c) => c.parse().ok()?,
        None => 1,
    };
    let cpapi_url = var("CPAPI_URL").unwrap_or("https://127.0.0.1:5000").to_string();
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

// There is deliberately NO `load_ibkr_config(env)` convenience here any more. It was a two-line
// wrapper that opened the workspace credential store itself and fed this function — a LIBRARY
// reading global configuration state its caller can neither see nor substitute, which is the class
// `crates/vike-ops/tests/settings_registry.rs`'s `CREDENTIAL_STORE_PIN` ratchets down. Every caller
// was a binary or a test, so each now loads the map and passes it: `load_ibkr_config_from(env,
// &vike_ibkr::load_workspace_dotenv())` (the loader is re-exported from this crate's root so
// vike-run / vike-backfill need no direct vike-bridge-core dependency). Resolution is unchanged —
// the SAME loader, called one frame up.

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use vike_bridge_core::credentials::Environment;

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn demo_defaults_to_paper_socket_port() {
        let v = vars(&[("IBKR_DEMO_CLIENT_ID", "7"), ("IBKR_DEMO_ACCOUNT", "DUQ186573")]);
        let cfg = load_ibkr_config_from(Environment::Demo, &v).expect("demo config");
        assert_eq!(cfg.backend, IbkrBackend::Socket);
        assert_eq!(cfg.host, "127.0.0.1");
        assert_eq!(cfg.port, 7497); // TWS paper default
        assert_eq!(cfg.client_id, 7);
        assert_eq!(cfg.account, "DUQ186573");
    }

    #[test]
    fn live_defaults_to_live_port() {
        let v = vars(&[("IBKR_LIVE_ACCOUNT", "U13112916")]);
        let cfg = load_ibkr_config_from(Environment::Live, &v).expect("live config");
        assert_eq!(cfg.port, 7496); // TWS live default
        assert_eq!(cfg.client_id, 1); // default client id
    }

    #[test]
    fn explicit_overrides_win() {
        let v = vars(&[
            ("IBKR_DEMO_ACCOUNT", "DUQ186573"),
            ("IBKR_DEMO_HOST", "<host>"),
            ("IBKR_DEMO_PORT", "4002"),
            ("IBKR_DEMO_BACKEND", "socket"),
        ]);
        let cfg = load_ibkr_config_from(Environment::Demo, &v).unwrap();
        assert_eq!(cfg.host, "<host>");
        assert_eq!(cfg.port, 4002); // Gateway paper
    }

    #[test]
    fn absent_account_is_the_paper_gate() {
        assert!(load_ibkr_config_from(Environment::Demo, &vars(&[])).is_none());
    }

    #[test]
    fn cpapi_url_defaults_and_overrides() {
        let v = vars(&[("IBKR_DEMO_ACCOUNT", "DUQ186573"), ("IBKR_DEMO_BACKEND", "cpapi")]);
        let cfg = load_ibkr_config_from(Environment::Demo, &v).expect("cpapi cfg");
        assert_eq!(cfg.backend, IbkrBackend::Cpapi);
        assert_eq!(cfg.cpapi_url, "https://127.0.0.1:5000");
        let v2 = vars(&[
            ("IBKR_DEMO_ACCOUNT", "DUQ186573"),
            ("IBKR_DEMO_BACKEND", "cpapi"),
            ("IBKR_DEMO_CPAPI_URL", "https://127.0.0.1:5555"),
        ]);
        assert_eq!(
            load_ibkr_config_from(Environment::Demo, &v2).unwrap().cpapi_url,
            "https://127.0.0.1:5555"
        );
    }

    #[test]
    fn unknown_backend_is_none() {
        let v = vars(&[("IBKR_DEMO_ACCOUNT", "DUQ186573"), ("IBKR_DEMO_BACKEND", "grpc")]);
        assert!(load_ibkr_config_from(Environment::Demo, &v).is_none());
    }

    #[test]
    fn mktdata_type_defaults_to_delayed_and_parses() {
        let v = vars(&[("IBKR_DEMO_ACCOUNT", "DUQ186573"), ("IBKR_DEMO_CLIENT_ID", "7")]);
        let cfg = load_ibkr_config_from(Environment::Demo, &v).unwrap();
        assert_eq!(cfg.mktdata_type, MktDataType::Delayed); // default
        assert_eq!(cfg.data_client_id, 8); // exec client_id (7) + 1
        let v2 = vars(&[
            ("IBKR_DEMO_ACCOUNT", "DUQ186573"),
            ("IBKR_DEMO_CLIENT_ID", "7"),
            ("IBKR_DEMO_MKTDATA_TYPE", "realtime"),
            ("IBKR_DEMO_DATA_CLIENT_ID", "20"),
        ]);
        let cfg2 = load_ibkr_config_from(Environment::Demo, &v2).unwrap();
        assert_eq!(cfg2.mktdata_type, MktDataType::Realtime);
        assert_eq!(cfg2.data_client_id, 20);
    }
}
