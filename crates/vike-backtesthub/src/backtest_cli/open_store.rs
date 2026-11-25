//! `backtest`'s ONE store opening: settings, the `--store` refusal, the route.

use std::process::ExitCode;
use std::sync::Arc;

// The hist ROUTE. ⚠ It was DECLARED here until 2026-09-23 and moved DOWN, because `vike-report`
// became the fourth reader and cannot name this crate: both declare layer 45, and
// `crates/vike-ops/tests/architecture/layer_gate.rs` refuses a same-rank edge. The home was arithmetic rather
// than taste — `vike_datahub_client::route`'s own module doc argues it, and the comment above
// states the same fact from the other side: this is the one crate both `vike-cli` and this one
// take as a normal dependency, which is what makes it a place two readers can agree in.
use vike_datahub_client::route::history_route;
use vike_model::run_request::flag_vocab::store_flag_removed;
// The TRAIT, so the ONE `Arc<dyn HistoryStore + Send + Sync>` binding above the sweep branch can be
// annotated. The unsize coercion used to happen implicitly at four `Arc::new(store)` call sites,
// one per ladder arm; it happens once now, and the annotation is what performs it.
use vike_data::HistoryStore;

use super::args::RunArgs;
use super::serve::load_backtest_settings;

pub(super) struct OpenedStore {
    pub(super) provenance: String,
    pub(super) store: Arc<dyn HistoryStore + Send + Sync>,
}

#[expect(clippy::question_mark)]
pub(super) fn open_store(
    vars: &std::collections::HashMap<String, String>,
    run: &RunArgs,
) -> Result<OpenedStore, ExitCode> {
    // ⚠ The settings load sits HERE and not beside the `--addr` dispatch above, because the ARGV
    // TRIAGE between them refuses a command line WITHOUT PERFORMING ANY I/O, and a settings load
    // is I/O. `load_backtest_settings` is the same function the daemon arm calls; the two arms are
    // mutually exclusive, so one walk happens per process and never two.
    let settings = match load_backtest_settings(vars, "backtest") {
        Ok(s) => s,
        Err(code) => return Err(code),
    };
    // The one-shot run's sweep pool takes the `preferences.sweep_threads` row too: the `--addr`
    // arm installs it in `serve.rs`, and the two arms never share a process.
    vike_backtest::search::paramscan::install_paramscan_threads(settings.preferences.sweep_threads);
    // ⚠ **`--store DIR` is REFUSED on the run path since 2026-09-25**, and refused BY NAME rather
    // than ignored: the owner closed the local READ door, so every run reads history through a
    // datahub. Somebody passing it believes a directory will be read, and the message names the
    // one command that gives them that run now. `--store` is a declared (hidden) flag of `RunArgs` so that `clap` consumes its value instead
    // of mistaking it for the profile path, which is what lets this refusal be the message they see.
    if run.store.is_some() {
        eprintln!("{}", store_flag_removed("backtesthub"));
        return Err(ExitCode::FAILURE);
    }

    // ⚠ **The WIRE is the only route** (`docs/decisions/0084-only-the-datahub-touches-the-store.md`):
    // the hist store has ONE reader, the datahub, and everything else asks it over the wire. Until
    // 2026-09-25 `--store` ON THE LINE opted out; the owner closed that door, so no setting and no
    // flag selects a local read here any more. `--archive`, the other local read, was deleted on
    // the owner's ruling of 2026-11-06.
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
    // With the local arm closed those records would have been false on EVERY run, so they all take
    // this value: the datahub's address.
    // ⚠ A search STARTED before this change recorded its local root, so resuming it now compares
    // unequal and is refused. That is the honest outcome: the identity it recorded was wrong.
    let provenance = route.label();
    // ⚠ `Arc::from`, NOT `Arc::new`: the routed opener hands back a `Box<dyn HistoryStore + …>`, and
    // `Arc::new` over one would build an `Arc<Box<dyn …>>` — a second indirection that still
    // type-checks at every use below, because `Box` derefs to the trait object the methods live
    // on. `Arc::from` consumes the box and keeps ONE pointer.
    let store: Arc<dyn HistoryStore + Send + Sync> =
        match crate::binutil::routed_history(&route, vars) {
            Ok(store) => Arc::from(store),
            Err(e) => {
                eprintln!("backtesthub: {e}");
                return Err(ExitCode::from(2));
            }
        };
    Ok(OpenedStore { provenance, store })
}
