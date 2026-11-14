//! **THE JOIN, proved through the production route, which is a sandboxed child now.** A request naming a
//! real `.rs` strategy FILE (a Studio verb's `StrategySpecWire::File`, a profile's `[strategy] file`) reaches a REAL
//! hub over its wire; the hub reads the file in its `strategies/` folder and routes the run to
//! `vike-backend strategy-sandbox backtest <builds> <sha256>` (this package's own binary); the child jails
//! itself, loads the artifact the builder made of that file's text, runs the request, and the answer trades.
//!
//! # Why this file exists rather than another hand-wired proof
//!
//! `docs/decisions/0082-the-plugin-mechanism-lands-without-the-feature.md` names the shape this test
//! refuses to repeat: a test that does the wiring BY HAND proves the two mechanisms agree while production
//! has no route to either. So nothing here loads a strategy file: the ONLY way the file reaches the engine
//! is the hub's own route (`vike_backtesthub::strategy_sandbox`) and the child's own load
//! (`crates/vike-strategy-sandbox/src/lib.rs`), and
//! [`this_file_reaches_the_strategy_file_only_through_the_production_route`] holds that. The artifact
//! half is the production builder (`vike_strategy_builder::render::build_strategy_file`).
//!
//! The by-hand in-process run a parity needs is `tests/strategy_sandbox_backtest.rs`'s, a separate file,
//! so this one keeps its property. It moved here from `vike-backtesthub` (plan 2b): the child is the
//! `vike-backend` binary, which only this package's tests can run.
//!
//! # ⚠ THE ONE STEP THIS FILE CANNOT EXERCISE, NAMED RATHER THAN LEFT TO BE FOUND
//!
//! **The CROSS-UNIT hand-off.** On a real box the builder is one unit writing its build folder under
//! `<root>/user_data/strategy-builds` and the backtesthub another reading it; here one process stages the
//! artifact with `std::fs::copy` where the hub's OWN resolver looks (`strategy_files_dir()`). The
//! writer's half is gated where the deploy files can be read:
//! `crates/vike-ops/tests/container_deploy/deploy_layout_gate/strategy_file_and_profile.rs`'s
//! `every_shipped_strategy_file_artifact_dir_is_one_spelling`.
//!
//! # One `#[test]` for the phases, on purpose
//!
//! The hub resolves its artifact directory from the PROCESS's working directory, which is process-global;
//! under the plain runner every `#[test]` of one binary shares a process. So the phases run in order in
//! one test.
//!
//! ```text
//! LANE=vike-fresh3 MSYS_NO_PATHCONV=1 just the CI box <branch> cargo test -p vike-backend --test strategy_file_run
//! ```
#![cfg(all(feature = "vike-strategy-sandbox", target_os = "linux"))]

#[path = "support/strategy_file.rs"]
mod strategy_file;

use std::time::{Duration, Instant};

use vike_backtesthub_client::proto::{Request, Response};
use vike_backtesthub_client::wire_studio::{ParamscanWire, StrategySpecWire};
use vike_hub_protocol::frame::write_frame;
use vike_hub_protocol::hash::sha256_hex;
use vike_model::params::ParamValue;

use strategy_file::{
    FILE, SOURCE, ask, build_the_artifact, enter_a_project_holding, sandbox_children, seeded_store,
    slice, spec, use_the_multicall_as_the_sandbox,
};

fn run_slice(spec: StrategySpecWire) -> Request {
    Request::RunSlice { spec, slice: slice(), params: None }
}

#[test]
fn a_strategy_file_request_runs_through_the_hub_and_its_sandboxed_child() {
    // This test plays the hub, so it runs the boot's step 0 first, as a root does: no descriptor it
    // inherited (CI's runner hands it a pipe) reaches the strategy process, which would refuse it.
    let _ = vike_boot::descriptors::mark_inherited_close_on_exec();
    // ---- phase 0: build the artifact with the REAL builder, stage file and artifact ----------------
    let artifact = build_the_artifact();
    let sha = sha256_hex(SOURCE.as_bytes());
    assert_eq!(
        artifact.file_name().and_then(|n| n.to_str()),
        Some(format!("{sha}.so").as_str()),
        "the builder's artifact is named by the text's sha256"
    );
    let (_project, strategies, restore) = enter_a_project_holding(&artifact);
    use_the_multicall_as_the_sandbox();
    let addr =
        vike_backtesthub::test_support::spawn_hub_over(seeded_store(400), strategies.clone());

    // ---- phase 1: THE JOIN — a full Run over the wire, exactly as a Studio Run reaches it ------------
    let dense = match ask(addr, &run_slice(spec(5, 0))) {
        Response::RunResult(r) => r,
        other => panic!("a strategy file with a built artifact must RUN: {other:?}"),
    };
    assert!(!dense.equity_curve.is_empty(), "the file ran over the slice");
    assert!(
        dense.n_trades > 0,
        "the file must TRADE — a loaded-but-inert file (a null handle from a params parse failure) \
         produces exactly this shape with no trade"
    );
    assert_eq!(dense.strategy_sha256, sha, "the result records the file's sha256");
    assert!(sandbox_children().is_empty(), "no child outlives the run it served");

    // ---- phase 2: the params table genuinely crossed the C-ABI, inside the child --------------------
    let sparse = match ask(addr, &run_slice(spec(40, 0))) {
        Response::RunResult(r) => r,
        other => panic!("the same file with other params must run: {other:?}"),
    };
    assert!(
        sparse.n_trades < dense.n_trades,
        "a larger `every` must trade less: {} at 40 vs {} at 5",
        sparse.n_trades,
        dense.n_trades
    );

    // ---- phase 3: the SWEEP door reaches the same artifact, every point in the one child ------------
    let sweep = Request::RunParamscan {
        spec: spec(40, 0),
        slice: slice(),
        paramscan: ParamscanWire {
            axes: vec![("every".to_string(), vec![ParamValue::Int(5), ParamValue::Int(40)])],
        },
        params: None,
    };
    let entries = match ask(addr, &sweep) {
        Response::ParamscanResult(r) => r.entries,
        other => panic!("the sweep door must run a strategy file: {other:?}"),
    };
    let trades_at = |every: i64| {
        entries
            .iter()
            .find(|e| e.overrides.iter().any(|(k, v)| k == "every" && *v == ParamValue::Int(every)))
            .map(|e| e.result.n_trades)
            .expect("the point")
    };
    assert!(
        trades_at(40) < trades_at(5),
        "a sweep point's override must reach the file's params table: every=5 traded {}, every=40 {}",
        trades_at(5),
        trades_at(40)
    );

    // ---- phase 4: a PROFILE verb routes the same way (`[strategy] file`) ------------------------------
    let profile = format!(
        "[data]\nvenue = \"binance\"\nsymbols = [\"BTCUSDT\"]\nkind = \"bar\"\ninterval = \"1m\"\n\
         from = \"0\"\nto = \"100000000\"\n\n[engine]\ncash = 100000.0\n\n[strategy]\nfile = \
         \"{FILE}\"\n[strategy.params]\nevery = 5\n"
    );
    match ask(addr, &Request::RunBacktest(profile)) {
        Response::Report(json) => {
            assert!(json.contains(&sha), "the report records the file's sha256: {json}")
        }
        other => panic!("a profile naming a .rs file must run in the child: {other:?}"),
    }

    // ---- phase 5: the refusals, in their own words ---------------------------------------------------
    // A file the builder never built has no artifact: the loader's refusal, from the child, names the build.
    let unbuilt = "other/unbuilt.rs";
    std::fs::write(
        strategies.join(unbuilt),
        format!("{SOURCE}\n// edited: a new text, no build\n"),
    )
    .expect("write an unbuilt file");
    let unbuilt_spec =
        StrategySpecWire::File { file: unbuilt.to_string(), params_toml: String::new() };
    match ask(addr, &run_slice(unbuilt_spec)) {
        Response::Error(m) => {
            assert!(m.starts_with("strategy: "), "a strategy error: {m}");
            assert!(m.contains("build"), "it tells the author to build it: {m}");
        }
        other => panic!("a file with no artifact behind it must not produce a run: {other:?}"),
    }
    // A key the file's header does not declare is the HUB's read refusing it, before any child.
    let undeclared =
        StrategySpecWire::File { file: FILE.to_string(), params_toml: "fast = 3\n".into() };
    match ask(addr, &run_slice(undeclared)) {
        Response::Error(m) => assert!(m.contains("fast"), "the refusal names the key: {m}"),
        other => panic!("an undeclared key must be refused: {other:?}"),
    }
    // A built artifact named by (name, sha) is refused, naming the file to run instead.
    let plugin = StrategySpecWire::Plugin {
        name: "join_probe".into(),
        sha: sha.clone(),
        params_toml: String::new(),
    };
    match ask(addr, &run_slice(plugin)) {
        Response::Error(m) => assert!(m.contains("[strategy] file"), "{m}"),
        other => panic!("an artifact by (name, sha) must be refused: {other:?}"),
    }

    // ---- phase 6: a client that leaves KILLS its child, mid-engine ---------------------------------
    // 400 bars at 50 ms each is 20 s of run; the client leaves after the child is seen running.
    let mut stream = std::net::TcpStream::connect(addr).expect("dial");
    write_frame(&mut stream, &run_slice(spec(5, 50))).expect("send the slow run");
    let seen = Instant::now();
    while sandbox_children().is_empty() {
        assert!(seen.elapsed() < Duration::from_secs(20), "the slow run's child never appeared");
        std::thread::sleep(Duration::from_millis(20));
    }
    drop(stream);
    let left = Instant::now();
    while !sandbox_children().is_empty() {
        assert!(
            left.elapsed() < Duration::from_secs(5),
            "the child of a gone client must be killed, not left to finish its 20 s run"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    println!("cancel: the child was gone {:?} after its client left", left.elapsed());

    // ---- phase 7: a file NEVER built runs on its FIRST request: the hub asks the builder and waits ----
    // The builder here is the production build function behind the hub's builder door (the deployed one
    // is the same function behind the builder service's socket, `vike_strategy_builder::client::build_file`).
    // Installed only now: phase 5 proved a hub WITHOUT a builder refuses an unbuilt file.
    let builds = vike_backtesthub::studio_run::strategy_files_dir();
    let asked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = std::sync::Arc::clone(&asked);
    vike_backtesthub::strategy_sandbox::use_builder(Box::new(move |path, text| {
        counted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let artifact = strategy_file::build_text(path, text)?;
        strategy_file::stage(&artifact, &builds);
        Ok(sha256_hex(text.as_bytes()))
    }));
    let fresh = "other/fresh.rs";
    let fresh_text = format!("{SOURCE}\n// fresh: a text no build has seen\n");
    std::fs::write(strategies.join(fresh), &fresh_text).expect("write a never-built file");
    let first =
        StrategySpecWire::File { file: fresh.to_string(), params_toml: "every = 5\n".into() };
    match ask(addr, &run_slice(first)) {
        Response::RunResult(r) => {
            assert!(r.n_trades > 0, "the never-built file ran on its first request");
            assert_eq!(r.strategy_sha256, sha256_hex(fresh_text.as_bytes()));
        }
        other => panic!("a never-built file must be built, then run: {other:?}"),
    }
    assert_eq!(
        asked.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "the hub asked the builder once"
    );
    // A file that does not compile: the builder's diagnostics are the run's answer, and no child starts.
    let broken = "other/broken.rs";
    std::fs::write(strategies.join(broken), format!("{SOURCE}\nfn broken( {{\n"))
        .expect("write a broken file");
    let spec_broken =
        StrategySpecWire::File { file: broken.to_string(), params_toml: String::new() };
    match ask(addr, &run_slice(spec_broken)) {
        Response::Error(m) => assert!(m.contains("could not be built"), "{m}"),
        other => panic!("a file that does not build must not run: {other:?}"),
    }
    assert!(sandbox_children().is_empty(), "no child started for the broken file");

    std::env::set_current_dir(restore).expect("restore the working directory");
}

/// **The structural property this file rests on, GATED rather than claimed**: nothing here (or in the
/// support module it shares) loads a strategy file or installs one by hand, so the only route to a
/// running file is the production one. It reads its own source and the support module's through
/// `include_str!`; comment lines are dropped, because the header NAMES what it forbids.
#[test]
fn this_file_reaches_the_strategy_file_only_through_the_production_route() {
    // Each needle is ASSEMBLED FROM HALVES: spelled whole, it would sit in this file's own code region.
    let banned: Vec<String> = [
        ("vike_rust", "_strategy"),
        ("StrategyFile", "Host"),
        ("Strategy", "VTableAbi"),
        ("loader", "::load"),
        ("loaded_file", "_builder"),
        ("answer_in_this", "_process"),
    ]
    .iter()
    .map(|(a, b)| format!("{a}{b}"))
    .collect();
    // Each file's floor witness is a call its code really makes, assembled from halves like the needles.
    for (file, src, witness) in [
        (
            "strategy_file_run.rs",
            include_str!("strategy_file_run.rs"),
            format!("{}{}", "spawn_hub", "_over("),
        ),
        (
            "support/strategy_file.rs",
            include_str!("support/strategy_file.rs"),
            format!("{}{}", "CargoHomePin", "::disabled()"),
        ),
    ] {
        let code: String = src
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        for needle in &banned {
            assert!(
                !code.contains(needle.as_str()),
                "`{file}` names `{needle}` in CODE: its proof is that the only route to a running \
                 strategy file is the hub's sandbox route (docs/decisions/0082)"
            );
        }
        // The floor: the scan sees code.
        assert!(code.contains(&witness), "`{file}`: the scan found no code");
    }
    let comment_only = format!("{}{}", "THE ONE STEP THIS FILE", " CANNOT EXERCISE");
    assert!(
        !include_str!("strategy_file_run.rs")
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .any(|l| l.contains(&comment_only)),
        "the comment filter kept the doc header"
    );
}
