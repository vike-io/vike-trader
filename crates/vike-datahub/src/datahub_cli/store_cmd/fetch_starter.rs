//! `store fetch-starter`: download the PUBLISHED starter dataset into the store.

use std::path::Path;
use std::process::ExitCode;

use vike_store::HistoryStoreDataFusion;

use super::root::store_root;
use crate::datahub_cli::args::StoreFlags;
use crate::starter;

/// `store fetch-starter` — the published dataset, for a box that cannot reach a venue.
///
/// This verb touches no venue — it is a plain HTTPS GET of a prepared dataset, so it stays outside
/// decision 0164's gatewayhub. [`crate::starter`] carries what the download does and does not verify.
pub(super) fn run_fetch_starter(
    booted: &vike_boot::Booted,
    vars: &std::collections::HashMap<String, String>,
    cwd: Option<&Path>,
    flags: &StoreFlags,
) -> ExitCode {
    let row = booted.settings.config.store_root.as_ref().map(|p| p.display().to_string());
    let root = store_root(flags.store_path(), row, vars, cwd);
    let store = match HistoryStoreDataFusion::open(&root) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("vike-datahub: cannot open the hist store at {}: {e}", root.display());
            return ExitCode::from(2);
        }
    };
    // ⚠ `<project>/tmp`, NEVER the operating system's temp directory — production scratch must land
    // where the project folder is, and `crates/vike-ops/tests/hygiene/system_temp_gate.rs` refuses
    // otherwise. Two reasons it gives, both about a box that is not this one: inside the container
    // the system temp is not the host's and does not survive a restart, so the same path resolves
    // somewhere else on an operator's machine, silently; and the system temp is emptied by
    // something we do not control, while a leak in it is invisible until a filesystem fills (26,851
    // leaked scratch directories, 215 GB, measured on the build box).
    //
    // `ScratchDir` owns what it creates and removes it on every path out of here, error ones
    // included — a download that leaks a Parquet file per attempt is the same leak wearing our name.
    let tmp_root = booted
        .settings_dir
        .as_deref()
        .and_then(Path::parent)
        .filter(|project| !project.as_os_str().is_empty())
        .map(|project| project.join(vike_paths::state_path::PROJECT_TMP_DIR))
        // No project above the working directory — a bare checkout, or a binary run from elsewhere.
        // The store's own parent is then the honest fallback: it is where this command is writing
        // anyway, so a file that briefly appears beside it cannot surprise anyone.
        .unwrap_or_else(|| root.parent().unwrap_or(&root).join("tmp"));
    if let Err(e) = std::fs::create_dir_all(&tmp_root) {
        eprintln!("vike-datahub: cannot create {}: {e}", tmp_root.display());
        return ExitCode::from(2);
    }
    let scratch = match vike_paths::scratch::ScratchDir::create_in(&tmp_root, "starter") {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "vike-datahub: cannot create a scratch directory in {}: {e}",
                tmp_root.display()
            );
            return ExitCode::from(2);
        }
    };
    println!("downloading the starter dataset into {} …", root.display());
    match starter::fetch_into(starter::STARTER_BASE, &store, scratch.path(), |m| println!("  {m}"))
    {
        Ok(done) => {
            let mut rows = 0usize;
            for d in &done {
                rows += d.rows;
                println!(
                    "  {} — {} bytes, sha256 {}, {} rows{}",
                    d.file,
                    d.bytes,
                    d.sha256,
                    d.rows,
                    if d.rows == 0 { "  (already present)" } else { "" }
                );
            }
            if rows == 0 {
                println!("this store already held the starter dataset; nothing was written.");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("vike-datahub: {e}");
            ExitCode::from(2)
        }
    }
}
