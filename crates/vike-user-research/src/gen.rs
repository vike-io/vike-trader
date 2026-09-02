// The PURE half of the user-study scan: directory listing -> generated registry source.
//
// `build.rs` `include!`s this file (the `vike-buildinfo` precedent, copied from
// `crates/vike-user-strategies/src/gen.rs`: one source of truth compiled into both the build script
// and the library, so the unit tests exercise EXACTLY the code the build runs — never a
// re-implementation that can drift). Everything here is a pure function of paths and strings: no
// environment reads, no OUT_DIR knowledge, no cargo directives — those stay in `build.rs`, the
// impure caller.
//
// ## The scanned convention
//
// ```text
// <user_data>/research/studies/rust/<name>/
// ├─ <name>.rs        entry file — stem MUST equal the folder name (the rhai tier's rule)
// └─ *.toml           recipes: named configurations, loaded by the CALLER, not by this scan
// ```
//
// ⚠ **Every path component above is spelled through `vike_model::state_path`** —
// `RESEARCH_SUBDIR`, `STUDIES_SUBDIR`, `RUST_SUBDIR`, `RHAI_SUBDIR` — and never as a literal. Those
// constants are the layout's one authority (`user_studies_dir` and
// `crates/vike-studio-core/src/listing.rs`'s `list_studies` resolve the same way), and a second
// spelling here would be a second authority that rots the first time the layout moves.
//
// ## Two deliberate DIFFERENCES from the strategy tier's scan
//
// * **No `study.toml`, and therefore no `live` flag.** The strategy tier has one because mounting a
//   strategy on a venue signs real orders, so the tier needs a default-deny opt-in. A study places
//   no orders and — under `docs/decisions/0029-a-study-reads-the-store-never-a-vendor-api.md` —
//   cannot reach the network at all, so there is nothing for such a flag to gate. Adding one would
//   be a settings key nothing reads, the defect class this workspace names as worse than an
//   unimplemented feature.
// * **A folder holding `.rs` files but not `<name>.rs` is an ERROR.** The strategy tier skips an
//   entry-less folder silently because a presets-only folder for a BUILT-IN is legitimate there. A
//   folder with SOME Rust in it and the wrong file name is not that; it is
//   `crates/vike-studio-core/src/user_strategies/load.rs`'s `MissingEntry` diagnostic — the
//   operator's file is right there and nothing will ever compile it. A folder with no `.rs` at all
//   is still skipped silently: that is a recipes-only folder.
//
// A malformed folder is an ERROR carried in [`ScanOutcome::errors`] — the caller turns those into a
// failed build naming the path. Silence is how the strategy tier spent months being mistaken for a
// working mechanism.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use vike_model::state_path::{RESEARCH_SUBDIR, RHAI_SUBDIR, RUST_SUBDIR, STUDIES_SUBDIR};

/// One discovered user study.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedStudy {
    /// The folder name == registry name == entry-file stem. Validated by [`valid_name`].
    pub name: String,
    /// Absolute path of `<name>.rs`, exactly as found (rendered with forward slashes).
    pub entry: PathBuf,
}

/// The scan result: studies found, every objection, and every non-fatal observation — each naming
/// its path.
#[derive(Debug, Default)]
pub struct ScanOutcome {
    pub studies: Vec<ScannedStudy>,
    pub errors: Vec<String>,
    /// Non-fatal findings the build surfaces as `cargo:warning=` lines. A warning is for a state
    /// that is AMBIGUOUS rather than broken — failing the build over one would stop an operator
    /// who has done nothing wrong yet.
    pub warnings: Vec<String>,
}

/// `<user_data>/research/studies/rust` — the COMPILED tier this crate hosts.
pub fn rust_tier(user_data_root: &Path) -> PathBuf {
    studies_root(user_data_root).join(RUST_SUBDIR)
}

/// `<user_data>/research/studies/rhai` — the INTERPRETED tier, which this crate does not compile
/// and reads only to notice a name that exists in both (see [`scan`]).
pub fn rhai_tier(user_data_root: &Path) -> PathBuf {
    studies_root(user_data_root).join(RHAI_SUBDIR)
}

fn studies_root(user_data_root: &Path) -> PathBuf {
    user_data_root.join(RESEARCH_SUBDIR).join(STUDIES_SUBDIR)
}

/// A registry name must be a lowercase Rust-identifier-safe token: the generated module is
/// `user_<name>` and the match arm quotes it, so the charset is the whole safety argument.
pub fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('a'..='z'))
        && chars.all(|c| matches!(c, 'a'..='z' | '0'..='9' | '_'))
}

/// Scan [`rust_tier`] (where `root` is a user_data directory). Absent root or absent tier directory
/// is the ordinary empty state — no studies, no errors. Present-but-malformed content is an error,
/// never a skip.
///
/// It also lists [`rhai_tier`] — not to compile anything, but because a name present in BOTH tiers
/// is ambiguous to whatever resolves a study by name, exactly as it is for a strategy
/// (`crates/vike-studio-core/src/user_strategies/load.rs` raises a `DuplicateName` diagnostic for
/// the same tree shape). It is a WARNING rather than an error: the interpreted tier is the one a
/// binary install can run, so refusing to build the compiled twin would take away the resolution
/// the operator still has.
pub fn scan(user_data_root: &Path) -> ScanOutcome {
    let mut out = ScanOutcome::default();
    let rhai_names = folder_names(&rhai_tier(user_data_root));
    let tier = rust_tier(user_data_root);
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
            if holds_any_rust(&dir) {
                // The `MissingEntry` case: Rust IS here and none of it will ever be compiled.
                out.errors.push(format!(
                    "{}: holds Rust source but no `{name}.rs` — the entry file's stem must equal \
                     the folder name (the rhai tier's rule), or nothing in this folder is \
                     compiled",
                    dir.display()
                ));
            }
            // Otherwise: a recipes-only folder. Silent, like the strategy tier's presets-only one.
            continue;
        }
        if !valid_name(&name) {
            out.errors.push(format!(
                "{}: study folder name must match [a-z][a-z0-9_]* (it becomes the registry name \
                 and the generated module name)",
                dir.display()
            ));
            continue;
        }
        if rhai_names.contains(&name) {
            out.warnings.push(format!(
                "{name}: a study of this name exists in BOTH tiers ({} and {}) — whatever \
                 resolves a study by name will have to pick one",
                rhai_tier(user_data_root).display(),
                dir.display()
            ));
        }
        out.studies.push(ScannedStudy { name, entry });
    }
    out
}

/// The sub-directory names directly under `dir`, or an empty set when `dir` is absent/unreadable.
/// Absence is the ordinary state — a checkout with no interpreted studies — so it is not a finding.
fn folder_names(dir: &Path) -> BTreeSet<String> {
    let Ok(entries) = fs::read_dir(dir) else { return BTreeSet::new() };
    entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .collect()
}

/// Does this folder hold ANY `.rs` file? The question that separates "half-written study" (an
/// error) from "recipes only" (silent).
fn holds_any_rust(dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(dir) else { return false };
    entries.filter_map(|e| e.ok()).any(|e| {
        let p = e.path();
        p.is_file() && p.extension().and_then(|x| x.to_str()) == Some("rs")
    })
}

/// Render the generated registry source. Deterministic: input order is the (sorted) scan order.
///
/// The generated surface (same shape for an empty set):
/// - `#[path]` module per study (forward-slash absolute paths — rustc accepts them on Windows, and
///   they need no escaping inside the quoted literal);
/// - `USER_STUDIES` — the derived roster, never hand-written;
/// - `user_study_entry(name) -> Option<StudyFn>` — the entry as a FUNCTION POINTER, which is what
///   makes the coercion `user_<name>::run as StudyFn` a compile-time check of the whole signature;
/// - `run_user_study(name, ctx, params)` — one line over it, for a caller that just wants the
///   result. `None` = not a user study; `Some(Err(..))` = it ran and failed.
///
/// Every path is spelled `vike_user_research::…` rather than `crate::…` so the SAME rendered text
/// compiles in both places it is used: `include!`d into this crate's `lib.rs` (which declares
/// `extern crate self as vike_user_research;`) and `include!`d into an integration test, where the
/// name is an ordinary dependency.
pub fn render(studies: &[ScannedStudy]) -> String {
    let mut src = String::from(
        "// @generated by vike-user-research/build.rs — NEVER committed, lives in OUT_DIR.\n",
    );
    for s in studies {
        let path = s.entry.display().to_string().replace('\\', "/");
        src.push_str(&format!("#[path = \"{path}\"]\npub mod user_{};\n", s.name));
    }
    let names: Vec<String> = studies.iter().map(|s| format!("\"{}\"", s.name)).collect();
    src.push_str(&format!(
        "\n/// Every user study the scan found (derived — the folder listing is the authority).\n\
         pub const USER_STUDIES: &[&str] = &[{}];\n",
        names.join(", ")
    ));
    src.push_str(
        "\n/// Resolve a user study's entry function by name. `None` = not a user study (the caller\n\
         /// keeps its own error wording). The `as StudyFn` coercion below is what turns a drifted\n\
         /// entry signature into a BUILD error naming the study.\n\
         pub fn user_study_entry(name: &str) -> Option<vike_user_research::StudyFn> {\n",
    );
    if studies.is_empty() {
        src.push_str("    let _ = name;\n    None\n}\n");
    } else {
        src.push_str("    match name {\n");
        for s in studies {
            src.push_str(&format!(
                "        \"{n}\" => Some(user_{n}::run as vike_user_research::StudyFn),\n",
                n = s.name
            ));
        }
        src.push_str("        _ => None,\n    }\n}\n");
    }
    src.push_str(
        "\n/// Run a user study by name: `None` = not a user study, `Some(Err(..))` = it ran and\n\
         /// failed. One line over [`user_study_entry`], so the two can never disagree.\n\
         pub fn run_user_study(\n\
         \x20   name: &str,\n\
         \x20   ctx: &vike_user_research::StudyContext,\n\
         \x20   params: &toml::Value,\n\
         ) -> Option<Result<vike_user_research::StudyOutcome, vike_user_research::StudyError>> {\n\
         \x20   Some(user_study_entry(name)?(ctx, params))\n\
         }\n",
    );
    src
}
