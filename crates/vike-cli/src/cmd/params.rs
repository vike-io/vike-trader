//! `vike-cli backtest params [--script PATH | --strategy FILE]` — **what knobs exist**, OFFLINE
//! (spec §8.5). No server, no store, no engine.
//!
//! Two sources, one verb, because the question is the same one: `--script` discovers a Rhai file's
//! `param(name, default)` calls through `vike_rhai::discover_params` (this is `backtest
//! --list-params`, re-homed), and `--strategy` reads a strategy FILE's header, its `[params.*]`
//! declarations (`vike_strategy::file::StrategyFileHeader`): the file is the path under the
//! project's `strategies/` folder a profile's `[strategy] file` names.
//!
//! ⚠ **The two answers are different shapes and the `--json` document says which it is.** A Rhai
//! knob's default is an `f64` — `discover_params` returns `Vec<(String, f64)>`, so a `param()` is
//! numeric-only — while a declared key carries its header TYPE and, where the header states one, a
//! default of that type. One document with a `source` discriminator beats two documents nobody can
//! tell apart.
//!
//! ⚠ A header with no `[params]` declares that the file accepts NO key, and that is what is printed:
//! it is the file's own rule (`vike_strategy::file`'s format note), not a gap in this answer. The
//! missing half §8.5 names — parameters can be SEEN and not SET — is `run --param`.

use std::path::Path;

use vike_strategy::file::{ParamType, StrategyFileHeader};

use crate::exit::{CliError, CommandResult};

/// A header type in the header's own word (`vike_strategy::file`'s format note).
fn type_word(t: ParamType) -> &'static str {
    match t {
        ParamType::Number => "number",
        ParamType::Integer => "integer",
        ParamType::Text => "text",
        ParamType::Bool => "bool",
        ParamType::Table => "table",
        ParamType::TextOrInteger => "text_or_integer",
        ParamType::Array => "array",
    }
}

pub(crate) fn strategy_json(file: &str, header: &StrategyFileHeader) -> String {
    let params: Vec<serde_json::Value> = header
        .params
        .iter()
        .map(|(k, p)| {
            serde_json::json!({
                "name": k,
                "type": type_word(p.r#type),
                "default": p.default.as_ref().map(ToString::to_string),
            })
        })
        .collect();
    let doc = serde_json::json!({ "source": "strategy", "strategy": file, "params": params });
    serde_json::to_string_pretty(&doc).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
}

pub(crate) fn script_json(path: &str, params: &[(String, f64)]) -> String {
    let rows: Vec<serde_json::Value> =
        params.iter().map(|(n, d)| serde_json::json!({ "name": n, "default": d })).collect();
    let doc = serde_json::json!({ "source": "script", "script": path, "params": rows });
    serde_json::to_string_pretty(&doc).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
}

/// `strategies_dir` is the project's `strategies/` folder, `None` with no project above the working
/// directory (then `--strategy` names nothing it could read, and says so).
pub(crate) fn run_params(
    script: Option<&str>,
    strategy: Option<&str>,
    strategies_dir: Option<&Path>,
    json: bool,
) -> CommandResult<()> {
    match (script, strategy) {
        (Some(_), Some(_)) => Err(CliError::usage(
            "--script and --strategy name two different sources for the same question — give \
             exactly one",
        )),
        (None, None) => Err(CliError::usage(
            "`backtest params` needs a source: --script <s.rhai> for a script's param() knobs, or \
             --strategy <file> for a strategy file's declared keys",
        )),
        (Some(path), None) => run_script(path, json),
        (None, Some(file)) => run_strategy(file, strategies_dir, json),
    }
}

fn run_script(path: &str, json: bool) -> CommandResult<()> {
    let src = std::fs::read_to_string(path)
        .map_err(|e| CliError::failed(format!("cannot read script {path}: {e}")))?;
    let params = vike_rhai::discover_params(&src)
        .map_err(|e| CliError::failed(format!("rhai compile error: {e}")))?;
    if json {
        println!("{}", script_json(path, &params));
        return Ok(());
    }
    if params.is_empty() {
        println!("(no tunable params — the script declares no param(name, default) calls)");
    } else {
        for (name, default) in params {
            println!("{name} = {default}");
        }
    }
    Ok(())
}

/// The header of `file` under `strategies_dir`, or the refusal that says which of the three steps
/// failed: no folder, a path outside it, a file that is not there or whose header does not parse.
pub(crate) fn header_of(
    file: &str,
    strategies_dir: Option<&Path>,
) -> CommandResult<StrategyFileHeader> {
    let dir = strategies_dir.ok_or_else(|| {
        CliError::usage(
            "--strategy names a file of the project's strategies/ folder, and there is no project \
             above the working directory",
        )
    })?;
    let path = vike_strategy::file::scan::resolve(dir, file).map_err(CliError::usage)?;
    let text = std::fs::read_to_string(&path)
        .map_err(|e| CliError::failed(format!("cannot read {}: {e}", path.display())))?;
    StrategyFileHeader::parse(&text).map_err(|e| CliError::failed(format!("{file}: {e}")))
}

fn run_strategy(file: &str, strategies_dir: Option<&Path>, json: bool) -> CommandResult<()> {
    let header = header_of(file, strategies_dir)?;
    if json {
        println!("{}", strategy_json(file, &header));
        return Ok(());
    }
    if header.params.is_empty() {
        println!("{file}: accepts no key (its header declares no [params])");
    }
    for (k, p) in &header.params {
        match &p.default {
            Some(d) => println!("{k} : {} = {d}", type_word(p.r#type)),
            None => println!("{k} : {}", type_word(p.r#type)),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "//! ```toml\n//! [strategy]\n//! [params.size]\n//! type = \"number\"\n\
                          //! default = 1.0\n//! [params.symbol]\n//! type = \"text\"\n//! ```\n";

    /// A folder holding `trend/demo.rs` with [`HEADER`].
    fn folder() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("trend")).unwrap();
        std::fs::write(dir.path().join("trend/demo.rs"), HEADER).unwrap();
        dir
    }

    /// A file's declared keys come back in header order, each with its type word and default.
    #[test]
    fn a_strategy_file_answers_with_its_declared_keys() {
        let dir = folder();
        let header = header_of("trend/demo.rs", Some(dir.path())).expect("the header parses");
        let doc: serde_json::Value =
            serde_json::from_str(&strategy_json("trend/demo.rs", &header)).unwrap();
        assert_eq!(doc["source"], serde_json::json!("strategy"));
        assert_eq!(doc["strategy"], serde_json::json!("trend/demo.rs"));
        assert_eq!(doc["params"][0]["name"], serde_json::json!("size"));
        assert_eq!(doc["params"][0]["type"], serde_json::json!("number"));
        assert_eq!(doc["params"][0]["default"], serde_json::json!("1.0"));
        assert_eq!(doc["params"][1]["type"], serde_json::json!("text"));
        run_params(None, Some("trend/demo.rs"), Some(dir.path()), false).expect("prints");
    }

    /// No project, a path outside the folder and a missing file are each refused in words.
    #[test]
    fn a_strategy_file_that_cannot_be_read_is_refused() {
        let e = run_params(None, Some("trend/demo.rs"), None, false).expect_err("no project");
        assert_eq!(e.exit, crate::exit::Exit::Usage);
        assert!(e.message.contains("no project"), "{}", e.message);
        let dir = folder();
        let e = run_params(None, Some("../x.rs"), Some(dir.path()), false).expect_err("outside");
        assert_eq!(e.exit, crate::exit::Exit::Usage);
        let e = run_params(None, Some("trend/gone.rs"), Some(dir.path()), false).expect_err("gone");
        assert!(e.message.contains("cannot read"), "{}", e.message);
    }

    #[test]
    fn exactly_one_of_script_and_strategy_is_required() {
        let e = run_params(None, None, None, false).expect_err("neither");
        assert_eq!(e.exit, crate::exit::Exit::Usage);
        assert!(e.message.contains("--script"), "{}", e.message);
        assert!(e.message.contains("--strategy"), "{}", e.message);

        let e =
            run_params(Some("s.rhai"), Some("trend/buy_hold.rs"), None, false).expect_err("both");
        assert_eq!(e.exit, crate::exit::Exit::Usage);
    }

    /// ⚠ A Rhai knob's default is an `f64` — `vike_rhai::discover_params` returns
    /// `Vec<(String, f64)>` — so the `--json` document emits NUMBERS, never typed values. A declared
    /// key carries its header TYPE instead, and the two documents say which they are so nobody
    /// writes one parser for both.
    #[test]
    fn the_json_documents_declare_which_shape_they_are() {
        let doc: serde_json::Value =
            serde_json::from_str(&script_json("s.rhai", &[("threshold".to_string(), 60.0)]))
                .unwrap();
        assert_eq!(doc["source"], serde_json::json!("script"));
        assert!(doc["params"][0]["default"].is_number(), "a Rhai knob's default is a NUMBER");
    }
}
