//! **Mechanism equivalence — the test the whole design rests on.**
//!
//! One fixture strategy (`tests/fixtures/equivalence/ma_cross.rs`), one bar series, two paths:
//!
//! * **compiled in** — the fixture's bytes compiled as a MODULE of this test binary and its
//!   `build::<SimBroker>` called directly. That is exactly the mechanism
//!   `vike-user-strategies`' build-time tier produces: its generated registry is
//!   `#[path = "<abs>"] pub mod user_<name>;` plus a `user_<name>::build::<B>(params)` call
//!   (`crates/vike-user-strategies/src/codegen.rs`'s `render`), and
//!   [`generated_registry_still_reaches_a_user_file_as_a_path_module`] holds that claim honest by
//!   asserting the real renderer still emits that shape.
//! * **loaded as a plugin** — the SAME bytes handed to the real
//!   `vike_strategy_builder::render::build_plugin`, which renders the cdylib template around them
//!   and runs cargo, then `vike_strategy_plugin::loader::load` + `host::PluginStrategy`.
//!
//! Both are then driven through the SAME `vike_backtest::StrategyEngine`/`SimBroker` the shipped
//! backtest path uses, over a byte-identical bar series, and the trade lists are compared FIELD BY
//! FIELD and the metrics AS VALUES — bit-for-bit, never within a tolerance. The design states the
//! stake outright: *"if the results differ, the host is deciding more than the mechanism."* So a
//! divergence here is a finding in the DESIGN. Do not widen an assertion, round a comparison, or
//! sort one side to match the other — that would destroy exactly the information this file exists
//! to produce.
//!
//! ⚠ **This file was `#[ignore]`d and is NOT any more, and the reason it was is worth keeping.**
//! It used to say it had to be, on two grounds: that it runs cargo (true, and it still does), and
//! that `build_plugin` hardcoded a release build while a debug test binary bakes
//! `profile=debug;opt=0` into `vike_strategy_plugin::fingerprint::FINGERPRINT`, so a debug run
//! could only ever reach `LoadError::FingerprintMismatch`. The second was a real constraint and
//! the FIRST was not a reason at all — `tests/load_refusals.rs`, in this very crate, runs cargo
//! four times, is un-ignored, and gates every PR. Citing it as precedent for ignoring was the
//! wrong reading of the one sibling that contradicts it.
//!
//! So the constraint was removed instead of worked around: `build_plugin` takes a
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
//!    `BacktestResult` is silent about it. `SimBroker`'s per-symbol working-order list is not, and
//!    it outlives `run` — so the comparison reads it there.
//!
//! ⚠ **Six hooks are NOT in that comparison and cannot be** — `on_feed_status`, `on_mark`,
//! `on_reference_quote`, `on_flow`, `on_order_event`, `on_params_updated`. No backtest engine
//! emits any of them (each one's doc in `crates/vike-model/src/strategy/mod.rs` says so, and
//! neither engine holds a call site), so folding them in would mean comparing two runs in which
//! they never happened — an agreement that says nothing. They get a narrower, honestly-labelled
//! witness instead: `assert_live_only_hooks_reach_the_plugin`, at the bottom of this file, which
//! states in its own doc what it does and does not establish.
//!
//! ⚠ And `the_fixture_overrides_every_hook_the_vtable_carries` is what keeps the pairing honest
//! in the other direction: a hook added to `WIRED_HOOKS` that the fixture never overrides makes
//! this comparison agree about two no-op trait defaults, which reads green and proves nothing.
//!
//! # ⚠ Nested cargo inside the merge gate — the decision, not a discovery
//!
//! Un-ignoring this test has a consequence worth stating outright rather than finding out about
//! on a red run: `vike-strategy-plugin` is an ordinary workspace member, so it joins the DERIVED
//! CI roster (`xtask::ci::ci_crates`) automatically, and CI's fast lane therefore now shells
//! `cargo` from inside `cargo nextest`, in parallel, against one `$CARGO_HOME`. Nested cargo has
//! been a real flake source in this tree. **The decision is to keep it there**, and these are the
//! four properties it rests on — each one LOAD-BEARING, so removing any of them reopens this:
//!
//! 1. **The known CARGO_HOME flake is already fixed at the runner, not worked around here.**
//!    Concurrent jobs mutating one shared `~/.cargo` produced the intermittent
//!    `could not parse/generate dep info` failures of 2026-07-15; #301 gave every runner instance
//!    its own persistent `CARGO_HOME`. What remains WITHIN a job is cargo's `.package-cache`
//!    lock, which BLOCKS — several `nextest` processes each running a nested build serialize on
//!    it and none of them fails.
//! 2. **No nested build can touch the shared workspace `target/`.** `render::build_plugin` sets
//!    `CARGO_TARGET_DIR` explicitly to a directory under the scratch tree, and
//!    `load_refusals.rs`'s fixture builds pass `--target-dir`. That is what keeps this away from
//!    the OTHER known class in this repo — a poisoned shared target cache.
//! 3. **Scratch directories are keyed by checkout as well as by content**, so two concurrent PR
//!    jobs on one box building this same committed fixture do not land in one directory and
//!    rewrite each other's rendered manifest. `render::build_plugin`'s own comment at the
//!    `root_tag` line carries that incident; it was introduced BY this test being un-ignored.
//! 4. **The nested dependency graph is a strict subset of one the outer build just resolved** —
//!    `vike-model` and `vike-strategy-plugin` by path, plus `toml`, which is a workspace
//!    dependency. So the registry cache is warm by construction and a nested build performs no
//!    fetch in practice.
//!
//! ⚠ **`--offline` was considered for the nested invocation and REJECTED**, though it looks like
//! the obvious hardening. It does not remove the contention that matters (cargo still takes the
//! package-cache lock offline), property 4 already removes the fetch it would prevent, and
//! `build_plugin` is PRODUCTION code: the deployed builder service calls the same function, where
//! `--offline` would turn "this box's registry cache has not seen this crate version" from a
//! fetch into a hard build failure for a user's strategy. A flag that helps a test and degrades
//! the service is the wrong place to put it; if CI ever needs it, it belongs as a parameter the
//! test passes, not as a default.
//!
//! ⚠ **Excluding this crate from the fast lane (`xtask/src/ci/tables.rs`) was also considered and
//! rejected**, for the plainest reason available: the whole point of the work above was to make
//! the design's central claim gate every PR. An exclusion would un-do that and leave the file
//! looking as though it still did.

use std::path::{Path, PathBuf};

// ⚠ **No `use vike_indicators::Indicator` below, and its absence is deliberate.** The probe in
// this file advances a `Box<dyn Indicator>` the FIXTURE built, and a trait object's own methods
// are inherent to it — so importing the trait here is an UNUSED import, which this workspace's
// `-D warnings` clippy gate refuses. The compiled-in half of the comparison resolves
// `vike-indicators` through this crate's `[dev-dependencies]`, which is what compiles the fixture
// MODULE; a plugin resolves the same crate through the rendered template. Neither depends on an
// import here.
use vike_backtest::engine::Tick;
use vike_backtest::schedule::DateRule;
use vike_backtest::{BacktestResult, EngineParams, SimBroker, StrategyEngine, metrics};
use vike_model::{
    Bar, BookLevel, BookUpdate, BookUpdateKind, Broker, HftBroker, QuoteTick, Strategy, Trade,
    TradeTick, WorkingOrder,
};
use vike_strategy_builder::render::{Profile, artifact_dir_tag, build_plugin};
use vike_strategy_plugin::abi;
use vike_strategy_plugin::host::PluginStrategy;
use vike_strategy_plugin::loader;

/// The fixture compiled INTO this binary, the way the build-time tier compiles a user file: as a
/// module reached by `#[path]`, in a separate compilation unit from the framework, importing
/// nothing but `vike_model` and `toml`.
#[path = "fixtures/equivalence/ma_cross.rs"]
mod ma_cross;

/// The SAME bytes the module above was compiled from, handed to the builder. `include_str!` and
/// `#[path]` reading one file is what makes this a comparison of MECHANISMS rather than of two
/// sources that happen to look alike.
const FIXTURE_SOURCE: &str = include_str!("fixtures/equivalence/ma_cross.rs");

/// The strategy's parameters, crossing to the plugin as TEXT (no Rust type crosses this ABI by
/// value) and parsed on both sides by the fixture's own lenient reader.
/// ⚠ **Every value here DIFFERS from the fixture's own default** (`build`'s fallbacks are
/// `5`/`20`/`0.25`), and that is load-bearing rather than arbitrary. If the params document
/// failed to cross — silently parsed as empty, dropped, or truncated — the plugin would run
/// 5/20/0.25 while the compiled half ran 6/24/0.3, and the comparison would fail LOUDLY. Equal
/// values would have made a lost params table invisible, and this file's first version had
/// exactly that hole: its `warmup` probe asserted `20`, which is also the default.
/// ⚠ `rsi_period` and the `[controller]` table are the two keys that reach the crates the
/// TEMPLATE gained — the fixture's own fallbacks are "no indicator" and "no controller", so these
/// obey the same differs-from-the-default rule as the three above and they are also the switches
/// [`NO_INDICATOR_PARAMS`] / [`NO_CONTROLLER_PARAMS`] turn off. The table must come LAST: in TOML
/// every bare key after a table header belongs to that table.
const PARAMS_TOML: &str = "fast = 6\nslow = 24\npct = 0.3\nrsi_period = 14\n\n\
                           [controller]\nqty = 2.0\nthreshold = 0.05\n";

/// [`PARAMS_TOML`] with the catalog indicator turned off, and NOTHING else changed.
const NO_INDICATOR_PARAMS: &str = "fast = 6\nslow = 24\npct = 0.3\nrsi_period = 0\n\n\
                                   [controller]\nqty = 2.0\nthreshold = 0.05\n";

/// [`PARAMS_TOML`] with the controller framework turned off, and NOTHING else changed.
const NO_CONTROLLER_PARAMS: &str = "fast = 6\nslow = 24\npct = 0.3\nrsi_period = 14\n";

/// `slow`, and therefore the fixture's declared `warmup()`. Named so the probe below asserts the
/// value the DOCUMENT carries rather than a number that happens to match a fallback.
const PARAMS_SLOW: usize = 24;

/// A minimal but VALID user strategy: the entry contract, one wired hook, and nothing else.
///
/// Built beside the real fixture so a run reports a FLOOR as well as a figure — the bytes a
/// plugin costs before its author has written anything, i.e. whatever the template's declared
/// dependency closure contributes on its own.
///
/// ⚠ **It exists to make a template CHANGE measurable, and the fixture's own number cannot do
/// that job.** A crate a user file never NAMES is dead code the linker drops, so widening the
/// template's `[dependencies]` moves this floor only if mere availability costs bytes; the
/// fixture's number moves for availability and for USE at once and cannot separate them. Compare
/// this constant's artifact across two templates, and the fixture's artifact across two fixtures.
const FLOOR_SOURCE: &str = r#"
use vike_model::{Bar, Broker, Strategy};

pub struct Floor;

impl<B: Broker> Strategy<B> for Floor {
    fn on_bar(&mut self, _broker: &mut B, _bar: &Bar) {}
}

pub fn build<B: vike_model::HftBroker + 'static>(
    _params: &toml::Value,
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

// ---------------------------------------------------------------------------------------------
// The series and the engine
// ---------------------------------------------------------------------------------------------

/// A deterministic bar series: an LCG random walk in whole hundredths, built from integer
/// arithmetic and one division so it is bit-identical on every box and carries no platform
/// transcendental. `symbol` is left `None` — `StrategyEngine::new` stamps every bar with
/// `format_instrument(default_venue, symbol)`, and with the default (absent) venue that is the
/// bare symbol `SimBroker::idx` resolves.
fn bar_series() -> Vec<Bar> {
    let mut seed: u64 = 0x5EED_FACE_C0DE_1234;
    let mut price: f64 = 100.0;
    let mut out = Vec::with_capacity(N_BARS);
    for i in 0..N_BARS {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        // -2.00 ..= +2.00 in hundredths: enough amplitude that a 5/20 SMA pair crosses often.
        let step = ((seed >> 33) % 401) as f64 / 100.0 - 2.0;
        price = (price + step).max(5.0);
        out.push(Bar {
            ts: 1_700_000_000_000 + i as i64 * 60_000,
            open: price,
            high: price + 0.5,
            low: (price - 0.5).max(0.01),
            close: price,
            volume: 1_000.0 + (i % 17) as f64,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        });
    }
    out
}

fn universe() -> Vec<(String, Vec<Bar>)> {
    vec![(SYMBOL.to_string(), bar_series())]
}

/// Built fresh per run rather than cloned: `EngineParams` carries boxed closures and is not
/// `Clone`. Non-zero fees and slippage on purpose — they put more arithmetic between the
/// strategy's order and the trade the result reports.
fn engine_params() -> EngineParams {
    EngineParams { cash: 100_000.0, fee_rate: 0.000_6, slippage: 0.000_4, ..Default::default() }
}

/// A deterministic tick tape for the `run_ticks` lane: quotes and trades alternating, with an
/// anchoring book SNAPSHOT before the first priced tick and a book DELTA every [`BOOK_EVERY`].
///
/// ⚠ **The book's PRICE levels never move; only the best bid's resting QTY does.** That is what
/// makes the fixture's `book_bias` read a moving number while `best_bid`/`best_ask` stay put, so a
/// cursor that lost the quantities — or delivered the two sides in the wrong order — changes every
/// subsequent order's size instead of changing nothing. A delta that introduced new price levels
/// would grow the book without bound and make the bias drift for a reason that says nothing about
/// the boundary.
///
/// Sequence numbers are contiguous from the snapshot, because `run_ticks` folds a `Delta` only
/// when `L2Book::delta_decision` says Apply under `SeqPolicy::Strict` (`seq == last_seq + 1`) and
/// DROPS the book otherwise — a gap here would silently stop delivering `on_order_book` at all.
fn tick_series() -> Vec<Tick> {
    fn book(ts: i64, seq: u64, kind: BookUpdateKind, bids: Vec<BookLevel>) -> Tick {
        Tick::Book(BookUpdate {
            ts,
            local_ts: 0,
            seq,
            kind,
            tick_size: 0.5,
            bids,
            asks: if matches!(kind, BookUpdateKind::Snapshot) {
                vec![BookLevel::new(100.0, 6.0), BookLevel::new(100.5, 4.0)]
            } else {
                Vec::new()
            },
            symbol: SYMBOL.to_string(),
        })
    }

    let mut seed: u64 = 0x1234_5678_9ABC_DEF0;
    let mut price: f64 = 100.0;
    let mut ts = 1_700_000_000_000i64;
    let mut seq = 1u64;
    let mut out: Vec<Tick> = Vec::with_capacity(N_TICKS + N_TICKS / BOOK_EVERY + 1);
    // Anchor the book BEFORE any priced tick, so `book_bias` is already real by the time the
    // fixture sizes its first order rather than being zero for the first stretch of the run.
    out.push(book(
        ts,
        seq,
        BookUpdateKind::Snapshot,
        vec![BookLevel::new(99.5, 10.0), BookLevel::new(99.0, 5.0), BookLevel::new(98.5, 7.0)],
    ));
    for i in 0..N_TICKS {
        ts += 100;
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        // Same integer-arithmetic walk as `bar_series`: whole hundredths, one division, no
        // platform transcendental, so the tape is bit-identical on every box.
        let step = ((seed >> 33) % 401) as f64 / 100.0 - 2.0;
        price = (price + step).max(5.0);
        if i.is_multiple_of(2) {
            out.push(Tick::Quote(QuoteTick {
                ts,
                local_ts: 0,
                bid: price - 0.05,
                ask: price + 0.05,
                bid_size: 1.0,
                ask_size: 1.0,
                symbol: SYMBOL.to_string(),
            }));
        } else {
            out.push(Tick::Trade(TradeTick {
                ts,
                local_ts: 0,
                price,
                size: 0.5,
                is_buyer_maker: i % 4 == 1,
                symbol: SYMBOL.to_string(),
            }));
        }
        if i % BOOK_EVERY == BOOK_EVERY - 1 {
            ts += 1;
            seq += 1;
            // Re-price the SAME best-bid level with a different resting size — the one input
            // `on_order_book` feeds into the fixture's order sizing.
            let qty = 4.0 + (i % 7) as f64;
            out.push(book(ts, seq, BookUpdateKind::Delta, vec![BookLevel::new(99.5, qty)]));
        }
    }
    out
}

/// Which engine lane a comparison is run down. Both drive the SAME fixture instance type through
/// the SAME `StrategyEngine`; they differ in which `Strategy` hooks the engine emits, which is
/// exactly what makes two lanes worth having.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lane {
    /// `StrategyEngine::run` — emits `warmup`, `on_start`, `on_bar`, `on_schedule`, `on_fill`,
    /// `on_stop`.
    Bars,
    /// `StrategyEngine::run_ticks` — emits `warmup`, `on_start`, `on_quote_tick`, `on_trade_tick`,
    /// `on_order_book`, `on_fill`, `on_stop`.
    Ticks,
}

impl Lane {
    fn label(self) -> &'static str {
        match self {
            Lane::Bars => "bar lane (run)",
            Lane::Ticks => "tick lane (run_ticks)",
        }
    }
}

/// What one run produced: the ordinary result, PLUS the engine's still-working orders.
///
/// ⚠ **`pending` is here because `on_stop` is otherwise unobservable.** The engine calls it, but a
/// `BacktestResult` carries no trace of anything it did: the run is over, so an order submitted
/// there can never fill and never becomes a `Trade`. It does land in `SimBroker`'s per-symbol
/// working-order list, which outlives `run`, so reading it there is what turns `on_stop` from a
/// hook with a narrower witness into one the bit-for-bit comparison actually covers.
struct Run {
    result: BacktestResult,
    pending: Vec<WorkingOrder>,
}

/// Drive one strategy down one lane. The SAME function for both mechanisms — a compiled-in
/// strategy and a `PluginStrategy` are both just `S: Strategy<SimBroker>` here, which is the
/// property the whole comparison rests on.
fn drive<S: Strategy<SimBroker>>(strategy: S, lane: Lane) -> Run {
    let bars = match lane {
        Lane::Bars => universe(),
        // `run_ticks` needs a symbol SLOT, not a bar series — the tape is the input. An empty
        // series is the shape `crates/vike-backtest/tests/book_replay.rs` drives it with.
        Lane::Ticks => vec![(SYMBOL.to_string(), Vec::new())],
    };
    let mut engine = StrategyEngine::new(bars, strategy, engine_params());
    // Registered on the ENGINE, not by the strategy: `Schedule::on` is an inherent `SimBroker`
    // verb, so a portable `impl<B: Broker> Strategy<B>` — which the fixture is, and must stay, to
    // be a real user strategy — cannot reach it. Both mechanisms get the identical registration
    // because both go through this one function.
    engine.core.schedule.on(DateRule::every_n_bars(SCHEDULE_EVERY_N), ma_cross::REBALANCE_TAG);
    let result = match lane {
        Lane::Bars => engine.run(),
        Lane::Ticks => engine.run_ticks(&[(SYMBOL.to_string(), tick_series())]),
    };
    let pending = engine.core.sym.iter().flat_map(|s| s.pending.clone()).collect();
    Run { result, pending }
}

/// PATH 1 — the build-time tier's mechanism: a direct Rust call into a module of this binary.
fn run_compiled(lane: Lane) -> Run {
    run_compiled_with(PARAMS_TOML, lane)
}

/// The same path under an explicit params document. Used by the differential probes below, which
/// need two runs of the COMPILED half that differ in exactly one params key — no cargo, no
/// plugin, and therefore cheap enough to gate every PR.
fn run_compiled_with(params_toml: &str, lane: Lane) -> Run {
    let params: toml::Value =
        toml::from_str(params_toml).expect("fixture params must be valid TOML");
    // The entry contract returns `+ Send`; the engine's bound is the auto-trait-free object, and
    // dropping an auto trait from a trait object is an ordinary unsizing coercion.
    let strategy: Box<dyn Strategy<SimBroker>> = ma_cross::build::<SimBroker>(&params);
    drive(strategy, lane)
}

/// PATH 2 — the plugin mechanism: the real builder, the real loader, the real host wrapper.
fn run_plugin(so: &Path, lane: Lane) -> Run {
    let strategy = PluginStrategy::<SimBroker>::new(load_or_panic(so).vtable, PARAMS_TOML);
    drive(strategy, lane)
}

fn load_or_panic(so: &Path) -> loader::LoadedPlugin {
    loader::load(so).unwrap_or_else(|e| {
        panic!(
            "the loader refused the artifact the builder just produced at {}: {e}\n\
             (a FingerprintMismatch here means the host and the plugin were built under \
             different profiles — see this file's module doc; a NoSymbol here means the template \
             did not export a dispatch symbol `loader::load` binds, which at ABI_VERSION 3 is \
             every hook in `vike_strategy_plugin::host::WIRED_HOOKS`)",
            so.display()
        )
    })
}

/// Build the fixture into a real `.so` through the production builder. Returns the artifact path.
fn build_fixture_plugin() -> PathBuf {
    build_through_the_production_builder(FIXTURE_SOURCE, "ma_cross")
}

/// Build [`FLOOR_SOURCE`] the same way, through the same builder, into the same output directory.
///
/// ⚠ **Called from inside the one test that already builds**, never from a `#[test]` of its own —
/// under `nextest` a second test is a second PROCESS, and two processes racing a cold
/// `build_plugin` share one scratch target directory. Same rule `assert_live_only_hooks_reach_the_plugin`
/// states at its own definition.
fn build_floor_plugin() -> PathBuf {
    build_through_the_production_builder(FLOOR_SOURCE, "floor")
}

/// The one `build_plugin` call site: profile, output directory, checkout root and cargo, resolved
/// once so the fixture and the floor artifact are produced under byte-identical conditions and
/// their sizes are therefore comparable to each other as well as across runs.
fn build_through_the_production_builder(source: &str, name: &str) -> PathBuf {
    // `CARGO_TARGET_TMPDIR` is Cargo's own scratch directory under this checkout's `target/`, the
    // same one `tests/load_refusals.rs` caches its fixtures in. It is not the shared system
    // scratch directory, and nothing in this file builds a path under that one.
    //
    // ⚠ That last sentence is worded the way it is ON PURPOSE. It used to name the std accessor
    // outright, and `crates/vike-ops/tests/temp_path_gate.rs` matches that accessor as RAW TEXT —
    // comments included — and then reads the next `.join("literal")` it finds up to the following
    // `;`. So a comment saying "this is NOT that directory" made the gate read the line below as
    // a fixed-name scratch directory under it and reject it. Measured on this branch's first gate
    // run. Do not reintroduce the name here.
    //
    // The directory is keyed on `render::artifact_dir_tag()` — the ABI version and the whole
    // toolchain fingerprint, i.e. exactly what `loader::load` compares — because `build_plugin`'s
    // artifact name carries the source sha and NEITHER. Without it a run after a bump (of the
    // ABI, of rustc, of the profile, of the target) is handed the PREVIOUS artifact and refused
    // on `AbiMismatch`/`FingerprintMismatch`.
    //
    // ⚠ Not hypothetical and not caught by reasoning: it is what a warm `CARGO_TARGET_TMPDIR` did
    // to `tests/load_refusals.rs`'s two `build_plugin` tests when `ABI_VERSION` went 2 -> 3, and
    // then again to `crates/vike-studio-core/tests/plugin_run.rs` in CI. This test escaped the
    // first time only because the fixture SOURCE changed in the same commit, so its sha changed
    // too — i.e. by luck, not by design. The rule is a LIBRARY function now, so there is ONE
    // spelling rather than one per crate; `render::artifact_dir_tag`'s own doc carries the
    // finding and says why production deliberately does not use it.
    let profile = Profile::of_this_binary();
    let out_dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("equivalence-plugin-{}", artifact_dir_tag()));
    // Two levels up from this crate's manifest directory is the checkout root, which is what the
    // rendered scratch crate's `path` dependencies resolve against.
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    // The exact cargo running this test, so the nested build uses the same toolchain rather than
    // whatever is first on PATH. `Layer::TestOnly`; the `(CARGO, vike-strategy-plugin)` row in
    // `vike_ops::settings::SETTINGS` already declares this crate's reads.
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    build_plugin(source, name, &out_dir, &workspace_root, &cargo, profile).unwrap_or_else(|e| {
        panic!("the builder must produce a plugin from the `{name}` source:\n{e}")
    })
}

/// A directory-name-safe tag for a profile. A `Debug` formatting of the enum would do, but
/// spelling it keeps the directory name stable if that derive ever changes.
fn profile_tag(p: Profile) -> &'static str {
    match p {
        Profile::Debug => "debug",
        Profile::Release => "release",
    }
}

// ---------------------------------------------------------------------------------------------
// Comparison — field by field, value by value, bit for bit
// ---------------------------------------------------------------------------------------------

/// `None` when the two `f64`s are the SAME BIT PATTERN, a described difference otherwise.
///
/// Bits rather than `==` for two reasons this tree already relies on
/// (`crates/vike-backtest/src/engine.rs`'s own cross-order equality tests do the same): `==` calls
/// two `NaN`s different when they are the identical computed answer, and it calls `0.0` and
/// `-0.0` the same when they are not. Neither tolerance nor rounding appears anywhere in this
/// file on purpose.
fn f64_diff(label: &str, compiled: f64, plugin: f64) -> Option<String> {
    if compiled.to_bits() == plugin.to_bits() {
        return None;
    }
    Some(format!(
        "{label}: compiled {compiled:?} (bits {:#018x}) vs plugin {plugin:?} (bits {:#018x}), \
         difference {:?}",
        compiled.to_bits(),
        plugin.to_bits(),
        plugin - compiled
    ))
}

/// Every difference between the two trade lists, described — not the first one, and never a
/// length check standing in for a comparison. `assert_eq!(a.len(), b.len())` passes for two runs
/// that traded completely differently, which is the failure mode this function exists to refuse.
fn trade_diffs(compiled: &[Trade], plugin: &[Trade]) -> Vec<String> {
    let mut out = Vec::new();
    if compiled.len() != plugin.len() {
        out.push(format!("trade COUNT: compiled {} vs plugin {}", compiled.len(), plugin.len()));
    }
    for (i, (c, p)) in compiled.iter().zip(plugin.iter()).enumerate() {
        for (name, cv, pv) in [
            ("entry_price", c.entry_price, p.entry_price),
            ("exit_price", c.exit_price, p.exit_price),
            ("size", c.size, p.size),
            ("pnl", c.pnl, p.pnl),
            ("fees", c.fees, p.fees),
            ("mae", c.mae, p.mae),
            ("mfe", c.mfe, p.mfe),
        ] {
            if let Some(d) = f64_diff(&format!("trade[{i}].{name}"), cv, pv) {
                out.push(d);
            }
        }
        if c.entry_ts != p.entry_ts {
            out.push(format!(
                "trade[{i}].entry_ts: compiled {} vs plugin {}",
                c.entry_ts, p.entry_ts
            ));
        }
        if c.exit_ts != p.exit_ts {
            out.push(format!("trade[{i}].exit_ts: compiled {} vs plugin {}", c.exit_ts, p.exit_ts));
        }
        if c.symbol != p.symbol {
            out.push(format!(
                "trade[{i}].symbol: compiled {:?} vs plugin {:?}",
                c.symbol, p.symbol
            ));
        }
        if c.is_long != p.is_long {
            out.push(format!("trade[{i}].is_long: compiled {} vs plugin {}", c.is_long, p.is_long));
        }
    }
    out
}

/// The metric catalog computed over a result — the numbers an operator reads off a run, as
/// VALUES rather than as a count of them.
fn metric_values(r: &BacktestResult) -> Vec<(&'static str, f64)> {
    let eq = &r.equity_curve;
    let tr = &r.trades;
    vec![
        ("final_equity", r.final_equity),
        ("total_return", metrics::total_return(eq)),
        ("max_drawdown", metrics::max_drawdown(eq)),
        ("sharpe", metrics::sharpe(eq, PERIODS_PER_YEAR)),
        ("sortino", metrics::sortino(eq, PERIODS_PER_YEAR)),
        ("calmar", metrics::calmar(eq, PERIODS_PER_YEAR)),
        ("cagr", metrics::cagr(eq, PERIODS_PER_YEAR)),
        ("mar_ratio", metrics::mar_ratio(eq, PERIODS_PER_YEAR)),
        ("omega", metrics::omega(eq, 0.0)),
        ("ulcer_index", metrics::ulcer_index(eq)),
        ("ulcer_performance_index", metrics::ulcer_performance_index(eq, PERIODS_PER_YEAR)),
        ("recovery_factor", metrics::recovery_factor(eq)),
        ("risk_return_ratio", metrics::risk_return_ratio(eq)),
        ("returns_volatility", metrics::returns_volatility(eq, PERIODS_PER_YEAR)),
        ("returns_skewness", metrics::returns_skewness(eq)),
        ("returns_kurtosis", metrics::returns_kurtosis(eq)),
        ("tail_ratio", metrics::tail_ratio(eq)),
        ("value_at_risk", metrics::value_at_risk(eq, 0.95)),
        ("expected_shortfall", metrics::expected_shortfall(eq, 0.95)),
        ("k_ratio", metrics::k_ratio(eq)),
        ("win_rate", metrics::win_rate(tr)),
        ("profit_factor", metrics::profit_factor(tr)),
        ("net_profit", metrics::net_profit(tr)),
        ("gross_profit", metrics::gross_profit(tr)),
        ("gross_loss", metrics::gross_loss(tr)),
        ("total_fees", metrics::total_fees(tr)),
        ("expected_payoff", metrics::expected_payoff(tr)),
        ("largest_win", metrics::largest_win(tr)),
        ("largest_loss", metrics::largest_loss(tr)),
        ("avg_win", metrics::avg_win(tr)),
        ("avg_loss", metrics::avg_loss(tr)),
        ("payoff_ratio", metrics::payoff_ratio(tr)),
        ("long_ratio", metrics::long_ratio(tr)),
        ("sqn", metrics::sqn(tr)),
    ]
}

fn metric_diffs(compiled: &BacktestResult, plugin: &BacktestResult) -> Vec<String> {
    let (a, b) = (metric_values(compiled), metric_values(plugin));
    let mut out = Vec::new();
    for ((name, cv), (_, pv)) in a.into_iter().zip(b) {
        if let Some(d) = f64_diff(&format!("metric {name}"), cv, pv) {
            out.push(d);
        }
    }
    out
}

/// Everything on the result that is NOT a trade or a metric: the equity curve point by point,
/// the timestamps, and the engine's own diagnostic counters. A strategy that traded identically
/// but was fed a different bar index would show up here first.
fn result_shape_diffs(compiled: &BacktestResult, plugin: &BacktestResult) -> Vec<String> {
    let mut out = Vec::new();
    if compiled.n_trades != plugin.n_trades {
        out.push(format!("n_trades: compiled {} vs plugin {}", compiled.n_trades, plugin.n_trades));
    }
    if compiled.equity_curve.len() != plugin.equity_curve.len() {
        out.push(format!(
            "equity_curve LENGTH: compiled {} vs plugin {}",
            compiled.equity_curve.len(),
            plugin.equity_curve.len()
        ));
    }
    for (i, (c, p)) in compiled.equity_curve.iter().zip(&plugin.equity_curve).enumerate() {
        if let Some(d) = f64_diff(&format!("equity_curve[{i}]"), *c, *p) {
            out.push(d);
        }
    }
    // Element by element with its index, not a whole-vector `!=`. A bare inequality was the one
    // detail-free message left in this file: it said the timestamps differed and nothing about
    // WHERE or BY HOW MUCH, which is the shape of report this whole comparison exists to avoid.
    if compiled.equity_ts.len() != plugin.equity_ts.len() {
        out.push(format!(
            "equity_ts LENGTH: compiled {} vs plugin {}",
            compiled.equity_ts.len(),
            plugin.equity_ts.len()
        ));
    }
    for (i, (c, p)) in compiled.equity_ts.iter().zip(&plugin.equity_ts).enumerate() {
        if c != p {
            out.push(format!("equity_ts[{i}]: compiled {c} vs plugin {p} (difference {})", p - c));
        }
    }
    if compiled.per_symbol_pnl.len() != plugin.per_symbol_pnl.len() {
        out.push("per_symbol_pnl LENGTH differs".to_string());
    }
    for (i, ((cs, cv), (ps, pv))) in
        compiled.per_symbol_pnl.iter().zip(&plugin.per_symbol_pnl).enumerate()
    {
        if cs != ps {
            out.push(format!("per_symbol_pnl[{i}] symbol: compiled {cs:?} vs plugin {ps:?}"));
        }
        if let Some(d) = f64_diff(&format!("per_symbol_pnl[{i}] value"), *cv, *pv) {
            out.push(d);
        }
    }
    for (name, c, p) in [
        ("stale_deferrals", compiled.stale_deferrals, plugin.stale_deferrals),
        ("session_deferrals", compiled.session_deferrals, plugin.session_deferrals),
        ("impact_unpriced", compiled.impact_unpriced, plugin.impact_unpriced),
    ] {
        if c != p {
            out.push(format!("{name}: compiled {c} vs plugin {p}"));
        }
    }
    // Field by field, like everything else. This was a LENGTH check, which would have passed two
    // runs whose orders were refused for entirely different reasons — and the gate reason is
    // exactly the interesting part of a dropped order.
    if compiled.dropped.len() != plugin.dropped.len() {
        out.push(format!(
            "dropped ORDER COUNT: compiled {} vs plugin {}",
            compiled.dropped.len(),
            plugin.dropped.len()
        ));
    }
    for (i, ((cs, cr, csz, cw), (ps, pr, psz, pw))) in
        compiled.dropped.iter().zip(&plugin.dropped).enumerate()
    {
        if cs != ps {
            out.push(format!("dropped[{i}].symbol: compiled {cs:?} vs plugin {ps:?}"));
        }
        if cr != pr {
            out.push(format!("dropped[{i}].reason: compiled {cr:?} vs plugin {pr:?}"));
        }
        if let Some(d) = f64_diff(&format!("dropped[{i}].size"), *csz, *psz) {
            out.push(d);
        }
        if let Some(d) = f64_diff(&format!("dropped[{i}].weight"), *cw, *pw) {
            out.push(d);
        }
    }
    out
}

/// The engine's still-working orders, compared field by field — the channel `on_stop` reaches.
///
/// Field by field rather than by `PartialEq` on the whole vector (which `WorkingOrder` does
/// derive) for the reason every other comparison in this file is: `assert_eq!` on two vectors of
/// f64-carrying structs reports "not equal" and nothing about WHERE, and an f64 `==` calls two
/// NaNs different and `0.0`/`-0.0` the same. The bit comparison is `f64_diff`'s, shared with the
/// trade and metric comparisons.
fn pending_diffs(compiled: &[WorkingOrder], plugin: &[WorkingOrder]) -> Vec<String> {
    let mut out = Vec::new();
    if compiled.len() != plugin.len() {
        out.push(format!(
            "pending ORDER COUNT: compiled {} vs plugin {}",
            compiled.len(),
            plugin.len()
        ));
    }
    for (i, (c, p)) in compiled.iter().zip(plugin.iter()).enumerate() {
        if c.kind != p.kind {
            out.push(format!("pending[{i}].kind: compiled {:?} vs plugin {:?}", c.kind, p.kind));
        }
        if c.side != p.side {
            out.push(format!("pending[{i}].side: compiled {} vs plugin {}", c.side, p.side));
        }
        if let Some(d) = f64_diff(&format!("pending[{i}].size"), c.size, p.size) {
            out.push(d);
        }
        // `Option<f64>` compared as a pair: PRESENCE first (a `None` and a `Some` are not a bit
        // difference), then the bits when both are present.
        for (name, cv, pv) in
            [("price", c.price, p.price), ("trail", c.trail, p.trail), ("stop", c.stop, p.stop)]
        {
            match (cv, pv) {
                (Some(a), Some(b)) => {
                    if let Some(d) = f64_diff(&format!("pending[{i}].{name}"), a, b) {
                        out.push(d);
                    }
                }
                (a, b) if a.is_some() != b.is_some() => out.push(format!(
                    "pending[{i}].{name}: compiled {a:?} vs plugin {b:?} (presence differs)"
                )),
                _ => {}
            }
        }
        if let Some(d) = f64_diff(&format!("pending[{i}].weight"), c.weight, p.weight) {
            out.push(d);
        }
    }
    out
}

/// The comparison is only worth anything if the fixture actually did something. Asserted on the
/// COMPILED side, before the two are compared: if this run traded nothing, an equality that holds
/// proves nothing about either mechanism.
fn assert_not_vacuous(run: &Run, lane: Lane) {
    let r = &run.result;
    let where_ = lane.label();
    match lane {
        Lane::Bars => assert_eq!(
            r.equity_curve.len(),
            N_BARS,
            "{where_}: the engine must have folded every bar — a short curve means the run \
             stopped early"
        ),
        // One equity sample per PRICED tick under the default `EquitySampling::EveryTick`; a
        // `Tick::Book` bumps neither the index nor the curve, which is what this asserts.
        Lane::Ticks => assert_eq!(
            r.equity_curve.len(),
            N_TICKS,
            "{where_}: the engine must have folded every priced tick and no book event"
        ),
    }
    // A FLOOR, not a golden — it exists so a fixture edit that quietly stopped the run from
    // trading fails here instead of making the comparison below vacuously true, and it is set well
    // BELOW what the run actually produces so it never becomes a number somebody rebaselines.
    // MEASURED on the latency box lane1, debug, 2026-09-22: 17 closed trades on the bar lane, 29 on the tick
    // lane. The bar lane's eight is the number this test has asserted since it was written.
    let floor = match lane {
        Lane::Bars => 8,
        Lane::Ticks => 12,
    };
    assert!(
        r.n_trades >= floor,
        "{where_}: the fixture must trade repeatedly for this comparison to mean anything, got \
         {} closed trades (floor {floor})",
        r.n_trades
    );
    assert!(
        r.trades.iter().any(|t| t.is_long),
        "{where_}: the fixture must open at least one LONG — a one-sided run would not exercise \
         the sign change this comparison is built around"
    );
    assert!(
        r.trades.iter().any(|t| !t.is_long),
        "{where_}: the fixture must open at least one SHORT — see above"
    );
    assert!(
        r.trades.iter().any(|t| t.pnl > 0.0) && r.trades.iter().any(|t| t.pnl < 0.0),
        "{where_}: the fixture must produce both winning and losing trades, so the trade-keyed \
         metrics (profit_factor, avg_win/avg_loss, payoff_ratio) are computed over real inputs \
         rather than over an empty branch"
    );
    // ...and the HOOK-specific half. Each of these is the observable an override was written to
    // move, so a comparison that passed while the hook did nothing is refused HERE rather than
    // reported as agreement.
    assert!(
        r.n_trades > 0,
        "{where_}: `on_start` never armed the strategy — it submits nothing until it fires, so a \
         zero-trade run is what an undelivered `on_start` looks like on BOTH mechanisms at once, \
         which the comparison itself could never catch"
    );
    let parting =
        run.pending.iter().find(|o| o.kind == vike_model::OrderKind::Limit).unwrap_or_else(|| {
            panic!(
                "{where_}: `on_stop` left no resting LIMIT in the engine's pending list. That \
                 order is the ONLY trace `on_stop` can leave — the run is over, so it can never \
                 fill and never becomes a Trade — and without it the pending comparison below is \
                 vacuous."
            )
        });
    assert_eq!(
        parting.price,
        Some(ma_cross::ON_STOP_PRICE),
        "{where_}: the parting order must rest at the price the fixture named"
    );
    assert!(
        parting.size > 1.0,
        "{where_}: the parting order's size is `1.0 + the fill count`, so a size of exactly 1.0 \
         means `on_fill` never reached the strategy even though the run traded — the one \
         observable that separates a delivered fill from an undelivered one on the compiled side \
         alone"
    );
    assert_the_engine_emits_the_state_dependent_hooks(lane);
}

/// The two hooks whose delivery depends on ENGINE STATE rather than on the fixture, witnessed
/// against the engine itself.
///
/// ⚠ **This is the hole the rest of `assert_not_vacuous` did not cover, and it is the one that
/// matters most.** `on_start`, `on_fill` and `on_stop` are probed above through observables the
/// FIXTURE produces. `on_schedule` and `on_order_book` are not like them: whether they arrive at
/// all is decided by state this file sets up and the engine then judges —
/// `StrategyEngine::run` fires `on_schedule` only for tags `Schedule::check_due` returns, and
/// `run_ticks` fires `on_order_book` only when `apply_book_event` reports `applied`, which under
/// `SeqPolicy::Strict` means every delta's `seq` was exactly `last_seq + 1`. A seq gap
/// introduced into [`tick_series`] drops the book and STOPS DELIVERY SILENTLY; a schedule rule
/// registered with the wrong cadence, or a warm-up that swallowed every due bar, does the same
/// for tags.
///
/// **And the bit-for-bit comparison cannot see either.** A hook that never arrives does not arrive
/// on EITHER mechanism, so both fall to identical behaviour and the comparison agrees perfectly —
/// green, with the hook unexercised. That is exactly the family this project has produced five
/// times: a test performing for the system the very step it was meant to witness.
///
/// So the claim is made where it can be MEASURED: the REAL fixture, wrapped in a counting
/// decorator, driven through the SAME [`drive`] the comparison uses — same engine, same params,
/// same tape, same registration — reporting what the engine actually emitted. No cargo, no
/// plugin; it gates in an ordinary run.
///
/// ⚠ **It WRAPS the fixture rather than standing in for it, and the first version did not.** A
/// passive spy that overrides only the hooks it counts submits no orders, so the engine has
/// nothing to fill and `on_fill` never fires — the probe's own fill assertions then measured a
/// strategy that could not trade, and said so: `0 buy / 0 sell` on its first real run. Every hook
/// is forwarded to the inner strategy, `warmup` included, so the run these counters describe is
/// the run the comparison compares.
///
/// The book half asserts a NON-ZERO bias rather than a delivery count, because a delivered book
/// the fixture reads as `0.0` changes no order and is worth exactly as little as no book at all.
fn assert_the_engine_emits_the_state_dependent_hooks(lane: Lane) {
    struct HookSpy {
        inner: Box<dyn Strategy<SimBroker>>,
        schedule_fires: std::rc::Rc<std::cell::Cell<usize>>,
        books_with_a_nonzero_bias: std::rc::Rc<std::cell::Cell<usize>>,
        buy_fills: std::rc::Rc<std::cell::Cell<usize>>,
        sell_fills: std::rc::Rc<std::cell::Cell<usize>>,
        filled_units: std::rc::Rc<std::cell::Cell<f64>>,
        fills_below_the_ladder: std::rc::Rc<std::cell::Cell<usize>>,
        fills_above_the_ladder: std::rc::Rc<std::cell::Cell<usize>>,
    }
    impl Strategy<SimBroker> for HookSpy {
        // ---- forwarded UNCHANGED, so the inner strategy runs exactly as it does in the
        // comparison. `warmup` above all: answering `0` here would open the R2 gate at a
        // different bar and make every count describe a different run.
        fn warmup(&self) -> usize {
            self.inner.warmup()
        }
        fn on_start(&mut self, b: &mut SimBroker) {
            self.inner.on_start(b);
        }
        fn on_bar(&mut self, b: &mut SimBroker, bar: &Bar) {
            self.inner.on_bar(b, bar);
        }
        fn on_quote_tick(&mut self, b: &mut SimBroker, q: &QuoteTick) {
            self.inner.on_quote_tick(b, q);
        }
        fn on_trade_tick(&mut self, b: &mut SimBroker, t: &TradeTick) {
            self.inner.on_trade_tick(b, t);
        }
        fn on_stop(&mut self, b: &mut SimBroker) {
            self.inner.on_stop(b);
        }

        // ---- counted, THEN forwarded ----
        fn on_schedule(&mut self, b: &mut SimBroker, tag: &str) {
            if tag == ma_cross::REBALANCE_TAG {
                self.schedule_fires.set(self.schedule_fires.get() + 1);
            }
            self.inner.on_schedule(b, tag);
        }
        fn on_order_book(&mut self, b: &mut SimBroker, book: &vike_model::L2Book) {
            // The fixture's OWN expression, called rather than re-typed — see its doc.
            if ma_cross::top_of_book_bias(book) != 0.0 {
                self.books_with_a_nonzero_bias.set(self.books_with_a_nonzero_bias.get() + 1);
            }
            self.inner.on_order_book(b, book);
        }
        fn on_fill(&mut self, b: &mut SimBroker, fill: &vike_model::Fill) {
            // ⚠ The SAME guard the fixture applies, applied before counting anything — because
            // the assertions below name the FIXTURE's accumulator as their subject, and counters
            // that accepted a fill the fixture skips would describe a different number under that
            // name. Immaterial against `SimBroker`, which emits no zero-size or side-0 fill; the
            // point is that the message and the measurement have one subject rather than two.
            // The fill is FORWARDED either way — the fixture does its own guarding, and skipping
            // the forward would make the spy's run diverge from the comparison's.
            if fill.size <= 0.0 || fill.side == 0 {
                self.inner.on_fill(b, fill);
                return;
            }
            if fill.side > 0 {
                self.buy_fills.set(self.buy_fills.get() + 1);
            } else {
                self.sell_fills.set(self.sell_fills.get() + 1);
            }
            // The RUNG the fixture's `size_step` takes for this fill, recomputed the way the
            // fixture computes it — the running total AFTER this fill, compared to the ladder.
            let units = self.filled_units.get() + fill.size;
            self.filled_units.set(units);
            if units > ma_cross::FILL_UNITS_LADDER {
                self.fills_above_the_ladder.set(self.fills_above_the_ladder.get() + 1);
            } else {
                self.fills_below_the_ladder.set(self.fills_below_the_ladder.get() + 1);
            }
            self.inner.on_fill(b, fill);
        }
    }

    let params: toml::Value =
        toml::from_str(PARAMS_TOML).expect("fixture params must be valid TOML");
    let spy = HookSpy {
        inner: ma_cross::build::<SimBroker>(&params),
        schedule_fires: std::rc::Rc::default(),
        books_with_a_nonzero_bias: std::rc::Rc::default(),
        buy_fills: std::rc::Rc::default(),
        sell_fills: std::rc::Rc::default(),
        filled_units: std::rc::Rc::default(),
        fills_below_the_ladder: std::rc::Rc::default(),
        fills_above_the_ladder: std::rc::Rc::default(),
    };
    let (fires, biased_books) = (
        std::rc::Rc::clone(&spy.schedule_fires),
        std::rc::Rc::clone(&spy.books_with_a_nonzero_bias),
    );
    let (buys, sells, units) = (
        std::rc::Rc::clone(&spy.buy_fills),
        std::rc::Rc::clone(&spy.sell_fills),
        std::rc::Rc::clone(&spy.filled_units),
    );
    let (below, above) = (
        std::rc::Rc::clone(&spy.fills_below_the_ladder),
        std::rc::Rc::clone(&spy.fills_above_the_ladder),
    );
    drive(spy, lane);
    let where_ = lane.label();

    // ...and the same treatment for `on_fill`'s other two reads, which are otherwise backed only
    // by the fixture's own comment — the exact shape that comment was corrected FOR.
    // `assert_not_vacuous` already proves a fill ARRIVED (the parting order's size exceeds 1.0);
    // these two prove the SIDE and the SIZE reads DISCRIMINATE rather than being constants
    // wearing a field's clothes.
    assert!(
        buys.get() > 0 && sells.get() > 0,
        "{where_}: the run filled on only ONE side ({} buy / {} sell), so the fixture's SIDE read \
         takes the same branch all run and a mirror that crossed `side` inverted would change \
         nothing",
        buys.get(),
        sells.get()
    );
    // ⚠ BOTH directions, because "never crosses" and "crossed on the FIRST fill" are the same
    // always-one-branch vacuity mirrored — and the first version of this assertion checked only
    // the former. A ladder every fill sits past is exactly as constant as one nothing reaches,
    // and a mirror that halved every size would change nothing under either. The PARTWAY property
    // is the whole claim, so it is asserted from the counters rather than computed in a comment:
    // arithmetic in a doc comment is what `16f7e4cdb` corrected one assertion to the left.
    assert!(
        below.get() > 0 && above.get() > 0,
        "{where_}: every fill took the SAME rung of `FILL_UNITS_LADDER` ({}) — {} below, {} \
         above, {} units filled in total. The fixture's size-MAGNITUDE read is then a constant \
         and a mirror that halved every size would change nothing. The ladder must sit PARTWAY \
         through the run: a threshold nothing reaches and a threshold everything passes on its \
         first fill are the same defect.",
        ma_cross::FILL_UNITS_LADDER,
        below.get(),
        above.get(),
        units.get()
    );
    match lane {
        Lane::Bars => {
            assert!(
                fires.get() > 0,
                "{where_}: the engine emitted NO `on_schedule` for `{}`. The fixture's override is \
                 then dead code on both mechanisms and the comparison agrees about nothing. Check \
                 the rule this file registers in `drive` against `StrategyEngine::run`'s warm-up \
                 gate — `check_due` is only consulted once `index >= warmup`.",
                ma_cross::REBALANCE_TAG
            );
            assert_eq!(
                biased_books.get(),
                0,
                "{where_}: the BAR lane emitted an `on_order_book`, which it has no call site for \
                 — this probe's lane split is wrong, or the engine grew one"
            );
        }
        Lane::Ticks => {
            assert!(
                biased_books.get() > 0,
                "{where_}: the engine delivered no book with a NON-ZERO top-of-book bias. Either \
                 `on_order_book` is not being emitted at all — `apply_book_event` drops the book \
                 on any `seq` that is not `last_seq + 1` under `SeqPolicy::Strict`, and stops \
                 delivering SILENTLY — or every delivered book had equal size on both sides, which \
                 the fixture reads as 0.0 and sizes no differently for. Either way its override is \
                 dead code on both mechanisms."
            );
            assert_eq!(
                fires.get(),
                0,
                "{where_}: the TICK lane emitted an `on_schedule`, which `run_ticks` has no call \
                 site for — this probe's lane split is wrong, or the engine grew one"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The tests
// ---------------------------------------------------------------------------------------------

/// THE test. Un-ignored — see this file's module doc for what used to stop that and how it was
/// removed rather than worked around.
///
/// ⚠ **It is the ONLY caller of [`build_fixture_plugin`], deliberately.** Under `cargo nextest`
/// every test is its own PROCESS, and two processes racing one `build_plugin` call on a cold
/// cache would share a scratch target directory whose cdylib the winner `rename`s away before
/// the loser looks for it. One caller means no race by construction; a second test wanting an
/// artifact should take one built here rather than build its own into the same directory.
#[test]
fn the_two_mechanisms_produce_identical_trades_and_metrics() {
    let so = build_fixture_plugin();
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
        let handle = (probe.vtable.create)(PARAMS_TOML.as_ptr(), PARAMS_TOML.len());
        assert!(
            !handle.is_null(),
            "`vike_plugin_create` returned NULL for a well-formed params document \
             ({PARAMS_TOML:?}). Every dispatch will report `PluginStatus::BadHandle` and the \
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
        let plugin = run_plugin(&so, lane);

        let where_ = lane.label();
        let mut lane_diffs = result_shape_diffs(&compiled.result, &plugin.result);
        lane_diffs.extend(trade_diffs(&compiled.result.trades, &plugin.result.trades));
        lane_diffs.extend(metric_diffs(&compiled.result, &plugin.result));
        lane_diffs.extend(pending_diffs(&compiled.pending, &plugin.pending));
        diffs.extend(lane_diffs.into_iter().map(|d| format!("[{where_}] {d}")));

        measured.push(format!(
            "MEASURED {where_}: {} closed trades, {} pending order(s) after on_stop, final \
             equity {:?} (compiled) / {:?} (plugin)",
            compiled.result.n_trades,
            compiled.pending.len(),
            compiled.result.final_equity,
            plugin.result.final_equity
        ));
    }

    // Reported BEFORE the assertion so the numbers land in the run log whether it passes or
    // fails: the design's open sizing question is answered from exactly this artifact, the one
    // the equivalence verdict above was produced from.
    // ⚠ NAMES THE PROFILE, because this test is un-ignored now and therefore usually runs in
    // DEBUG — where the artifact is several times the size of the one a deployed builder
    // produces. The figure recorded in the design doc is the RELEASE one; a debug number read off
    // this line and filed as "the plugin size" would be wrong by a multiple.
    println!(
        "MEASURED plugin artifact ({} profile): {bytes} bytes ({:.1} KiB) at {}",
        profile_tag(Profile::of_this_binary()),
        bytes as f64 / 1024.0,
        so.display()
    );
    // ...and the FLOOR, from the same builder in the same profile into the same directory. See
    // `FLOOR_SOURCE`: this is the figure that moves when the TEMPLATE's dependency set changes
    // and a user file names none of the new crates, which is the only way to price mere
    // AVAILABILITY apart from use.
    let floor = build_floor_plugin();
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

    assert!(
        diffs.is_empty(),
        "THE TWO MECHANISMS DIVERGED — {} difference(s). This is a finding in the DESIGN, not a \
         tolerance to widen:\n  {}",
        diffs.len(),
        diffs.join("\n  ")
    );

    // The six hooks no engine emits, proven the only honest way available — see that function's
    // own doc for what it does and does not establish.
    assert_live_only_hooks_reach_the_plugin(&so);
}

/// The compiled-in half is only the build-time tier's mechanism if the tier still reaches a user
/// file that way. This asserts it against the REAL renderer rather than against this file's
/// memory of it — costs no cargo, so it gates in an ordinary run.
#[test]
fn generated_registry_still_reaches_a_user_file_as_a_path_module() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("vike-user-strategies")
        .join("tests")
        .join("fixture_user_data");
    let outcome = vike_user_strategies::codegen::scan(&fixtures);
    assert!(
        outcome.errors.is_empty(),
        "the committed fixture tree must scan clean: {:?}",
        outcome.errors
    );
    assert!(!outcome.strategies.is_empty(), "the committed fixture tree must hold strategies");
    let rendered = vike_user_strategies::codegen::render(&outcome.strategies);
    assert!(
        rendered.contains("#[path = \""),
        "the build-time tier must still compile a user file in as a `#[path]` MODULE — the \
         mechanism `tests/equivalence.rs` reproduces for its compiled-in half:\n{rendered}"
    );
    assert!(
        rendered.contains("::build::<B>(params)"),
        "the build-time tier must still reach the entry file's generic `build` directly:\n{rendered}"
    );
}

/// The equivalence comparison proves the two mechanisms AGREE. It cannot, by construction, prove
/// that the widened API surface is reached at all: a fixture that merely `use`d `vike_indicators`
/// without letting it decide anything would agree across both mechanisms for the same reason two
/// no-op trait defaults agree, and the manifest change underneath would be untested.
///
/// ⚠ **This is the shape the design's *Testing* section calls "a test that performs for the
/// system the step it was meant to witness", and it has produced six of them in this project.**
/// The witness that is not one: run the COMPILED half twice, differing in exactly the one params
/// key that switches the catalog indicator off, and require the two trade lists to DIFFER. If the
/// indicator is inert — never advanced, never resolved, dead-code-eliminated — the two runs are
/// identical and this goes red.
///
/// It costs no cargo and no `.so`, so it gates every PR. The link to the PLUGIN half is the
/// equivalence test: the compiled half's numbers now depend on `vike_indicators`, and the plugin
/// half is required to reproduce them bit for bit.
#[test]
fn the_indicator_catalog_changes_what_the_fixture_trades() {
    assert_a_params_switch_changes_the_trade_list(
        NO_INDICATOR_PARAMS,
        "the `vike_indicators` CATALOG",
        "rsi_period",
    );
}

/// The `vike_strategy` half of the same argument, switched by the presence of the `[controller]`
/// table. See [`the_indicator_catalog_changes_what_the_fixture_trades`] for why a differential is
/// the honest witness here and a `use` line is not.
#[test]
fn the_controller_framework_changes_what_the_fixture_trades() {
    assert_a_params_switch_changes_the_trade_list(
        NO_CONTROLLER_PARAMS,
        "the `vike_strategy` CONTROLLER framework",
        "[controller]",
    );
}

/// Both lanes, because the two emit different hooks and a contribution live on only one of them
/// is a fact worth failing over rather than averaging away.
fn assert_a_params_switch_changes_the_trade_list(off_params: &str, what: &str, key: &str) {
    for lane in [Lane::Bars, Lane::Ticks] {
        let on = run_compiled(lane);
        let off = run_compiled_with(off_params, lane);
        assert_not_vacuous(&on, lane);
        assert_not_vacuous(&off, lane);
        let diffs = trade_diffs(&on.result.trades, &off.result.trades);
        assert!(
            !diffs.is_empty(),
            "[{}] turning `{key}` OFF changed NOTHING about what the fixture traded, so {what} \
             is not reaching a decision in this fixture. The equivalence comparison would still \
             pass — two mechanisms can agree perfectly about a crate neither of them uses — and \
             the template's widened dependency table would be proven by nothing. Make the \
             contribution decide a size or a direction, do not relax this.",
            lane.label()
        );
    }
}

/// The catalog indicator must produce a reading that actually MOVES the multiplier over this
/// fixture's own series. A differential can be satisfied by a single warm-up bar's worth of
/// difference; this says the contribution is live across the run.
///
/// It calls the fixture's own [`ma_cross::make_rsi`] and [`ma_cross::rsi_scale`] rather than
/// re-typing either — the [`ma_cross::top_of_book_bias`] precedent, and for the same reason: a
/// re-typed copy drifts until the probe asserts arithmetic the strategy does not perform.
#[test]
fn the_catalog_indicator_resolves_and_its_reading_moves_the_size_multiplier() {
    // Read out of the params DOCUMENT rather than restated as a second constant: the document is
    // what both mechanisms are actually handed, and a literal here would be free to drift from it.
    let doc: toml::Value = toml::from_str(PARAMS_TOML).expect("fixture params must be valid TOML");
    let period = doc
        .get("rsi_period")
        .and_then(toml::Value::as_integer)
        .expect("the params document must carry `rsi_period`") as usize;
    let mut ind =
        ma_cross::make_rsi(period).expect("a non-zero period must resolve a catalog indicator");
    let mut scales: Vec<f64> = Vec::new();
    for bar in bar_series() {
        let v = ind.on_bar(&bar).first().copied().unwrap_or(f64::NAN);
        if v.is_finite() {
            scales.push(ma_cross::rsi_scale(v));
        }
    }
    assert!(
        scales.len() > N_BARS / 2,
        "the catalog indicator warmed on only {} of {N_BARS} bars — a reading that is NaN for \
         most of the run leaves `rsi_scale` at 1.0 and makes the surface it stands for nearly \
         inert",
        scales.len()
    );
    let (lo, hi) = scales.iter().fold((f64::MAX, f64::MIN), |(l, h), &s| (l.min(s), h.max(s)));
    assert!(
        lo < 1.0 && hi > 1.0,
        "the indicator's multiplier never crossed 1.0 in BOTH directions over this series \
         (min {lo}, max {hi}). One-sided or constant, it scales every order the same way and \
         says far less about whether the catalog is genuinely driving the fixture."
    );
    println!("MEASURED rsi size multiplier over {N_BARS} bars: min {lo}, max {hi}");
}

/// The fixture must not be a buy-and-hold, on EITHER lane. Same guard as the one inside the
/// equivalence test, broken out so a fixture edit that quietly made the comparison vacuous fails
/// FAST and in an ordinary run, without waiting for a cargo build.
#[test]
fn the_fixture_trades_both_directions_and_is_not_a_buy_and_hold() {
    for lane in [Lane::Bars, Lane::Ticks] {
        assert_not_vacuous(&run_compiled(lane), lane);
    }
}

/// Every hook `PluginVTable` carries must be one the FIXTURE actually overrides.
///
/// ⚠ **This is the guard against the failure this project has produced four times: a test that
/// performs for the system the very step it is meant to witness.** Wiring a hook and then not
/// exercising it leaves the equivalence comparison agreeing for a reason that has nothing to do
/// with that hook — both mechanisms run the trait's no-op default and agree perfectly. A hook
/// added to `WIRED_HOOKS` without a matching override in the fixture therefore fails HERE, by
/// name, rather than being quietly carried as covered.
///
/// Text over the fixture's own committed bytes, the same bytes both mechanisms compile — no
/// cargo, no reflection. It proves the OVERRIDE EXISTS; that each override changes behaviour is
/// what the fixture's own doc table and `assert_not_vacuous`'s hook half are for, and what the
/// bit-for-bit comparison then measures.
#[test]
fn the_fixture_overrides_every_hook_the_vtable_carries() {
    // ⚠ Over the fixture's CODE, not its text. A raw `contains` is satisfied by a hook named in a
    // comment — including the doc table above each override, which names every one of them — so
    // an override commented out during debugging and left that way would keep this green while
    // the comparison compared two no-ops. Comments are blanked first; a `//` inside a string
    // literal would truncate that line, which can only cause a FALSE FAILURE (loud, and the safe
    // direction) and which this fixture contains none of.
    let code = strip_comments(FIXTURE_SOURCE);
    let missing: Vec<&&str> = vike_strategy_plugin::host::WIRED_HOOKS
        .iter()
        .filter(|hook| !code.contains(&format!("fn {hook}(")))
        .collect();
    assert!(
        missing.is_empty(),
        "`vike_strategy_plugin::host::WIRED_HOOKS` names {missing:?}, which the equivalence \
         fixture (tests/fixtures/equivalence/ma_cross.rs) does not override. A wired hook the \
         fixture never implements is a hook this test suite COMPARES two no-ops for: both \
         mechanisms fall through to the trait default and agree perfectly, so the comparison \
         reads green while saying nothing. Add an override that CHANGES BEHAVIOUR (see that \
         file's own table of what each one puts at stake), or — if the hook genuinely cannot be \
         driven — say so where the narrower witness lives."
    );
}

/// The spy in [`assert_the_engine_emits_the_state_dependent_hooks`] must FORWARD exactly the hooks
/// `StrategyEngine` emits — no more, no fewer.
///
/// ⚠ **Its forwarding roster is hand-written, and a hand-written roster rots.** If `Strategy`,
/// `engine.rs` and the fixture all grew a tenth hook, the spy would silently swallow it to the
/// trait's own no-op default: its counters would then describe a DIFFERENT run from the one the
/// comparison compares, while every assertion stayed green. That is the roster-rot shape this repo
/// gates everywhere else, and the probe is worth nothing the moment its subject drifts from the
/// comparison's.
///
/// So the two sets are DERIVED from two independent sources and compared, the same shape
/// `tests/hook_roster.rs` uses on the trait: the engine's emitted set out of
/// `crates/vike-backtest/src/engine.rs`'s own call sites, the spy's out of THIS file. Neither is
/// typed here, so neither can be quietly corrected into agreement.
///
/// ⚠ Scoped to `engine.rs` deliberately — that is the engine [`drive`] runs, both lanes of it.
/// A hook emitted only by `vector_engine.rs` reaches neither the probe nor the comparison and is
/// not this gate's subject.
#[test]
fn the_spy_forwards_exactly_the_hooks_the_engine_emits() {
    // Assembled from halves so this file's own scan cannot match the needle's own source — the
    // one trap a test that reads itself has, and the one `vike-studio-core`'s `plugin_run.rs`
    // records paying.
    let forward_prefix = concat!("self.", "inner.");

    let engine_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("vike-backtest")
        .join("src")
        .join("engine.rs");
    let engine =
        strip_comments(&std::fs::read_to_string(&engine_path).unwrap_or_else(|e| {
            panic!("cannot read the engine at {}: {e}", engine_path.display())
        }));
    let own = strip_comments(include_str!("equivalence.rs"));

    // Candidates are the REAL trait's methods, so neither scan can invent a name out of a string
    // literal or an unrelated `strategy.` receiver.
    let every_hook: Vec<&str> = vike_strategy_plugin::host::WIRED_HOOKS
        .iter()
        .chain(vike_strategy_plugin::host::UNWIRED_HOOKS)
        .copied()
        .collect();

    let emitted: Vec<&str> =
        every_hook.iter().copied().filter(|h| engine.contains(&format!("strategy.{h}("))).collect();
    let forwarded: Vec<&str> = every_hook
        .iter()
        .copied()
        .filter(|h| own.contains(&format!("{forward_prefix}{h}(")))
        .collect();

    assert!(
        !emitted.is_empty(),
        "the engine scan found NO emitted hook, so this gate cannot fail for its stated reason. \
         `StrategyEngine`'s call sites are spelled `self.strategy.<hook>(` (and `strategy.warmup()` \
         in `new`); if that changed, this scan needs to change with it."
    );

    let missing: Vec<&&str> = emitted.iter().filter(|h| !forwarded.contains(h)).collect();
    assert!(
        missing.is_empty(),
        "`StrategyEngine` emits {missing:?}, which the probe's `HookSpy` does NOT forward — so it \
         swallows them to the trait's no-op default and its counters describe a different run from \
         the one the equivalence comparison compares, silently and greenly. Forward each one to \
         `inner` (count it too, if the probe should have an opinion about it)."
    );
    let extra: Vec<&&str> = forwarded.iter().filter(|h| !emitted.contains(h)).collect();
    assert!(
        extra.is_empty(),
        "`HookSpy` forwards {extra:?}, which `StrategyEngine` never emits. Harmless to run, but it \
         means this roster and the engine's have drifted — delete the forward, or (if the engine \
         genuinely grew the call site) confirm the scan above still sees it."
    );
}

/// Blank `//` line comments and `/* … */` blocks, keeping every other byte and every newline, so
/// a search over the result is a search over CODE.
///
/// Deliberately NOT string-literal aware, and the bias is chosen: a `//` inside a string truncates
/// that line, so the only thing this can get wrong is to HIDE code from the caller — which makes
/// the caller's assertion fail loudly rather than pass quietly. The opposite bias (treat
/// everything as code) is the one that reads a commented-out override as a live one.
fn strip_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut in_block = false;
    for line in src.lines() {
        let mut rest = line;
        loop {
            if in_block {
                match rest.find("*/") {
                    Some(i) => {
                        in_block = false;
                        rest = &rest[i + 2..];
                    }
                    None => {
                        rest = "";
                        break;
                    }
                }
            } else {
                // Whichever opener comes FIRST decides, and the match is written without a guard
                // on purpose: `match arms with guards don't count towards exhaustivity`, so the
                // guarded version compiled to a non-exhaustive match (E0004, caught on the lane).
                let opener = match (rest.find("//"), rest.find("/*")) {
                    (Some(l), Some(b)) => Some((l.min(b), b < l)),
                    (Some(l), None) => Some((l, false)),
                    (None, Some(b)) => Some((b, true)),
                    (None, None) => None,
                };
                match opener {
                    None => break,
                    Some((at, is_block)) => {
                        out.push_str(&rest[..at]);
                        if is_block {
                            in_block = true;
                            rest = &rest[at + 2..];
                        } else {
                            rest = "";
                            break;
                        }
                    }
                }
            }
        }
        out.push_str(rest);
        out.push('\n');
    }
    out
}

/// The blanking above is only worth anything if it can actually tell the two apart — the same
/// "a gate needs a non-empty input" rule every ratchet in `crates/vike-ops/tests` states.
#[test]
fn the_comment_blanker_hides_a_commented_out_override_and_keeps_a_live_one() {
    let src = "impl S {\n    fn on_fill(&mut self) {}\n    // fn on_mark(&mut self) {}\n    /* fn on_flow(&mut self) {} */\n}\n";
    let code = strip_comments(src);
    assert!(code.contains("fn on_fill("), "a LIVE override must survive: {code:?}");
    assert!(!code.contains("fn on_mark("), "a `//`-commented override must not: {code:?}");
    assert!(!code.contains("fn on_flow("), "a `/* */`-commented override must not: {code:?}");
    assert_eq!(
        code.lines().count(),
        src.lines().count(),
        "blanking must keep the line structure, so a future caller can report a line number"
    );
}

// ---------------------------------------------------------------------------------------------
// The narrower witnesses — the six hooks no backtest engine emits
// ---------------------------------------------------------------------------------------------

/// A broker the plugin's `guest::HostBroker` can really call back into, recording every submit.
///
/// ⚠ **Not a `SimBroker`, and it cannot be one.** `SimBroker::idx` resolves a symbol by position
/// in the mounted universe and PANICS on anything else, and the six hooks below describe
/// themselves through fabricated symbols (`"feed:stale"`, `"ord:<coid>:…"`). More importantly a
/// `SimBroker` would add nothing: no engine calls these hooks, so there is no engine behaviour to
/// compare — the question is only whether the payload arrived intact.
#[derive(Default)]
struct RecordingBroker {
    submits: Vec<(String, i32, f64)>,
}

impl Broker for RecordingBroker {
    fn submit_market(&mut self, symbol: &str, side: i32, qty: f64) {
        self.submits.push((symbol.to_string(), side, qty));
    }
    fn submit_limit(&mut self, _symbol: &str, _side: i32, _qty: f64, _price: f64) {}
    fn position(&self, _symbol: &str) -> f64 {
        0.0
    }
    fn price(&self, _symbol: &str) -> f64 {
        0.0
    }
    fn equity(&self) -> f64 {
        0.0
    }
    fn bars(&self, _symbol: &str) -> &[Bar] {
        &[]
    }
    fn index(&self) -> usize {
        0
    }
    fn now(&self) -> i64 {
        0
    }
}

impl HftBroker for RecordingBroker {
    fn position(&self) -> f64 {
        0.0
    }
    fn submit_limit_tagged(&mut self, _tag: &str, _side: i32, _qty: f64, _price: f64) {}
    fn modify_tagged(&mut self, _tag: &str, _new_qty: Option<f64>, _new_price: Option<f64>) {}
    fn cancel_tagged(&mut self, _tag: &str) {}
}

/// The six hooks the equivalence comparison CANNOT prove, and what is done about that instead.
///
/// ⚠ **Say which, and why, rather than leaving them quietly untested.** `on_feed_status`,
/// `on_mark`, `on_reference_quote`, `on_flow`, `on_order_event` and `on_params_updated` are
/// LIVE-ONLY: every one of their doc comments in `crates/vike-model/src/strategy/mod.rs` states
/// that the backtest engines never fire them, and neither `StrategyEngine::run` nor `run_ticks`
/// contains a call site for any of them. So there is no engine behaviour for two mechanisms to
/// agree or disagree about, and folding them into the bit-for-bit comparison would have meant
/// comparing two runs in which they never happened — an agreement that says nothing, which is
/// exactly the failure mode this file exists to avoid producing.
///
/// What IS honest is narrower and stated as such: drive each hook through the real
/// `PluginStrategy` host wrapper against a real `dlopen`ed artifact built by the real builder, and
/// read back what reached the user strategy. Everything the equivalence test proves about the
/// PIPELINE is still in play — the template's export, the loader's bind, the host's thunks, the
/// guest's decode, the `catch_unwind` on both sides. What is NOT proven is that an engine would
/// route them the same way, because no engine routes them at all.
///
/// The evidence channel is the fixture's own: each of these hooks submits a SELF-DESCRIBING order
/// (`"<hook>:<payload>"`), so this reads back not merely that the hook fired but which payload
/// crossed — the venue string, the absent-vs-empty tag, the reason text, the `None` barrier.
///
/// ⚠ Called from inside the one `build_fixture_plugin` caller rather than from a `#[test]` of its
/// own, for the reason that test's own doc gives: under `nextest` a second test is a second
/// PROCESS, and two processes racing one cold `build_plugin` share a scratch target directory.
fn assert_live_only_hooks_reach_the_plugin(so: &Path) {
    use abi::{BrokerRef, COptStrRef, COrderLifecycle, CStrRef, PluginStatus};

    let plugin = load_or_panic(so);
    let mut broker = RecordingBroker::default();
    {
        let mut strategy: Box<dyn Strategy<RecordingBroker>> =
            Box::new(PluginStrategy::<RecordingBroker>::new(plugin.vtable, PARAMS_TOML));

        strategy.on_feed_status(&mut broker, vike_model::FeedStatus::Stale);
        strategy.on_mark(
            &mut broker,
            &vike_model::MarkTick {
                symbol: "btcusdt".to_string(),
                price: 64_321.5,
                ts: 1_700_000_000_005,
            },
        );
        strategy.on_reference_quote(
            &mut broker,
            "binance",
            &QuoteTick {
                ts: 1,
                local_ts: 0,
                bid: 99.0,
                ask: 101.0,
                bid_size: 0.0,
                ask_size: 0.0,
                symbol: SYMBOL.to_string(),
            },
        );
        strategy.on_flow(&mut broker, vike_model::FlowToxicity { bid: 0.25, ask: 0.75, ts: 7 });
        strategy.on_order_event(
            &mut broker,
            &vike_model::OrderLifecycle {
                client_order_id: "vike-77".to_string(),
                tag: Some("bid-1".to_string()),
                kind: vike_model::OrderEventKind::Canceled { reason: "replaced".to_string() },
            },
        );
        // ...and the ABSENT tag, which is a different fact from an empty one and the whole reason
        // `abi::COptStrRef` carries a `present` flag rather than leaning on a null pointer.
        strategy.on_order_event(
            &mut broker,
            &vike_model::OrderLifecycle {
                client_order_id: "vike-78".to_string(),
                tag: None,
                kind: vike_model::OrderEventKind::Accepted,
            },
        );
        strategy.on_params_updated(
            &mut broker,
            &vike_model::StrategyParams::PositionController(vike_model::ControllerParams::new(
                5_000,
                1.5,
                // Every leg UNARMED — four `None`s. This is the value a TOML hop could not have
                // carried AT ALL (`toml` refuses `serialize_none`), so seeing `none` come back is
                // what makes this a witness for the ENCODING and not only for the dispatch.
                vike_model::TripleBarrier::default(),
                0.75,
            )),
        );
    }

    let expected: Vec<(String, i32, f64)> = vec![
        ("feed:stale".to_string(), 1, 2.0),
        ("mark:btcusdt:1700000000005".to_string(), 1, 64_321.5),
        (format!("refq:binance:{SYMBOL}"), 1, 100.0),
        ("flow:7".to_string(), 1, 0.25),
        ("flow:7".to_string(), -1, 0.75),
        ("ord:vike-77:some(bid-1):canceled:replaced".to_string(), 1, 1.0),
        ("ord:vike-78:none:accepted:".to_string(), 1, 1.0),
        ("params:controller:5000:0.75:none".to_string(), 1, 1.5),
    ];
    assert_eq!(
        broker.submits, expected,
        "a live-only hook did not reach the plugin, or reached it with a mangled payload. Each \
         entry is `(<hook>:<payload>, side, scalar)` written by the fixture itself — read the \
         first differing row: a MISSING row means the hook was never delivered, a row with the \
         wrong payload means the mirror lost a field on the way across."
    );

    // ...and the REFUSAL half of the exhaustive mapping, driven through the same real artifact.
    // Without this, `abi::order_event_from_code`'s `None` arm is proven only by a unit test that
    // never crosses a `.so` boundary — and the arm exists precisely so a kind this build does not
    // know is never delivered as a guessed transition.
    let handle = (plugin.vtable.create)(PARAMS_TOML.as_ptr(), PARAMS_TOML.len());
    assert!(!handle.is_null(), "create must succeed for the BadParams probe");
    let mut probe_broker = RecordingBroker::default();
    let broker_ref = BrokerRef {
        ctx: std::ptr::from_mut(&mut probe_broker).cast(),
        vtable: vike_strategy_plugin::host::broker_vtable::<RecordingBroker>(),
    };
    let coid = "vike-99";
    let bad = COrderLifecycle {
        client_order_id: CStrRef::of(coid),
        tag: COptStrRef::of(None),
        // A code no `ORDER_EVENT_*` constant names. If a seventh `OrderEventKind` variant is ever
        // added AND given this code, this probe starts testing delivery instead of refusal —
        // which is why it picks a number far above the six, and why the assertion below names
        // what it is really claiming.
        kind: 9_999,
        reason: CStrRef::empty(),
    };
    let status = (plugin.vtable.on_order_event)(handle, broker_ref, &bad as *const COrderLifecycle);
    (plugin.vtable.destroy)(handle);
    assert_eq!(
        status,
        PluginStatus::BadParams,
        "an order-event kind this build does not know must be REFUSED whole. Delivering a guessed \
         transition would make a live order look dead to a strategy's retry machine and free a \
         slot that is still occupied."
    );
    assert!(
        probe_broker.submits.is_empty(),
        "a refused order event must not have reached user code at all, and it did: {:?}",
        probe_broker.submits
    );
}
