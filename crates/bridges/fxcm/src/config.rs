//! FXCM ForexConnect login config: the login pair from the caller's credential map, the host URL
//! and connection name from the venue's `venue_setting` rows.
//!
//! FXCM does NOT use the HMAC [`Credentials`](vike_bridge_core::credentials::Credentials) shape —
//! it auths with a user/password plus a host-discovery URL and a connection name ("Demo"/"Real").
//! The URL and the connection are the TIER's settings — `venue.fxcm.<tier>.{url,connection}`
//! (decision 0095, whose Task 7 retired the credential-map fold that carried them as
//! `FXCM_{TIER}_{URL,CONNECTION}`). The password never reaches Debug/Display. Absent user/password
//! → `None` (the live gate).

use std::collections::HashMap;
use vike_bridge_core::credentials::{Environment, TierKeys, account_var};
use vike_model::accounts::account_keys::{AccountLabel, account_key};
use vike_secrets::venue_setting::VenueSettings;

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

/// The `(user, password)` credential names for FXCM at `env` (`FXCM_DEMO_USER`,
/// `FXCM_DEMO_PASSWORD`). The URL and the connection are settings, not credentials, since decision
/// 0095's Task 7 — see the module doc.
pub fn fxcm_env_var_names(env: Environment) -> (String, String) {
    tier_var_names(env.as_str())
}

/// The `(user, password)` variable names for one tier STRING — the single composition site.
/// [`fxcm_env_var_names`] (the `Environment`-keyed spelling), [`login_at`] (which also walks the
/// LEGACY tier string, which no `Environment` spells) and [`tier_keys`] all fold through it, so a
/// rename cannot leave the loader reading one spelling and a report naming another.
fn tier_var_names(tier: &str) -> (String, String) {
    let prefix = format!("FXCM_{tier}");
    (format!("{prefix}_USER"), format!("{prefix}_PASSWORD"))
}

/// The names ONE account's login at `env` is written under — one entry per tier spelling
/// [`load_fxcm_config_for_account`] walks (the current tier, then the legacy one) — for a report of
/// which are missing from a half-written login ([`vike_bridge_core::credentials::TierKeys`]). Both
/// are required and both belong to the tier. Label-composed. The URL and the connection are
/// settings, not credentials, so they are not here.
#[must_use]
pub fn tier_keys(env: Environment, label: &AccountLabel) -> Vec<TierKeys> {
    let mut tiers = vec![env.as_str()];
    tiers.extend(env.legacy_str());
    tiers
        .into_iter()
        .map(|tier| {
            let (user, password) = tier_var_names(tier);
            let names = vec![account_key(&user, label), account_key(&password, label)];
            TierKeys { required: names.clone(), tier_named: names }
        })
        .collect()
}

fn default_connection(env: Environment) -> &'static str {
    match env {
        Environment::Live => "Real",
        _ => "Demo",
    }
}

/// Read FXCM config from a var map and the venue's settings. `None` when user or password is
/// unset/blank — that absence IS the live gate (no creds → stay paper). URL and connection fall
/// back to sensible defaults when the tier has no row.
pub fn load_fxcm_config_from(
    env: Environment,
    vars: &HashMap<String, String>,
    settings: &VenueSettings,
) -> Option<FxcmConfig> {
    load_fxcm_config_for_account(env, &AccountLabel::Default, vars, settings)
}

/// [`load_fxcm_config_from`] for ONE NAMED ACCOUNT — `FXCM_{TIER}_{SUFFIX}__{LABEL}`.
///
/// Everything [`load_fxcm_config_from`] documents holds word for word, the LEGACY-tier fallback and
/// the URL/connection defaults included; the ONLY difference is the credential NAMES read, composed
/// by `vike_bridge_core::credentials::account_var`, which appends the label after the WHOLE of
/// today's key. The FX `_USER`/`_PASSWORD` pair is one of the shapes `vike_model::accounts::account_keys`
/// names as forcing the label to the END of the grammar, so it needs no entry in any table.
///
/// ⚠ **[`AccountLabel::Default`] is byte-identically [`load_fxcm_config_from`]**, reached through
/// it.
///
/// ⚠ **No fallback to the unlabelled login key.** The URL and the connection are not an ACCOUNT's
/// to carry at all: they are the TIER's `venue.fxcm.<tier>.{url,connection}` rows (ruling 10,
/// decision 0095), so a labelled account reads the same host and connection every account of its
/// tier reads, at the credential tier's setting tier — the legacy `MAINNET` login included, which
/// reads the `live` tier's rows.
pub fn load_fxcm_config_for_account(
    env: Environment,
    label: &AccountLabel,
    vars: &HashMap<String, String>,
    settings: &VenueSettings,
) -> Option<FxcmConfig> {
    // primary tier (e.g. FXCM_LIVE_*), then the legacy tier (FXCM_MAINNET_*) so pre-rename
    // `.env` files keep working — same contract as credentials::load_credentials_from.
    let (user, password) = login_at(env.as_str(), label, vars)
        .or_else(|| env.legacy_str().and_then(|t| login_at(t, label, vars)))?;
    // EXACTLY the tier's row (a machine-scoped `any` row of a tier-scoped field is not read),
    // trimmed, and a blank row reads as no row — as the credential-map read did.
    let setting = |field: &str| {
        settings.get_exact(env.setting_tier(), field).map(str::trim).filter(|v| !v.is_empty())
    };
    let url = setting("url").unwrap_or(DEFAULT_HOST_URL).to_string();
    let connection = setting("connection").unwrap_or(default_connection(env)).to_string();
    Some(FxcmConfig { user, password, url, connection })
}

/// The `(user, password)` pair under the credential-tier token `tier`, both non-blank, or `None`.
/// (Its predecessor, `load_fxcm_tier`, read the whole tier — the URL and the connection under
/// `FXCM_{TIER}_{URL,CONNECTION}` included — until decision 0095's Task 7 made those two the tier's
/// settings.)
fn login_at(
    tier: &str,
    label: &AccountLabel,
    vars: &HashMap<String, String>,
) -> Option<(String, String)> {
    let (user_k, password_k) = tier_var_names(tier);
    let get = |k: &str| account_var(vars, k, label).map(str::to_string).unwrap_or_default();
    let user = get(&user_k);
    let password = get(&password_k);
    (!user.is_empty() && !password.is_empty()).then_some((user, password))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_model::accounts::account_keys::account_key;
    use vike_secrets::venue_setting::VenueSettings;

    fn none() -> VenueSettings {
        VenueSettings::default()
    }

    /// The venue's `venue.fxcm.<tier>.<field>` rows — the shape the composition root reads out of
    /// the settings database (decision 0095).
    fn settings(tier: &str, rows: &[(&str, &str)]) -> VenueSettings {
        let rows: Vec<vike_secrets::VenueSettingRow> = rows
            .iter()
            .map(|(field, value)| vike_secrets::VenueSettingRow {
                venue: "fxcm".to_string(),
                tier: Some(tier.to_string()),
                field: field.to_ascii_uppercase(),
                value: (*value).to_string(),
            })
            .collect();
        VenueSettings::from_rows("fxcm", &rows)
    }

    #[test]
    fn gate_defaults_and_no_password_leak() {
        let mut vars = HashMap::new();
        // no creds -> live gate returns None
        assert!(load_fxcm_config_from(Environment::Demo, &vars, &none()).is_none());

        vars.insert("FXCM_DEMO_USER".into(), "D251112911".into());
        vars.insert("FXCM_DEMO_PASSWORD".into(), "s3cr3t".into());
        let c = load_fxcm_config_from(Environment::Demo, &vars, &none()).unwrap();
        assert_eq!(c.user, "D251112911");
        assert_eq!(c.connection, "Demo"); // default for Demo
        assert!(c.url.contains("fxcorporate"));
        // Debug must not leak the password
        assert!(!format!("{c:?}").contains("s3cr3t"));

        let c2 = load_fxcm_config_from(
            Environment::Live,
            &{
                let mut m = HashMap::new();
                m.insert("FXCM_MAINNET_USER".into(), "u".into());
                m.insert("FXCM_MAINNET_PASSWORD".into(), "p".into());
                m
            },
            &none(),
        )
        .unwrap();
        assert_eq!(c2.connection, "Real"); // default for Live
    }

    /// Decision 0095, Task 7: the host URL and the connection name are the TIER's `venue_setting`
    /// rows, read at the credential tier's setting tier — the legacy `MAINNET` login included —
    /// and a legacy name left in the credential map is read by nothing (the boot refuses it).
    #[test]
    fn url_and_connection_come_from_the_tiers_settings() {
        let (user_k, pass_k) = fxcm_env_var_names(Environment::Demo);
        let mut vars = HashMap::from([(user_k, "u".to_string()), (pass_k, "p".to_string())]);
        let s = settings(
            "demo",
            &[("url", "http://elsewhere.example/Hosts.jsp"), ("connection", "Real")],
        );
        let c = load_fxcm_config_from(Environment::Demo, &vars, &s).unwrap();
        assert_eq!(
            (c.url.as_str(), c.connection.as_str()),
            ("http://elsewhere.example/Hosts.jsp", "Real")
        );

        // Composed rather than spelled: the registry's literal sweep would read a variable here.
        vars.insert(concat!("FXCM", "_DEMO_CONNECTION").to_string(), "Real".to_string());
        let ignored = load_fxcm_config_from(Environment::Demo, &vars, &none()).unwrap();
        assert_eq!(ignored.connection, "Demo", "a credential-map connection is read by nothing");

        // The legacy MAINNET login reads the LIVE tier's rows.
        let live = HashMap::from([
            ("FXCM_MAINNET_USER".to_string(), "u".to_string()),
            ("FXCM_MAINNET_PASSWORD".to_string(), "p".to_string()),
        ]);
        let real = settings("live", &[("connection", "Demo")]);
        assert_eq!(
            load_fxcm_config_from(Environment::Live, &live, &real).unwrap().connection,
            "Demo"
        );

        // A padded row reads trimmed and a whitespace-only one reads as no row — `vike-cli config
        // set` and the move copy a value as written.
        let padded = settings(
            "demo",
            &[("url", " http://elsewhere.example/Hosts.jsp "), ("connection", " Real ")],
        );
        let c = load_fxcm_config_from(Environment::Demo, &vars, &padded).unwrap();
        assert_eq!(
            (c.url.as_str(), c.connection.as_str()),
            ("http://elsewhere.example/Hosts.jsp", "Real")
        );
        let blank = settings("demo", &[("url", "   "), ("connection", " ")]);
        let c = load_fxcm_config_from(Environment::Demo, &vars, &blank).unwrap();
        assert_eq!((c.url.as_str(), c.connection.as_str()), (DEFAULT_HOST_URL, "Demo"));

        // The two fields are TIER-scoped: a machine-scoped (`any`) row is not this tier's value.
        let any = VenueSettings::from_rows(
            "fxcm",
            &[vike_secrets::VenueSettingRow {
                venue: "fxcm".to_string(),
                tier: None,
                field: "CONNECTION".to_string(),
                value: "Real".to_string(),
            }],
        );
        assert_eq!(
            load_fxcm_config_from(Environment::Demo, &vars, &any).unwrap().connection,
            "Demo"
        );
    }

    /// **The 2×2 that keeps [`load_fxcm_config_for_account`] account-aware** — the twin of
    /// `crates/vike-tradehub/tests/account_credential_isolation.rs`, which cannot reach this venue
    /// (vike-mount's registry carries it `FeatureAbsent`); its 2×2 through the mount's decision is
    /// `a_labelled_account_reads_only_its_own_keys` in `crates/bridges/fxcm/src/mount_tests.rs`.
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
        let (user_k, pass_k) = fxcm_env_var_names(Environment::Demo);
        let default_only = HashMap::from([
            (user_k.clone(), "first".to_string()),
            (pass_k.clone(), "pw1".to_string()),
        ]);
        let labelled_only = HashMap::from([
            (account_key(&user_k, &alt), "second".to_string()),
            (account_key(&pass_k, &alt), "pw2".to_string()),
        ]);
        let n = none();

        // THE mutation detector: the default account's keys configure the default account ONLY.
        assert_eq!(
            load_fxcm_config_for_account(
                Environment::Demo,
                &AccountLabel::Default,
                &default_only,
                &n
            )
            .map(|c| c.user),
            Some("first".to_string())
        );
        assert!(
            load_fxcm_config_for_account(Environment::Demo, &alt, &default_only, &n).is_none(),
            "a labelled account must NOT borrow the default account's FX login"
        );

        // …and the mirror, which catches a loader that appended the label unconditionally.
        assert_eq!(
            load_fxcm_config_for_account(Environment::Demo, &alt, &labelled_only, &n)
                .map(|c| c.user),
            Some("second".to_string())
        );
        assert!(
            load_fxcm_config_for_account(
                Environment::Demo,
                &AccountLabel::Default,
                &labelled_only,
                &n
            )
            .is_none()
        );
    }

    /// **BYTE-IDENTITY**: with both accounts in one store the DEFAULT account resolves exactly what
    /// it resolves alone — an EQUALITY, so nothing here pins today's answer.
    ///
    /// ⚠ The URL and the connection are no longer an account's to carry: since decision 0095's
    /// Task 7 both are the TIER's `venue_setting` rows (ruling 10), so a labelled account reads the
    /// same host and connection the default account does, and nothing a second account's
    /// credentials hold can move either.
    #[test]
    fn a_second_account_changes_nothing_about_the_first() {
        let alt = AccountLabel::parse("ALT").expect("a legal label");
        let (user_k, pass_k) = fxcm_env_var_names(Environment::Demo);
        let alone = HashMap::from([
            (user_k.clone(), "first".to_string()),
            (pass_k.clone(), "pw1".to_string()),
        ]);
        let mut beside = alone.clone();
        for (k, v) in [(&user_k, "second"), (&pass_k, "pw2")] {
            beside.insert(account_key(k, &alt), v.to_string());
        }
        let tier = settings(
            "demo",
            &[("connection", "Real"), ("url", "http://elsewhere.example/Hosts.jsp")],
        );

        let a = load_fxcm_config_from(Environment::Demo, &alone, &tier).expect("configured");
        let b = load_fxcm_config_from(Environment::Demo, &beside, &tier).expect("configured");
        assert_eq!(
            (a.user, a.password, a.url.clone(), a.connection.clone()),
            (b.user, b.password, b.url, b.connection)
        );

        // …and the labelled account reads its own login with the TIER's host and connection.
        let c = load_fxcm_config_for_account(Environment::Demo, &alt, &beside, &tier)
            .expect("configured");
        assert_eq!(c.user, "second");
        assert_eq!((c.url, c.connection), (a.url, a.connection));
    }
}
