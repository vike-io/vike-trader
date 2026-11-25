use super::*;

#[test]
fn unknown_field_message_yields_the_key() {
    let msg = "TOML parse error at line 2, column 1\nunknown field `max_leverag`, expected \
                   one of `max_leverage`, `rate`";
    assert_eq!(key_from_parse_message(msg).as_deref(), Some("max_leverag"));
}

#[test]
fn a_syntax_error_names_no_key() {
    let msg = "TOML parse error at line 1, column 5\nexpected `.`, `=`";
    assert_eq!(key_from_parse_message(msg), None);
}

#[test]
fn value_error_reads_as_the_canonical_sentence() {
    let e = ConfigError::value(
        std::path::Path::new("policy.toml"),
        "max_leverage",
        crate::refusals::ConfigRefusal::NotFinite { value: f64::INFINITY },
    );
    assert_eq!(e.to_string(), "policy.toml: max_leverage = inf is not a finite number");
}

/// A bounded integer row's refusal names the key, the value and BOTH bounds, in one sentence shape
/// every row shares (`ConfigError::out_of_range`).
#[test]
fn an_out_of_range_error_names_the_key_the_value_and_both_bounds() {
    let e = ConfigError::out_of_range(
        std::path::Path::new("config.toml"),
        "node_ack_wait_ms",
        499,
        500,
        30_000,
        "the wait is how long a write tool stalls",
    );
    assert_eq!(
        e.to_string(),
        "config.toml: node_ack_wait_ms = 499 is outside the allowed range 500..=30000 — the wait \
         is how long a write tool stalls"
    );
}
