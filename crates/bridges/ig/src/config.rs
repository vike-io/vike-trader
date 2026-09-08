//! IG (IG Group) v20-style REST login config from the gitignored `.env`.
//!
//! IG auths in two parts: a per-app **API key** (`X-IG-API-KEY`) plus account **identifier +
//! password** exchanged at `POST /session` for short-lived `CST` / `X-SECURITY-TOKEN` headers.
//! None of the three secrets reach Debug/Display. Absent any of them → `None` (the live gate).

use std::collections::HashMap;
use vike_bridge_core::credentials::{Environment, account_var};
use vike_model::account_keys::AccountLabel;

/// IG REST base for an environment. Demo = the demo gateway, Live = the live gateway.
pub fn ig_rest_base(env: Environment) -> &'static str {
    match env {
        Environment::Live => "https://api.ig.com/gateway/deal",
        _ => "https://demo-api.ig.com/gateway/deal",
    }
}

/// IG login parameters.
#[derive(Clone)]
pub struct IgConfig {
    pub api_key: String,
    pub identifier: String,
    pub password: String,
    pub rest_base: String,
}

impl std::fmt::Debug for IgConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // never leak api_key / identifier / password
        write!(f, "IgConfig(rest_base={})", self.rest_base)
    }
}

/// The `(api_key, identifier, password)` env-var names for IG at `env`
/// (e.g. `IG_DEMO_API_KEY`, `IG_DEMO_IDENTIFIER`, `IG_DEMO_PASSWORD`).
pub fn ig_env_var_names(env: Environment) -> (String, String, String) {
    let prefix = format!("IG_{}", env.as_str());
    (format!("{prefix}_API_KEY"), format!("{prefix}_IDENTIFIER"), format!("{prefix}_PASSWORD"))
}

/// Read IG config from a var map (process env or a parsed `.env`). `None` when any of the three
/// secrets is unset/blank — the live gate.
pub fn load_ig_config_from(env: Environment, vars: &HashMap<String, String>) -> Option<IgConfig> {
    load_ig_config_for_account(env, &AccountLabel::Default, vars)
}

/// [`load_ig_config_from`] for ONE NAMED ACCOUNT — `IG_{TIER}_{SUFFIX}__{LABEL}`.
///
/// Everything [`load_ig_config_from`] documents holds word for word, the LEGACY-tier fallback
/// included; the ONLY difference is the NAMES read, composed by
/// `vike_bridge_core::credentials::account_var`, which appends the label after the WHOLE of today's
/// key. `IG_DEMO_IDENTIFIER` — one of the two bespoke suffixes that forced the label to the END of
/// the grammar (`vike_model::account_keys`) — therefore needs no entry in any table.
///
/// ⚠ **[`AccountLabel::Default`] is byte-identically [`load_ig_config_from`]**, reached through it.
///
/// ⚠ **No fallback to the unlabelled key.** All THREE secrets are per-account here — IG's
/// `identifier`/`password` are exchanged at `POST /session` for the tokens every later request
/// carries, so borrowing any of them logs account `ALT` into the FIRST account.
pub fn load_ig_config_for_account(
    env: Environment,
    label: &AccountLabel,
    vars: &HashMap<String, String>,
) -> Option<IgConfig> {
    load_ig_tier(env, env.as_str(), label, vars)
        .or_else(|| env.legacy_str().and_then(|t| load_ig_tier(env, t, label, vars)))
}

fn load_ig_tier(
    env: Environment,
    tier: &str,
    label: &AccountLabel,
    vars: &HashMap<String, String>,
) -> Option<IgConfig> {
    let prefix = format!("IG_{tier}");
    let (key_k, id_k, pw_k) =
        (format!("{prefix}_API_KEY"), format!("{prefix}_IDENTIFIER"), format!("{prefix}_PASSWORD"));
    let get = |k: &str| account_var(vars, k, label).map(str::to_string).unwrap_or_default();

    let api_key = get(&key_k);
    let identifier = get(&id_k);
    let password = get(&pw_k);
    if api_key.is_empty() || identifier.is_empty() || password.is_empty() {
        return None;
    }
    Some(IgConfig { api_key, identifier, password, rest_base: ig_rest_base(env).to_string() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_and_no_secret_leak() {
        let mut vars = HashMap::new();
        assert!(load_ig_config_from(Environment::Demo, &vars).is_none());

        vars.insert("IG_DEMO_API_KEY".into(), "key-xyz".into());
        vars.insert("IG_DEMO_IDENTIFIER".into(), "myuser".into());
        vars.insert("IG_DEMO_PASSWORD".into(), "s3cr3t".into());
        let c = load_ig_config_from(Environment::Demo, &vars).unwrap();
        assert!(c.rest_base.contains("demo-api.ig.com"));
        let dbg = format!("{c:?}");
        assert!(!dbg.contains("key-xyz") && !dbg.contains("myuser") && !dbg.contains("s3cr3t"));

        assert!(ig_rest_base(Environment::Live).contains("//api.ig.com"));
    }
}
