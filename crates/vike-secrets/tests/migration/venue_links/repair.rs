//! The repair the refusals send an operator to.

use super::*;

// ---------------------------------------------------------------------------------------------
// The repair the refusals send an operator to
// ---------------------------------------------------------------------------------------------

/// Final review M2: the SQLite-client repair trap 7 names must not be able to make things worse. It
/// offers the CORRECTION before the delete (a `DELETE` of a `credential` row removes a secret for
/// good), turns foreign keys ON first (the `sqlite3` shell starts with them OFF, so a premature
/// parent delete would go through and leave dangling references), and names the client version
/// below which the shell cannot open a store of `STRICT` tables at all.
#[test]
fn trap_7_offers_the_correction_first_and_opens_its_repair_with_foreign_keys_on() {
    let fx = with_an_off_roster_account();
    let text =
        vike_secrets::set_venue_setting_in(fx.dir(), "polymarket", None, "PROXY_ENABLED", "true")
            .expect_err("refused")
            .to_string();
    assert!(text.contains("PRAGMA foreign_keys = ON;"), "the repair turns foreign keys on: {text}");
    assert!(text.contains("3.37"), "…and names the client version STRICT tables need: {text}");
    let correct = text.find("UPDATE <table> SET venue").expect("the correction is offered");
    let delete = text.find("DELETE FROM <table>").expect("the delete is offered");
    assert!(correct < delete, "the CORRECTION comes before the delete: {text}");
    assert!(
        text.contains("only after copying out every value"),
        "…and the delete only after the values are copied out: {text}"
    );
}

/// Final review M2's third step: a rebuild that finds a reference to a row that does not exist used
/// to refuse naming NO row — *"left 1 dangling reference(s); nothing was committed"* — while every
/// write stayed refused. It names each one now, from `pragma_foreign_key_check`.
#[test]
fn a_dangling_reference_is_refused_naming_its_rows() {
    let fx = planted_on_the_old_shape();
    {
        // Foreign keys OFF, as the `sqlite3` shell starts — the only way a credential row comes to
        // name an account that is not there. Turned off EXPLICITLY: the SQLite this workspace
        // bundles is built with them ON by default (MEASURED — the plant was refused
        // `FOREIGN KEY constraint failed` without this line), which is not the shell's default.
        let conn = fx.conn();
        conn.execute_batch("PRAGMA foreign_keys = OFF;").expect("foreign keys off");
        conn.execute(
            "INSERT INTO credential (id, account_id, field, value, name) \
             VALUES (5, 99, 'API_KEY', 'a-dangling-value', 'A_DANGLING_KEY')",
            [],
        )
        .expect("plant a credential row naming no account");
    }
    let err =
        vike_secrets::set_venue_setting_in(fx.dir(), "polymarket", None, "PROXY_ENABLED", "true")
            .expect_err("the carry's rebuild of `account` finds the dangling reference");
    let text = err.to_string();
    assert!(
        text.contains("`credential` row 5 (its `account` row is missing)"),
        "the refusal names the row, its table and the parent it names: {text}"
    );
    assert!(!text.contains("a-dangling-value"), "…and never a value: {text}");
    assert!(!is_not_null(&fx.conn(), "account", "venue_id"), "…and nothing was committed");
}
