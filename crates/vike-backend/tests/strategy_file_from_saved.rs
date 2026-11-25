//! **An IDENTICAL run is answered from the run the hub saved, and nothing else is** (decision 0078's amendment of
//! 2026-11-20, the owner's "do"), proved through the production route: a real hub over its wire, a real
//! `vike-backend strategy-sandbox` child, the fixture strategy FILE built by the production builder, a store that
//! stamps its days (`vike_backtesthub::test_support::stamping_restamped_by`, as the datahub's `SeriesDays` does). Each
//! hub is BUILT as the `--addr` daemon builds itself (`vike_backtesthub::test_support::spawn_serving_hub`: its
//! `serving_study`, its bare commit, the answer-from-saved identity hashed from the program its children run).
//!
//! - **The hit:** the same request twice; the second answer is the first's (a summary's every field but the names
//!   of the run it is held and saved as, a whole report byte for byte but its last key) and says which saved run it
//!   came from. Its trades and its whole curve page as the first's did. The wall of each is printed. And a USDT
//!   PERPETUAL whose window ended YESTERDAY, its store holding today's bars and today's USDT-USD rate: its pair is read
//!   no further than the run can see, so it is answered from saved too (2026-11-21).
//! - **One miss each:** a param changed; a day restamped (a re-imported day); a window reaching today (the spot pair's
//!   and the perpetual's); `Fresh` (which keys nothing); a new
//!   artifact under the same name; a changed kept instrument list; a summarized search whose recorded reads moved, and
//!   a search that is not the WHOLE identical request.
//! - **Retention:** a run the retention removed answers nothing again, and a hit from it still pages its whole curve
//!   (the hit holds it).
//! - **Old runs:** a run saved with no key (by a hub before this change) is never a hit.
//!
//! One `#[test]`: the hub resolves its artifact directory from the PROCESS's working directory.
//!
//! ```text
//! LANE=the build runner MSYS_NO_PATHCONV=1 just the CI box <branch> cargo test --release -p vike-backend --test strategy_file_from_saved -- --nocapture
//! ```
#![cfg(all(feature = "vike-strategy-sandbox", target_os = "linux"))]

#[allow(dead_code, clippy::allow_attributes)]
#[path = "support/strategy_file.rs"]
mod strategy_file;

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use vike_backtesthub::test_support::{
    DayStamps, spawn_hub_over_saving_to, spawn_serving_hub, stamping_restamped_by,
};
use vike_backtesthub_client::proto::{Request, Response};
use vike_backtesthub_client::summary::{ParamscanProfileSummaryWire, RunSummaryWire};
use vike_data::{HistoryStore, HistoryStoreMemory};
use vike_hub_protocol::hash::sha256_hex;

use strategy_file::{
    FILE, ask, build_the_artifact, enter_a_project_holding, use_the_multicall_as_the_sandbox,
};

/// One-minute bars from the first minute of 1970: about 35 days, every one of them long past.
const BARS: usize = 50_000;

/// A window over every bar, closed long before today.
const PAST: &str = "4000000000";

/// A window that reaches past today.
const OPEN: &str = "4102444800000";

/// The profile every single run here sends: the fixture file flipping every `every` bars, up to `to`.
fn profile(every: i64, to: &str) -> String {
    format!(
        "[data]\nvenue = \"binance\"\nsymbols = [\"BTCUSDT\"]\nkind = \"bar\"\ninterval = \"1m\"\n\
         from = \"0\"\nto = \"{to}\"\n\n[engine]\ncash = 100000.0\n\n[strategy]\nfile = \"{FILE}\"\n\
         [strategy.params]\nevery = {every}\n"
    )
}

/// The USDT-quoted perpetual a Studio run of the CI box trades.
const PERP: &str = "BTCUSDT.P";

const HOUR: i64 = 3_600_000;

/// The fixture file on [`PERP`]'s hourly bars over `[from, to]`, its funding attached (a perpetual's default), on the
/// dollar ledger: the run converts USDT at the store's own USDT-USD series.
fn perpetual(from: i64, to: i64) -> Request {
    summarized(format!(
        "[data]\nvenue = \"binance\"\nsymbols = [\"{PERP}\"]\nkind = \"bar\"\ninterval = \"1h\"\n\
         from = \"{from}\"\nto = \"{to}\"\n\n[engine]\ncash = 100000.0\n\n[strategy]\nfile = \"{FILE}\"\n\
         [strategy.params]\nevery = 5\n"
    ))
}

/// [`profile`] searched over `every` in `grid`.
fn search(grid: &str) -> Request {
    let profile_toml = format!("{}\n[paramscan]\nevery = [{grid}]\n", profile(5, PAST));
    Request::Summarized(Box::new(Request::RunParamscanProfile {
        profile_toml,
        rank_by: None,
        search: None,
    }))
}

fn summarized(profile_toml: String) -> Request {
    Request::Summarized(Box::new(Request::RunBacktest(profile_toml)))
}

/// `request`'s answer and how long it took.
fn timed(address: SocketAddr, request: &Request) -> (Response, Duration) {
    let started = Instant::now();
    let answer = ask(address, request);
    (answer, started.elapsed())
}

fn summary(answer: Response) -> RunSummaryWire {
    match answer {
        Response::RunSummary(summary) => *summary,
        other => panic!("a single run's summary: {other:?}"),
    }
}

fn search_summary(answer: Response) -> ParamscanProfileSummaryWire {
    match answer {
        Response::ParamscanProfileSummary(summary) => *summary,
        other => panic!("a search's summary: {other:?}"),
    }
}

/// A summary's digest without the names of the run it is held and saved as, and without its from-saved words: the
/// run's own numbers, rows, curve, trades count, refusals and results.
fn digest(summary: &RunSummaryWire) -> String {
    let bare = RunSummaryWire {
        run: String::new(),
        saved_run: String::new(),
        answered_from_saved_run: String::new(),
        ..summary.clone()
    };
    sha256_hex(serde_json::to_string(&bare).expect("encodes").as_bytes())
}

/// The ids of the finished runs under `runs`.
fn saved_runs(runs: &Path) -> Vec<String> {
    let mut ids: Vec<String> = std::fs::read_dir(runs)
        .map(|dir| {
            dir.flatten()
                .filter(|e| e.path().join("manifest.json").is_file())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    ids
}

#[test]
fn an_identical_run_is_answered_from_its_saved_run_and_nothing_else_is() {
    let _ = vike_boot::descriptors::mark_inherited_close_on_exec();
    let artifact = build_the_artifact();
    let (project, strategies, restore) = enter_a_project_holding(&artifact);
    use_the_multicall_as_the_sandbox();
    let memory = Arc::new(HistoryStoreMemory::new());
    let bars =
        vike_studio_core::test_support::walk::random_walk_bars(BARS, 60_000, Some("BTCUSDT"));
    memory.append_bars("binance", "BTCUSDT", "1m", &bars, None).expect("seed bars");
    // Named as the datahub's write stores a kept row (decision 0290): a spot row's user name is its symbol.
    let instrument = |symbol: &str, base: &str| vike_model::Instrument {
        naming: Some(vike_model::InstrumentNaming::User(symbol.to_string())),
        ..vike_model::Instrument::new(
            "binance",
            symbol,
            base,
            "USDT",
            vike_model::Contract::CryptoSpot,
        )
    };
    // A USDT perpetual's hourly bars over the last four days and TODAY (a collector keeps writing them), and the store's
    // daily USDT-USD rate up to today's day, whose bar is still changing: what a Studio run on the CI box reads.
    let day = vike_model::MS_PER_DAY;
    let now = i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH).expect("now").as_millis())
        .expect("ms");
    let today = now - now.rem_euclid(day);
    let perp_from = today - 4 * day;
    let hours = usize::try_from((now - perp_from) / HOUR).expect("hours");
    let mut perp = vike_studio_core::test_support::walk::random_walk_bars(hours, HOUR, Some(PERP));
    perp.iter_mut().for_each(|bar| bar.timestamp += perp_from - HOUR);
    memory.append_bars("binance", PERP, "1h", &perp, None).expect("seed the perpetual");
    let mut rate = vike_studio_core::test_support::walk::random_walk_bars(7, day, Some("USDTUSD"));
    for bar in &mut rate {
        bar.timestamp += today - 7 * day;
        (bar.open, bar.high, bar.low, bar.close) = (1.0, 1.0, 1.0, 1.0);
    }
    memory.append_bars("binance", "USDTUSD", "1d", &rate, None).expect("seed the USDT rate");
    let perpetual_row = vike_model::Instrument {
        naming: Some(vike_model::InstrumentNaming::User(PERP.to_string())),
        ..vike_model::Instrument::new(
            "binance",
            PERP,
            "BTC",
            "USDT",
            vike_model::Contract::Perp(vike_model::ContractPerp {
                margining: vike_model::Margining::Linear,
                settle: "USDT".into(),
                value_currency: None,
                funding_interval_ms: None,
                settling_ms: None,
            }),
        )
    };
    memory.serve_kept_instruments(vec![instrument("BTCUSDT", "BTC"), perpetual_row]);
    let restamp = Arc::new(AtomicU64::new(0));
    let inner: Arc<dyn HistoryStore + Send + Sync> = memory.clone();
    let store = stamping_restamped_by(inner, DayStamps::Settled, Arc::clone(&restamp));
    let runs = project.path().join("user_data").join("runs");
    // Each hub is built as the `--addr` daemon builds itself (`serving_study` over the project this test entered, and
    // the answer-from-saved identity of the program its children run, this package's `vike-backend`), its commit the
    // bare one the daemon is handed. `user_data` moves its runs root.
    let serving = |user_data: Option<&Path>, keep: Option<u32>, commit: &str| {
        let vars = user_data
            .map(|dir| ("VIKE_USER_DATA_DIR".to_string(), dir.display().to_string()))
            .into_iter()
            .collect();
        spawn_serving_hub(Arc::clone(&store), vars, keep, Some(commit.to_string()))
    };
    let hub = serving(None, None, "e32ef713dc");

    // ---- THE HIT: the same summarized run twice -------------------------------------------------------------
    let (first, miss_wall) = timed(hub, &summarized(profile(5, PAST)));
    let first = summary(first);
    assert!(first.n_trades > 0, "the file trades");
    assert!(first.answered_from_saved_run.is_empty(), "a run just computed says nothing");
    assert!(!first.saved_run.is_empty(), "the run was saved");
    let (second, hit_wall) = timed(hub, &summarized(profile(5, PAST)));
    let second = summary(second);
    assert_eq!(second.answered_from_saved_run, first.saved_run, "answered from the run it saved");
    assert_eq!(second.saved_run, first.saved_run);
    assert_ne!(second.run, first.run, "held again under a name of its own");
    assert_eq!(digest(&second), digest(&first), "the answer IS the saved run's");
    eprintln!(
        "from_saved: summarized single run over {BARS} bars: computed {miss_wall:?}, answered from saved run \
         {} in {hit_wall:?}; digest {}",
        first.saved_run,
        digest(&first)
    );
    // Its later reads page what the first's did: the trades it held, the whole curve its saved run keeps.
    let reads: [fn(&str) -> Request; 2] = [
        |run| Request::ReadTrades { run: run.to_string(), point: None, page: 0, page_size: 200 },
        |run| Request::ReadCurve { run: run.to_string(), page: 0, page_size: 100_000 },
    ];
    for read in reads {
        let a = serde_json::to_string(&ask(hub, &read(&first.run))).expect("encodes");
        let b = serde_json::to_string(&ask(hub, &read(&second.run))).expect("encodes");
        assert!(!a.contains("\"Error\""), "{a}");
        assert_eq!(a, b, "a page of the answer from saved is the page of the run that saved it");
    }
    assert_eq!(saved_runs(&runs).len(), 1, "a hit saves nothing");

    // The WHOLE answer is a hit too, its report byte for byte but its last key.
    let whole = Request::RunBacktest(profile(5, PAST));
    let (Response::Report(a), Response::Report(b)) = (ask(hub, &whole), ask(hub, &whole)) else {
        panic!("a whole run answers its report");
    };
    let mark = ",\"answered_from_saved_run\":\"";
    let at = b.rfind(mark).expect("the second whole answer says where it came from");
    assert_eq!(format!("{}}}", &b[..at]), a, "the whole report, byte for byte, but its last key");
    let id = &b[at + mark.len()..b.len() - 2];
    assert!(saved_runs(&runs).iter().any(|run| run == id) && id != first.saved_run, "{id}");

    // A profile's `name` is out of the key where the answer carries none: a summary.
    let named = profile(5, PAST).replacen("[data]", "name = \"another\"\n[data]", 1);
    let renamed = summary(ask(hub, &summarized(named)));
    assert_eq!(
        renamed.answered_from_saved_run, first.saved_run,
        "the name is no input of a summary"
    );

    // ---- MISS: a param changed ----------------------------------------------------------------------------------
    let other = summary(ask(hub, &summarized(profile(6, PAST))));
    assert!(other.answered_from_saved_run.is_empty(), "every = 6 is another run");
    assert_ne!(digest(&other), digest(&first));

    // ---- MISS: another build (another rule for the report: a tick curve kept by time and its `returns_interval_ms`
    // came with one). The profile's sampling keys are in its text: `answer_from_saved_tests.rs` holds that half.
    let rebuilt_hub = serving(None, None, "f00dfeed00");
    let other_build = summary(ask(rebuilt_hub, &summarized(profile(5, PAST))));
    assert!(
        other_build.answered_from_saved_run.is_empty(),
        "a run saved by another build answers none of this one"
    );
    assert_eq!(digest(&other_build), digest(&first));

    // ---- MISS: a day restamped (a re-imported day) ---------------------------------------------------------------
    restamp.fetch_add(1, Ordering::Relaxed);
    let restamped = summary(ask(hub, &summarized(profile(5, PAST))));
    assert!(restamped.answered_from_saved_run.is_empty(), "the bars it read were restamped");
    assert_eq!(digest(&restamped), digest(&first), "the same rows run to the same answer");
    let again = summary(ask(hub, &summarized(profile(5, PAST))));
    assert_eq!(
        again.answered_from_saved_run, restamped.saved_run,
        "the run saved over the new stamps"
    );

    // ---- MISS: a window that reaches today is never kept, never answered ---------------------------------------
    for _ in 0..2 {
        let open = summary(ask(hub, &summarized(profile(5, OPEN))));
        assert!(
            open.answered_from_saved_run.is_empty(),
            "a window reaching today is run each time"
        );
    }

    // ---- HIT: a perpetual whose window ended YESTERDAY (2026-11-21: its USDT-USD pair was read one run bar past the
    // window, into today, and no such run was ever answered from saved) ----------------------------------------------
    let computed = summary(ask(hub, &perpetual(perp_from, today - 1)));
    assert!(computed.n_trades > 0, "the file trades the perpetual");
    assert!(computed.answered_from_saved_run.is_empty() && !computed.saved_run.is_empty());
    let answered = summary(ask(hub, &perpetual(perp_from, today - 1)));
    assert_eq!(
        answered.answered_from_saved_run, computed.saved_run,
        "a window that ended yesterday is answered from its saved run"
    );
    assert_eq!(digest(&answered), digest(&computed));
    // ---- MISS: the perpetual over a window that reaches today is never kept, never answered ------------------------
    for _ in 0..2 {
        let open = summary(ask(hub, &perpetual(perp_from, OPEN.parse().expect("ms"))));
        assert!(open.answered_from_saved_run.is_empty(), "an open window runs each time");
    }

    // ---- MISS: Fresh runs it and saves it, keying nothing (it does none of the key's work) --------------------------
    let before = saved_runs(&runs).len();
    let fresh = Request::Fresh(Box::new(summarized(profile(5, PAST))));
    let fresh = summary(ask(hub, &fresh));
    assert!(fresh.answered_from_saved_run.is_empty(), "Fresh recomputes");
    assert_eq!(saved_runs(&runs).len(), before + 1, "and saves");
    let fresh_manifest = std::fs::read_to_string(runs.join(&fresh.saved_run).join("manifest.json"))
        .expect("the fresh run's manifest");
    assert!(!fresh_manifest.contains("input_key"), "a fresh run keys nothing: {fresh_manifest}");
    let after = summary(ask(hub, &summarized(profile(5, PAST))));
    assert_eq!(
        after.answered_from_saved_run, restamped.saved_run,
        "the newest KEYED saved run answers"
    );

    // ---- MISS: a new artifact under the same name (another toolchain's build of the same text) -----------------
    let builds = vike_backtesthub::studio_run::strategy_files_dir();
    let staged = std::fs::read_dir(&builds)
        .expect("builds")
        .flatten()
        .flat_map(|sdk| std::fs::read_dir(sdk.path()).into_iter().flatten().flatten())
        .map(|folder| folder.path().join(artifact.file_name().expect("artifact name")))
        .find(|path| path.is_file())
        .expect("the staged artifact");
    let mut bytes = std::fs::read(&staged).expect("artifact");
    bytes.extend_from_slice(&[0; 64]);
    std::fs::write(&staged, &bytes).expect("rebuilt artifact");
    let rebuilt = summary(ask(hub, &summarized(profile(5, PAST))));
    assert!(rebuilt.answered_from_saved_run.is_empty(), "another artifact is another run");
    assert_eq!(digest(&rebuilt), digest(&first));

    // ---- MISS: a changed kept instrument list --------------------------------------------------------------------
    assert!(!summary(ask(hub, &summarized(profile(5, PAST)))).answered_from_saved_run.is_empty());
    memory.serve_kept_instruments(vec![instrument("BTCUSDT", "BTC"), instrument("ETHUSDT", "ETH")]);
    let relisted = summary(ask(hub, &summarized(profile(5, PAST))));
    assert!(relisted.answered_from_saved_run.is_empty(), "the kept lists it read changed");

    // ---- A SEARCH: a hit only when the WHOLE request is identical; a miss once a read it recorded moved ------------
    let searched = search_summary(ask(hub, &search("5, 40")));
    assert!(searched.answered_from_saved_run.is_empty());
    let repeated = search_summary(ask(hub, &search("5, 40")));
    assert_eq!(repeated.answered_from_saved_run, searched.saved_run, "the whole search, identical");
    assert_eq!(repeated.rows, searched.rows);
    let rows =
        |run: &str| Request::ReadParamscanRows { run: run.to_string(), page: 0, page_size: 1 };
    assert_eq!(
        serde_json::to_string(&ask(hub, &rows(&repeated.run))).expect("encodes"),
        serde_json::to_string(&ask(hub, &rows(&searched.run))).expect("encodes"),
        "its rows page as the search's did"
    );
    let wider = search_summary(ask(hub, &search("5, 40, 60")));
    assert!(
        wider.answered_from_saved_run.is_empty(),
        "no saved point is mixed into another search"
    );
    restamp.fetch_add(1, Ordering::Relaxed);
    let moved = search_summary(ask(hub, &search("5, 40")));
    assert!(moved.answered_from_saved_run.is_empty(), "a read the search recorded was restamped");

    // ---- RETENTION: a run the retention removed answers nothing again -------------------------------------------
    let kept_one = project.path().join("user_data_kept_one");
    let kept_runs = kept_one.join("runs");
    let pruned = serving(Some(&kept_one), Some(1), "e32ef713dc");
    let x = summary(ask(pruned, &summarized(profile(5, PAST))));
    assert!(x.answered_from_saved_run.is_empty());
    let x_hit = summary(ask(pruned, &summarized(profile(5, PAST))));
    assert_eq!(x_hit.answered_from_saved_run, x.saved_run, "answered from x's saved run");
    let y = summary(ask(pruned, &summarized(profile(7, PAST))));
    assert!(y.answered_from_saved_run.is_empty());
    // The prune runs once the answer is written: give it its moment.
    for _ in 0..50 {
        if saved_runs(&kept_runs).len() == 1 {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(saved_runs(&kept_runs), vec![y.saved_run.clone()], "one run of the file is kept");
    // The hit HOLDS its whole curve as a fresh run does: the saved run it came from is pruned, and it still pages.
    let curve = |run: &str| {
        let read = Request::ReadCurve { run: run.to_string(), page: 0, page_size: 100_000 };
        serde_json::to_string(&ask(pruned, &read)).expect("encodes")
    };
    let hit_page = curve(&x_hit.run);
    assert!(!hit_page.contains("\"Error\""), "{hit_page}");
    assert_eq!(hit_page, curve(&x.run), "the hit's curve is the run's");
    let x_again = summary(ask(pruned, &summarized(profile(5, PAST))));
    assert!(x_again.answered_from_saved_run.is_empty(), "the pruned run is not resurrected");
    // Recomputed, so its digest is x's (its id may be x's own again: an id is minted by the second, and x's directory
    // is gone).
    assert_eq!(digest(&x_again), digest(&x));

    // ---- OLD RUNS: a run saved with no key is never a hit -------------------------------------------------------
    let old_user_data = project.path().join("user_data_old");
    let old_runs = old_user_data.join("runs");
    let before_the_change =
        spawn_hub_over_saving_to(Arc::clone(&store), strategies.clone(), old_runs.clone());
    let old = summary(ask(before_the_change, &summarized(profile(5, PAST))));
    assert!(!old.saved_run.is_empty(), "saved, with no key");
    let manifest =
        std::fs::read_to_string(old_runs.join(&old.saved_run).join("manifest.json")).expect("read");
    assert!(
        !manifest.contains("input_key"),
        "a hub that answers nothing from saved runs keys nothing: {manifest}"
    );
    let now = serving(Some(&old_user_data), None, "e32ef713dc");
    let read_again = summary(ask(now, &summarized(profile(5, PAST))));
    assert!(read_again.answered_from_saved_run.is_empty(), "a run with no key is never a hit");
    assert_eq!(digest(&read_again), digest(&old));

    std::env::set_current_dir(restore).expect("restore the working directory");
}
