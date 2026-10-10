//! The ruled fold measured against the subject, and the store-side checks: the DDL column, its write and the fixture builder.

use std::collections::BTreeSet;
use std::path::Path;

use vike_secrets::Account;

use super::fixtures::{A_LABELLED_ACCOUNT_WITH_NO_LINE, Fixture, PROD2_SHAPE};
use super::judge::{RULED, SUBJECT, mode, new_effective, objections};

/// **A FINDING against the ruled rule, executable rather than argued.**
///
/// `VenuePolicy::account` resolves a LABELLED account with no `[accounts]` line of its own to
/// `paper` — *"the `bybit = "live"` line was written when bybit had one account; reading it as
/// consent for an account that did not exist when it was written is precisely the silent
/// escalation the ceiling exists to prevent"*. The ruled fold reads the VENUE row only, so it arms
/// that account: old effective `paper`, new effective `live`.
///
/// ⚠ **This is unreachable on either live box today** — every account a migration writes carries
/// `label: None` (`vike_secrets::Account`'s own doc), and neither store holds a `__LABEL`
/// credential key. It is reachable the moment one is filed, which is an operator action needing no
/// code change, so it is a live hazard for stage 3 rather than a curiosity.
///
/// ⚠ **And it is exactly what `VenuePolicy::get` cannot see**, which is why this gate does not use
/// it: the same rows, judged by the venue answer, produce NO objection at all.
#[test]
fn the_ruled_fold_widens_a_labelled_account_with_no_line_of_its_own() {
    let fx = Fixture::build(A_LABELLED_ACCOUNT_WITH_NO_LINE);
    let labelled: Vec<&Account> =
        fx.accounts.iter().filter(|a| a.label.as_deref() == Some("ALT")).collect();
    assert_eq!(labelled.len(), 1, "the fixture mints one LABELLED account: {:?}", fx.accounts);
    let alt = labelled[0].id;

    let found = objections(&fx, &RULED(&fx));
    assert!(
        found.iter().any(|f| f.contains(&format!("account {alt} ")) && f.contains("WIDENS")),
        "the ruled fold arms a labelled account the venue line never consented to, and the gate \
         must say so: {found:?}"
    );

    // The blindness the brief names, measured rather than asserted: judged by the VENUE answer,
    // the same rows raise nothing.
    let blind: Vec<String> = RULED(&fx)
        .iter()
        .filter(|(a, armed)| {
            let old = fx.policy.get(&a.venue).cap(mode(&a.tier));
            new_effective(a, *armed) > old
        })
        .map(|(a, _)| a.id.to_string())
        .collect();
    assert!(
        blind.is_empty(),
        "`VenuePolicy::get` makes a labelled widening INVISIBLE — that is the whole reason this \
         gate resolves through `VenuePolicy::account`: {blind:?}"
    );
}

/// **[`SUBJECT`] and §5.2 step 5's venue-row wording agree on the the CI box shape, row for row.**
///
/// That equality is what makes the shipped fold a safer SPELLING of the same migration rather
/// than a different migration: on a store whose accounts are all unlabelled — which is what both
/// live boxes hold — the two arms [`RULED`] omits are unreachable, so the two folds are
/// byte-identical.
///
/// ⚠ It compares the spec's wording against the BITS
/// `crates/vike-secrets/src/settings/arming.rs`'s `fold_arming_into_accounts` actually wrote into a
/// real store, not against a model.
///
/// ⚠ **This is INFERENCE from shape equivalence, not a measurement of either real store.** The
/// fixture is constructed to §5.2 step 5's description of the CI box (see [`PROD2_SHAPE`]); nobody has
/// run this fold against the actual database. What it licenses is *"the subject changes nothing
/// the ruled rule would not have changed, on a store of this shape"* — not *"the CI box is
/// unaffected"*. Confirming the latter needs the fold run on the box, which is stage 3's rollout
/// and not this gate's claim to make.
#[test]
fn the_subject_and_the_specs_venue_row_wording_agree_on_the_prod2_shape() {
    let fx = Fixture::build(PROD2_SHAPE);
    let ruled: Vec<(i64, bool)> = RULED(&fx).iter().map(|(a, b)| (a.id, *b)).collect();
    let subject: Vec<(i64, bool)> = SUBJECT(&fx).iter().map(|(a, b)| (a.id, *b)).collect();
    assert_eq!(
        ruled, subject,
        "the two folds must agree on every store whose accounts are unlabelled"
    );
}

/// **The column must EXIST**, and that is not ceremony: [`SUBJECT`] now reads `Account::armed`, and
/// a `bool` field reads `false` just as happily when the column has been dropped, renamed or lost
/// in a rebuild. Every inequality in this file would stay green over sixteen `false` bits — a
/// NARROWING — so without this the gate could go blind to the column's disappearance while
/// reporting success.
#[test]
fn the_shipped_ddl_declares_the_armed_column() {
    let account_table = vike_secrets::DDL
        .split("CREATE TABLE IF NOT EXISTS account (")
        .nth(1)
        .and_then(|rest| rest.split(") STRICT;").next())
        .expect("the shipped DDL declares an `account` table");
    assert!(
        account_table.lines().any(|line| line.trim_start().starts_with("armed ")),
        "`account.armed` is GONE from the shipped DDL. This gate reads that column through \
         `vike_secrets::Account::armed` and would report every row as `paper` — a narrowing every \
         other assertion here tolerates. Re-point the gate before removing the column: {account_table}"
    );
    assert!(
        account_table.contains("CHECK (armed IN (0, 1))"),
        "…and the column keeps its `CHECK`: the fold writes 0/1, and `STRICT` types the column \
         INTEGER without constraining the value: {account_table}"
    );
}

/// **…and the column is actually WRITTEN, which the DDL alone cannot say.**
///
/// `armed INTEGER NOT NULL DEFAULT 0` means a store where the fold never ran answers `false` for
/// every row — indistinguishable, to every inequality in this file, from a store the operator
/// armed nothing on. This is the positive check: on the the CI box shape, some row comes back `true`,
/// so the write path was genuinely exercised rather than defaulted through.
#[test]
fn the_armed_column_is_written_by_the_store_and_not_merely_defaulted() {
    let fx = Fixture::build(PROD2_SHAPE);
    assert!(
        fx.accounts.iter().any(|a| a.armed),
        "every account came back DISARMED. Either `write_settings` no longer folds the arming \
         rows onto the account rows, or the fold ran before they landed — both read as a safe \
         narrowing to this gate's inequality and neither is what stage 3 shipped: {:?}",
        fx.accounts
    );
}

/// A guard on the fixture builder itself: a store that minted no accounts, or whose arming rows
/// never landed, would make every assertion above vacuous.
#[test]
fn the_fixture_builder_produces_a_real_store_with_both_tables_filled() {
    let fx = Fixture::build(PROD2_SHAPE);
    assert!(!fx.accounts.is_empty(), "the migration minted account rows");
    assert!(!fx.arming.is_empty(), "…and the mirror wrote arming rows");
    assert!(
        fx.policy.is_declared(),
        "…and the policy the gate compares against was DECLARED by those rows, not defaulted — \
         an undeclared policy reads every venue `paper` and would make every comparison pass"
    );
    for (venue, mode, _keys) in PROD2_SHAPE {
        assert_eq!(fx.policy.get(venue).as_str(), *mode, "the store's rows resolve {venue}'s line");
    }

    // …and the table is ROSTER-COMPLETE, which is what makes "14 venue lines" a fact about this
    // fixture rather than an accident. The mirror writes one arming row per roster venue whether
    // this table names it or not, so a roster that outgrew the table would leave venue lines with
    // no account under them. `just new-venue` reaches the table through the marker in
    // `PROD2_SHAPE`; this is the assertion that notices when it has not been run.
    let named: BTreeSet<&str> = PROD2_SHAPE.iter().map(|(venue, _, _)| *venue).collect();
    let unnamed: Vec<&&str> = vike_model::VENUES.iter().filter(|v| !named.contains(*v)).collect();
    assert!(
        unnamed.is_empty(),
        "`PROD2_SHAPE` must name every roster venue and does not: {unnamed:?}. Run \
         `just new-venue <name>`, fill in the scaffolded row, and move the three counts in \
         `the_prod2_shape_comes_out_with_fifteen_armed_and_exactly_one_named_narrowing` with it."
    );
    let path: &Path = fx._dir.path();
    assert!(path.join("db").join("vike.db").is_file(), "a real database file is on disk");
}
