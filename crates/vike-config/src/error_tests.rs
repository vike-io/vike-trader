use super::*;

#[test]
fn unknown_field_message_yields_the_key() {
    let msg = "TOML parse error at line 2, column 1\nunknown field `max_leverag`, expected \
                   one of `max_leverage`, `rate`";
    assert_eq!(key_from_parse_message(msg).as_deref(), Some("max_leverag"));
}

/// THE redaction test: a credential written into the wrong TOML must not come back out in the
/// error that rejects it.
///
/// Driven through the real `toml` deserializer rather than a hand-built message, because the
/// thing being asserted is a property of that renderer: its snippet arm is what leaked, and a
/// fixture string would keep passing if the renderer changed.
#[test]
fn an_unknown_key_error_never_echoes_the_value_it_rejected() {
    #[derive(Debug, serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Owned {
        #[allow(dead_code)]
        known: Option<String>,
    }

    let text = "known = \"fine\"\napi_key = \"DUMMY-TOML-APIKEY-SHOULD-NEVER-PRINT\"\n";
    let error = toml::from_str::<Owned>(text).unwrap_err();
    // The unredacted renderer DOES carry it — the leak this guards is real, not hypothetical.
    assert!(error.to_string().contains("DUMMY-TOML-APIKEY-SHOULD-NEVER-PRINT"));

    let message = redacted_parse_message(text, error);
    assert!(!message.contains("DUMMY-TOML-APIKEY"), "the value leaked: {message}");
    // …while everything an operator needs to FIX it survives: where, and which key.
    assert!(message.contains("at line 2, column 1"), "{message}");
    assert!(message.contains("unknown field `api_key`"), "{message}");
    assert_eq!(key_from_parse_message(&message).as_deref(), Some("api_key"));
}

/// A location is 1-based, counts columns in CHARACTERS, and never panics on a multi-byte
/// character before the offset (the reason the walk is byte-indexed).
#[test]
fn a_location_is_one_based_and_character_counted() {
    assert_eq!(line_col("", 0), (1, 1));
    assert_eq!(line_col("abc", 0), (1, 1));
    assert_eq!(line_col("abc", 2), (1, 3));
    assert_eq!(line_col("a\nbc\nd", 5), (3, 1));
    // "é" is two bytes: the offset AFTER it is column 2, not column 3.
    assert_eq!(line_col("é=1", 2), (1, 2));
    // Past the end clamps rather than panicking.
    assert_eq!(line_col("ab", 99), (1, 3));
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
        "market_slippage",
        0.9,
        "exceeds the allowed maximum 0.05",
    );
    assert_eq!(
        e.to_string(),
        "policy.toml: market_slippage = 0.9 exceeds the allowed maximum 0.05"
    );
}

#[test]
fn env_error_names_the_variable_and_its_value() {
    let e = ConfigError::Env {
        var: "VIKE_RECONCILE".into(),
        value: "true".into(),
        message: "expected exactly \"1\" or \"0\"".into(),
    };
    assert!(e.to_string().starts_with("VIKE_RECONCILE=true: "));
}
