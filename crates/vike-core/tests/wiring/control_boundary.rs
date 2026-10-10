//! The control boundary end-to-end: a Command::Order in via the handle, a CoreSnapshot query out.
//! Plus three source gates: order submission is single-site, this crate re-exports no symbol a
//! lower crate owns (so `Command` is named `vike_exec::Command`, never through `vike_core`), and
//! below the crate root it gives none of its own items a second public path.
use crate::kit::handle::wait_for_snapshot;
use vike_core::{CoreConfig, CoreHandle, spawn_core};
use vike_exec::testing::TestExecutionClient;
use vike_exec::{Account, BalanceMode, Command, ExecutionEngine, OrderIntent, RiskGate};
use vike_model::OrderRequest;
use vike_model::RiskLimits;

fn handle() -> CoreHandle {
    let engine = ExecutionEngine::new(
        Account::new(1_000.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        TestExecutionClient::new("sim", 100.0),
        "sim",
        "BTCUSDT",
    );
    spawn_core(engine, CoreConfig { seed_cash: 1_000.0, ..Default::default() })
}

#[test]
fn command_in_snapshot_query_out() {
    let h = handle();
    let req = OrderRequest {
        client_order_id: "cli-1".into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: "market".into(),
        ..Default::default()
    };
    h.send_command(Command::Order(OrderIntent::Submit(Box::new(req))));
    // wait for the fold to publish the order, then query THROUGH the accessors
    wait_for_snapshot(&h.snapshot_cell(), 5, "the submitted order is folded and published", |s| {
        s.order("cli-1").is_some()
    });
    let snap = h.snapshot();
    assert!(snap.order("cli-1").is_some(), "the order is visible via the query accessor");
    assert_eq!(snap.orders_for("BTCUSDT").count(), 1);
    h.shutdown_and_join();
}

/// The "one submission path" invariant: engine ORDER-SUBMISSION verbs are called ONLY inside
/// apply.rs. (Cancels/confirms are also issued by the panic safe-state sweep,
/// `crates/vike-core/src/runtime/watchdog/orders.rs`'s `enter_safe_state`, so they are NOT part of
/// this invariant — only order CREATION must be single-site, which is what guarantees mint +
/// RiskGate.) A cheap scan over the PRODUCTION prefix (before `#[cfg(test)]`) of each module.
///
/// ⚠ This read a HAND-WRITTEN list of seven files until the vike-core layout split, and that list was
/// already short of its own comment ("every runtime module EXCEPT apply.rs"): `deadman.rs`,
/// `link_deadman.rs` and `recon_held.rs` were never in it. Splitting `mod.rs`, `strategy_drive.rs` and
/// `watchdog.rs` into folders would have left every new child in no list at all, and the invariant
/// would have gone on passing while covering a fraction of the code. It WALKS `src/runtime/` now:
/// every production `.rs` file except `apply.rs` and its `apply/` children (the single site) and the
/// white-box tests (`tests/` and any `*_tests.rs`).
#[test]
fn order_submission_lives_only_in_apply_rs() {
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if path.is_dir() {
                if name == "apply" || name == "tests" || name.ends_with("_tests") {
                    continue;
                }
                walk(&path, out);
            } else if name.ends_with(".rs") && name != "apply.rs" && !name.ends_with("_tests.rs") {
                out.push(path);
            }
        }
    }
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/runtime");
    let mut files = Vec::new();
    walk(&dir, &mut files);
    // NON-VACUITY: a walk that stopped finding files would pass this test while checking nothing, and
    // a split that adds a folder the walk skips would do the same. 31 production files at the split.
    assert!(
        files.len() >= 25,
        "the runtime walk found only {} production files — it has stopped reaching the tree: {files:?}",
        files.len()
    );
    for must_reach in [
        ["strategy_drive", "broker_drain.rs"],
        ["watchdog", "orders.rs"],
        ["reconcile.rs", ""],
        ["run_loop.rs", ""],
    ] {
        let tail: std::path::PathBuf = must_reach.iter().filter(|s| !s.is_empty()).collect();
        assert!(
            files.iter().any(|f| f.ends_with(&tail)),
            "the runtime walk must reach `{}` (a child the layout split created): {files:?}",
            tail.display()
        );
    }
    for f in &files {
        let src = std::fs::read_to_string(f).unwrap();
        let prod = src.split("#[cfg(test)]").next().unwrap(); // ignore in-module unit tests
        for verb in [".submit_order(", ".submit_order_batch("] {
            assert!(
                !prod.contains(verb),
                "{} submits orders outside apply.rs (`{verb}`) — submission must be single-site",
                f.display()
            );
        }
    }
}

/// Every `pub use vike_*::…` statement in `src` (line comments stripped), whitespace-collapsed, so a
/// braced re-export wrapped over several lines comes back as ONE entry. Only a re-export of ANOTHER
/// workspace crate counts: `pub use crate::…`, `pub use self::…` and a sibling module are this crate's
/// own vocabulary, and a `pub(crate) use` exports nothing.
fn lower_crate_reexports(src: &str) -> Vec<String> {
    pub_use_paths(src)
        .into_iter()
        .filter(|path| path.trim_start_matches("::").starts_with("vike_"))
        .collect()
}

/// Every `pub use` statement's path in `src` (line comments stripped), whitespace-collapsed.
fn pub_use_paths(src: &str) -> Vec<String> {
    let code = vike_model::scan::strip_comments(src);
    let mut found = Vec::new();
    let mut rest = code.as_str();
    while let Some(at) = rest.find("pub use ") {
        let after = &rest[at + "pub use ".len()..];
        let stmt = &after[..after.find(';').unwrap_or(after.len())];
        found.push(stmt.split_whitespace().collect::<Vec<_>>().join(" "));
        rest = &after[stmt.len()..];
    }
    found
}

/// Every `pub use` in a NON-ROOT file of this crate that gives one of the crate's OWN items a second
/// public path: a path through `crate::` or `super::` (the item already has a public path of its
/// own, or the root re-exports it), or through a child the same file declares `pub mod` (the child's
/// path is public already). A module root re-exporting its PRIVATE children (`mod handle;` +
/// `pub use handle::CoreHandle;`) is the item's ONE path there, and is not counted.
fn own_symbol_second_paths(src: &str) -> Vec<String> {
    let code = vike_model::scan::strip_comments(src);
    pub_use_paths(src)
        .into_iter()
        .filter(|path| {
            if path.starts_with("crate::") || path.starts_with("super::") {
                return true;
            }
            let first = path.split([':', '{', ' ']).next().unwrap_or("");
            !first.is_empty()
                && (code.contains(&format!("pub mod {first};"))
                    || code.contains(&format!("pub mod {first} {{")))
        })
        .collect()
}

/// Every `.rs` file under `dir`, recursively.
fn rs_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rs_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// **This crate re-exports no symbol another workspace crate owns**: a symbol has one name (the root
/// `CLAUDE.md`'s "Conventions that will bite you if ignored"). `vike_core::StrategyParams`, `vike_core::TimeRule` and
/// `vike_core::control::Command` were second names of `vike_model::StrategyParams`,
/// `vike_model::time::schedule::TimeRule` and `vike_exec::Command`: callers split between the two
/// spellings, so a reader could not tell which path was the owner. Every caller names the owning crate
/// now; a `pub use vike_*::` statement anywhere under `src/` is refused.
///
/// The walk covers every `.rs` file under `src/` (test modules included: a re-export there is the
/// same second name), and asserts it reached the three files the re-exports used to sit in.
#[test]
fn no_lower_crate_symbol_is_re_exported() {
    // KILL-PROOF: the scanner must see each shape a re-export takes, or a green below means nothing.
    assert_eq!(
        lower_crate_reexports("pub use vike_model::{ControllerParams, StrategyParams};"),
        ["vike_model::{ControllerParams, StrategyParams}"]
    );
    assert_eq!(
        lower_crate_reexports(
            "pub use vike_exec::{\n    Command,\n    OrderIntent,\n};\npub use vike_model::time::schedule::TimeRule;"
        ),
        ["vike_exec::{ Command, OrderIntent, }", "vike_model::time::schedule::TimeRule"]
    );
    assert!(
        lower_crate_reexports(
            "// pub use vike_model::X;\npub use crate::runtime::CoreHandle;\npub(crate) use vike_exec::Y;\n\
             pub use schedule::LiveSchedule;\nuse vike_model::Z;"
        )
        .is_empty(),
        "a comment, an own-crate path, a crate-private import and a plain `use` are not re-exports"
    );

    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rs_files(&src, &mut files);
    for must_reach in ["lib.rs", "control.rs", "schedule.rs"] {
        assert!(
            files.contains(&src.join(must_reach)),
            "the src walk must reach `{must_reach}` (a file a re-export used to sit in): {files:?}"
        );
    }
    let mut offenders = Vec::new();
    for f in &files {
        let text = std::fs::read_to_string(f).unwrap();
        for path in lower_crate_reexports(&text) {
            offenders.push(format!("{}: pub use {path};", f.strip_prefix(&src).unwrap().display()));
        }
    }
    assert!(
        offenders.is_empty(),
        "vike-core re-exports symbols another crate owns — delete each and name the owning crate's \
         path at every call site (one name per symbol):\n{}",
        offenders.join("\n")
    );
}

/// **Outside `lib.rs`, this crate gives none of its OWN items a second public path.**
/// `vike_core::control::{CommandRejected, CommandSink, CoreHandle}` stood beside the crate-root
/// `vike_core::{…}` that every caller names (and `vike_core::runtime::…`, the module that owns
/// them): a third spelling of three types, used by no caller. The crate ROOT is the deliberate
/// vocabulary (the root `CLAUDE.md`'s one-name rule, exception 1), so `lib.rs` is not walked; a
/// module root re-exporting its PRIVATE children is that item's one path inside the module, and is
/// not counted either (`own_symbol_second_paths` says how the two are told apart).
#[test]
fn no_own_symbol_gets_a_second_public_path_below_the_root() {
    // KILL-PROOF: each shape a second path takes is seen, and the module-root shape is not.
    assert_eq!(
        own_symbol_second_paths(
            "pub use crate::runtime::{CommandRejected, CommandSink, CoreHandle};"
        ),
        ["crate::runtime::{CommandRejected, CommandSink, CoreHandle}"]
    );
    assert_eq!(
        own_symbol_second_paths("pub use super::{\n    handle::CoreHandle,\n};"),
        ["super::{ handle::CoreHandle, }"]
    );
    assert_eq!(
        own_symbol_second_paths("pub mod build;\npub use build::{accounts_epoch_of, build};"),
        ["build::{accounts_epoch_of, build}"]
    );
    assert!(
        own_symbol_second_paths(
            "mod handle;\npub use handle::{CommandSink, CoreHandle};\n\
             // pub use crate::runtime::CoreHandle;\npub(crate) use crate::runtime::MountAttribution;\n\
             use crate::runtime::CoreHandle;"
        )
        .is_empty(),
        "a private child re-exported, a comment, a crate-private import and a plain `use` are not \
         second public paths"
    );

    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rs_files(&src, &mut files);
    for must_reach in ["control.rs", "runtime/mod.rs", "snapshot.rs", "replay.rs"] {
        assert!(
            files.contains(&src.join(must_reach)),
            "the src walk must reach `{must_reach}` (a file that re-exports): {files:?}"
        );
    }
    let mut offenders = Vec::new();
    for f in files.iter().filter(|f| **f != src.join("lib.rs")) {
        let text = std::fs::read_to_string(f).unwrap();
        for path in own_symbol_second_paths(&text) {
            offenders.push(format!("{}: pub use {path};", f.strip_prefix(&src).unwrap().display()));
        }
    }
    assert!(
        offenders.is_empty(),
        "vike-core gives its own items a second public path below the crate root — delete each \
         and name the crate-root `vike_core::X` (or the owning module's path) at every call site \
         (one name per symbol):\n{}",
        offenders.join("\n")
    );
}
