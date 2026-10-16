use super::*;

/// A blank or whitespace value is ABSENT, and a present one comes back trimmed — the rule every
/// loader applies, so a half-written field cannot arm anything.
#[test]
fn a_blank_field_reads_as_absent_and_a_value_comes_back_trimmed() {
    let fields = CredentialFields::from_pairs([("API_KEY", "  k1 "), ("API_SECRET", "   ")]);
    assert_eq!(fields.get("API_KEY"), Some("k1"));
    assert_eq!(fields.get("API_SECRET"), None);
    assert_eq!(fields.get("API_PASSPHRASE"), None);
    assert_eq!(fields.fields().collect::<Vec<_>>(), vec!["API_KEY"]);
    assert!(!fields.is_empty());
    assert!(CredentialFields::from_pairs([("X", " ")]).is_empty());
}

/// **The view reads the MOUNTED account's fields and no other account's** — two accounts of one
/// venue holding the same field never answer for each other, and an account with no row answers
/// nothing (the live gate).
#[test]
fn the_view_reads_only_its_own_account_and_its_venue() {
    let mut book = CredentialBook::new();
    book.insert_account(7, "API_KEY", "seven");
    book.insert_account(8, "API_KEY", "eight");
    book.insert_venue("ctrader", "CLIENT_ID", "app");
    let seven = book.view("binance", Some(7));
    assert_eq!(seven.get("API_KEY"), Some("seven"));
    assert_eq!(seven.account_id(), Some(7));
    assert_eq!(seven.venue_get("CLIENT_ID"), None, "binance holds no application row");
    assert_eq!(book.view("binance", Some(8)).get("API_KEY"), Some("eight"));
    let ctrader = book.view("ctrader", Some(9));
    assert_eq!(ctrader.get("API_KEY"), None, "account 9 holds nothing");
    assert_eq!(ctrader.venue_get("CLIENT_ID"), Some("app"));
    assert!(!book.view("binance", None).holds_any());
    assert_eq!(AccountCredentials::none().get("API_KEY"), None);
}

/// `missing` names the required fields the account lacks, in the order asked, and `holds_any`
/// tells a half-written set from an empty one.
#[test]
fn missing_names_the_absent_required_fields_in_order() {
    let fields = CredentialFields::from_pairs([("API_KEY", "k")]);
    let view = AccountCredentials::new(Some(1), Some(&fields), None);
    assert_eq!(
        view.missing(&["API_KEY", "API_SECRET", "API_PASSPHRASE"]),
        vec!["API_SECRET", "API_PASSPHRASE"]
    );
    assert!(view.holds_any());
}

/// **No value reaches `Debug`.** The derived `Debug`s print field NAMES and a redaction marker.
#[test]
fn no_value_reaches_debug() {
    let mut book = CredentialBook::new();
    book.insert_account(3, "API_SECRET", "hunter2-secret");
    book.insert_venue("ctrader", "CLIENT_SECRET", "app-secret");
    let printed = format!("{book:?} {:?}", book.view("ctrader", Some(3)));
    assert!(!printed.contains("hunter2-secret"), "{printed}");
    assert!(!printed.contains("app-secret"), "{printed}");
    assert!(printed.contains("API_SECRET"), "the field NAME is safe to print: {printed}");
}
