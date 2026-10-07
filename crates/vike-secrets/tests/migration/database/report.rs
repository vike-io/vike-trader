//! Proofs 12-14 - the report reconciles, the ugly store round-trips, the shadowed finding.

use super::*;

// ---------------------------------------------------------------------------------------------
// PROOF 12 — the report reconciles against a `grep -c`
// ---------------------------------------------------------------------------------------------

/// **A name in BOTH files with an identical value is counted once and NAMED.**
///
/// It is attributed to the node file, which is its home — so the credential file's `N key(s) read`
/// is lower than the number of `KEY=` lines that file holds, on precisely the half-migrated box
/// where an operator most wants to reconcile the report against a `grep -c`. Naming the doubly
/// claimed names closes the arithmetic without double-counting an insert.
#[test]
fn a_name_in_both_files_is_counted_once_and_named() {
    let fx = Fixture::empty();
    std::fs::write(
        fx.store(),
        "VIKE_TRADEHUB_OBSERVE_KEY=same\nBINANCE_DEMO_API_KEY=b\nOKX_DEMO_API_KEY=o\n",
    )
    .unwrap();
    std::fs::write(fx.node(), "VIKE_TRADEHUB_OBSERVE_KEY=same\n").unwrap();

    let report = fx.migrate();
    assert_eq!(report.doubly_claimed, vec!["VIKE_TRADEHUB_OBSERVE_KEY".to_string()]);

    let secrets_read: usize =
        report.sources.iter().filter(|s| s.file == fx.store()).map(|s| s.read).sum();
    let lines_in_file = std::fs::read_to_string(fx.store())
        .unwrap()
        .lines()
        .filter(|l| l.contains('=') && !l.trim_start().starts_with('#'))
        .count();
    assert_eq!(lines_in_file, 3);
    assert_eq!(secrets_read, 2, "the credential file's rows count the names it alone claimed");
    assert_eq!(
        secrets_read + report.doubly_claimed.len(),
        lines_in_file,
        "the report must reconcile against a `grep -c` of the credential file"
    );

    let said = report.to_string();
    assert!(said.contains("VIKE_TRADEHUB_OBSERVE_KEY"), "the name must be printed: {said}");
    assert!(!said.contains("=same"), "a report printed a credential VALUE: {said}");
}

// ---------------------------------------------------------------------------------------------
// PROOF 13 — the UGLY file still round-trips, and is byte-identical afterwards
// ---------------------------------------------------------------------------------------------

/// A credential file with every shape a hand-edited store actually takes: **equals signs inside a
/// value, an empty value, single- and double-quoted values, leading and trailing spaces, a DUPLICATE
/// line, CRLF line endings, a UTF-8 BOM, and no trailing newline.**
///
/// Every one of these is real. The BOM is what a Windows editor writes; CRLF is what a store edited
/// on the dev box and copied to a Linux daemon carries; the duplicate is one keystroke away
/// (`vike-cli secrets template >> settings/secrets.env`, the append typo of the documented `>`); an
/// equals sign inside a value is ordinary in a base64 secret; an empty value is a key somebody
/// cleared without deleting.
const UGLY_STORE: &[u8] =
    b"\xEF\xBB\xBF# a hand-edited store, in the shapes people really leave\r\n\
\r\n\
BINANCE_DEMO_API_KEY=plain\r\n\
BINANCE_DEMO_API_SECRET=has=equals=signs==\r\n\
BYBIT_DEMO_API_KEY=\r\n\
BYBIT_DEMO_API_SECRET=\"double quoted\"\r\n\
OKX_DEMO_API_KEY='single quoted'\r\n\
   OKX_DEMO_API_SECRET   =   spaced out   \r\n\
# the duplicate below is the `>>` typo, and LAST wins for every reader in this workspace\r\n\
OKX_DEMO_API_PASSPHRASE=first\r\n\
OKX_DEMO_API_PASSPHRASE=second\r\n\
CLOUDFLARE_API_TOKEN=no-trailing-newline";

/// **The ugly store round-trips through the database, and the file is byte-identical afterwards.**
///
/// Two claims in one test, deliberately: the migration must carry these values UNCHANGED (a store
/// whose values were trimmed, unquoted or re-encoded on the way in signs orders with the wrong
/// bytes), and it must not have touched the operator's only copy of their live venue keys while
/// doing it. The byte comparison is against the ORIGINAL bytes, so a BOM stripped, a CRLF
/// normalised or a duplicate line collapsed all fail.
///
/// ⚠ The expected VALUES are read through `parse_dotenv` — the one parser every loader in this
/// workspace uses — rather than spelled here. Spelling them would be a second opinion about what a
/// quoted or spaced line MEANS, and this test is about the migration preserving the parser's answer,
/// not about re-litigating the grammar.
#[test]
fn the_ugly_store_round_trips_and_stays_byte_identical() {
    let fx = Fixture::empty();
    std::fs::write(fx.store(), UGLY_STORE).expect("write the ugly store");
    let before = std::fs::read(fx.store()).expect("read");
    assert_eq!(before, UGLY_STORE, "the fixture must be on disk exactly as written");

    let expected = vike_secrets::parse_dotenv(&String::from_utf8_lossy(UGLY_STORE));
    assert!(expected.len() >= 7, "the fixture must exercise more than a couple of shapes");
    assert_eq!(
        expected.get("OKX_DEMO_API_PASSPHRASE").map(String::as_str),
        Some("second"),
        "the parser is LAST-wins over a duplicate, and the migration must carry that answer"
    );

    let report = fx.migrate();
    assert_eq!(report.outcome, vike_secrets::MigrationOutcome::Created);

    let resolved = vike_secrets::resolve_project(fx.arg()).expect("resolve_project");
    assert_eq!(resolved.source, Source::Database(fx.db()), "the database must answer");
    let got = resolved.secrets.clone().into_map();
    assert_eq!(
        got.into_iter().collect::<std::collections::BTreeMap<_, _>>(),
        expected.into_iter().collect::<std::collections::BTreeMap<_, _>>(),
        "a value changed on the way through the database"
    );

    // …and the file is untouched, down to the BOM, the CRLFs, the duplicate and the missing final
    // newline. A BYTE comparison against the original bytes, so nothing about this fixture can be
    // "tidied" without failing here — which is the rule that outranks everything else in this tree.
    assert_eq!(
        std::fs::read(fx.store()).expect("read"),
        before,
        "the credential file was modified by a migration that is supposed to READ it (digest now \
         {})",
        digest(&fx.store())
    );

    // A WRITE through the router must be just as careful on the file branch — the unmigrated twin,
    // on the same ugly bytes.
    let other = Fixture::empty();
    std::fs::write(other.store(), UGLY_STORE).expect("write");
    vike_secrets::save_credentials_to_store(
        other.dir(),
        Table::Credential,
        &[("BINANCE_DEMO_API_KEY".to_string(), "rotated".to_string())],
        Some(&classify),
    )
    .expect("write");
    let after = std::fs::read_to_string(other.store()).expect("read");
    // Every line the write was NOT asked to touch survives verbatim and in order — including the
    // comment, the blank line and the DUPLICATE.
    let untouched = |text: &str| {
        text.lines()
            .filter(|l| !l.contains("BINANCE_DEMO_API_KEY"))
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        untouched(&after),
        untouched(&String::from_utf8_lossy(UGLY_STORE)),
        "the upsert touched a line it was not asked to"
    );
    assert!(after.contains("BINANCE_DEMO_API_KEY=rotated"), "the write did not land: {after}");
    assert!(after.starts_with('\u{feff}'), "the BOM must survive a write");
    assert_eq!(
        vike_secrets::parse_dotenv(&after).get("OKX_DEMO_API_PASSPHRASE").map(String::as_str),
        Some("second"),
        "the duplicate must still be there, and still LAST-wins"
    );

    // ⚠ **MEASURED, and it is a PRE-EXISTING property of the file writer rather than of the
    // routing.** `vike_secrets::upsert_env` rebuilds the store through `str::lines()` and
    // `join("\n")`, and `lines()` strips a trailing `\r` — so a CRLF store comes back LF-ended, and
    // a store with no final newline gains one. Nothing here introduced that (the file branch is
    // `save_credentials` VERBATIM, unchanged), and this test does not "fix" it: the one sanctioned
    // write into the user's only copy of their live venue keys is not a thing to change as a
    // side effect of a routing change.
    //
    // It is PINNED rather than left unmeasured because the writer's own contract says it "leaves
    // every other line, comment, blank line and their ORDER byte-identical", and on a CRLF store
    // that is true of the CONTENT and not of the BYTES. If somebody makes the endings survive, this
    // assertion is what tells them the behaviour moved.
    assert!(
        !after.contains("\r\n"),
        "the file writer's CRLF normalisation has changed. That is very likely an IMPROVEMENT — \
         but it is a change to the one sanctioned write into the user's only copy of their live \
         venue keys, so it must be a deliberate edit with its own test, not a surprise here: \
         {after:?}"
    );
    assert_eq!(
        after.lines().count(),
        String::from_utf8_lossy(UGLY_STORE).lines().count(),
        "…and no line was added or dropped while the endings were rewritten"
    );

    // The DATABASE branch has no such residual, and that is worth stating rather than implying: on
    // a migrated box a rotation writes rows and never touches the file, so the operator's CRLF,
    // BOM and missing final newline are all still exactly where they were — which the byte
    // comparison at the top of this test already proved.
}

// ---------------------------------------------------------------------------------------------
// PROOF 14 — the SHADOWED finding is produced, so a consumer has something to print
// ---------------------------------------------------------------------------------------------

/// **A migrated project whose credential file is still on disk reports it as SHADOWED.**
///
/// `ShadowedStore` is returned as DATA by this crate, which carries no logging dependency — so the
/// half this test can hold is that the finding EXISTS and names both artifacts. That it is actually
/// PRINTED is held where the printing happens:
/// `vike_bridge_core::credentials::try_load_workspace_secrets_at` (the one site every composition
/// root's credential read converges on) and the two `vike-cli` verbs.
#[test]
fn a_migrated_project_reports_the_file_it_shadows() {
    let fx = Fixture::live_shaped();
    fx.migrate();

    let resolved = vike_secrets::resolve_project(fx.arg()).expect("resolve_project");
    let shadowed = resolved.shadowed.expect("the file is still on disk and is no longer read");
    assert_eq!(shadowed.file, fx.store());
    assert_eq!(shadowed.db, fx.db());
    let said = shadowed.to_string();
    assert!(said.contains("NO LONGER READ"), "{said}");
    assert!(said.contains("secrets.env"), "{said}");
    assert!(said.contains("vike.db"), "{said}");

    // Remove the file — an operator's own act, which nothing in this workspace performs — and the
    // finding goes away rather than becoming a complaint about a file that is not there.
    std::fs::remove_file(fx.store()).expect("the operator retires the file");
    let after = vike_secrets::resolve_project(fx.arg()).expect("resolve_project");
    assert!(after.shadowed.is_none(), "nothing is shadowed once the file is gone");
    assert_eq!(after.secrets.len(), 67, "…and the database still answers");
}
