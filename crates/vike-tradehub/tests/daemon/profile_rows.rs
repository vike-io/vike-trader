//! **Phase 3's proof: a migration to profile rows changes nothing about what a box trades.**
//!
//! `docs/decisions/0057-the-seven-settings-files-answered-one-at-a-time.md` Question 3 was left open
//! and the owner has since ruled — **the ROW wins**. That ruling makes the migration itself capable
//! of arming a mount, because today a selection's ABSENCE is a state a box can be in and a row is a
//! selection's presence. Everything here exists to make the no-change claim checkable rather than
//! asserted.
//!
//! What is proven, and against which seam:
//!
//! * **The arming OUTCOME is preserved**, compared as an outcome and not as rows
//!   (`vike_tradehub::profile_rows::resolve_arming`, the same value the daemon logs at startup) —
//!   for a the build runner LIVE state and for the dangerous state 0057 singles out, where the live flag
//!   is on and nothing selects a run profile so today's daemon EXITS FAILURE.
//! * **The fence that buys it** (`vike_secrets::profile_store::plan_active_row`) — the one place a
//!   migration may decide to write an active row, in all four of its situations.
//! * **The explicit primary survives a row REORDERING**, which is the property a table cannot get
//!   from order and the reason 0057 requires the column.
//! * **A primary naming a venue this node runs no engine for is refused in PR #1866's own
//!   vocabulary**, sharing one sentence with the order path rather than inventing a second.
//!
//! ⚠ **The STORE half is not here and deliberately so.** Creating a settings database needs the
//! credential-migration fixture, which lives one crate down, so
//! `crates/vike-secrets/tests/profile_store.rs` carries the read-only read under the deployed
//! daemon's own permission shape, the write refusal from inside it, and the
//! absence-equals-emptiness property. Everything in THIS file is pure, which is also why it can say
//! what it says: no assertion below depends on a filesystem, so none of them can pass for a reason
//! about one.
//!
//! No network, no credentials, no feature flags.

use std::collections::HashMap;

use vike_core::RunProfile;
use vike_secrets::profile_store::{
    ActivePlan, Primary, ProfileKind, WithholdReason, plan_active_row,
};
use vike_tradehub::config::{DaemonProfile, PrimaryMount};
use vike_tradehub::profile_rows::{
    ArmingOutcome, LiveRefusal, REFUSING_RISK_KEYS, daemon_profile_to_rows,
    live_risk_budget_missing, plan_migration, resolve_arming, rows_to_daemon_profile,
    rows_to_run_profile, select_daemon_profile, select_run_profile,
};

/// The daemon profile the CI box actually runs, in shape: one bybit mount on the wired symbol.
///
/// MEASURED in `deploy/vike-tradehub.service`'s own header — the ready banner read
/// `"mode":"LIVE (venue=bybit)"` and the store held `BYBIT_DEMO_*`. The SYMBOL here is this file's
/// own choice (that page records the venue, not the pair); nothing in these assertions depends on
/// which symbol it is, only on it staying the same across the migration.
const PROD2_SHAPED_DAEMON_PROFILE: &str = "\
venue = \"bybit\"
symbol = \"BTCUSDT\"
interval = \"1m\"
asset_class = \"CryptoSpot\"
";

/// A run profile of the shape the CI box's `.env` names through `VIKE_RUN_PROFILE`: `mode = \"live\"`
/// and a `[risk]` table carrying the two caps a live mount refuses to start without.
///
/// ⚠ **This fixture used to carry `[event_source]` and `[broker]` and they are GONE**, deleted by
/// 0057's Phase 0 — measured as read only inside `RunProfile::validate`, so they were required by
/// the schema and consumed by no runner in either binary. The owner asked for them to be migrated
/// and the record returned a deletion instead, on the ground that a row reads as more authoritative
/// than the file line it replaced.
///
/// Phase 0 and this branch were each green alone and red together: Phase 0 landed first, and this
/// fixture then hit Phase 0's own refusal — *"`[event_source]` is no longer part of a run profile"*
/// — which is the refusal working exactly as written, against a fixture nobody had re-read. Keeping
/// the note here rather than silently deleting two tables, because the next person to copy a run
/// profile out of a test will otherwise reintroduce them.
/// ⚠ **`max_total_exposure` was `1000.0` here and the CI box's file says `500.0`**, which is also what
/// [`prod2_run_rows`] carries — so the two halves of every before/after comparison in this file
/// disagreed about a live pre-trade ceiling while claiming to be the same box. Nothing caught it:
/// `ArmingOutcome::Live` carries no risk numbers, so the comparison that reads *"reading the run
/// profile from ROWS changed what this box trades"* passed on any non-`None` pair. The value is
/// corrected to the live file's (`crates/vike-cli/src/cmd/config_mirror_profile.rs`'s `PROD2_RUN`
/// is the VERBATIM copy), and
/// [`a_prod2_shaped_live_box_arms_identically_from_the_run_rows`] now compares the `[risk]` table
/// itself so a future drift between the two fixtures cannot pass again.
const PROD2_SHAPED_RUN_PROFILE: &str = "\
mode = \"live\"

[risk]
max_notional_per_order = 100.0
max_total_exposure     = 500.0
";

fn daemon(text: &str) -> DaemonProfile {
    DaemonProfile::from_toml_str(text).expect("the fixture profile must load")
}

fn run(text: &str) -> RunProfile {
    RunProfile::from_toml_str(text).expect("the fixture run profile must load")
}

// ---------------------------------------------------------------------------------------------
// THE ONE THAT MATTERS: the arming outcome is preserved
// ---------------------------------------------------------------------------------------------

/// **A box that is LIVE today is live after, with the same mounts.**
///
/// The comparison is `ArmingOutcome`, which folds the three inputs that decide what this daemon does
/// (`flags.tradehub_live`, the run profile's `[risk]`, and the daemon profile's mount set) into the
/// one value the daemon logs at startup. Comparing ROWS would prove only that a migration copied
/// bytes; comparing this proves it did not change what the box trades.
#[test]
fn a_prod2_shaped_live_box_resolves_to_the_same_arming_outcome_after_the_migration() {
    let profile = daemon(PROD2_SHAPED_DAEMON_PROFILE);
    let rp = run(PROD2_SHAPED_RUN_PROFILE);

    // BEFORE: the `--config` argument selects the only profile there is, the live flag is on, and
    // `VIKE_RUN_PROFILE` names a live `[risk]`.
    let before = resolve_arming(true, Some(&rp), &profile);
    assert!(
        matches!(&before, ArmingOutcome::Live { primary, .. } if primary.as_str() == "bybit/BTCUSDT"),
        "the pre-migration state must resolve LIVE on bybit, or this test is measuring the wrong \
         box: {before:?}"
    );

    // THE MIGRATION. `--config` is what is in force, and the body is stored under the name derived
    // from it, so `plan_active_row` may write the row: it names exactly what already runs.
    let plan = plan_migration("tradehub", &profile, Some("tradehub"), None)
        .expect("the fixture names its asset class");
    assert_eq!(
        plan.active,
        ActivePlan::Write { name: "tradehub".to_string() },
        "a row naming what already runs is the one case a migration may write"
    );

    // AFTER: the store's active row wins, the body comes back through the EXISTING parser, and the
    // run profile is untouched by any of it.
    let selection = select_daemon_profile(Some("tradehub"), "/settings/tradehub.toml");
    assert_eq!(selection.value.as_deref(), Some("tradehub"), "the row wins: {selection:?}");
    let reloaded = rows_to_daemon_profile(&plan.body).expect("the stored body must reload");
    let after = resolve_arming(true, Some(&rp), &reloaded);

    assert_eq!(
        before, after,
        "the migration changed WHAT THIS BOX TRADES, which is the one thing \
                               Phase 3 may not do"
    );
}

/// **The dangerous state 0057 Question 3 names, and the one the fence exists for.**
///
/// The live flag is ON and nothing selects a run profile. Today that box does not trade and does not
/// run: the live mount refuses for want of a risk budget and the daemon exits FAILURE
/// (`vike_config::ceilings`'s `absent_means` for `max_notional_per_order` states it in those words).
/// A migration that wrote an active run-profile row would resolve a `[risk]` table where there was
/// none and start a live mount that has never run.
///
/// ⚠ **This is the mutation target.** Change `plan_active_row`'s `None` arm from `Withhold` to
/// `Write` and the `after` half below becomes `Live` while `before` stays `LiveRefused`.
#[test]
fn a_box_where_nothing_selects_a_run_profile_is_not_armed_by_the_migration() {
    let profile = daemon(PROD2_SHAPED_DAEMON_PROFILE);
    let rp = run(PROD2_SHAPED_RUN_PROFILE);
    let no_vars: HashMap<String, String> = HashMap::new();

    // BEFORE — nothing selects a run profile, so `vike_core::resolve_profile` would answer
    // `Ok(None)` and the live mount refuses.
    let before_selection = select_run_profile(None, None, &no_vars);
    assert_eq!(before_selection.value, None, "the premise: nothing selects one today");
    let before = resolve_arming(true, None, &profile);
    assert_eq!(
        before,
        ArmingOutcome::LiveRefused(LiveRefusal::MissingRiskBudget(REFUSING_RISK_KEYS.to_vec())),
        "today this box exits FAILURE rather than trading — that is the state being preserved"
    );

    // THE MIGRATION. It stores the run profile's body whatever happens; the question is the row.
    let plan = plan_active_row(None, None, "run-live");

    // AFTER — the store's rung is whatever the plan decided, and nothing else moved.
    let active_after = match &plan {
        ActivePlan::Write { name } => Some(name.as_str()),
        ActivePlan::Withhold { .. } => None,
    };
    let after_selection = select_run_profile(active_after, None, &no_vars);
    let after_run = after_selection.value.as_deref().map(|_| &rp);
    let after = resolve_arming(true, after_run, &profile);

    // ⚠ THE OUTCOME COMPARISON COMES FIRST, DELIBERATELY. An earlier draft asserted the PLAN before
    // computing the outcome, and the mutation proof then reddened on the plan — which is an
    // intermediate value, not the property. A test whose headline claim is shadowed by a cheaper
    // assertion cannot be said to be checking the claim. This is the assertion that must break.
    assert_eq!(
        before, after,
        "THE MIGRATION ARMED THIS BOX. Before it the daemon exited FAILURE with no risk budget; \
         after it a live mount starts. `plan_active_row`'s `NothingSelectedToday` arm is the fence \
         that was removed"
    );
    // …and only then the plan, which is the MECHANISM the outcome rests on.
    assert_eq!(
        plan,
        ActivePlan::Withhold { reason: WithholdReason::NothingSelectedToday },
        "writing an active row here is this migration ARMING a mount that an absent selection was \
         holding"
    );
}

/// **A box that is on PAPER today is on paper after**, and the migration cannot reach the flag that
/// decides it: `flags.tradehub_live` is not a profile key and no row here touches it.
#[test]
fn a_paper_box_stays_paper_whatever_the_store_holds() {
    let profile = daemon(PROD2_SHAPED_DAEMON_PROFILE);
    let rp = run(PROD2_SHAPED_RUN_PROFILE);
    assert_eq!(resolve_arming(false, None, &profile), ArmingOutcome::Paper);
    assert_eq!(
        resolve_arming(false, Some(&rp), &profile),
        ArmingOutcome::Paper,
        "a fully-armed run profile does not make a paper box live — the live gate is the FLAG"
    );
}

/// **The migration will not silently repoint a box at a different body.**
///
/// The second mutation surface: `plan_active_row`'s `WouldRepoint` arm. If it wrote, a box running
/// `tradehub` would come back running `tradehub-alt` — a different venue, a different book.
#[test]
fn the_migration_will_not_repoint_a_box_at_a_profile_it_does_not_run() {
    let running = daemon(PROD2_SHAPED_DAEMON_PROFILE);
    let other = daemon("venue = \"okx\"\nsymbol = \"BTCUSDT\"\nasset_class = \"CryptoSpot\"\n");

    let plan = plan_migration("tradehub-alt", &other, Some("tradehub"), None)
        .expect("the fixture names its asset class");

    // The OUTCOME first, for the reason the sibling test above spells out: the property is what the
    // box trades, and an assertion on the intermediate plan would shadow it.
    let rp = run(PROD2_SHAPED_RUN_PROFILE);
    let before = resolve_arming(true, Some(&rp), &running);
    let active_after = match &plan.active {
        ActivePlan::Write { name } => Some(name.clone()),
        ActivePlan::Withhold { .. } => None,
    };
    let after_profile = match active_after.as_deref() {
        Some("tradehub-alt") => rows_to_daemon_profile(&plan.body).expect("reload"),
        _ => running.clone(),
    };
    assert_eq!(
        before,
        resolve_arming(true, Some(&rp), &after_profile),
        "the migration moved this daemon onto another venue's book"
    );
    assert_eq!(
        plan.active,
        ActivePlan::Withhold {
            reason: WithholdReason::WouldRepoint {
                in_force: "tradehub".to_string(),
                proposed: "tradehub-alt".to_string(),
            }
        },
        "storing a SECOND profile's body may not change which one is live"
    );
}

/// A migration does not overturn a selection an operator already made.
#[test]
fn the_migration_leaves_an_operators_own_active_row_alone() {
    assert_eq!(
        plan_active_row(Some("tradehub"), Some("tradehub-alt"), "tradehub"),
        ActivePlan::Withhold {
            reason: WithholdReason::AlreadyActive { held_by: "tradehub-alt".to_string() }
        }
    );
    assert_eq!(
        plan_active_row(Some("tradehub"), Some("tradehub"), "tradehub"),
        ActivePlan::Write { name: "tradehub".to_string() },
        "re-storing the body of the profile that is already active is a no-op, not a refusal"
    );
}

// ---------------------------------------------------------------------------------------------
// The explicit primary
// ---------------------------------------------------------------------------------------------

const TWO_MOUNTS_SECOND_DECLARED: &str = "\
[[mounts]]
venue = \"bybit\"
symbol = \"BTCUSDT\"
asset_class = \"CryptoSpot\"
interval = \"1m\"

[[mounts]]
venue = \"okx\"
symbol = \"BTC-USDT\"
asset_class = \"CryptoSpot\"
interval = \"1m\"
primary = true
";

/// **The property a table cannot get from order.** Rows are stored, their ordinals are permuted, and
/// the primary is still the same mount — which is exactly what `resolved[0]` could not survive.
#[test]
fn an_explicit_primary_survives_a_row_reordering() {
    let profile = daemon(TWO_MOUNTS_SECOND_DECLARED);
    assert_eq!(profile.primary_mount(), PrimaryMount::Declared(1), "the SECOND row declared it");

    let mut stored = daemon_profile_to_rows("two", &profile).expect("lowers");
    assert_eq!(stored.primary(), Primary::Declared(1));
    let primary_venue = stored.primary_mount().expect("a primary").venue.clone();
    assert_eq!(primary_venue, "okx");

    // REORDER: swap the two ordinals, which is what an editor, a re-import or a `SELECT` with no
    // `ORDER BY` can do to a table and cannot do to a TOML array.
    stored.mounts[0].ord = 1;
    stored.mounts[1].ord = 0;
    stored.mounts.sort_by_key(|m| m.ord);

    assert_eq!(
        stored.primary_mount().expect("a primary").venue,
        primary_venue,
        "a declared primary must name the same MOUNT after a reordering — if this fails the column \
         is decorative and the position is still deciding"
    );
    assert_eq!(
        stored.primary(),
        Primary::Declared(0),
        "…and it reports the ordinal it now sits at, not the one it was stored at"
    );
}

/// The historical answer is unchanged for every profile that declares nothing — which is every
/// profile that has ever shipped.
#[test]
fn a_profile_that_declares_no_primary_keeps_the_first_row_and_says_it_is_implicit() {
    let single = daemon(PROD2_SHAPED_DAEMON_PROFILE);
    assert_eq!(single.primary_mount(), PrimaryMount::ImplicitFirst(0));
    assert!(single.primary_mount().word().contains("implicit"));

    let multi = daemon(
        "[[mounts]]\nvenue = \"bybit\"\nsymbol = \"BTCUSDT\"\nasset_class = \"CryptoSpot\"\n\n\
         [[mounts]]\nvenue = \"okx\"\nsymbol = \"BTC-USDT\"\nasset_class = \"CryptoSpot\"\n",
    );
    assert_eq!(multi.primary_mount(), PrimaryMount::ImplicitFirst(0));

    let stored = daemon_profile_to_rows("implicit", &multi).expect("lowers");
    assert!(
        stored.mounts.iter().all(|m| !m.is_primary),
        "an IMPLICIT primary is stored as a declaration by nobody — promoting the accident into a \
         decision is exactly what the migration must not do"
    );
    assert_eq!(stored.primary(), Primary::ImplicitFirst(0));
}

/// Two declared primaries is a LOAD refusal naming both rows — the file-side twin of the store's
/// `mount_one_primary_per_profile` partial unique index.
#[test]
fn two_declared_primaries_are_refused_at_load_naming_both_rows() {
    let e = DaemonProfile::from_toml_str(
        "[[mounts]]\nvenue = \"bybit\"\nsymbol = \"BTCUSDT\"\nprimary = true\n\n\
         [[mounts]]\nvenue = \"okx\"\nsymbol = \"BTC-USDT\"\nprimary = true\n",
    )
    .expect_err("two primaries must not load");
    assert!(e.contains("mounts[0]") && e.contains("mounts[1]"), "names both rows: {e}");
    assert!(e.contains("primary = true"), "names the key: {e}");
}

// ---------------------------------------------------------------------------------------------
// #1866's vocabulary, on the mount side
// ---------------------------------------------------------------------------------------------

/// **One sentence, two paths.** A profile whose primary names a venue this node runs no engine for
/// is refused in the exact words PR #1866 gave the order path, because it is the same fault one
/// layer earlier — 0057 Phase 0 calls it *"the mount-side twin of the defect the order path had
/// fixed"*.
#[test]
fn a_primary_naming_an_engineless_venue_is_refused_in_the_1866_vocabulary() {
    let profile = daemon(TWO_MOUNTS_SECOND_DECLARED); // primary is okx
    let engines = vec!["bybit".to_string(), "binance".to_string()];
    let e = profile.refuse_unrunnable_primary(&engines).expect_err("okx is not in the roster");

    // The #1866 sentence, verbatim in both halves.
    assert!(e.contains("this node runs no engine for venue `okx`"), "{e}");
    assert!(e.contains("it runs: binance, bybit"), "names the roster, sorted: {e}");
    assert!(
        e.contains(
            "an order names the book it is for, and a node that cannot honour the name must not \
             choose one"
        ),
        "the shared closing clause must be byte-identical to the order path's: {e}"
    );
    assert!(
        e.contains("The declared primary mount (mounts[1])"),
        "names WHICH mount and says the primary was DECLARED rather than implicit — the repair \
         differs (drop the key, or arm the venue): {e}"
    );

    // ⚠ A ROW-LOADED PROFILE REACHES THE SAME FUNCTION, which is what makes "one vocabulary" a
    // property rather than a promise: the store holds no refusal of its own, the rows go back
    // through the real parser, and the refusal that fires is byte-identical to the file path's.
    let reloaded =
        rows_to_daemon_profile(&daemon_profile_to_rows("two", &profile).expect("lowers"))
            .expect("reload");
    assert_eq!(
        reloaded
            .refuse_unrunnable_primary(&engines)
            .expect_err("the row-loaded profile refuses too"),
        e,
        "a stored profile and a file profile must refuse in the SAME words — two spellings of one \
         refusal teach an operator to read two different faults into one situation"
    );
}

/// An EMPTY roster refuses nothing — #1866's own reading (the core has not published yet), kept
/// rather than re-decided.
#[test]
fn an_empty_engine_roster_refuses_nothing() {
    let profile = daemon(TWO_MOUNTS_SECOND_DECLARED);
    assert!(profile.refuse_unrunnable_primary(&[]).is_ok());
    let reloaded =
        rows_to_daemon_profile(&daemon_profile_to_rows("two", &profile).expect("lowers"))
            .expect("reload");
    assert!(reloaded.refuse_unrunnable_primary(&[]).is_ok());
}

// ---------------------------------------------------------------------------------------------
// Bodies round-trip through the EXISTING parser
// ---------------------------------------------------------------------------------------------

/// A body stored as rows and loaded back is the same mount set — and it gets there through
/// `DaemonProfile::from_toml_str`, so every refusal the file path carries applies to a row-loaded
/// profile too. No second validator exists to disagree with the first.
#[test]
fn a_body_round_trips_through_the_rows_and_back_through_the_real_parser() {
    for text in [
        PROD2_SHAPED_DAEMON_PROFILE,
        TWO_MOUNTS_SECOND_DECLARED,
        "token_id = \"12345678901234567890123\"\nqty = 20.0\nseed_cash = 1000.0\nasset_class = \"PredictionMarket\"\n",
        "[[mounts]]\nvenue = \"bybit\"\nsymbol = \"BTCUSDT\"\nasset_class = \"CryptoSpot\"\n\n[mounts.strategy]\nrhai = \"s.rhai\"\n\n[mounts.strategy.params]\nk = 1.5\n",
    ] {
        let original = daemon(text);
        let stored = daemon_profile_to_rows("rt", &original).expect("lowers");
        let back = rows_to_daemon_profile(&stored).unwrap_or_else(|e| {
            panic!("a stored body must reload through the real parser: {e}\n--- for ---\n{text}")
        });
        let want: Vec<String> = original
            .mount_rows()
            .iter()
            .map(|r| format!("{}/{}", r.venue(), r.mount_symbol()))
            .collect();
        let got: Vec<String> = back
            .mount_rows()
            .iter()
            .map(|r| format!("{}/{}", r.venue(), r.mount_symbol()))
            .collect();
        assert_eq!(want, got, "the mount set changed shape on the way through the store: {text}");
        assert_eq!(
            original.primary_mount().index(),
            back.primary_mount().index(),
            "the primary moved on the way through the store: {text}"
        );
        assert_eq!(
            original.daemon.summary_ms, back.daemon.summary_ms,
            "the [daemon] table did not survive: {text}"
        );
        assert_eq!(original.daemon.shutdown_deadline_ms, back.daemon.shutdown_deadline_ms);
    }
}

// ---------------------------------------------------------------------------------------------
// The derived risk-budget answer
// ---------------------------------------------------------------------------------------------

/// Every `[risk]` ceiling that refuses a live mount when absent has a reader, so
/// `live_risk_budget_missing` can never report a ceiling as permanently missing — which would turn
/// every live box into `LiveRefused`.
#[test]
fn every_refusing_risk_ceiling_has_a_reader() {
    let declared: Vec<&str> = vike_config::ceilings::PRE_TRADE_CEILINGS
        .iter()
        .filter(|c| {
            c.home == vike_config::ceilings::CeilingHome::RunProfileRisk
                && c.refuses_live_mount_when_absent
        })
        .map(|c| c.name)
        .collect();
    assert_eq!(
        declared,
        REFUSING_RISK_KEYS.to_vec(),
        "`PRE_TRADE_CEILINGS` gained or lost a run-profile ceiling that refuses a live mount. Add \
         (or remove) its arm in `profile_rows::risk_key_is_set` and re-pin `REFUSING_RISK_KEYS` — \
         an unread refusing ceiling reads as permanently missing, which refuses every live mount"
    );
    assert!(!declared.is_empty(), "the derivation must not silently answer nothing");
}

/// An absent run profile is every key missing — the same verdict
/// `vike_mount::require_live_risk_budget` gives when `make_engine`'s `risk_profile` is `None`.
#[test]
fn an_absent_run_profile_is_every_refusing_key_missing() {
    assert_eq!(live_risk_budget_missing(None), REFUSING_RISK_KEYS.to_vec());
    let rp = run(PROD2_SHAPED_RUN_PROFILE);
    assert!(live_risk_budget_missing(Some(&rp.risk)).is_empty(), "the fixture arms both caps");
}

/// A `mode = "backtest"` profile is refused for a live mount outright, through the SAME production
/// call the daemon's live arm makes.
#[test]
fn a_non_live_run_profile_refuses_the_live_mount_rather_than_merging() {
    // `[event_source]`/`[broker]` deleted with the fixture above — see `PROD2_SHAPED_RUN_PROFILE`.
    let rp = run("mode = \"backtest\"\n\n[risk]\n\
         max_notional_per_order = 100.0\nmax_total_exposure = 1000.0\n");
    let profile = daemon(PROD2_SHAPED_DAEMON_PROFILE);
    assert!(
        matches!(
            resolve_arming(true, Some(&rp), &profile),
            ArmingOutcome::LiveRefused(LiveRefusal::NotLiveMode(_))
        ),
        "a backtest profile's [risk] must not arm a live mount"
    );
}

/// `recorder` PARSES since 2026-09-16, and **this crate still must not load one as a daemon
/// profile.**
///
/// ⚠ This test used to be the other way round — it asserted that `ProfileKind::parse("recorder")`
/// was a NAMED refusal citing 0057's NO. The owner overruled that NO, so the word is readable now;
/// what this file is really protecting is not the word but the ROUTE: a recorder profile is the
/// DATA daemon's subscription list, and `rows_to_daemon_profile` here must never be handed one and
/// quietly build a mount set out of it. The kind is the discriminator that keeps those apart, so
/// the assertion moves from "the word is refused" to "the word is a DIFFERENT kind from this
/// crate's".
#[test]
fn a_recorder_profile_kind_is_not_a_daemon_profile() {
    let kind = ProfileKind::parse("recorder").expect("the owner overruled 0057's NO on 2026-09-16");
    assert_eq!(kind, ProfileKind::Recorder);
    assert_ne!(
        kind,
        ProfileKind::Daemon,
        "a recorder profile is the DATA daemon's subscription list — loading one as a mount set \
         would be this crate building a live mount out of a document that names venue FEEDS"
    );
    // …and a word neither side knows is still a NAMED refusal rather than a silent drop.
    let e = ProfileKind::parse("sweeper").expect_err("an unknown kind must not read as anything");
    assert!(e.to_string().contains("sweeper"), "names the word it could not read: {e}");
}

// ---------------------------------------------------------------------------------------------
// The mount names its class — 0061 phase 5
// ---------------------------------------------------------------------------------------------

/// ⚠ **THE CROSS-CRATE PIN: the Rust enum and the SQL `CHECK` cannot drift, and this is where that
/// is proved.**
///
/// It is proved by DERIVATION rather than by comparing two lists. `vike_secrets` declares NO
/// `vike-*` dependency at all — by design and by `crates/vike-boot/tests/dependency_floor.rs` — so
/// it cannot name `vike_model::AssetClass` from any layer, and its `profile_ddl` spells NO
/// asset-class word and renders the `CHECK` from the list it is handed. ⚠ This sentence used to
/// argue from LAYERS (leaf at 15, taxonomy at 20). The taxonomy has since moved down to
/// `vike-model` at layer 10, where the layer rule would permit the edge; it is the zero-dependency
/// floor, not the layering, that keeps the parameter shape necessary. `vike_tradehub::profile_rows::mount_asset_class_vocabulary` is the one production site
/// that hands one over, and it hands over `AssetClass::SQL_WORDS` — which the `asset_classes!` macro
/// expands from the enum's single declaration.
///
/// This crate is the only one in the workspace that can see BOTH, which is why the pin lives here —
/// the same reason `crates/vike-bridge-core/tests/settings_dir_spellings.rs` lives where it does.
/// Adding a twelfth variant changes the rendered schema with no edit in either place; what this
/// fails on is somebody re-introducing a hand-written list.
#[test]
fn mount_asset_class_vocabulary_is_the_enums_own() {
    use vike_model::AssetClass;
    assert_eq!(
        vike_tradehub::profile_rows::mount_asset_class_vocabulary(),
        AssetClass::SQL_WORDS,
        "the vocabulary handed to the schema must BE the enum's, not a copy of it"
    );

    // ...and the rendered CHECK carries exactly those words — every variant present, and nothing
    // that is not a variant.
    let ddl = vike_secrets::profile_store::profile_ddl(
        vike_tradehub::profile_rows::mount_asset_class_vocabulary(),
    )
    .expect("the enum's own words must render");
    for class in AssetClass::ALL {
        assert!(
            ddl.contains(&format!("'{}'", class.sql_word())),
            "{:?} is a variant and is missing from the schema's CHECK",
            class
        );
    }
    let clause = ddl
        .split("CHECK (asset_class IN (")
        .nth(1)
        .and_then(|s| s.split("))").next())
        .expect("the CHECK clause must be in the rendered schema");
    assert_eq!(
        clause.split(", ").count(),
        AssetClass::ALL.len(),
        "the CHECK carries a different number of words than the enum has variants: {clause}"
    );
}

/// **A mount that does not name its product is REFUSED, and nothing is stored.** 0061 phase 5's
/// *required, not nullable*: the TOML key stays optional so a profile written before it existed
/// keeps PARSING, and the bill comes due here, once, at the migration.
#[test]
fn a_mount_that_declares_no_asset_class_cannot_become_a_row() {
    let profile = daemon("venue = \"bybit\"\nsymbol = \"BTCUSDT\"\ninterval = \"1m\"\n");
    let e = daemon_profile_to_rows("unclassed", &profile)
        .expect_err("a mount ROW must say which product it trades");
    assert!(e.contains("asset_class"), "names the key: {e}");
    assert!(e.contains("bybit") && e.contains("BTCUSDT"), "names the mount: {e}");
    assert!(e.contains("CryptoPerp"), "shows the operator a word to type: {e}");
    assert!(e.contains("0061"), "cites the record that decided it: {e}");

    // ...and the same refusal reaches the migration PLAN, rather than a plan that stores less.
    assert!(plan_migration("unclassed", &profile, Some("unclassed"), None).is_err());
}

/// A word outside the vocabulary is refused BY NAME at the profile seam, before anything is written
/// — so the operator is told which key is wrong rather than being handed a SQL constraint error.
#[test]
fn an_unknown_asset_class_word_is_refused_by_name() {
    let profile = daemon("venue = \"bybit\"\nsymbol = \"BTCUSDT\"\nasset_class = \"perp\"\n");
    let e = daemon_profile_to_rows("typo", &profile).expect_err("`perp` is not a vike asset class");
    assert!(e.contains("\"perp\""), "names the offending word: {e}");
    assert!(e.contains("CryptoPerp"), "names the word that was meant: {e}");
}

/// **The class survives the whole round trip** — TOML → rows → the rendered document → the REAL
/// parser. The rendered `asset_class` line is unconditional, so a row can never come back as a
/// profile that no longer says what it trades.
#[test]
fn the_asset_class_survives_the_row_round_trip() {
    let original = daemon(
        "[[mounts]]\nvenue = \"bybit\"\nsymbol = \"BTCUSDT\"\nasset_class = \"CryptoPerp\"\n\n\
         [[mounts]]\nvenue = \"deribit\"\nsymbol = \"BTC-27JUN25-60000-C\"\n\
         asset_class = \"Option\"\n",
    );
    let stored = daemon_profile_to_rows("rt", &original).expect("lowers");
    assert_eq!(stored.mounts[0].asset_class, "CryptoPerp");
    assert_eq!(stored.mounts[1].asset_class, "Option");

    let back = rows_to_daemon_profile(&stored).expect("reloads through the real parser");
    let words: Vec<Option<String>> =
        back.mount_rows().iter().map(|m| m.asset_class.clone()).collect();
    assert_eq!(
        words,
        vec![Some("CryptoPerp".to_string()), Some("Option".to_string())],
        "the rendered document must carry the class back, or the round trip loses it"
    );
}

/// Every word the enum has is one a profile can actually declare — the two ends of the seam agree
/// about the vocabulary, not just about its length.
#[test]
fn every_asset_class_variant_is_declarable_in_a_profile() {
    for class in vike_model::AssetClass::ALL {
        let text =
            format!("venue = \"bybit\"\nsymbol = \"X\"\nasset_class = \"{}\"\n", class.sql_word());
        let profile = daemon(&text);
        let stored = daemon_profile_to_rows("v", &profile)
            .unwrap_or_else(|e| panic!("{:?} must be declarable: {e}", class));
        assert_eq!(stored.mounts[0].asset_class, class.sql_word());
    }
}

// ---------------------------------------------------------------------------------------------
// THE RUN PLANE — the rows that BIND, and the refusal they must not be able to remove
// ---------------------------------------------------------------------------------------------

/// A `run`-kind `StoredProfile` from `(path, TOML-rendered value)` pairs — the shape
/// `vike-cli config mirror --profile` writes, built by hand here so no assertion below depends on
/// a filesystem or on the CLI.
fn run_rows(name: &str, settings: &[(&str, &str)]) -> vike_secrets::profile_store::StoredProfile {
    vike_secrets::profile_store::StoredProfile {
        row: vike_secrets::profile_store::ProfileRow {
            name: name.to_string(),
            kind: ProfileKind::Run,
            active: false,
            note: None,
        },
        mounts: Vec::new(),
        params: std::collections::BTreeMap::new(),
        settings: settings.iter().map(|(p, v)| ((*p).to_string(), (*v).to_string())).collect(),
        recorder: None,
    }
}

/// the CI box's `settings/run-live.toml`, as rows: `mode` plus the two ceilings.
fn prod2_run_rows() -> vike_secrets::profile_store::StoredProfile {
    run_rows(
        "run-live",
        &[
            ("mode", "\"live\""),
            ("risk.max_notional_per_order", "100.0"),
            ("risk.max_total_exposure", "500.0"),
        ],
    )
}

/// **EVERY KEY OF THE LIVE RUN PROFILE SURVIVES the rows and comes back through the REAL parser.**
/// Named individually rather than counted: a count passes over a swap.
#[test]
fn a_run_profile_round_trips_through_the_rows_and_back_through_the_real_parser() {
    let from_rows = rows_to_run_profile(&prod2_run_rows()).expect("the stored body must reload");
    let from_file = run(PROD2_SHAPED_RUN_PROFILE);
    assert_eq!(from_rows.mode, vike_core::run_profile::Mode::Live, "`mode` survived");
    assert_eq!(from_rows.risk.max_notional_per_order, Some(100.0));
    assert_eq!(from_rows.risk.max_total_exposure, Some(500.0));
    assert_eq!(
        from_rows.mode, from_file.mode,
        "the row body and the CI box's own file agree about the one key that gates a live mount"
    );
    // ...and every table the file does not set stays unset rather than acquiring a default the
    // operator never typed.
    assert_eq!(from_rows.sinks, vike_core::run_profile::Sinks::default());
    assert_eq!(from_rows.guards, vike_core::run_profile::Guards::default());
    assert!(from_rows.sinks.journal_config().is_none(), "no [sinks.journal] means no WAL");
}

/// **THE ONE THAT MATTERS FOR THE RUN PLANE: a box that is LIVE today is live after.**
///
/// Two comparisons, because neither is the whole claim:
///
/// * `ArmingOutcome` — the value the daemon logs at startup — resolved once from the FILE-loaded
///   run profile and once from the ROW-loaded one, against the same daemon profile and the same
///   live flag. That answers WHETHER it arms and on WHICH mounts.
/// * the `[risk]` table itself, field for field. ⚠ **This half did not exist and the assert
///   message claimed it did.** `ArmingOutcome::Live { primary, mounts }` carries no risk numbers
///   at all, so *"reading the run profile from ROWS changed what this box trades"* passed on any
///   non-`None` pair — a WIDENED ceiling included. It was not hypothetical either: the two
///   fixtures disagreed about `max_total_exposure` (`1000.0` here against the rows' `500.0`) for
///   as long as the test existed, and it was green. See [`PROD2_SHAPED_RUN_PROFILE`].
///
/// `ProfileRisk` derives `PartialEq`, so the second comparison is the WHOLE table rather than the
/// two ceilings a sample would name — a `[risk]` key added to that type and dropped by the
/// migration fails here without anybody remembering to list it.
#[test]
fn a_prod2_shaped_live_box_arms_identically_from_the_run_rows() {
    let profile = daemon(PROD2_SHAPED_DAEMON_PROFILE);
    let from_file = run(PROD2_SHAPED_RUN_PROFILE);
    let before = resolve_arming(true, Some(&from_file), &profile);
    assert!(
        matches!(&before, ArmingOutcome::Live { primary, .. } if primary == "bybit/BTCUSDT"),
        "the pre-migration state must resolve LIVE, or this test measures the wrong box: {before:?}"
    );
    let from_rows = rows_to_run_profile(&prod2_run_rows()).expect("the stored body must reload");
    let after = resolve_arming(true, Some(&from_rows), &profile);
    assert_eq!(
        before, after,
        "reading the run profile from ROWS changed WHETHER this box arms, or WHICH mounts it arms"
    );
    assert_eq!(
        from_rows.risk, from_file.risk,
        "reading the run profile from ROWS changed the pre-trade CEILINGS this box judges every \
         order against, which is the one thing this migration may not do — and which the \
         ArmingOutcome comparison above cannot see, because that value carries no risk numbers"
    );
}

/// **⚠ THE REFUSAL, KILL-PROVED ON THE ROW PATH.** `run-live.toml`'s own comment says *"A live
/// mount REFUSES TO START without it"*, and the whole hazard of this migration is a row plane that
/// resolves to "no ceiling" where the file plane refused.
///
/// Three unresolvable shapes, one verdict each, and none of them is `Live`:
///
/// 1. a `run` body with NO `[risk]` table at all — the shape a half-filled migration leaves;
/// 2. a body carrying only ONE of the two refusing ceilings — the shape a typo leaves;
/// 3. no run profile at all, which is the state every unmigrated box is in and the one the row
///    path must reproduce exactly.
///
/// The missing-key list is DERIVED from `vike_config::ceilings::PRE_TRADE_CEILINGS`
/// (`live_risk_budget_missing`), so this cannot pass by agreeing with a second copy of the roster.
#[test]
fn a_run_body_that_cannot_supply_the_budget_refuses_the_live_mount_rather_than_defaulting() {
    let profile = daemon(PROD2_SHAPED_DAEMON_PROFILE);

    // 1 — a body with `mode` and nothing else.
    let bare = rows_to_run_profile(&run_rows("bare", &[("mode", "\"live\"")]))
        .expect("a profile that sets no ceiling is a VALID profile — it just cannot arm one");
    assert_eq!(bare.risk.max_notional_per_order, None);
    assert_eq!(bare.risk.max_total_exposure, None);
    assert_eq!(
        resolve_arming(true, Some(&bare), &profile),
        ArmingOutcome::LiveRefused(LiveRefusal::MissingRiskBudget(REFUSING_RISK_KEYS.to_vec())),
        "a row body with no ceilings must REFUSE the mount, never fall back to a permissive default"
    );

    // 2 — half a budget. The refusal must name the key that is missing and not the one that is set.
    let half = rows_to_run_profile(&run_rows(
        "half",
        &[("mode", "\"live\""), ("risk.max_notional_per_order", "100.0")],
    ))
    .expect("loads");
    assert_eq!(
        live_risk_budget_missing(Some(&half.risk)),
        vec!["max_total_exposure"],
        "exactly the unset refusing ceiling, derived from PRE_TRADE_CEILINGS"
    );
    assert!(
        matches!(
            resolve_arming(true, Some(&half), &profile),
            ArmingOutcome::LiveRefused(LiveRefusal::MissingRiskBudget(_))
        ),
        "half a budget is still a refusal"
    );

    // 3 — no profile at all: the state of every box that has not crossed, unchanged.
    assert_eq!(
        resolve_arming(true, None, &profile),
        ArmingOutcome::LiveRefused(LiveRefusal::MissingRiskBudget(REFUSING_RISK_KEYS.to_vec())),
        "absence still resolves to absence"
    );
}

/// A row body whose `mode` is not `live` is REFUSED for a live mount rather than merged — the same
/// refusal `RunProfile::risk_for_live_venue_mount` makes for a file, reached through the row path.
/// That is why `mode` is a stored row at all: the retired `profile_risk` mirror carried none, so it
/// could never have answered "may this arm a live mount".
#[test]
fn a_non_live_run_body_refuses_the_live_mount_through_the_row_path_too() {
    let profile = daemon(PROD2_SHAPED_DAEMON_PROFILE);
    let paper = rows_to_run_profile(&run_rows(
        "paper",
        &[
            ("mode", "\"paper\""),
            ("risk.max_notional_per_order", "100.0"),
            ("risk.max_total_exposure", "500.0"),
        ],
    ))
    .expect("a paper profile is a valid profile");
    assert!(paper.risk_for_live_venue_mount().is_err(), "the live arm refuses it outright");
    assert!(
        matches!(
            resolve_arming(true, Some(&paper), &profile),
            ArmingOutcome::LiveRefused(LiveRefusal::NotLiveMode(_))
        ),
        "and the folded outcome says WHY, rather than reporting a missing budget"
    );
}

/// **A row body that cannot be MATERIALISED is an `Err`, never a silent `None`.** The daemon turns
/// this into `ExitCode::FAILURE`; what is proven here is the half a pure test can prove — that the
/// function refuses rather than returning a profile with the bad key dropped.
///
/// Two shapes, and the first is the one that matters: a `profile_setting` path naming nothing real
/// is caught by `RunProfile`'s own `deny_unknown_fields`, which is why the renderer needs no key
/// vocabulary of its own.
#[test]
fn an_unreadable_run_body_refuses_rather_than_dropping_the_key_it_cannot_read() {
    let e = rows_to_run_profile(&run_rows(
        "typo",
        &[("mode", "\"live\""), ("risk.max_notionl_per_order", "100.0")],
    ))
    .expect_err("an unknown `[risk]` key must refuse, or a ceiling silently vanishes");
    assert!(e.contains("max_notionl_per_order"), "the refusal names the key: {e}");

    // ...and a body with no `mode` at all: the field has no serde default, so it cannot be dropped.
    let e = rows_to_run_profile(&run_rows("nomode", &[("risk.max_total_exposure", "500.0")]))
        .expect_err("`mode` is required");
    assert!(e.contains("mode"), "{e}");

    // ⚠ And the tombstones are refused through the row path too, in `validate`'s own words — a row
    // for a table the daemon DELETES would read as more authoritative than the file line it
    // replaced.
    let e =
        rows_to_run_profile(&run_rows("dead", &[("mode", "\"live\""), ("broker.kind", "\"x\"")]))
            .expect_err("`[broker]` is a tombstone");
    assert!(e.contains("broker"), "{e}");
}

/// **A box with NO rows keeps working off its file, byte-identically** — the property every other
/// box and every CI lane depends on. Asserted at the SELECTION seam, which is where a regression
/// would actually show up.
#[test]
fn with_no_active_row_the_file_rungs_still_decide_the_run_profile() {
    let vars: HashMap<String, String> =
        [("VIKE_RUN_PROFILE".to_string(), "/p/settings/run-live.toml".to_string())]
            .into_iter()
            .collect();

    let no_row = select_run_profile(None, None, &vars);
    assert_eq!(no_row.value.as_deref(), Some("/p/settings/run-live.toml"));
    assert_eq!(no_row.source, Some(vike_secrets::profile_store::SelectionSource::EnvVar));
    assert!(!no_row.row_shadowed_something(), "nothing was shadowed, so nothing is warned about");

    // ...and an explicit `--profile` still beats the variable, exactly as `resolve_profile` does.
    let explicit = select_run_profile(None, Some("/no/such/other.toml"), &vars);
    assert_eq!(explicit.value.as_deref(), Some("/no/such/other.toml"));

    // ...while an ACTIVE ROW beats both and SAYS SO, which is the disclosure the ruling owes.
    let row = select_run_profile(Some("run-live"), Some("/no/such/other.toml"), &vars);
    assert_eq!(row.value.as_deref(), Some("run-live"));
    assert!(
        row.row_shadowed_something(),
        "the daemon must WARN when a row shadowed a live setting"
    );
    assert_eq!(row.shadowed.len(), 2, "both file rungs are named as shadowed: {row:?}");
}

/// **THE JOURNAL SHORT-CIRCUIT.** `vike_core::journal_config_from` re-opens the file
/// `VIKE_RUN_PROFILE` names, so with a row active and that variable still set the startup line
/// would say the variable "no longer decides anything" while it still decided the WAL sink.
/// `journal_config_for` is the fix, and this is the difference it makes.
#[test]
fn the_journal_sink_follows_the_resolved_profile_rather_than_the_environment() {
    // ⚠ **`VIKE_RUN_PROFILE` is deliberately ABSENT here, and this test used to set it** — to
    // `/no/such/run-live.toml`, which made the first assertion below unfalsifiable: with that
    // variable set `journal_config_from` short-circuits on it, fails to load the path, warns, and
    // answers `None` — so the OLD code and the new one agreed and the assert could not fail for
    // the reason it states. With the variable absent, the directory rung is LIVE: the line below
    // proves `journal_config_from` answers `Some`, which is what makes the profile arm's `None` a
    // measurement rather than a coincidence.
    let mut vars: HashMap<String, String> = HashMap::new();
    vars.insert("VIKE_JOURNAL_DIR".to_string(), "/no/such/journal-dir".to_string());
    let from_env = vike_core::journal_config_from(&vars)
        .expect("the ENV rung must be armed, or the next assertion measures nothing");
    assert_eq!(
        from_env.dir,
        std::path::PathBuf::from("/no/such/journal-dir"),
        "…and it is the directory the variable names"
    );

    // With a resolved profile in hand, the profile decides — and this one names no `[sinks]`, so
    // the answer is "no WAL" and NOT the directory the live variable names.
    let resolved = rows_to_run_profile(&prod2_run_rows()).expect("loads");
    assert!(
        vike_tradehub::profile_rows::journal_config_for(Some(&resolved), &vars).is_none(),
        "the resolved profile says no journal, so no journal — whatever VIKE_JOURNAL_DIR holds"
    );

    // ⚠ …AND THE OPERATOR IS TOLD. A rung that silently stops deciding is this whole phase's own
    // hazard: the migration's completion path (activate the row, drop the shadowed
    // `VIKE_RUN_PROFILE=` line) leaves a box in exactly this state, and a box that was journalling
    // from the directory would lose the write-ahead journal with no warning and no log line.
    let note = vike_tradehub::profile_rows::journal_rung_shadowed(Some(&resolved), &vars)
        .expect("a silenced directory rung must be disclosed");
    for needle in ["/no/such/journal-dir", "OFF", "sinks.journal"] {
        assert!(note.contains(needle), "the disclosure must name {needle:?}: {note}");
    }

    // ...and the profile arm is byte-identical to what `journal_config_from` computes from a
    // profile, which is the claim that makes this change a no-op on every box that has not crossed.
    let with_journal = rows_to_run_profile(&run_rows(
        "j",
        &[("mode", "\"paper\""), ("sinks.journal.dir", "\"/no/such/vike-wal\"")],
    ))
    .expect("loads");
    let via_helper =
        vike_tradehub::profile_rows::journal_config_for(Some(&with_journal), &HashMap::new())
            .expect("this profile names a journal directory");
    let direct =
        with_journal.sinks.journal_config().expect("…and `Sinks::journal_config` agrees it does");
    // ⚠ Field by field rather than `assert_eq!`: `vike_core::JournalConfig` derives no `PartialEq`,
    // so a whole-value comparison does not compile. Every field it has is named here, which is what
    // makes this a comparison rather than a sample — a field added to that struct and forgotten
    // here is a `non_exhaustive`-free struct literal away from being caught by the compiler.
    assert_eq!(via_helper.dir, direct.dir);
    assert_eq!(via_helper.snapshot_every, direct.snapshot_every);
    assert_eq!(via_helper.file.segment_bytes, direct.file.segment_bytes);
    assert_eq!(via_helper.file.flush_every, direct.file.flush_every);
}

/// **The disclosure is SILENT on every box the two rungs cannot disagree on**, which is every box
/// today and every CI lane — a warning that fires where nothing changed is the same defect as a
/// silence where something did, wearing the other sign.
#[test]
fn the_journal_disclosure_is_silent_where_the_two_rungs_cannot_disagree() {
    use vike_tradehub::profile_rows::journal_rung_shadowed;
    let resolved = rows_to_run_profile(&prod2_run_rows()).expect("loads");
    let dir = |v: &str| -> HashMap<String, String> {
        [("VIKE_JOURNAL_DIR".to_string(), v.to_string())].into_iter().collect()
    };

    // No profile resolved: `journal_config_for` falls to `journal_config_from`, which is what it
    // always did, so there is nothing to disclose.
    assert_eq!(journal_rung_shadowed(None, &dir("/some/where")), None);

    // No directory rung armed — `deploy/vike-tradehub.service` comments its
    // `Environment=VIKE_JOURNAL_DIR=` out and sets no `config.journal_dir`, so this is the CI box.
    assert_eq!(journal_rung_shadowed(Some(&resolved), &HashMap::new()), None);
    assert_eq!(journal_rung_shadowed(Some(&resolved), &dir("   ")), None, "blank is not a rung");

    // `VIKE_RUN_PROFILE` set: `journal_config_from` short-circuits on it and never reads the
    // directory, so the directory was already deciding nothing BEFORE the row plane existed.
    // Reporting it here would blame this landing for a state that predates it.
    let mut with_env_profile = dir("/some/where");
    with_env_profile.insert("VIKE_RUN_PROFILE".to_string(), "/no/such/run-live.toml".to_string());
    assert_eq!(journal_rung_shadowed(Some(&resolved), &with_env_profile), None);

    // …and the OTHER direction of the divergence IS reported: a profile that names a journal
    // writes somewhere the directory does not.
    let with_journal = rows_to_run_profile(&run_rows(
        "j",
        &[("mode", "\"paper\""), ("sinks.journal.dir", "\"/no/such/vike-wal\"")],
    ))
    .expect("loads");
    let note = journal_rung_shadowed(Some(&with_journal), &dir("/some/where"))
        .expect("two rungs naming two directories is a divergence worth a line");
    assert!(note.contains("/no/such/vike-wal"), "it names the winner: {note}");
    assert!(note.contains("/some/where"), "…and the loser: {note}");
}
