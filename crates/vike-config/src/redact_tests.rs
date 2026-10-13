use super::*;

// NOTE the fixtures use INVENTED prefixes (`ACME_*`). `crates/vike-ops/tests/settings_secrets/settings_registry.rs`
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
