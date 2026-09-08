//! FXCM ForexConnect login config, read from the gitignored `.env`.
//!
//! FXCM does NOT use the HMAC [`Credentials`](vike_bridge_core::credentials::Credentials) shape —
//! it auths with a user/password plus a host-discovery URL and a connection name ("Demo"/"Real").
//! The password never reaches Debug/Display. Absent user/password → `None` (the live gate).

use std::collections::HashMap;
use vike_bridge_core::credentials::{Environment, account_var};
use vike_model::account_keys::AccountLabel;

/// FXCM's default host-discovery URL (both demo and real resolve through it via `connection`).
const DEFAULT_HOST_URL: &str = "http://www.fxcorporate.com/Hosts.jsp";

/// ForexConnect session parameters.
#[derive(Clone)]
pub struct FxcmConfig {
    pub user: String,
    pub password: String,
    pub url: String,
    /// "Demo" | "Real"
    pub connection: String,
}

impl std::fmt::Debug for FxcmConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // never leak the password
        write!(
            f,
            "FxcmConfig(user={}, connection={}, url={})",
            self.user, self.connection, self.url
        )
    }
}

/// The `(user, password, url, connection)` env-var names for FXCM at `env`
/// (e.g. `FXCM_DEMO_USER`, `FXCM_DEMO_PASSWORD`, …).
pub fn fxcm_env_var_names(env: Environment) -> (String, String, String, String) {
    let prefix = format!("FXCM_{}", env.as_str());
    (
        format!("{prefix}_USER"),
        format!("{prefix}_PASSWORD"),
        format!("{prefix}_URL"),
        format!("{prefix}_CONNECTION"),
    )
}

fn default_connection(env: Environment) -> &'static str {
    match env {
        Environment::Live => "Real",
        _ => "Demo",
    }
}

/// Read FXCM config from a var map (process env or a parsed `.env`). `None` when user or
/// password is unset/blank — that absence IS the live gate (no creds → stay paper). URL and
/// connection fall back to sensible defaults when unset.
pub fn load_fxcm_config_from(
    env: Environment,
    vars: &HashMap<String, String>,
) -> Option<FxcmConfig> {
    load_fxcm_config_for_account(env, &AccountLabel::Default, vars)
}

/// [`load_fxcm_config_from`] for ONE NAMED ACCOUNT — `FXCM_{TIER}_{SUFFIX}__{LABEL}`.
///
/// Everything [`load_fxcm_config_from`] documents holds word for word, the LEGACY-tier fallback and
/// the URL/connection defaults included; the ONLY difference is the NAMES read, composed by
/// `vike_bridge_core::credentials::account_var`, which appends the label after the WHOLE of today's
/// key. The FX `_USER`/`_PASSWORD` pair is one of the shapes `vike_model::account_keys` names as
/// forcing the label to the END of the grammar, so it needs no entry in any table.
///
/// ⚠ **[`AccountLabel::Default`] is byte-identically [`load_fxcm_config_from`]**, reached through
/// it.
///
/// ⚠ **No fallback to the unlabelled key** — including for the OPTIONAL `_URL`/`_CONNECTION`, which
/// fall back to the same literals they always did rather than to the default account's values. A
/// borrowed `_CONNECTION` is the one that would bite: `"Real"` where the operator wrote `"Demo"`
/// silently moves a second account onto the live gateway.
pub fn load_fxcm_config_for_account(
    env: Environment,
    label: &AccountLabel,
    vars: &HashMap<String, String>,
) -> Option<FxcmConfig> {
    // primary tier (e.g. FXCM_LIVE_*), then the legacy tier (FXCM_MAINNET_*) so pre-rename
    // `.env` files keep working — same contract as credentials::load_credentials_from.
    load_fxcm_tier(env, env.as_str(), label, vars)
        .or_else(|| env.legacy_str().and_then(|t| load_fxcm_tier(env, t, label, vars)))
}

fn load_fxcm_tier(
    env: Environment,
    tier: &str,
    label: &AccountLabel,
    vars: &HashMap<String, String>,
) -> Option<FxcmConfig> {
    let prefix = format!("FXCM_{tier}");
    let (user_k, pass_k, url_k, conn_k) = (
        format!("{prefix}_USER"),
        format!("{prefix}_PASSWORD"),
        format!("{prefix}_URL"),
        format!("{prefix}_CONNECTION"),
    );
    let get = |k: &str| account_var(vars, k, label).map(str::to_string).unwrap_or_default();

    let user = get(&user_k);
    let password = get(&pass_k);
    if user.is_empty() || password.is_empty() {
        return None;
    }

    let url = {
        let v = get(&url_k);
        if v.is_empty() { DEFAULT_HOST_URL.to_string() } else { v }
    };
    let connection = {
        let v = get(&conn_k);
        if v.is_empty() { default_connection(env).to_string() } else { v }
    };

    Some(FxcmConfig { user, password, url, connection })
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_model::account_keys::account_key;

    #[test]
    fn gate_defaults_and_no_password_leak() {
        let mut vars = HashMap::new();
        // no creds -> live gate returns None
        assert!(load_fxcm_config_from(Environment::Demo, &vars).is_none());

        vars.insert("FXCM_DEMO_USER".into(), "D251112911".into());
        vars.insert("FXCM_DEMO_PASSWORD".into(), "s3cr3t".into());
        let c = load_fxcm_config_from(Environment::Demo, &vars).unwrap();
        assert_eq!(c.user, "D251112911");
        assert_eq!(c.connection, "Demo"); // default for Demo
        assert!(c.url.contains("fxcorporate"));
        // Debug must not leak the password
        assert!(!format!("{c:?}").contains("s3cr3t"));

        let c2 = load_fxcm_config_from(Environment::Live, &{
            let mut m = HashMap::new();
            m.insert("FXCM_MAINNET_USER".into(), "u".into());
            m.insert("FXCM_MAINNET_PASSWORD".into(), "p".into());
            m
        })
        .unwrap();
        assert_eq!(c2.connection, "Real"); // default for Live
    }

    /// **The 2×2 that keeps [`load_fxcm_config_for_account`] account-aware** — the twin of
    /// `crates/vike-mount/tests/account_credential_isolation.rs`, which cannot reach this venue:
    /// `vike_mount`'s fxcm row short-circuits on `vike_fxcm::sdk_linked()`, false in every build any
    /// gate will ever run, so the credential half is never consulted there.
    ///
    /// The load-bearing cell is `labelled reads none of the default account's credentials`: a
    /// loader that stopped appending the label would log account `ALT` into the FIRST account's FX
    /// session.
    ///
    /// ⚠ Every name here is COMPOSED — `fxcm_env_var_names` for the base, `account_key` for the
    /// labelled one — and none is spelled. A fixture that spelled the labelled name out would
    /// prove the grammar works on a COPY of the venue's names and would keep passing after a
    /// rename; it would also plant an account separator in a bridge `src/` literal, which
    /// `crates/vike-model/tests/account_keys.rs`'s
    /// `no_existing_credential_key_contains_the_separator` sweeps for and refuses.
    #[test]
    fn a_labelled_account_reads_its_own_credentials_and_only_its_own() {
        let alt = AccountLabel::parse("ALT").expect("a legal label");
        let (user_k, pass_k, _, _) = fxcm_env_var_names(Environment::Demo);
        let default_only = HashMap::from([
            (user_k.clone(), "first".to_string()),
            (pass_k.clone(), "pw1".to_string()),
        ]);
        let labelled_only = HashMap::from([
            (account_key(&user_k, &alt), "second".to_string()),
            (account_key(&pass_k, &alt), "pw2".to_string()),
        ]);

        // THE mutation detector: the default account's keys configure the default account ONLY.
        assert_eq!(
            load_fxcm_config_for_account(Environment::Demo, &AccountLabel::Default, &default_only)
                .map(|c| c.user),
            Some("first".to_string())
        );
        assert!(
            load_fxcm_config_for_account(Environment::Demo, &alt, &default_only).is_none(),
            "a labelled account must NOT borrow the default account's FX login"
        );

        // …and the mirror, which catches a loader that appended the label unconditionally.
        assert_eq!(
            load_fxcm_config_for_account(Environment::Demo, &alt, &labelled_only).map(|c| c.user),
            Some("second".to_string())
        );
        assert!(
            load_fxcm_config_for_account(Environment::Demo, &AccountLabel::Default, &labelled_only)
                .is_none()
        );
    }

    /// **BYTE-IDENTITY**: with both accounts in one store the DEFAULT account resolves exactly what
    /// it resolves alone — an EQUALITY, so nothing here pins today's answer.
    ///
    /// The OPTIONAL fields are the ones worth the assertion: `_URL`/`_CONNECTION` fall back to this
    /// module's own literals, never to the neighbour's values, so a second account configured for
    /// `"Real"` cannot drag the first onto the live gateway (or the reverse).
    #[test]
    fn a_second_account_changes_nothing_about_the_first() {
        let alt = AccountLabel::parse("ALT").expect("a legal label");
        let (user_k, pass_k, url_k, conn_k) = fxcm_env_var_names(Environment::Demo);
        let alone = HashMap::from([
            (user_k.clone(), "first".to_string()),
            (pass_k.clone(), "pw1".to_string()),
        ]);
        let mut beside = alone.clone();
        for (k, v) in [
            (&user_k, "second"),
            (&pass_k, "pw2"),
            (&conn_k, "Real"),
            (&url_k, "http://elsewhere.example/Hosts.jsp"),
        ] {
            beside.insert(account_key(k, &alt), v.to_string());
        }

        let a = load_fxcm_config_from(Environment::Demo, &alone).expect("configured");
        let b = load_fxcm_config_from(Environment::Demo, &beside).expect("configured");
        assert_eq!(
            (a.user, a.password, a.url, a.connection),
            (b.user, b.password, b.url, b.connection)
        );

        // …and the labelled account really did get its own optional fields, so the equality above
        // is not the vacuous kind where nothing was read at all.
        let c = load_fxcm_config_for_account(Environment::Demo, &alt, &beside).expect("configured");
        assert_eq!(c.connection, "Real");
        assert!(c.url.contains("elsewhere.example"));
    }
}
