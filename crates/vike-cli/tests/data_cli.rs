//! `vike-cli data` — the store-filling AND store-inspecting verb, driven as the SHIPPED binary.
//!
//! The grammar is unit-tested beside the module; what needs a real process is the part no parser
//! test can see: which exit code reaches a caller, that the WRITE half genuinely SPAWNS an engine
//! rather than pretending to, and that the READ half genuinely talks to a datahub.
//!
//! The read half's cases run against a REAL [`vike_datahub::serve`] over a seeded in-memory
//! `MemHistStore` on an ephemeral loopback port — the spawn pattern `tests/backtest_cli.rs` and
//! `crates/vike-datahub/tests/roundtrip.rs` already use, with a store that actually holds rows so
//! the inventory has something to enumerate. No prod store, no external network, no DataFusion:
//! the double is the trait-only in-memory one, which is what keeps this file on the fast lane
//! beside the crate it tests.
//!
//! ⚠ **Every case names the engine with `--engine`, and that is not laziness.** The search's third
//! rung looks beside THIS executable, and a lane that has also built
//! `-p vike-backtest --features datafusion-store` into the same `target/` really does leave a
//! `backtest` binary sitting there. A test that relied on the search finding nothing would pass on
//! a developer box and fail in the full CI matrix, for a reason having nothing to do with this
//! code — so the tests that care about the MISS name a path that is not there, and the tests that
//! care about the SPAWN name a stand-in they wrote themselves.
//!
//! The child's environment is pinned on the child (`Command::env` / `env_remove`, never
//! `std::env::set_var`): without the settings redirect a run on a developer box resolves the REPO's
//! settings directory, and an exported removed variable would make every case exit on the startup
//! refusal instead of the rung it is testing.

use std::path::Path;
use std::process::{Command, Output};

/// The planted-engine plant and the `ETXTBSY` retry that survives spawning one. See that module's
/// doc for the race and for why matching the errno — and only the errno — is what keeps the retry
/// from hiding a genuinely missing engine, which this file has a case about.
mod common;

/// The ONE way this file spawns `vike-cli` (unlike `backtest_cli.rs`, which keeps a dozen direct
/// spawns for the ambient environment they need), so every case — the eight that plant a stand-in
/// engine and the rest that do not — inherits the `ETXTBSY` retry without having to know it exists.
/// `planted_binary_retry.rs`'s `a_case_that_plants_an_engine_may_not_spawn_the_cli_itself` holds
/// that property for both files.
///
/// ⚠ The retry is NOT belt-and-braces here. Each planted engine is exec'd by the CHILD, so a lost
/// coin toss arrives as a `vike-cli` that died before producing anything and the case fails on its
/// own assertion about missing output. `common`'s module doc carries the mechanism; the predicate
/// is the errno alone, and a spawn failure that is not that errno still panics on the first
/// attempt exactly as this function did before.
///
/// ⚠ **The DATAHUB node pair is removed here because the settings redirect CANNOT answer for it.**
/// `crates/vike-cli/src/boot.rs`'s `datahub_keyring` reads `node_keys_from_vars` over the PROCESS
/// ENVIRONMENT first and the node store only second — "The environment still WINS", its own doc —
/// so `VIKE_SETTINGS_DIR` pointed at an empty temp directory does not make a box that EXPORTS the
/// pair look keyless, and that box is the shape that function's doc records as the real the CI box one.
/// One case cares: [`the_capability_matrix_says_which_side_refused_when_a_server_answers_and_says_no`]
/// names `DatahubClient::connect`'s post-handshake refusal as the path it proves, and with the pair
/// exported the run takes `connect_authed` instead, the keyed fixture denies the mac, and the
/// refusal arrives as `Response::AuthDenied`. The verdict is `PermissionDenied` either way, so the
/// case stayed green either way — what was unpinned was the PATH its doc names, which is how a
/// later author deletes the `connect` arm's branch and sees nothing redden.
/// `crates/vike-cli/tests/study_report_refusal_cli.rs` removes the same pair for the same reason.
fn run(settings_dir: &Path, args: &[&str]) -> Output {
    common::output_retrying_etxtbsy(&format!("run vike-cli {args:?}"), || {
        let mut c = Command::new(env!("CARGO_BIN_EXE_vike-cli"));
        c.args(args)
            .env("VIKE_SETTINGS_DIR", settings_dir)
            .env_remove("VIKE_MAX_ORDER_NOTIONAL")
            .env_remove("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL")
            .env_remove("VIKE_DATAHUB_OBSERVE_KEY")
            .env_remove("VIKE_DATAHUB_CONTROL_KEY");
        c
    })
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[path = "data_cli/backfill.rs"]
mod backfill;
#[path = "data_cli/capability_and_export.rs"]
mod capability_and_export;
#[path = "data_cli/catalog.rs"]
mod catalog;
#[path = "data_cli/class.rs"]
mod class;
#[path = "data_cli/gate.rs"]
mod gate;
#[path = "data_cli/grammar.rs"]
mod grammar;
#[path = "data_cli/import.rs"]
mod import;
#[path = "data_cli/read_verbs.rs"]
mod read_verbs;
#[path = "data_cli/realtime.rs"]
mod realtime;
#[path = "data_cli/record.rs"]
mod record;
#[path = "data_cli/rm_repair.rs"]
mod rm_repair;
#[path = "data_cli/source.rs"]
mod source;
#[path = "data_cli/support.rs"]
mod support;
