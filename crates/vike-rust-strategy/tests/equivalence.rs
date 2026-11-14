//! **Mechanism equivalence — the test the whole design rests on.**
//!
//! One fixture strategy (`tests/fixtures/equivalence/ma_cross.rs`), one bar series, two paths:
//!
//! * **compiled in** — the fixture's bytes compiled as a MODULE of this test binary and its
//!   `build::<OrderBrokerSimulator>` called directly: a `#[path]` module plus a direct call of its generic
//!   `build`, the mechanism the retired build-time tier generated.
//! * **loaded as a strategy file** — the SAME bytes handed to the real
//!   `vike_strategy_builder::render::build_strategy_file`, which renders the cdylib template around them
//!   and runs cargo, then `vike_rust_strategy::loader::load` + `host::StrategyFileHost`.
//!
//! Both are then driven through the SAME `vike_simulator::StrategyEngine`/`OrderBrokerSimulator` the shipped
//! backtest path uses, over a byte-identical bar series, and the trade lists are compared FIELD BY
//! FIELD and the metrics AS VALUES — bit-for-bit, never within a tolerance. The design states the
//! stake outright: *"if the results differ, the host is deciding more than the mechanism."* So a
//! divergence here is a finding in the DESIGN. Do not widen an assertion, round a comparison, or
//! sort one side to match the other — that would destroy exactly the information this file exists
//! to produce.
//!
//! ⚠ **This file was `#[ignore]`d and is NOT any more, and the reason it was is worth keeping.**
//! It used to say it had to be, on two grounds: that it runs cargo (true, and it still does), and
//! that `build_strategy_file` hardcoded a release build while a debug test binary bakes
//! `profile=debug;opt=0` into `vike_rust_strategy::fingerprint::FINGERPRINT`, so a debug run
//! could only ever reach `LoadError::FingerprintMismatch`. The second was a real constraint and
//! the FIRST was not a reason at all — `tests/load_refusals.rs`, in this very crate, runs cargo
//! four times, is un-ignored, and gates every PR. Citing it as precedent for ignoring was the
//! wrong reading of the one sibling that contradicts it.
//!
//! So the constraint was removed instead of worked around: `build_strategy_file` takes a
//! `render::Profile`, and this file passes `Profile::of_this_binary()` — derived from the host's
//! OWN fingerprint, so the two halves match in a debug lane and in a release lane alike. **The
//! design's central claim now gates every PR**, which is what it was always supposed to do.
//!
//! One nested cargo build is the cost, cached by content between runs exactly as
//! `load_refusals.rs`'s four fixture builds are.
//!
//! ⚠ **What this file covers grew with `ABI_VERSION` 3, and it grew in TWO directions.**
//!
//! 1. **Two LANES, not one.** `StrategyEngine::run` and `StrategyEngine::run_ticks` emit
//!    different `Strategy` hooks — `on_bar`/`on_schedule` on the first, `on_quote_tick`/
//!    `on_trade_tick`/`on_order_book` on the second, `warmup`/`on_start`/`on_fill`/`on_stop` on
//!    both — so a single-lane comparison would have left three newly-wired hooks compiled,
//!    exported, bound and never once driven. Both lanes are now compared bit for bit, from the
//!    one built artifact.
//! 2. **`pending`, because `on_stop` leaves no other trace.** The engine calls it, but the run is
//!    over: an order submitted there can never fill and never becomes a `Trade`, so a
//!    `BacktestResult` is silent about it. `OrderBrokerSimulator`'s per-symbol working-order list is not, and
//!    it outlives `run` — so the comparison reads it there.
//!
//! 3. **The order language, at `ABI_VERSION` 5.** A second fixture,
//!    `tests/fixtures/equivalence/tactics_cross.rs`,
//!    writes a `vike_tactics::Rule` and builds it in `vike_tactics::RuleRun` over
//!    `B: vike_model::OrderBroker`. Its compiled-in half is a `#[path]` module of this binary too,
//!    its `build::<OrderBrokerSimulator>` called directly; `equivalence/tactics.rs` builds the strategy file
//!    from the same bytes, runs both on the bar lane under the simulator's default and reference
//!    in-bar rules, and compares them the same way, plus every fill and order end delivered, with its
//!    tag.
//!
//! 4. **The strategy side's reads, at `ABI_VERSION` 6** (decision 0271). Both fixtures declare an
//!    input (`Strategy::inputs`, `vike_tactics::Rule::inputs`) and read it and their closed trades,
//!    and [`drive`] loads exactly what a strategy declares, as the backtest's input planner does
//!    (`series::input_series`), so the `inputs` export, the two reads and the closed rows all decide
//!    what the two halves trade.
//!
//! 5. **The symbol, at `ABI_VERSION` 8** (the one broker interface). Every order slot carries the
//!    symbol an order names, so the order-language fixture also runs a TWO-symbol rule (a hedge on a
//!    second symbol through `Tactics::on`) over a two-symbol run, compared the same way, with the
//!    symbols it declares (`Strategy::symbols`).
//!
//! ⚠ **Six hooks are NOT in that comparison and cannot be** — `on_feed_status`, `on_mark`,
//! `on_reference_quote`, `on_flow`, `on_order_event`, `on_params_updated`. No backtest engine
//! emits any of them (each one's doc in `crates/vike-model/src/strategy/mod.rs` says so, and
//! neither engine holds a call site), so folding them in would mean comparing two runs in which
//! they never happened — an agreement that says nothing. They get a narrower, honestly-labelled
//! witness instead: `assert_live_only_hooks_reach_the_strategy_file`, at the bottom of this file, which
//! states in its own doc what it does and does not establish.
//!
//! ⚠ And `the_fixture_overrides_every_hook_the_vtable_carries` is what keeps the pairing honest
//! in the other direction: a hook added to `WIRED_HOOKS` that the fixture never overrides makes
//! this comparison agree about two no-op trait defaults, which reads green and proves nothing.
//!
//! # ⚠ Nested cargo inside the merge gate — the decision, not a discovery
//!
//! Un-ignoring this test has a consequence worth stating outright rather than finding out about
//! on a red run: `vike-rust-strategy` is an ordinary workspace member, so it joins the DERIVED
//! CI roster (`xtask::ci::roster::ci_crates`) automatically, and CI's fast lane therefore now shells
//! `cargo` from inside `cargo nextest`, in parallel, against one `$CARGO_HOME`. Nested cargo has
//! been a real flake source in this tree. **The decision is to keep it there**, and these are the
//! four properties it rests on — each one LOAD-BEARING, so removing any of them reopens this:
//!
//! 1. **The known CARGO_HOME flake is already fixed at the runner, not worked around here.**
//!    Concurrent jobs mutating one shared `~/.cargo` produced the intermittent
//!    `could not parse/generate dependency info` failures of 2026-07-15; #301 gave every runner instance
//!    its own persistent `CARGO_HOME`. What remains WITHIN a job is cargo's `.package-cache`
//!    lock, which BLOCKS — several `nextest` processes each running a nested build serialize on
//!    it and none of them fails.
//! 2. **No nested build can touch the shared workspace `target/`.** `render::build_strategy_file` sets
//!    `CARGO_TARGET_DIR` explicitly to a directory under the scratch tree, and
//!    `load_refusals.rs`'s fixture builds pass `--target-dir`. That is what keeps this away from
//!    the OTHER known class in this repo — a poisoned shared target cache.
//! 3. **Scratch directories are keyed by checkout as well as by content**, so two concurrent PR
//!    jobs on one box building this same committed fixture do not land in one directory and
//!    rewrite each other's rendered manifest. `render::build_strategy_file`'s own comment at the
//!    `root_tag` line carries that incident; it was introduced BY this test being un-ignored.
//! 4. **The nested dependency graph is a strict subset of one the outer build just resolved** —
//!    `vike-model` and `vike-rust-strategy` by path, plus `toml` (the guest's params parser),
//!    which is a workspace dependency. So the registry cache is warm by construction and a nested
//!    build performs no fetch in practice.
//!
//! ⚠ **`--offline` was considered for the nested invocation and REJECTED**, though it looks like
//! the obvious hardening. It does not remove the contention that matters (cargo still takes the
//! package-cache lock offline), property 4 already removes the fetch it would prevent, and
//! `build_strategy_file` is PRODUCTION code: the deployed builder service calls the same function, where
//! `--offline` would turn "this box's registry cache has not seen this crate version" from a
//! fetch into a hard build failure for a user's strategy. A flag that helps a test and degrades
//! the service is the wrong place to put it; if CI ever needs it, it belongs as a parameter the
//! test passes, not as a default.
//!
//! ⚠ **Excluding this crate from the fast lane (`xtask/src/ci/tables/roster.rs`) was also considered and
//! rejected**, for the plainest reason available: the whole point of the work above was to make
//! the design's central claim gate every PR. An exclusion would un-do that and leave the file
//! looking as though it still did.

// ⚠ **No `use vike_indicators::Indicator` below, and its absence is deliberate.** The probe in
// this file advances a `Box<dyn Indicator>` the FIXTURE built, and a trait object's own methods
// are inherent to it — so importing the trait here is an UNUSED import, which this workspace's
// `-D warnings` clippy gate refuses. The compiled-in half of the comparison resolves
// `vike-indicators` through this crate's `[dev-dependencies]`, which is what compiles the fixture
// MODULE; a strategy file resolves the same crate through the rendered template. Neither depends on an
// import here.
use vike_rust_strategy::loader;
use vike_strategy_builder::render::Profile;

use diffs::{metric_diffs, pending_diffs, result_shape_diffs, trade_diffs};
use drive::{
    Lane, build_fixture_strategy_file, build_floor_strategy_file, profile_tag, run_compiled,
    run_strategy_file,
};
use live_only::assert_live_only_hooks_reach_the_strategy_file;
use vacuity::assert_not_vacuous;

/// The fixture compiled INTO this binary: as a module reached by `#[path]`, in a separate
/// compilation unit from the framework, importing nothing but the user-facing crates.
#[path = "fixtures/equivalence/ma_cross.rs"]
mod ma_cross;

/// The SAME bytes the module above was compiled from, handed to the builder. `include_str!` and
/// `#[path]` reading one file is what makes this a comparison of MECHANISMS rather than of two
/// sources that happen to look alike.
const FIXTURE_SOURCE: &str = include_str!("fixtures/equivalence/ma_cross.rs");

/// The ORDER LANGUAGE fixture (`ABI_VERSION` 5) compiled INTO this binary the same way: a
/// `vike_tactics::Rule` whose `build` bounds `B: vike_model::OrderBroker`.
#[path = "fixtures/equivalence/tactics_cross.rs"]
mod tactics_cross;

/// The SAME bytes [`tactics_cross`] was compiled from, handed to the builder by
/// `tactics::tactics_diffs`.
const TACTICS_SOURCE: &str = include_str!("fixtures/equivalence/tactics_cross.rs");

/// The tactics fixture's parameters; both windows differ from its fallbacks (5 and 20), and its
/// input and ledger gain from its none and 0, so a document that failed to cross runs a different
/// rule.
const TACTICS_PARAMS_TOML: &str = "fast = 6\nslow = 24\nsymbol = \"BTCUSDT\"\n\
                                   input = \"bybit:ETHUSDT:1m\"\nledger_gain = 0.02\n";

/// [`TACTICS_PARAMS_TOML`] with the rule hedging on [`OTHER_SYMBOL`] (`ABI_VERSION` 8): the
/// two-symbol row, run over `series::pair_universe`.
const TACTICS_PAIR_PARAMS_TOML: &str = "fast = 6\nslow = 24\nsymbol = \"BTCUSDT\"\n\
                                        input = \"bybit:ETHUSDT:1m\"\nledger_gain = 0.02\n\
                                        other = \"ETHUSDT\"\n";

/// The second symbol the two-symbol row trades.
const OTHER_SYMBOL: &str = "ETHUSDT";

/// [`TACTICS_PARAMS_TOML`] declaring no input, and NOTHING else changed.
const TACTICS_NO_INPUT_PARAMS: &str =
    "fast = 6\nslow = 24\nsymbol = \"BTCUSDT\"\nledger_gain = 0.02\n";

/// [`TACTICS_PARAMS_TOML`] with the closed net read for nothing, and NOTHING else changed.
const TACTICS_NO_LEDGER_PARAMS: &str =
    "fast = 6\nslow = 24\nsymbol = \"BTCUSDT\"\ninput = \"bybit:ETHUSDT:1m\"\n";

/// The input both fixtures' documents name. One name on both lanes; `series::input_series` says why
/// the tick lane's stand-in is one-second bars.
const INPUT_NAME: &str = "bybit:ETHUSDT:1m";

/// The strategy's parameters, crossing to the strategy file as TEXT (no Rust type crosses this ABI by
/// value) and parsed on both sides by the fixture's own lenient reader.
/// ⚠ **Every value here DIFFERS from the fixture's own default** (`build`'s fallbacks are
/// `5`/`20`/`0.25`), and that is load-bearing rather than arbitrary. If the params document
/// failed to cross — silently parsed as empty, dropped, or truncated — the strategy file would run
/// 5/20/0.25 while the compiled half ran 6/24/0.3, and the comparison would fail LOUDLY. Equal
/// values would have made a lost params table invisible, and this file's first version had
/// exactly that hole: its `warmup` probe asserted `20`, which is also the default.
/// ⚠ `rsi_period` and the `[controller]` table are the two keys that reach the crates the
/// TEMPLATE gained — the fixture's own fallbacks are "no indicator" and "no controller", so these
/// obey the same differs-from-the-default rule as the three above and they are also the switches
/// [`NO_INDICATOR_PARAMS`] / [`NO_CONTROLLER_PARAMS`] turn off. `input` and `ledger_gain` are the
/// two the `ABI_VERSION` 6 reads hang on, under the same rule ([`NO_INPUT_PARAMS`] /
/// [`NO_LEDGER_PARAMS`]). The table must come LAST: in TOML every bare key after a table header
/// belongs to that table.
const PARAMS_TOML: &str = "fast = 6\nslow = 24\npct = 0.3\nrsi_period = 14\n\
                           input = \"bybit:ETHUSDT:1m\"\nledger_gain = 2.0\n\n\
                           [controller]\nqty = 2.0\nthreshold = 0.05\n";

/// [`PARAMS_TOML`] with the catalog indicator turned off, and NOTHING else changed.
const NO_INDICATOR_PARAMS: &str = "fast = 6\nslow = 24\npct = 0.3\nrsi_period = 0\n\
                                   input = \"bybit:ETHUSDT:1m\"\nledger_gain = 2.0\n\n\
                                   [controller]\nqty = 2.0\nthreshold = 0.05\n";

/// [`PARAMS_TOML`] with the controller framework turned off, and NOTHING else changed.
const NO_CONTROLLER_PARAMS: &str = "fast = 6\nslow = 24\npct = 0.3\nrsi_period = 14\n\
                                    input = \"bybit:ETHUSDT:1m\"\nledger_gain = 2.0\n";

/// [`PARAMS_TOML`] declaring no input, and NOTHING else changed.
const NO_INPUT_PARAMS: &str = "fast = 6\nslow = 24\npct = 0.3\nrsi_period = 14\n\
                               ledger_gain = 2.0\n\n\
                               [controller]\nqty = 2.0\nthreshold = 0.05\n";

/// [`PARAMS_TOML`] with the closed-trade ledger read for nothing, and NOTHING else changed.
const NO_LEDGER_PARAMS: &str = "fast = 6\nslow = 24\npct = 0.3\nrsi_period = 14\n\
                                input = \"bybit:ETHUSDT:1m\"\n\n\
                                [controller]\nqty = 2.0\nthreshold = 0.05\n";

/// `slow`, and therefore the fixture's declared `warmup()`. Named so the probe below asserts the
/// value the DOCUMENT carries rather than a number that happens to match a fallback.
const PARAMS_SLOW: usize = 24;

/// A minimal but VALID user strategy: the entry contract, one wired hook, and nothing else.
///
/// Built beside the real fixture so a run reports a FLOOR as well as a figure — the bytes a
/// strategy file costs before its author has written anything, i.e. whatever the template's declared
/// dependency closure contributes on its own.
///
/// ⚠ **It exists to make a template CHANGE measurable, and the fixture's own number cannot do
/// that job.** A crate a user file never NAMES is dead code the linker drops, so widening the
/// template's `[dependencies]` moves this floor only if mere availability costs bytes; the
/// fixture's number moves for availability and for USE at once and cannot separate them. Compare
/// this constant's artifact across two templates, and the fixture's artifact across two fixtures.
const FLOOR_SOURCE: &str = r#"
use vike_model::{OrderBroker, Strategy};
use vike_marketdata::Bar;

pub struct Floor;

impl<B: OrderBroker> Strategy<B> for Floor {
    fn on_bar(&mut self, _broker: &mut B, _bar: &Bar) {}
}

pub fn build<B: vike_model::OrderBroker + 'static>(
    _params: &vike_model::params::Params,
) -> Box<dyn vike_model::Strategy<B> + Send> {
    Box::new(Floor)
}
"#;

const SYMBOL: &str = "BTCUSDT";
const N_BARS: usize = 400;
/// Priced ticks (quotes and trades, alternating) in the tick-lane series. The tick lane's warm-up
/// index counts PRICED ticks only — a `Tick::Book` never bumps it — so the fixture's declared
/// warm-up of [`PARAMS_SLOW`] ticks must pass, and then another [`PARAMS_SLOW`] must accumulate
/// before its slow SMA exists at all. This is comfortably past both.
const N_TICKS: usize = 700;
/// How often a book DELTA is interleaved into the priced tape. Chosen coprime-ish with the
/// quote/trade alternation so a delta does not always land after the same kind of tick.
const BOOK_EVERY: usize = 23;
/// The schedule rule the bar lane registers, in bars. Fires four times over [`N_BARS`] once the
/// warm-up gate has opened — often enough that a mechanism which never delivered `on_schedule`
/// diverges, rare enough that the crossover is still what drives the run.
const SCHEDULE_EVERY_N: usize = 97;
/// Trading days per year — the `periods_per_year` argument the ratio metrics take. Any fixed
/// value works here: both sides are handed the identical one, so it scales two numbers that must
/// agree rather than deciding whether they do.
const PERIODS_PER_YEAR: f64 = 365.0;

#[path = "equivalence/diffs.rs"]
mod diffs;
#[path = "equivalence/drive.rs"]
mod drive;
#[path = "equivalence/live_only.rs"]
mod live_only;
#[path = "equivalence/probes.rs"]
mod probes;
#[path = "equivalence/series.rs"]
mod series;
#[path = "equivalence/tactics.rs"]
mod tactics;
#[path = "equivalence/vacuity.rs"]
mod vacuity;

// ---------------------------------------------------------------------------------------------
// The tests
// ---------------------------------------------------------------------------------------------

/// THE test. Un-ignored — see this file's module doc for what used to stop that and how it was
/// removed rather than worked around.
///
/// ⚠ **It is the ONLY caller of [`build_fixture_strategy_file`], deliberately.** Under `cargo nextest`
/// every test is its own PROCESS, and two processes racing one `build_strategy_file` call on a cold
/// cache would share a scratch target directory whose cdylib the winner `rename`s away before
/// the loser looks for it. One caller means no race by construction; a second test wanting an
/// artifact should take one built here rather than build its own into the same directory.
#[test]
fn the_two_mechanisms_produce_identical_trades_and_metrics() {
    let so = build_fixture_strategy_file();
    let bytes = std::fs::metadata(&so)
        .unwrap_or_else(|e| panic!("cannot stat the built artifact {}: {e}", so.display()))
        .len();

    // ⚠ A NAMED witness for the defect this test found on its first execution, asserted before
    // the comparison so a regression reports its own cause instead of arriving as "the two
    // mechanisms traded differently". The template used to parse its params with
    // `str::parse::<toml::Value>()`, which under the pinned `toml` 1.1 parses a single VALUE
    // expression rather than a document, so `create` returned null for EVERY params string —
    // and a null handle is the one failure this ABI cannot report through a status code.
    {
        let probe = loader::load(&so).expect("the artifact must load for the null-handle probe");
        let handle =
            (probe.vtable.create)(PARAMS_TOML.as_ptr(), PARAMS_TOML.len(), "{}".as_ptr(), 2);
        assert!(
            !handle.is_null(),
            "`vike_strategy_create` returned NULL for a well-formed params document \
             ({PARAMS_TOML:?}). Every dispatch will report `StrategyFileStatusAbi::BadHandle` and the \
             strategy will trade nothing, while the artifact builds, loads and handshakes \
             cleanly. The known cause is the cdylib template parsing params as a TOML VALUE \
             instead of a DOCUMENT — `toml::from_str`, never `str::parse`."
        );
        // The declared warmup must have SURVIVED the document, not merely been non-null. This
        // compares against the value PARAMS_TOML carries, which is deliberately not the
        // fixture's own fallback — so a parse that silently yielded an empty table fails here
        // instead of passing on a coincidence.
        assert_eq!(
            (probe.vtable.warmup)(handle),
            PARAMS_SLOW,
            "the warmup must come from the params DOCUMENT; the fixture's own fallback is a \
             different number, so this cannot pass on an empty table"
        );
        (probe.vtable.destroy)(handle);
        // The `ABI_VERSION` 6 export, named before the comparison: the declared input must cross,
        // or the run loads nothing for the strategy file and the trades below differ for that reason.
        let strategy_file = vike_rust_strategy::host::StrategyFileHost::<
            vike_simulator::OrderBrokerSimulator,
        >::new(probe.vtable, PARAMS_TOML, "{}");
        assert_eq!(
            vike_model::Strategy::inputs(&strategy_file),
            [INPUT_NAME],
            "the strategy file must declare the input its params document names"
        );
    }

    // BOTH lanes, because they emit DIFFERENT hooks and neither alone reaches the whole seam:
    // `run` emits `on_bar`/`on_schedule`, `run_ticks` emits `on_quote_tick`/`on_trade_tick`/
    // `on_order_book`, and both emit `warmup`/`on_start`/`on_fill`/`on_stop`. A single-lane
    // comparison would have left the three tick-lane hooks wired and unexercised — the precise
    // shape of "a test that performed for the system the step it was meant to witness".
    let mut diffs: Vec<String> = Vec::new();
    let mut measured: Vec<String> = Vec::new();
    for lane in [Lane::Bars, Lane::Ticks] {
        let compiled = run_compiled(lane);
        assert_not_vacuous(&compiled, lane);
        let strategy_file = run_strategy_file(&so, lane);

        let where_ = lane.label();
        let mut lane_diffs = result_shape_diffs(&compiled.result, &strategy_file.result);
        lane_diffs.extend(trade_diffs(&compiled.result.trades, &strategy_file.result.trades));
        lane_diffs.extend(metric_diffs(&compiled.result, &strategy_file.result));
        lane_diffs.extend(pending_diffs(&compiled.pending, &strategy_file.pending));
        diffs.extend(lane_diffs.into_iter().map(|d| format!("[{where_}] {d}")));

        measured.push(format!(
            "MEASURED {where_}: {} closed trades, {} pending order(s) after on_stop, final \
             equity {:?} (compiled) / {:?} (strategy file)",
            compiled.result.n_trades,
            compiled.pending.len(),
            compiled.result.final_equity,
            strategy_file.result.final_equity
        ));
    }

    // Reported BEFORE the assertion so the numbers land in the run log whether it passes or
    // fails: the design's open sizing question is answered from exactly this artifact, the one
    // the equivalence verdict above was produced from.
    // ⚠ NAMES THE PROFILE, because this test is un-ignored now and therefore usually runs in
    // DEBUG — where the artifact is several times the size of the one a deployed builder
    // produces. The figure recorded in the design doc is the RELEASE one; a debug number read off
    // this line and filed as "the strategy file size" would be wrong by a multiple.
    println!(
        "MEASURED strategy file artifact ({} profile): {bytes} bytes ({:.1} KiB) at {}",
        profile_tag(Profile::of_this_binary()),
        bytes as f64 / 1024.0,
        so.display()
    );
    // ...and the FLOOR, from the same builder in the same profile into the same directory. See
    // `FLOOR_SOURCE`: this is the figure that moves when the TEMPLATE's dependency set changes
    // and a user file names none of the new crates, which is the only way to price mere
    // AVAILABILITY apart from use.
    let floor = build_floor_strategy_file();
    let floor_bytes = std::fs::metadata(&floor)
        .unwrap_or_else(|e| panic!("cannot stat the floor artifact {}: {e}", floor.display()))
        .len();
    println!(
        "MEASURED floor artifact ({} profile, a strategy that names nothing but vike_model): \
         {floor_bytes} bytes ({:.1} KiB)",
        profile_tag(Profile::of_this_binary()),
        floor_bytes as f64 / 1024.0
    );
    // ⚠ DERIVED, not measured, and labelled so. It is `bytes * 40` — arithmetic on the measured
    // base above, under the assumption that forty edits produce forty distinct artifacts of
    // about this size. It is also a MAPPED-bytes figure rather than an RSS one: `dlopen` maps
    // file-backed segments, so a stale mapping costs address space and page cache and costs
    // resident memory only for pages that were touched. Nothing here measured RSS.
    println!(
        "DERIVED (= measured artifact x 40, mapped bytes, not RSS) 40-edit session leak, nothing \
         calls dlclose: {} bytes ({:.1} MiB)",
        bytes * 40,
        (bytes * 40) as f64 / (1024.0 * 1024.0)
    );
    for line in &measured {
        println!("{line}");
    }

    // The order language, from its own fixture and its own artifact, built HERE (trap 5) — the
    // `ABI_VERSION` 5 seam (`OrderBroker`, `instrument_grid`, the tagged fill) compared the same way.
    diffs.extend(tactics::tactics_diffs());

    assert!(
        diffs.is_empty(),
        "THE TWO MECHANISMS DIVERGED — {} difference(s). This is a finding in the DESIGN, not a \
         tolerance to widen:\n  {}",
        diffs.len(),
        diffs.join("\n  ")
    );

    // The six hooks no engine emits, proven the only honest way available — see that function's
    // own doc for what it does and does not establish.
    assert_live_only_hooks_reach_the_strategy_file(&so);
}
