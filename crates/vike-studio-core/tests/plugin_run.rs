//! **THE JOIN, proved through the production path.** A `StrategySpec::Plugin` naming a real built
//! artifact reaches `vike_studio_core::run::build_strategy` the way a Run reaches it, produces a
//! working `Box<dyn Strategy<SimBroker>>`, and trades.
//!
//! # Why this file exists rather than another hand-wired proof
//!
//! `docs/decisions/0082-the-plugin-mechanism-lands-without-the-feature.md` names the exact shape
//! this test refuses to repeat: `crates/vike-strategy-plugin/tests/equivalence.rs` *"performs the
//! wiring BY HAND inside the test, which is why it can prove the two mechanisms agree while
//! production has no route to either. A test that reaches a property through its own harness
//! proves the property, not the reachability."* So nothing here constructs a `PluginStrategy`, a
//! `PluginVTable` or a `loader::LoadedPlugin`. This file names no symbol from
//! `vike-strategy-plugin` at all — deliberately, and it is the single strongest property of the
//! file: the ONLY way a plugin can reach the engine below is through
//! `vike_studio_core::run`'s own `StrategySpec::Plugin` arm, so deleting that arm makes this test
//! fail rather than making it load the plugin some other way.
//!
//! The artifact half is the production builder (`vike_strategy_builder::render::build_plugin`, the
//! same function the deployed service calls), not a hand-rolled `cargo` invocation, for the reason
//! the equivalence test gives for its own copy of that edge.
//!
//! # ⚠ THE ONE STEP THIS FILE CANNOT EXERCISE, NAMED RATHER THAN LEFT TO BE FOUND
//!
//! **The CROSS-UNIT hand-off.** On a real box the builder is one systemd unit writing
//! `<its root>/user_data/plugins` and the compute daemon is another reading
//! `<its root>/user_data/plugins`; this file is ONE process with no systemd, no second unit and no
//! second root, so it stages the artifact with `std::fs::copy` across precisely that joint.
//!
//! That is the same species as the defect
//! `docs/decisions/0082-the-plugin-mechanism-lands-without-the-feature.md` records one level up —
//! *whatever a test DOES for the system, the test cannot prove ABOUT the system* — and it bit
//! here: the join first shipped with the two units' directories compared by nothing, and this
//! file's copy is exactly what hid it. Two things follow, and both are deliberate:
//!
//! 1. **The staging path is `plugins_dir()`'s own answer, never a path this file spells.** The
//!    copy proves the READER's half honestly (stage where the reader looks, not where the test
//!    thinks it looks) and claims nothing about the writer's.
//! 2. **The writer's half is gated where the deploy files can be read**:
//!    `crates/vike-ops/tests/deploy_layout_gate.rs`'s
//!    `every_shipped_plugin_artifact_dir_is_one_spelling` compares the builder unit's
//!    `VIKE_STRATEGY_BUILDER_OUT_DIR` against the root-relative path the compute daemon resolves,
//!    and requires both files to tell the operator to substitute one `$R`. A test that cannot
//!    exercise a joint should at least fail when the joint is misconfigured; that is the rule that
//!    does it, and this file names it so a reader of either finds the other.
//!
//! # One `#[test]`, on purpose
//!
//! The production `build_strategy` resolves its artifact directory from the PROCESS's working
//! directory (`run::plugins_dir`), which is what makes it correct on a box where every shipped
//! unit runs `WorkingDirectory=<project>`. Testing that honestly means setting the working
//! directory, which is process-global: two tests doing it in parallel threads would race. Under
//! nextest each test is its own process and there would be no race; under the plain runner every
//! `#[test]` in ONE integration binary shares a process. So this file holds exactly one test and
//! runs its phases in order, which is safe under BOTH runners — the workspace requires both.
//!
//! ```text
//! LANE=lane1 MSYS_NO_PATHCONV=1 just the latency box <branch> cargo test -p vike-studio-core --test plugin_run
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use vike_backtest::{EngineParams, SimBroker};
use vike_data::{DataFusionHist, HistStore, TsRange};
use vike_model::{Bar, Strategy};
use vike_strategy_builder::render::{Profile, artifact_dir_tag, build_plugin, sha256_hex};
use vike_studio_core::{
    DataSlice, RunError, StoreHandle, StrategySpec, build_strategy, build_strategy_with, run_slice,
};

/// The user strategy this test builds — an ORDINARY entry file, with no idea FFI exists: it obeys
/// `crates/vike-user-strategies/src/lib.rs`'s entry contract, imports `vike_model` and `toml` and
/// nothing else.
///
/// It is deliberately PARAMETER-DRIVEN and deliberately TRADES. Both properties are load-bearing:
///
/// * `every` decides how often it flips, so two different params tables produce two different
///   trade counts. That is what turns "params crossed the boundary" from an assumption into an
///   observation — the template once parsed its params with `str::parse::<toml::Value>()` and
///   returned a NULL handle for every params string including the empty one, and a strategy that
///   never trades looks identical to one whose params silently vanished.
/// * It alternates SIDE, so the engine closes round trips and the result carries real trades
///   rather than one open position.
///
/// It implements `warmup` and `on_bar` and no other hook — which is all this fixture needs, not a
/// constraint any more. ⚠ That sentence used to read "because those are the two `PluginVTable`
/// wires", and the vtable carries fifteen since `ABI_VERSION` 3; only `params`, `save_state` and
/// `load_state` are still refused at build time. A fixture overriding one of those three would
/// fail at build time rather than here.
///
/// ⚠ **`every` is read leniently — integer OR float — and that is a finding this test made rather
/// than a tidiness choice.** A SWEEP point's overrides are `(String, f64)` pairs, and
/// `vike_studio_core::params_with_overrides` writes every one of them in as a `Value::Float`. So
/// an `as_integer()`-only reader sees a swept knob as ABSENT and silently runs on its own
/// fallback: the first version of this fixture did exactly that, and phase 4 below caught it as
/// two sweep points trading identically. That is not a plugin property — the shared override
/// helper does the same for a NATIVE strategy, and `BuyHold::from_params` accepts both types for
/// the same reason — but a plugin author writing against this tier needs it stated somewhere, and
/// a fixture that could not see its own sweep would have made phase 4 vacuous.
const SOURCE: &str = r#"
use vike_model::{Bar, Broker, Strategy};

pub struct Pulse {
    size: f64,
    every: usize,
    seen: usize,
    long: bool,
}

impl<B: Broker> Strategy<B> for Pulse {
    fn warmup(&self) -> usize {
        1
    }

    fn on_bar(&mut self, broker: &mut B, bar: &Bar) {
        self.seen += 1;
        if self.seen % self.every != 0 {
            return;
        }
        let Some(symbol) = bar.symbol.clone().filter(|s| !s.is_empty()) else {
            return;
        };
        let side = if self.long { -1 } else { 1 };
        broker.submit_market(&symbol, side, self.size);
        self.long = !self.long;
    }
}

pub fn build<B: vike_model::HftBroker + 'static>(
    params: &toml::Value,
) -> Box<dyn vike_model::Strategy<B> + Send> {
    let every = params
        .get("every")
        .and_then(|v| v.as_integer().or_else(|| v.as_float().map(|f| f as i64)))
        .map(|i| i.max(1) as usize)
        .unwrap_or(7);
    let size = params
        .get("size")
        .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
        .unwrap_or(1.0);
    Box::new(Pulse { size, every, seen: 0, long: false })
}
"#;

/// The declared plugin name — the `<name>` half of `<name>-<sha>.so`.
const NAME: &str = "join_probe";

#[test]
fn a_plugin_spec_naming_a_real_artifact_runs_through_the_production_build_strategy() {
    // ---- phase 0: build the artifact with the REAL builder --------------------------------
    let artifact = build_the_artifact();
    let sha = sha256_hex(SOURCE.as_bytes());
    assert!(
        artifact
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n == format!("{NAME}-{sha}.so")),
        "the builder's artifact name is the design's wire contract: {}",
        artifact.display()
    );

    // ---- phase 1: a project the production resolver can find ------------------------------
    // `run::plugins_dir` walks UP from the working directory for a project marker, so the test
    // plants one and moves into it. A `settings/` DIRECTORY is the marker the DEPLOYED shape uses
    // (`crates/vike-model/src/state_path.rs`), which is the shape this is standing in for.
    let project = tempfile::tempdir().expect("project root");
    std::fs::create_dir_all(project.path().join("settings")).expect("plant the project marker");

    let restore = std::env::current_dir().expect("current dir");
    std::env::set_current_dir(project.path()).expect("enter the project");
    // Every assertion below runs inside this project; `restore` is put back at the end so a
    // later test binary in the same process (there is none today) is not left somewhere odd.

    // ⚠ **THE STAGING PATH IS THE PRODUCTION RESOLVER'S OWN ANSWER, not a path this file spells.**
    // It used to be `project/user_data/plugins` written out here, which made the test agree with
    // `plugins_dir` by coincidence: a change to either would have been invisible to the other.
    // Asking `plugins_dir()` is the difference between staging where the reader looks and staging
    // where the test thinks the reader looks.
    let plugins = vike_studio_core::plugins_dir();
    // The resolver must have found THIS project — asserted by the marker rather than by comparing
    // paths, because a temp root can be reached through a symlink (`/tmp` -> `/private/var/...` on
    // some boxes) and `current_dir()` answers the resolved spelling, so a string prefix test would
    // be a flake rather than a check.
    let resolved_root = plugins.parent().and_then(std::path::Path::parent).expect("<project>");
    assert!(
        resolved_root.join("settings").is_dir(),
        "the resolver answered {} , whose project root carries no `settings/` marker — it did not \
         find the project this test just entered",
        plugins.display()
    );
    std::fs::create_dir_all(&plugins).expect("plant the resolved plugin directory");
    std::fs::copy(&artifact, plugins.join(format!("{NAME}-{sha}.so"))).expect("stage the artifact");

    // ---- phase 2: THE JOIN — a full Run, exactly as a Studio Run reaches it ----------------
    // `run_slice` is the entry every local Run and (through `wire_run::run_slice_local`) every
    // REMOTE one funnels into. Nothing below reaches the loader itself.
    let (_store_dir, store, bars) = seeded_store();
    let spec = plugin_spec(&sha, 5);
    let result = run_slice(&spec, &slice(), &store, EngineParams::default())
        .expect("a plugin spec naming a real artifact must RUN through the production pipeline");
    assert!(
        !result.equity_curve.is_empty(),
        "the plugin ran over the slice and produced an equity curve"
    );
    assert!(
        !result.trades.is_empty(),
        "the plugin must actually TRADE — a loaded-but-inert plugin (a null handle from a params \
         parse failure) produces exactly this shape with an empty trade list"
    );

    // ---- phase 3: the params table genuinely crossed the C-ABI -----------------------------
    // A DIFFERENT `every` must produce a DIFFERENT number of trades. Without this, a plugin whose
    // params arrived empty (or not at all) would pass phase 2 on its fallback values.
    let sparse = run_slice(&plugin_spec(&sha, 40), &slice(), &store, EngineParams::default())
        .expect("the same artifact with different params must run");
    assert!(
        sparse.trades.len() < result.trades.len(),
        "a larger `every` must trade less often — same artifact, different params table. \
         got {} at every=40 vs {} at every=5",
        sparse.trades.len(),
        result.trades.len()
    );

    // ---- phase 4: the SWEEP door reaches the same artifact ----------------------------------
    // `build_strategy_with` is the door every sweep point enters. A sweep instantiates ONE
    // artifact N times with N params tables; each call here must produce a working strategy, and
    // the override must reach the plugin (proved the same way phase 3 proves it: by the answer
    // changing).
    let dense = drive(
        build_strategy_with(&plugin_spec(&sha, 40), &[("every".to_string(), 5.0)])
            .expect("the sweep door must build a plugin strategy"),
        &bars,
    );
    let coarse = drive(
        build_strategy_with(&plugin_spec(&sha, 5), &[("every".to_string(), 40.0)])
            .expect("the sweep door must build a plugin strategy"),
        &bars,
    );
    assert!(
        coarse < dense,
        "a sweep point's override must reach the plugin's params table: every=5 traded {dense}, \
         every=40 traded {coarse}"
    );

    // ---- phase 5: the two refusals still fire, through the same production door -------------
    // The EMPTY-SHA refusal, unconditional and ahead of any lookup. It is not an accident of a
    // missing file: it names the emptiness, and it must keep doing so.
    match build_strategy(&StrategySpec::plugin(NAME, "", vike_studio_core::empty_params())) {
        Err(RunError::Strategy(msg)) => {
            assert!(msg.contains("no sha"), "{msg}");
            assert!(msg.contains("names no artifact"), "{msg}");
        }
        Ok(_) => panic!("an empty sha must be refused even with a real artifact on disk"),
        Err(other) => panic!("wrong RunError variant: {other:?}"),
    }
    // ...and through the SWEEP door too, so no sweep point can smuggle one past it.
    assert!(
        build_strategy_with(
            &StrategySpec::plugin(NAME, "   ", vike_studio_core::empty_params()),
            &[("every".to_string(), 5.0)]
        )
        .is_err(),
        "a whitespace-only sha is not a sha, on the sweep door either"
    );

    // The MISSING-ARTIFACT refusal: a well-formed sha naming no file must REFUSE, naming the
    // build — never fall through to a paper/no-op strategy, and never wait for the file to appear
    // (a server that waited would know a builder exists, which the design denies it).
    let absent = StrategySpec::plugin(NAME, "b".repeat(64), vike_studio_core::empty_params());
    match run_slice(&absent, &slice(), &store, EngineParams::default()) {
        Err(RunError::Strategy(msg)) => {
            assert!(msg.contains(NAME), "the refusal must name the artifact: {msg}");
            assert!(msg.to_lowercase().contains("build"), "{msg}");
        }
        Ok(_) => panic!("a sha with no artifact behind it must not produce a run"),
        Err(other) => panic!("wrong RunError variant: {other:?}"),
    }

    std::env::set_current_dir(restore).expect("restore the working directory");
}

/// Build `SOURCE` through the production builder into a CACHED directory under this test binary's
/// own scratch space, and return the artifact path.
///
/// Cached rather than built into the throwaway project root because `build_plugin`'s cache is the
/// artifact's existence in `out_dir` — a fresh directory per run would pay a full cargo build
/// every time. The directory is keyed on `render::artifact_dir_tag()` — the ABI version and the
/// toolchain fingerprint, which is exactly what the loader compares — because the artifact name
/// carries the SOURCE sha and neither of those.
///
/// ⚠ **This tag was the PROFILE alone, and that gap went red in CI.** A profile tag covers a
/// debug run colliding with a release one and nothing else, so after `ABI_VERSION` went 2 -> 3 a
/// warm runner cache handed this test an ABI-2 artifact and `loader::load` correctly refused it
/// (`plugin ABI version 2 does not match this host's 3`). The refusal was right; the directory
/// was wrong. Two sibling harnesses in `vike-strategy-plugin` had already been fixed by hand and
/// this third one, in another crate, was missed — which is why the rule now lives in the library
/// all three depend on rather than being written out a third time.
///
/// ⚠ **A cache-shaped defect is invisible wherever the cache is COLD.** `just verify-branch` was
/// green on this branch while CI was red, honestly: its lane had built this crate at the new ABI,
/// so nothing stale was there to be handed back. Do not read a green from a fresh lane as covering
/// this class.
fn build_the_artifact() -> PathBuf {
    let profile = Profile::of_this_binary();
    let out_dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("plugin-join-artifacts-{}", artifact_dir_tag()));
    // Two levels up from this crate's manifest directory is the checkout root, which is what the
    // rendered scratch crate's `path` dependencies resolve against.
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    // The exact cargo running this test, so the nested build uses the same toolchain rather than
    // whatever is first on PATH. `Layer::TestOnly`.
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    build_plugin(SOURCE, NAME, &out_dir, &workspace_root, &cargo, profile).unwrap_or_else(|e| {
        panic!("the production builder must produce a plugin from this fixture source:\n{e}")
    })
}

/// The spec under test: this plugin, this sha, and an `every` the assertions can move.
fn plugin_spec(sha: &str, every: i64) -> StrategySpec {
    let mut table = toml::map::Map::new();
    table.insert("every".to_string(), toml::Value::Integer(every));
    table.insert("size".to_string(), toml::Value::Float(1.0));
    StrategySpec::plugin(NAME, sha, toml::Value::Table(table))
}

/// Run an already-built strategy over `bars` and answer how many trades it produced — the sweep
/// door's assertions need a number, and `build_strategy_with` hands back a strategy rather than a
/// result.
fn drive(strategy: Box<dyn Strategy<SimBroker>>, bars: &[Bar]) -> usize {
    vike_backtest::StrategyEngine::new(
        vec![("BTCUSDT".to_string(), bars.to_vec())],
        strategy,
        EngineParams::default(),
    )
    .run()
    .trades
    .len()
}

/// A deterministic 400-bar series in a real store — the same shape `run.rs`'s own tests seed.
fn seeded_store() -> (tempfile::TempDir, StoreHandle, Vec<Bar>) {
    let dir = tempfile::tempdir().expect("store dir");
    let store = DataFusionHist::open(dir.path()).expect("open the store");
    let mut px = 100.0f64;
    let mut seed = 0x1234_5678u64;
    let bars: Vec<Bar> = (0..400)
        .map(|i| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            px = (px + ((seed >> 32) as f64 / u32::MAX as f64 - 0.5) * 2.0).max(1.0);
            Bar {
                ts: 60_000 * (i as i64 + 1),
                open: px,
                high: px,
                low: px,
                close: px,
                volume: 0.0,
                funding: None,
                bid: None,
                ask: None,
                symbol: Some("BTCUSDT".into()),
            }
        })
        .collect();
    store.append_bars("binance", "BTCUSDT", "1m", &bars, None).expect("seed bars");
    (dir, Arc::new(store) as StoreHandle, bars)
}

fn slice() -> DataSlice {
    DataSlice::bars("binance", "BTCUSDT", "1m", TsRange::all())
}

/// **The structural property this file rests on, GATED rather than merely claimed.**
///
/// The header says the only route from a `StrategySpec::Plugin` to a running strategy here is the
/// production `build_strategy` arm, because nothing in this file names a `vike-strategy-plugin`
/// symbol. ⚠ **That was a convention and nothing held it.** `vike-strategy-plugin` is a NORMAL
/// dependency of `vike-studio-core` and an integration test links its host crate's normal deps, so
/// ONE `use` line reaches `loader::load` with no manifest change and no review signal — at which
/// point this file could load the plugin by hand and keep passing, which is the exact shape
/// [0082](../../../docs/decisions/0082-the-plugin-mechanism-lands-without-the-feature.md) was
/// written about.
///
/// It reads its OWN source through `include_str!`, so it moves with the file and pins no path in
/// a table somewhere else that a rename would rot. Comment lines are dropped, because the header
/// NAMES every symbol below in the course of explaining why it must not use them.
///
/// It is a second `#[test]` in a file whose header argues for one, and that is safe: it touches no
/// working directory and no filesystem — `include_str!` is resolved by the compiler — so it cannot
/// race the cwd-mutating test beside it under the plain runner.
#[test]
fn this_file_reaches_the_plugin_only_through_the_production_build_strategy() {
    // ⚠ **Each needle is ASSEMBLED FROM HALVES, and it has to be.** A `const BANNED: &[&str]`
    // spelling them whole would put every one of them in this file's own code region, so the scan
    // below would find them on its own declaration and the test could never pass — the
    // self-reference trap `crates/vike-ops/tests/temp_path_gate.rs` and its siblings each solve the
    // same way. Halves that are not themselves the needle keep the scan honest.
    let banned: Vec<String> = [
        ("vike_strategy", "_plugin"),
        ("Plugin", "Strategy"),
        ("Plugin", "VTable"),
        ("loader", "::load"),
        ("Loaded", "Plugin"),
    ]
    .iter()
    .map(|(a, b)| format!("{a}{b}"))
    .collect();
    let src = include_str!("plugin_run.rs");
    let code: String =
        src.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
    for banned in &banned {
        assert!(
            !code.contains(banned.as_str()),
            "this file names `{banned}` in CODE. Its whole proof is that the only route from a \
             StrategySpec::Plugin to a running strategy is `build_strategy`'s own arm — a test \
             that reaches the loader itself proves the loader works and says nothing about \
             whether production can get there, which is the defect docs/decisions/0082 records. \
             If a direct-loader test is genuinely wanted, it belongs in vike-strategy-plugin, \
             beside the equivalence test that already does exactly that."
        );
    }
    // The floor: the scan must SEE this file's code, or the loop above passed over an empty
    // string. Two witnesses — a symbol that is definitely in the code region, and a phrase that is
    // ONLY in the comment region, which must have been stripped.
    assert!(
        code.contains("build_strategy"),
        "the source scan found no code — `include_str!` or the comment filter is broken, and \
         every assertion above just passed by looking at nothing"
    );
    // ⚠ Assembled from halves for the same reason the needles above are, and this floor PROVED
    // that reason on its first real run: spelled whole, the phrase appeared in `code` via THIS
    // VERY ASSERTION and the floor fired on itself. A self-referential scan is the trap, not an
    // edge case of one.
    let comment_only = format!("{}{}", "THE ONE STEP THIS FILE", " CANNOT EXERCISE");
    assert!(
        !code.contains(&comment_only),
        "the comment filter kept the doc header, so the scan is judging prose that deliberately \
         names every banned symbol and this test would fail for the wrong reason"
    );
    // ...and the needles are genuinely assembled: a half is not the whole.
    assert!(banned.iter().any(|b| b.len() > 12), "the needles did not assemble: {banned:?}");
}
