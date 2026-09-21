// The PURE half of the user-strategy scan: directory listing -> generated registry source.
//
// `build.rs` `include!`s this file (the `vike-buildinfo` precedent: one source of truth compiled
// into both the build script and the library, so the unit tests below exercise EXACTLY the code
// the build runs — never a re-implementation that can drift). Everything here is a pure function
// of paths and strings: no environment reads, no OUT_DIR knowledge, no cargo directives — those
// stay in `build.rs`, the impure caller.
//
// ## The scanned convention (the tier README's contract)
//
// ```text
// <user_data>/strategies/rust/<name>/
// ├─ <name>.rs        entry file — stem MUST equal the folder name (the rhai tier's rule)
// ├─ strategy.toml    optional manifest: `live = true` opts into live mounting (default sim-only)
// └─ *.toml           presets (loaded by vike-studio-core, not by this scan)
// ```
//
// A malformed folder is an ERROR carried in [`ScanOutcome::errors`] — the caller turns those into
// a failed build naming the path. Silence is how this tier spent months being mistaken for a
// working mechanism.

use std::fs;
use std::path::{Path, PathBuf};

/// One discovered user strategy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedStrategy {
    /// The folder name == registry name == entry-file stem. Validated by [`valid_name`].
    pub name: String,
    /// Absolute path of `<name>.rs`, exactly as found (rendered with forward slashes).
    pub entry: PathBuf,
    /// `strategy.toml`'s `live = true` — absent file or absent key means `false` (sim-only).
    pub live: bool,
}

/// The scan result: strategies found plus every objection, each naming its path.
#[derive(Debug, Default)]
pub struct ScanOutcome {
    pub strategies: Vec<ScannedStrategy>,
    pub errors: Vec<String>,
}

/// A registry name must be a lowercase Rust-identifier-safe token: the generated module is
/// `user_<name>` and the match arm quotes it, so the charset is the whole safety argument.
pub fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('a'..='z'))
        && chars.all(|c| matches!(c, 'a'..='z' | '0'..='9' | '_'))
}

/// Scan `<root>/strategies/rust` (where `root` is a user_data directory). Absent root or absent
/// tier directory is the ordinary empty state — no strategies, no errors. Present-but-malformed
/// content is an error, never a skip.
pub fn scan(user_data_root: &Path) -> ScanOutcome {
    let mut out = ScanOutcome::default();
    let tier = user_data_root.join("strategies").join("rust");
    let entries = match fs::read_dir(&tier) {
        Ok(e) => e,
        Err(_) => return out, // absent tier = empty registry, the CI/default state
    };
    let mut dirs: Vec<PathBuf> =
        entries.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.is_dir()).collect();
    dirs.sort(); // deterministic generation regardless of filesystem order
    for dir in dirs {
        let name = match dir.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => {
                out.errors.push(format!("{}: folder name is not valid UTF-8", dir.display()));
                continue;
            }
        };
        let entry = dir.join(format!("{name}.rs"));
        if !entry.is_file() {
            // A folder with no entry file is a PRESETS-ONLY folder for a built-in (the
            // vike-studio-core `StrategyBody::Native` convention) — not ours, not an error.
            continue;
        }
        if !valid_name(&name) {
            out.errors.push(format!(
                "{}: strategy folder name must match [a-z][a-z0-9_]* (it becomes the registry \
                 name and the generated module name)",
                dir.display()
            ));
            continue;
        }
        let live = match read_live_flag(&dir.join("strategy.toml")) {
            Ok(l) => l,
            Err(e) => {
                out.errors.push(e);
                continue;
            }
        };
        out.strategies.push(ScannedStrategy { name, entry, live });
    }
    out
}

/// Read the optional per-folder manifest. Absent file => `Ok(false)`. A file that exists but does
/// not parse, or whose `live` key is not a boolean, is an error naming the path — an operator who
/// wrote `live = "yes"` believes a live gate is armed.
fn read_live_flag(manifest: &Path) -> Result<bool, String> {
    let text = match fs::read_to_string(manifest) {
        Ok(t) => t,
        Err(_) if !manifest.exists() => return Ok(false),
        Err(e) => return Err(format!("{}: unreadable: {e}", manifest.display())),
    };
    let value: toml::Value = toml::from_str(&text)
        .map_err(|e| format!("{}: not valid TOML: {e}", manifest.display()))?;
    match value.get("live") {
        None => Ok(false),
        Some(toml::Value::Boolean(b)) => Ok(*b),
        Some(other) => {
            Err(format!("{}: `live` must be a boolean, got {other:?}", manifest.display()))
        }
    }
}

/// Render the generated registry source. Deterministic: input order is the (sorted) scan order.
///
/// The generated surface (same shape for an empty set):
/// - `#[path]` module per strategy (forward-slash absolute paths — rustc accepts them on Windows,
///   and they need no escaping inside the quoted literal);
/// - `USER_STRATEGIES` / `USER_LIVE_CAPABLE` — derived rosters, never hand-written;
/// - `user_strategy_by_name::<B>(name, params)` calling each entry file's
///   `build::<B>(params) -> Box<dyn Strategy<B> + Send>`.
pub fn render(strategies: &[ScannedStrategy]) -> String {
    let mut src = String::from(
        "// @generated by vike-user-strategies/build.rs — NEVER committed, lives in OUT_DIR.\n",
    );
    for s in strategies {
        let path = s.entry.display().to_string().replace('\\', "/");
        src.push_str(&format!("#[path = \"{path}\"]\npub mod user_{};\n", s.name));
    }
    let names: Vec<String> = strategies.iter().map(|s| format!("\"{}\"", s.name)).collect();
    let live: Vec<String> =
        strategies.iter().filter(|s| s.live).map(|s| format!("\"{}\"", s.name)).collect();
    src.push_str(&format!(
        "\n/// Every user strategy the scan found (derived — the folder listing is the authority).\n\
         pub const USER_STRATEGIES: &[&str] = &[{}];\n",
        names.join(", ")
    ));
    src.push_str(&format!(
        "\n/// The subset whose `strategy.toml` declares `live = true`. Absent declaration is\n\
         /// sim-only — the same default-deny posture as `vike_strategy::LIVE_CAPABLE`.\n\
         pub const USER_LIVE_CAPABLE: &[&str] = &[{}];\n",
        live.join(", ")
    ));
    src.push_str(
        "\n/// Resolve a user strategy by name. `None` = not a user strategy (the caller keeps its\n\
         /// own error wording). Built-in registries consult this AFTER their own arms, so a user\n\
         /// folder can never shadow a built-in name.\n\
         pub fn user_strategy_by_name<B: vike_model::HftBroker + 'static>(\n\
         \x20   name: &str,\n\
         \x20   params: &toml::Value,\n\
         ) -> Option<Box<dyn vike_model::Strategy<B> + Send>> {\n",
    );
    if strategies.is_empty() {
        src.push_str("    let _ = (name, params);\n    None\n}\n");
    } else {
        src.push_str("    match name {\n");
        for s in strategies {
            src.push_str(&format!(
                "        \"{n}\" => Some(user_{n}::build::<B>(params)),\n",
                n = s.name
            ));
        }
        src.push_str("        _ => None,\n    }\n}\n");
    }
    src
}
