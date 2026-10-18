//! **Which NAMES must never have their value printed** — ONE classifier for every disclosure surface.
//!
//! ONE rule table, [`SECRET_RULES`], and two thin entry points over it with DIFFERENT meanings:
//!
//! - [`matches_secret_rule`] is the STRICT predicate: the name matches a row (case-insensitively,
//!   whole name uppercased first), nothing excused. Use it where over-masking is harmless and a
//!   miss leaks — `vike-tradehub-core`'s incident bundle, whose environment dump is pasted into issues.
//! - [`is_secret`] / [`is_secret_key`] are the same predicate MINUS the few non-credentials
//!   [`NOT_SECRET`] excuses by exact name. Use them where wrongly masking a real knob breaks the
//!   operator: `config show` and [`crate::boot`] (a settings value), [`crate::write`]'s credential
//!   fence (a settings key), the tradehub's settings-write audit, and the MCP agent transcript
//!   (argument names and the identifiers inside a string). [`is_secret`] takes an
//!   environment-variable / registry name; [`is_secret_key`] takes a dotted settings key and asks
//!   about its leaf.
//!
//! Every exemption is therefore a name the strict predicate STILL masks
//! (`every_excused_name_is_still_masked_by_the_strict_predicate`), so a surface on the strict side
//! masks everything it ever did. Nothing else in the workspace decides what is credential-shaped.
//!
//! A second copy of a SECURITY table is the one kind of duplication this tree cannot afford — a
//! venue added to one list and not the other prints a live key into a log file — and there WERE
//! two: this crate's suffix table and the incident bundle's substring list disagreed in both
//! directions (`FXCM_DEMO_USER` printed verbatim in a bundle, `ACME_LIVE_PK` not caught here).
//! [`SECRET_RULES`] is their UNION, and the monotonicity tests in `redact_tests.rs` carry both old
//! lists verbatim: [`matches_secret_rule`] must mask every name either one masked, and
//! [`is_secret`] every such name except the [`NOT_SECRET`] ones, which the tests name explicitly.
//!
//! The test that matters is still `vike-cli`'s: `no_settings_row_leaks` runs these shapes over the
//! REAL `vike_config::settings::SETTINGS` registry. (The registry sat in `vike-ops`, which this crate
//! could not see, until 2026-10-12; it is a sibling module now.) `redact_tests.rs` adds the other
//! half: the exact set of registry names that classify as secret is pinned to a list, so a new
//! setting whose name happens to contain a needle fails loudly and is decided on purpose.
//!
//! …and the two WORDS a redacted row prints ([`SET`] / [`UNSET`]) live here for the same reason the
//! shapes do. They are the vocabulary of that decision, not a second table — an operator comparing
//! a `config show` row against a daemon's boot block is reading one fact, and two surfaces that
//! spelled it differently would make the same value look like two.

/// How a [`Rule`]'s needle is matched against the UPPERCASED name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// The needle appears ANYWHERE in the name (`ACME_TOKEN_FILE`, `WALLET_SEED_PHRASE`). The
    /// widest shape, so a needle takes it only when a credential genuinely hides in the middle.
    Anywhere,
    /// The name ENDS with the needle (`ACME_DEMO_USER`) or IS the needle minus its leading
    /// underscore (a setting named exactly `USER`). Narrower than [`Shape::Anywhere`] on purpose:
    /// `USERNAME`, `USERPROFILE`, `SUDO_USER_HOME` and `LOGIN_URL` are not credentials.
    SuffixOrBare,
    /// The name IS the needle and nothing else.
    Whole,
}

/// One row of [`SECRET_RULES`]: a needle, how it is matched, and why it is there.
#[derive(Debug, Clone, Copy)]
pub struct Rule {
    /// How [`Rule::needle`] is matched.
    pub shape: Shape,
    /// SHOUTING_CASE text. A [`Shape::SuffixOrBare`] needle starts with `_`.
    pub needle: &'static str,
    /// The credential this row exists for, with a real example. A row with no example has no
    /// business in a security table.
    pub why: &'static str,
}

impl Rule {
    /// Whether this rule matches `upper`, a name that is ALREADY uppercased.
    pub fn matches(&self, upper: &str) -> bool {
        match self.shape {
            Shape::Anywhere => upper.contains(self.needle),
            Shape::SuffixOrBare => upper.ends_with(self.needle) || upper == &self.needle[1..],
            Shape::Whole => upper == self.needle,
        }
    }
}

/// **THE table.** Name shapes whose VALUE must never be printed: the union of this crate's former
/// suffix list and `vike-tradehub-core`'s former incident-bundle substring list, one row per needle.
///
/// The first group is every row of the old incident list (a substring match, kept as one: a needle
/// that hid a credential in the MIDDLE of a name there still does); the second is what only this
/// crate's old list caught (identifiers a venue logs in with, matched as a suffix because a
/// substring would swallow `USERNAME`/`USERPROFILE`); the third is bare names, which a row that needs
/// a leading underscore (`_KEY`, `_PK`) cannot match.
///
/// Over-redaction is the safe direction and is treated as free: a knob wrongly reported as `<set>`
/// costs one grep, while a leaked key costs a key rotation. The price of the substring rows is
/// that a plainly non-secret name containing a needle is swallowed too, and [`NOT_SECRET`] is the
/// one place such a name is excused, by exact name and with a reason.
pub const SECRET_RULES: &[Rule] = &[
    // ---- anywhere in the name -------------------------------------------------------------
    Rule {
        shape: Shape::Anywhere,
        needle: "SECRET",
        why: "`ACME_API_SECRET`, `CTRADER_CLIENT_SECRET` (OAuth2), polymarket's L2 `POLY_LIVE_SECRET`",
    },
    Rule {
        shape: Shape::Anywhere,
        needle: "PASSWORD",
        why: "`DUKASCOPY_PASSWORD`, `IBKR_LIVE_PASSWORD`",
    },
    Rule { shape: Shape::Anywhere, needle: "PASSWD", why: "the unix spelling, `DB_PASSWD`" },
    Rule {
        shape: Shape::Anywhere,
        needle: "PASSPHRASE",
        why: "`ACME_API_PASSPHRASE`, `POLY_PASSPHRASE` (the L2 trio's third leg), `VIKE_SECRETS_PASSPHRASE`",
    },
    Rule {
        shape: Shape::Anywhere,
        needle: "TOKEN",
        why: "`VIKE_TELEGRAM_BOT_TOKEN`, `GITHUB_TOKEN`, `CLAUDE_CODE_OAUTH_TOKEN`; also the bare `TOKEN`",
    },
    Rule {
        shape: Shape::Anywhere,
        needle: "PRIVATE",
        why: "`HYPERLIQUID_LIVE_PRIVATE_KEY`, `ACME_PRIVATE_KEY_PATH`",
    },
    Rule {
        shape: Shape::Anywhere,
        needle: "PRIVKEY",
        why: "the short spelling of a private key, `ACME_PRIVKEY`",
    },
    Rule {
        shape: Shape::Anywhere,
        needle: "MNEMONIC",
        why: "a wallet recovery phrase, `ACME_WALLET_MNEMONIC`",
    },
    Rule {
        shape: Shape::Anywhere,
        needle: "SEED",
        why: "a wallet seed phrase, `ACME_WALLET_SEED`",
    },
    Rule {
        shape: Shape::Anywhere,
        needle: "SIGNATURE",
        why: "`POLY_SIGNATURE`, a pre-signed authorization blob",
    },
    Rule {
        shape: Shape::Anywhere,
        needle: "CREDENTIAL",
        why: "`GOOGLE_APPLICATION_CREDENTIALS`, `ACME_CREDENTIALS`",
    },
    Rule {
        shape: Shape::Anywhere,
        needle: "APIKEY",
        why: "the run-together spelling, `ACME_APIKEY`",
    },
    Rule {
        shape: Shape::Anywhere,
        needle: "API_KEY",
        why: "`ANTHROPIC_API_KEY`, `DATABENTO_API_KEY`, `ACME_API_KEY_ID`",
    },
    Rule {
        shape: Shape::Anywhere,
        needle: "_KEY",
        why: "`VIKE_TRADEHUB_CONTROL_KEY` / `VIKE_TRADEHUB_OBSERVE_KEY`, the HMAC hub keys that grant \
               remote ORDER PLACEMENT on a running daemon: the single worst leak in the table",
    },
    Rule { shape: Shape::Anywhere, needle: "_PK", why: "polymarket's private key, `POLY_LIVE_PK`" },
    Rule {
        shape: Shape::Anywhere,
        needle: "WALLET",
        why: "wallet key material or its file, `ACME_WALLET_FILE` (an address sits in the same name \
              family; over-redaction is free)",
    },
    // ---- a suffix, or the whole name ------------------------------------------------------
    Rule {
        shape: Shape::SuffixOrBare,
        needle: "_USER",
        why: "venue LOGIN identifiers, `FXCM_DEMO_USER`; the same class as `_LOGIN`",
    },
    Rule {
        shape: Shape::SuffixOrBare,
        needle: "_LOGIN",
        why: "`DUKASCOPY_LOGIN`; bare `LOGIN` too",
    },
    // ---- the whole name -------------------------------------------------------------------
    Rule {
        shape: Shape::Whole,
        needle: "KEY",
        why: "a setting spelled exactly `key`: the `_KEY` row above needs the underscore, and \
               `key`/`setting_key` arguments are the MCP transcript's credential-shaped case",
    },
    Rule {
        shape: Shape::Whole,
        needle: "PK",
        why: "a setting spelled exactly `pk`, the private-key shorthand whose `_PK` row above needs \
               the underscore. Neither old table masked it (that was a gap, not a decision: \
               WIDENING only); whole-name only, so `PKG` and `PKG_CONFIG_PATH` stay unmasked",
    },
];

/// **The one place a name is excused**, by EXACT name (lower case, compared case-insensitively),
/// each with the reason its value is not a credential. These are names a [`SECRET_RULES`]
/// substring row swallows although their value is a flag, a count or a path; masking them would
/// print `<set>` for `VIKE_NO_LIVE_KEYS` in `config show` and REFUSE `config set
/// flags.allow_withdraw_keys` at the credential fence.
///
/// Spelled in lower case on purpose: the literal sweep in `vike-ops` reads an env-shaped
/// SHOUTING_CASE string in a `src/` file as a READ of that variable by this crate.
///
/// The excuses apply to [`is_secret`] / [`is_secret_key`] ONLY. [`matches_secret_rule`] ignores
/// this table, so the incident bundle's environment dump still masks every one of these names.
/// Adding a row unmasks a name on the `config show` / write-fence side, so it needs a reason a
/// reviewer can check against the registry or the `Flags` field. `redact_tests.rs` holds four
/// properties: each row is a name the registry declares or a settings key's leaf; each row would
/// otherwise be swallowed by a rule (a stale excuse fails); no row is a name this crate's former
/// suffix list masked; and the strict predicate still masks every row.
pub const NOT_SECRET: &[(&str, &str)] = &[
    (
        "vike_allow_withdraw_keys",
        "a boolean: a refused (removed) variable of the `flags.allow_withdraw_keys` row",
    ),
    ("allow_withdraw_keys", "the leaf of the `flags.allow_withdraw_keys` settings key: a boolean"),
    (
        "vike_datahub_chart_seed",
        "a boolean: a refused (removed) variable of the `flags.datahub_chart_seed` row",
    ),
    (
        "datahub_chart_seed",
        "the leaf of the `flags.datahub_chart_seed` settings key: a boolean (seed the chart with history, not an RNG or wallet seed)",
    ),
    (
        "vike_no_live_keys",
        "a boolean fence (`true` withholds the live accounts' credentials), not a credential",
    ),
    (
        "vike_credential_scope",
        "a refused (removed) variable; its value is never read, only its presence refused",
    ),
    ("vike_hold_tokens", "a test-only count of tokens to hold, not an auth token"),
    (
        "vike_strategy_builder_key_file",
        "a filesystem PATH to the key file; the key is inside the file",
    ),
    ("ctrader_token_file", "a refused (removed) variable naming a token FILE path"),
    ("poly_ws_tokens_per_socket", "a refused (removed) count of subscribed asset ids per socket"),
    (
        "vike_trade_seed",
        "a desktop QA hook, present-or-absent: seeds two PAPER orders in the Account window",
    ),
];

/// What a redacted row prints instead of its value: "a store or a file holds a non-empty value for
/// this". Everything else — unset, empty, or falling through to the documented default — is
/// [`UNSET`], because a secret's compile-time default is always "absent" (absent credentials ARE
/// the live gate).
pub const SET: &str = "<set>";
/// The other half of [`SET`]: nothing supplied a value.
pub const UNSET: &str = "<unset>";

/// What a redacted row prints in the DEFAULT column when the row somehow declares a non-empty
/// default. No row does today (every credential row's default is `""`), but a hardcoded credential
/// must not become printable by someone editing the table. Moved down with [`SET`]/[`UNSET`] (from
/// `crates/vike-cli/src/cmd/config.rs`, which still imports it for its env half) the day
/// [`crate::show`] joined the disclosure surfaces — same one-vocabulary argument as the other two
/// words.
pub const REDACTED: &str = "<redacted>";

/// **The STRICT predicate**: does ANY row of [`SECRET_RULES`] match this whole name? Case-insensitive,
/// nothing excused — [`NOT_SECRET`] is deliberately not consulted.
///
/// For a surface where over-masking is harmless and a miss leaks a credential: `vike-tradehub-core`'s
/// incident bundle masks its environment dump through this, so it masks every name it ever did
/// (`every_excused_name_is_still_masked_by_the_strict_predicate`). A surface where wrongly masking a
/// real knob breaks the operator (`config show`, the write fence) asks [`is_secret`] instead; do
/// not reach for this one there, and do not reach for [`is_secret`] in a dump nobody can re-run.
pub fn matches_secret_rule(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    SECRET_RULES.iter().any(|rule| rule.matches(&upper))
}

/// Whether a NAME holds a credential, so its value must be masked: [`matches_secret_rule`] minus the
/// few names [`NOT_SECRET`] excuses. By name shape alone, deliberately not by a hand-maintained
/// allowlist, so a venue added tomorrow is masked by construction. Case-insensitive; the argument is
/// a whole environment-variable or registry name (a dotted settings key goes through
/// [`is_secret_key`], which takes its leaf).
pub fn is_secret(name: &str) -> bool {
    matches_secret_rule(name) && !is_excused(name)
}

/// Whether [`NOT_SECRET`] excuses this exact name.
fn is_excused(name: &str) -> bool {
    NOT_SECRET.iter().any(|(excused, _)| excused.eq_ignore_ascii_case(name))
}

/// [`is_secret`], applied to a dotted TOML key's LEAF segment: `config.log_dir` -> `log_dir`.
///
/// No settings field is credential-shaped today — credentials live in the store, not in these
/// files — so this is pure insurance, and both surfaces assert that emptiness rather than assuming
/// it. It is worth having anyway: these are settings DISCLOSURE surfaces, a `bot_token` field is
/// one PR away, and "remember to redact it" is not a mechanism. [`is_secret`] folds the case, so a
/// snake_case TOML key meets the environment's SHOUTING_CASE table without a conversion here.
pub fn is_secret_key(key: &str) -> bool {
    let leaf = key.rsplit('.').next().unwrap_or(key);
    is_secret(leaf)
}

#[path = "redact_tests.rs"]
#[cfg(test)]
mod redact_tests;
