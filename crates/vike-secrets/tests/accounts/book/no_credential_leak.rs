//! PROOF 4 — nothing here is, or can print, a credential; plus the normalizer that decides what a book is.

use vike_secrets::DbErrorKind;

use super::fixture::{DEMO1_BOOK, FIXTURE_KEYS, Fixture};
use crate::support::fake_value;

// ---------------------------------------------------------------------------------------------
// PROOF 4 — nothing here is, or can print, a credential
// ---------------------------------------------------------------------------------------------

/// **A malformed book is refused and the refusal echoes NO token.**
///
/// The commonest way to reach this arm is pasting something into the wrong flag, and a refusal that
/// helpfully quoted the offending value would write a credential into the terminal scrollback of
/// the very session it exists to protect — the same reasoning as `vike-cli`'s `ARGV_VALUE_REFUSAL`.
#[test]
fn a_malformed_book_is_refused_and_the_refusal_echoes_no_token() {
    let fx = Fixture::migrated();
    let (first, _) = fx.dukascopy_ids();

    for bad in ["", "   ", "sk-live-abcdef", "two tokens", "line\nbreak", "tab\there"] {
        // `sk-live-abcdef` is a single token and IS accepted by the predicate — it is included here
        // to make the echo assertion below cover the one input an operator most regrets.
        let result = fx.set_book(first, bad, false);
        match result {
            Err(e) => {
                let text = e.to_string();
                assert!(
                    !text.contains(bad) || bad.trim().is_empty(),
                    "the refusal echoed the offered token: {text}"
                );
            }
            Ok(done) => {
                assert_eq!(
                    bad, "sk-live-abcdef",
                    "only the single-token case may be accepted; {bad:?} was"
                );
                // …and undo it, so the loop's later cases still see a row with a known book.
                assert!(done.changed);
                fx.set_book(first, DEMO1_BOOK, true).expect("restore");
            }
        }
    }

    // The refusal's own words, on a case that is unambiguously malformed.
    let err = fx.set_book(first, "two tokens", false).expect_err("two tokens is not one");
    assert!(matches!(err.kind, DbErrorKind::BookMalformed), "{:?}", err.kind);
    let text = err.to_string();
    assert!(text.contains("ONE token"), "{text}");
    assert!(!text.contains("two tokens"), "the refusal echoed the token: {text}");
}

/// **The write touches no credential, no file, and no other column of the row.**
///
/// Four claims in one test because they are one property: this is a targeted `UPDATE` of one column
/// and nothing else. `last_verified_at` in particular stays `None` — §4.5 gives that column to *a
/// successful authenticated session*, and an operator typing a number read off a web page has
/// performed none, so stamping it would make a hand-entered row indistinguishable from a confirmed
/// one.
#[test]
fn the_write_touches_no_credential_no_file_and_no_other_column() {
    let fx = Fixture::migrated();
    let (first, _) = fx.dukascopy_ids();
    let before_file = std::fs::read_to_string(fx.store()).unwrap();
    let before_row = fx.accounts().into_iter().find(|a| a.id == first).expect("the row");
    let before_keys: Vec<String> =
        vike_secrets::read_table(&fx.db(), vike_secrets::Table::Credential)
            .expect("the credential table")
            .keys()
            .map(str::to_string)
            .collect();

    let done = fx.set_book(first, DEMO1_BOOK, false).expect("the write lands");

    // 1. The credential file is byte-identical — this writer never opens it in any branch.
    assert_eq!(std::fs::read_to_string(fx.store()).unwrap(), before_file);

    // 2. Every credential name survives, and the map still answers.
    let after_keys: Vec<String> =
        vike_secrets::read_table(&fx.db(), vike_secrets::Table::Credential)
            .expect("the credential table")
            .keys()
            .map(str::to_string)
            .collect();
    assert_eq!(after_keys, before_keys, "a credential row moved");

    // 3. Only `venue_account_id` changed on the row.
    let after_row = fx.accounts().into_iter().find(|a| a.id == first).expect("the row");
    assert_eq!(after_row.venue, before_row.venue);
    assert_eq!(after_row.tier, before_row.tier);
    assert_eq!(after_row.label, before_row.label, "a label was written");
    assert_eq!(after_row.parent_id, before_row.parent_id);
    assert_eq!(after_row.active, before_row.active);
    assert_eq!(
        after_row.last_verified_at, None,
        "last_verified_at belongs to a successful authenticated SESSION, not to a hand-entered book"
    );
    assert_eq!(after_row.venue_account_id.as_deref(), Some(DEMO1_BOOK));

    // 4. Nothing the writer returns is a credential value.
    let rendered = format!("{done:?}");
    for key in FIXTURE_KEYS {
        assert!(
            !rendered.contains(&fake_value(key)),
            "a credential value reached the write's own Debug: {rendered}"
        );
    }
}

/// **The normalizer trims the paste and refuses everything else** — the ONE predicate the CLI's
/// early refusal and the store's write-path refusal both ask.
///
/// It is a pure function and it is tested as one, because everything above depends on it agreeing
/// with itself: `"4100017 "` and `"4100017"` must be the same book, or `account_one_account_per_book`
/// would be deciding a collision by an invisible byte.
#[test]
fn the_normalizer_trims_the_paste_and_refuses_everything_else() {
    use vike_secrets::normalized_venue_account_id as norm;

    assert_eq!(norm("4100017").as_deref(), Some("4100017"));
    assert_eq!(norm("  4100017 \n").as_deref(), Some("4100017"), "a pasted value is trimmed");
    assert_eq!(norm("DU186573").as_deref(), Some("DU186573"), "IBKR's shape");
    assert_eq!(
        norm("0x1234567890abcdef1234567890abcdef12345678").as_deref(),
        Some("0x1234567890abcdef1234567890abcdef12345678"),
        "an EVM address is the widest real shape"
    );

    for bad in ["", "   ", "\n", "two tokens", "a\tb", "a\nb", "with\u{0}nul"] {
        assert_eq!(norm(bad), None, "{bad:?} must be refused");
    }
    let too_long = "9".repeat(vike_secrets::VENUE_ACCOUNT_ID_MAX_BYTES + 1);
    assert_eq!(norm(&too_long), None, "a whole file pasted into the flag must not become a row");
    let at_the_cap = "9".repeat(vike_secrets::VENUE_ACCOUNT_ID_MAX_BYTES);
    assert!(norm(&at_the_cap).is_some(), "the cap itself is accepted");
}

/// **The INVISIBLE characters a paste carries — trimmed at the edges, REFUSED in the middle.**
///
/// ⚠ The predicate this file tested above used to be `is_whitespace() || is_control()`, and that
/// pair classifies **none of Unicode's FORMAT characters**: `char::is_control` is category Cc alone,
/// `is_whitespace` is the `White_Space` property, and `str::trim` strips only the second. So
/// U+FEFF (a byte-order mark), U+200B (a zero-width space) and U+200E (a left-to-right mark) all
/// passed, and a number pasted off a venue's own web page with a leading BOM was STORED with it.
///
/// That is not cosmetic here and is not a display bug. This column is the value
/// `account_one_account_per_book` compares two rows by, so `"\u{FEFF}4100017"` and `"4100017"`
/// render identically on every screen an operator can read and are DIFFERENT to the index: the one
/// rule standing between two accounts of one venue and one book would be defeated by a character
/// nobody can see, on the one venue where the two accounts are two legal entities.
///
/// Both halves are asserted, because they are different answers on purpose: an edge invisible is
/// what a paste actually produces and is trimmed (the operator gets the book they meant), while an
/// INTERIOR one is refused outright (there is no reading of it that is the operator's intent).
#[test]
fn an_invisible_character_is_trimmed_at_the_edge_and_refused_in_the_middle() {
    use vike_secrets::normalized_venue_account_id as norm;

    // Built from [`DEMO1_BOOK`] rather than re-typed: the number is one operator's account data and
    // this file already carries it once, as the subject.
    let edged = |lead: &str, trail: &str| format!("{lead}{DEMO1_BOOK}{trail}");
    let (head, tail) = DEMO1_BOOK.split_at(3);
    let inside = |c: &str| format!("{head}{c}{tail}");

    // The edge: the paste artefacts. Each must yield the clean book, byte-identical to the
    // hand-typed one — which is the property that keeps the index honest.
    for (raw, what) in [
        (edged("\u{FEFF}", ""), "a leading byte-order mark"),
        (edged("", "\u{FEFF}"), "a trailing byte-order mark"),
        (edged("\u{200B}", "\u{200B}"), "zero-width spaces at both ends"),
        (edged("\u{200E}", ""), "a left-to-right mark"),
        (edged("\u{00AD}", " "), "a soft hyphen and a space"),
        (edged("\u{2066}", "\u{2069}"), "a bidi isolate wrapping the number"),
    ] {
        assert_eq!(norm(&raw).as_deref(), Some(DEMO1_BOOK), "{what} must be trimmed away");
    }

    // The middle: no reading of this is the operator's intent, and storing it would make two books
    // that look the same compare unequal.
    for (raw, what) in [
        (inside("\u{FEFF}"), "a byte-order mark inside the number"),
        (inside("\u{200B}"), "a zero-width space inside the number"),
        (inside("\u{200E}"), "a left-to-right mark inside the number"),
    ] {
        assert_eq!(norm(&raw), None, "{what} must be REFUSED, never silently stored");
    }

    // …and the homoglyph case the ASCII rule closes for free: a Cyrillic `о` renders exactly like a
    // Latin `o` and is a different book to the index.
    assert_eq!(norm("DU18657\u{043E}"), None, "a Cyrillic letter must not pass for a Latin one");

    // ⚠ The measurement behind the whole test: the OLD predicate accepted every case above, edge
    // and interior alike — which is why `is_whitespace() || is_control()` is not the predicate any
    // more. `char::is_control` is category Cc alone and `is_whitespace` is `White_Space`; Cf is in
    // neither, and `str::trim` strips only the second.
    for raw in [edged("\u{FEFF}", ""), edged("", "\u{200B}"), inside("\u{200B}")] {
        assert!(
            !raw.trim().chars().any(|c| c.is_whitespace() || c.is_control()),
            "{raw:?} passes `trim` + is_whitespace/is_control"
        );
    }
}
