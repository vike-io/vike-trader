//! The ONE-TIME migration off `studio_strategies.json` into `user_data/strategies/`.
//!
//! # The two entry kinds are not the same kind of thing, and that is the whole design
//!
//! The legacy blob holds one row per "saved strategy", but `StrategySource` splits them into two
//! populations that only look alike (`crates/vike-studio/src/saved.rs`'s `SavedStrategy`):
//!
//! * **Rhai** — a row that carries the SCRIPT. It genuinely is a strategy, and it becomes a
//!   strategy folder: `strategies/rhai/<name>/<name>.rhai`.
//! * **Native** — a row that carries a `vike_backtest::harness::registry` NAME plus free-form
//!   param rows. It carries no code and cannot: the strategy is compiled into the binary. What the
//!   user actually authored is the PARAMETER SET, so the row becomes a preset FOR that registry
//!   strategy: `strategies/rust/<native>/<entry-name>.toml`. Migrating it as a strategy folder
//!   would produce an empty folder named after something the user never wrote.
//!
//! ⚠ **Consequence, and a future rust-side loader must know it:** a folder under
//! `strategies/rust/` created by this migration holds presets and NO `<name>.rs` entry file,
//! because the strategy it presets lives in the binary's registry. That is a legitimate second
//! shape for that tree, not a broken strategy — a rust loader that copies this module's
//! `LoadDiagnostic::MissingEntry` rule verbatim would report every migrated native entry as an
//! error.
//!
//! # Free-form text rows into TOML
//!
//! The JSON's `params` are `Vec<(String, String)>` — deliberately TEXT, because there is no
//! `ParamSpec` seam to type them against (`crates/vike-studio-core/src/spec.rs`'s module doc is the
//! authority). The migration must not invent one either, so it reuses the EXACT function the
//! Studio uses at run time, `crates/vike-studio-core/src/spec.rs`'s `params_from_rows`, and
//! serialises its output. That is what makes the migrated preset MEAN the same thing as the JSON
//! row it came from: `2` stays an integer, `true` stays a bool, and a bare `BTCUSDT` — not valid
//! TOML on its own — becomes the quoted string the run-time path would have made of it. Anything
//! else (a value with a quote in it, a leading `#`) is quoted and escaped by the TOML serialiser,
//! which is the point of going through a `toml::Value` rather than pasting text into a template.
//!
//! # Idempotent, and the JSON is never touched
//!
//! [`apply_migration`] writes a file only when NOTHING is at that path — a second run writes
//! nothing and reports every target as [`MigrationSkip::AlreadyPresent`], and a user's later edits
//! to a migrated file are safe. It never reads, writes, moves or deletes `studio_strategies.json`:
//! this module does not even take its path. A migration that consumes its source cannot be re-run
//! and turns a rollback into data loss, and the JSON stays the read-only fallback for a build the
//! user rolls back to.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use vike_model::state_path::{RHAI_SUBDIR, RUST_SUBDIR, STRATEGIES_SUBDIR};

use crate::spec::params_from_rows;

/// One row of the legacy list, in the shape this migration reads.
///
/// Deliberately NOT `vike_studio::saved::SavedStrategy`: that type lives one layer UP, in the egui
/// crate that owns the JSON schema and its back-compat contract, and this crate cannot depend on
/// it. Keeping the schema in exactly one place and taking already-parsed rows here also matches
/// how the rest of this workspace draws the line — the caller owns the I/O and the library takes
/// data (see `crates/vike-ops/tests/settings_registry.rs`'s rule for the environment twin of it).
#[derive(Debug, Clone, PartialEq)]
pub struct LegacyEntry {
    /// The name the user gave the row in the Studio's Saved pane. Free text — it has never been
    /// constrained to anything a filesystem accepts, which is what [`slug`] exists for.
    pub name: String,
    pub body: LegacyBody,
}

/// Which population a [`LegacyEntry`] belongs to — see the module doc on why they migrate to
/// different places.
#[derive(Debug, Clone, PartialEq)]
pub enum LegacyBody {
    /// A Rhai script: the row IS a strategy.
    Rhai { code: String },
    /// A registry strategy name plus its free-form `(key, value-text)` param rows: the row is a
    /// PRESET.
    Native { native: String, params: Vec<(String, String)> },
}

/// One file the migration intends to write.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedFile {
    /// RELATIVE to `<project>/user_data`, so the plan is pure and a test can read it without a
    /// filesystem. [`apply_migration`] joins the root — which is also what confines every write
    /// under it.
    pub path: PathBuf,
    pub contents: String,
    /// The JSON row this came from, so a skip or collision names something the user recognises
    /// from the Saved pane rather than a path they have never seen.
    pub entry: String,
}

/// What the migration WOULD do — the pure half.
#[derive(Debug, Clone, PartialEq)]
pub struct MigrationPlan {
    pub files: Vec<PlannedFile>,
    /// Rows that cannot be planned at all. [`apply_migration`] appends its own write-time skips to
    /// these, so a caller reports ONE list.
    pub skipped: Vec<MigrationSkip>,
}

/// What the migration did NOT do, NAMED — one variant per reason.
///
/// The first three come from [`plan_migration`] and the last two only from [`apply_migration`];
/// they share an enum because a caller reports them together and a person reading the report is
/// asking one question ("where did my saved strategy go?").
#[derive(Debug, Clone, PartialEq)]
pub enum MigrationSkip {
    /// The row's name has no filesystem-safe form at all — it was blank, or made entirely of
    /// characters a path cannot hold.
    Unnameable { entry: String },

    /// A native row whose registry name is blank: there is nothing for the preset to belong to.
    NamelessNative { entry: String },

    /// Two rows plan the SAME file. Reachable because saved names were never unique-constrained,
    /// and because `slug` maps distinct names onto one stem (`my strat` and `my/strat` both become
    /// `my-strat`). The first row wins; the second is reported.
    Collision { entry: String, path: PathBuf, first: String },

    /// Something is already at the target path. THE idempotency arm: a re-run reports every file
    /// this way and writes nothing, and a user's edits to a migrated file are never overwritten.
    AlreadyPresent { entry: String, path: PathBuf },

    /// The directory or the file could not be written.
    WriteFailed { entry: String, path: PathBuf, error: String },

    /// A native row's params could not be serialised to TOML. Nothing is lost: the JSON is still
    /// there, untouched, and the row can be re-created by hand from the message.
    PresetNotSerializable { entry: String, error: String },
}

impl std::fmt::Display for MigrationSkip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MigrationSkip::Unnameable { entry } => write!(
                f,
                "'{entry}': not migrated — the name has no usable filename form; rename it in the \
                 Saved pane and migrate again"
            ),
            MigrationSkip::NamelessNative { entry } => write!(
                f,
                "'{entry}': not migrated — a native entry with no registry strategy name has \
                 nothing to be a preset for"
            ),
            MigrationSkip::Collision { entry, path, first } => write!(
                f,
                "'{entry}': not migrated — '{first}' already claims {}; rename one of them",
                path.display()
            ),
            MigrationSkip::AlreadyPresent { entry, path } => {
                write!(f, "'{entry}': already migrated — {} exists, left untouched", path.display())
            }
            MigrationSkip::WriteFailed { entry, path, error } => {
                write!(f, "'{entry}': could not write {} — {error}", path.display())
            }
            MigrationSkip::PresetNotSerializable { entry, error } => {
                write!(
                    f,
                    "'{entry}': not migrated — its params are not expressible as TOML: {error}"
                )
            }
        }
    }
}

/// What the migration actually did.
#[derive(Debug, Clone, PartialEq)]
pub struct MigrationOutcome {
    /// Absolute paths written by THIS run. Empty on a re-run — that is the idempotency contract,
    /// observable rather than asserted.
    pub written: Vec<PathBuf>,
    /// [`MigrationPlan::skipped`] plus everything the writes themselves skipped.
    pub skipped: Vec<MigrationSkip>,
}

/// Plan the migration — PURE: no filesystem, no clock, no environment. Every path is relative to
/// `<project>/user_data`.
///
/// Order is the input's order, so a caller's report reads in the order the Saved pane showed.
pub fn plan_migration(entries: &[LegacyEntry]) -> MigrationPlan {
    let mut files: Vec<PlannedFile> = Vec::new();
    let mut skipped: Vec<MigrationSkip> = Vec::new();
    // Case-FOLDED, because the collision that matters is the one the target filesystem cannot
    // hold: `Fast` and `fast` are two files on Linux and one on Windows, and a migration that
    // silently overwrites on one platform and not the other is worse than one that refuses.
    let mut claimed: BTreeMap<String, String> = BTreeMap::new();

    for entry in entries {
        let Some(stem) = slug(&entry.name) else {
            skipped.push(MigrationSkip::Unnameable { entry: entry.name.clone() });
            continue;
        };
        let (path, contents) = match &entry.body {
            LegacyBody::Rhai { code } => (
                Path::new(STRATEGIES_SUBDIR)
                    .join(RHAI_SUBDIR)
                    .join(&stem)
                    .join(format!("{stem}.rhai")),
                // VERBATIM. The user's script is theirs; a migration that reformats or annotates
                // it has rewritten their work.
                code.clone(),
            ),
            LegacyBody::Native { native, params } => {
                let Some(folder) = slug(native) else {
                    skipped.push(MigrationSkip::NamelessNative { entry: entry.name.clone() });
                    continue;
                };
                let contents = match preset_toml(&entry.name, native, params) {
                    Ok(text) => text,
                    Err(error) => {
                        skipped.push(MigrationSkip::PresetNotSerializable {
                            entry: entry.name.clone(),
                            error,
                        });
                        continue;
                    }
                };
                (
                    Path::new(STRATEGIES_SUBDIR)
                        .join(RUST_SUBDIR)
                        .join(&folder)
                        .join(format!("{stem}.toml")),
                    contents,
                )
            }
        };

        let key = path.to_string_lossy().to_lowercase();
        if let Some(first) = claimed.get(&key) {
            skipped.push(MigrationSkip::Collision {
                entry: entry.name.clone(),
                path,
                first: first.clone(),
            });
            continue;
        }
        claimed.insert(key, entry.name.clone());
        files.push(PlannedFile { path, contents, entry: entry.name.clone() });
    }

    MigrationPlan { files, skipped }
}

/// Execute `plan` under `user_data` — `<project>/user_data`, from
/// `crates/vike-model/src/state_path.rs`'s `project_user_data_dir`.
///
/// Writes ONLY where nothing exists, creating parent directories on the way. Every failure is a
/// [`MigrationSkip`] rather than an `Err`: one unwritable file must not abandon the other
/// nineteen, and the JSON is still there for anything that did not land.
pub fn apply_migration(plan: &MigrationPlan, user_data: &Path) -> MigrationOutcome {
    let mut written: Vec<PathBuf> = Vec::new();
    let mut skipped = plan.skipped.clone();

    for file in &plan.files {
        let path = user_data.join(&file.path);
        // `symlink_metadata`, not `exists()`: a DANGLING symlink reports "nothing there" to
        // `exists()` and would then be written THROUGH, truncating whatever it points at. Same
        // reasoning as `crates/vike-model/src/state_path.rs`'s `write_path`.
        if std::fs::symlink_metadata(&path).is_ok() {
            skipped.push(MigrationSkip::AlreadyPresent { entry: file.entry.clone(), path });
            continue;
        }
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                skipped.push(MigrationSkip::WriteFailed {
                    entry: file.entry.clone(),
                    path,
                    error: e.to_string(),
                });
                continue;
            }
        }
        match std::fs::write(&path, &file.contents) {
            Ok(()) => written.push(path),
            Err(e) => skipped.push(MigrationSkip::WriteFailed {
                entry: file.entry.clone(),
                path,
                error: e.to_string(),
            }),
        }
    }

    MigrationOutcome { written, skipped }
}

/// The preset file for a native row: a provenance header plus the params table.
///
/// The header exists because this file is the only artifact of a row the user saved somewhere
/// else; six months later "where did this come from" has to be answerable from the file itself.
/// The table is FLAT (`size = 2`, not `[params]`), which is the shape
/// `crates/vike-studio-core/src/spec.rs`'s `StrategySpec::native_from_toml_str` parses and the same
/// shape a Rhai preset has — one preset format for both trees.
///
/// The two names are folded onto ONE line before they go in the comment: a name carrying a newline
/// would end the comment and leave its own tail as a bare line, turning a provenance note into a
/// TOML parse error in the file it documents.
fn preset_toml(entry: &str, native: &str, params: &[(String, String)]) -> Result<String, String> {
    let table = params_from_rows(params);
    let body = toml::to_string(&table).map_err(|e| e.to_string())?;
    Ok(format!(
        "# preset '{}' for the built-in strategy '{}'\n\
         # migrated from studio_strategies.json\n\
         {body}",
        one_line(entry),
        one_line(native)
    ))
}

/// `text` with every control character (newlines included) replaced by a space — see
/// [`preset_toml`].
fn one_line(text: &str) -> String {
    text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

/// A filesystem-safe stem for a free-text saved-strategy name.
///
/// The Saved pane never constrained the name box, so a row can be called `BTC / ETH pairs (v2)` —
/// or `../../secrets`. Anything that is not a letter, digit, `-`, `_` or `.` collapses to a single
/// `-`, which removes every path separator, every Windows-reserved character and every control
/// character in one rule rather than by enumerating a deny-list that a new platform outgrows.
/// Leading and trailing `-`/`.` are then stripped, which is what makes traversal structurally
/// impossible: `..` cannot survive at either end, so no output can be `.` or `..`, and no output
/// can contain a separator to escape with.
///
/// Letters are Unicode, not ASCII: a strategy named in Cyrillic or Japanese is a perfectly good
/// filename on every filesystem this runs on, and folding it to `Unnameable` would migrate a
/// user's work into nothing.
///
/// `None` when nothing survives — reported as [`MigrationSkip::Unnameable`], never silently
/// renamed to something the user would not recognise.
fn slug(name: &str) -> Option<String> {
    let mut out = String::with_capacity(name.len());
    let mut last_was_dash = false;
    for ch in name.trim().chars() {
        if ch.is_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            out.push(ch);
            last_was_dash = ch == '-';
        } else if !last_was_dash {
            out.push('-');
            last_was_dash = true;
        }
    }
    let trimmed = out.trim_matches(|c| c == '-' || c == '.');
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_strategies::{load_rhai_strategies, load_user_strategies, resolve_preset};

    fn rhai(name: &str, code: &str) -> LegacyEntry {
        LegacyEntry { name: name.to_string(), body: LegacyBody::Rhai { code: code.to_string() } }
    }

    fn native(name: &str, registry: &str, params: &[(&str, &str)]) -> LegacyEntry {
        LegacyEntry {
            name: name.to_string(),
            body: LegacyBody::Native {
                native: registry.to_string(),
                params: params.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            },
        }
    }

    /// A Rhai row becomes a strategy FOLDER with a matching entry file, and the script is
    /// byte-verbatim — a migration that touched the user's code would be rewriting their work.
    #[test]
    fn a_rhai_entry_becomes_a_folder_and_a_matching_entry_file() {
        let code = "fn on_bar() {\n  // mine\n}\n";
        let plan = plan_migration(&[rhai("sma-cross", code)]);

        assert!(plan.skipped.is_empty(), "unexpected: {:?}", plan.skipped);
        assert_eq!(plan.files.len(), 1);
        assert_eq!(
            plan.files[0].path,
            Path::new("strategies").join("rhai").join("sma-cross").join("sma-cross.rhai")
        );
        assert_eq!(plan.files[0].contents, code, "the script must be verbatim");
    }

    /// A Native row is NOT a strategy: the folder is the REGISTRY strategy's, the file is the
    /// user's entry name, and the content is the params table. See the module doc.
    #[test]
    fn a_native_entry_becomes_a_preset_filed_under_its_registry_strategy() {
        let plan = plan_migration(&[native("hold-2", "buy_hold", &[("size", "2")])]);

        assert!(plan.skipped.is_empty(), "unexpected: {:?}", plan.skipped);
        assert_eq!(
            plan.files[0].path,
            Path::new("strategies").join("rust").join("buy_hold").join("hold-2.toml"),
            "folder = the registry strategy, file = the user's preset name"
        );
        let parsed: toml::Value = toml::from_str(&plan.files[0].contents).unwrap();
        assert_eq!(parsed.get("size").and_then(toml::Value::as_integer), Some(2));
        assert!(
            plan.files[0].contents.contains("buy_hold"),
            "the provenance header must name what it presets: {}",
            plan.files[0].contents
        );
    }

    /// AWKWARD VALUES — the reason this goes through `params_from_rows` + a TOML serialiser rather
    /// than a text template. Each row below is invalid TOML *as written* or would corrupt a
    /// template, and each must survive as the value the run-time path would have produced.
    #[test]
    fn awkward_param_values_survive_as_valid_quoted_toml() {
        let rows = [
            ("symbol", "BTCUSDT"),          // bare: not valid TOML on its own -> string
            ("note", "he said \"buy\""),    // embedded quotes -> escaped
            ("comment", "# not a comment"), // a leading '#' would eat the line in a template
            ("path", "C:\\data\\ticks"),    // backslashes -> escaped, not an escape sequence
            ("size", "2"),                  // stays an INTEGER
            ("live", "true"),               // stays a BOOL
            ("rate", "2.5"),                // stays a FLOAT
        ];
        let plan = plan_migration(&[native("awkward", "buy_hold", &rows)]);
        assert!(plan.skipped.is_empty(), "unexpected: {:?}", plan.skipped);

        let parsed: toml::Value = toml::from_str(&plan.files[0].contents)
            .expect("the migrated preset must be valid TOML");

        // The load-bearing claim: the migrated file means EXACTLY what the JSON row meant at run
        // time, which is what `params_from_rows` would have made of the same text.
        let owned: Vec<(String, String)> =
            rows.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        assert_eq!(
            parsed,
            params_from_rows(&owned),
            "the preset must round-trip the run-time table"
        );

        assert_eq!(parsed.get("symbol").and_then(toml::Value::as_str), Some("BTCUSDT"));
        assert_eq!(parsed.get("note").and_then(toml::Value::as_str), Some("he said \"buy\""));
        assert_eq!(parsed.get("comment").and_then(toml::Value::as_str), Some("# not a comment"));
        assert_eq!(parsed.get("path").and_then(toml::Value::as_str), Some("C:\\data\\ticks"));
        assert_eq!(parsed.get("size").and_then(toml::Value::as_integer), Some(2));
        assert_eq!(parsed.get("live").and_then(toml::Value::as_bool), Some(true));
        assert_eq!(parsed.get("rate").and_then(toml::Value::as_float), Some(2.5));
    }

    /// IDEMPOTENCY, in the shape that matters: the second run writes NOTHING, reports every target
    /// as already present, and does not overwrite a file the user has since edited.
    #[test]
    fn migration_is_idempotent_and_never_overwrites() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let entries = [rhai("sma-cross", "fn on_bar() {}\n"), native("hold", "buy_hold", &[])];
        let plan = plan_migration(&entries);

        let first = apply_migration(&plan, root);
        assert_eq!(first.written.len(), 2, "{:?}", first.skipped);
        assert!(first.skipped.is_empty(), "unexpected: {:?}", first.skipped);

        // The user edits a migrated file — the case a re-run must not destroy.
        let edited = root.join("strategies").join("rhai").join("sma-cross").join("sma-cross.rhai");
        std::fs::write(&edited, "fn on_bar() { /* my edit */ }\n").unwrap();

        let second = apply_migration(&plan, root);
        assert!(second.written.is_empty(), "a re-run must write nothing: {:?}", second.written);
        assert_eq!(second.skipped.len(), 2);
        assert!(
            second.skipped.iter().all(|s| matches!(s, MigrationSkip::AlreadyPresent { .. })),
            "{:?}",
            second.skipped
        );
        assert_eq!(
            std::fs::read_to_string(&edited).unwrap(),
            "fn on_bar() { /* my edit */ }\n",
            "the user's edit must survive"
        );
    }

    /// A saved name is free text and was never constrained. A traversal attempt must land INSIDE
    /// the root — the property `slug`'s leading/trailing strip buys structurally.
    #[test]
    fn a_name_that_would_escape_the_root_is_confined_to_it() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("user_data");
        let plan = plan_migration(&[
            rhai("../../secrets", "fn on_bar() {}\n"),
            rhai("BTC / ETH pairs (v2)", "fn on_bar() {}\n"),
        ]);

        for file in &plan.files {
            assert!(!file.path.is_absolute(), "{} must be relative", file.path.display());
            assert!(
                !file.path.components().any(|c| c == std::path::Component::ParentDir),
                "{} must not traverse",
                file.path.display()
            );
        }
        let out = apply_migration(&plan, &root);
        assert_eq!(out.written.len(), 2, "{:?}", out.skipped);
        for path in &out.written {
            assert!(path.starts_with(&root), "{} escaped the root", path.display());
        }
    }

    /// A blank name (and one made only of separators) has no filename form. Reported, never
    /// silently renamed to something the user would not find again.
    #[test]
    fn an_unnameable_entry_is_reported_rather_than_invented() {
        let plan = plan_migration(&[rhai("   ", "fn on_bar() {}\n"), rhai("///", "x")]);
        assert!(plan.files.is_empty());
        assert_eq!(plan.skipped.len(), 2);
        assert!(plan.skipped.iter().all(|s| matches!(s, MigrationSkip::Unnameable { .. })));
        assert!(plan.skipped[0].to_string().contains("rename"), "the message must state the fix");
    }

    /// Two rows whose names collapse to one stem: the first wins, the second is named. Saved names
    /// were never unique-constrained, so this is reachable from ordinary use.
    #[test]
    fn two_entries_that_would_write_the_same_file_collide_loudly() {
        let plan = plan_migration(&[
            rhai("my strat", "fn on_bar() {}\n"),
            rhai("my/strat", "fn on_bar() { /* other */ }\n"),
            rhai("MY-STRAT", "fn on_bar() { /* third */ }\n"),
        ]);

        assert_eq!(plan.files.len(), 1, "only the first may claim the path");
        assert_eq!(plan.files[0].entry, "my strat");
        assert_eq!(plan.skipped.len(), 2);
        match &plan.skipped[0] {
            MigrationSkip::Collision { entry, first, .. } => {
                assert_eq!(entry, "my/strat");
                assert_eq!(first, "my strat");
            }
            other => panic!("expected Collision, got {other:?}"),
        }
        assert!(
            matches!(&plan.skipped[1], MigrationSkip::Collision { entry, .. } if entry == "MY-STRAT"),
            "a case-only difference collides too: the next filesystem cannot hold both"
        );
    }

    /// A native row with no registry name has nothing to be a preset for.
    #[test]
    fn a_native_entry_without_a_registry_name_is_reported() {
        let plan = plan_migration(&[native("orphan", "", &[("size", "1")])]);
        assert!(plan.files.is_empty());
        assert!(matches!(plan.skipped[0], MigrationSkip::NamelessNative { .. }));
    }

    /// END TO END: a migrated Rhai row is a strategy the LOADER finds, compiles and reports
    /// clean. Either half can be correct alone and still disagree about the layout; this is the
    /// test that says they do not.
    #[test]
    fn a_migrated_rhai_entry_loads_back_through_the_directory_loader() {
        let tmp = tempfile::tempdir().unwrap();
        let user_data = tmp.path().join("user_data");
        let plan = plan_migration(&[rhai("sma-cross", "fn on_bar() {}\n")]);
        let out = apply_migration(&plan, &user_data);
        assert_eq!(out.written.len(), 1, "{:?}", out.skipped);

        let report = load_rhai_strategies(&user_data.join(STRATEGIES_SUBDIR).join(RHAI_SUBDIR));

        assert!(report.diagnostics.is_empty(), "unexpected: {:?}", report.diagnostics);
        assert_eq!(report.strategies.len(), 1);
        assert_eq!(report.strategies[0].name, "sma-cross");
        assert_eq!(report.strategies[0].source(), Some("fn on_bar() {}\n"));
    }

    /// END TO END for the OTHER population, and the claim this module's doc made in advance: a
    /// migrated NATIVE row lands as a preset for a built-in strategy, the loader accepts that
    /// entry-file-less folder as legitimate, and the preset RESOLVES back into the params the
    /// strategy is constructed with. Before the rust-side loader existed, the same tree produced
    /// one `MissingEntry` error per migrated row and nothing was resolvable at all.
    #[test]
    fn a_migrated_native_entry_resolves_back_as_a_preset_for_its_builtin() {
        let tmp = tempfile::tempdir().unwrap();
        let user_data = tmp.path().join("user_data");
        let plan = plan_migration(&[native(
            "hold-2",
            "buy_hold",
            &[("size", "2"), ("symbol", "BTCUSDT")],
        )]);
        let out = apply_migration(&plan, &user_data);
        assert_eq!(out.written.len(), 1, "{:?}", out.skipped);

        let report = load_user_strategies(&user_data.join(STRATEGIES_SUBDIR));

        assert!(report.diagnostics.is_empty(), "unexpected: {:?}", report.diagnostics);
        assert_eq!(report.strategies.len(), 1);
        assert!(report.strategies[0].is_native(), "the code for buy_hold is in the binary");
        let run = resolve_preset(&report, "buy_hold", "hold-2").expect("the migrated preset");
        match &run.spec {
            crate::StrategySpec::Native { name, params } => {
                assert_eq!(name, "buy_hold");
                assert_eq!(params.get("size").and_then(toml::Value::as_integer), Some(2));
                assert_eq!(params.get("symbol").and_then(toml::Value::as_str), Some("BTCUSDT"));
            }
            other => panic!("expected Native, got {other:?}"),
        }
        run.build().expect("a migrated preset builds the strategy it presets");
    }

    /// `slug` keeps what a filesystem accepts and drops what it does not — pinned directly,
    /// because every other test in this file rests on it.
    #[test]
    fn slug_keeps_readable_names_and_refuses_empty_ones() {
        assert_eq!(slug("sma-cross").as_deref(), Some("sma-cross"));
        assert_eq!(slug("BTC / ETH pairs (v2)").as_deref(), Some("BTC-ETH-pairs-v2"));
        assert_eq!(slug("  padded  ").as_deref(), Some("padded"));
        assert_eq!(slug("../../secrets").as_deref(), Some("secrets"));
        assert_eq!(
            slug("моя-стратегия").as_deref(),
            Some("моя-стратегия"),
            "Unicode is a filename"
        );
        assert_eq!(slug(""), None);
        assert_eq!(slug("..."), None);
        assert_eq!(slug("///"), None);
    }
}
