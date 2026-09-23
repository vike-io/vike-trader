//! **The STORE folds ruling 10's `venue_setting` rows — the proof, and the two measured defects it
//! closes.**
//!
//! Ruling 10 moved ten config-shaped names out of the `credential` table into `venue_setting`.
//! Every reader keeps looking the LEGACY name up, so something has to render a row back into those
//! names. Until 2026-09-22 that renderer lived in `vike_bridge_core::credentials` — layer 30 —
//! which meant the fold could only be applied by a caller that had already reached up there. Two
//! of the three readers had not, and both were measured on the live box:
//!
//! | reader | what it did before this fold moved down |
//! |---|---|
//! | `vike_secrets::load_workspace_dotenv_from` | `crates/bridges/vike-ibkr/tests/ibkr_mktdata_smoke.rs` read the right store, found no `IBKR_DEMO_PORT`, fell back to its built-in demo default against a gateway listening elsewhere, and SELF-SKIPPED while printing `test result: ok. 1 passed` |
//! | `vike_secrets::load_workspace_dotenv_scoped` | `crates/bridges/polymarket/src/egress.rs`'s `dotenv_proxy_vars` reads through it, so the operator's three `POLY_PROXY_*` rows were read by NOTHING and the built-in defaults stayed in force with no error anywhere |
//! | `vike_bridge_core::credentials::load_workspace_secrets_at_checked` | folded, because it is above the renderer |
//!
//! ⚠ **Every test here would still pass if the fold ran only in `vike-bridge-core`, EXCEPT that it
//! reaches the store through `vike-secrets` alone.** This crate cannot link `vike-bridge-core`
//! (dependencies run down only), so a fold that had not moved is structurally unreachable from
//! this file — which is what makes a green here evidence about the layer rather than about the
//! composition.
//!
//! | test | what would be true without it |
//! |---|---|
//! | [`a_moved_row_answers_through_the_plain_reader`] | the ibkr smoke's silent self-skip, unfixed |
//! | [`a_moved_row_answers_through_the_scoped_reader`] | the polymarket proxy rows, still read by nothing |
//! | [`the_scoped_fold_does_not_widen_past_its_scope`] | a scoped read whose fold reached names it never declared — ⚠ but read that test's own ⚠ before trusting it: it is the CONTROL, and the claim its name suggests is one that NO test against this crate's public API can make. Two mutation runs measured that, and the gate is what enforces it |
//! | [`a_moved_one_to_many_row_answers_for_both_legacy_names`] | one of dukascopy's two accounts silently on the default JNLP |
//! | [`a_collision_keeps_the_credential_value_and_reports_the_name`] | a half-done move resolved by insertion order, silently |
//! | [`the_node_key_table_never_gains_a_venue_name`] | `docs/decisions/0051`'s namespace split, undone by the fold |
//! | [`a_box_with_no_database_is_byte_identical`] | a fold that changed what an unmigrated box resolves |
//!
//! ⚠ Values are obviously fake. No real credential exists anywhere near this file.

use std::path::{Path, PathBuf};

use vike_secrets::{KeyScope, Lookup, Table};

// ---------------------------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------------------------

/// A settings directory with a credential file, migrated into a database.
struct Fixture {
    _dir: tempfile::TempDir,
    settings: PathBuf,
}

impl Fixture {
    /// A migrated store holding exactly `credentials`, plus a node file so the node table is not
    /// empty (the namespace test needs something on the other side of the split).
    fn migrated(credentials: &[(&str, &str)]) -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let settings = dir.path().join("settings");
        std::fs::create_dir_all(&settings).expect("settings dir");
        write_store(&settings.join("secrets.env"), credentials);
        write_store(&settings.join("node.env"), &[(NODE_KEY, "node-value")]);
        let fx = Fixture { _dir: dir, settings };
        if let Err(e) = vike_secrets::migrate(fx.arg(), is_node_key, &classify) {
            panic!("migration refused: {e}");
        }
        fx
    }

    /// The same, with NO migration — a box still on `Backend::Files`.
    fn files_only(credentials: &[(&str, &str)]) -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let settings = dir.path().join("settings");
        std::fs::create_dir_all(&settings).expect("settings dir");
        write_store(&settings.join("secrets.env"), credentials);
        Fixture { _dir: dir, settings }
    }

    fn dir(&self) -> &Path {
        &self.settings
    }

    fn arg(&self) -> Option<&str> {
        Some(self.settings.to_str().expect("utf-8 temp path"))
    }

    /// Plant one `venue_setting` row — the state a completed `secrets move-venue-config` leaves.
    fn plant(&self, venue: &str, tier: Option<&str>, field: &str, value: &str) {
        vike_secrets::set_venue_setting_in(self.dir(), venue, tier, field, value)
            .unwrap_or_else(|e| panic!("planting ({venue}, {tier:?}, {field}) failed: {e}"));
    }

    /// The whole-map read every composition root performs, through `vike-secrets` alone.
    fn plain(&self) -> std::collections::HashMap<String, String> {
        vike_secrets::load_workspace_dotenv_from(self.arg())
    }

    /// The SCOPED read `egress.rs` performs, through `vike-secrets` alone.
    ///
    /// `resolve_project_scoped` rather than `load_workspace_dotenv_scoped` for ONE reason: the
    /// latter takes no settings-directory override and walks from the working directory, which a
    /// test cannot redirect without mutating process state. The two reach the identical
    /// `resolve_store_scoped_in` — `crates/vike-secrets/src/dotenv.rs`'s
    /// `load_workspace_dotenv_scoped` is that call plus an infallible degradation — so this drives
    /// the code the defect was in.
    fn scoped(&self, names: &[&str]) -> vike_secrets::ScopedSecrets {
        vike_secrets::resolve_project_scoped(self.arg(), &KeyScope::of(names))
            .expect("the store must open")
    }
}

fn write_store(path: &Path, rows: &[(&str, &str)]) {
    let mut text =
        String::from("# a hand-edited store — comments and order are the operator's\n\n");
    for (k, v) in rows {
        text.push_str(&format!("{k}={v}\n"));
    }
    std::fs::write(path, text).expect("write store");
}

/// The one node key this fixture plants, so `docs/decisions/0051`'s namespace has a row in it.
const NODE_KEY: &str = "VIKE_TRADEHUB_OBSERVE_KEY";

fn is_node_key(key: &str) -> bool {
    key == NODE_KEY
}

/// A deliberately SIMPLER classifier than the production one, for the reason
/// `crates/vike-secrets/tests/database_migration.rs`'s own `classify` gives: this crate cannot link
/// the crate that owns the real tables, and every assertion here is about what the FOLD does with a
/// planted row rather than about how a name was classified on the way in.
fn classify(name: &str) -> vike_secrets::Classification {
    use vike_secrets::{AccountKey, Classification};
    for (prefix, venue, tier) in [
        ("IBKR_DEMO_", "ibkr", "demo"),
        ("DUKASCOPY_DEMO1_", "dukascopy", "demo"),
        ("DUKASCOPY_DEMO2_", "dukascopy", "demo"),
        ("POLY_", "polymarket", "live"),
    ] {
        if let Some(field) = name.strip_prefix(prefix) {
            return Classification {
                placement: vike_secrets::Placement::Account(AccountKey {
                    venue: venue.to_string(),
                    tier: tier.to_string(),
                    label: None,
                    // The two dukascopy rows are TWO accounts of one venue at one tier — without
                    // the discriminator the migration refuses them as ambiguous.
                    discriminator: name
                        .starts_with("DUKASCOPY_DEMO1_")
                        .then(|| "DEMO1".to_string())
                        .or_else(|| {
                            name.starts_with("DUKASCOPY_DEMO2_").then(|| "DEMO2".to_string())
                        }),
                }),
                field: field.to_string(),
                secret: true,
                recognised: true,
                pending_move: None,
            };
        }
    }
    Classification::unrecognised(name)
}

// ---------------------------------------------------------------------------------------------
// The two measured defects
// ---------------------------------------------------------------------------------------------

/// ⚠ **THE IBKR SMOKE'S DEFECT, as a test.** A value that has moved into `venue_setting` must come
/// back from the plain reader under its LEGACY name. Without the fold this reader answers `None`,
/// which every caller reads as *not configured* — and "not configured" is a legitimate state, so
/// nothing logs, nothing errors, and a smoke self-skips while printing `ok`.
#[test]
fn a_moved_row_answers_through_the_plain_reader() {
    let fx = Fixture::migrated(&[("IBKR_DEMO_HOST", "127.0.0.1")]);
    fx.plant("ibkr", Some("demo"), "PORT", "4102");

    let vars = fx.plain();
    assert_eq!(
        vars.get("IBKR_DEMO_PORT").map(String::as_str),
        Some("4102"),
        "a moved row must answer under its legacy name: {:?}",
        vars.keys().collect::<Vec<_>>()
    );
    // …and the credential rows are untouched beside it.
    assert_eq!(vars.get("IBKR_DEMO_HOST").map(String::as_str), Some("127.0.0.1"));
}

/// ⚠ **THE POLYMARKET EGRESS DEFECT, as a test.** The proxy family reaches the store through the
/// SCOPED reader and through nothing else, and that reader has its own branch —
/// `read_table_scoped` binds the declared names rather than reading the table — so a fold applied
/// only to the whole-map path leaves this one blind.
#[test]
fn a_moved_row_answers_through_the_scoped_reader() {
    let fx = Fixture::migrated(&[("POLY_PRIVATE_KEY", "not-a-real-key")]);
    fx.plant("polymarket", None, "PROXY_HOST", "an-example-proxy-host");

    let scoped = fx.scoped(&["POLY_PROXY_HOST"]);
    assert_eq!(
        scoped.get("POLY_PROXY_HOST"),
        Lookup::Present("an-example-proxy-host"),
        "the machine-scoped proxy row must answer under its legacy name"
    );
}

/// **Only declared names reach the answer** — the control beside the scoped fold.
///
/// # ⚠ THIS TEST DOES NOT KILL THE UNSCOPED FOLD, AND THAT IS MEASURED, NOT ASSUMED
///
/// The obvious claim to make here is *the fix did not undo the owner's 2026-09-16 narrowing*.
/// **No test written against this crate's public API can make it**, and two mutation runs on
/// 2026-09-22 are what established that rather than an argument:
///
/// * replacing `Some(scope)` with `None` in `crates/vike-secrets/src/store.rs`'s
///   `resolve_store_scoped_in` left every assertion below GREEN — `ScopedSecrets::narrow` filters
///   to the scope AFTER the fold, so the returned map is identical either way;
/// * a SECOND attempt planted a half-done move on a name outside the scope, expecting the
///   `collisions` list to widen, and stayed green too. The database arm's base map comes from
///   `crate::read_table_scoped`, which BINDS the declared names — so an unscoped fold has no
///   undeclared credential row to collide with, and the list is identical as well. The FILES arm
///   does read the whole file, but it is by construction the arm with NO database, so it has no
///   `venue_setting` rows to fold at all.
///
/// What the scope buys is therefore a MATERIALISATION property and nothing else: without it every
/// venue setting on the box is written into a map this process holds, briefly. That is exactly
/// what the ruling is about — *a core dump, a panic payload or a future logging bug reaches them
/// all* — and exactly what no return value can show.
/// `crates/vike-ops/tests/smoke_store_parity_gate.rs`'s `the_scoped_fold_is_handed_its_scope` is
/// the ONLY thing that kills it, and it does: MEASURED red under the first mutation above.
///
/// So read this test as what it is — the scoped reader folds AT ALL, and the answer holds the
/// declared names and no others — and read the gate as the enforcement of the rest.
#[test]
fn the_scoped_fold_does_not_widen_past_its_scope() {
    let fx = Fixture::migrated(&[("POLY_PRIVATE_KEY", "not-a-real-key")]);
    fx.plant("polymarket", None, "PROXY_HOST", "an-example-proxy-host");
    fx.plant("polymarket", None, "PROXY_PORT", "1080");
    fx.plant("ibkr", Some("demo"), "PORT", "4102");

    let scoped = fx.scoped(&["POLY_PROXY_HOST"]);
    // It folded — the control that keeps the rest of this test from passing vacuously.
    assert_eq!(scoped.get("POLY_PROXY_HOST"), Lookup::Present("an-example-proxy-host"));
    // …and nothing else reached the answer. See the ⚠ above for what this cannot see.
    assert_eq!(
        scoped.names().collect::<Vec<_>>(),
        vec!["POLY_PROXY_HOST"],
        "a process that declared one name must materialise one name"
    );
    for undeclared in ["POLY_PROXY_PORT", "IBKR_DEMO_PORT"] {
        assert!(
            scoped.get(undeclared).is_undeclared(),
            "{undeclared} was folded into a scope that never asked for it"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// The fold's own properties — re-homed from `crate::db`'s `read_table_folded` tests, which went
// with that function on 2026-09-22. They are the same claims, driven through the REAL renderer
// instead of a stub that could only ever agree with itself.
// ---------------------------------------------------------------------------------------------

/// ⚠ **THE ONE-TO-MANY ROW.** The ten moving names are NINE rows, because dukascopy's two demo
/// SERVER keys hold one value and collapse onto one row. A fold that answered ONE name would drop
/// a key the operator wrote, and one of the two accounts would fall back to the default demo JNLP
/// in silence — a working mount with a changed meaning.
#[test]
fn a_moved_one_to_many_row_answers_for_both_legacy_names() {
    let fx =
        Fixture::migrated(&[("DUKASCOPY_DEMO1_LOGIN", "one"), ("DUKASCOPY_DEMO2_LOGIN", "two")]);
    fx.plant("dukascopy", Some("demo"), "SERVER", "jforex-demo");

    let vars = fx.plain();
    for name in ["DUKASCOPY_DEMO1_SERVER", "DUKASCOPY_DEMO2_SERVER"] {
        assert_eq!(
            vars.get(name).map(String::as_str),
            Some("jforex-demo"),
            "ONE row must answer for BOTH legacy names: {:?}",
            vars.keys().collect::<Vec<_>>()
        );
    }
    // The credential rows beside them are untouched.
    assert_eq!(vars.get("DUKASCOPY_DEMO1_LOGIN").map(String::as_str), Some("one"));
    assert_eq!(vars.get("DUKASCOPY_DEMO2_LOGIN").map(String::as_str), Some("two"));
}

/// ⚠ **THE HALF-DONE STATE IS REPORTED, AND `credential` WINS.** A `venue_setting` row rendering a
/// name a live credential row still holds resolves to what the box resolved BEFORE the move — the
/// only choice that cannot alter live behaviour while the two tables disagree. The finding comes
/// back as DATA because this crate carries no logging dependency.
#[test]
fn a_collision_keeps_the_credential_value_and_reports_the_name() {
    let fx = Fixture::migrated(&[("DUKASCOPY_DEMO1_SERVER", "from-credential")]);
    fx.plant("dukascopy", Some("demo"), "SERVER", "from-settings");

    let resolved =
        vike_secrets::resolve_store_in(fx.dir(), Table::Credential).expect("the store must open");
    assert_eq!(
        resolved.collisions,
        vec!["DUKASCOPY_DEMO1_SERVER".to_string()],
        "the half-done move is reported BY NAME"
    );
    let map = resolved.secrets.into_map();
    assert_eq!(
        map.get("DUKASCOPY_DEMO1_SERVER").map(String::as_str),
        Some("from-credential"),
        "the pre-move value wins while the tables disagree"
    );
    // The name with no credential row folds in normally — the move is HALF done, not undone.
    assert_eq!(map.get("DUKASCOPY_DEMO2_SERVER").map(String::as_str), Some("from-settings"));
}

/// ⚠ **`docs/decisions/0051`: a node key and a venue credential are different NAMESPACES.** No
/// renderer produces a node name, and folding a rendered one into that table would be exactly the
/// shared-probe defect that record was written to remove.
#[test]
fn the_node_key_table_never_gains_a_venue_name() {
    let fx = Fixture::migrated(&[("IBKR_DEMO_HOST", "127.0.0.1")]);
    fx.plant("ibkr", Some("demo"), "PORT", "4102");

    let node =
        vike_secrets::resolve_store_in(fx.dir(), Table::NodeKey).expect("the store must open");
    assert!(node.collisions.is_empty(), "nothing renders a node name: {:?}", node.collisions);
    let names: Vec<&str> = node.secrets.keys().collect();
    assert_eq!(names, vec![NODE_KEY], "the node namespace holds its own key and no other");
}

/// A box that has not migrated has no `venue_setting` table to read, so the fold is a no-op BY
/// CONSTRUCTION rather than by a backend test — which is why `resolve_store_in` runs it on both
/// arms instead of only the database one. This is what says so.
#[test]
fn a_box_with_no_database_is_byte_identical() {
    let fx = Fixture::files_only(&[("IBKR_DEMO_HOST", "127.0.0.1")]);
    let resolved =
        vike_secrets::resolve_store_in(fx.dir(), Table::Credential).expect("the store must open");
    assert!(resolved.collisions.is_empty());
    assert_eq!(
        resolved.secrets.keys().collect::<Vec<_>>(),
        vec!["IBKR_DEMO_HOST"],
        "an unmigrated box resolves exactly what its file holds"
    );
}
