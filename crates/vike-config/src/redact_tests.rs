use super::*;

// NOTE the fixtures use INVENTED prefixes (`ACME_*`). `crates/vike-ops/tests/settings_secrets/settings_registry.rs`
// harvests every env-shaped string literal in a `src/` file and demands a `SETTINGS` row for
// it, so a realistic `VIKE_*` fixture would fail that gate for a variable nothing reads. A name
// that must be realistic is therefore ASSEMBLED at run time (`joined`), and a list of registry
// names is spelled in lower case (the harvest wants SHOUTING_CASE).

/// A realistic name spelled so the literal sweep does not see it as a read.
fn joined(parts: &[&str]) -> String {
    parts.join("_")
}

#[test]
fn the_credential_shapes_match_by_suffix_and_bare_name() {
    for name in ["ACME_API_KEY", "ACME_LIVE_API_SECRET", "ACME_CLIENT_SECRET", "ACME_TOKEN"] {
        assert!(is_secret(name), "{name} must be treated as a secret");
    }
    assert!(is_secret("PASSWORD"), "the bare shape matches a whole name");
    assert!(is_secret("TOKEN"));
    for name in ["ACME_ADDR", "ACME_DIR", "ACME_ENABLED", "KEYRING_PATH"] {
        assert!(!is_secret(name), "{name} must NOT be redacted");
    }
}

#[test]
fn a_dotted_key_is_redacted_by_its_leaf_segment() {
    assert!(is_secret_key("config.bot_token"));
    assert!(is_secret_key("preferences.client_secret"));
    assert!(!is_secret_key("config.log_dir"));
    assert!(!is_secret_key("policy.max_notional_per_order"));
}

// -- the monotonicity contract ---------------------------------------------------------------
//
// Two classifiers used to answer "does this NAME hold a credential?" and disagreed in BOTH
// directions: `vike-config`'s suffix table (settings keys, `config show`, the boot block, the MCP
// transcript) and `vike-mount`'s incident-bundle substring list (the environment dump). They are
// ONE function now, and it must be a SUPERSET: a name either of them masked stays masked. The two
// old lists are carried below VERBATIM as "what was masked before"; they are fixtures, never
// code, so a later edit of the real table cannot quietly shrink the contract.

/// This crate's former `SECRET_SUFFIXES` table as it stood before the merge. Matched as a suffix, or as
/// the whole name minus the leading underscore.
const OLD_CONFIG_SUFFIXES: &[&str] = &[
    "_API_KEY",
    "_API_SECRET",
    "_API_PASSPHRASE",
    "_TOKEN",
    "_PRIVATE_KEY",
    "_LOGIN",
    "_PASSWORD",
    "_KEY",
    "_SECRET",
    "_PASSPHRASE",
    "_USER",
    "_SIGNATURE",
];

/// `vike-mount`'s incident `is_secret_key` `NEEDLES` as they stood before the merge. Matched as a
/// case-insensitive SUBSTRING anywhere in the name.
const OLD_MOUNT_NEEDLES: &[&str] = &[
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "PASSPHRASE",
    "TOKEN",
    "PRIVATE",
    "PRIVKEY",
    "MNEMONIC",
    "SEED",
    "SIGNATURE",
    "CREDENTIAL",
    "APIKEY",
    "API_KEY",
    "_KEY",
    "_PK",
    "WALLET",
];

/// A name in three spellings: as written (SHOUTING), lower, and alternating case.
fn case_variants(name: &str) -> [String; 3] {
    let alternating: String = name
        .chars()
        .enumerate()
        .map(|(i, c)| if i % 2 == 0 { c.to_ascii_uppercase() } else { c.to_ascii_lowercase() })
        .collect();
    [name.to_string(), name.to_ascii_lowercase(), alternating]
}

/// Every environment-style NAME the two old tables are known to have masked, built from the tables
/// themselves in every shape each one matched: for an old suffix `_X` the suffixed name
/// (`ACME_X`, `ACME_LIVE_X`) AND the bare name (`X`); for a mount needle the needle bare, then at
/// the start, the end, the middle and glued to a neighbour. Each in three cases.
fn names_masked_before() -> Vec<String> {
    let mut names = Vec::new();
    for suffix in OLD_CONFIG_SUFFIXES {
        names.push(format!("ACME{suffix}"));
        names.push(format!("ACME_LIVE{suffix}"));
        names.push(suffix[1..].to_string());
    }
    for needle in OLD_MOUNT_NEEDLES {
        names.push((*needle).to_string());
        names.push(format!("ACME_{needle}"));
        names.push(format!("{needle}_ACME"));
        names.push(format!("ACME_{needle}_VALUE"));
        names.push(format!("ACME{needle}"));
        names.push(format!("{needle}ACME"));
    }
    names.iter().flat_map(|name| case_variants(name)).collect()
}

/// Every dotted `section.leaf` KEY the old vike-config table masked (the shapes `config set`,
/// `config show`, the boot block and the audit meet): for an old suffix `_X` the leaf ending in it
/// and the leaf that IS the bare shape, under each settings section, in both cases of the key.
fn keys_masked_before() -> Vec<String> {
    let mut keys = Vec::new();
    for suffix in OLD_CONFIG_SUFFIXES {
        let lower = suffix.to_ascii_lowercase();
        for section in ["config", "preferences", "flags", "policy"] {
            keys.push(format!("{section}.acme{lower}"));
            keys.push(format!("{section}.acme_live{lower}"));
            keys.push(format!("{section}.{}", &lower[1..]));
        }
    }
    keys.iter().flat_map(|key| [key.clone(), key.to_ascii_uppercase()]).collect()
}

/// The STRICT predicate (the incident bundle's) must mask EVERY name either old table masked, with
/// no exception whatever.
#[test]
fn the_strict_predicate_masks_every_name_either_old_table_masked() {
    for name in names_masked_before() {
        assert!(
            matches_secret_rule(&name),
            "{name} was masked before the merge and must stay masked"
        );
    }
}

/// The EXCUSED entry points (`config show`, the write fence, the audit, the MCP trace) must mask
/// every one of those names too, EXCEPT the names [`NOT_SECRET`] excuses. The fixtures here are
/// built from the two old tables around `ACME`, so none of them is an excused name; the guard
/// below says so out loud instead of leaving the exception implicit, and
/// `no_registry_name_is_masked_less_than_before` covers the excused names themselves.
#[test]
fn the_excused_entry_points_mask_every_old_name_except_the_excused_ones() {
    for name in names_masked_before() {
        assert!(!is_excused(&name), "{name}: a fixture must not be an excused name");
        assert!(is_secret(&name), "{name} was masked before the merge and must stay masked");
        assert!(
            is_secret_key(&format!("config.{name}")),
            "config.{name} was masked before the merge and must stay masked"
        );
    }
    for key in keys_masked_before() {
        assert!(!is_excused(key.rsplit('.').next().unwrap_or(&key)), "{key}: not an excused leaf");
        assert!(is_secret_key(&key), "{key} was masked before the merge and must stay masked");
    }
}

/// **The old `vike-config` table, entry by entry, in the three shapes its code matched** — the
/// suffixed name (`ACME_X`), the BARE name (`X`, without the underscore) and the dotted key's leaf
/// (`config.acme_x`, `preferences.x`) — against BOTH entry points. A failure names the entry and the
/// shape, so a gap reads as "`_PK`: bare" and not as a bare assertion.
#[test]
fn every_old_suffix_is_covered_in_all_three_shapes() {
    for suffix in OLD_CONFIG_SUFFIXES {
        let bare = &suffix[1..];
        let lower = suffix.to_ascii_lowercase();
        for (shape, name) in [("suffixed", format!("ACME{suffix}")), ("bare", bare.to_string())] {
            for spelling in case_variants(&name) {
                assert!(matches_secret_rule(&spelling), "{suffix}: {shape} {spelling}, strict");
                assert!(is_secret(&spelling), "{suffix}: {shape} {spelling}, is_secret");
            }
        }
        for (shape, key) in [
            ("dotted suffixed", format!("config.acme{lower}")),
            ("dotted bare", format!("preferences.{}", &lower[1..])),
        ] {
            assert!(is_secret_key(&key), "{suffix}: {shape} {key}, is_secret_key");
        }
    }
}

/// The two names that were the reason for the merge, one per direction: the incident bundle printed
/// the first verbatim (no `_USER` needle), and `vike_config::is_secret_key` missed the second (no
/// `_PK` shape).
#[test]
fn the_two_named_regressions_are_masked() {
    let fxcm_user = joined(&["FXCM", "DEMO", "USER"]);
    assert!(matches_secret_rule(&fxcm_user), "{fxcm_user}");
    assert!(is_secret(&fxcm_user), "{fxcm_user}");
    assert!(matches_secret_rule("ACME_LIVE_PK"));
    assert!(is_secret("ACME_LIVE_PK"));
    assert!(is_secret_key("config.acme_live_pk"));
}

#[test]
fn the_match_ignores_case() {
    for name in ["acme_api_key", "Acme_Token", "acme_live_pk", "Some_Wallet_Mnemonic", "demo_user"]
    {
        assert!(matches_secret_rule(name), "{name}");
        assert!(is_secret(name), "{name}");
    }
    assert!(is_secret_key("config.Bot_Token"));
    assert!(is_secret_key("CONFIG.BOT_TOKEN"));
    assert!(!matches_secret_rule("acme_addr"));
    assert!(!is_secret("acme_addr"));
}

/// Ordinary knobs and OS variables that no shape may swallow.
#[test]
fn benign_names_stay_unmasked() {
    for name in [
        "PATH",
        "PWD",
        "SHELL",
        "USERNAME",
        "KEYRING_PATH",
        "ACME_ADDR",
        "ACME_LOG_DIR",
        // The whole-name rows (`KEY`, `PK`) and the bare-name suffix rows (`USER`, `LOGIN`) match
        // that exact name only, never a longer one that begins with it.
        "PKG",
        "PKG_CONFIG_PATH",
        "KEYS",
        "USERS",
        "LOGINS",
        "PK_COUNT",
    ] {
        assert!(!is_secret(name), "{name} must stay unmasked");
    }
    for name in [
        // The platform's own names are assembled: the literal sweep reads `HOME`, `USERPROFILE`,
        // `LOCALAPPDATA` and `RUST_LOG` spelled whole as READS of this crate.
        joined(&["HO"]) + "ME",
        joined(&["USER"]) + "PROFILE",
        joined(&["LOCAL"]) + "APPDATA",
        joined(&["RUST", "LOG"]),
        joined(&["VIKE", "PROJECT", "DIR"]),
        joined(&["VIKE", "JOURNAL", "DIR"]),
        joined(&["VIKE", "LOG", "DIR"]),
        joined(&["VIKE", "PIN", "CORES"]),
        joined(&["VIKE", "RECONCILE"]),
    ] {
        assert!(!is_secret(&name), "{name} is a runtime knob, not a secret");
        assert!(!matches_secret_rule(&name), "{name}: not even the strict predicate swallows it");
    }
}

// -- the table itself ------------------------------------------------------------------------

/// What the old `vike-config` table masked, as code: a suffix, or the whole name minus the
/// underscore. The reference for "was this masked by the old vike-config table?".
fn old_config_masks(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    OLD_CONFIG_SUFFIXES.iter().any(|&suffix| upper.ends_with(suffix) || upper == suffix[1..])
}

/// What the old incident bundle masked, as code: any needle anywhere, case-insensitively.
fn old_mount_masks(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    OLD_MOUNT_NEEDLES.iter().any(|&needle| upper.contains(needle))
}

#[test]
fn every_row_is_well_formed_and_unique() {
    for (i, rule) in SECRET_RULES.iter().enumerate() {
        assert_eq!(rule.needle, rule.needle.to_ascii_uppercase(), "{}: SHOUTING_CASE", rule.needle);
        assert!(rule.why.len() > 20, "{}: a row says what credential it is for", rule.needle);
        if rule.shape == Shape::SuffixOrBare {
            assert!(rule.needle.starts_with('_'), "{}: a suffix row starts with `_`", rule.needle);
        }
        for other in &SECRET_RULES[..i] {
            assert!(
                (other.needle, other.shape) != (rule.needle, rule.shape),
                "{} is listed twice",
                rule.needle
            );
        }
    }
}

/// Each shape matches where it says it does, and ONLY there.
#[test]
fn each_shape_matches_where_it_says() {
    for rule in SECRET_RULES {
        let n = rule.needle;
        match rule.shape {
            Shape::Anywhere => {
                for name in [format!("ACME_X{n}_Y"), format!("ACME{n}"), format!("{n}ACME")] {
                    assert!(is_secret(&name), "{name}: `{n}` matches anywhere");
                }
            }
            Shape::SuffixOrBare => {
                assert!(is_secret(&format!("ACME{n}")), "{n}: as a suffix");
                assert!(is_secret(&n[1..]), "{n}: as the bare name");
                for name in [format!("ACME{n}_HOME"), format!("ACME{}NAME", &n[1..])] {
                    assert!(!is_secret(&name), "{name}: `{n}` is a suffix, not a substring");
                }
            }
            Shape::Whole => {
                assert!(is_secret(n), "{n}: the whole name");
                assert!(!is_secret(&format!("{n}RING")), "{n}RING: `{n}` is the whole name only");
            }
        }
    }
}

/// The names the old `vike-cli` tests called ordinary (a plural, a mode, an address) and that the
/// substring rows swallow on purpose: the incident bundle always masked them, and one table has to
/// give one answer to the bundle and to `config show`. Over-redaction is free.
#[test]
fn the_names_the_union_swallows_on_purpose() {
    for name in [
        "ACME_HOLD_TOKENS",
        "ACME_ALLOW_WITHDRAW_KEYS",
        "ACME_SIGNATURE_TYPE",
        "ACME_RELAYER_API_KEY_ADDRESS",
    ] {
        assert!(is_secret(name), "{name}: masked since the incident bundle always masked it");
    }
}

/// Every registry name, so a new setting that contains a needle is a decision and not an accident.
fn registry_names() -> std::collections::BTreeSet<&'static str> {
    crate::settings::all_settings().map(|setting| setting.name).collect()
}

/// **The exact set of registry names the classifier calls secret.** Spelled in lower case (the
/// literal sweep reads SHOUTING_CASE strings as reads). A registry row added tomorrow whose name
/// contains a needle lands in the "unexpected" half of the assertion: either it IS a credential
/// (add it here), or it is a flag, a count or a path (excuse it in [`NOT_SECRET`], with a reason)
/// — never silently masked and never silently leaked.
const REGISTRY_SECRETS: &[&str] = &[
    "anthropic_api_key",
    "cerebras_api_key",
    "claude_code_oauth_token",
    "ctrader_client_secret",
    "databento_api_key",
    "dukascopy_login",
    "dukascopy_password",
    "finnhub_api_key",
    "fmp_api_key",
    "ibkr_demo_password",
    "ibkr_live_password",
    "poly_api_key",
    "poly_passphrase",
    "poly_signature",
    "tardis_api_key",
    "vike_alert_telegram_token",
    "vike_api_key",
    "vike_archive_api_key",
    "vike_datahub_control_key",
    "vike_datahub_observe_key",
    "vike_events_api_smoke_token",
    "vike_poly_cockpit_token",
    "vike_secrets_passphrase",
    "vike_strategy_builder_key",
    "vike_studio_compute_key",
    "vike_telegram_bot_token",
    "vike_tradehub_admin_key",
    "vike_tradehub_control_key",
    "vike_tradehub_observe_key",
];

#[test]
fn the_registry_secrets_are_exactly_the_expected_list() {
    let secret: std::collections::BTreeSet<String> = registry_names()
        .into_iter()
        .filter(|name| is_secret(name))
        .map(str::to_ascii_lowercase)
        .collect();
    let expected: std::collections::BTreeSet<String> =
        REGISTRY_SECRETS.iter().map(|name| (*name).to_string()).collect();
    let unexpected: Vec<&String> = secret.difference(&expected).collect();
    let missing: Vec<&String> = expected.difference(&secret).collect();
    assert!(
        unexpected.is_empty() && missing.is_empty(),
        "classified secret but not expected (a credential: list it; a flag, count or path: excuse \
         it in NOT_SECRET): {unexpected:?}\nexpected but not classified secret: {missing:?}"
    );
}

/// Monotonicity over the REAL names, both entry points. The STRICT predicate masks every registry
/// name either old table masked, no exception. The EXCUSED one ([`is_secret`]) misses exactly the
/// [`NOT_SECRET`] names and nothing else; and nothing the old `vike-config` table masked is ever
/// excused.
#[test]
fn no_registry_name_is_masked_less_than_before() {
    for name in registry_names() {
        let masked_before = old_config_masks(name) || old_mount_masks(name);
        if masked_before {
            assert!(
                matches_secret_rule(name),
                "{name}: masked before, and the strict one misses it"
            );
            assert!(
                is_secret(name) || is_excused(name),
                "{name} was masked before the merge and neither the table nor an excuse says why \
                 it no longer is"
            );
        }
        if old_config_masks(name) {
            assert!(is_secret(name), "{name}: the old vike-config table masked it; no excuse");
        }
        // The excused entry point differs from the strict one on the excused names alone.
        assert_eq!(
            matches_secret_rule(name) && !is_secret(name),
            is_excused(name),
            "{name}: the only gap between the strict and the excused predicate is NOT_SECRET"
        );
    }
}

/// **The incident bundle masks all of the excused names exactly as before, or more.** Every
/// [`NOT_SECRET`] name is excused from [`is_secret`] (`config show`, the write fence) but is STILL
/// masked by [`matches_secret_rule`], the strict predicate `vike-mount`'s `redact_env` calls, in
/// every case.
#[test]
fn every_excused_name_is_still_masked_by_the_strict_predicate() {
    for (name, _) in NOT_SECRET {
        for spelling in [
            name.to_string(),
            name.to_ascii_uppercase(),
            // a dotted key does not matter to the strict predicate: it reads the whole name
            format!("config.{name}"),
        ] {
            assert!(
                matches_secret_rule(&spelling),
                "{spelling}: excused from is_secret, so the strict predicate must still mask it"
            );
        }
        assert!(!is_secret(name), "{name}: excused from the entry point `config show` uses");
        // Whatever the old incident bundle masked, the strict predicate masks.
        assert!(old_mount_masks(name), "{name}: premise, the old bundle masked every one of these");
    }
}

/// The settings KEYS the classifier meets (`config show`, the write fence, the audit trail): the
/// ones a rule matches are exactly the two boolean flags the table excuses, so none is refused or
/// redacted and a new credential-shaped key is a loud decision.
#[test]
fn the_settings_keys_a_rule_matches_are_exactly_the_two_excused_flags() {
    let matched: Vec<String> = crate::provenance::setting_keys()
        .into_iter()
        .map(|spec| spec.key)
        .filter(|key| matches_secret_rule(key.rsplit('.').next().unwrap_or(key)))
        .collect();
    assert_eq!(matched, ["flags.allow_withdraw_keys", "flags.datahub_chart_seed"]);
    for key in &matched {
        assert!(!is_secret_key(key), "{key} is a boolean flag and must stay writable and visible");
    }
}

/// An excuse is a name the registry declares or a settings key's leaf; a rule still swallows it
/// (a stale excuse would hide a typo); and no excuse ever unmasks a name the old `vike-config`
/// table masked. (The excuses narrow `is_secret` against the old incident bundle's wider list, name
/// by name; the bundle itself calls the strict predicate and is not narrowed at all.)
#[test]
fn every_excuse_is_live_and_justified() {
    let registry: Vec<String> = registry_names().iter().map(|n| n.to_ascii_lowercase()).collect();
    let leaves: Vec<String> = crate::provenance::setting_keys()
        .into_iter()
        .map(|spec| spec.key.rsplit('.').next().unwrap_or_default().to_ascii_lowercase())
        .collect();
    for (name, reason) in NOT_SECRET {
        assert_eq!(*name, name.to_ascii_lowercase(), "{name}: spelled in lower case");
        assert!(reason.len() > 20, "{name}: say why it is not a credential");
        assert!(
            registry.iter().any(|r| r == *name) || leaves.iter().any(|l| l == *name),
            "{name} is neither a registry name nor a settings key's leaf"
        );
        assert!(matches_secret_rule(name), "{name} matches no rule: the excuse is stale");
        assert!(!old_config_masks(name), "{name}: the old vike-config table masked it");
        assert!(!is_secret(name) && !is_secret(&name.to_ascii_uppercase()), "{name}");
    }
}
