use super::{Secret, key_tail};

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
    struct Credentials {
        user: String,
        key: Secret<String>,
        passphrase: Option<Secret<String>>,
    }
    let c = Credentials {
        user: "alice".to_string(),
        key: Secret::new(VALUE.to_string()),
        passphrase: Some(Secret::from(VALUE.to_string())),
    };
    let shown = format!("{c:?}");
    assert_eq!(
        shown,
        "Credentials { user: \"alice\", key: Secret(<redacted>), passphrase: Some(Secret(<redacted>)) }"
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

#[test]
fn a_blank_value_is_no_secret_and_a_held_one_is_trimmed() {
    assert!(Secret::non_blank("").is_none());
    assert!(Secret::non_blank("  \t ").is_none());
    assert_eq!(Secret::non_blank("  tok  ").expect("trimmed").expose(), "tok");
}

#[test]
fn redact_masks_every_occurrence_and_an_empty_value_masks_nothing() {
    let s = Secret::non_blank(VALUE).expect("non-blank");
    let masked = s.redact(&format!("bad key {VALUE}, again {VALUE}"));
    assert_eq!(masked, "bad key ***, again ***");
    // `Secret::new` can hold an empty value; masking it must not splice `***` between every char.
    assert_eq!(Secret::new(String::new()).redact("abc"), "abc");
}

/// `key_tail` keeps the last four CHARACTERS, never bytes: an ASCII key as the byte slice cut it, a
/// key holding multibyte characters on a character boundary (the byte slice panicked there: `é` and
/// `€` are two and three bytes), and fewer than four characters show nothing.
#[test]
fn key_tail_cuts_four_characters_and_never_panics_on_a_non_ascii_key() {
    assert_eq!(key_tail("abcdefgh"), "efgh");
    assert_eq!(key_tail("abcd"), "abcd");
    assert_eq!(key_tail("abc"), "");
    assert_eq!(key_tail(""), "");
    assert_eq!(key_tail("key-é123"), "é123");
    assert_eq!(key_tail("key-€€€€"), "€€€€");
    assert_eq!(key_tail("x€yz"), "x€yz");
    assert_eq!(key_tail("€€€"), "");
    // Every character boundary of a mixed key: never a panic, always four characters or none.
    let mixed = "aé€😀bcdé€😀";
    for end in mixed.char_indices().map(|(i, _)| i).chain([mixed.len()]) {
        let head = &mixed[..end];
        let tail = key_tail(head);
        let expected = if head.chars().count() >= 4 { 4 } else { 0 };
        assert_eq!(tail.chars().count(), expected, "{head:?}");
        assert!(head.ends_with(tail), "{head:?}");
    }
}
