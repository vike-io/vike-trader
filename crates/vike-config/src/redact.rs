//! **Which NAMES must never have their value printed** — one authority, two disclosure surfaces.
//!
//! `vike-cli config show` and [`crate::boot`] both render settings for a human to read and both
//! must hide a credential-shaped value. They used to be one surface, so the shapes lived in
//! `crates/vike-cli/src/cmd/config.rs`; the boot dump made that a second copy of a SECURITY table,
//! which is the one kind of duplication this tree cannot afford — a venue added to one list and not
//! the other prints a live key into a log file. So the table moved down here, to the crate both
//! surfaces already depend on, and `config show` imports it.
//!
//! The test that matters is still `vike-cli`'s: `no_settings_row_leaks` runs these shapes over the
//! REAL `vike_ops::settings::SETTINGS` registry, which this crate cannot see (`vike-ops` sits far
//! above it). This module owns the shapes; that gate owns the proof they are exhaustive.
//!
//! …and the two WORDS a redacted row prints ([`SET`] / [`UNSET`]) live here for the same reason the
//! shapes do. They are the vocabulary of that decision, not a second table — an operator comparing
//! a `config show` row against a daemon's boot block is reading one fact, and two surfaces that
//! spelled it differently would make the same value look like two.

/// Name shapes whose VALUE must never be printed. Matched as a suffix (`ACME_API_KEY` matches
/// `_API_KEY`) or as the whole name minus the leading underscore (a setting named exactly
/// `PASSWORD`).
///
/// The first seven are the credential shapes the store actually holds (see
/// `vike_bridge_core::credentials`, the naming authority). The rest are a deliberate WIDENING,
/// each justified by a real row in `vike_ops::settings::SETTINGS` that the seven miss:
///
/// - `_KEY` — `VIKE_TRADEHUB_CONTROL_KEY` / `VIKE_TRADEHUB_OBSERVE_KEY`, the HMAC node keys. These
///   grant remote ORDER PLACEMENT on a running daemon; they are the single worst leak in the table
///   and none of the seven shapes catches them.
/// - `_SECRET` — `ALPACA_SANDBOX_CLIENT_SECRET`, `CTRADER_CLIENT_SECRET`, `POLY_MAINNET_SECRET`
///   (OAuth2 / L2 secrets that are not spelled `_API_SECRET`).
/// - `_PASSPHRASE` — `POLY_PASSPHRASE` (the L2 trio's third leg).
/// - `_USER` — `FXCM_*_USER`, `ASTER_*_USER`: venue login identifiers, the same class as the
///   `_LOGIN` shape the brief already requires.
/// - `_SIGNATURE` — `POLY_SIGNATURE`, a pre-signed authorization blob.
///
/// Over-redaction is the safe direction and is treated as free: a knob wrongly reported as `<set>`
/// costs one grep, while a leaked key costs a key rotation. Checked in `vike-cli`'s
/// `no_settings_row_leaks` against the REAL registry, so a newly added credential row that these
/// shapes miss shows up as a test failure rather than in someone's pasted issue.
pub const SECRET_SUFFIXES: &[&str] = &[
    // The seven required credential shapes.
    "_API_KEY",
    "_API_SECRET",
    "_API_PASSPHRASE",
    "_TOKEN",
    "_PRIVATE_KEY",
    "_LOGIN",
    "_PASSWORD",
    // The widening (each subsumes one or more of the above; kept explicit as documentation).
    "_KEY",
    "_SECRET",
    "_PASSPHRASE",
    "_USER",
    "_SIGNATURE",
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

/// Whether a setting's value must be redacted, by NAME shape alone — deliberately not by a
/// hand-maintained allowlist, so a venue added tomorrow is redacted by construction.
pub fn is_secret(name: &str) -> bool {
    SECRET_SUFFIXES.iter().any(|&suffix| {
        // `_TOKEN` matches `VIKE_..._TOKEN`; the bare shape matches a whole name spelled `TOKEN`.
        let bare = &suffix[1..];
        name.ends_with(suffix) || name == bare
    })
}

/// The same shapes, applied to a dotted TOML key's LEAF segment: `config.log_dir` -> `LOG_DIR`.
///
/// No settings field is credential-shaped today — credentials live in the store, not in these
/// files — so this is pure insurance, and both surfaces assert that emptiness rather than assuming
/// it. It is worth having anyway: these are settings DISCLOSURE surfaces, a `bot_token` field is
/// one PR away, and "remember to redact it" is not a mechanism. Uppercased because [`is_secret`]
/// speaks the environment's SHOUTING_CASE while a TOML key is snake_case.
pub fn is_secret_key(key: &str) -> bool {
    let leaf = key.rsplit('.').next().unwrap_or(key);
    is_secret(&leaf.to_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    // NOTE the fixtures use INVENTED prefixes (`ACME_*`). `crates/vike-ops/tests/settings_registry.rs`
    // harvests every env-shaped string literal in a `src/` file and demands a `SETTINGS` row for
    // it, so a realistic `VIKE_*` fixture would fail that gate for a variable nothing reads.
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
}
