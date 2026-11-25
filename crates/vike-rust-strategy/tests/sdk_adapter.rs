//! **The versioned strategy SDK's step 6, end to end: a file pinned to an OLDER SDK whose boundary moved
//! builds against THIS host's boundary through its adapter, loads here, and trades exactly as the same
//! strategy built against this checkout** (decision 0296, verdict 7; `src/guest.rs`'s `face`,
//! `crates/vike-strategy-builder/src/render.rs`'s `Adapted`).
//!
//! Two synthetic older SDKs, each a copy of this checkout's strategy closure (what
//! `scripts/package_strategy_source.sh` packs) with ONE difference and a boundary of its own:
//! - **SDK 1, kind (a), a renamed call**: `vike_model::OrderBroker::history_only_grows` is still
//!   `inputs_only_grow` (the 12 -> 13 rename, undone across the copy). Its adapter's face is the host's with
//!   one line changed (`tests/fixtures/sdk_adapter/face_renamed.rs`).
//! - **SDK 2, kind (b), a lent row laid out otherwise**: `vike_marketdata::Bar` with `symbol` first (and its
//!   `lent_fields.txt` saying so, as CI makes a real cut's say). It is RETIRED, never adapted (verdict 7, the
//!   owner's "Б" of 2026-11-20): refused in words naming `migrate`, though an adapter for it is held, and
//!   never built. No face can adapt it: every one reads the host's bars in place under `lent_in_place!`, which
//!   does not compile against it: the BACKSTOP, proved by a third such SDK whose golden says nothing moved (a
//!   cut no CI checked), offered an adapter, whose build fails with "is not laid out as the boundary's".
//!
//! SDK 3 is the installed SDK that holds this host's boundary (this checkout's `vike-rust-strategy`, the
//! glue's source); the adapters are the test's, served in place of the ones this binary was built with
//! (`vike_rust_strategy::sdk::with_adapters`). The probe strategy reads its bars' history on every bar and asks whether the
//! history only grows (by the name its SDK spells), then trades a moving-average cross. Built four ways
//! (this checkout; SDK 1 through its adapter; and two CONTROLS, a numbered SDK of this boundary built with no
//! adapter, and an older SDK with this checkout's API adapted by a copy of this checkout's own face,
//! `src/guest/face.rs`, which prices adapting with nothing to adapt), each artifact passes this host's
//! loader (its compatibility record states this boundary) and the runs agree bit for bit. Each is then timed,
//! the runs interleaved in a rotating order, and the medians printed: what a renamed call costs against the
//! controls.
//!
//! ⚠ `#[ignore]`d: it copies the closure three times and compiles three copies. On a lane, in release for the
//! timing (`--release`, so the artifacts are release too: `Profile::of_this_binary`):
//!
//! ```text
//! just the CI box <branch> cargo test -p vike-rust-strategy --release --test integration -- --ignored --nocapture sdk_adapter::
//! ```

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use vike_marketdata::Bar;
use vike_rust_strategy::host::StrategyFileHost;
use vike_rust_strategy::loader;
use vike_rust_strategy::sdk::{
    AdapterSet, BOUNDARY_HASH, ONNX_DEPENDENCY_LINE, RUST_STRATEGY_CRATE_DIR, SdkAdapter,
    SdkMarker, snapshot_dir, with_adapters,
};
use vike_simulator::{OrderBrokerSimulator, StrategyEngine, StrategyEngineConfig};
use vike_strategy_builder::render::{self, InstalledSdks, Profile, build_strategy_file};

// The build script's walk, compiled once in this binary by `tests/source_hash.rs`.
use crate::source_hash::source_hash;

/// The probe, as this checkout's SDK spells it. SDK 1's copy reads `inputs_only_grow` instead.
const PROBE: &str = r#"//! The SDK adapter's probe: a moving-average cross over its own bars' history.
use vike_marketdata::Bar;
use vike_model::{OrderBroker, OrderPlacement, Strategy};

pub struct Probe {
    fast: usize,
    slow: usize,
}

impl<B: OrderBroker> Strategy<B> for Probe {
    fn on_bar(&mut self, broker: &mut B, bar: &Bar) {
        let Some(symbol) = bar.symbol else { return };
        if !broker.history_only_grows() {
            return;
        }
        let bars = broker.bars(&symbol);
        if bars.len() < self.slow {
            return;
        }
        let mean = |n: usize| bars[bars.len() - n..].iter().map(|b| b.close).sum::<f64>() / n as f64;
        let (fast, slow, held) = (mean(self.fast), mean(self.slow), broker.position(&symbol));
        if fast > slow && held <= 0.0 {
            broker.place(&symbol, "", 1, 1.0 - held, OrderPlacement::Market, false);
        } else if fast < slow && held >= 0.0 {
            broker.place(&symbol, "", -1, 1.0 + held, OrderPlacement::Market, false);
        }
    }
}

pub fn build<B: OrderBroker + 'static>(
    _params: &vike_model::params::Params,
) -> Box<dyn Strategy<B> + Send> {
    Box::new(Probe { fast: 8, slow: 34 })
}
"#;

/// The kind (a) adapter's face, and this checkout's own face as the adapter of an older SDK whose API is this
/// checkout's: the control, what adapting costs with no difference to adapt (and the face the retired SDK 2
/// is offered). No adapter ships: SDK 1, whose lent rows held `String` symbols, retired with them (decision
/// 0296's amendment of 2026-11-20, the `Symbol` rows).
const FACE_RENAMED: &str = include_str!("fixtures/sdk_adapter/face_renamed.rs");
const FACE_OF_THIS_CHECKOUT: &str = include_str!("../src/guest/face.rs");

/// The run's bars and the rounds each artifact is timed over.
const N_BARS: usize = 300_000;
const ROUNDS: usize = 21;
const SYMBOL: &str = "BTCUSDT";

fn checkout() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// Copy `from` to `to`, every `.rs` and `.txt` file through `edit` (its root-relative path, its text);
/// `target/` and `.git` stay behind.
fn copy(from: &Path, to: &Path, relative: &str, edit: &dyn Fn(&str, String) -> String) {
    if from.is_dir() {
        for entry in fs::read_dir(from).expect("a folder of the closure").flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == "target" || name == ".git" {
                continue;
            }
            copy(&entry.path(), &to.join(&name), &format!("{relative}/{name}"), edit);
        }
        return;
    }
    fs::create_dir_all(to.parent().expect("a parent")).expect("the folder");
    if relative.ends_with(".rs") || relative.ends_with(".txt") {
        let text = fs::read_to_string(from).expect("a source");
        fs::write(to, edit(relative, text)).expect("the copy");
    } else {
        fs::copy(from, to).expect("the copy");
    }
}

/// Install synthetic SDK `number` under `sdk_root`: this checkout's strategy closure, root manifest and
/// lock (the packer's paths), every source through `edit`, its boundary moved (a line of code in its own
/// `abi.rs`: a comment would not move it), then its marker. Answers its marker.
fn install_older_sdk(
    sdk_root: &Path,
    number: u32,
    edit: &dyn Fn(&str, String) -> String,
) -> SdkMarker {
    install_sdk(sdk_root, number, edit, true)
}

/// [`install_older_sdk`], its boundary `moved` or this host's.
fn install_sdk(
    sdk_root: &Path,
    number: u32,
    edit: &dyn Fn(&str, String) -> String,
    moved: bool,
) -> SdkMarker {
    let (from, dir) = (checkout(), snapshot_dir(sdk_root, number));
    let template =
        fs::read_to_string(from.join(RUST_STRATEGY_CRATE_DIR).join("template/Cargo.toml.in"))
            .expect("the strategy template");
    let template: String = template
        .split_inclusive('\n')
        .filter(|line| !line.starts_with(ONNX_DEPENDENCY_LINE))
        .collect();
    for krate in source_hash::closure(&from, &[template.as_str()]) {
        copy(&from.join(&krate), &dir.join(&krate), &krate, edit);
    }
    for file in ["Cargo.toml", "Cargo.lock"] {
        fs::copy(from.join(file), dir.join(file)).expect("the root manifest and lock");
    }
    if moved {
        let abi = dir.join(RUST_STRATEGY_CRATE_DIR).join("src/abi.rs");
        let older = fs::read_to_string(&abi).expect("its abi.rs")
            + "pub const AN_OLDER_BOUNDARY: u8 = 0;\n";
        fs::write(&abi, older).expect("move its boundary");
    }
    let marker =
        SdkMarker::of_snapshot(&dir, number, "synthetic", vike_rust_strategy::abi::ABI_VERSION);
    assert_eq!(marker.boundary_hash != BOUNDARY_HASH, moved, "SDK {number}'s boundary");
    marker.write(&dir).expect("its marker");
    marker
}

/// Install SDK `number`, the one that holds THIS host's boundary (this checkout's `vike-rust-strategy`, as
/// the glue copies it).
fn install_host_sdk(sdk_root: &Path, number: u32) -> PathBuf {
    let dir = snapshot_dir(sdk_root, number);
    let (from, to) = (checkout().join(RUST_STRATEGY_CRATE_DIR), dir.join(RUST_STRATEGY_CRATE_DIR));
    for part in ["build.rs", "build", "src", "template", "tests/boundary", "adapters", "Cargo.toml"]
    {
        copy(&from.join(part), &to.join(part), part, &|_, text| text);
    }
    // The root manifest: the glue reads there the versions its crate's `workspace = true` lines pin.
    fs::copy(checkout().join("Cargo.toml"), dir.join("Cargo.toml")).expect("the root manifest");
    let marker = SdkMarker::of_snapshot(&dir, number, "host", vike_rust_strategy::abi::ABI_VERSION);
    assert_eq!(marker.boundary_hash, BOUNDARY_HASH, "SDK {number} holds this host's boundary");
    marker.write(&dir).expect("its marker");
    dir
}

/// An adapter of this host from the SDK of `older` (its boundary, as this vike hashes it: the marker this
/// test wrote holds it), with `face`.
fn adapter(older: &SdkMarker, face: &str) -> SdkAdapter {
    SdkAdapter {
        number: older.number,
        boundary_hash: older.boundary_hash.clone(),
        face: face.to_string(),
    }
}

/// Move `Bar`'s `symbol` to the front of the struct, and of its line in the lent-field golden: another
/// layout, the same API.
fn reorder_bar(relative: &str, text: String) -> String {
    if relative == "crates/vike-rust-strategy/tests/boundary/lent_fields.txt" {
        let symbol = text.lines().find(|l| l.starts_with("Bar.symbol:")).expect("Bar's symbol");
        return std::iter::once(symbol)
            .chain(text.lines().filter(|l| *l != symbol))
            .map(|l| format!("{l}\n"))
            .collect();
    }
    if relative != "crates/vike-marketdata/src/bar.rs" {
        return text;
    }
    // The symbol field is last, with its comment and its doc: all of it moves to the front.
    let field = "    #[serde(default)]\n    pub symbol: Option<Symbol>,\n}\n";
    assert_eq!(text.matches(field).count(), 1, "Bar's symbol field, last");
    let end = text.find(field).expect("the field");
    let start = text[..end].rfind("    pub ask: Option<f64>,\n").expect("the field before it")
        + "    pub ask: Option<f64>,\n".len();
    let head = "pub struct Bar {\n";
    assert_eq!(text.matches(head).count(), 1, "Bar's head");
    let moved = text[start..end + field.len() - "}\n".len()].to_string();
    let text = format!("{}}}\n{}", &text[..start], &text[end + field.len()..]);
    text.replacen(head, &format!("{head}{moved}"), 1)
}

/// Build `text` against the tree at `root` into `builds`, as the daemon does.
fn build(text: &str, name: &str, root: &Path, builds: &Path) -> PathBuf {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let child_env = render::child_env(&std::env::vars().collect());
    let started = Instant::now();
    let built = build_strategy_file(
        text,
        &format!("{name}.rs"),
        builds,
        root,
        &cargo,
        &child_env,
        Profile::of_this_binary(),
        &render::CargoHomePin::disabled(),
    )
    .unwrap_or_else(|e| {
        panic!("the builder must build the probe against {}:\n{e}", root.display())
    });
    eprintln!("built {name} in {:.1} s: {}", started.elapsed().as_secs_f64(), built.display());
    built
}

/// The run's bars: a slow wave and a fast one, so the cross trades often.
fn bars() -> Vec<Bar> {
    (0..N_BARS)
        .map(|i| {
            let t = i as f64;
            let close = 100.0 + 10.0 * (t / 400.0).sin() + 2.0 * (t / 37.0).sin();
            let mut bar =
                Bar::ohlcv(i as i64 * 60_000, close, close + 0.5, close - 0.5, close, 10.0);
            bar.symbol = Some(SYMBOL.into());
            bar
        })
        .collect()
}

/// One run of the loaded `vtable` over `bars`: what it traded (its trades, equity curve and final equity,
/// as text, so a last-bit difference shows) and how long it took.
fn run(vtable: vike_rust_strategy::host::StrategyVTableAbi, bars: &[Bar]) -> (String, Duration) {
    let strategy = StrategyFileHost::<OrderBrokerSimulator>::new(vtable, "", "{}");
    let config = StrategyEngineConfig { cash: 100_000.0, fee_rate: 0.000_6, ..Default::default() };
    let mut engine =
        StrategyEngine::new(vec![(SYMBOL.to_string(), bars.to_vec())], strategy, config);
    let started = Instant::now();
    let record = engine.run();
    let took = started.elapsed();
    let traded = format!("{:?}", (&record.trades, &record.equity_curve, record.final_equity));
    assert!(record.n_trades > 10, "the probe trades ({} trades)", record.n_trades);
    (traded, took)
}

fn median(mut times: Vec<Duration>) -> Duration {
    times.sort();
    times[times.len() / 2]
}

#[test]
#[ignore = "copies and compiles two SDK closures; lane-only"]
fn an_older_sdk_builds_through_its_adapter_and_trades_as_this_one() {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("sdk-adapter-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let sdk_root = root.join("vike-strategy-source");
    // The builds OUTSIDE this checkout: a numbered SDK works in `<builds>/sdk-N/work-…`, and cargo reads every
    // `.cargo/config.toml` above its work directory, which the build jail does not grant inside a checkout.
    let builds = render::scratch_root().join(format!("sdk-adapter-{}", std::process::id()));
    let _ = fs::remove_dir_all(&builds);

    let renamed = install_older_sdk(&sdk_root, 1, &|_, text| {
        text.replace("history_only_grows", "inputs_only_grow")
    });
    let reordered = install_older_sdk(&sdk_root, 2, &reorder_bar);
    install_host_sdk(&sdk_root, 3);
    // The controls, in a root of their own (the window holds two older SDKs): SDK 2, a whole numbered SDK of
    // THIS boundary, built as any such SDK is (no adapter); and SDK 1, an older boundary with this checkout's
    // API, through a copy of this checkout's own face (adapting, with nothing to adapt).
    let control_root = root.join("control");
    let same = install_older_sdk(&control_root, 1, &|_, text| text);
    install_sdk(&control_root, 2, &|_, text| text, false);
    // The backstop's root: an SDK whose `Bar` moved while its golden says it did not (a cut no CI checked).
    let stale_root = root.join("stale");
    let stale = install_older_sdk(&stale_root, 1, &|relative, text| {
        if relative.ends_with("lent_fields.txt") { text } else { reorder_bar(relative, text) }
    });
    install_host_sdk(&stale_root, 2);
    // This host's adapters, the test's: each found by its SDK's number AND boundary, so the renamed SDK 1 and
    // the controls' (whose boundary the stale SDK shares, its golden unmoved) take each its own face.
    assert_ne!(renamed.boundary_hash, same.boundary_hash, "the rename reaches the boundary's face");
    assert_eq!(
        stale.boundary_hash, same.boundary_hash,
        "the stale SDK's boundary is the control's"
    );
    let held = vec![
        adapter(&renamed, FACE_RENAMED),
        adapter(&reordered, FACE_OF_THIS_CHECKOUT),
        adapter(&same, FACE_OF_THIS_CHECKOUT),
    ];
    with_adapters(AdapterSet { adapters: held, broken: Vec::new() }, || {
        check(&sdk_root, &control_root, &stale_root, &builds);
    });
    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&builds);
}

/// The test's body, run with its adapters served (`with_adapters`): resolve, refuse, build, load, run, time.
fn check(sdk_root: &Path, control_root: &Path, stale_root: &Path, builds: &Path) {
    let (sdk_root, control_root) = (sdk_root.to_path_buf(), control_root.to_path_buf());
    let (stale_root, builds) = (stale_root.to_path_buf(), builds.to_path_buf());
    // The daemon's resolution: SDK 1 resolves to its snapshot through its adapter; SDK 2, whose lent row
    // moved, is retired though its adapter is held, and a build of it is refused before cargo runs.
    let sdks = InstalledSdks::open(&sdk_root).expect("the installed SDKs");
    let report = sdks.report().join("\n");
    assert_eq!(report.matches("through its adapter").count(), 1, "{report}");
    assert_eq!(sdks.resolve(None, Some(1)).ok(), Some(snapshot_dir(&sdk_root, 1)));
    let why = sdks.resolve(None, Some(2)).expect_err("SDK 2's rows moved").to_string();
    assert!(why.contains("SDK 2 is retired") && why.contains("migrate <FILE> --to 3"), "{why}");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let refused = build_strategy_file(
        PROBE,
        "probe_reordered.rs",
        &builds,
        &snapshot_dir(&sdk_root, 2),
        &cargo,
        &render::child_env(&std::env::vars().collect()),
        Profile::of_this_binary(),
        &render::CargoHomePin::disabled(),
    )
    .expect_err("a build against SDK 2 is refused");
    assert!(
        matches!(&refused, render::BuildError::Sdk { why } if why.contains("is retired") && why.contains("migrate")),
        "{refused:?}"
    );
    assert!(!builds.join("sdk-2").exists(), "nothing was built for the retired SDK");

    let probe_one = PROBE.replace("history_only_grows", "inputs_only_grow");
    let artifacts = [
        ("this checkout", build(PROBE, "probe", &checkout(), &builds)),
        (
            "SDK 1, kind (a)",
            build(&probe_one, "probe_renamed", &snapshot_dir(&sdk_root, 1), &builds),
        ),
        (
            "control: a numbered SDK of this boundary, no adapter",
            build(PROBE, "probe_numbered", &snapshot_dir(&control_root, 2), &builds),
        ),
        (
            "control: an older boundary of this API, this checkout's face as its adapter",
            build(PROBE, "probe_control", &snapshot_dir(&control_root, 1), &builds),
        ),
    ];
    for (name, artifact) in &artifacts[1..] {
        let sdk = artifact.parent().and_then(Path::parent).and_then(Path::file_name);
        assert!(
            sdk.is_some_and(|sdk| sdk != "sdk-dev"),
            "{name}'s artifact lies in its own SDK's builds"
        );
    }
    // Each artifact's compatibility record states this host's boundary, so this host loads all of them.
    let loaded: Vec<_> = artifacts
        .iter()
        .map(|(name, artifact)| {
            let loaded = loader::load(artifact)
                .unwrap_or_else(|e| panic!("this host must load {name}'s artifact: {e}"));
            (*name, loaded)
        })
        .collect();

    // The backstop: an SDK whose `Bar` moved while its golden says it did not (a cut no CI checked) is offered
    // an adapter; the rule cannot tell, so the build runs, and `lent_in_place!` refuses it at compile time.
    let refused = build_strategy_file(
        PROBE,
        "probe_stale.rs",
        &builds,
        &snapshot_dir(&stale_root, 1),
        &cargo,
        &render::child_env(&std::env::vars().collect()),
        Profile::of_this_binary(),
        &render::CargoHomePin::disabled(),
    )
    .expect_err("an SDK whose Bar moved does not build");
    let words = refused.to_string();
    assert!(
        matches!(refused, render::BuildError::Compile { .. })
            && words.contains("is not laid out as the boundary's"),
        "{}",
        words.chars().take(4000).collect::<String>()
    );

    let bars = bars();
    let mut traded: Vec<Option<String>> = vec![None; loaded.len()];
    let mut times: Vec<Vec<Duration>> = vec![Vec::new(); loaded.len()];
    // Each round starts at another artifact, so no one always runs after another.
    for round in 0..ROUNDS {
        for k in 0..loaded.len() {
            let i = (round + k) % loaded.len();
            let (name, file) = &loaded[i];
            let (what, took) = run(file.vtable, &bars);
            if let Some(first) = &traded[i] {
                assert_eq!(*first, what, "{name} trades alike every run");
            }
            traded[i] = Some(what);
            times[i].push(took);
        }
    }
    for (i, (name, _)) in loaded.iter().enumerate().skip(1) {
        assert_eq!(traded[i], traded[0], "{name} trades exactly as this checkout's SDK");
    }
    let base = median(times[0].clone()).as_secs_f64();
    for (i, (name, _)) in loaded.iter().enumerate() {
        let med = median(times[i].clone()).as_secs_f64();
        let (fastest, slowest) = (times[i].iter().min(), times[i].iter().max());
        eprintln!(
            "{name}: median {:.1} ms over {ROUNDS} runs of {N_BARS} bars, {:.0} ns a bar ({:+.0} ns a bar, \
             {:+.1}%, against this checkout; fastest {:.1} ms, slowest {:.1} ms)",
            med * 1e3,
            med * 1e9 / N_BARS as f64,
            (med - base) * 1e9 / N_BARS as f64,
            (med / base - 1.0) * 100.0,
            fastest.map_or(0.0, |t| t.as_secs_f64() * 1e3),
            slowest.map_or(0.0, |t| t.as_secs_f64() * 1e3),
        );
    }
}
