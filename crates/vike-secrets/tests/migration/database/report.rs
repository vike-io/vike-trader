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
/// ⚠ The expected VALUES are spelled here, as the grammar every `secrets.env` on a real box was
/// written in reads them: one layer of surrounding quotes stripped, the value trimmed, the first
/// `=` splitting, and LAST-wins over a duplicate. They were read through the store's own parser
/// until the credential file plane was removed (2026-10-07); that parser now lives inside the
/// migrate carry, crate-private, so this test pins what it must answer instead of asking it.
#[test]
fn the_ugly_store_round_trips_and_stays_byte_identical() {
    let fx = Fixture::empty();
    std::fs::write(fx.store(), UGLY_STORE).expect("write the ugly store");
    let before = std::fs::read(fx.store()).expect("read");
    assert_eq!(before, UGLY_STORE, "the fixture must be on disk exactly as written");

    let expected: std::collections::BTreeMap<String, String> = [
        ("BINANCE_DEMO_API_KEY", "plain"),
        ("BINANCE_DEMO_API_SECRET", "has=equals=signs=="),
        ("BYBIT_DEMO_API_KEY", ""),
        ("BYBIT_DEMO_API_SECRET", "double quoted"),
        ("OKX_DEMO_API_KEY", "single quoted"),
        ("OKX_DEMO_API_SECRET", "spaced out"),
        // LAST-wins over the duplicate, and the migration must carry that answer.
        ("OKX_DEMO_API_PASSPHRASE", "second"),
        ("CLOUDFLARE_API_TOKEN", "no-trailing-newline"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();

    let report = fx.migrate();
    assert_eq!(report.outcome, vike_secrets::MigrationOutcome::Created);

    let resolved = vike_secrets::resolve_project(fx.arg()).expect("resolve_project");
    assert_eq!(resolved.source, Source::Database(fx.db()), "the database must answer");
    let got = resolved.secrets.clone().into_map();
    assert_eq!(
        got.into_iter().collect::<std::collections::BTreeMap<_, _>>(),
        expected,
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

    // A rotation on the migrated box writes ROWS and never the file, so the operator's CRLF, BOM
    // and missing final newline all stay exactly where they were. (The file-branch twin of this
    // write — the byte-preserving upsert into an unmigrated `secrets.env`, with its pinned CRLF
    // normalisation — went with the credential file plane on 2026-10-07: with no database a write
    // is refused, see `writes.rs`.)
    vike_secrets::save_credentials_to_store(
        fx.dir(),
        Table::Credential,
        &[("BINANCE_DEMO_API_KEY".to_string(), "rotated".to_string())],
        Some(&classify),
    )
    .expect("write");
    assert_eq!(std::fs::read(fx.store()).expect("read"), before, "a rotation touched the file");
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
