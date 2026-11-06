use super::Secret;

/// A value no other string in these tests contains, so finding it anywhere is unambiguous.
const VALUE: &str = "C0FFEE-THE-SECRET";

#[test]
fn debug_prints_the_redaction_and_never_the_value() {
    let s = Secret::new(VALUE.to_string());
    let shown = format!("{s:?}");
    assert_eq!(shown, "Secret(<redacted>)");
    assert!(!shown.contains(VALUE));
    // the alternate form is the same
    assert!(!format!("{s:#?}").contains(VALUE));
}

#[test]
fn an_optional_secret_stays_redacted() {
    let some: Option<Secret<String>> = Some(Secret::new(VALUE.to_string()));
    assert_eq!(format!("{some:?}"), "Some(Secret(<redacted>))");
    let none: Option<Secret<String>> = None;
    assert_eq!(format!("{none:?}"), "None");
}

#[test]
fn a_derived_debug_on_a_carrier_stays_redacted() {
    #[derive(Debug)]
    #[expect(dead_code, reason = "the fields are read only through `Debug`, which is the point")]
    struct Creds {
        user: String,
        key: Secret<String>,
        passphrase: Option<Secret<String>>,
    }
    let c = Creds {
        user: "alice".to_string(),
        key: Secret::new(VALUE.to_string()),
        passphrase: Some(Secret::from(VALUE.to_string())),
    };
    let shown = format!("{c:?}");
    assert_eq!(
        shown,
        "Creds { user: \"alice\", key: Secret(<redacted>), passphrase: Some(Secret(<redacted>)) }"
    );
    assert!(!format!("{c:#?}").contains(VALUE));
}

#[test]
fn a_t_without_debug_still_formats() {
    struct Opaque;
    let s = Secret::new(Opaque);
    assert_eq!(format!("{s:?}"), "Secret(<redacted>)");
}

#[test]
fn the_value_comes_back_only_through_the_two_named_doors() {
    let s = Secret::new(VALUE.to_string());
    assert_eq!(s.expose(), VALUE);
    let c = s.clone();
    assert_eq!(c.into_inner(), VALUE);
    assert_eq!(s.into_inner(), VALUE);
}

#[test]
fn equality_is_by_value() {
    assert_eq!(Secret::new(1_u8), Secret::from(1_u8));
    assert_ne!(Secret::new("a"), Secret::new("b"));
}
