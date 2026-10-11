// The STRATEGY-SPECIFIC half of the user-strategy scan: the tier's path, the per-folder
// `strategy.toml` manifest, and the generated resolver text. Pure functions of paths and strings.
//
// `build.rs` `include!`s this file (the `vike-buildinfo` precedent: one source of truth compiled
// into both the build script and the library, so the unit tests exercise EXACTLY the code the
// build runs — never a re-implementation that can drift). No environment reads, no OUT_DIR
// knowledge, no cargo directives: those stay in `build.rs` and in
// `vike_model::host_build::driver::run`, its body.
//
// What moved OUT of this file into `crates/vike-model/src/host_build.rs`'s module tree, shared with
// `vike-user-research`: the tier listing itself (sorted folders, the `<name>.rs` entry rule, the
// name charset `vike_model::host_build::scan::valid_name`, every objection naming its path —
// `scan_tier`), and the generated text both registries share (`render`'s `header`,
// `module_lines`, `quoted_list`). What stays: the tier's path, the `live` manifest and the
// `USER_STRATEGIES`/`USER_LIVE_CAPABLE`/`user_strategy_by_name` text.
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
// A folder with no entry file is a PRESETS-ONLY folder for a built-in (the vike-studio-core
// `StrategyBody::Native` convention), skipped silently (`EntryPolicy::SkipEntryless`). A malformed
// folder is an ERROR carried in [`ScanOutcome::errors`] — the caller turns those into a failed
// build naming the path. Silence is how this tier spent months being mistaken for a working
// mechanism.

use std::fs;
use std::path::{Path, PathBuf};

use vike_model::host_build::render::{header, module_lines, quoted_list};
use vike_model::host_build::scan::{EntryPolicy, Found, scan_tier};

/// One discovered user strategy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedStrategy {
    /// The folder name == registry name == entry-file stem. Validated by
    /// `vike_model::host_build::scan::valid_name`.
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

/// `<user_data>/strategies/rust` — the COMPILED tier, and the only directory [`scan`] reads.
pub fn rust_tier(user_data_root: &Path) -> PathBuf {
    user_data_root.join("strategies").join("rust")
}

/// Every directory [`scan`] reads — and `build.rs` watches exactly these, never the whole
/// user_data. Everything the scan opens (the tier listing, each folder's entry file and its
/// `strategy.toml`) sits under them, so a write anywhere else (`runs/`, `logs/`, `strategies/rhai/`)
/// cannot change the registry and must not re-run the build script.
pub fn scanned_dirs(user_data_root: &Path) -> Vec<PathBuf> {
    vec![rust_tier(user_data_root)]
}

/// Scan [`rust_tier`] (where `root` is a user_data directory). Absent root or absent tier
/// directory is the ordinary empty state — no strategies, no errors. Present-but-malformed content
/// is an error, never a skip. The listing is `scan_tier`'s; each folder it finds then has its
/// `strategy.toml` read, and a bad manifest is an error that DROPS that strategy. Errors: the
/// listing's, then the manifests'.
pub fn scan(user_data_root: &Path) -> ScanOutcome {
    let tier = scan_tier(&rust_tier(user_data_root), "strategy", EntryPolicy::SkipEntryless);
    let mut out = ScanOutcome { strategies: Vec::new(), errors: tier.errors };
    for found in tier.found {
        match read_live_flag(&found.dir.join("strategy.toml")) {
            Ok(live) => {
                out.strategies.push(ScannedStrategy { name: found.name, entry: found.entry, live });
            }
            Err(e) => out.errors.push(e),
        }
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
/// - `#[path]` module per strategy (`module_lines`: forward-slash absolute paths);
/// - `USER_STRATEGIES` / `USER_LIVE_CAPABLE` — derived rosters, never hand-written;
/// - `user_strategy_by_name::<B>(name, params)` calling each entry file's
///   `build::<B>(params) -> Box<dyn Strategy<B> + Send>`.
pub fn render(strategies: &[ScannedStrategy]) -> String {
    let mut src = header("vike-user-strategies");
    // `module_lines` reads only each folder's name and entry file; the folder itself is the
    // entry's parent.
    let found: Vec<Found> = strategies
        .iter()
        .map(|s| Found {
            name: s.name.clone(),
            dir: s.entry.parent().map(Path::to_path_buf).unwrap_or_default(),
            entry: s.entry.clone(),
        })
        .collect();
    src.push_str(&module_lines(&found));
    let names: Vec<&str> = strategies.iter().map(|s| s.name.as_str()).collect();
    let live: Vec<&str> = strategies.iter().filter(|s| s.live).map(|s| s.name.as_str()).collect();
    src.push_str(&format!(
        "\n/// Every user strategy the scan found (derived — the folder listing is the authority).\n\
         pub const USER_STRATEGIES: &[&str] = &[{}];\n",
        quoted_list(&names)
    ));
    src.push_str(&format!(
        "\n/// The subset whose `strategy.toml` declares `live = true`. Absent declaration is\n\
         /// sim-only — the same default-deny posture as `vike_strategy::LIVE_CAPABLE`.\n\
         pub const USER_LIVE_CAPABLE: &[&str] = &[{}];\n",
        quoted_list(&live)
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
