//! Proof 15 - the dry run writes nothing and predicts exactly what the apply does.

use super::*;

// ---------------------------------------------------------------------------------------------
// PROOF 15 — the DRY RUN writes nothing, and predicts exactly what the apply does
// ---------------------------------------------------------------------------------------------

/// **A dry run on a box with no store leaves NO database, NO directory, and the backend unmoved.**
///
/// This is the property the whole verb rests on. Creating the store is irreversible in practice —
/// from the moment `vike.db` exists `workspace_backend_from` answers `Database` for every process on
/// the box — so a preview that created anything at all would be the act it exists to let somebody
/// avoid. The three assertions are separate because the three failures are: a stamped database, a
/// bare `db/` directory left by a create that got that far, and a backend that moved.
#[test]
fn a_dry_run_creates_nothing_at_all() {
    let fx = Fixture::empty();

    let plan = preview(&fx);
    assert!(plan.would_create, "{plan}");

    assert!(
        !fx.db().exists(),
        "A DRY RUN CREATED THE DATABASE. From here `backend_at` answers `Database` for every \
         process on this box and the box holds an EMPTY store — which is the exact act \
         the preview exists to let somebody decide about first."
    );
    assert!(!fx.db().parent().unwrap().exists(), "…nor even the `db/` directory");
    assert_eq!(
        vike_secrets::workspace_backend_from(fx.arg()),
        Backend::Absent,
        "still no store for this project — the preview created none"
    );
}

/// **What the dry run SAID is what the apply DID — on the run that creates.**
#[test]
fn the_dry_run_predicts_exactly_what_the_apply_does() {
    let fx = Fixture::empty();

    let plan = preview(&fx);
    let done = fx.create();

    assert!(plan.would_create && done.created, "{plan} / {done}");
    assert_eq!(plan.db, done.db, "the two named different databases");

    // And the prediction was about the DATABASE, not about a report: the store it promised is the
    // one the resolver reads, empty.
    let resolved = vike_secrets::resolve_project(fx.arg()).expect("resolve_project");
    assert_eq!(resolved.source, Source::Database(fx.db()));
    assert!(resolved.secrets.is_empty());
}

/// **…and on the run an operator reaches most often: a box that already has its store.** The plan
/// says nothing would be written, and the apply writes nothing.
#[test]
fn the_dry_run_predicts_the_second_run() {
    let fx = Fixture::live_shaped();
    let plan = preview(&fx);
    assert!(!plan.would_create, "{plan}");
    let done = fx.create();
    assert!(!done.created, "{done}");
    assert_eq!(plan.db, done.db);
}

/// **A plan is never mistakable for a finished creation**, which is why `preview_create_store`
/// returns its own type.
///
/// `StoreCreation`'s `Display` says `created`. A preview returning one would assert a database
/// exists when none does — and the caller most likely to be misled is a CLI printing the report
/// straight through. So this asserts the RENDERING an operator reads, on the one box where the
/// difference is dangerous.
#[test]
fn a_plan_does_not_claim_a_database_exists() {
    let fx = Fixture::empty();
    let said = preview(&fx).to_string();

    assert!(said.contains("would be CREATED"), "{said}");
    assert!(said.contains("NOTHING WAS WRITTEN"), "{said}");
    assert!(
        !said.contains("(created"),
        "a plan must not render in the past tense — that is `StoreCreation`'s vocabulary: {said}"
    );
    assert!(!fx.db().exists());
}
