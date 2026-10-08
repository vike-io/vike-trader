//! `backtest`'s ONE store opening: settings, the `--store` refusal, `--archive`, the route.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use vike_analytics::binutil::{arg, has_flag};
// The hist ROUTE. ⚠ It was DECLARED here until 2026-09-23 and moved DOWN, because `vike-report`
// became the fourth reader and cannot name this crate: both declare layer 45, and
// `crates/vike-ops/tests/architecture/layer_gate.rs` refuses a same-rank edge. The home was arithmetic rather
// than taste — `vike_datahub_client::route`'s own module doc argues it, and the comment above
// states the same fact from the other side: this is the one crate both `vike-cli` and this one
// take as a normal dependency, which is what makes it a place two readers can agree in.
use vike_datahub_client::flag_vocab::store_flag_removed;
use vike_datahub_client::route::{history_route, open_routed_history};
// The TRAIT, so the ONE `Arc<dyn HistStore + Send + Sync>` binding above the sweep branch can be
// annotated. The unsize coercion used to happen implicitly at four `Arc::new(store)` call sites,
// one per ladder arm; it happens once now, and the annotation is what performs it.
use vike_data::HistStore;

use super::serve::load_backtest_settings;

pub(super) struct OpenedStore {
    pub(super) provenance: String,
    pub(super) concrete: Option<Arc<dyn HistStore + Send + Sync>>,
    pub(super) store: Arc<dyn HistStore + Send + Sync>,
}

#[allow(clippy::question_mark)]
pub(super) fn open_store(
    vars: &std::collections::HashMap<String, String>,
    args: &[String],
) -> Result<OpenedStore, ExitCode> {
    // ⚠ `store_root_resolved`, not `store_root`: the RUNG comes with the path here, because a
    // `data.explain` plan leads with both. `store_root` is the same call with the rung dropped
    // (`binutil`'s own doc says so), so the two cannot resolve differently — and "which store
    // answered" is the first question a plan has to settle, exactly as it is for `data rm`.
    // ⚠ The settings load sits HERE and not beside the `--addr` dispatch above, because the ARGV
    // TRIAGE between them refuses a command line WITHOUT PERFORMING ANY I/O, and a settings load
    // is I/O. `load_backtest_settings` is the same function the daemon arm calls; the two arms are
    // mutually exclusive, so one walk happens per process and never two.
    let settings = match load_backtest_settings(vars, "backtest") {
        Ok(s) => s,
        Err(code) => return Err(code),
    };
    // ⚠ **`--store DIR` is REFUSED on the run path since 2026-09-25**, and refused BY NAME rather
    // than ignored: the owner closed the local READ door, so every run reads history through a
    // datahub. Somebody passing it believes a directory will be read, and the message names the
    // one command that gives them that run now. The `data` verbs keep their own `--store` — they
    // WRITE, and the ruling was about readers — and they are dispatched before this line.
    // `PROFILE_PATH_VALUED` still lists the flag so argv triage consumes its value rather than
    // mistaking it for the profile path, which is what lets this refusal be the message they see.
    if has_flag(args, "--store") || arg(args, "--store").is_some() {
        eprintln!("{}", store_flag_removed("backtest"));
        return Err(ExitCode::FAILURE);
    }

    // ⚠ **`--archive` reads downloaded `data.vike.io` Parquet IN PLACE, and it is a DIFFERENT
    // BACKEND rather than a different root.** Until 2026-09-20 this engine could not open one at
    // all: the reader lived in `vike-backfill`, a COLLECTOR, which ranks above this crate — so the
    // only way to point the harness at an archive day was a separate binary in that crate, whose
    // own doc called itself "the leaf binary that can depend on it directly". That is a workaround
    // around an inverted arrow, not a feature, and `vike_data::store::backtest_store` is where the choice
    // lives now. The binary is deleted with this change.
    //
    // Why it is worth a flag rather than "import it first": reading in place skips the import
    // entirely, and the import is not cheap. MEASURED on the CI box (2026-09-20, one flat-layout day):
    // **16.2 s for ONE row group of 999,420 rows**, and a day carries 300 of them — the cost is
    // driven by how many distinct `token_id`s a row group holds (~450 in the flat layout), because
    // each group is split across that many destination partitions.
    //
    // ⚠ `--archive` is a LOCAL READ too, and it survived the 2026-09-25 ruling on purpose: that
    // ruling closed `--store DIR` (a second path to the SAME kind of store), and `--archive` is a
    // different BACKEND the datahub does not serve. Closing it would re-open exactly the gap the
    // 2026-09-20 ruling shut, when a separate collector binary was the only way to backtest an
    // archive day. The "both given" refusal that stood here went with `--store`.
    let archive = arg(args, "--archive").map(PathBuf::from);
    // ⚠ **A path that does not EXIST is refused here, before anything runs.** The archive backend
    // reads a non-directory path as ONE file and opens lazily, so a missing path opened "fine",
    // every scan answered no rows, and the run printed a report over NO data, saved it and exited 0
    // (measured 2026-10-06). An existing directory with no `.parquet` was already refused, by the
    // backend's empty-selection check below; a single existing FILE is a supported shape and passes.
    if let Some(path) = &archive
        && let Err(e) = std::fs::metadata(path)
    {
        eprintln!(
            "backtest: --archive {}: {e} — name a directory of downloaded .parquet files or one \
             .parquet file. Nothing was run",
            path.display()
        );
        return Err(ExitCode::FAILURE);
    }
    // ⚠ **The WIRE is the only route** (`docs/decisions/0084-only-the-datahub-touches-the-store.md`):
    // the hist store has ONE reader, the datahub, and everything else asks it over the wire. Until
    // 2026-09-25 `--store` ON THE LINE opted out; the owner closed that door, so no environment
    // variable and no flag selects a local read here any more (`$VIKE_HIST_STORE` never did — the
    // deployed unit set it until 2026-09-26, and leaving it in charge would have kept every run
    // local). `--archive` is the one other answer and wins outright: it names a different BACKEND,
    // not a different root, so no route is consulted for it at all.
    //
    // ⚠ The ADDRESS comes from the settings — which means the DATABASE on a migrated box, through
    // the same `StoreLayer` the daemon arm reads. `load_backtest_settings` carries the owner
    // ruling behind that; the short version is that a box has ONE answer to "where is my
    // datahub", and an env-only ladder here would have given this arm a different one from the
    // `--addr` arm two hundred lines up.
    let route = history_route(settings.config.datahub_addr.as_deref());
    // ⚠ **ONE provenance for the whole run, and it is what every record below says.** Until
    // 2026-09-25 three records disagreed on a WIRE run: `DataFingerprint.store` named the datahub
    // (0084 fixed that one), while the run manifest's `"store"` and the search identity's
    // `store` still recorded the LOCAL root this process resolved — a directory nothing opened.
    // And on an `--archive` run the data plan was labelled with the datahub, though every byte
    // came from the archive. With the local arm closed those records would have been false on
    // EVERY run, so they all take this value: the archive path when one was given, the datahub
    // otherwise. CANONICALIZED for the archive, because two projects' `--archive days` name two
    // different directories and a shared runs root would otherwise compare them EQUAL.
    // ⚠ A search STARTED before this change recorded its local root, so resuming it now compares
    // unequal and is refused. That is the honest outcome: the identity it recorded was wrong.
    let provenance = match &archive {
        Some(p) => std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()).display().to_string(),
        None => route.label(),
    };
    // NONE when the run reads an archive: the archive backend keeps no per-series manifest, so it
    // has no facts to fingerprint a run with, and the consumers below take the absence as the fact
    // it is. ⚠ That used to be a statement about the TYPE — "there is no `DataFusionHist` behind
    // it" — and the type stopped being the reason: `list_series` and `series_facts` are both on
    // the `HistStore` trait now, so `Some` no longer means "concrete" and this binding is no
    // longer the concrete handle. What it means is "a store that can report its own provenance".
    let concrete: Option<Box<dyn HistStore + Send + Sync>> = match &archive {
        Some(_) => None,
        None => Some(open_routed_history(&route, vars)),
    };
    // ONE handle, UNSIZED ONCE. `run_backtest` and every evaluator constructor take
    // `Arc<dyn HistStore + Send + Sync>`; the ladder this replaced wrote `Arc::new(store)` at FOUR
    // call sites, one per arm, and coerced at each.
    //
    // ⚠ `Arc::from`, NOT `Arc::new`: the routed opener hands back a `Box<dyn HistStore + …>`, and
    // `Arc::new` over one would build an `Arc<Box<dyn …>>` — a second indirection that still
    // type-checks at every use below, because `Box` derefs to the trait object the methods live
    // on. `Arc::from` consumes the box and keeps ONE pointer.
    //
    // ⚠ The handle is KEPT under its own name rather than shadowed away, so the FINGERPRINT
    // capture can happen below, on the single-run path where the record is written, instead of
    // here above the sweep branch where its whole result is discarded. That reason used to be a
    // TYPE reason — `list_series` and `series_facts` were inherent to `DataFusionHist`, so only
    // the concrete handle could be asked — and it is not one any more: both are on the trait since
    // 0084's seventh verb landed. What survives is the PLACEMENT, which is what the name was
    // really buying. An `Arc` clone costs one refcount and nothing else.
    let concrete: Option<Arc<dyn HistStore + Send + Sync>> = concrete.map(Arc::from);
    let store: Arc<dyn HistStore + Send + Sync> = match (&concrete, archive.clone()) {
        (Some(c), _) => c.clone(),
        // The archive selector REFUSES an empty selection rather than returning a store that
        // answers every scan "no rows" — `HistStore`'s `Ok(vec![])` is a claim of fact, and
        // `crates/vike-data/src/store/archive_store.rs`'s module doc carries what believing that wrongly
        // once cost (a scalper scoring -2.74% where the truth was -27.64%).
        (None, Some(path)) => {
            match vike_data::store::backtest_store::open_backtest_store(
                vike_data::store::backtest_store::BacktestStore::Archive(path),
            ) {
                Ok(s) => s,
                Err(why) => {
                    eprintln!("backtest: {why}");
                    return Err(ExitCode::FAILURE);
                }
            }
        }
        // Unreachable by construction: `concrete` is `None` only in the `--archive` arm above.
        (None, None) => {
            eprintln!("backtest: no store was opened and no --archive was given");
            return Err(ExitCode::FAILURE);
        }
    };
    Ok(OpenedStore { provenance, concrete, store })
}
