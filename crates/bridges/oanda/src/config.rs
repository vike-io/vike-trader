//! OANDA v20 login config from the gitignored `.env`.
//!
//! OANDA auths with a personal API token (Bearer) + an account id — not the HMAC
//! [`Credentials`](vike_bridge_core::credentials::Credentials) shape. The token never reaches
//! Debug/Display. Absent token or account id → `None` (the live gate).

use std::collections::HashMap;
use vike_bridge_core::credentials::{account_var, Environment};
use vike_model::account_keys::{account_key, AccountLabel};

/// OANDA REST + streaming hosts for an environment. Demo = fxPractice, Live = fxTrade.
pub fn oanda_hosts(env: Environment) -> (&'static str, &'static str) {
    match env {
        Environment::Live => ("https://api-fxtrade.oanda.com", "https://stream-fxtrade.oanda.com"),
        _ => ("https://api-fxpractice.oanda.com", "https://stream-fxpractice.oanda.com"),
    }
}

/// ForexConnect-free v20 session parameters.
#[derive(Clone)]
pub struct OandaConfig {
    pub api_token: String,
    pub account_id: String,
    pub rest_base: String,
    pub stream_base: String,
}

impl std::fmt::Debug for OandaConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // never leak the token
        write!(f, "OandaConfig(account_id={}, rest_base={})", self.account_id, self.rest_base)
    }
}

/// The `(api_key, account_id)` variable names for ONE tier STRING — the single composition site.
/// [`oanda_env_var_names`] (the public, `Environment`-keyed spelling), [`load_oanda_tier`] (which
/// also needs the LEGACY tier string, which no `Environment` spells) and
/// [`live_tier_var_names`] all fold through it, so a rename cannot leave one of them reading the
/// old names while the others read the new ones.
fn tier_var_names(tier: &str) -> (String, String) {
    let prefix = format!("OANDA_{tier}");
    (format!("{prefix}_API_KEY"), format!("{prefix}_ACCOUNT_ID"))
}

/// The `(api_key, account_id)` env-var names for OANDA at `env`
/// (e.g. `OANDA_DEMO_API_KEY`, `OANDA_DEMO_ACCOUNT_ID`).
pub fn oanda_env_var_names(env: Environment) -> (String, String) {
    tier_var_names(env.as_str())
}

/// Read OANDA config from a var map (process env or a parsed `.env`). `None` when the token or
/// account id is unset/blank — the live gate.
pub fn load_oanda_config_from(
    env: Environment,
    vars: &HashMap<String, String>,
) -> Option<OandaConfig> {
    load_oanda_config_for_account(env, &AccountLabel::Default, vars)
}

/// [`load_oanda_config_from`] for ONE NAMED ACCOUNT — `OANDA_{TIER}_{SUFFIX}__{LABEL}`.
///
/// Everything [`load_oanda_config_from`] documents holds word for word, the LEGACY-tier fallback
/// included; the ONLY difference is the NAMES read, composed by
/// `vike_bridge_core::credentials::account_var`, which appends the label after the WHOLE of today's
/// key. `OANDA_DEMO_ACCOUNT_ID` is **the** key that forced the label to the end of the grammar —
/// under the rejected `{VENUE}_{TIER}_{LABEL}{SUFFIX}` spelling it would read as *label `ACCOUNT`,
/// suffix `_ID`* (`vike_model::account_keys`) — so it is the one worth naming here.
///
/// ⚠ **[`AccountLabel::Default`] is byte-identically [`load_oanda_config_from`]**, reached through
/// it.
///
/// ⚠ **No fallback to the unlabelled key**, and the `_ACCOUNT_ID` half is why: OANDA's v20 REST
/// paths embed the account id, so a labelled account borrowing the default one's would place
/// `ALT`'s orders in the FIRST account even under a distinct token.
pub fn load_oanda_config_for_account(
    env: Environment,
    label: &AccountLabel,
    vars: &HashMap<String, String>,
) -> Option<OandaConfig> {
    load_oanda_tier(env, env.as_str(), label, vars)
        .or_else(|| env.legacy_str().and_then(|t| load_oanda_tier(env, t, label, vars)))
}

fn load_oanda_tier(
    env: Environment,
    tier: &str,
    label: &AccountLabel,
    vars: &HashMap<String, String>,
) -> Option<OandaConfig> {
    let (key_k, acct_k) = tier_var_names(tier);
    let get = |k: &str| account_var(vars, k, label).map(str::to_string).unwrap_or_default();

    let api_token = get(&key_k);
    let account_id = get(&acct_k);
    if api_token.is_empty() || account_id.is_empty() {
        return None;
    }

    let (rest_base, stream_base) = oanda_hosts(env);
    Some(OandaConfig {
        api_token,
        account_id,
        rest_base: rest_base.to_string(),
        stream_base: stream_base.to_string(),
    })
}

/// Every LIVE-named OANDA variable, composed from the SAME tier strings [`load_oanda_config_from`]
/// walks for [`Environment::Live`]: the current `LIVE` pair, then the `MAINNET` pair
/// [`Environment::legacy_str`] still accepts. Derived rather than written out, so a tier rename
/// moves the loader and this probe together.
fn live_tier_var_names() -> Vec<String> {
    let mut tiers = vec![Environment::Live.as_str()];
    tiers.extend(Environment::Live.legacy_str());
    tiers
        .into_iter()
        .flat_map(|t| {
            let (key, acct) = tier_var_names(t);
            [key, acct]
        })
        .collect()
}

/// **A LIVE-named key set in the store that no code path can select** — the evidence behind
/// [`MountableTier::LiveUnreachable`], and an ERROR rather than an absence: an ABSENT credential is
/// the ordinary unconfigured state and is silent, while one that is PRESENT and unusable must be
/// said out loud (the same class, and the same disposition, as
/// `vike_bridge_core::credentials::missing_required_passphrase`).
///
/// Carries the variable NAMES that were found and NOTHING else. An OANDA API token is a bearer
/// credential, so no value reaches this type, its derived `Debug`, or the line [`Display`] renders
/// — the same rule [`OandaConfig`]'s hand-written `Debug` keeps.
///
/// [`Display`]: std::fmt::Display
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreachableLiveTier {
    /// The LIVE-named variables actually present and non-blank, in the order the loader would try
    /// their tiers. Never empty — [`mountable_tier`] builds this only when it found at least one.
    pub names: Vec<String>,
}

impl std::fmt::Display for UnreachableLiveTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "oanda: {} present, but this build can reach NO fxTrade tier — `oanda_hosts` maps \
             Environment::Live onto the live hosts and nothing in the workspace ever asks \
             `load_oanda_config_from` for that tier, so a live-named key set selects nothing. \
             OANDA therefore stays PAPER, and deliberately does NOT fall through to the practice \
             tier: trading fxPractice under a live-armed store is exactly the silent ignore this \
             refusal exists to prevent. To trade the PRACTICE account, put a practice token and \
             account id under {} / {} in <project>/settings/secrets.env and remove the variables \
             named above; there is no supported way to trade an fxTrade account from this build.",
            self.names.join(", "),
            oanda_env_var_names(Environment::Demo).0,
            oanda_env_var_names(Environment::Demo).1,
        )
    }
}

/// What an OANDA mount may actually do with the credential store it was handed — the ONE decision,
/// so `vike_mount::make_engine`'s oanda arm and its `would_mount_live` probe row cannot drift about
/// which tier is reachable.
#[derive(Debug)]
pub enum MountableTier {
    /// The practice (fxPractice) tier, resolved from `OANDA_DEMO_*`. The only tier with a caller.
    Practice(OandaConfig),
    /// The store names a live (fxTrade) key set. REFUSED — see [`UnreachableLiveTier`].
    LiveUnreachable(UnreachableLiveTier),
    /// Nothing configured. The ordinary unconfigured state: absent credentials ARE the live gate,
    /// and an absence is reported by staying paper, silently.
    Unconfigured,
}

/// Resolve the tier an OANDA mount may use from a var map (the workspace credential store).
///
/// ⚠ **A live-named key set WINS over a present practice one, and the win is the point.** The
/// naive order — try `Demo`, fall back — is what made this venue configured-and-inert: an operator
/// who wrote `OANDA_LIVE_*` got a silent paper mount with nothing but their own memory to say the
/// keys were ignored, and one who wrote BOTH tiers got real orders on the practice account while
/// believing they were live. Refusing first makes the ignore impossible in both stores.
///
/// The refusal fires on ANY non-blank live-named variable, including HALF a pair: half a live key
/// set is a half-written LIVE intent, not an absence, and reading it as "unconfigured" is the same
/// mistake wearing a smaller hat.
pub fn mountable_tier(vars: &HashMap<String, String>) -> MountableTier {
    mountable_tier_for_account(&AccountLabel::Default, vars)
}

/// [`mountable_tier`] for ONE NAMED ACCOUNT.
///
/// ⚠ **The live-unreachable REFUSAL is scoped to the account too, and that is the load-bearing
/// half.** The names probed are `account_key(base, label)` of the same `live_tier_var_names`, so a
/// labelled account is refused for ITS OWN `OANDA_LIVE_API_KEY__ALT` and not for the default
/// account's `OANDA_LIVE_API_KEY` — and, symmetrically, a default-account box is not refused
/// because somebody else wrote a labelled live key. Probing the unlabelled names for every account
/// would make one operator's live-named key set disarm every practice account they own, which is
/// the mirror-image of the silent-ignore this refusal exists to prevent.
///
/// [`AccountLabel::Default`] is [`mountable_tier`], reached through it — the same names, since
/// `account_key` returns its input unchanged for that account.
pub fn mountable_tier_for_account(
    label: &AccountLabel,
    vars: &HashMap<String, String>,
) -> MountableTier {
    let names: Vec<String> = live_tier_var_names()
        .into_iter()
        .map(|k| account_key(&k, label))
        .filter(|k| vars.get(k).is_some_and(|v| !v.trim().is_empty()))
        .collect();
    if !names.is_empty() {
        return MountableTier::LiveUnreachable(UnreachableLiveTier { names });
    }
    match load_oanda_config_for_account(Environment::Demo, label, vars) {
        Some(cfg) => MountableTier::Practice(cfg),
        None => MountableTier::Unconfigured,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_hosts_and_no_token_leak() {
        let mut vars = HashMap::new();
        assert!(load_oanda_config_from(Environment::Demo, &vars).is_none());

        vars.insert("OANDA_DEMO_API_KEY".into(), "tok-abc-123".into());
        vars.insert("OANDA_DEMO_ACCOUNT_ID".into(), "101-004-1234567-001".into());
        let c = load_oanda_config_from(Environment::Demo, &vars).unwrap();
        assert_eq!(c.account_id, "101-004-1234567-001");
        assert!(c.rest_base.contains("fxpractice"));
        assert!(!format!("{c:?}").contains("tok-abc-123")); // token must not leak

        let (rest, stream) = oanda_hosts(Environment::Live);
        assert!(rest.contains("fxtrade") && stream.contains("fxtrade"));
    }

    /// **The LIVE-UNREACHABLE refusal is scoped to ONE account, in both directions.**
    ///
    /// [`mountable_tier`] refuses on ANY non-blank live-named variable, and that refusal is
    /// unusually destructive to get wrong: it disarms the venue outright. So the two mirror
    /// mistakes are asserted rather than argued —
    ///
    /// * a labelled account must be refused for its OWN `OANDA_LIVE_*__ALT` (an unscoped probe
    ///   would refuse it for somebody else's key, or fail to refuse it for its own);
    /// * a DEFAULT-account box must NOT be disarmed because a labelled live key exists beside it,
    ///   which is the direction an unscoped probe silently gets wrong and which no operator would
    ///   ever attribute to the second account they just added.
    #[test]
    fn the_live_unreachable_refusal_is_per_account() {
        let alt = AccountLabel::parse("ALT").expect("a legal label");
        // ⚠ COMPOSED, never spelled: `tier_var_names` is the venue's own naming site and
        // `account_key` is the grammar's. A fixture that wrote the labelled names out would prove
        // the rule against a COPY of them — and would plant an account separator in a bridge `src/`
        // literal, which `crates/vike-model/tests/account_keys.rs`'s
        // `no_existing_credential_key_contains_the_separator` sweeps for and refuses.
        let (demo_key, demo_acct) = tier_var_names(Environment::Demo.as_str());
        let (live_key, _) = tier_var_names(Environment::Live.as_str());
        let mut vars = HashMap::new();
        vars.insert(demo_key.clone(), "tok".to_string());
        vars.insert(demo_acct.clone(), "101-004-1-001".to_string());
        vars.insert(account_key(&demo_key, &alt), "tok2".to_string());
        vars.insert(account_key(&demo_acct, &alt), "101-004-2-002".to_string());
        // …and a LIVE-named key belonging to the LABELLED account only.
        let live_key_alt = account_key(&live_key, &alt);
        vars.insert(live_key_alt.clone(), "live-tok".to_string());

        assert!(
            matches!(mountable_tier_for_account(&alt, &vars), MountableTier::LiveUnreachable(u)
                if u.names == vec![live_key_alt.clone()]),
            "the labelled account is refused for its OWN live-named key, named as it is written"
        );
        assert!(
            matches!(mountable_tier(&vars), MountableTier::Practice(_)),
            "…and the DEFAULT account is untouched by its neighbour's live-named key"
        );

        // The mirror: a DEFAULT-account live key refuses the default account and not the labelled
        // one — so an unscoped probe cannot pass this test in either direction.
        let mut mirrored = vars.clone();
        mirrored.remove(&live_key_alt);
        mirrored.insert(live_key.clone(), "live-tok".to_string());
        assert!(matches!(mountable_tier(&mirrored), MountableTier::LiveUnreachable(_)));
        assert!(matches!(mountable_tier_for_account(&alt, &mirrored), MountableTier::Practice(_)));
    }
}
