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
        ProfileBacktest::from_path(&path).unwrap().with_stand_in(vike_strategies_check::stand_in);
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
        .with_stand_in(vike_strategies_check::stand_in);
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
        .replace("fee_rate = 0.001", "fee_rate = 0.001\nmultipliers = { NOPE = 50.0 }");
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
        Err(HarnessError::Validation(m)) => m,
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
        p.strategy.read_file(dir.path()).map(|()| p)
    };
    let p = read("demo.rhai").expect("the file is read");
    let source = p.strategy.source.as_ref().expect("read");
    assert_eq!(source.text, text);
    assert_eq!(source.sha256, StrategySource::of(text.clone()).sha256);
    assert_eq!(p.strategy.sha256(), source.sha256);
    assert!(!source.optimistic_quotes);

    let missing = read("gone.rhai");
    assert_matches!(missing, Err(HarnessError::Validation(m)) if m.contains("cannot read"));
    std::fs::write(dir.path().join("bare.rhai"), "fn on_bar() {}\n").unwrap();
    let bare = read("bare.rhai");
    assert_matches!(bare, Err(HarnessError::Validation(m)) if m.starts_with("strategy.file \"bare.rhai\": "));
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
        p.strategy.read_file(dir.path()).map(|()| p)
    };
    read("fast = 10\nslow = 20").expect("declared keys of the right type, in range");
    for (params, needle) in [
        ("fast = 10\nsize = 2.0", "size not declared"),
        ("fast = \"ten\"", "`fast` wants an integer"),
        ("slow = 900", "`slow` wants a value in [1, 50]"),
    ] {
        match read(params) {
            Err(HarnessError::Validation(m)) => assert!(m.contains(needle), "{params}: {m}"),
            other => panic!("{params}: expected a refusal, got {other:?}"),
        }
    }
}

/// `ProfileStrategy::build`: a `.rhai` file's text compiles here, a `.rhai` file no door read and a
/// `.rs` file with no sandbox route are refused, and a builder a door set wins over both.
#[test]
fn build_compiles_a_read_rhai_file_and_never_a_rust_file() {
    let refusal = |config: &ProfileStrategy| match config.build() {
        Err(HarnessError::Validation(m)) => m,
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
        .with_stand_in(vike_strategies_check::stand_in);
    assert!(standing_in.strategy.build().is_ok(), "the stand-in builds the compiled twin");
}
