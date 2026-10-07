//! `backtest data fetch-starter`: download the PUBLISHED starter dataset into the store.

use std::path::PathBuf;
use std::process::ExitCode;

use vike_analytics::binutil::arg;
use vike_data::DataFusionHist;

use crate::binutil::store_root;

// Gated exactly as the module is: a `datafusion-store`-only build has no `starter`, and an
// unconditional import made that configuration fail to compile — a build CI runs and I did not.
#[cfg(feature = "venue-fetch")]
use crate::starter;

/// `data fetch-starter` — the published dataset, for a box that cannot reach a venue.
///
/// ⚠ This doc said "see [`crate::starter`] for why the same feature covers BOTH" — both being this
/// and `data fetch`. There is no both any more: `data fetch` and the `crate::fetch` module it
/// called are DELETED, because a compute-plane binary reaching an exchange directly is the thing
/// that change removed. This verb never touched a venue — it is a plain HTTPS GET of a prepared
/// dataset — and it is now the only thing `venue-fetch` gates that fetches anything at all.
/// [`crate::starter`] still carries what the download does and does not verify.
#[cfg(feature = "venue-fetch")]
pub(super) fn run_fetch_starter(
    vars: &std::collections::HashMap<String, String>,
    args: &[String],
) -> ExitCode {
    let root = store_root(arg(args, "--store").map(PathBuf::from), vars);
    let store = match DataFusionHist::open(&root) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("backtest: cannot open the hist store at {}: {e}", root.display());
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
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let tmp_root = vike_model::paths::state_path::project_tmp_dir_from(
        vars.get("VIKE_SETTINGS_DIR").map(String::as_str),
        &cwd,
    )
    // No project above the working directory — a bare checkout, or a binary run from elsewhere.
    // The store's own parent is then the honest fallback: it is where this command is writing
    // anyway, so a file that briefly appears beside it cannot surprise anyone.
    .unwrap_or_else(|| root.parent().unwrap_or(&root).join("tmp"));
    if let Err(e) = std::fs::create_dir_all(&tmp_root) {
        eprintln!("backtest: cannot create {}: {e}", tmp_root.display());
        return ExitCode::from(2);
    }
    let scratch = match vike_model::scratch::ScratchDir::create_in(&tmp_root, "starter") {
        Ok(s) => s,
        Err(e) => {
            eprintln!("backtest: cannot create a scratch directory in {}: {e}", tmp_root.display());
            return ExitCode::from(2);
        }
    };
    println!("downloading the starter dataset into {} …", root.display());
    match starter::fetch_into(&store, scratch.path(), |m| println!("  {m}")) {
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
            eprintln!("backtest: {e}");
            ExitCode::from(2)
        }
    }
}
