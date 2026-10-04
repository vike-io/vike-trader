use super::*;

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
}

/// The whole precedence chain. No longer ONE test function out of necessity: the old version
/// had to be, because it MUTATED the process environment and a sibling test asserting the
/// unset-fallback concurrently would have raced its `set_var`. The map parameter removes both
/// the race and the mutation.
#[test]
fn store_root_precedence_explicit_then_env_then_repo_root() {
    // 1. Explicit (--store) wins over everything, env set or not.
    assert_eq!(
        store_root(Some(PathBuf::from("explicit")), &env(&[("VIKE_HIST_STORE", "env-store")])),
        PathBuf::from("explicit")
    );

    // 2. The env var, when no explicit path is given.
    assert_eq!(
        store_root(None, &env(&[("VIKE_HIST_STORE", "env-store")])),
        PathBuf::from("env-store")
    );

    // 3. Neither: the REPO-root default `<repo>/market_data/hist`, anchored two levels above this
    //    crate's manifest dir — never CWD (the cheap_np bins' original bug).
    let expected = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .join("market_data")
        .join("hist");
    assert_eq!(store_root(None, &env(&[])), expected);
}

/// **A BLANK `--store=` must fall through, not resolve the store to `""`.**
///
/// This became reachable the moment `vike_analytics::binutil::arg` learned the inline `=`
/// spelling: before that, `--store=` matched nothing and the flag read as absent. Now it
/// answers `Some("")` — deliberately, so a caller can refuse it by name — and
/// `vike_model::paths::store_path::resolve_store_root` honours an EXPLICIT path unfiltered (its
/// `Explicit` rung returns before the `.filter(|s| !s.trim().is_empty())` that guards the env
/// rung). So without this filter, `backtest --store=` opens `""` and `DataFusionHist::open`
/// creates a store in the working directory.
///
/// The filter lives HERE rather than at each bin, because all five store-driven bins share this
/// one funnel and only one of them was going to remember.
#[test]
fn a_blank_explicit_store_falls_through_like_a_blank_env_var() {
    for blank in ["", "   "] {
        assert_eq!(
            store_root(Some(PathBuf::from(blank)), &env(&[("VIKE_HIST_STORE", "env-store")])),
            PathBuf::from("env-store"),
            "--store={blank:?} must fall through to the next rung, not resolve to itself"
        );
    }
    // …and a non-blank explicit path is untouched, so the filter buys nothing by refusing more.
    assert_eq!(
        store_root(Some(PathBuf::from("explicit")), &env(&[("VIKE_HIST_STORE", "env-store")])),
        PathBuf::from("explicit")
    );
}

/// **Behaviour preservation across the map lift.** A dev checkout (this repo, where the
/// compile-time repo root exists) resolves to `<repo>/market_data/hist` no matter what the platform
/// trio says — the compatibility hinge in `resolve_store_root`. This is the case every existing
/// the CI box workflow takes, and it must be untouched on a unix-shaped AND a windows-shaped map.
#[test]
fn a_dev_checkout_is_unaffected_by_the_platform_trio_on_either_platform_shape() {
    let expected = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .join("market_data")
        .join("hist");
    // Spelled through `vike_model::paths::store_path`'s constants, not as bare literals: that crate is
    // the ONE place these three variable names appear, and a fixture that re-spelled them here
    // would put an incidental `vike-backtest` row back on the settings registry for a read this
    // crate no longer performs.
    use vike_model::paths::store_path::{HOME_VAR, LOCALAPPDATA_VAR, XDG_DATA_HOME_VAR};
    let unix = env(&[(XDG_DATA_HOME_VAR, "/xdg"), (HOME_VAR, "/home/u")]);
    let windows =
        env(&[(LOCALAPPDATA_VAR, "C:\\Users\\u\\AppData\\Local"), (HOME_VAR, "C:\\Users\\u")]);
    assert_eq!(store_root(None, &unix), expected);
    assert_eq!(store_root(None, &windows), expected);
}

/// **THIS crate's call site, on the rung a `cargo test` run cannot otherwise reach.** The
/// compile-time repo path always exists while testing, so the dev-checkout hinge always fires
/// and `store_root` can never be observed resolving to a project. Injecting a `repo_default`
/// that exists on no machine exposes the rest of the ladder through the SAME wiring the bins
/// take.
///
/// It is the transposition guard for this crate: the project and per-user rungs are made
/// unmistakably different (a scratch project vs a fake `$HOME`), so a wiring that swapped them
/// reddens on the value. Verified by mutation — swapping the two inside
/// `vike_model::paths::store_path::resolve_store_root_from` turns this red with the `$HOME` path.
///
/// ⚠ **The scratch is a BOUND `tempfile::TempDir`**, not
/// `<system-temp>/vike-binutil-store-<nanos>`. Three reasons, and the third is the one a unique
/// name does not answer. (1) The old cleanup was an inline `remove_dir_all` before the
/// assertions, so a panic anywhere earlier leaked the tree permanently into a 1777 sticky
/// `/tmp` that nothing prunes. (2) `SystemTime::now()` has ~15.6 ms granularity on Windows, so
/// two concurrent runs of this test (two worktrees, two nextest invocations) could mint the
/// same `{nanos}` and one's `remove_dir_all` would delete the other's fixture MID-TEST. (3) A
/// `TempDir` claims its name `O_EXCL` and removes it on the panic path, which is what
/// OWNERSHIP means here — a unique name alone is not a repair
/// (`crates/vike-ops/tests/temp_path_gate.rs`'s banner measured 44,840 leaked directories on
/// the CI box taken exactly that way).
///
/// ⚠ **What the `TempDir` does NOT fix, stated rather than assumed away.**
/// `vike_model::paths::state_path`'s `ManifestChain::walk` climbs EVERY ancestor of its start to the
/// filesystem root, and `decisive_root` prefers the OUTERMOST manifest declaring `[workspace]`
/// — or the outermost manifest of any kind when one could not be READ — over a nearer
/// `settings/` marker. Those ancestors are the system temp root and everything above it, which
/// no test can own. So a `Cargo.toml` left in `/tmp` by anybody (a `cargo new`, a crate
/// unpacked by a build), or one left there mode 0600 by the OTHER user, makes the walk answer
/// with the stranger's directory and this assertion fail — for the user who cannot read it,
/// which is the cross-user shape this whole sweep is about. That is the PRODUCT behaving as
/// designed over a fixture whose premise has been invalidated, so the repair available here is
/// to make the failure SAY so: [`stray_ancestor_manifest`] names the offending file in the
/// message, and costs nothing when there is none.
#[test]
fn the_call_site_reaches_the_project_rung_when_this_box_has_no_checkout() {
    use vike_model::paths::store_path::{
        HOME_VAR, LOCALAPPDATA_VAR, StoreRootRung, XDG_DATA_HOME_VAR,
    };

    let scratch = tempfile::tempdir().expect("tempdir");
    let project = scratch.path().join("proj");
    let fake_home = scratch.path().join("home");
    std::fs::create_dir_all(project.join("settings")).unwrap();

    let no_checkout = Path::new("/definitely/not/a/real/build/machine/path/market_data/hist");
    let vars = env(&[
        (HOME_VAR, fake_home.to_str().unwrap()),
        (XDG_DATA_HOME_VAR, fake_home.to_str().unwrap()),
        (LOCALAPPDATA_VAR, fake_home.to_str().unwrap()),
    ]);

    let got = resolve_with(None, Some(no_checkout), Some(&project), &vars);
    let user = vike_model::paths::store_path::user_data_dir_from_vars(&vars).unwrap();

    // THE POSITIVE PROOF, and the half a green run cannot make: the resolved root is inside
    // the directory THIS test created and will remove, rather than somewhere above it that
    // another user owns.
    assert!(
        got.root.starts_with(scratch.path()),
        "the resolved store root must lie inside this test's own TempDir, and it is {}{}",
        got.root.display(),
        stray_ancestor_manifest(scratch.path())
    );
    assert_eq!(
        got.root,
        project.join("market_data").join("hist"),
        "the PROJECT's own data folder{}",
        stray_ancestor_manifest(scratch.path())
    );
    assert_eq!(got.rung, StoreRootRung::Project);
    assert_ne!(got.root, user, "the two rungs must be distinguishable in this fixture");
}

/// A sentence naming the ancestor `Cargo.toml` that invalidated the fixture above, or `""`.
///
/// Computed only when an assertion is already failing, so a manifest that declares no
/// `[workspace]` and reads fine — which is harmless — never costs a false alarm. The message is
/// the whole value: without it the failure reads as a store-root regression, and the actual
/// cause is a file in a directory nothing in this test mentions.
fn stray_ancestor_manifest(root: &Path) -> String {
    let mut dir = root.parent();
    while let Some(d) = dir {
        let manifest = d.join("Cargo.toml");
        if manifest.is_file() {
            return format!(
                "\n⚠ NOT necessarily a regression: {} sits ABOVE this test's own TempDir. \
                     `ManifestChain::walk` climbs every ancestor, and a manifest declaring \
                     `[workspace]` — or one this user cannot READ — outranks the fixture's own \
                     `settings/` marker, so the walk answers with that directory instead. Under a \
                     1777 sticky /tmp the user who did not create it cannot remove it either. \
                     Delete the file, or re-run with TMPDIR pointing somewhere with no manifest \
                     above it.",
                manifest.display()
            );
        }
        dir = d.parent();
    }
    String::new()
}

/// …and the same call site still honours everything stated ABOVE the project rung — the half
/// that proves the injection above did not quietly bypass the shared precedence.
#[test]
fn the_call_site_still_prefers_what_was_stated() {
    let no_checkout = Path::new("/definitely/not/a/real/build/machine/path/market_data/hist");
    let vars = env(&[("VIKE_HIST_STORE", "/y/env")]);
    assert_eq!(
        resolve_with(Some(PathBuf::from("/x/explicit")), Some(no_checkout), None, &vars).root,
        PathBuf::from("/x/explicit")
    );
    assert_eq!(
        resolve_with(None, Some(no_checkout), None, &vars).root,
        PathBuf::from("/y/env"),
        "the map key this crate reads must still be VIKE_HIST_STORE"
    );
}

/// **The marker is PRESENT and it NAMES the convention actually in force.** Compared to the
/// constant rather than to a second copy of the string: `vike_analytics::metrics` carries the
/// one literal pin, beside the body being named, so a convention change is typed once.
#[test]
fn the_provenance_block_names_the_current_percentile_method() {
    let v = stats_provenance();
    assert_eq!(
        v["percentile_method"],
        serde_json::json!(vike_analytics::metrics::PERCENTILE_METHOD),
        "the block must publish the method the shared percentile actually computes"
    );
    assert_eq!(v["note"], serde_json::json!(PERCENTILE_NOTE));
    // Exactly these two keys: an added one is a deliberate act, and a reader of an OLD file has
    // to be able to trust that what this block does NOT say, it never said.
    let obj = v.as_object().expect("the block is an object");
    assert_eq!(obj.len(), 2, "unexpected keys in the provenance block: {obj:?}");
}

/// The note must carry the ABSENCE rule, because that is the half a reader cannot derive: an
/// old file says nothing at all, so the new file is the only place the comparison can be
/// refused. Asserted on the substance, not the punctuation — the sentence may be reworded, and
/// may not quietly lose a fact.
#[test]
fn the_note_states_the_absence_rule_and_names_both_conventions() {
    for fact in [
        // the key whose absence IS the signal, spelled so a reader can grep for it
        "stats_provenance",
        // ...and what its absence means
        "nearest-rank",
        // the new convention, by name and by algorithm
        "percentile",
        "interpolat",
        // the reason a conversion is not on offer
        "no scale factor",
        "re-run",
    ] {
        assert!(PERCENTILE_NOTE.contains(fact), "the note dropped {fact:?}: {PERCENTILE_NOTE}");
    }
    assert!(
        PERCENTILE_NOTE.is_ascii(),
        "the note is pasted into terminals and grepped; keep it ASCII"
    );
}
