//! Proof 15 - the dry run writes nothing and predicts exactly what the apply does.

use super::*;

// ---------------------------------------------------------------------------------------------
// PROOF 15 — the DRY RUN writes nothing, and predicts exactly what the apply does
// ---------------------------------------------------------------------------------------------

/// **A dry run over a live-shaped box leaves NO database, NO directory, and the backend unmoved.**
///
/// This is the property the whole verb rests on. A migration's first successful run is irreversible
/// in practice — from the moment `vike.db` exists `workspace_backend_from` answers `Database` for
/// every process on the box and `secrets.env` stops being read — so a preview that created anything
/// at all would be the act it exists to let somebody avoid. The three assertions are separate
/// because the three failures are: a stamped database, a bare `db/` directory left by a create that
/// got that far, and a backend that moved.
#[test]
fn a_dry_run_creates_nothing_at_all() {
    let fx = Fixture::live_shaped();

    let plan = preview(&fx);
    assert_eq!(plan.outcome, vike_secrets::PlannedOutcome::WouldCreate, "{plan}");
    assert!(plan.would_create(), "{plan}");
    assert_eq!(plan.would_insert(), 71, "67 credentials + 4 node keys would be inserted\n{plan}");
    assert_eq!(plan.keys_read(), 71);

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

    // …and the source files are untouched by a preview, exactly as they are by a migration.
    let before = (digest(&fx.store()), digest(&fx.node()));
    preview(&fx);
    assert_eq!((digest(&fx.store()), digest(&fx.node())), before, "a preview touched a file");
}

/// **What the dry run SAID is what the apply DID — field for field, on the run that creates.**
///
/// A weaker test would assert the two totals agree, and that would stay green on a preview whose
/// per-file attribution, doubly-claimed list or refusal set had drifted from the migration's. The
/// report shapes are deliberately the same types (`SourceReport`, `Ambiguity`) precisely so this
/// comparison can be exact.
#[test]
fn the_dry_run_predicts_exactly_what_the_apply_does() {
    let fx = Fixture::live_shaped();

    let plan = preview(&fx);
    let done = fx.migrate();

    assert_eq!(done.outcome, vike_secrets::MigrationOutcome::Created);
    assert_eq!(plan.db, done.db, "the two named different databases");
    assert_eq!(plan.sources, done.sources, "the per-file attribution drifted\n{plan}\n{done}");
    assert_eq!(plan.doubly_claimed, done.doubly_claimed);
    assert_eq!(plan.refused, done.refused);
    assert_eq!(plan.would_insert(), done.inserted(), "the predicted row count was wrong");
    assert_eq!(plan.keys_read(), done.keys_read());
    assert_eq!(plan.inserted_keys, done.inserted_keys, "the predicted NAMES were wrong");
    assert_eq!(
        done.inserted_keys.len(),
        done.inserted(),
        "the names and the per-file counts are two decompositions of one set of rows, and they \
         disagree"
    );

    // And the prediction was about the DATABASE, not about a report: every name it promised is in
    // the store the resolver reads.
    let resolved = vike_secrets::resolve_project(fx.arg()).expect("resolve_project");
    assert_eq!(resolved.source, Source::Database(fx.db()));
    assert_eq!(resolved.secrets.len(), 67);
}

/// **The three OTHER states predict correctly too**, and each is a state the creating run cannot
/// reach.
///
/// `WouldAdd` and `AlreadyComplete` both require a database to exist already; `NothingToMigrate`
/// requires that none does AND that the files carry nothing. Covering only the creating run would
/// leave the arm an operator reaches most often — the second run, on a box that has already migrated
/// — unproven.
#[test]
fn the_dry_run_predicts_the_second_run_and_the_empty_box() {
    // AlreadyComplete: migrate, then preview again.
    let fx = Fixture::live_shaped();
    fx.migrate();
    let plan = preview(&fx);
    assert_eq!(plan.outcome, vike_secrets::PlannedOutcome::AlreadyComplete, "{plan}");
    assert!(!plan.would_create());
    assert_eq!(plan.would_insert(), 0, "a converged box would write nothing\n{plan}");
    assert_eq!(plan.keys_read(), 71, "…it still READ all 71, it just has nothing to do");
    assert!(plan.inserted_keys.is_empty(), "a converged box would name no key: {plan}");
    let second = fx.migrate();
    assert_eq!(second.outcome, vike_secrets::MigrationOutcome::AlreadyComplete);
    assert_eq!(plan.sources, second.sources);

    // WouldAdd: one more key appears in the file after the migration.
    std::fs::write(
        fx.store(),
        format!("{}\nNEWVENUE_DEMO_API_KEY=fresh\n", std::fs::read_to_string(fx.store()).unwrap()),
    )
    .unwrap();
    let plan = preview(&fx);
    assert_eq!(plan.outcome, vike_secrets::PlannedOutcome::WouldAdd, "{plan}");
    assert_eq!(plan.would_insert(), 1, "{plan}");
    let done = fx.migrate();
    assert_eq!(done.outcome, vike_secrets::MigrationOutcome::Updated);
    assert_eq!(done.inserted(), 1);
    assert_eq!(plan.sources, done.sources);

    // ⚠ **THE ADDED RUN NAMES ONE KEY, not sixty-eight.** `inserted_keys` is what this run WROTE,
    // and the defect it exists against is the tempting alternative: a caller that needed the names
    // for a ledger record and read the whole table back would name every key in the store and claim
    // this run inserted them — an append-only record asserting something false.
    assert_eq!(done.inserted_keys, vec!["NEWVENUE_DEMO_API_KEY".to_string()], "{done}");
    assert_eq!(plan.inserted_keys, done.inserted_keys);
    assert_eq!(
        vike_secrets::read_table(&fx.db(), Table::Credential).expect("read back").len(),
        68,
        "…while the STORE holds all 68, which is the number a read-back would have recorded"
    );

    // NothingToMigrate: a bare project. The preview must not create one either — this is the same
    // harm `an_empty_database_is_never_created_so_it_cannot_shadow_the_real_file` measures on the
    // apply path, reached through the preview.
    let bare = Fixture::empty();
    let plan = preview(&bare);
    assert_eq!(plan.outcome, vike_secrets::PlannedOutcome::NothingToMigrate, "{plan}");
    assert_eq!(plan.would_insert(), 0);
    assert!(!bare.db().exists(), "a preview of an empty project created a database");
    assert!(!bare.db().parent().unwrap().exists(), "…nor even the `db/` directory");
}

/// **A REFUSAL is predicted identically and neither entry point writes.**
///
/// The whole-run refusal is decided before any write in both, so a dry run genuinely tells an
/// operator that their box is undecidable — which is the case where a preview is worth most, because
/// the apply would have told them the same thing and they would not have known that in advance.
#[test]
fn an_ambiguous_box_is_refused_by_the_preview_and_by_the_apply_alike() {
    let fx = Fixture::empty();
    std::fs::write(fx.store(), "VIKE_TRADEHUB_OBSERVE_KEY=old\nBINANCE_DEMO_API_KEY=b\n").unwrap();
    std::fs::write(fx.node(), "VIKE_TRADEHUB_OBSERVE_KEY=new\n").unwrap();

    let predicted = match vike_secrets::preview(
        fx.arg(),
        is_node_key,
        &classify,
        vike_secrets::WhenNothingToCarry::CreateNothing,
    ) {
        Err(vike_secrets::MigrateError::Ambiguous(list)) => list,
        Ok(p) => panic!("expected a refusal, got: {p}"),
        Err(e) => panic!("expected an ambiguity refusal, got: {e}"),
    };
    assert_eq!(
        predicted,
        vec![vike_secrets::Ambiguity::DisagreeingFiles {
            key: "VIKE_TRADEHUB_OBSERVE_KEY".to_string()
        }]
    );
    assert!(!fx.db().exists(), "a refused PREVIEW must not leave a database behind");
    assert!(!fx.db().parent().unwrap().exists(), "…nor even the directory");

    // …and the apply refuses with exactly the same findings.
    assert_eq!(refusal(&fx), predicted, "the preview and the apply disagreed about a refusal");
    assert!(!fx.db().exists());
}

/// **A per-KEY refusal is predicted too, and it rides an `Ok` on both sides.**
///
/// The distinction the library draws — a whole-run refusal is an `Err` and writes nothing, a
/// disagreeing key is reported on an otherwise successful run — has to survive into the preview, or
/// a dry run would tell an operator their migration is fine and the apply would then leave a key
/// behind.
#[test]
fn a_per_key_refusal_is_predicted_on_an_otherwise_successful_plan() {
    let fx = Fixture::empty();
    std::fs::write(fx.store(), "BINANCE_DEMO_API_KEY=first\n").unwrap();
    fx.migrate();

    // The operator edits the migrated key AND adds a brand-new one in the same edit.
    std::fs::write(fx.store(), "BINANCE_DEMO_API_KEY=second\nOKX_DEMO_API_KEY=fresh\n").unwrap();

    let plan = preview(&fx);
    assert_eq!(plan.outcome, vike_secrets::PlannedOutcome::WouldAdd, "{plan}");
    assert_eq!(plan.would_insert(), 1, "only the NEW key would land\n{plan}");
    assert_eq!(
        plan.refused,
        vec![vike_secrets::Ambiguity::DisagreesWithDatabase {
            key: "BINANCE_DEMO_API_KEY".to_string(),
            table: Table::Credential,
        }],
        "the disagreeing key must be NAMED in the plan\n{plan}"
    );
    // The plan says so in its own words, and says nothing was written.
    let said = plan.to_string();
    assert!(said.contains("would be REFUSED"), "{said}");
    assert!(said.contains("NOTHING WAS WRITTEN"), "{said}");

    let done = fx.migrate();
    assert_eq!(done.inserted(), 1);
    assert_eq!(done.refused, plan.refused, "the apply refused a different set than predicted");

    // The stored value is the FIRST one — the refusal protected it, as predicted.
    let rows = vike_secrets::read_table(&fx.db(), Table::Credential).expect("read back");
    assert_eq!(value(&rows, "BINANCE_DEMO_API_KEY").as_deref(), Some("first"));
    assert_eq!(value(&rows, "OKX_DEMO_API_KEY").as_deref(), Some("fresh"));
}

/// **A plan is never mistakable for a finished migration**, which is why `preview` returns its own
/// type.
///
/// `Migration::database_exists` is documented as *"as a result of this run having SUCCEEDED"* and
/// `MigrationOutcome`'s `Display` says `created`. A preview returning one would assert a database
/// exists when none does — and the caller most likely to be misled is a CLI printing the report
/// straight through. So this asserts the RENDERING an operator reads, on the one box where the
/// difference is dangerous.
#[test]
fn a_plan_does_not_claim_a_database_exists() {
    let fx = Fixture::live_shaped();
    let said = preview(&fx).to_string();

    assert!(said.contains("would be CREATED"), "{said}");
    assert!(said.contains("NOTHING WAS WRITTEN"), "{said}");
    assert!(
        !said.contains("(created)"),
        "a plan must not render in the past tense — that is `Migration`'s vocabulary: {said}"
    );
    assert!(!fx.db().exists());
}
