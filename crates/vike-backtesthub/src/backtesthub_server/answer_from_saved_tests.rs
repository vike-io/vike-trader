//! The parts of the answer from a saved run that need no child (`super`'s module doc): the key's `name` rule, the
//! nondeterminism rule, which saved run a lookup finds, what the saver keeps, and how an answer says where it came
//! from. The answer itself, through a real sandboxed child, is `crates/vike-backend/tests/strategy_file_from_saved.rs`'s.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::json;
use vike_backtest::run::StrategySource;
use vike_backtesthub_client::proto::{Request, Response};
use vike_backtesthub_client::summary::RunSummaryWire;
use vike_data::HistoryStore;
use vike_datahub_client::proto::build_feature;

use super::{
    ANSWERED_FROM_KEY, AnswersFromSaved, Capped, INPUT_KEY, KEPT_ANSWER_KEY, KEPT_ANSWER_MAX_BYTES,
    Kept, KeptAnswer, KeptAnswerOf, Reusable, datahub_identity, forget, keep_into, marked,
    newest_keyed, nondeterministic, remember, without_name,
};
use crate::backtesthub_server::StudyDirs;
use crate::strategy_sandbox::RustRun;
use crate::strategy_sandbox::witness::ReadWitnesses;

const PROFILE: &str = "# a run\nname = \"momentum, 5\"\n\n[data]\nvenue = \"binance\"\n\n[strategy]\n\
                       file = \"trend/momentum.rs\"\nname = \"a table's own name\"\n";

#[test]
fn a_profiles_top_level_name_line_is_dropped_and_nothing_else() {
    let (unnamed, name) = without_name(PROFILE);
    assert_eq!(name.as_deref(), Some("momentum, 5"));
    assert_eq!(unnamed, PROFILE.replace("name = \"momentum, 5\"\n", ""));
    assert!(unnamed.contains("name = \"a table's own name\""), "a table's key is the profile's");
    // Two profiles that differ in their name alone key alike; any other byte keys apart.
    let renamed = PROFILE.replace("momentum, 5", "another");
    assert_eq!(without_name(&renamed).0, unnamed);
    assert_ne!(without_name(&PROFILE.replace("binance", "bybit")).0, unnamed);
    // A name the line rule cannot drop cleanly (a quoted key) stays whole: the key then holds it, and only misses.
    let quoted = "\"name\" = \"x\"\n[data]\nvenue = \"binance\"\n";
    assert_eq!(without_name(quoted), (quoted.to_string(), Some("x".to_string())));
    // An unnamed profile and one that does not parse are kept as they are.
    assert_eq!(without_name("[data]\n").0, "[data]\n");
    assert_eq!(without_name("[data\n"), ("[data\n".to_string(), None));
}

#[test]
fn a_strategy_naming_the_clock_randomness_a_hash_map_or_a_thread_is_never_answered_from_saved() {
    let file =
        |code: &str| format!("//! A strategy: random walks and HashMaps in its docs.\n{code}\n");
    assert_eq!(
        nondeterministic(&file("let x = bar.close * 2.0;")),
        None,
        "its comments are not code"
    );
    for (code, named) in [
        ("let t = std::time::SystemTime::now();", "SystemTime"),
        ("let started = Instant::now();", "Instant"),
        ("use std::collections::HashMap;", "HashMap"),
        ("let pick = rand::random::<f64>();", "rand"),
        ("std::thread::spawn(|| {});", "spawn"),
    ] {
        assert_eq!(nondeterministic(&file(code)), Some(named), "{code}");
    }
    // Whole identifiers only: a word that holds one is not it.
    assert_eq!(nondeterministic(&file("let random_walk = randomness_free(); spawned();")), None);
    // A sleep moves nothing an answer holds.
    assert_eq!(nondeterministic(&file("std::thread::sleep(pause);")), None);
    // The SDK's own clock, its order-id generator and its threads, as a strategy names them.
    for (code, named) in [
        ("let t = vike_model::now_ms();", "now_ms"),
        ("let t = now_ns();", "now_ns"),
        ("let t = vike_model::time::clock::now_unix_secs();", "now_unix_secs"),
        ("let clock = ClockLive;", "ClockLive"),
        ("let ids = OrderClientIdGenerator::new(None);", "OrderClientIdGenerator"),
        ("scope.spawn_scoped(|| {});", "spawn_scoped"),
        ("let n = std::thread::available_parallelism();", "available_parallelism"),
        ("let h = builder.spawn_unchecked(work);", "spawn_unchecked"),
        ("let pid = std::process::id();", "process"),
        ("let at = (&bar as *const Bar).addr();", "addr"),
        ("let at = ptr.expose_provenance();", "expose_provenance"),
        ("let c = core::arch::x86_64::_rdtsc();", "_rdtsc"),
        ("let ok = _rdrand64_step(&mut x);", "_rdrand"),
        ("let ok = _rdseed32_step(&mut x);", "_rdseed"),
    ] {
        assert_eq!(nondeterministic(&file(code)), Some(named), "{code}");
    }
    // Every wall-clock reader the SDK has, whatever is added there later (E6).
    for name in vike_model::time::clock::WALL_CLOCK_READERS {
        assert_eq!(nondeterministic(&file(&format!("let t = {name}();"))), Some(name), "{name}");
    }
}

/// The key moves with everything that decides an answer, a tick report's curve sampling and `returns_interval_ms`
/// among them (decision 0078's amendment of 2026-11-20 on the curve kept by time: the build's rule and the profile's
/// `engine.equity_sample_every` / `engine.seed_bar_interval_ms`), and with nothing else: not a profile's name.
#[test]
fn the_key_moves_with_the_build_the_texts_the_artifact_and_the_profile_and_not_with_a_name() {
    let tick = "[data]\nkind = \"tick\"\n\n[engine]\ncash = 1000.0\nseed_bar_interval_ms = 60000\n";
    let run = |profile: &str| {
        vike_backtesthub_client::proto::Request::Summarized(Box::new(
            vike_backtesthub_client::proto::Request::RunBacktest(profile.to_string()),
        ))
    };
    let models = std::collections::BTreeMap::new();
    let key = |build: &str, sha: &str, artifact: &[u8], profile: &str| {
        super::input_key(build, sha, artifact, &models, &run(profile)).expect("a key").0
    };
    let base = key("git a clean,", "s", b"so", tick);
    assert_eq!(key("git a clean,", "s", b"so", &format!("name = \"x\"\n{tick}")), base);
    for other in [
        key("git b clean,", "s", b"so", tick),
        key("git a clean,", "t", b"so", tick),
        key("git a clean,", "s", b"so2", tick),
        key("git a clean,", "s", b"so", &tick.replace("60000", "300000")),
        key("git a clean,", "s", b"so", &format!("{tick}equity_sample_every = 1\n")),
    ] {
        assert_ne!(other, base);
    }
    let with_model: std::collections::BTreeMap<String, String> =
        [("run-1".to_string(), "weights".to_string())].into_iter().collect();
    let modelled =
        super::input_key("git a clean,", "s", b"so", &with_model, &run(tick)).expect("a key").0;
    assert_ne!(modelled, base);
}

/// A finished run under `runs`, named `id`, its manifest's `detail` `detail`: the manifest `vike_runs` writes.
fn plant(runs: &std::path::Path, id: &str, manifest_id: &str, detail: serde_json::Value) {
    let dir = runs.join(id);
    std::fs::create_dir_all(&dir).unwrap();
    let manifest = vike_runs::RunManifest {
        schema: vike_runs::MANIFEST_SCHEMA,
        run_id: manifest_id.to_string(),
        kind: vike_runs::BACKTEST_RUN_KIND.to_string(),
        produced_by: "test".to_string(),
        started_at: "2026-11-20T00:00:00Z".to_string(),
        finished_at: "2026-11-20T00:00:01Z".to_string(),
        git_sha: None,
        strategy_sdk: None,
        fingerprint: None,
        config: vike_runs::RunConfig { path: None, name: None },
        detail,
        kept_rows: Vec::new(),
    };
    let path = dir.join(vike_runs::MANIFEST_FILE);
    vike_runs::write_json(&path, vike_runs::MANIFEST_FILE, &manifest).unwrap();
}

/// **The lookup reads an index, not the runs root** (the review's point 4): the keyed runs are read once, the newest
/// with the key is found by its one manifest, a run saved with no key (before this change) never is, a run the
/// retention removed is forgotten, a save is remembered, and a candidate whose directory went another way is dropped.
#[test]
fn the_newest_finished_run_with_the_key_is_found_through_the_index_and_a_run_with_none_never_is() {
    let runs = tempfile::tempdir().unwrap();
    let root = runs.path();
    plant(root, "1763600000-aa-0", "1763600000-aa-0", json!({ "protocol": "single" }));
    plant(
        root,
        "1763600001-aa-0",
        "1763600001-aa-0",
        json!({ "protocol": "single", INPUT_KEY: "k" }),
    );
    plant(
        root,
        "1763600002-aa-0",
        "1763600002-aa-0",
        json!({ "protocol": "single", INPUT_KEY: "k" }),
    );
    plant(
        root,
        "1763600003-aa-0",
        "1763600003-aa-0",
        json!({ "protocol": "single", INPUT_KEY: "o" }),
    );
    // A directory whose manifest names another run is no entry, nor an unfinished run (no manifest).
    plant(
        root,
        "1763600008-aa-0",
        "1763600002-aa-0",
        json!({ "protocol": "single", INPUT_KEY: "k" }),
    );
    std::fs::create_dir_all(root.join("1763600009-aa-0")).unwrap();
    assert_eq!(super::warm(root), 3, "the three keyed runs, read once");
    let newest = |key: &str| newest_keyed(root, key).map(|(found, _)| found.run_id);
    assert_eq!(newest("k").as_deref(), Some("1763600002-aa-0"));
    assert_eq!(newest("none"), None, "an old run has no key");
    // A run planted NOW is in no index until this process saves it: the scan is not repeated per request.
    plant(
        root,
        "1763600005-aa-0",
        "1763600005-aa-0",
        json!({ "protocol": "single", INPUT_KEY: "k" }),
    );
    assert_eq!(newest("k").as_deref(), Some("1763600002-aa-0"));
    let kept = Kept { key: "k".to_string(), strategy_sha256: String::new(), bytes: Vec::new() };
    remember(root, "1763600005-aa-0", &kept);
    assert_eq!(newest("k").as_deref(), Some("1763600005-aa-0"), "a save is remembered");
    forget(root, &["1763600005-aa-0".to_string()]);
    assert_eq!(newest("k").as_deref(), Some("1763600002-aa-0"), "a pruned run is forgotten");
    // A candidate removed behind this process's back is dropped at its lookup, and the next one answers.
    std::fs::remove_dir_all(root.join("1763600002-aa-0")).unwrap();
    assert_eq!(newest("k").as_deref(), Some("1763600001-aa-0"));
    // A candidate whose manifest no longer keeps the key is no answer either.
    plant(
        root,
        "1763600001-aa-0",
        "1763600001-aa-0",
        json!({ "protocol": "single", INPUT_KEY: "x" }),
    );
    assert_eq!(newest("k"), None);
}

/// **The daemon's own construction keys a request** (the review's point 1: the `--addr` arm hands its BARE commit as
/// `build`, which an older rule refused to key). `crate::backtest_cli::serve::serving_study`, what `run_serve` builds
/// its study with, carries the answer-from-saved identity, and that identity names the program by its bytes, the
/// commit as a plain input; once the program changes on disk (a deploy, a rebuild) it keys nothing more.
#[test]
fn the_serving_daemons_study_keys_on_its_programs_bytes_until_the_program_changes() {
    let scratch = tempfile::tempdir().unwrap();
    let program = scratch.path().join("vike-backend");
    std::fs::write(&program, b"the program's bytes").unwrap();
    let vars = [("VIKE_USER_DATA_DIR".to_string(), scratch.path().display().to_string())]
        .into_iter()
        .collect();
    let answers = AnswersFromSaved::of_program(&program, None);
    let study = crate::backtest_cli::serve::serving_study(
        &vars,
        None,
        &vike_config::Settings::default(),
        Some("e32ef713dc"),
        answers,
    );
    assert_eq!(study.build.as_deref(), Some("e32ef713dc"), "the build stays the bare commit");
    assert!(study.save_backtest_runs);
    let answers =
        study.answers_from_saved.as_ref().expect("a serving daemon answers from saved runs");
    let sha = vike_hub_protocol::hash::sha256_hex(b"the program's bytes");
    let identity = answers.identity(study.build.as_deref()).expect("its identity keys");
    assert!(identity.contains(&sha) && identity.contains("build e32ef713dc"), "{identity}");
    // Another program under the same name (the rebuild a build line does not show): nothing more is keyed.
    std::fs::write(&program, b"the program's other bytes, longer").unwrap();
    assert_eq!(answers.identity(study.build.as_deref()), None);
}

/// A program file under a scratch directory, and the scratch it lives in.
fn a_program() -> (tempfile::TempDir, std::path::PathBuf) {
    let scratch = tempfile::tempdir().unwrap();
    let program = scratch.path().join("vike-backend");
    std::fs::write(&program, b"the program's bytes").unwrap();
    (scratch, program)
}

/// A summarized profile run of `trend/x.rs` over bars up to `to`.
fn profile_run(to: &str) -> Request {
    Request::Summarized(Box::new(Request::RunBacktest(format!(
        "[data]\nvenue = \"binance\"\nsymbols = [\"BTCUSDT\"]\nkind = \"bar\"\ninterval = \"1m\"\n\
         from = \"0\"\nto = \"{to}\"\n\n[engine]\ncash = 100000.0\n\n[strategy]\nfile = \"trend/x.rs\"\n"
    ))))
}

/// **A request that may not be kept costs nothing** (the control review's point 4): asked fresh, over a window that
/// reaches today, or of a file that names the clock, it is refused before the datahub is dialled, and so before the
/// artifact is read and hashed (which comes after the dial); a request that may be kept dials it once.
#[test]
fn a_fresh_or_unkeepable_request_dials_no_datahub_and_hashes_no_artifact() {
    let (_scratch, program) = a_program();
    let dials = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&dials);
    let describe: Arc<dyn Fn() -> Option<String> + Send + Sync> = Arc::new(move || {
        counted.fetch_add(1, Ordering::Relaxed);
        Some(datahub_identity(&[build_feature("d1")]))
    });
    let study = StudyDirs {
        save_backtest_runs: true,
        answers_from_saved: AnswersFromSaved::of_program(&program, Some(describe)),
        ..StudyDirs::working_directory()
    };
    let run = |text: &str| RustRun {
        source: StrategySource {
            text: text.to_string(),
            sha256: "s".to_string(),
            optimistic_quotes: false,
            fill_at_close: false,
            backtest_only: false,
            sdk: None,
        },
        artifact: "sdk-1/absent/absent.so".to_string(),
        models: BTreeMap::new(),
    };
    // 2025-11-20: the past window closes in 1970, the open one in 2100.
    let now = 1_763_600_000_000;
    let (past, open) = (profile_run("4000000000"), profile_run("4102444800000"));
    let plain = run("let x = bar.close;");
    assert!(super::reaches_today(&open, now - now % 86_400_000));
    assert!(!super::reaches_today(&past, now - now % 86_400_000));
    assert!(super::reusable(&past, &study, &plain, now, true).is_none(), "fresh");
    assert!(super::reusable(&open, &study, &plain, now, false).is_none(), "reaches today");
    let clocked = run("let t = Instant::now();");
    assert!(super::reusable(&past, &study, &clocked, now, false).is_none(), "names the clock");
    assert_eq!(dials.load(Ordering::Relaxed), 0, "none of them dialled the datahub");
    // One that may be kept dials it (its artifact is absent here, so it is not keyed either).
    assert!(super::reusable(&past, &study, &plain, now, false).is_none());
    assert_eq!(dials.load(Ordering::Relaxed), 1);
}

/// **The datahub's BUILD is in the key** (the owner's yes of 2026-11-20 to stating it in the `Welcome`): another build
/// is another key, a datahub that states one is named by it alone, and one that states none (older than the entry) is
/// named by its advertised features and still keys.
#[test]
fn a_datahub_build_change_moves_the_key_and_an_older_datahub_still_keys() {
    let (_scratch, program) = a_program();
    let identity_with = |features: Vec<String>| {
        let describe: Arc<dyn Fn() -> Option<String> + Send + Sync> =
            Arc::new(move || Some(datahub_identity(&features)));
        let answers = AnswersFromSaved::of_program(&program, Some(describe)).expect("hashed");
        answers.identity(Some("e32ef713dc")).expect("it keys")
    };
    let one = identity_with(vec!["load_bars".to_string(), build_feature("aaa")]);
    let other = identity_with(vec!["load_bars".to_string(), build_feature("bbb")]);
    let older = identity_with(vec!["load_bars".to_string(), "series_days".to_string()]);
    assert!(one.contains("datahub build aaa"), "{one}");
    assert!(older.contains("datahub features load_bars series_days"), "{older}");
    let models = BTreeMap::new();
    let request = profile_run("4000000000");
    let key = |hub: &str| super::input_key(hub, "s", b"so", &models, &request).expect("a key").0;
    assert_ne!(key(&one), key(&other), "another datahub build is another key");
    assert_ne!(key(&one), key(&older));
    let reordered = identity_with(vec!["series_days".to_string(), build_feature("aaa")]);
    assert_eq!(key(&one), key(&reordered), "a datahub that states its build is named by it");
}

/// **A hit whose saved run is pruned under it is a miss** (the control review's point 3): a single run's answer holds its
/// whole curve, so a run whose curve is gone (the retention removed it on another connection after the lookup found the
/// run) answers nothing; with its curve it answers.
#[test]
fn a_single_run_whose_curve_is_gone_under_its_lookup_is_a_miss() {
    let runs = tempfile::tempdir().unwrap();
    let root = runs.path();
    let id = "1763600010-aa-0";
    let answer = Response::RunSummary(Box::default());
    let points: [(RunSummaryWire, Vec<vike_model::Trade>); 1] =
        [(RunSummaryWire::default(), Vec::new())];
    let document = KeptAnswerOf { reads: &[], answer: &answer, points: &points, rows: &[] };
    let bytes = rmp_serde::encode::to_vec_named(&document).unwrap();
    let named = json!({ "fnv": vike_utility::fnv::fnv1a64_hex(&bytes), "bytes": bytes.len() });
    plant(root, id, id, json!({ "protocol": "single", INPUT_KEY: "race", KEPT_ANSWER_KEY: named }));
    std::fs::write(root.join(id).join(vike_runs::KEPT_ANSWER_FILE), &bytes).unwrap();
    let kept = Kept { key: "race".to_string(), strategy_sha256: String::new(), bytes: Vec::new() };
    remember(root, id, &kept);
    let reusable = Reusable {
        key: "race".to_string(),
        name: None,
        strategy_sha256: String::new(),
        reads: ReadWitnesses::new(0),
    };
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(vike_data::HistoryStoreMemory::new());
    assert!(super::answer(&reusable, root, &store).is_none(), "its curve is gone: a miss");
    let curve = vike_runs::RunEquityCurve { equity: vec![1.0, 2.0], timestamp: vec![0, 60_000] };
    std::fs::write(root.join(id).join(vike_runs::EQUITY_CURVE_FILE), curve.to_bytes()).unwrap();
    let Some(Response::RunSummary(summary)) = super::answer(&reusable, root, &store) else {
        panic!("its curve held, it answers");
    };
    assert_eq!(summary.answered_from_saved_run, id);
}

/// The saver keeps a run's key and answer only when its report names the text the key names.
#[test]
fn a_run_is_kept_only_when_its_report_names_the_keyed_strategy_text() {
    let kept =
        Kept { key: "k".to_string(), strategy_sha256: "abc".to_string(), bytes: vec![1, 2, 3] };
    let mut other = json!({ "protocol": "single", "strategy_sha256": "def" });
    assert!(keep_into(Some(&kept), &mut other).is_none(), "the file changed under the run");
    assert!(other.get(INPUT_KEY).is_none());
    let mut detail = json!({ "protocol": "single", "strategy_sha256": "abc" });
    assert_eq!(keep_into(Some(&kept), &mut detail).map(Kept::bytes), Some(&[1u8, 2, 3][..]));
    assert_eq!(detail[INPUT_KEY], "k");
    assert_eq!(detail[KEPT_ANSWER_KEY]["bytes"], 3);
    assert!(
        keep_into(None, &mut json!({})).is_none(),
        "a run that may not answer again keeps nothing"
    );
}

/// What is kept is encoded from BORROWED parts into bytes the owned document reads back, and the encoding stops at the
/// cap instead of growing a buffer past it.
#[test]
fn a_kept_answer_is_encoded_from_borrowed_parts_and_never_past_the_cap() {
    let answer = Response::Report("{\"final_equity\":1.5}".to_string());
    let document = KeptAnswerOf { reads: &[], answer: &answer, points: &[], rows: &[] };
    let mut bytes = Capped(Vec::new());
    rmp_serde::encode::write_named(&mut bytes, &document).expect("under the cap");
    let back: KeptAnswer = rmp_serde::from_slice(&bytes.0).expect("the owned document reads it");
    assert!(matches!(back.answer, Response::Report(text) if text == "{\"final_equity\":1.5}"));
    let over = Response::Report("x".repeat(KEPT_ANSWER_MAX_BYTES));
    let mut capped = Capped(Vec::new());
    let document = KeptAnswerOf { reads: &[], answer: &over, points: &[], rows: &[] };
    assert!(rmp_serde::encode::write_named(&mut capped, &document).is_err(), "over the cap");
    assert!(capped.0.len() <= KEPT_ANSWER_MAX_BYTES, "{} bytes buffered", capped.0.len());
}

/// A summary says where it came from in its field; a text report in its LAST key, every byte before it the saved one's.
#[test]
fn an_answer_from_a_saved_run_says_so() {
    let report = "{\"name\":null,\"final_equity\":1.5}".to_string();
    let Response::Report(marked_report) =
        marked(Response::Report(report.clone()), "1763600002-aa-0")
    else {
        panic!("a report stays a report");
    };
    assert_eq!(
        marked_report,
        format!("{},\"{ANSWERED_FROM_KEY}\":\"1763600002-aa-0\"}}", &report[..report.len() - 1])
    );
    let summary = Response::RunSummary(Box::default());
    let Response::RunSummary(summary) = marked(summary, "1763600002-aa-0") else { panic!() };
    assert_eq!(summary.answered_from_saved_run, "1763600002-aa-0");
    assert_eq!(
        *summary,
        RunSummaryWire {
            answered_from_saved_run: summary.answered_from_saved_run.clone(),
            ..RunSummaryWire::default()
        }
    );
    // An answer that is neither (a refusal) goes back as it came.
    assert!(matches!(marked(Response::Error("no".to_string()), "x"), Response::Error(_)));
}
