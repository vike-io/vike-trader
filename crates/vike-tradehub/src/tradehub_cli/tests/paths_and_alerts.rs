//! The absent policy file.

// -- settings / policy (settings-unification Phase 6c) ---------------------------------------

/// THE no-file property, at this daemon's own edge: with no `policy` rows on the machine the
/// loader yields `Policy::default()`, whose venue-facing projection is `AdapterPolicy::default()`.
/// Driven through the REAL loader (`load(None)` is exactly "no home directory, no project
/// file") rather than asserting the default struct, so a default that stopped
/// being the no-file answer would fail here.
///
/// Every SCALAR field is `None` — each venue arm keeps its compiled-in literal. What ARMS a venue
/// is not a `policy` field at all since decision 0119 (an account trades at its own `account.tier`
/// while active), so the projection's `accounts` is the UNREAD directory here, and an unread
/// `account` table arms nothing even beside a credential set — asserted over the REAL probe the
/// live mount claims its locks from, because the equality above would hold just as well if the
/// default directory started arming on both sides.
///
/// The credentials are planted by FIELD under an account id (decision 0156 item 6) — no row of the
/// unread table carries it, which is the point: credentials alone never arm an account.
#[test]
fn an_absent_policy_file_is_the_start_default_and_arms_no_venue() {
    let settings = vike_config::load(None).unwrap();
    let mount = vike_tradehub_core::AdapterPolicy::from(&settings.policy);
    assert_eq!(mount, vike_tradehub_core::AdapterPolicy::default());
    assert_eq!(mount.market_slippage, None, "no file ⇒ each venue keeps its own literal");
    let mut book = vike_secrets::account_credentials::CredentialBook::new();
    book.insert_account(1, "API_KEY", "k");
    book.insert_account(1, "API_SECRET", "s");
    let beside = vike_tradehub_core::AdapterPolicy {
        accounts: mount.accounts.clone().with_credentials(book),
        ..mount.clone()
    };
    let armed = vike_tradehub_core::armed_live_venues(
        crate::registry::REGISTRY,
        &std::collections::HashMap::new(),
        &beside,
    );
    assert!(
        armed.is_empty(),
        "no account row must arm NOTHING — credential presence is not a gate that can act alone: \
         {armed:?}"
    );
    // ⚠ This asserted `warnings.is_empty()`, and that emptiness was the register entry
    // `docs/ops/kill-switches.md` carried as "a missing settings directory is silently
    // uncapped": the VALUES above are right and the operator was told nothing about why they
    // are the compiled-in defaults. The values did not move; only the silence did.
    assert_eq!(
        settings.warnings,
        vec![vike_config::NO_SETTINGS_DIRECTORY_WARNING.to_string()],
        "a daemon that resolved no project must SAY its ceilings are defaults: {settings:?}"
    );
}

// -- the boot restore's scratch root (decision 0127) -----------------------------------------

use std::path::{Path, PathBuf};

use super::super::paths::restore_scratch_root;

/// The scratch root hangs off the BOOT's state directory — `<state>/restore-scratch` — and only
/// without one falls back to the journal's own directory, and with neither to the empty path.
/// PURE: nothing is created.
#[test]
fn the_restore_scratch_root_is_under_the_state_dir_then_the_journal_dir() {
    let state = Path::new("/srv/x/settings/state");
    let journal = Path::new("/srv/x/settings/state/journal");
    assert_eq!(
        restore_scratch_root(Some(state), Some(journal)),
        state.join(vike_paths::state_path::RESTORE_SCRATCH_SUBDIR),
        "a resolved project wins over the journal directory"
    );
    assert_eq!(restore_scratch_root(Some(state), None), state.join("restore-scratch"));
    assert_eq!(restore_scratch_root(None, Some(journal)), journal.to_path_buf());
    assert_eq!(restore_scratch_root(None, None), PathBuf::new());
    assert!(!state.join("restore-scratch").exists(), "the function resolves and creates nothing");
}

/// The first-boot case the old `<project>/tmp` spelling got wrong and `create_dir_all` fixes: the
/// scratch root does not exist yet, the replay's guard creates it AND its parent chain, and the
/// guard's drop leaves the root EMPTY (nothing leaks between boots). A later `sweep` of that root
/// is a no-op.
#[test]
fn the_restore_scratch_root_is_created_on_demand_and_left_empty() {
    let state = tempfile::tempdir().unwrap();
    let root = restore_scratch_root(Some(state.path()), None);
    assert!(!root.exists());
    let guard = vike_paths::scratch::ScratchDir::create_in(&root, "vike-replay-probe").unwrap();
    assert!(guard.path().starts_with(state.path()), "the scratch is inside the state directory");
    drop(guard);
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0, "the guard removed its directory");
    assert_eq!(vike_paths::scratch::sweep(&root, Some(0)).found, 0);
}

/// The values of every ACTIVE (non-comment) `key=` directive of a unit file.
fn active_directives<'a>(unit: &'a str, key: &str) -> Vec<&'a str> {
    unit.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.strip_prefix(key).and_then(|rest| rest.strip_prefix('=')))
        .collect()
}

/// Is `path` inside one of the unit's granted paths? A leading `-` (optional path) is stripped.
fn granted(grants: &[PathBuf], path: &Path) -> bool {
    grants.iter().any(|g| path.starts_with(g))
}

/// **THE regression gate for the v0.1.56 measurement**: the boot restore's scratch root, computed
/// by the daemon's own function over the unit's own state directory, lies INSIDE the shipped
/// unit's `ReadWritePaths=` line. Under `ProtectSystem=strict` anything else is `EROFS`, and the
/// restore silently degrades to snapshot-only (`Read-only file system (os error 30)`), which
/// cannot replay a journal tail. The OLD root (`<project>/tmp`) is asserted to be OUTSIDE the same
/// grant, so the reader is proven able to fail.
///
/// Reads `deploy/vike-tradehub.service` at RUN time (a test may not `include_str!` it: `deploy/` is
/// not in the public mirror) and skips loudly when the file is absent.
#[test]
fn the_restore_scratch_root_lies_inside_the_units_read_write_paths() {
    let unit_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/vike-tradehub.service");
    let Ok(unit) = std::fs::read_to_string(&unit_path) else {
        eprintln!("SKIPPED: {} is not present (public mirror?)", unit_path.display());
        return;
    };
    let roots = active_directives(&unit, "WorkingDirectory");
    assert_eq!(roots.len(), 1, "one active WorkingDirectory= expected: {roots:?}");
    let project = PathBuf::from(roots[0]);
    let lines = active_directives(&unit, "ReadWritePaths");
    assert_eq!(lines.len(), 1, "ONE ReadWritePaths= line (deploy_layout_gate): {lines:?}");
    let grants: Vec<PathBuf> =
        lines[0].split_whitespace().map(|p| PathBuf::from(p.trim_start_matches('-'))).collect();
    assert!(!grants.is_empty(), "the unit grants nothing writable?");

    // The unit's state directory is `<WorkingDirectory>/settings/state` (`Booted::state_dir`).
    let state = project
        .join(vike_paths::state_path::PROJECT_SETTINGS_DIR)
        .join(vike_paths::state_path::STATE_SUBDIR);
    let scratch = restore_scratch_root(Some(&state), None);
    assert!(
        granted(&grants, &scratch),
        "the boot restore's scratch root {} is not inside the unit's ReadWritePaths= {grants:?}: \
         a sandboxed daemon cannot create it (EROFS) and the restore degrades to snapshot-only",
        scratch.display()
    );

    // Floor: the pre-fix spelling is NOT granted, so this reader can fail.
    let old = project.join(vike_paths::state_path::PROJECT_TMP_DIR);
    assert!(
        !granted(&grants, &old),
        "`<project>/tmp` became writable: re-read this gate's premise"
    );
}
