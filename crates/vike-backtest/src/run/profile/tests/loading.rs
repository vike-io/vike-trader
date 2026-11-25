//! The loader doors: the parsed text handed back, and `validate_all` accumulating diagnostics.

use super::*;
use std::assert_matches;

/// The text a profile was parsed FROM comes back with it, so the run record can store the bytes
/// that actually drove the run rather than re-reading a file that may have changed since.
#[test]
fn loading_a_profile_hands_back_the_exact_text_it_parsed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sma.toml");
    std::fs::write(&path, BAR_TOML).unwrap();

    let (profile, text) = ProfileBacktest::from_path_with_text(&path).unwrap();

    assert_eq!(text, BAR_TOML, "byte for byte, comments and blank lines included");
    assert_eq!(profile.data.interval, "1h");
    assert!(profile.base_dir.is_some(), "and it still records the sidecar base directory");
}

/// The old door keeps working and keeps meaning the same thing — it has callers this plan does
/// not touch, and it must not grow a second parse.
#[test]
fn the_path_loader_still_answers_and_delegates_to_the_same_parse() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sma.toml");
    std::fs::write(&path, BAR_TOML).unwrap();

    let only =
        ProfileBacktest::from_path(&path).unwrap().with_stand_in(vike_strategy_lab::stand_in);
    let (both, _) = ProfileBacktest::from_path_with_text(&path).unwrap();

    assert_eq!(only.data.interval, both.data.interval);
    assert_eq!(only.base_dir, both.base_dir);
}

/// A profile with mistakes in four `[engine]` keys and one in `[data]`, kept deliberately
/// PARSEABLE so every one of them is semantic: this is what the accumulating door exists for,
/// and the exact KEY SEQUENCE is asserted rather than a count, because the sequence is the
/// contract `ProfileBacktest::validate` reads element zero of.
const FIVE_MISTAKES_TOML: &str = r#"
name = "five-mistakes"

[data]
venue = "binance"
symbols = ["BTCUSDT"]
kind = "bar"
interval = "1h"
from = "2026-01-02T00"
to = "2026-01-01T00"

[engine]
cash = 0.0
feed_latency = true
maint_margin = -1.0
multiplier = 0.0

[strategy]
file = "trend/sma_cross.rs"
"#;

/// ⚠ **Five mistakes, ONE pass** — the whole point of the second door. Before it existed an
/// operator learned about `engine.cash`, fixed it, learned about `engine.feed_latency`, fixed
/// that, and paid five edit-and-rerun cycles for one file; on the remote route each of those
/// was also a dial to a backtesthub.
#[test]
fn validate_all_reports_every_mistake_in_one_pass() {
    let p: ProfileBacktest =
        toml::from_str(FIVE_MISTAKES_TOML).expect("it PARSES — every mistake here is semantic");
    let all = p.validate_all();
    let keys: Vec<&str> = all.iter().map(|d| d.key.as_str()).collect();
    assert_eq!(
        keys,
        [
            "engine.cash",
            "engine.feed_latency",
            "engine.maint_margin",
            "engine.multiplier",
            "data.from",
        ],
        "every rule that refused, in the order the rules run: {all:?}"
    );
    assert!(
        all.iter().all(|d| d.severity == vike_model::DiagnosticSeverity::Error),
        "a load-time refusal is all this validator has to say, so every row is an Error: \
             {all:?}"
    );
    assert!(
        all[3].message.contains("scales every position's notional"),
        "and each row carries the validator's OWN sentence, not a summary of it: {:?}",
        all[3].message
    );
}

/// ⚠ **The first diagnostic is the error the first-error door returns.** The two doors are one
/// walk, so they cannot disagree about which mistake governs — and if a future edit reorders a
/// rule, this is what goes red rather than the published surface (which sorts its refusal rows
/// by message text and would notice nothing).
#[test]
fn the_first_diagnostic_is_what_validate_itself_returns() {
    let p: ProfileBacktest = toml::from_str(FIVE_MISTAKES_TOML).expect("it parses");
    let err = p.validate().expect_err("five mistakes is not zero mistakes");
    let all = p.validate_all();
    assert_eq!(all[0].key, "engine.cash");
    assert_eq!(all[0].message, err.message(), "same walk, same governing refusal");
}

/// ⚠ **The accumulator carries the real [`HarnessError`], which is why the VARIANT survives.**
/// An unparseable stamp is a [`HarnessError::Parse`] — `rejects_unparsable_timestamp` asserts
/// exactly that — and rebuilding an error from a `Diagnostic`'s text would have quietly turned
/// it into a `Validation`, changing what every existing caller sees for a profile nobody
/// edited.
#[test]
fn the_accumulating_door_does_not_change_the_error_variant() {
    let toml = BAR_TOML.replace("from = \"2026-01-01T00\"", "from = \"not-a-timestamp\"");
    let p: ProfileBacktest = toml::from_str(&toml).expect("it parses; the stamp is semantic");
    let err = p.validate().expect_err("an unparseable stamp is refused");
    assert_matches!(err, HarnessError::Parse(_), "got {err:?}");
    let all = p.validate_all();
    assert_eq!(all.len(), 1, "one mistake, one diagnostic: {all:?}");
    assert_eq!(all[0].key, "data", "anchored on the table, since the range spans two keys");
    assert_eq!(all[0].message, err.message());
}

/// A profile that loads collects nothing — the emptiness is the success signal, so a caller
/// can drive the accumulating door alone and never ask the other one.
#[test]
fn a_profile_that_loads_collects_no_diagnostics() {
    let p = ProfileBacktest::from_toml_str(BAR_TOML)
        .expect("the fixture loads")
        .with_stand_in(vike_strategy_lab::stand_in);
    assert!(p.validate_all().is_empty(), "{:?}", p.validate_all());
}

/// ⚠ A refused `[data]` slice SKIPS the `engine.multipliers` key check rather than answering
/// it from wreckage: that rule names the loaded symbols back, and a "the data slice is: "
/// listing built from a malformed slice would be a second, wrong refusal chasing the first.
/// One diagnostic, not two.
#[test]
fn a_refused_slice_does_not_produce_a_second_refusal_about_the_symbols_it_never_resolved() {
    let toml = BAR_TOML
        .replace("\"BTCUSDT\", \"ETHUSDT\"", "\"BTCUSDT\", \"BTCUSDT\"")
        .replace("# engine", "# engine\nmultipliers = { NOPE = 50.0 }");
    let p: ProfileBacktest = toml::from_str(&toml).expect("it parses; the duplicate is semantic");
    let all = p.validate_all();
    assert_eq!(all.len(), 1, "the duplicate symbol alone: {all:?}");
    assert_eq!(all[0].key, "data");
    assert!(all[0].message.contains("duplicate symbol"), "{:?}", all[0].message);
}

/// A strategy file's header the way a shipped file opens, declaring the two keys `BAR_TOML` hands it.
const HEADER: &str = "//! ```toml\n//! [strategy]\n//! runs_on = \"backtest\"\n//! [params.fast]\n\
                      //! type = \"integer\"\n//! [params.slow]\n//! type = \"integer\"\n\
                      //! range = [1, 50]\n//! ```\n";

/// `BAR_TOML` with its `[strategy] file = …` line replaced by `line`.
fn selecting(line: &str) -> String {
    BAR_TOML.replace("file = \"trend/sma_cross.rs\"", line)
}

/// The retired `name` key is refused BEFORE the typed read, with the line to write instead: the
/// file the scan finds where the door knows the folder, the `<name>.rs` to look for where it does
/// not. `name = "rhai"` (an inline script) and the daemon's `rhai =` are refused the same way.
#[test]
fn a_retired_selection_key_is_refused_with_the_line_to_write() {
    let refused = |toml: &str| match ProfileBacktest::from_toml_str(toml) {
        Err(HarnessError::Validation(m)) => m.to_string(),
        other => panic!("expected a validation refusal, got {other:?}"),
    };
    let named = selecting("name = \"sma_cross\"");
    assert_eq!(
        refused(&named),
        "[strategy] name = \"sma_cross\" is no longer read: a profile names its strategy's file. \
         Write [strategy] file = \"<folder>/sma_cross.rs\""
    );
    assert!(refused(&selecting("name = \"rhai\"")).contains(".rhai file"));
    assert!(refused(&selecting("rhai = \"x.rhai\"")).starts_with("[strategy] rhai = \"x.rhai\""));

    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("trend")).unwrap();
    std::fs::write(dir.path().join("trend/sma_cross.rs"), HEADER).unwrap();
    match ProfileBacktest::from_toml_str_in(&named, dir.path()) {
        Err(HarnessError::Validation(m)) => {
            let m = m.to_string();
            assert!(m.ends_with("Write [strategy] file = \"trend/sma_cross.rs\""), "{m}");
        }
        other => panic!("expected a validation refusal, got {other:?}"),
    }
}

/// `strategy.file` is a path under the `strategies/` folder, `.rs` or `.rhai`: anything else is
/// refused at load, naming the key.
#[test]
fn a_strategy_file_outside_the_folder_or_of_another_kind_is_refused_at_load() {
    for path in ["/etc/passwd.rs", "../x.rs", "trend/../../x.rs", "x.py", "x", ""] {
        refused_at_load(
            &selecting(&format!("file = {path:?}")),
            HarnessError::Validation,
            &["strategy.file"],
        );
    }
    for (path, language) in
        [("trend/a.rs", StrategyFileLanguage::Rust), ("a.rhai", StrategyFileLanguage::Rhai)]
    {
        assert_eq!(
            ProfileStrategy::new(path, Params::new()).language().unwrap(),
            language,
            "{path}"
        );
    }
}

/// `[strategy] sdk = N` loads beside a `.rs` file (the backtest's SDK override, decision 0296), is absent
/// by default, and is refused beside a `.rhai` file, naming the key.
#[test]
fn the_sdk_override_loads_beside_a_rust_file_and_is_refused_beside_a_rhai_file() {
    let pinned =
        ProfileBacktest::from_toml_str(&selecting("file = \"trend/sma_cross.rs\"\nsdk = 3"))
            .expect("loads");
    assert_eq!(pinned.strategy.as_ref().unwrap().sdk, Some(3));
    assert_eq!(
        ProfileBacktest::from_toml_str(BAR_TOML).expect("loads").strategy.as_ref().unwrap().sdk,
        None
    );
    refused_at_load(
        &selecting("file = \"a.rhai\"\nsdk = 3"),
        HarnessError::Validation,
        &["strategy.sdk = 3", ".rhai"],
    );
}

/// A Studio slice's SDK (`StrategySpecWire::File`'s `sdk`) is written into the slice's profile, so its saved
/// run runs again on the same SDK, and a `.rhai` slice naming one is refused as a profile's is.
#[test]
fn a_slices_sdk_is_written_and_a_rhai_slice_naming_one_is_refused() {
    let range = vike_data::TimestampRange { start: Some(0), end: Some(3_600_000) };
    let kind = vike_model::run_request::replay::ReplayKind::Bar;
    let symbols = ["BTCUSDT".to_string()];
    let pinned =
        |file: &str| ProfileStrategy { sdk: Some(4), ..ProfileStrategy::new(file, Params::new()) };
    let slice = |file: &str| {
        ProfileBacktest::for_slice("binance", &symbols, "1h", range, kind, None, pinned(file))
    };
    assert_eq!(slice("trend/a.rs").expect("a .rs slice").strategy.as_ref().unwrap().sdk, Some(4));
    let text = ProfileBacktest::slice_text(
        "binance",
        &symbols,
        "1h",
        range,
        kind,
        None,
        &pinned("trend/a.rs"),
    )
    .expect("the slice's text");
    assert!(text.contains("sdk = 4"), "{text}");
    let Err(refused) = slice("a.rhai") else { panic!("a .rhai slice naming an SDK loaded") };
    assert!(refused.message().contains("strategy.sdk = 4"), "{refused:?}");
}

/// The door that holds a project reads the file: its text, its sha256 (lowercase hex, the FIPS
/// 180-2 vector pins the spelling) and its header's backtest word. A file that is missing or whose
/// header does not parse is refused naming it.
#[test]
fn a_door_reads_the_strategy_file_and_records_its_sha256() {
    assert_eq!(
        StrategySource::of("abc".to_string()).sha256,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    let dir = tempfile::tempdir().unwrap();
    let text = format!("{HEADER}fn on_bar() {{}}\n");
    std::fs::write(dir.path().join("demo.rhai"), &text).unwrap();
    let read = |file: &str| {
        let mut p =
            ProfileBacktest::from_toml_str_in(&selecting(&format!("file = {file:?}")), dir.path())?;
        p.strategy.as_mut().unwrap().read_file(dir.path()).map(|()| p)
    };
    let p = read("demo.rhai").expect("the file is read");
    let source = p.strategy.as_ref().unwrap().source.as_ref().expect("read");
    assert_eq!(source.text, text);
    assert_eq!(source.sha256, StrategySource::of(text.clone()).sha256);
    assert_eq!(p.strategy.as_ref().unwrap().sha256(), source.sha256);
    assert!(!source.optimistic_quotes);

    let missing = read("gone.rhai");
    assert_matches!(missing, Err(HarnessError::Validation(m)) if m.to_string().contains("cannot read"));
    std::fs::write(dir.path().join("bare.rhai"), "fn on_bar() {}\n").unwrap();
    let bare = read("bare.rhai");
    assert_matches!(bare, Err(HarnessError::Validation(m)) if m.to_string().starts_with("strategy.file \"bare.rhai\": "));
}

/// The header's load-time refusals over the params: an undeclared key, a value of the wrong type
/// and one outside its range are each refused naming the key, never run on a default.
#[test]
fn a_strategy_file_refuses_params_its_header_does_not_take() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("demo.rhai"), format!("{HEADER}fn on_bar() {{}}\n")).unwrap();
    let read = |params: &str| {
        let toml = selecting("file = \"demo.rhai\"").replace("fast = 10\nslow = 20", params);
        let mut p = ProfileBacktest::from_toml_str(&toml).expect("loads");
        p.strategy.as_mut().unwrap().read_file(dir.path()).map(|()| p)
    };
    read("fast = 10\nslow = 20").expect("declared keys of the right type, in range");
    for (params, needle) in [
        ("fast = 10\nsize = 2.0", "size not declared"),
        ("fast = \"ten\"", "`fast` wants an integer"),
        ("slow = 900", "`slow` wants a value in [1, 50]"),
    ] {
        match read(params) {
            Err(HarnessError::Validation(m)) => {
                assert!(m.to_string().contains(needle), "{params}: {m}")
            }
            other => panic!("{params}: expected a refusal, got {other:?}"),
        }
    }
}

/// `ProfileStrategy::build`: a `.rhai` file's text compiles here, a `.rhai` file no door read and a
/// `.rs` file with no sandbox route are refused, and a builder a door set wins over both.
#[test]
fn build_compiles_a_read_rhai_file_and_never_a_rust_file() {
    let refusal = |config: &ProfileStrategy| match config.build() {
        Err(HarnessError::Validation(m)) => m.to_string(),
        Err(other) => panic!("expected a validation refusal, got {other:?}"),
        Ok(_) => panic!("expected a refusal, it built"),
    };
    let rust = ProfileStrategy::new("trend/buy_hold.rs", Params::new());
    assert!(refusal(&rust).contains("runs only in a sandboxed child process"));
    let unread = ProfileStrategy::new("demo.rhai", Params::new());
    assert!(refusal(&unread).contains("was not read"));
    let read = ProfileStrategy {
        source: Some(StrategySource::of("fn on_bar() {}".to_string())),
        ..unread.clone()
    };
    assert!(read.build().is_ok());
    let broken =
        ProfileStrategy { source: Some(StrategySource::of("fn on_bar( {".to_string())), ..unread };
    assert!(refusal(&broken).contains("rhai compile error"));
    let standing_in = ProfileBacktest::from_toml_str(&selecting("file = \"trend/buy_hold.rs\""))
        .expect("loads")
        .with_stand_in(vike_strategy_lab::stand_in);
    assert!(
        standing_in.strategy.as_ref().unwrap().build().is_ok(),
        "the stand-in builds the compiled twin"
    );
}

/// A two-mount portfolio over `BAR_TOML`'s slice, the second mount sharing the first's slot: the profile every
/// `[[mounts]]` test below perturbs.
const MOUNTS_TOML: &str = r#"
[data]
venue = "binance"
symbols = ["BTCUSDT", "ETHUSDT"]
kind = "bar"
interval = "1h"
from = "2026-01-01T00"
to = "2026-01-02T00"

[engine]
cash = 100000.0
max_mount_leverage = 3.0
# engine

[[mounts]]
symbols = ["BTCUSDT"]
max_leverage = 2.0
max_loss = 1500.0
[mounts.strategy]
file = "trend/sma_cross.rs"
[mounts.strategy.params]
fast = 10

[[mounts]]
symbols = ["ETHUSDT", "BTCUSDT"]
[mounts.strategy]
file = "trend/buy_hold.rs"
"#;

/// Every `(key, message)` a typed read of `toml` collects ([`ProfileBacktest::validate_all`]).
fn diagnostics_of(toml: &str) -> Vec<(String, String)> {
    let p: ProfileBacktest =
        toml::from_str(toml).unwrap_or_else(|e| panic!("it parses: {e}\n{toml}"));
    p.validate_all().into_iter().map(|d| (d.key, d.message)).collect()
}

/// A portfolio loads, a slot two mounts name included, and is read in mount order: its strategies, the table
/// whose header laws the run takes (the first mount's), and its identity (the files joined, a hash over each
/// file and its hash once the files are read). A route that runs one strategy refuses it in words.
#[test]
fn a_portfolio_loads_and_reads_its_strategies_in_mount_order() {
    let p = ProfileBacktest::from_toml_str(MOUNTS_TOML).expect("a portfolio loads");
    assert!(p.is_portfolio() && p.strategy.is_none());
    let files: Vec<&str> = p.strategies().map(|s| s.file.as_str()).collect();
    assert_eq!(files, ["trend/sma_cross.rs", "trend/buy_hold.rs"]);
    assert_eq!(p.law_strategy().map(|s| s.file.as_str()), Some("trend/sma_cross.rs"));
    assert_eq!(p.mounts[1].symbols, ["ETHUSDT", "BTCUSDT"], "a slot two mounts name loads");
    assert_eq!((p.mounts[0].max_leverage, p.mounts[0].max_loss), (Some(2.0), Some(1500.0)));
    assert!(p.one_strategy().unwrap_err().message().contains("names [[mounts]]"));
    let joined = "trend/sma_cross.rs + trend/buy_hold.rs".to_string();
    assert_eq!(p.strategy_identity(), (joined.clone(), String::new(), None), "no file read");
    let mut read = p.clone();
    for strategy in read.strategies_mut() {
        strategy.source = Some(StrategySource::of(format!("// {}", strategy.file)));
    }
    let listed: String =
        read.strategies().map(|s| format!("{}\t{}\n", s.file, s.sha256())).collect();
    assert_eq!(read.strategy_identity(), (joined, StrategySource::of(listed).sha256, None));
    let one = ProfileBacktest::from_toml_str(BAR_TOML).expect("loads");
    assert_eq!(one.strategy_identity(), ("trend/sma_cross.rs".to_string(), String::new(), None));
    assert!(!one.is_portfolio() && one.one_strategy().is_ok());
}

/// Every load-time `[[mounts]]` rule refuses naming its key: what a mount names, its two budget figures, the
/// slots (a mount names only the slice's, and every slot is a mount's), and every route or key that runs one
/// strategy or cannot run several.
#[test]
fn every_mount_rule_refuses_naming_its_key() {
    let engine = |line: &str| MOUNTS_TOML.replace("# engine", &format!("# engine\n{line}"));
    let first = |from: &str, to: &str| MOUNTS_TOML.replacen(from, to, 1);
    let cases: Vec<(String, &str, &str)> = vec![
        (
            MOUNTS_TOML.split("[[mounts]]").next().unwrap_or_default().to_string(),
            "strategy",
            "a profile names what it trades",
        ),
        (
            first("symbols = [\"BTCUSDT\"]\n", "symbols = []\n"),
            "mounts.symbols",
            "mounts[0].symbols is empty",
        ),
        (
            first("[\"ETHUSDT\", \"BTCUSDT\"]", "[\"ETHUSDT\", \"ETHUSDT\", \"BTCUSDT\"]"),
            "mounts.symbols",
            "mounts[1].symbols names \"ETHUSDT\" twice",
        ),
        (first("max_leverage = 2.0", "max_leverage = 0.5"), "mounts.max_leverage", "at least 1"),
        (
            first("max_leverage = 2.0", "max_leverage = 4.0"),
            "mounts.max_leverage",
            "above engine.max_mount_leverage = 3",
        ),
        (first("max_loss = 1500.0", "max_loss = 0.0"), "mounts.max_loss", "mounts[0].max_loss"),
        (
            first("symbols = [\"BTCUSDT\"]\n", "symbols = [\"SOLUSDT\"]\n"),
            "mounts.symbols",
            "\"SOLUSDT\", which is no slot of the data slice",
        ),
        (
            first("[\"BTCUSDT\", \"ETHUSDT\"]", "[\"BTCUSDT\", \"ETHUSDT\", \"SOLUSDT\"]"),
            "mounts",
            "\"SOLUSDT\" is traded by no mount",
        ),
        (format!("{MOUNTS_TOML}\n[walkforward]\nn_splits = 2\n"), "mounts", "beside [walkforward]"),
        (format!("{MOUNTS_TOML}\n[paramscan]\nfast = [5, 10]\n"), "mounts", "beside a [paramscan]"),
        (
            format!("{MOUNTS_TOML}\n[permutation]\npermutations = 10\n"),
            "mounts",
            "beside [permutation]",
        ),
        (engine("decide = \"simultaneous\""), "mounts", "beside engine.decide"),
        (first("kind = \"bar\"", "kind = \"tick\""), "mounts", "on the tick lane"),
        (
            engine("in_bar = { fill_at_close = true }"),
            "engine.in_bar",
            "fill_at_close with two or more mounts",
        ),
        (
            engine("in_bar = { committed_stops = true }"),
            "engine.in_bar",
            "committed_stops with two or more mounts",
        ),
        (engine("closed_trades = false"), "engine.closed_trades", "each mount's trades"),
        (
            first("file = \"trend/buy_hold.rs\"", "file = \"x.py\""),
            "mounts.strategy.file",
            "mounts[1].strategy.file",
        ),
        (
            first("file = \"trend/buy_hold.rs\"", "file = \"b.rhai\"\nsdk = 3"),
            "mounts.strategy.sdk",
            "mounts[1].strategy.sdk = 3",
        ),
    ];
    for (toml, key, needle) in cases {
        let all = diagnostics_of(&toml);
        assert!(
            all.iter().any(|(k, m)| k == key && m.contains(needle)),
            "{key}: {needle:?} is not among {all:?}"
        );
    }
    // The two in-bar rules are refused only beside a SECOND mount: one mount's book is its own.
    for rule in ["fill_at_close", "committed_stops"] {
        let mut alone: ProfileBacktest =
            toml::from_str(&engine(&format!("in_bar = {{ {rule} = true }}"))).expect("parses");
        alone.mounts.truncate(1);
        let messages: Vec<String> = alone.validate_all().into_iter().map(|d| d.message).collect();
        assert!(messages.iter().all(|m| !m.contains(rule)), "{rule}, one mount: {messages:?}");
    }
    // Both ways at once is a hand-built profile's (the text's is refused before the typed read, below).
    let mut both = ProfileBacktest::from_toml_str(MOUNTS_TOML).expect("loads");
    both.strategy = Some(both.mounts[0].strategy.clone());
    assert!(
        both.validate_all().iter().any(|d| d.key == "strategy" && d.message.contains("two ways"))
    );
}

/// `[strategy]` beside `[[mounts]]` is refused BEFORE the typed read (P1), naming the CLI flags and the MCP
/// argument that write `[strategy]`: a `--param`-shaped `[strategy.params]` alone would otherwise meet serde's
/// "missing field `file`".
#[test]
fn a_strategy_table_beside_mounts_is_refused_before_the_typed_read() {
    let param = format!("{MOUNTS_TOML}\n[strategy.params]\nfast = 5\n");
    refused_at_load(&param, HarnessError::Validation, &["names no [strategy] table", "--param"]);
}

/// A strategy that trades nothing and reads `inputs` beside its bars.
struct Idle(Vec<String>);

impl vike_model::Strategy<vike_simulator::OrderBrokerSimulator> for Idle {
    fn inputs(&self) -> Vec<String> {
        self.0.clone()
    }
    fn on_bar(&mut self, _: &mut vike_simulator::OrderBrokerSimulator, _: &vike_marketdata::Bar) {}
}

/// The run-time rules over what the doors read and the run built: the mounts' header laws (a different
/// `optimistic_quotes`, unless `[engine.quotes]` states one; a `fill = "close"` file beside another mount), their
/// costs (one commission for the run) and their inputs (the union, read whole only when every file is
/// backtest-only, and a mixed portfolio declaring any refused).
#[test]
fn the_mounts_share_one_header_law_one_cost_and_one_input_reading() {
    let p = ProfileBacktest::from_toml_str(MOUNTS_TOML).expect("loads");
    p.mount_laws().expect("no file read: both the plain law");
    let header = |words: &str| {
        StrategySource::of(format!(
            "//! ```toml\n//! [strategy]\n//! {words}\n//! ```\nfn on_bar() {{}}\n"
        ))
    };
    let mut maker = p.clone();
    maker.mounts[1].strategy.source = Some(header("optimistic_quotes = true"));
    assert!(maker.mount_laws().unwrap_err().message().contains("different laws"));
    maker.engine.quotes = Some(QuotesConfig { optimistic: true });
    maker.mount_laws().expect("a stated [engine.quotes] is every mount's law");
    let mut close = p.clone();
    close.mounts[0].strategy.source = Some(header("fill = \"close\""));
    assert!(close.mount_laws().unwrap_err().message().contains("fill = \"close\""));

    let idle =
        |inputs: &[&str]| -> Box<dyn vike_model::Strategy<vike_simulator::OrderBrokerSimulator>> {
            Box::new(Idle(inputs.iter().map(|n| (*n).to_string()).collect()))
        };
    let built = [idle(&["binance:SPY:1d"]), idle(&["binance:SPY:1d", "binance:QQQ:1d"])];
    p.mount_costs(&built).expect("neither mount states a cost: one cost, none");
    let mut charged = p.clone();
    let commission = vike_model::params::ParamValue::Float(0.05);
    charged.mounts[0].strategy.params.insert("commission", commission);
    assert!(charged.mount_costs(&built).unwrap_err().message().contains("different commissions"));

    let (inputs, whole) = p.mount_inputs(&built).expect("one reading");
    assert_eq!(inputs, ["binance:SPY:1d", "binance:QQQ:1d"], "each once, in mount order");
    assert!(!whole, "neither file is backtest-only");
    let mut mixed = p.clone();
    mixed.mounts[0].strategy.source = Some(header("runs_on = \"backtest\""));
    assert!(mixed.mount_inputs(&built).unwrap_err().message().contains("backtest-only"));
    for mount in &mut mixed.mounts {
        mount.strategy.source = Some(header("runs_on = \"backtest\""));
    }
    assert!(mixed.mount_inputs(&built).expect("every file backtest-only").1);
}
