//! `store rm` and `store repair` over a REAL scratch store, through the shipped
//! binary, asserted on the BYTES of the tree. The store is the only copy of the data, so "it refused"
//! and "it wrote nothing" are two claims, and only the second one is what an operator is relying on.

use std::io::Write as _;
use std::process::Stdio;
use std::sync::Arc;

use vike_data::store::removal::SeriesSelector;
use vike_data::{HistoryStore, StoreKindId};
use vike_datahub::test_support::tree_bytes;
use vike_datahub_client::DatahubClient;
use vike_hub_protocol::auth::{HubKeys, Scope};

use super::*;

/// One series of the test tape (`seed_test_tape`), fully named, so no `--produced-by` is required.
const ONE_SERIES: [&str; 8] =
    ["--kind", "bar", "--venue", "demo", "--symbol", "BTCUSDT", "--interval", "1h"];

/// `store <verb> <ONE_SERIES> --store <store> <rest>`.
fn on_one_series<'a>(verb: &'a str, store: &'a str, rest: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec!["store", verb];
    args.extend_from_slice(&ONE_SERIES);
    args.extend_from_slice(&["--store", store]);
    args.extend_from_slice(rest);
    args
}

/// Serve `store` on a fresh loopback port holding `keys`, the only posture that serves a delete
/// (decision 0050): the far side of `vike-cli data hist rm --addr`.
fn serve_keyed(store: &Path, keys: HubKeys) -> String {
    let hist = HistoryStoreDataFusion::open(store)
        .unwrap_or_else(|e| panic!("open the scratch store {}: {e}", store.display()));
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(hist);
    let listener =
        std::net::TcpListener::bind("127.0.0.1:0").expect("bind a loopback port for the datahub");
    let addr = listener.local_addr().expect("the bound address").to_string();
    std::thread::spawn(move || {
        let _ = vike_datahub::serve_authed(listener, store, None, Some(keys), None, None, None);
    });
    addr
}

// ---- `store rm` ---------------------------------------------------------------------------------

/// **A mistyped `--store` is refused, created by nothing, and says so, rehearsal or not.** It used to
/// be created by `HistoryStoreDataFusion::open`, match nothing and exit 0, so an operator who typed the wrong
/// path read "nothing to delete". An existing EMPTY store is still a success: nothing matched, on a
/// store that was really looked at.
#[test]
fn data_rm_on_a_mistyped_root_refuses_and_creates_nothing() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let typo = dir.path().join("hsit");
    let arg = typo.to_str().expect("a UTF-8 path");

    for rest in [&[][..], &["--dry-run"][..], &["--yes"][..]] {
        let out = run(&on_one_series("rm", arg, rest));
        let stderr = stderr_of(&out);
        assert_eq!(out.status.code(), Some(2), "{rest:?}: {stderr}");
        assert!(stdout_of(&out).starts_with("store: "), "{rest:?}: the store leads");
        assert!(stderr.contains("no store at"), "{rest:?}: {stderr}");
        assert!(stderr.contains("hsit"), "{rest:?}: names the path: {stderr}");
        assert!(!typo.exists(), "{rest:?}: the refusal created the root");
    }

    let empty = dir.path().join("empty");
    std::fs::create_dir(&empty).expect("an empty store root");
    let arg = empty.to_str().expect("a UTF-8 path");
    let out = run(&on_one_series("rm", arg, &[]));
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    assert!(stdout_of(&out).contains("matched 0 series"), "{}", stdout_of(&out));
}

/// **No verb writes the synthetic tape any more** (the owner's ruling of 2026-11-13: no demo or fake
/// data in the product): `store seed-demo` is an unknown verb, exit 2, and it writes nothing into the
/// tree a `config.store_root` row names, nor beside it. `rm`, which reads no row, refuses a missing
/// root rather than creating the tree.
#[test]
fn no_verb_writes_the_synthetic_tape_and_rm_reads_no_row() {
    let scratch = Scratch::project();
    let tape = scratch.dir.path().join("tape");
    let default = scratch.dir.path().join("market_data");
    scratch.row_names_store(&tape);

    let out = scratch.tool().args(["store", "seed-demo"]).output().expect("run seed-demo");
    assert_eq!(out.status.code(), Some(2), "{}", stdout_of(&out));
    assert!(out.stdout.is_empty(), "{}", stdout_of(&out));
    assert!(stderr_of(&out).contains("seed-demo"), "names the word: {}", stderr_of(&out));
    assert!(!tape.exists(), "nothing was written into the row's tree, or created");
    assert!(!default.exists(), "and nothing beside it");

    let mut rm = vec!["store", "rm", "--yes"];
    rm.extend_from_slice(&ONE_SERIES);
    let out = scratch.tool().args(&rm).output().expect("run rm");
    assert_eq!(out.status.code(), Some(2), "{}", stderr_of(&out));
    assert!(stderr_of(&out).contains("no store at"), "{}", stderr_of(&out));
    assert!(!default.exists(), "`rm` reads no row, and it created nothing");
}

/// `fetch-starter` follows the row too. A row naming a path under a FILE fails at the open, before any
/// download, so this reaches no network, and the refusal names the path it tried.
#[test]
fn fetch_starter_naming_no_store_opens_the_tree_the_row_names() {
    let scratch = Scratch::project();
    let file = scratch.dir.path().join("a-file");
    std::fs::write(&file, "not a directory").expect("the blocker");
    let tape = file.join("tape");
    scratch.row_names_store(&tape);

    let out = scratch.tool().args(["store", "fetch-starter"]).output().expect("run fetch-starter");
    assert_eq!(out.status.code(), Some(2), "{}", stderr_of(&out));
    assert!(stderr_of(&out).contains(tape.to_str().unwrap()), "{}", stderr_of(&out));
    assert!(!scratch.dir.path().join("market_data").exists());
}

/// **No `--yes` and no terminal deletes nothing, and a pipe is not a person.** `yes | rm` is the
/// shape the confirmation exists to refuse: the pipe below carries the exact sentence a human would
/// type, and it still deletes nothing.
#[test]
fn data_rm_without_yes_and_without_a_terminal_deletes_nothing() {
    let (_dir, root, arg) = seeded();
    let before = tree_bytes(&root);
    let args = on_one_series("rm", &arg, &[]);

    let closed = run(&args);
    let scratch = Scratch::project();
    let mut piped = scratch
        .tool()
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the tool");
    // Ignored: the child may have refused and gone already, and a closed pipe is that same answer.
    let _ = piped.stdin.take().expect("the pipe").write_all(b"delete 1 series\ndelete 1 series\n");
    let piped = piped.wait_with_output().expect("the child's exit");

    for (what, out) in [("a closed stdin", &closed), ("a pipe", &piped)] {
        let stderr = stderr_of(out);
        assert_eq!(out.status.code(), Some(2), "{what}: {stderr}");
        assert!(stderr.contains("not a terminal"), "{what}: {stderr}");
        assert_eq!(tree_bytes(&root), before, "{what} must delete nothing");
    }
}

/// **A provenance refusal deletes nothing, even under `--yes`.** The rows carry `demo-tape:` keys, so
/// asserting `klines:` over them is exactly what a sweep of the wrong store looks like.
#[test]
fn data_rm_under_a_foreign_produced_by_refuses_and_deletes_nothing() {
    let (_dir, root, arg) = seeded();
    let before = tree_bytes(&root);

    let out = run(&on_one_series("rm", &arg, &["--produced-by", "klines:", "--yes"]));
    let stderr = stderr_of(&out);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("provenance REFUSED"), "{stderr}");
    assert!(stderr.contains("nothing was deleted"), "{stderr}");
    assert_eq!(tree_bytes(&root), before, "a refused delete must delete nothing");
}

/// **`--dry-run` wins over `--yes`, and a rehearsal is byte-identical.**
#[test]
fn data_rm_dry_run_wins_over_yes_and_leaves_the_tree_byte_identical() {
    let (_dir, root, arg) = seeded();
    let before = tree_bytes(&root);

    let out = run(&on_one_series("rm", &arg, &["--dry-run", "--yes"]));
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(stdout_of(&out).contains("nothing was deleted"), "{}", stdout_of(&out));
    assert_eq!(tree_bytes(&root), before, "a rehearsal must write nothing");
}

/// **An executed `rm` leaves one WARN naming the series, the prefix and the rows, and a rehearsal none.**
/// No route journals a delete, so this line is the only record of one. The wire's half is
/// `crates/vike-datahub/tests/default_build/delete_gate.rs`'s
/// `every_executed_delete_leaves_one_warning_and_a_rehearsal_none`.
#[test]
fn data_rm_logs_its_executed_delete_and_a_rehearsal_logs_none() {
    let (_dir, _root, arg) = seeded();
    let dry = run(&on_one_series("rm", &arg, &["--dry-run"]));
    assert!(!stderr_of(&dry).contains("series deleted"), "{}", stderr_of(&dry));

    let out = run(&on_one_series("rm", &arg, &["--yes"]));
    let stderr = stderr_of(&out);
    assert!(out.status.success(), "{stderr}");
    let lines: Vec<&str> = stderr.lines().filter(|l| l.contains("series deleted")).collect();
    assert_eq!(lines.len(), 1, "one series, one line: {stderr}");
    for word in ["WARN", "series=bar/demo/symbol=BTCUSDT/1h", "prefix=none", "rows="] {
        assert!(lines[0].contains(word), "`{word}` in: {}", lines[0]);
    }
}

/// ⚠ **ONE OPERATION, TWO ROUTES: `--store` and the wire plan the same and delete the same.** Both
/// call `vike_data::store::removal` over a real store, one in this binary and one inside a keyed
/// datahub, through the same client verb `vike-cli data hist rm --addr` calls. The core is one and the
/// procedure around it is written twice, so this is what stops the two drifting apart.
#[test]
fn data_rm_plans_and_deletes_the_same_on_the_store_and_over_the_wire() {
    let keys = || {
        HubKeys::new(
            b"observe-key-observe-key-observe-".to_vec(),
            b"control-key-control-key-control-".to_vec(),
        )
    };
    let (_local_dir, local_root, local_arg) = seeded();
    let (_wire_dir, wire_root, _) = seeded();
    let addr = serve_keyed(&wire_root, keys());
    let mut client =
        DatahubClient::connect_authed(addr.as_str(), &keys(), Scope::Write).expect("a keyed dial");

    let prefix = "demo-tape:";
    let selector = SeriesSelector::new(StoreKindId::Bar, "demo");
    let sweep = |rest: &[&'static str]| {
        let mut args = vec!["store", "rm", "--kind", "bar", "--venue", "demo"];
        args.extend_from_slice(&["--produced-by", prefix, "--store", &local_arg, "--json"]);
        args.extend_from_slice(rest);
        let out = run(&args);
        assert!(out.status.success(), "{args:?}: {}", stderr_of(&out));
        serde_json::from_slice::<serde_json::Value>(&out.stdout).expect("stdout is the document")
    };

    // The PLAN, on each side, deleting nothing.
    let local = sweep(&["--dry-run"]);
    let wire = client.delete_series(&selector, Some(prefix), true).expect("the wire's plan");
    assert!(wire.outcome.is_none(), "a dry run carries no outcome");
    assert!(wire.plan.matched() >= 2, "the tape holds several series: {:?}", wire.plan);
    assert_eq!(local["selector"], serde_json::to_value(&wire.plan.selector).expect("a selector"));
    assert_eq!(local["produced_by"], prefix);
    assert_eq!(local["matched"], wire.plan.matched());
    assert_eq!(local["series"], serde_json::to_value(&wire.plan.series).expect("a plan's rows"));
    assert!(!tree_bytes(&series_dir(&wire_root)).is_empty(), "a dry run deleted nothing");

    // The DELETE, on each side.
    let local = sweep(&["--yes"]);
    let wire = client.delete_series(&selector, Some(prefix), false).expect("the wire's delete");
    let outcome = wire.outcome.expect("a delete carries an outcome");
    assert!(outcome.is_clean(), "{outcome:?}");
    assert_eq!(outcome.deleted.len(), wire.plan.matched(), "{outcome:?}");
    assert_eq!(local["outcome"], serde_json::to_value(&outcome).expect("an outcome"));
    for root in [&local_root, &wire_root] {
        let store = HistoryStoreDataFusion::open_read_only(root).expect("the store");
        let left = store.list_series().expect("the listing");
        assert!(left.is_empty(), "{}: {left:?}", root.display());
    }
}

// ---- `store repair` -----------------------------------------------------------------------------

/// A rehearsal leaves the WHOLE TREE byte-identical, not only the manifest
/// `data_repair_rehearses_by_default_and_says_nothing_was_written` reads: it runs on a read-only
/// handle, and this is what holds it to that.
#[test]
fn a_data_repair_rehearsal_leaves_the_whole_tree_byte_identical() {
    let (_dir, root, arg) = seeded();
    let before = tree_bytes(&root);

    let out = run(&on_one_series("repair", &arg, &[]));
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(stdout_of(&out).contains("NOTHING WAS WRITTEN"), "{}", stdout_of(&out));
    assert_eq!(tree_bytes(&root), before, "a rehearsal must write nothing");
}

/// **`open_read_only` creates no root**, the difference from `store rm`'s open: a repair on a mistyped
/// `--store` refuses, under `--yes` too, and leaves no directory behind.
#[test]
fn data_repair_on_an_absent_root_refuses_and_creates_nothing() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let typo = dir.path().join("hsit");
    let arg = typo.to_str().expect("a UTF-8 path");

    let out = run(&on_one_series("repair", arg, &["--yes"]));
    let stderr = stderr_of(&out);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("cannot open the hist store"), "{stderr}");
    assert!(!typo.exists(), "a repair must not create the root it was pointed at by mistake");
}

/// **A contended rebuild writes nothing.** `crates/vike-store/tests/store/repair.rs`'s
/// `a_contended_rebuild_refuses_and_writes_nothing` holds the library; this holds the verb, its exit
/// code and its sentence, with the lock taken the way a live writer takes it.
#[test]
fn a_contended_data_repair_refuses_and_writes_nothing() {
    let (_dir, root, arg) = seeded();
    let lock = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(series_dir(&root).join("_manifest.lock"))
        .expect("open the lock file");
    lock.try_lock().expect("take the advisory lock");
    let before = tree_bytes(&root);

    let out = run(&on_one_series("repair", &arg, &["--yes"]));
    let stderr = stderr_of(&out);
    drop(lock);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("NOTHING WAS WRITTEN"), "{stderr}");
    assert_eq!(tree_bytes(&root), before, "a contended rebuild must write nothing");
}
