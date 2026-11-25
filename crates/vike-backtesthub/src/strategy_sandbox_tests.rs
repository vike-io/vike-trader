//! The route's predicate, the backtest's mount facts, and the history channel end to end in one process
//! (a `PipeHistStore` on one end of two pipes, the parent's `answer` on the other).

use vike_backtesthub_client::proto::Request;
use vike_backtesthub_client::test_support::btcusdt_1m_bars;
use vike_backtesthub_client::wire_studio::StrategySpecWire;

use super::{facts_of, rust_files_of};
use crate::studio_run::StrategyFiles;

/// The ONE `.rs` table `request` routes to a child ([`rust_files_of`], a run of one mount): its path, its text and
/// sha256, and its SDK.
fn one_rust_file_of(
    request: &Request,
    files: &StrategyFiles,
) -> Option<(String, vike_backtest::run::StrategySource, Option<u32>)> {
    let mut tables = rust_files_of(request, files)?.expect("no mixed portfolio here");
    assert_eq!(tables.len(), 1, "one mount");
    let (table, source) = tables.pop()?;
    Some((table.file, source, table.sdk))
}

/// A process with no builder runs the artifact already built from the text, named by its path under the
/// builds directory (`sdk-N/<folder>/<sha256>.so`, decision 0296): never one of the layout before SDKs,
/// and nothing when none is built.
#[test]
fn with_no_builder_the_artifact_already_built_is_run() {
    let builds = tempfile::tempdir().expect("builds");
    let sha = "ab".repeat(32);
    let plant = |relative: &str| {
        let path = builds.path().join(relative);
        std::fs::create_dir_all(path.parent().expect("a folder")).expect("the folder");
        std::fs::write(path, b"so").expect("the artifact");
    };
    assert_eq!(super::already_built(builds.path(), &sha), None, "nothing built");
    plant(&format!("0123abcd/{sha}.so"));
    assert_eq!(super::already_built(builds.path(), &sha), None, "the layout before SDKs");
    plant(&format!("sdk-dev/0123abcd/{sha}.so"));
    let found = super::already_built(builds.path(), &sha);
    assert_eq!(found, Some(format!("sdk-dev/0123abcd/{sha}.so")));
}

/// A sandboxed child's run of a file built against SDK 7 (an override: the child read 7 off its artifact's
/// path, `vike_strategy_sandbox::artifact_of`) records 7 in its report, `TradingResults::strategy_sdk`; a run
/// of a file with none records none and writes no key. In process: the child's own dispatch over the loaded
/// file (`loaded_strategy_files`), a do-nothing strategy, an hour of bars.
#[test]
fn the_sdk_a_loaded_file_was_built_against_is_what_its_report_records() {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use vike_backtesthub_client::proto::Response;
    use vike_data::{HistoryStore, HistoryStoreMemory};
    use vike_marketdata::Bar;

    use super::loaded_strategy_files;
    use crate::backtesthub_server::StudyDirs;
    use crate::named_run::NamedRunLane;
    use crate::studio_run::StrategyFileBuild;

    struct Idle;
    impl vike_model::Strategy<vike_simulator::OrderBrokerSimulator> for Idle {}

    let memory = HistoryStoreMemory::new();
    let bars: Vec<Bar> = (0..60i64)
        .map(|i| Bar {
            timestamp: i * 60_000,
            open: 100.0,
            high: 101.0,
            low: 99.0,
            close: 100.0 + (i % 3) as f64,
            volume: 1.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: Some("BTCUSDT".into()),
        })
        .collect();
    memory.append_bars("binance", "BTCUSDT", "1m", &bars, None).expect("seed bars");
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(memory);
    let text = "//! A.\n//!\n//! ```toml\n//! [strategy]\n//! runs_on = \"backtest\"\n//! ```\npub fn x() {}\n";
    let profile = "[data]\nvenue = \"binance\"\nsymbols = [\"BTCUSDT\"]\ninterval = \"1m\"\nkind = \"bar\"\n\
                   from = \"0\"\nto = \"100000000\"\n[engine]\ncash = 1000.0\n[strategy]\nfile = \"other/a.rs\"\n";
    for sdk in [Some(7), None] {
        let request = Request::RunBacktest(profile.to_string());
        let build: StrategyFileBuild = Box::new(|_params, _facts| Ok(Box::new(Idle)));
        let strategies = loaded_strategy_files(
            &request,
            vec![(Ok(build), sdk)],
            vec![(text.to_string(), BTreeMap::new())],
        );
        let study = StudyDirs { strategies, ..StudyDirs::working_directory() };
        let answer = crate::backtesthub_server::dispatch::handle_request_here(
            request,
            &store,
            &study,
            NamedRunLane::DISARMED,
            None,
        );
        let Response::Report(json) = answer else { panic!("a report, got {answer:?}") };
        let compact = json.replace([' ', '\n'], "");
        match sdk {
            Some(_) => assert!(compact.contains("\"strategy_sdk\":7"), "{json}"),
            None => assert!(!json.contains("strategy_sdk"), "no key without an SDK: {json}"),
        }
    }
}

fn slice_request(spec: StrategySpecWire) -> Request {
    Request::RunSlice { spec, slice: Box::new(btcusdt_1m_bars()), params: None }
}

fn file(path: &str, params_toml: &str) -> Request {
    slice_request(StrategySpecWire::File {
        file: path.into(),
        params_toml: params_toml.into(),
        sdk: None,
    })
}

/// A `.rs` FILE on a hub goes to a child, read as the run reads it (its text and sha256); a `.rhai` file,
/// an editor buffer, an artifact by (name, sha), a process that builds `.rs` files itself, and a file the
/// read refuses all stay in this process, where the run answers them in their own words.
#[test]
fn only_a_readable_rs_file_on_a_hub_goes_to_a_child() {
    let dir = tempfile::tempdir().expect("strategies folder");
    let text = "//! A.\n//!\n//! ```toml\n//! [strategy]\n//! runs_on = \"backtest\"\n//! [params.every]\n//! type = \"integer\"\n//! ```\npub fn x() {}\n";
    std::fs::create_dir_all(dir.path().join("other")).expect("folder");
    std::fs::write(dir.path().join("other/a.rs"), text).expect("a.rs");
    std::fs::write(dir.path().join("other/b.rhai"), text).expect("b.rhai");
    let hub = StrategyFiles::hub(dir.path().to_path_buf());

    let (path, source, sdk) =
        one_rust_file_of(&file("other/a.rs", "every = 3"), &hub).expect("routed");
    assert_eq!(path, "other/a.rs");
    assert_eq!(sdk, None, "a Studio slice names no SDK: the file's own pin builds");
    assert_eq!(source.text, text);
    assert_eq!(source.sha256, vike_hub_protocol::hash::sha256_hex(text.as_bytes()));

    assert!(one_rust_file_of(&file("other/b.rhai", ""), &hub).is_none(), "Rhai runs here");
    assert!(one_rust_file_of(&file("other/missing.rs", ""), &hub).is_none(), "the read refuses it");
    assert!(one_rust_file_of(&file("other/a.rs", "fast = 1"), &hub).is_none(), "undeclared key");
    assert!(
        one_rust_file_of(
            &file("other/a.rs", ""),
            &StrategyFiles::stand_in(vike_strategy_lab::stand_in)
        )
        .is_none()
    );
    assert!(
        one_rust_file_of(&slice_request(StrategySpecWire::Rhai("fn on_bar() {}".into())), &hub)
            .is_none()
    );
    let plugin = StrategySpecWire::Plugin {
        name: "a".into(),
        sha: "a".repeat(64),
        params_toml: String::new(),
    };
    assert!(one_rust_file_of(&slice_request(plugin), &hub).is_none(), "refused in this process");
    assert!(one_rust_file_of(&Request::Ping, &hub).is_none());
}

/// A backtest profile's `[strategy] sdk = N` is what the hub hands the builder for its `.rs` file
/// (decision 0296's override), on every profile verb; a profile without the key hands `None`.
#[test]
fn a_profiles_sdk_override_reaches_the_build() {
    let dir = tempfile::tempdir().expect("strategies folder");
    let text = "//! A.\n//!\n//! ```toml\n//! [strategy]\n//! runs_on = \"backtest\"\n//! ```\npub fn x() {}\n";
    std::fs::create_dir_all(dir.path().join("other")).expect("folder");
    std::fs::write(dir.path().join("other/a.rs"), text).expect("a.rs");
    let hub = StrategyFiles::hub(dir.path().to_path_buf());
    let profile = |sdk_line: &str| {
        format!(
            "[data]\nvenue = \"binance\"\nsymbols = [\"BTCUSDT\"]\ninterval = \"1h\"\nkind = \"bar\"\n\
             from = \"2026-01-01T00\"\nto = \"2026-01-02T00\"\n[engine]\ncash = 1000.0\n\
             [strategy]\nfile = \"other/a.rs\"\n{sdk_line}"
        )
    };
    let pinned = profile("sdk = 7\n");
    for request in [
        Request::RunBacktest(pinned.clone()),
        Request::Summarized(Box::new(Request::RunBacktest(pinned.clone()))),
        Request::RunWalkforwardProfile { profile_toml: pinned.clone() },
    ] {
        let (_, _, sdk) = one_rust_file_of(&request, &hub).expect("routed");
        assert_eq!(sdk, Some(7));
    }
    let (_, _, sdk) = one_rust_file_of(&Request::RunBacktest(profile("")), &hub).expect("routed");
    assert_eq!(sdk, None);

    // A Studio run's file spec names it the same way, on each of the three Studio verbs.
    let picked = StrategySpecWire::File {
        file: "other/a.rs".into(),
        params_toml: String::new(),
        sdk: Some(7),
    };
    let (_, _, sdk) = one_rust_file_of(&slice_request(picked.clone()), &hub).expect("routed");
    assert_eq!(sdk, Some(7));
    let summarized = Request::Summarized(Box::new(slice_request(picked)));
    assert_eq!(one_rust_file_of(&summarized, &hub).expect("routed").2, Some(7));
}

/// A Studio slice of a `.rhai` file that names an SDK is refused as a profile's `[strategy] sdk` beside
/// one is, in the same words, and never runs on this vike's own code as though it had been honoured.
#[test]
fn a_studio_rhai_file_naming_an_sdk_is_refused() {
    use vike_studio_core::wire_run::to_strategy_spec;

    let dir = tempfile::tempdir().expect("strategies folder");
    let text = "//! A.\n//!\n//! ```toml\n//! [strategy]\n//! runs_on = \"backtest\"\n//! ```\nfn on_bar() {}\n";
    std::fs::write(dir.path().join("b.rhai"), text).expect("b.rhai");
    let hub = StrategyFiles::hub(dir.path().to_path_buf());
    let spec = to_strategy_spec(&StrategySpecWire::File {
        file: "b.rhai".into(),
        params_toml: String::new(),
        sdk: Some(3),
    })
    .expect("the wire spec converts");
    let strategy = crate::studio_run::strategy_config(&spec, &hub).expect("the table builds");
    assert_eq!(strategy.sdk, Some(3), "the SDK reaches the slice's table");
    let range = vike_data::TimestampRange { start: Some(0), end: Some(3_600_000) };
    let kind = vike_model::run_request::replay::ReplayKind::Bar;
    let Err(refused) = vike_backtest::run::profile::ProfileBacktest::for_slice(
        "binance",
        &["BTCUSDT".to_string()],
        "1h",
        range,
        kind,
        None,
        strategy,
    ) else {
        panic!("a .rhai slice naming an SDK loaded")
    };
    assert!(refused.message().contains("strategy.sdk = 3"), "{refused:?}");
}

/// A `type = "model"` key: the hub reads the named run's `model.txt` (and only a run of its own runs root),
/// the child's builder hands the file that TEXT where the request wrote the run id, and a run id the hub
/// did not read (a sweep axis) or cannot read refuses by name.
#[test]
fn a_model_key_crosses_as_the_runs_text_and_the_request_keeps_the_id() {
    use std::sync::{Arc, Mutex};

    use vike_backtest::run::profile::ProfileStrategy;
    use vike_model::params::{ParamValue, Params};

    use super::{loaded_strategy_files, model_texts_of};
    use crate::studio_run::{RustFileBuild, StrategyFileBuild};

    const MODEL: &str = "tree\nversion=v4\nend of trees\n";
    let dir = tempfile::tempdir().expect("strategies folder");
    let runs = tempfile::tempdir().expect("runs root");
    let text = "//! M.\n//!\n//! ```toml\n//! [strategy]\n//! runs_on = \"backtest\"\n//! [params.model]\n//! type = \"model\"\n//! [params.every]\n//! type = \"integer\"\n//! ```\npub fn x() {}\n";
    std::fs::create_dir_all(dir.path().join("other")).expect("folder");
    std::fs::write(dir.path().join("other/m.rs"), text).expect("m.rs");
    let run = runs.path().join("1763251200-study-0");
    std::fs::create_dir_all(&run).expect("run dir");
    std::fs::write(run.join(vike_runs::MODEL_ARTIFACT), MODEL).expect("model");
    let manifest = "{\"run_id\":\"1763251200-study-0\",\"kind\":\"study\",\"produced_by\":\"study\",\
                    \"started_at\":\"2026-11-16T00:00:00Z\",\"finished_at\":\"2026-11-16T00:00:01Z\",\
                    \"git_sha\":null,\"config\":{},\"detail\":{}}";
    std::fs::write(run.join(vike_runs::MANIFEST_FILE), manifest).expect("manifest");
    let hub = StrategyFiles::hub(dir.path().to_path_buf());

    let request = file("other/m.rs", "model = \"1763251200-study-0\"\nevery = 3\n");
    let Some(Ok(mut tables)) = rust_files_of(&request, &hub) else { panic!("routed") };
    let (table, source) = tables.remove(0);
    let models = model_texts_of(&table.params, &source, runs.path()).expect("the run reads");
    assert_eq!(models.get("1763251200-study-0").map(String::as_str), Some(MODEL));
    assert_eq!(models.len(), 1);

    let seen = Arc::new(Mutex::new(String::new()));
    let kept = Arc::clone(&seen);
    let build: StrategyFileBuild = Box::new(move |params_toml: &str, _facts: &str| {
        *kept.lock().expect("lock") = params_toml.to_string();
        Err("seen".to_string())
    });
    let loaded = loaded_strategy_files(
        &request,
        vec![(Ok(build), None)],
        vec![(source.text.clone(), models)],
    );
    let RustFileBuild::Loaded(mounts) = loaded.rust else {
        panic!("a loaded file");
    };
    let params = |toml_text: &str| -> Params { toml::from_str(toml_text).expect("params") };
    let mut config = ProfileStrategy::new("other/m.rs", params("model = \"1763251200-study-0\""));
    config.builder = Some(mounts[0].builder.clone());
    assert!(config.build().is_err(), "the stand-in build stops after it saw the params");
    let crossed = params(&seen.lock().expect("lock"));
    assert_eq!(crossed.get("model").and_then(ParamValue::as_str), Some(MODEL));
    assert_eq!(
        config.params.get("model").and_then(ParamValue::as_str),
        Some("1763251200-study-0"),
        "the run's own params keep the id"
    );

    config.params = params("model = \"1763251200-other-0\"");
    let refused = config.build().err().expect("an id the hub did not read").to_string();
    assert!(refused.contains("the hub read no model for it"), "{refused}");

    let gone = params("model = \"1763251200-gone-0\"\n");
    let why = model_texts_of(&gone, &source, runs.path()).expect_err("no such run");
    assert!(why.starts_with("`model` names model run \"1763251200-gone-0\": no "), "{why}");
    let plain = params("every = 3\n");
    assert!(model_texts_of(&plain, &source, runs.path()).expect("nothing").is_empty());
}

/// **A child that loaded several mounts' files builds each by its mount** (decision 0281's portfolio, one child): the
/// mount's own builder and the mount's own text (its header judges the mount's params), a mount past the files it was
/// handed refused by name, and the one-table entry (`StrategyFiles::prepare`) refused rather than building every table
/// with the first mount's file. A `[strategy]` profile is one mount: a second mount states no symbol.
#[test]
fn each_loaded_mount_builds_by_its_own_index() {
    use std::collections::BTreeMap;

    use vike_backtest::run::profile::ProfileStrategy;
    use vike_model::params::Params;

    use super::loaded_strategy_files;
    use crate::studio_run::StrategyFileBuild;

    let header = |key: &str| {
        format!(
            "//! A.\n//!\n//! ```toml\n//! [strategy]\n//! runs_on = \"backtest\"\n//! [params.{key}]\n//! \
             type = \"integer\"\n//! ```\npub fn x() {{}}\n"
        )
    };
    let refusing = |word: &'static str| -> StrategyFileBuild {
        Box::new(move |_params, _facts| Err(format!("built by mount {word}")))
    };
    let request = file("other/a.rs", "");
    let files = loaded_strategy_files(
        &request,
        vec![(Ok(refusing("zero")), None), (Ok(refusing("one")), Some(3))],
        vec![(header("every"), BTreeMap::new()), (header("fast"), BTreeMap::new())],
    );
    let table = |key: &str| {
        let params: Params = toml::from_str(&format!("{key} = 1")).expect("params");
        ProfileStrategy::new("other/a.rs", params)
    };
    let built = |mount: usize, key: &str| -> String {
        let mut strategy = table(key);
        match files.prepare_mount(mount, &mut strategy) {
            Ok(()) => strategy.build().err().expect("the stand-in refuses").to_string(),
            Err(e) => format!("not prepared: {e}"),
        }
    };
    assert!(built(0, "every").contains("built by mount zero"), "{}", built(0, "every"));
    assert!(built(1, "fast").contains("built by mount one"), "{}", built(1, "fast"));
    assert!(
        built(1, "every").starts_with("not prepared"),
        "mount 1's own header judges its params"
    );
    let mut second = table("fast");
    files.prepare_mount(1, &mut second).expect("mount 1");
    assert_eq!(second.source.as_ref().and_then(|source| source.sdk), Some(3), "its own SDK");
    let past = built(2, "every");
    assert!(past.contains("is mount 2") && past.contains("loaded 2 mount file(s)"), "{past}");
    let one_table = files.prepare(&mut table("every")).expect_err("ambiguous").to_string();
    assert!(one_table.contains("prepare_mount"), "{one_table}");

    let profile = "[data]\nvenue = \"binance\"\nsymbols = [\"BTCUSDT\", \"ETHUSDT\"]\ninterval = \"1h\"\n\
                   kind = \"bar\"\nfrom = \"2026-01-01T00\"\nto = \"2026-01-02T00\"\n[engine]\ncash = 1000.0\n\
                   [strategy]\nfile = \"other/a.rs\"\n";
    let request = Request::RunBacktest(profile.to_string());
    let symbol = |mount| {
        serde_json::from_str::<vike_model::params::MountFacts>(&facts_of(&request, mount))
            .expect("facts JSON")
            .symbol
    };
    assert_eq!((symbol(0), symbol(1)), ("BTCUSDT".to_string(), String::new()));
}

/// **A child runs exactly the mounts it loaded** (decision 0281's portfolio, one child): a start message naming another
/// count of mounts than the artifacts it was named is refused before anything runs (exit 3, its own build the only
/// frame it wrote); two and two run, and a `[strategy]` profile over two loaded mounts (one table) is refused by the
/// one-table entry, never built with the first mount's file.
#[test]
fn a_child_runs_exactly_the_mounts_it_loaded() {
    use std::collections::BTreeMap;
    use std::io::Read;
    use std::process::ExitCode;

    use vike_backtesthub_client::proto::Response;
    use vike_hub_protocol::child_pipe::{
        check_build, read_message, send_build, write_message_named,
    };

    use super::wire::{ChildFrame, ChildRequest};

    const BUILD: &str = "the build";
    let profile = "[data]\nvenue = \"binance\"\nsymbols = [\"BTCUSDT\"]\ninterval = \"1h\"\nkind = \"bar\"\n\
                   from = \"2026-01-01T00\"\nto = \"2026-01-02T00\"\n[engine]\ncash = 1000.0\n\
                   [strategy]\nfile = \"other/a.rs\"\n";
    let request = Request::RunBacktest(profile.to_string());
    let no_models = BTreeMap::<String, String>::new();
    let answered = |mounts: usize| {
        let mut input = Vec::new();
        send_build(&mut input, BUILD).expect("the hub's build");
        let first = ChildRequest {
            request: &request,
            paramscan_threads: 1,
            mounts: vec![("", &no_models); mounts],
        };
        write_message_named(&mut input, &first).expect("the start message");
        let (mut said, output) = std::io::pipe().expect("pipe");
        let builds = vec![(Err("not loaded".into()), None), (Err("not loaded".into()), None)];
        let code = super::serve_child(
            Box::new(std::io::Cursor::new(input)),
            Box::new(output),
            BUILD,
            builds,
        );
        let mut written = Vec::new();
        said.read_to_end(&mut written).expect("its output");
        (format!("{code:?}"), written)
    };

    let (code, written) = answered(1);
    assert_eq!(code, format!("{:?}", ExitCode::from(3)), "one mount for two artifacts");
    let mut own = Vec::new();
    send_build(&mut own, BUILD).expect("a build");
    assert_eq!(written, own, "nothing but its own build");

    let (code, written) = answered(2);
    assert_eq!(code, format!("{:?}", ExitCode::SUCCESS));
    let mut frames = written.as_slice();
    check_build(&mut frames, BUILD, "the child").expect("its build");
    let ChildFrame::Answer(answer) = read_message(&mut frames).expect("a frame") else {
        panic!("the answer is its first frame after the build");
    };
    let Response::Error(why) = &*answer else { panic!("a refusal, got {answer:?}") };
    assert!(why.contains("prepare_mount"), "{why}");
}

/// A backtest states its symbol and interval alone, and a request with no slice states nothing.
#[test]
fn a_backtest_states_its_symbol_and_interval_alone() {
    let facts: vike_model::params::MountFacts =
        serde_json::from_str(&facts_of(&slice_request(StrategySpecWire::Rhai(String::new())), 0))
            .expect("facts JSON");
    assert_eq!(
        facts,
        vike_model::params::MountFacts {
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            ..Default::default()
        }
    );
    let none = vike_model::params::MountFacts::default();
    assert_eq!(facts_of(&Request::Ping, 0), serde_json::to_string(&none).expect("json"));
}

/// Only a child that serves ONE run (a Studio slice, a profile's single backtest, either summarized) keeps no bar
/// frame; a scan, a walk, a permutation test and a profile that does not parse keep them, as every child did.
#[test]
fn only_a_single_runs_child_reads_each_series_once() {
    use super::pipe_store::reads_again;
    let profile = "[data]\nvenue = \"binance\"\nsymbols = [\"BTCUSDT\"]\nkind = \"bar\"\n\
                   interval = \"1m\"\nfrom = \"0\"\nto = \"100000000\"\n\n[engine]\ncash = 1000.0\n\n\
                   [strategy]\nfile = \"other/a.rs\"\n";
    let single = Request::RunBacktest(profile.to_string());
    let permutation =
        Request::RunBacktest(format!("{profile}\n[permutation]\npermutations = 5\nseed = 3\n"));
    let slice = file("other/a.rs", "");
    let summarized = |request: &Request| Request::Summarized(Box::new(request.clone()));
    for once in [&single, &slice, &summarized(&single), &summarized(&slice)] {
        assert!(!reads_again(once), "{once:?}");
    }
    let walk = Request::RunWalkforwardProfile { profile_toml: profile.to_string() };
    let scan = Request::RunParamscanProfile {
        profile_toml: profile.to_string(),
        rank_by: None,
        search: None,
    };
    let broken = Request::RunBacktest("not a profile".to_string());
    for again in [&permutation, &walk, &scan, &summarized(&permutation), &broken] {
        assert!(reads_again(again), "{again:?}");
    }
}

mod channel {
    use std::io::Write;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use vike_backtesthub_client::proto::Request;
    use vike_backtesthub_client::test_support::btcusdt_1m_bars;
    use vike_backtesthub_client::wire_studio::StrategySpecWire;
    use vike_data::{HistoryStore, HistoryStoreError, HistoryStoreMemory, TimestampRange};
    use vike_datahub_client::proto::BAR_COLUMNS;
    use vike_hub_protocol::child_pipe::{read_message, write_message};
    use vike_marketdata::Bar;

    use super::super::pipe_store::PipeHistStore;
    use super::super::relay;
    use super::super::wire::{
        BARS_PER_FRAME, ChildFrame, ReadAnswerFrame, StoreRead, bar_part_into, push_bar_part,
        row_count,
    };

    /// `n` bars, the sixth with a NaN close, the seventh an infinite volume, the eleventh a NaN bid and
    /// every eleventh a negative-zero ask: every price bit crosses, where JSON would have written `null` and
    /// refused to read it back, and an absent optional is told from a present zero. The NaN bid crosses
    /// ABSENT ([`carried`]).
    fn bars(n: usize) -> Vec<Bar> {
        (0..n)
            .map(|i| Bar {
                timestamp: 60_000 * i as i64,
                open: 100.0 + i as f64 / 7.0,
                high: 101.0 + i as f64 / 7.0,
                low: 99.0 + i as f64 / 7.0,
                close: if i == 5 { f64::NAN } else { 100.5 + i as f64 / 3.0 },
                volume: if i == 6 { f64::INFINITY } else { 1.0 / (i as f64 + 3.0) },
                funding: (i % 3 == 0).then_some(1e-4 * i as f64),
                bid: (i % 5 == 0).then_some(if i == 10 { f64::NAN } else { 99.0 }),
                ask: (i % 11 == 0).then_some(-0.0),
                // Runs of a symbol and of none, so the symbol column's run-length coding is crossed.
                symbol: (i % 7 < 4).then(|| "BTCUSDT".into()),
            })
            .collect()
    }

    /// A bar's every field, floats by their bits.
    fn bits(b: &Bar) -> (i64, [u64; 5], [Option<u64>; 3], Option<String>) {
        let f = [b.open, b.high, b.low, b.close, b.volume];
        let o = [b.funding, b.bid, b.ask];
        (
            b.timestamp,
            f.map(f64::to_bits),
            o.map(|v| v.map(f64::to_bits)),
            b.symbol.map(String::from),
        )
    }

    /// What a bar part carries of `b`: every field by its bits ([`bits`]), but an optional that is not finite
    /// ABSENT, the bar-columns rule (`vike_datahub_client::proto::encode_bar_columns_into`), by which the datahub
    /// already hands the parent its bars.
    fn carried(b: &Bar) -> (i64, [u64; 5], [Option<u64>; 3], Option<String>) {
        let mut b = b.clone();
        for optional in [&mut b.funding, &mut b.bid, &mut b.ask] {
            *optional = optional.filter(|v| v.is_finite());
        }
        bits(&b)
    }

    /// The child's store on one end, the parent's answers on the other: what `relay` does for every
    /// `ChildFrame::Read`, counted. The store keeps every answer ([`PipeHistStore::new`]).
    fn serve(store: HistoryStoreMemory) -> (Arc<PipeHistStore>, Arc<AtomicUsize>) {
        serve_as(store, PipeHistStore::new)
    }

    /// [`serve`] with the child's store opened by `open`.
    fn serve_as(
        store: HistoryStoreMemory,
        open: impl FnOnce(Box<dyn std::io::Read + Send>, Box<dyn Write + Send>) -> PipeHistStore,
    ) -> (Arc<PipeHistStore>, Arc<AtomicUsize>) {
        let (mut from_child, child_out) = std::io::pipe().expect("pipe up");
        let (child_in, mut to_child) = std::io::pipe().expect("pipe down");
        let reads = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&reads);
        std::thread::spawn(move || {
            while let Ok(ChildFrame::Read(read)) = read_message(&mut from_child) {
                counted.fetch_add(1, Ordering::Relaxed);
                relay::answer(&mut to_child, &store, read).expect("answer");
            }
        });
        let pipe = open(Box::new(child_in), Box::new(child_out));
        (Arc::new(pipe), reads)
    }

    /// **A child that serves ONE run keeps no bar frame** (`pipe_store::reads_again`): its bars are a keeping
    /// child's bit for bit, read into one buffer, and a repeat read crosses the channel again because nothing of the
    /// answer was kept; every other answer is still kept. A scan's child keeps its bars, and a repeat stays in it.
    #[test]
    fn a_single_runs_child_keeps_no_bar_frame_and_a_scans_child_does() {
        let rows = bars(BARS_PER_FRAME * 2 + 17);
        let seeded = || {
            let store = HistoryStoreMemory::new();
            store.append_bars("binance", "BTCUSDT", "1m", &rows, None).expect("seed");
            store
        };
        let read = |pipe: &PipeHistStore| {
            pipe.load_bars("binance", "BTCUSDT", "1m", TimestampRange::all()).expect("piped read")
        };
        let single = super::slice_request(StrategySpecWire::File {
            file: "other/a.rs".into(),
            params_toml: String::new(),
            sdk: None,
        });
        let scan = Request::RunParamscan {
            spec: StrategySpecWire::File {
                file: "other/a.rs".into(),
                params_toml: String::new(),
                sdk: None,
            },
            slice: Box::new(btcusdt_1m_bars()),
            paramscan: vike_backtesthub_client::wire_studio::ParamscanWire { axes: Vec::new() },
            params: None,
        };
        let (once, once_reads) = serve_as(seeded(), |i, o| PipeHistStore::serving(&single, i, o));
        let (kept, kept_reads) = serve_as(seeded(), |i, o| PipeHistStore::serving(&scan, i, o));

        let (first, keeping) = (read(&once), read(&kept));
        assert!(first.iter().map(bits).eq(keeping.iter().map(bits)), "the same bars, bit for bit");
        assert_eq!(first.capacity(), first.len(), "sized once, from the count the parent sent");
        let again = read(&once);
        assert!(again.iter().map(bits).eq(first.iter().map(bits)));
        assert_eq!(once_reads.load(Ordering::Relaxed), 2, "a single run's child kept no bar frame");
        read(&kept);
        assert_eq!(kept_reads.load(Ordering::Relaxed), 1, "a scan's child keeps its bars");

        once.list_series().expect("a row answer");
        once.list_series().expect("the same answer, kept");
        assert_eq!(once_reads.load(Ordering::Relaxed), 3, "every other answer is still kept");
    }

    /// **The child asks its parent for the kept rows of the venues it reads** (they are not in the start message): one
    /// read answers those venues' rows and splits as the parent's store gives them, a repeat stays in the child, and
    /// an ask of every venue answers the whole lists.
    #[test]
    fn the_kept_rows_of_named_venues_cross_on_demand() {
        let contract = || vike_model::Contract::Fx(vike_model::ContractFx::default());
        let rows = vec![
            vike_model::Instrument::new("binance", "BTCUSDT", "BTC", "USDT", contract()),
            vike_model::Instrument::new("okx", "BTC-USDT", "BTC", "USDT", contract()),
            vike_model::Instrument::new("binance", "ETHUSDT", "ETH", "USDT", contract()),
        ];
        let store = HistoryStoreMemory::new();
        store.serve_kept_instruments(rows.clone());
        let binance = vec!["binance".to_string()];
        let expected = store.kept_instruments_and_splits_of(&binance);
        let (pipe, reads) = serve(store);

        let (got, splits) = pipe.kept_instruments_and_splits_of(&binance);
        assert_eq!(got.expect("rows"), expected.0.expect("rows"));
        assert_eq!(splits.expect("splits"), expected.1.expect("splits"));
        let again = pipe.kept_instruments_and_splits_of(&binance).0.expect("cached").expect("rows");
        let symbols: Vec<&str> = again.iter().map(|r| r.raw_symbol.as_str()).collect();
        assert_eq!(symbols, ["BTCUSDT", "ETHUSDT"]);
        assert_eq!(reads.load(Ordering::Relaxed), 1, "the repeat read must not cross the channel");
        assert_eq!(pipe.kept_instruments().expect("every venue's"), Some(rows));
    }

    /// More rows than one part carries cross bit for bit, the second read of the same series comes from
    /// the cache, and the parent store's error and an unserved verb keep their words.
    #[test]
    fn the_parent_stores_answers_cross_whole_and_a_repeat_read_stays_in_the_child() {
        let rows = bars(BARS_PER_FRAME * 2 + 17);
        let store = HistoryStoreMemory::new();
        store.append_bars("binance", "BTCUSDT", "1m", &rows, None).expect("seed");
        let expected =
            store.load_bars("binance", "BTCUSDT", "1m", TimestampRange::all()).expect("read");
        let (pipe, reads) = serve(store);

        let got =
            pipe.load_bars("binance", "BTCUSDT", "1m", TimestampRange::all()).expect("piped read");
        assert_eq!(got.len(), rows.len());
        assert_eq!(got.capacity(), got.len(), "sized once, from the count the parent sent first");
        assert!(expected.iter().any(|b| b.close.is_nan()), "the NaN row is in the store");
        assert!(
            got.iter().map(bits).eq(expected.iter().map(carried)),
            "every bar must cross bit for bit, NaN and infinity included"
        );
        let (mut part, mut crossed) = (Vec::new(), Vec::new());
        bar_part_into(&mut part, &rows[..20]).expect("a part");
        push_bar_part(&part[4..], &mut crossed).expect("a part");
        assert!(rows[10].bid.is_some_and(f64::is_nan), "the fixture's NaN bid");
        assert!(crossed[10].bid.is_none(), "a NaN optional crosses absent, the bar-columns rule");
        let again =
            pipe.load_bars("binance", "BTCUSDT", "1m", TimestampRange::all()).expect("cached");
        assert!(again.iter().map(bits).eq(got.iter().map(bits)));
        assert_eq!(again.capacity(), again.len(), "a repeat read replays the count");
        assert_eq!(reads.load(Ordering::Relaxed), 1, "the repeat read must not cross the channel");

        let empty =
            pipe.load_bars("binance", "ETHUSDT", "1m", TimestampRange::all()).expect("empty");
        assert!(empty.is_empty());

        match pipe.append_bars("binance", "BTCUSDT", "1m", &rows, None) {
            Err(HistoryStoreError::Unsupported(m)) => {
                assert!(m.contains("strategy sandbox"), "{m}")
            }
            other => panic!("a write must be refused inside the child: {other:?}"),
        }
    }

    /// A part the child cannot decode fails its read, and the rest of that answer is still read through
    /// `Done`: the next read gets its own answer (the failed one was not cached), not the last one's tail.
    /// The parts are what an OLDER parent wrote (msgpack `Rows`, where a bar part is a bar-columns body) and
    /// a bar-columns part cut short.
    #[test]
    fn a_part_the_child_cannot_decode_fails_its_read_and_keeps_the_channel_in_step() {
        let store = HistoryStoreMemory::new();
        store.append_bars("binance", "BTCUSDT", "1m", &bars(10), None).expect("seed");
        let (mut from_child, child_out) = std::io::pipe().expect("pipe up");
        let (child_in, mut to_child) = std::io::pipe().expect("pipe down");
        std::thread::spawn(move || {
            let mut first = true;
            while let Ok(ChildFrame::Read(read)) = read_message(&mut from_child) {
                if std::mem::take(&mut first) {
                    for garbage in ["not a bar part", "nor this"] {
                        write_message(&mut to_child, &ReadAnswerFrame::Rows(garbage))
                            .expect("part");
                    }
                    let mut part = Vec::new();
                    bar_part_into(&mut part, &bars(3)).expect("part");
                    let cut = &part[4..part.len() - 8];
                    to_child.write_all(&(cut.len() as u32).to_be_bytes()).expect("length");
                    to_child.write_all(cut).expect("cut part");
                    write_message(&mut to_child, &ReadAnswerFrame::<()>::Done).expect("done");
                } else {
                    relay::answer(&mut to_child, &store, read).expect("answer");
                }
            }
        });
        let pipe = PipeHistStore::new(Box::new(child_in), Box::new(child_out));
        match pipe.load_bars("binance", "BTCUSDT", "1m", TimestampRange::all()) {
            Err(HistoryStoreError::Io(m)) => assert!(m.contains("cannot decode"), "{m}"),
            other => panic!("a garbled part must fail its read: {other:?}"),
        }
        let again =
            pipe.load_bars("binance", "BTCUSDT", "1m", TimestampRange::all()).expect("next read");
        assert_eq!(again.len(), 10);
    }

    /// Every answer frame body written into `buffer`, in order.
    fn bodies(buffer: &[u8]) -> Vec<Vec<u8>> {
        let mut cursor = std::io::Cursor::new(buffer);
        std::iter::from_fn(|| vike_hub_protocol::frame::read_frame_raw(&mut cursor).ok()).collect()
    }

    /// **The msgpack columns the bar relay sent until 2026-11-18** (`wire.rs`'s `BarColumns` as it stood
    /// on `e8e9577402`, its docs left out), kept as the reference the bar-columns part is held to: the same bars
    /// bit for bit (`every_read_crosses_bit_for_bit_in_the_parts_of_the_old_paging`) and the cost it
    /// replaced, measured in the same process (`measure_the_bar_relay`).
    mod msgpack_columns {
        use std::borrow::Cow;
        use std::io::Write;

        use serde::{Deserialize, Serialize};
        use vike_data::HistoryStoreError;
        use vike_hub_protocol::child_pipe::write_message;
        use vike_marketdata::Bar;

        use super::super::super::wire::{BARS_PER_FRAME, ReadAnswerFrame};

        /// The old relay's `send_bars`: every part through ONE `BarColumns` and ONE frame, then `Done`.
        pub(super) fn send(out: &mut Vec<u8>, rows: Vec<Bar>) {
            let (mut columns, mut frame) = (BarColumns::default(), Vec::new());
            for part in rows.chunks(BARS_PER_FRAME) {
                columns.fill(part);
                frame.clear();
                frame.extend_from_slice(&[0; 4]);
                rmp_serde::encode::write_named(&mut frame, &ReadAnswerFrame::Rows(&columns))
                    .expect("msgpack");
                let len = u32::try_from(frame.len() - 4).expect("a part under the cap");
                frame[..4].copy_from_slice(&len.to_be_bytes());
                out.write_all(&frame).expect("write");
            }
            let _ = std::thread::Builder::new().spawn(move || drop(rows));
            write_message(out, &ReadAnswerFrame::<()>::Done).expect("done");
        }

        /// The old child's decode of one part's body, appended to `out`.
        pub(super) fn push(body: &[u8], out: &mut Vec<Bar>) {
            match rmp_serde::from_slice::<ReadAnswerFrame<BarColumns<'_>>>(body) {
                Ok(ReadAnswerFrame::Rows(part)) => part.push_into(out).expect("columns"),
                other => panic!("not a column part: {other:?}"),
            }
        }

        #[derive(Debug, Default)]
        struct Bin<'a>(Cow<'a, [u8]>);

        impl Bin<'_> {
            fn put(&mut self, bits: u64) {
                self.0.to_mut().extend_from_slice(&bits.to_le_bytes());
            }

            fn word(&self, i: usize) -> u64 {
                let mut word = [0u8; 8];
                word.copy_from_slice(&self.0[i * 8..i * 8 + 8]);
                u64::from_le_bytes(word)
            }
        }

        impl Serialize for Bin<'_> {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_bytes(&self.0)
            }
        }

        impl<'de: 'a, 'a> Deserialize<'de> for Bin<'a> {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                struct Bytes;
                impl<'de> serde::de::Visitor<'de> for Bytes {
                    type Value = Bin<'de>;
                    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                        f.write_str("a column of bytes")
                    }
                    fn visit_borrowed_bytes<E: serde::de::Error>(
                        self,
                        bytes: &'de [u8],
                    ) -> Result<Bin<'de>, E> {
                        Ok(Bin(Cow::Borrowed(bytes)))
                    }
                    fn visit_bytes<E: serde::de::Error>(self, bytes: &[u8]) -> Result<Bin<'de>, E> {
                        Ok(Bin(Cow::Owned(bytes.to_vec())))
                    }
                }
                d.deserialize_bytes(Bytes)
            }
        }

        #[derive(Debug, Default, Serialize, Deserialize)]
        pub(super) struct BarColumns<'a> {
            rows: usize,
            #[serde(borrow)]
            timestamp: Bin<'a>,
            #[serde(borrow)]
            open: Bin<'a>,
            #[serde(borrow)]
            high: Bin<'a>,
            #[serde(borrow)]
            low: Bin<'a>,
            #[serde(borrow)]
            close: Bin<'a>,
            #[serde(borrow)]
            volume: Bin<'a>,
            #[serde(borrow)]
            funding: Bin<'a>,
            #[serde(borrow)]
            bid: Bin<'a>,
            #[serde(borrow)]
            ask: Bin<'a>,
            #[serde(borrow)]
            present: Bin<'a>,
            symbol: Vec<(usize, Option<String>)>,
        }

        impl BarColumns<'_> {
            pub(super) fn fill(&mut self, rows: &[Bar]) {
                let c = self;
                c.rows = rows.len();
                let words = [
                    &mut c.timestamp,
                    &mut c.open,
                    &mut c.high,
                    &mut c.low,
                    &mut c.close,
                    &mut c.volume,
                    &mut c.funding,
                    &mut c.bid,
                    &mut c.ask,
                    &mut c.present,
                ];
                for column in words {
                    column.0.to_mut().clear();
                }
                c.symbol.clear();
                for b in rows {
                    c.timestamp.put(b.timestamp as u64);
                    c.open.put(b.open.to_bits());
                    c.high.put(b.high.to_bits());
                    c.low.put(b.low.to_bits());
                    c.close.put(b.close.to_bits());
                    c.volume.put(b.volume.to_bits());
                    c.funding.put(b.funding.map_or(0, f64::to_bits));
                    c.bid.put(b.bid.map_or(0, f64::to_bits));
                    c.ask.put(b.ask.map_or(0, f64::to_bits));
                    let flags = u8::from(b.funding.is_some())
                        | (u8::from(b.bid.is_some()) << 1)
                        | (u8::from(b.ask.is_some()) << 2);
                    c.present.0.to_mut().push(flags);
                    match c.symbol.last_mut() {
                        Some((count, symbol)) if symbol.as_deref() == b.symbol.as_deref() => {
                            *count += 1
                        }
                        _ => c.symbol.push((1, b.symbol.map(String::from))),
                    }
                }
            }

            pub(super) fn push_into(self, out: &mut Vec<Bar>) -> Result<(), HistoryStoreError> {
                let n = self.rows;
                let runs: usize = self.symbol.iter().map(|(count, _)| count).sum();
                let words = [
                    &self.timestamp,
                    &self.open,
                    &self.high,
                    &self.low,
                    &self.close,
                    &self.volume,
                    &self.funding,
                    &self.bid,
                    &self.ask,
                ];
                if words.iter().any(|c| c.0.len() != n * 8)
                    || self.present.0.len() != n
                    || runs != n
                {
                    return Err(HistoryStoreError::Io(
                        "the strategy sandbox read a bar part whose columns disagree".into(),
                    ));
                }
                let optional = |column: &Bin<'_>, i: usize, bit: u8| {
                    ((self.present.0[i] & bit) != 0).then(|| f64::from_bits(column.word(i)))
                };
                out.reserve(n);
                let mut symbols =
                    self.symbol.iter().flat_map(|(count, s)| std::iter::repeat_n(s, *count));
                for i in 0..n {
                    out.push(Bar {
                        timestamp: self.timestamp.word(i) as i64,
                        open: f64::from_bits(self.open.word(i)),
                        high: f64::from_bits(self.high.word(i)),
                        low: f64::from_bits(self.low.word(i)),
                        close: f64::from_bits(self.close.word(i)),
                        volume: f64::from_bits(self.volume.word(i)),
                        funding: optional(&self.funding, i, 1),
                        bid: optional(&self.bid, i, 2),
                        ask: optional(&self.ask, i, 4),
                        symbol: symbols.next().cloned().flatten().map(Into::into),
                    });
                }
                Ok(())
            }
        }
    }

    /// **The child sees the store's own answer, its paging included**, over a long series and every
    /// length around a part's edge: each range read through the pipe is the parent store's answer for
    /// that range bit for bit (a NaN price, infinity, a negative zero, absent optionals and the symbol runs;
    /// a NaN optional absent, [`carried`]), and the same bars the msgpack columns it replaced give, in parts of
    /// exactly [`BARS_PER_FRAME`] but the last (the old paging), then `Done`.
    #[test]
    fn every_read_crosses_bit_for_bit_in_the_parts_of_the_old_paging() {
        let rows = bars(BARS_PER_FRAME * 4 + 3);
        let store = HistoryStoreMemory::new();
        store.append_bars("binance", "BTCUSDT", "1m", &rows, None).expect("seed");
        let at = |i: usize| rows[i].timestamp;
        let last = rows.len() - 1;
        let edges = [
            (0, 0),
            (0, BARS_PER_FRAME - 2),
            (0, BARS_PER_FRAME - 1),
            (0, BARS_PER_FRAME),
            (1, BARS_PER_FRAME * 2),
            (BARS_PER_FRAME - 1, BARS_PER_FRAME * 3 + 1),
            (BARS_PER_FRAME * 3, last),
            (0, last),
        ];
        let reference = HistoryStoreMemory::new();
        reference.append_bars("binance", "BTCUSDT", "1m", &rows, None).expect("seed");
        let (pipe, _) = serve(store);
        for (from, to) in edges {
            let range = TimestampRange::of(at(from), at(to));
            let expected = reference.load_bars("binance", "BTCUSDT", "1m", range).expect("read");
            assert_eq!(expected.len(), to - from + 1);
            let got = pipe.load_bars("binance", "BTCUSDT", "1m", range).expect("piped");
            assert!(got.iter().map(bits).eq(expected.iter().map(carried)), "{from}..={to}");
            let mut old = Vec::new();
            msgpack_columns::send(&mut old, expected.clone());
            let mut old_frames = bodies(&old);
            old_frames.pop();
            let mut via_old = Vec::new();
            old_frames.iter().for_each(|body| msgpack_columns::push(body, &mut via_old));
            assert!(
                got.iter().map(bits).eq(via_old.iter().map(carried)),
                "the bar columns and the msgpack columns they replaced cross the same bars, {from}..={to}"
            );

            let mut sent = Vec::new();
            relay::send_columns(&mut sent, &BAR_COLUMNS, BARS_PER_FRAME, None, || {
                Ok(expected.clone())
            })
            .expect("encode");
            let frames = bodies(&sent);
            let (count, frames) = frames.split_first().expect("the count");
            assert_eq!(row_count(count), Some(expected.len()), "the count first, {from}..={to}");
            let (done, parts) = frames.split_last().expect("Done");
            let done = rmp_serde::from_slice::<ReadAnswerFrame<()>>(done);
            assert!(matches!(done, Ok(ReadAnswerFrame::Done)), "the answer ends with Done");
            let mut sizes = Vec::new();
            for part in parts {
                let mut decoded = Vec::new();
                push_bar_part(part, &mut decoded).expect("a bar part");
                sizes.push(decoded.len());
            }
            let paging: Vec<usize> = (0..expected.len())
                .step_by(BARS_PER_FRAME)
                .map(|at| (expected.len() - at).min(BARS_PER_FRAME))
                .collect();
            assert_eq!(sizes, paging, "{from}..={to}");
            assert_eq!(parts.len(), old_frames.len(), "the old paging, {from}..={to}");
        }
        assert!(rows.iter().any(|b| b.bid.is_some_and(f64::is_nan)), "a NaN optional is crossed");
        assert!(rows.iter().any(|b| b.ask.is_some_and(|a| a.is_sign_negative())), "so does -0.0");
    }

    /// **A malformed bar part is refused, never read short**: cut anywhere, with bytes after it, two bodies in
    /// one, a presence byte with a bit no field has, symbol runs that do not cover the rows, or the msgpack part
    /// an older parent wrote.
    #[test]
    fn a_malformed_bar_part_is_refused() {
        let decode = |body: &[u8]| {
            let mut out = Vec::new();
            push_bar_part(body, &mut out).map(|()| out.len())
        };
        let mut frame = Vec::new();
        bar_part_into(&mut frame, &bars(100)).expect("encode");
        let body = &frame[4..];
        assert_eq!(decode(body).expect("a whole part"), 100);

        let refused = |bytes: &[u8], what: &str| match decode(bytes) {
            Err(HistoryStoreError::Io(m)) => assert!(m.contains("cannot decode"), "{what}: {m}"),
            other => panic!("{what} must be refused: {other:?}"),
        };
        for cut in [1, 4, 8, 64, body.len() / 3, body.len() / 2, body.len() - 9, body.len() - 1] {
            refused(&body[..cut], &format!("a part cut at {cut} of {}", body.len()));
        }
        refused(&[body, &b"tail"[..]].concat(), "a part with bytes after it");
        refused(&[body, body].concat(), "two bodies in one part");
        // After the magic and the row count come nine columns of eight-byte words, the presence bytes, the run
        // count, then the first run's count.
        let presence = 8 + 9 * 8 * 100;
        let mut garbled = body.to_vec();
        garbled[presence] = 0b1000;
        refused(&garbled, "a presence bit no field has");
        let mut uncovered = body.to_vec();
        uncovered[presence + 100 + 4] ^= 1;
        refused(&uncovered, "symbol runs that do not cover the rows");
        let old = rmp_serde::to_vec_named(&ReadAnswerFrame::Rows("columns")).expect("msgpack");
        refused(&old, "an older parent's msgpack part");
    }

    /// **What handing 1.4 M one-minute bars to the child costs** (32 months, the reference run of decision
    /// 0281's amendment of 2026-11-18): the msgpack columns this replaced and the bar-columns body in the SAME
    /// process, taken in turn for seven rounds (a loaded box moves both alike) and printed as median / best:
    /// each codec's encode of every part (the parent) and decode of them (the child); then the relay as the
    /// child meets it through two pipes (the parent's store read and encode, the pipe, the child's decode) and
    /// a repeat read (the child's decode and drop, from the bodies it cached), and the parent's store read (paid
    /// in-process too). Run in release, alone.
    #[test]
    #[ignore = "a measurement: cargo test --release -p vike-backtesthub --lib measure_the_bar_relay -- --ignored --nocapture"]
    fn measure_the_bar_relay() {
        use std::time::{Duration, Instant};

        let time = |f: &mut dyn FnMut()| {
            let t = Instant::now();
            f();
            t.elapsed()
        };
        let spread = |mut times: Vec<Duration>| {
            times.sort();
            format!("{:?} / {:?}", times[times.len() / 2], times[0])
        };
        let mut rows = bars(1_400_000);
        for b in &mut rows {
            b.symbol = Some("BTCUSDT".into());
        }
        let seeded = || {
            let store = HistoryStoreMemory::new();
            store.append_bars("binance", "BTCUSDT", "1m", &rows, None).expect("seed");
            store
        };
        let store = seeded();
        let read =
            store.load_bars("binance", "BTCUSDT", "1m", TimestampRange::all()).expect("read");
        let old_send: fn(&mut Vec<u8>, Vec<Bar>) = msgpack_columns::send;
        let new_send: fn(&mut Vec<u8>, Vec<Bar>) = |out, rows| {
            relay::send_columns(out, &BAR_COLUMNS, BARS_PER_FRAME, None, || Ok(rows))
                .expect("encode")
        };
        // The relay owns the store's answer, so the copy handed to an encoder is made untimed.
        let encode = |send: fn(&mut Vec<u8>, Vec<Bar>), out: &mut Vec<u8>| {
            let mut rows = Some(read.clone());
            out.clear();
            time(&mut || send(out, rows.take().expect("once")))
        };
        let (mut old_out, mut new_out) = (Vec::new(), Vec::new());
        encode(old_send, &mut old_out);
        encode(new_send, &mut new_out);
        // Every part, the bar relay's count frame and `Done` left out.
        let parts = |out: &[u8]| {
            let mut parts = bodies(out);
            parts.pop();
            parts.retain(|body| row_count(body).is_none());
            parts
        };
        let (old_parts, new_parts) = (parts(&old_out), parts(&new_out));
        let old_decode = |rows: &mut Vec<Bar>| {
            old_parts.iter().for_each(|body| msgpack_columns::push(body, rows));
        };
        let new_decode = |rows: &mut Vec<Bar>| {
            new_parts.iter().for_each(|body| push_bar_part(body, rows).expect("a bar part"));
        };
        let decode = |each: &dyn Fn(&mut Vec<Bar>), rows: &mut Vec<Bar>| {
            rows.clear();
            time(&mut || each(rows))
        };
        let mut taken: [Vec<Duration>; 5] = Default::default();
        let mut decoded = Vec::new();
        for _ in 0..7 {
            taken[0].push(encode(old_send, &mut old_out));
            taken[1].push(encode(new_send, &mut new_out));
            taken[2].push(decode(&old_decode, &mut decoded));
            assert!(
                decoded.iter().map(bits).eq(read.iter().map(bits)),
                "the old part crosses whole"
            );
            taken[3].push(decode(&new_decode, &mut decoded));
            assert!(decoded.iter().map(bits).eq(read.iter().map(carried)), "the bar columns too");
            taken[4].push(time(&mut || {
                drop(store.load_bars("binance", "BTCUSDT", "1m", TimestampRange::all()))
            }));
        }
        // A first read crosses the pipe once per child (a repeat comes from its cache): a fresh child each.
        let relayed: Vec<Duration> = (0..3)
            .map(|_| {
                let (pipe, _) = serve(seeded());
                let mut piped = Vec::new();
                let t = time(&mut || {
                    piped = pipe
                        .load_bars("binance", "BTCUSDT", "1m", TimestampRange::all())
                        .expect("piped");
                });
                assert!(
                    piped.iter().map(bits).eq(read.iter().map(carried)),
                    "the relay crosses whole"
                );
                t
            })
            .collect();
        let (pipe, _) = serve(seeded());
        pipe.load_bars("binance", "BTCUSDT", "1m", TimestampRange::all()).expect("first read");
        let repeat: Vec<Duration> = (0..3)
            .map(|_| {
                time(&mut || {
                    let again = pipe
                        .load_bars("binance", "BTCUSDT", "1m", TimestampRange::all())
                        .expect("cached");
                    assert_eq!(again.len(), read.len());
                })
            })
            .collect();
        let [old_enc, new_enc, old_dec, new_dec, store_read] = taken.map(spread);
        println!(
            "relay of {} bars, median / best:\n  msgpack columns: {} bytes in {} parts, encode \
             {old_enc}, decode {old_dec}\n  bar columns:     {} bytes in {} parts, encode \
             {new_enc}, decode {new_dec}\n  the relay through the pipes, store read included: {}; a \
             repeat read (the child's decode and drop): {}; the store read itself: {store_read}",
            read.len(),
            old_out.len(),
            old_parts.len(),
            new_out.len(),
            new_parts.len(),
            spread(relayed),
            spread(repeat),
        );
    }

    /// A summarized scan's child writes each point on its own (its trades in parts of at most
    /// `ROWS_PER_FRAME`, then the point) and finishes with the scan's answer: no frame carries a second
    /// point, so the point count sets no frame size.
    #[test]
    fn a_summarized_scan_crosses_one_point_and_one_trades_part_per_frame() {
        use super::super::wire::ROWS_PER_FRAME;
        use crate::run_summary::Summary;
        use vike_backtesthub_client::proto::Response;
        use vike_backtesthub_client::summary::{ParamscanSummaryWire, RunSummaryWire};
        let trade = |i: usize| vike_model::Trade {
            entry_price: 100.0,
            exit_price: 101.0,
            size: 1.0,
            pnl: 1.0,
            fees: 0.1,
            entry_timestamp: i as i64,
            exit_timestamp: i as i64 + 1,
            symbol: "BTCUSDT".to_string(),
            mae: 0.0,
            mfe: 0.0,
            is_long: true,
        };
        let point = |i: usize| RunSummaryWire { point: Some(i), ..RunSummaryWire::default() };
        let held: Vec<(RunSummaryWire, Vec<vike_model::Trade>)> = vec![
            (point(0), (0..ROWS_PER_FRAME * 2 + 5).map(trade).collect()),
            (point(1), Vec::new()),
            (point(2), (0..3).map(trade).collect()),
        ];
        let scan = ParamscanSummaryWire {
            run: String::new(),
            points: Vec::new(),
            best: None,
            dsr: None,
            pbo: None,
            best_index: 0,
            cost_model: None,
            answered_from_saved_run: String::new(),
        };
        let (mut from_child, child_out) = std::io::pipe().expect("pipe up");
        let (child_in, _to_child) = std::io::pipe().expect("pipe down");
        let pipe = PipeHistStore::new(Box::new(child_in), Box::new(child_out));
        let reader = std::thread::spawn(move || {
            let mut frames = Vec::new();
            loop {
                match read_message::<_, ChildFrame>(&mut from_child).expect("a frame") {
                    ChildFrame::HeldTrades(part) => frames.push(Ok(part.len())),
                    ChildFrame::HeldPoint(point) => frames.push(Err(point.point)),
                    ChildFrame::Answer(_) => return frames,
                    ChildFrame::Read(_) => panic!("a summary reads nothing"),
                    ChildFrame::StudioRun(_) => panic!("a scan leaves no saved run"),
                    ChildFrame::HeldCurve(_) | ChildFrame::HeldLog(_) => {
                        panic!("a scan holds no whole curve")
                    }
                    ChildFrame::ParamscanRow(..) | ChildFrame::ParamscanTail(_) => {
                        panic!("a Studio scan is no profile search")
                    }
                }
            }
        });
        let answer = pipe.send_summary(Summary::Scan(Box::new(scan), held)).expect("streamed");
        assert!(matches!(answer, Response::ParamscanSummary(_)));
        pipe.send_answer(answer).expect("answered");
        let frames = reader.join().expect("read every frame");
        assert_eq!(
            frames,
            vec![
                Ok(ROWS_PER_FRAME),
                Ok(ROWS_PER_FRAME),
                Ok(5),
                Err(Some(0)),
                Err(Some(1)),
                Ok(3),
                Err(Some(2)),
            ]
        );
    }

    /// A summarized SINGLE run's child writes its trades, then its whole equity curve in parts of at most
    /// `ROWS_PER_FRAME` steps (decision 0078's amendment of 2026-11-18), and finishes with the summary: the parts,
    /// joined, are the curve to the bit, so a curve of any length crosses the pipe.
    #[test]
    fn a_summarized_single_run_crosses_its_whole_curve_in_parts() {
        use super::super::wire::ROWS_PER_FRAME;
        use crate::run_summary::Summary;
        use vike_backtesthub_client::proto::Response;
        let steps = ROWS_PER_FRAME * 2 + 3;
        let curve = vike_runs::RunEquityCurve {
            equity: (0..steps).map(|i| 1000.0 + (i as f64).sqrt() / 7.0).collect(),
            timestamp: (0..steps as i64).map(|i| i * 60_000).collect(),
        };
        let (mut from_child, child_out) = std::io::pipe().expect("pipe up");
        let (child_in, _to_child) = std::io::pipe().expect("pipe down");
        let pipe = PipeHistStore::new(Box::new(child_in), Box::new(child_out));
        let reader = std::thread::spawn(move || {
            let mut parts = Vec::new();
            loop {
                match read_message::<_, ChildFrame>(&mut from_child).expect("a frame") {
                    ChildFrame::HeldCurve(part) => parts.push(part),
                    ChildFrame::Answer(_) => return parts,
                    ChildFrame::HeldTrades(trades) => assert!(trades.is_empty(), "no trades"),
                    other => panic!("a single run's summary streams its curve: {other:?}"),
                }
            }
        });
        let single = Summary::Run(Box::default(), Vec::new(), curve.clone(), None);
        let answer = pipe.send_summary(single).expect("streamed");
        assert!(matches!(answer, Response::RunSummary(_)));
        pipe.send_answer(answer).expect("answered");
        let parts = reader.join().expect("read every frame");
        let sizes: Vec<usize> = parts.iter().map(|p| p.equity.len()).collect();
        assert_eq!(sizes, vec![ROWS_PER_FRAME, ROWS_PER_FRAME, 3]);
        let equity: Vec<u64> = parts.iter().flat_map(|p| &p.equity).map(|v| v.to_bits()).collect();
        let timestamp: Vec<i64> = parts.iter().flat_map(|p| p.timestamp.clone()).collect();
        assert_eq!(equity, curve.equity.iter().map(|v| v.to_bits()).collect::<Vec<_>>());
        assert_eq!(timestamp, curve.timestamp);
    }

    /// The saved run a Studio single run leaves crosses the child's pipe whole, in its own frame ahead of the answer.
    #[test]
    fn a_studio_run_crosses_whole_ahead_of_the_answer() {
        use crate::backtesthub_server::saved_run::StudioRun;
        use vike_backtesthub_client::proto::Response;
        let (mut from_child, child_out) = std::io::pipe().expect("pipe up");
        let (child_in, _to_child) = std::io::pipe().expect("pipe down");
        let pipe = PipeHistStore::new(Box::new(child_in), Box::new(child_out));
        let table = "{\"rows\":[],\"strategy_file\":\"trend/momentum.rs\"}";
        let run = StudioRun {
            profile_toml: "[strategy]\nfile = \"trend/momentum.rs\"\n".to_string(),
            answer: Response::ParamscanReport(table.to_string()),
            source: Some("fn on_bar() {}".to_string()),
            equity_log: None,
        };
        let reader = std::thread::spawn(move || {
            let first = read_message::<_, ChildFrame>(&mut from_child).expect("a frame");
            let last = read_message::<_, ChildFrame>(&mut from_child).expect("a frame");
            (first, last)
        });
        pipe.send_studio_run(run).expect("sent");
        pipe.send_answer(Response::Pong).expect("answered");
        match reader.join().expect("read both frames") {
            (ChildFrame::StudioRun(run), ChildFrame::Answer(answer)) => {
                assert_eq!(run.profile_toml, "[strategy]\nfile = \"trend/momentum.rs\"\n");
                assert!(matches!(run.answer, Response::ParamscanReport(ref json) if json == table));
                assert_eq!(run.source.as_deref(), Some("fn on_bar() {}"));
                assert!(matches!(*answer, Response::Pong));
            }
            other => panic!("the saved run, then the answer: {other:?}"),
        }
    }

    /// `msg` written positional (or named), read back as `T`: its `Debug` text (a NaN prints as `NaN` either
    /// way), or the read's refusal, and its bytes.
    fn crossed<M, T>(msg: &M, positional: bool) -> (Result<String, String>, usize)
    where
        M: serde::Serialize,
        T: serde::de::DeserializeOwned + std::fmt::Debug,
    {
        let mut bytes = Vec::new();
        let written = if positional {
            write_message(&mut bytes, msg)
        } else {
            vike_hub_protocol::child_pipe::write_message_named(&mut bytes, msg)
        };
        written.expect("a frame");
        let read = read_message::<_, T>(&mut bytes.as_slice());
        (read.map(|t| format!("{t:?}")).map_err(|e| e.to_string()), bytes.len())
    }

    /// A frame the pipe writes POSITIONAL: read back positional and named it is the message, field for
    /// field, and positional is never the larger. Its two sizes.
    fn positional<M>(msg: &M) -> (usize, usize)
    where
        M: serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug,
    {
        let want = format!("{msg:?}");
        let ((by_position, small), (by_name, large)) =
            (crossed::<M, M>(msg, true), crossed::<M, M>(msg, false));
        assert_eq!(by_position.as_deref(), Ok(want.as_str()), "positional");
        assert_eq!(by_name.as_deref(), Ok(want.as_str()), "named");
        assert!(small <= large, "positional is never the larger: {small} > {large}");
        (small, large)
    }

    /// A payload the pipe writes NAMED: positional does not bring it back (its type leaves a field out
    /// before its last one), named does.
    fn needs_names<M>(msg: &M)
    where
        M: serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug,
    {
        let want = format!("{msg:?}");
        assert_ne!(crossed::<M, M>(msg, true).0.as_deref(), Ok(want.as_str()), "{want}");
        assert_eq!(crossed::<M, M>(msg, false).0.as_deref(), Ok(want.as_str()), "named");
    }

    fn json<T: serde::de::DeserializeOwned>(text: &str) -> T {
        serde_json::from_str(text).expect("a sample")
    }

    /// A pinned run's event log as its child sends it in its curve's place: a stamp past `i64`'s range, a lane and a
    /// split, so every field of the recipe crosses.
    fn held_log() -> crate::run_summary::HeldLog {
        use vike_backtest::run::equity_log::{EquityLog, EquityLogRecipe, EquityLogSeries};
        let series = EquityLogSeries {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            source: Some("user-x".into()),
            interval: "1m".into(),
            from: Some(0),
            to: None,
            splits: vec![
                vike_model::StockSplit::new("binance", "BTCUSDT", "2020-08-31", 4.0)
                    .expect("split"),
            ],
            first_bar: 0,
            last_bar: 120_000,
            closes_fnv: "00000000000000ff".into(),
            days: Some(vec![vike_data::BarDay { day: 0, stamp: u64::MAX, provisional: false }]),
        };
        let recipe = EquityLogRecipe {
            format: 1,
            build: None,
            steps: 3,
            curve_fnv: "0123456789abcdef".into(),
            cash: 1000.0,
            currency: "USD".into(),
            decide: "sequential".into(),
            symbols: vec!["BTCUSDT".into()],
            values: vec![vike_model::ContractValue::linear(1.0)],
            series: vec![series],
        };
        let log = EquityLog { recipe, events: b"vikeelg1\0\0\0\0\0\0\0\0".to_vec() };
        crate::run_summary::HeldLog::new("[data]\nvenue = \"binance\"\n".into(), log)
    }

    /// **Every frame the pipe writes positional reads back as it reads named** (the pipe before 2026-11-19),
    /// field for field, NaN included, and in fewer bytes: every history read, every row type a read answers,
    /// the counts, the ends and the refusals, a run's trades and its curve. **Every payload it writes NAMED
    /// shows why**: positional, a type that leaves a field out before its last one reads into the wrong slots
    /// (`super::super::wire`'s module doc).
    #[test]
    fn every_frame_crosses_positional_as_it_crosses_named() {
        use std::collections::BTreeMap;

        use vike_backtesthub_client::proto::{Request, Response};
        use vike_backtesthub_client::summary::{CurvePageWire, RunSummaryWire};
        use vike_data::{ChainRow, CohortRow, PerpMetricRow, SeriesCoverage, SeriesId};
        use vike_marketdata::{L2BookUpdate, TickQuote, TickTrade};

        use super::super::wire::{ChildRequest, KeptAnswer, StoreRead};

        let range = TimestampRange { start: Some(1_700_000_000_000), end: None };
        let (venue, symbol) = (|| "binance".to_string(), || "BTCUSDT".to_string());
        let id: SeriesId = json(
            r#"{"kind":"bar","venue":"binance","symbol":"BTCUSDT","interval":"1m","group":null,"source":"klines"}"#,
        );
        let reads = [
            StoreRead::LoadBars { venue: venue(), symbol: symbol(), interval: "1m".into(), range },
            StoreRead::LoadBarsSourcedCapped {
                venue: venue(),
                source: "klines".into(),
                symbol: symbol(),
                interval: "1m".into(),
                range,
                budget: None,
            },
            StoreRead::ScanQuotes { venue: venue(), symbol: symbol(), range },
            StoreRead::ScanTrades { venue: venue(), symbol: symbol(), range },
            StoreRead::ScanBookUpdates { venue: venue(), symbol: symbol(), range },
            StoreRead::ScanDepth { venue: venue(), symbol: symbol(), range },
            StoreRead::ScanCohort { venue: venue(), asset: "BTC".into(), range },
            StoreRead::ScanPerpMetrics { venue: venue(), symbol: symbol(), range },
            StoreRead::PropertiesAsOf { venue: venue(), symbol: symbol(), timestamp: 7 },
            StoreRead::ListSeries,
            StoreRead::Inventory,
            StoreRead::SeriesGaps(id.clone()),
            StoreRead::SeriesFacts(id.clone()),
            StoreRead::ScanChain { venue: "deribit".into(), underlying: "BTC".into(), range },
            StoreRead::KeptInstruments { venues: Some(vec![venue()]) },
            StoreRead::KeptInstruments { venues: None },
            StoreRead::BarDays {
                venue: venue(),
                source: None,
                symbol: symbol(),
                interval: "1m".into(),
                range,
            },
        ];
        let mut sizes: Vec<(usize, usize)> =
            reads.into_iter().map(|read| positional(&ChildFrame::Read(read))).collect();

        let quote: TickQuote =
            json(r#"{"timestamp":1,"bid":100.5,"ask":100.75,"bid_size":2.0,"symbol":"BTCUSDT"}"#);
        let quotes =
            vec![quote.clone(), TickQuote { bid: f64::NAN, symbol: Default::default(), ..quote }];
        sizes.push(positional(&ReadAnswerFrame::Rows(quotes)));
        let trade: TickTrade =
            json(r#"{"timestamp":2,"price":100.5,"size":0.25,"is_buyer_maker":true}"#);
        sizes.push(positional(&ReadAnswerFrame::Rows(vec![trade.clone(), trade])));
        let book: L2BookUpdate = json(
            r#"{"timestamp":3,"sequence":9,"kind":"Snapshot","tick_size":0.5,"bids":[[100.0,1.5]],"asks":[[100.5,2.0]]}"#,
        );
        sizes.push(positional(&ReadAnswerFrame::Rows(vec![book])));
        let cohort: CohortRow = json(
            r#"{"ts":4,"asset":"BTC","axis":"size","cohort":"4xWhale","grading":"g","label_basis":"b","long_usd":1.5,"total_usd":3.0}"#,
        );
        sizes.push(positional(&ReadAnswerFrame::Rows(vec![cohort])));
        let perp: PerpMetricRow = json(r#"{"ts":5,"premium":-0.0001,"open_interest":null}"#);
        sizes.push(positional(&ReadAnswerFrame::Rows(vec![perp])));
        let chain: ChainRow = json(
            r#"{"ts":6,"underlying":"BTC","instrument":"BTC-27JUN26-100000-C","expiry_ms":7,"strike":100000.0,
               "kind":"call","bid":null,"ask":12.5,"mark":12.0,"iv":0.5,"open_interest":null,"volume":null,
               "delta":0.5,"gamma":null,"theta":null,"vega":null}"#,
        );
        sizes.push(positional(&ReadAnswerFrame::Rows(vec![chain])));
        sizes.push(positional(&ReadAnswerFrame::Rows(vec![id.clone()])));
        let coverage = SeriesCoverage {
            first_timestamp: 1,
            last_timestamp: 2,
            rows: 3,
            bytes: 4,
            parts: 5,
            dates: 6,
        };
        sizes.push(positional(&ReadAnswerFrame::Rows(vec![(id, coverage)])));
        sizes.push(positional(&ReadAnswerFrame::Rows(vec![(1_i64, 2_i64)])));
        sizes.push(positional(&ReadAnswerFrame::<()>::Count(263_521)));
        sizes.push(positional(&ReadAnswerFrame::<()>::Done));
        for refusal in [
            HistoryStoreError::Query("q".into()),
            HistoryStoreError::Io("i".into()),
            HistoryStoreError::Unsupported("u".into()),
        ] {
            sizes.push(positional(&ReadAnswerFrame::<()>::Failed(refusal)));
        }
        let held = |i: i64| vike_model::Trade {
            entry_price: 100.0,
            exit_price: f64::NAN,
            size: 1.0,
            pnl: -0.5,
            fees: 0.0,
            entry_timestamp: i,
            exit_timestamp: i + 1,
            symbol: String::new(),
            mae: 0.0,
            mfe: 0.25,
            is_long: false,
        };
        sizes.push(positional(&ChildFrame::HeldTrades(vec![held(1), held(2)])));
        let page =
            CurvePageWire::of(&[1000.0, f64::NAN, 1001.5], &[60_000, 120_000, 180_000], 0, 0);
        sizes.push(positional(&ChildFrame::HeldCurve(page)));
        sizes.push(positional(&ChildFrame::HeldLog(Box::new(held_log()))));
        let (small, large) = sizes.iter().fold((0, 0), |(s, l), (a, b)| (s + a, l + b));
        assert!(small < large, "{small} B positional, {large} B named");

        needs_names(&ChildFrame::HeldPoint(Box::default()));
        needs_names(&ChildFrame::Answer(Box::new(Response::RunSummary(
            Box::<RunSummaryWire>::default(),
        ))));
        let properties = vike_model::SymbolProperties { taker_hold_ms: 250, ..Default::default() };
        needs_names(&ReadAnswerFrame::Rows(Some(properties)));
        let mut row = vike_model::Instrument::new(
            "ig",
            "CS.D.EURUSD.MINI.IP",
            "EUR",
            "USD",
            vike_model::Contract::CryptoSpot,
        );
        row.venue_figures.insert("lotSize".into(), 1.0);
        let kept: KeptAnswer = (Ok(Some(vec![row])), Ok(None));
        needs_names(&ReadAnswerFrame::Rows(kept));
        let read_trades =
            Request::ReadTrades { run: "r".into(), point: None, page: 0, page_size: 50 };
        let first = ChildRequest {
            request: read_trades,
            paramscan_threads: 2,
            mounts: vec![(String::new(), BTreeMap::<String, String>::new())],
        };
        needs_names(&first);
    }

    /// **A read's range and a store's refusal cross in the bytes they always had**, positional and named, pinned
    /// byte for byte (each frame's body, after its length): a range is its two optional bounds, `[start, end]` or
    /// `{start, end}`, and a refusal its variant's name over its sentence. Pinned while the sandbox carried its own
    /// copies of `vike_data::TimestampRange` and `vike_data::HistoryStoreError` (B5), and held unchanged when the
    /// types themselves replaced them.
    #[test]
    fn a_range_and_a_refusal_cross_in_their_pinned_bytes() {
        use super::super::wire::StoreRead;

        fn body<M: serde::Serialize>(msg: &M, named: bool) -> Vec<u8> {
            let mut frame = Vec::new();
            let written = if named {
                vike_hub_protocol::child_pipe::write_message_named(&mut frame, msg)
            } else {
                write_message(&mut frame, msg)
            };
            written.expect("a frame");
            frame.split_off(4)
        }
        let read = ChildFrame::Read(StoreRead::ScanTrades {
            venue: "v".into(),
            symbol: "s".into(),
            range: TimestampRange { start: Some(1), end: None },
        });
        assert_eq!(body(&read, false), b"\x81\xa4Read\x81\xaaScanTrades\x93\xa1v\xa1s\x92\x01\xc0");
        assert_eq!(
            body(&read, true),
            b"\x81\xa4Read\x81\xaaScanTrades\x83\xa5venue\xa1v\xa6symbol\xa1s\xa5range\x82\xa5start\x01\xa3end\xc0"
        );
        let refusal = ReadAnswerFrame::<()>::Failed(HistoryStoreError::Io("i".into()));
        for named in [false, true] {
            assert_eq!(body(&refusal, named), b"\x81\xa6Failed\x81\xa2Io\xa1i", "named: {named}");
        }
    }

    /// **The bulk frames positional against named**: one part of quote rows (a tick read's answer) and one of
    /// a run's trades (a summarized run's `HeldTrades`), each `ROWS_PER_FRAME` rows: bytes, and the median
    /// encode and decode of each form. Run in release, alone.
    #[test]
    #[ignore = "a measurement: cargo test --release -p vike-backtesthub --lib measure_the_positional_frames -- --ignored --nocapture"]
    fn measure_the_positional_frames() {
        use std::time::{Duration, Instant};

        use vike_marketdata::TickQuote;

        use super::super::wire::ROWS_PER_FRAME;

        fn measure<M>(what: &str, msg: &M)
        where
            M: serde::Serialize + serde::de::DeserializeOwned,
        {
            let median = |mut runs: Vec<Duration>| {
                runs.sort();
                runs[runs.len() / 2]
            };
            for positional in [false, true] {
                let (mut encode, mut decode, mut bytes) = (Vec::new(), Vec::new(), Vec::new());
                for _ in 0..9 {
                    bytes.clear();
                    let started = Instant::now();
                    if positional {
                        write_message(&mut bytes, msg).expect("a frame");
                    } else {
                        vike_hub_protocol::child_pipe::write_message_named(&mut bytes, msg)
                            .expect("a frame");
                    }
                    encode.push(started.elapsed());
                    let started = Instant::now();
                    std::hint::black_box(
                        read_message::<_, M>(&mut bytes.as_slice()).expect("reads"),
                    );
                    decode.push(started.elapsed());
                }
                println!(
                    "{what}, {}: {} B, encode {:?}, decode {:?}",
                    if positional { "positional" } else { "named" },
                    bytes.len(),
                    median(encode),
                    median(decode),
                );
            }
        }
        let quotes: Vec<TickQuote> = (0..ROWS_PER_FRAME as i64)
            .map(|i| TickQuote {
                timestamp: 1_763_000_000_000 + i * 100,
                local_timestamp: 1_763_000_000_000 + i * 100 + 3,
                bid: 90_000.0 + (i % 977) as f64 / 10.0,
                ask: 90_000.1 + (i % 977) as f64 / 10.0,
                bid_size: 1.0 + (i % 7) as f64,
                ask_size: 2.0 + (i % 5) as f64,
                symbol: "BTCUSDT".into(),
            })
            .collect();
        measure("quote rows", &ReadAnswerFrame::Rows(quotes));
        let trades: Vec<vike_model::Trade> = (0..ROWS_PER_FRAME as i64)
            .map(|i| vike_model::Trade {
                entry_price: 90_000.0 + (i % 977) as f64,
                exit_price: 90_010.0 + (i % 911) as f64,
                size: 0.01,
                pnl: (i % 13) as f64 - 6.0,
                fees: 0.036,
                entry_timestamp: 1_763_000_000_000 + i * 60_000,
                exit_timestamp: 1_763_000_000_000 + i * 60_000 + 300_000,
                symbol: "BTCUSDT".into(),
                mae: 0.001,
                mfe: 0.002,
                is_long: i % 2 == 0,
            })
            .collect();
        measure("held trades", &ChildFrame::HeldTrades(trades));
    }

    /// **Another build is refused in words at both ends**: the hub's relay stops a child that states another
    /// build (naming both, the child killed), and a child whose parent states another build, or opens with no
    /// build at all (a hub from before the check), answers nothing but its own build and exits 3.
    #[test]
    fn another_build_is_refused_in_words_at_both_ends() {
        use std::collections::BTreeMap;
        use std::io::Read;

        use vike_backtesthub_client::proto::Request;
        use vike_hub_protocol::child_pipe::send_build;

        let other = "git 0123abcd clean, built 2026-01-01T00:00:00Z, rustc 1.0.0, nowhere";
        let ours = "git 4567cdef clean, built 2026-11-19T00:00:00Z, rustc 1.0.0, here";
        let (mut from_child, mut child_out) = std::io::pipe().expect("pipe up");
        let (mut child_in, mut to_child) = std::io::pipe().expect("pipe down");
        let child = std::thread::spawn(move || {
            send_build(&mut child_out, other).expect("its build");
            std::io::copy(&mut child_in, &mut std::io::sink()).expect("drained")
        });
        let store = HistoryStoreMemory::new();
        let stopped = relay::relay(
            &mut to_child,
            &mut from_child,
            ours,
            &Request::Ping,
            "",
            &BTreeMap::new(),
            &store,
            &|| {},
            None,
        )
        .map(|_| ())
        .expect_err("another build is stopped");
        assert!(stopped.why.contains(other) && stopped.why.contains(ours), "{}", stopped.why);
        drop(to_child);
        child.join().expect("the child half");

        for first in [Some(other), None] {
            let mut input = Vec::new();
            match first {
                Some(build) => send_build(&mut input, build).expect("a build"),
                None => {
                    let no_models = BTreeMap::<String, String>::new();
                    let request = super::super::wire::ChildRequest {
                        request: &Request::Ping,
                        paramscan_threads: 1,
                        mounts: vec![("", &no_models)],
                    };
                    vike_hub_protocol::child_pipe::write_message_named(&mut input, &request)
                        .expect("an older hub's request")
                }
            }
            let (mut said, output) = std::io::pipe().expect("pipe");
            let code = super::super::serve_child(
                Box::new(std::io::Cursor::new(input)),
                Box::new(output),
                ours,
                vec![(Err("not loaded".into()), None)],
            );
            assert_eq!(format!("{code:?}"), format!("{:?}", std::process::ExitCode::from(3)));
            let mut written = Vec::new();
            said.read_to_end(&mut written).expect("its output");
            let mut own = Vec::new();
            send_build(&mut own, ours).expect("a build");
            assert_eq!(written, own, "nothing but its own build");
        }
    }

    /// A store whose kept rows and splits of `binance` are `rows` and `splits`, held ENCODED (as the datahub handle's
    /// stamped cache holds them) when `encoded`, or failing with `fails`.
    struct KeptStore {
        rows: Vec<vike_model::Instrument>,
        splits: Vec<vike_model::StockSplit>,
        encoded: bool,
        fails: Option<HistoryStoreError>,
    }

    impl HistoryStore for KeptStore {
        fn kept_instruments_and_splits_of(
            &self,
            _venues: &[String],
        ) -> (
            Result<Option<Vec<vike_model::Instrument>>, HistoryStoreError>,
            Result<Option<Vec<vike_model::StockSplit>>, HistoryStoreError>,
        ) {
            match &self.fails {
                Some(e) => (Err(e.clone()), Err(e.clone())),
                None => (Ok(Some(self.rows.clone())), Ok(Some(self.splits.clone()))),
            }
        }
        fn kept_instruments_and_splits_encoded_of(
            &self,
            _venues: &[String],
        ) -> Option<Result<vike_data::EncodedKept, HistoryStoreError>> {
            use vike_datahub_client::proto::kept_msgpack;
            self.encoded.then(|| match &self.fails {
                Some(e) => Err(e.clone()),
                None => Ok(vike_data::EncodedKept {
                    rows: kept_msgpack(&self.rows).expect("msgpack"),
                    splits: kept_msgpack(&self.splits).expect("msgpack"),
                }),
            })
        }
        vike_data::hist_store_stubs!(inert: all);
    }

    /// **The child reads the very bytes it read before** (the owner's "yes both" of 2026-11-19): a kept-rows answer
    /// relayed from the store's ENCODED rows (`wire::write_kept_frame`, no row decoded or encoded in the parent) is
    /// byte for byte the frames the typed answer writes, rows with floats a text form would round and a split
    /// included, and a store's refusal crosses in the same words either way.
    ///
    /// MUTATION PROOF: write `0x93` for the tuple's header in `write_kept_frame`; the frames then differ.
    #[test]
    fn a_kept_answer_relayed_from_encoded_rows_is_the_typed_answers_bytes() {
        let mut row = vike_model::Instrument::new(
            "binance",
            "BTCUSDT",
            "BTC",
            "USDT",
            vike_model::Contract::CryptoSpot,
        );
        row.properties.tick_size = 0.1 + 0.2;
        row.venue_figures.insert("tiny".into(), 5e-324);
        let rows = vec![row.clone(), row];
        let splits = vec![
            vike_model::StockSplit::new("binance", "BTCUSDT", "2020-08-31", 4.0).expect("split"),
        ];
        let frames = |encoded: bool, fails: Option<HistoryStoreError>| {
            let store = KeptStore { rows: rows.clone(), splits: splits.clone(), encoded, fails };
            let mut out = Vec::new();
            let read = StoreRead::KeptInstruments { venues: Some(vec!["binance".to_string()]) };
            relay::answer(&mut out, &store, read).expect("answered");
            out
        };
        assert_eq!(frames(true, None), frames(false, None), "the rows' frames");
        let refused = HistoryStoreError::Query("the datahub refused".into());
        assert_eq!(frames(true, Some(refused.clone())), frames(false, Some(refused)), "a refusal");

        let (child_in, mut to_child) = std::io::pipe().expect("pipe down");
        to_child.write_all(&frames(true, None)).expect("the frames");
        let (_from_child, child_out) = std::io::pipe().expect("pipe up");
        let pipe = PipeHistStore::new(Box::new(child_in), Box::new(child_out));
        let (got, got_splits) = pipe.kept_instruments_and_splits_of(&["binance".to_string()]);
        assert_eq!(got.expect("rows"), Some(rows));
        assert_eq!(got_splits.expect("splits"), Some(splits));
    }
}
