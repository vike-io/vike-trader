//! A strategy spec (a Rhai editor buffer, or a strategy FILE of the hub's folder) as the profile's
//! `[strategy]` table, and the boxed strategy that table builds. A `.rs` file is built only in the
//! sandboxed child that loaded it ([`RustFileBuild::Loaded`]); this crate links no loader.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use vike_backtest::HarnessError;
use vike_backtest::run::refusals::ProfileRefusal;
use vike_backtest::run::strategy_builder::{StandIn, StrategyBuilder};
use vike_backtest::run::{StrategySource, profile::ProfileStrategy};
use vike_backtesthub_client::proto::WireStrategyFile;
use vike_model::Strategy;
use vike_model::params::{ParamValue, Params};
use vike_simulator::OrderBrokerSimulator;
use vike_strategy::file::StrategyFileHeader;
use vike_strategy::file::scan::StrategyFileLanguage;

use vike_studio_core::{RunError, StrategySpec};

/// Where a `.rs` strategy file's built artifact is looked up (by the sandboxed child, under the build
/// folder only it can name): `<project>/user_data/strategy-builds`, resolved by `vike_paths::state_path`'s
/// ordinary project walk from this process's working directory.
///
/// ⚠⚠ **THIS IS A SECOND PROJECT WALK, and `crates/vike-boot/tests/one_owner.rs`'s `WALK` table
/// classifies it as one. Read this before reusing the shape.** `vike-boot`'s whole contract is
/// that ONE walk decides: every project-relative path a root uses is derived from `Booted`, because
/// the `_from`-less resolvers are `$VIKE_SETTINGS_DIR`-BLIND and a second call answers with
/// whatever the working directory happens to sit above. This function performs exactly that second
/// call, so a daemon booted with `VIKE_SETTINGS_DIR` pointing somewhere other than its working
/// directory would load strategy files from a DIFFERENT project than the one it booted into — silently,
/// with a `your Rust strategy's build was not found` at best and somebody else's artifact at
/// worst.
///
/// ⚠ **`a_crate_that_boots_may_not_walk_again` does NOT catch it**, and that is worth knowing
/// rather than discovering: that rule's roster is the crates whose own source calls
/// `vike_boot::boot`. This is a LIBRARY, reached from a booted root (`vike-backend backtesthub`) one
/// crate down, so the walk is outside the rule by construction. The `WALK` row is what records it.
///
/// **The bound, MEASURED rather than hoped for.** On every SHIPPED unit the two answers cannot
/// disagree: `crates/vike-ops/tests/container_deploy/deploy_layout_gate/project_folder.rs`'s `every_daemon_unit_is_one_project_folder`
/// requires each daemon unit's `VIKE_SETTINGS_DIR` to be exactly `<WorkingDirectory>/settings`, so
/// the boot's project and this walk's project are the same directory by gate. **Two holes survive
/// it**: an `EnvironmentFile=` line BEATS `Environment=`, so an operator's `<root>/.env` can move
/// `VIKE_SETTINGS_DIR` where no gate can see; and a hand-run daemon (`cd elsewhere;
/// VIKE_SETTINGS_DIR=… vike-backend backtesthub --addr`) diverges outright.
///
/// **The cure, and why it is not here yet.** [`strategy_files_dir_from`] already takes the directory as a
/// parameter — answer (2) of that gate's own panic message — so what is missing is a caller holding
/// `Booted`'s answer. Threading it through `strategy_config` reaches four runners and three `*_local`
/// wire entries, which `vike-backtesthub`'s server calls; and the one composition root that can name
/// both halves (`crates/vike-backend/src/main.rs`, layer 68) is FORBIDDEN to resolve a path at all —
/// `crates/vike-ops/tests/container_deploy/multicall_gate.rs`'s `the_dispatcher_starts_nothing` fails
/// a PR that so much as names `state_path::` there. The shape that fits is the one `run_study_local`
/// already uses beside it: `backtest_cli`'s `--addr` arm — the rung that already walks for this
/// daemon's settings — resolves the directory and hands it to the backtesthub
/// (`backtesthub_server::StudyDirs` is the study's). That is a new parameter across three crates, which
/// is a design change rather than a fix, so it is named here rather than smuggled in.
///
/// What is true today: every shipped unit runs `WorkingDirectory=<project>`, so this resolves the
/// project the daemon was installed as — the same walk `state_path::user_runs_dir` and its
/// siblings already answer every other `user_data/` question with. It reads no environment
/// variable, so it adds no row to `vike_config::settings::SETTINGS` and none to its `LIBRARY_PIN`
/// ratchet.
///
/// The fallback is the plain relative path, which is what the builder service's own out-dir
/// setting already defaults to: with no project marker above the working
/// directory there is nothing to resolve, and refusing outright would turn "no project" into a
/// different failure from the `LoadError::Missing` a caller is about to get anyway — with a worse
/// message.
#[must_use]
pub fn strategy_files_dir() -> PathBuf {
    strategy_files_dir_from(std::env::current_dir().ok().as_deref())
}

/// [`strategy_files_dir`] with the working directory supplied — `None` meaning "this process has none"
/// (a deleted or unreadable CWD, which `std::env::current_dir` reports as an error).
///
/// ⚠ **Split out so the FALLBACK branch can be proved rather than assumed.** The test that used to
/// cover it handed the resolver a temp directory and asserted `None` only *if* the resolver
/// happened to answer `None` — a conditional assertion, so the fallback was still reachable by
/// nothing. It cannot be made unconditional against a real filesystem either: the walk climbs to
/// `/`, and no test can promise that no ancestor of a temp directory carries a project marker.
/// Passing `None` here is the one input that reaches the fallback deterministically, on every box.
pub(super) fn strategy_files_dir_from(cwd: Option<&Path>) -> PathBuf {
    cwd.and_then(vike_paths::state_path::user_strategy_files_dir).unwrap_or_else(|| {
        PathBuf::from(vike_paths::state_path::PROJECT_USER_DATA_DIR)
            .join(vike_paths::state_path::STRATEGY_BUILDS_SUBDIR)
    })
}

/// The loaded strategy file a sandboxed child builds from: a strategy over a params table (TOML text)
/// and the run's mount facts (JSON text, `vike_model::params::MountFacts`, `ABI_VERSION` 9), or the
/// file's own refusal of those params in its words (its `refuse` export, asked before any strategy
/// exists). The child's own crate (`vike-strategy-sandbox`) makes it from the loader; this crate links no
/// loader.
pub type StrategyFileBuild = Box<
    dyn Fn(&str, &str) -> Result<Box<dyn Strategy<OrderBrokerSimulator>>, String> + Send + Sync,
>;

/// The [`StrategyBuilder`] over a loaded file: each build hands `build` the (overridden) params table
/// as TOML text and `facts_json`. Params cross the C-ABI as TEXT, never as a `Params`: `toml::to_string`
/// of the sorted table (`crates/vike-config/tests/params_wire_identity.rs`'s
/// `b_toml_document_bytes_are_identical` pins its bytes). An empty table is an empty string. The file's
/// refusal is the build's error, so a run refuses before it plans or reads any data.
///
/// `header` is the file's, when it declares a `type = "model"` key: each such key's run id is replaced by
/// its text in `models` (by run id, what the hub read: `crate::strategy_sandbox::model_texts_of`) before the
/// params cross, and a run id `models` does not hold refuses the build. `None` hands the params over as they are.
pub fn loaded_file_builder(
    build: StrategyFileBuild,
    facts_json: String,
    header: Option<StrategyFileHeader>,
    models: BTreeMap<String, String>,
) -> StrategyBuilder {
    StrategyBuilder::new(move |params| {
        let with_models = match &header {
            Some(header) => Some(
                header
                    .with_model_texts(params, |run| {
                        models.get(run).cloned().ok_or_else(|| {
                            "the hub read no model for it (it reads the run's own params, so a \
                             sweep over a model key is not supported)"
                                .to_string()
                        })
                    })
                    .map_err(|refusal| {
                        HarnessError::from(ProfileRefusal::StrategyModelTextsRefused { refusal })
                    })?,
            ),
            None => None,
        };
        let params_toml = toml::to_string(with_models.as_ref().unwrap_or(params)).map_err(|e| {
            HarnessError::from(ProfileRefusal::StrategyParamsNotSerialized { e: e.to_string() })
        })?;
        build(&params_toml, &facts_json).map_err(|refusal| {
            HarnessError::from(ProfileRefusal::StrategyFileBuildRefused { refusal })
        })
    })
}

/// The file a Studio EDITOR BUFFER runs as. A buffer is not a file of the `strategies/` folder and
/// nothing reads this path: the buffer's own text is the run's source ([`strategy_config`]), and its
/// sha256 is what the result records.
pub const EDITOR_BUFFER_FILE: &str = "editor-buffer.rhai";

/// **Where a Studio spec's strategy FILE comes from**: the folder it is read under, and how a `.rs`
/// file is built in THIS process.
#[derive(Debug, Clone)]
pub struct StrategyFiles {
    /// The project's `strategies/` folder (`vike_paths::state_path::strategies_dir_beside`).
    pub dir: PathBuf,
    /// How a `.rs` file is built here.
    pub rust: RustFileBuild,
}

/// How a `.rs` strategy file of a Studio run is built in this process.
#[derive(Debug, Clone)]
pub enum RustFileBuild {
    /// Never here: a hub hands a `.rs` run to a sandboxed child, and a run that reaches this
    /// process anyway is refused (`ProfileStrategy::build`'s words).
    Sandboxed,
    /// The files the sandboxed child loaded, ONE PER MOUNT in mount order (one for a `[strategy]` run or
    /// a Studio slice; a portfolio's `.rs` mounts each, decision 0281), looked up by mount
    /// ([`StrategyFiles::prepare_mount`]): two mounts of one file are two entries, because each builds
    /// over its own mount facts.
    Loaded(Vec<LoadedMount>),
    /// **Test support, never a door**: the file built in this process by a test's stand-in
    /// (`StrategyBuilder::stand_in`).
    StandIn(StandIn),
}

/// One mount's file as a sandboxed child loaded it ([`RustFileBuild::Loaded`]): its builder (over that
/// mount's facts), and the text the hub read it as: the child reads no folder, so the file's header
/// checks and recorded sha256 are run over this text (`ProfileStrategy::accept_text`), exactly as the
/// hub's read would have run them. `sdk` is the numbered SDK the artifact was built against (read off
/// its path by the child's crate, `vike_strategy_sandbox::artifact_of`), which every result of the run
/// records.
#[derive(Debug, Clone)]
pub struct LoadedMount {
    pub builder: StrategyBuilder,
    pub text: String,
    pub sdk: Option<u32>,
}

impl StrategyFiles {
    /// A hub's: its folder, and no `.rs` file built in its process.
    pub fn hub(dir: PathBuf) -> Self {
        StrategyFiles { dir, rust: RustFileBuild::Sandboxed }
    }

    /// **Test support, never a door**: no folder, and every `.rs` file built by `build`, a test's
    /// stand-in (the library names no builder of its own).
    pub fn stand_in(build: StandIn) -> Self {
        StrategyFiles { dir: PathBuf::new(), rust: RustFileBuild::StandIn(build) }
    }

    /// Every file of [`Self::dir`] with its states, as a Studio lists them
    /// (`Request::ListStrategyFiles`): `vike_strategy::file::scan`'s path, build state and broken
    /// reason, plus the `edited` mark.
    ///
    /// shortcut: no sandbox probe reaches the list, so `cannot run here` is never listed; a run on a
    /// box without the sandbox is still refused by name. Upgrade when the hub holds that answer.
    ///
    /// shortcut: this process cannot name THIS release's build folder (only the loader's crate can,
    /// and a hub links none), so a `.rs` file's state is read from whichever build folder under the
    /// artifact directory holds its sha; the builder removes every other folder at its start and
    /// after each build. Upgrade when the folder's name moves to a crate both may link.
    pub fn listing(&self) -> Vec<WireStrategyFile> {
        let folders = build_folders();
        vike_strategy::file::scan::scan(&self.dir, None, None)
            .files
            .into_iter()
            .map(|entry| WireStrategyFile {
                build: match (&entry.build, &entry.sha256) {
                    (Some(_), Some(sha)) => build_state_in(&folders, sha).to_string(),
                    _ => String::new(),
                },
                broken: entry.header.err().unwrap_or_default(),
                edited: self.edited(&entry.path),
                file: entry.path,
            })
            .collect()
    }

    /// The `edited` mark: `relative` differs from its twin under `<project>/vendor/vike-strategies`,
    /// beside this folder (`vike_strategy::differs_from_shipped`).
    fn edited(&self, relative: &str) -> bool {
        self.dir.parent().is_some_and(|project| {
            let shipped = project.join(vike_paths::state_path::SHIPPED_STRATEGIES_DIR);
            vike_strategy::differs_from_shipped(&shipped, &self.dir, Path::new(relative))
        })
    }

    /// **Write one file of [`Self::dir`]** (`Request::SaveStrategyFile`, the owner's ruling of
    /// 2026-11-13: the backtesthub manages the strategy files), created or replaced ATOMICALLY by
    /// `vike_utility::atomic_file::write_atomic` (a temporary file beside it, then a rename: a reader
    /// sees the old text or the new, never a torn file). Answers the text's sha256 and the file's
    /// states as the list now shows them. A running live mount is not touched: it keeps the file it
    /// opened until it restarts.
    ///
    /// Refused, nothing written: a path `vike_strategy::file::scan::resolve` refuses (empty,
    /// absolute, `..`, neither `.rs` nor `.rhai`); a folder on the way that is a link or not a folder
    /// (a link could lead the write out of the folder: checked BEFORE any folder is created, and the
    /// created parent's real path must still be inside); a destination that is a link.
    pub fn save_text(&self, file: &str, text: &str) -> Result<(String, WireStrategyFile), String> {
        let path = vike_strategy::file::scan::resolve(&self.dir, file)?;
        let refuse = |why: &str| format!("strategy file {file:?} is not saved: {why}");
        let cannot = |e: std::io::Error| refuse(&format!("{e}"));
        std::fs::create_dir_all(&self.dir).map_err(cannot)?;
        let root = self.dir.canonicalize().map_err(cannot)?;
        // Every folder that already exists between the root and the file is a real folder.
        let parent = path.parent().unwrap_or(&self.dir);
        let mut at = self.dir.clone();
        for part in parent.strip_prefix(&self.dir).map_err(|_| refuse("outside the folder"))? {
            at.push(part);
            match std::fs::symlink_metadata(&at) {
                Ok(m) if m.file_type().is_symlink() => {
                    return Err(refuse("a folder on its path is a link, which could lead out"));
                }
                Ok(m) if !m.is_dir() => return Err(refuse("a part of its path is not a folder")),
                Ok(_) => {}
                Err(_) => break,
            }
        }
        std::fs::create_dir_all(parent).map_err(cannot)?;
        if !parent.canonicalize().map_err(cannot)?.starts_with(&root) {
            return Err(refuse("its folder leads out of the strategies folder"));
        }
        if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(refuse("it is a link, and a link is not written through"));
        }
        vike_utility::atomic_file::write_atomic(&path, text.as_bytes()).map_err(cannot)?;
        let sha256 = vike_hub_protocol::hash::sha256_hex(text.as_bytes());
        let rust =
            vike_strategy::file::scan::language_of(&path) == Some(StrategyFileLanguage::Rust);
        let entry = WireStrategyFile {
            build: if rust {
                build_state_in(&build_folders(), &sha256).to_string()
            } else {
                String::new()
            },
            broken: vike_strategy::file::StrategyFileHeader::parse(text).err().unwrap_or_default(),
            edited: self.edited(file),
            file: file.to_string(),
        };
        Ok((sha256, entry))
    }

    /// One file's text by its path under [`Self::dir`] (`Request::ReadStrategyFile`). Refused: a
    /// path `vike_strategy::file::scan::resolve` refuses, a symbolic link (the scan lists one
    /// unread, and a run reads none), a file that resolves outside the folder, a missing or non-UTF-8
    /// file.
    pub fn read_text(&self, file: &str) -> Result<String, String> {
        let path = vike_strategy::file::scan::resolve(&self.dir, file)?;
        if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(format!(
                "strategy file {file:?} is a link, and a link is not read: copy the file itself \
                 into the strategies folder"
            ));
        }
        let cannot = |e: std::io::Error| format!("strategy file {file:?}: cannot read it: {e}");
        // A linked FOLDER on the way would lead out of the folder: the real path must stay inside.
        let (real, root) =
            (path.canonicalize().map_err(cannot)?, self.dir.canonicalize().map_err(cannot)?);
        if !real.starts_with(&root) {
            return Err(format!(
                "strategy file {file:?} leads out of the strategies folder through a link, and is \
                 not read"
            ));
        }
        std::fs::read_to_string(&real).map_err(cannot)
    }

    /// [`Self::prepare_mount`] of a run with ONE strategy table (a `[strategy]` profile, a Studio spec):
    /// mount 0. Refused in a child that loaded several mounts' files, where a table that names no mount
    /// would build with the first mount's facts.
    pub fn prepare(&self, strategy: &mut ProfileStrategy) -> Result<(), HarnessError> {
        if matches!(&self.rust, RustFileBuild::Loaded(mounts) if mounts.len() > 1) {
            return Err(HarnessError::from(ProfileRefusal::MountsPreparedAsOneStrategyTable));
        }
        self.prepare_mount(0, strategy)
    }

    /// Mount `mount`'s `strategy` file (its index among the profile's strategy tables in profile order;
    /// 0 for `[strategy]` or a Studio spec) made ready to run in this process: a `.rhai` file read under
    /// [`Self::dir`]; a `.rs` file read there too in a hub (its hash, its header; the run itself goes to a
    /// sandboxed child), given that mount's loaded file in that child ([`RustFileBuild::Loaded`]), or its
    /// stand-in in a test.
    pub fn prepare_mount(
        &self,
        mount: usize,
        strategy: &mut ProfileStrategy,
    ) -> Result<(), HarnessError> {
        match (strategy.language()?, &self.rust) {
            (StrategyFileLanguage::Rhai, _)
            | (StrategyFileLanguage::Rust, RustFileBuild::Sandboxed) => {
                strategy.read_file(&self.dir)
            }
            (StrategyFileLanguage::Rust, RustFileBuild::Loaded(mounts)) => {
                let Some(LoadedMount { builder, text, sdk }) = mounts.get(mount) else {
                    return Err(HarnessError::from(ProfileRefusal::StrategyFileMountNotLoaded {
                        file: strategy.file.clone(),
                        mount,
                        loaded: mounts.len(),
                    }));
                };
                strategy.accept_text(text.clone())?;
                if let Some(source) = &mut strategy.source {
                    source.sdk = *sdk;
                }
                strategy.builder = Some(builder.clone());
                Ok(())
            }
            (StrategyFileLanguage::Rust, RustFileBuild::StandIn(build)) => {
                strategy.builder = Some(StrategyBuilder::stand_in(&strategy.file, *build));
                Ok(())
            }
        }
    }
}

impl StrategyFiles {
    /// Every strategy file `profile` names made ready: its `[strategy]` through the one-table entry
    /// ([`Self::prepare`], which a child that loaded several mounts' files refuses), or each of its `[[mounts]]`'
    /// as its mount in mount order ([`Self::prepare_mount`]: a sandboxed child loaded one file a mount, decision
    /// 0281).
    pub fn prepare_profile(
        &self,
        profile: &mut vike_backtest::run::profile::ProfileBacktest,
    ) -> Result<(), HarnessError> {
        if let Some(strategy) = &mut profile.strategy {
            return self.prepare(strategy);
        }
        for (mount, strategy) in profile.strategies_mut().enumerate() {
            self.prepare_mount(mount, strategy)?;
        }
        Ok(())
    }
}

/// The build folders under the artifact directory ([`strategy_files_dir`]) a listed `.rs` file's
/// state is read from (`StrategyFiles::listing`'s second shortcut says why every one): every folder
/// inside each SDK's folder (`<builds>/sdk-N/<folder>/`, decision 0296), and, for a directory an older
/// release wrote, every folder directly under it.
fn build_folders() -> Vec<PathBuf> {
    let folders_in = |dir: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .map(|entries| entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect())
            .unwrap_or_default()
    };
    folders_in(&strategy_files_dir())
        .into_iter()
        .flat_map(|top| {
            let inner = folders_in(&top);
            std::iter::once(top).chain(inner)
        })
        .collect()
}

/// The build state of a `.rs` file whose text hashes to `sha256`: the first of `folders` that holds
/// a build or a failure for it, else `not built`.
fn build_state_in(
    folders: &[PathBuf],
    sha256: &str,
) -> vike_strategy::file::scan::StrategyFileBuildState {
    use vike_strategy::file::scan::{StrategyFileBuildState, build_state};
    folders
        .iter()
        .map(|folder| build_state(sha256, Some(folder), None))
        .find(|state| *state != StrategyFileBuildState::NotBuilt)
        .unwrap_or(StrategyFileBuildState::NotBuilt)
}

/// **The profile's `[strategy]` table a spec runs as** — the ONE place the buffer/file/artifact
/// branch is taken, so a Studio run, every sweep point and every walk-forward window build their
/// strategy where a `vike-cli backtest run` builds its own: `ProfileStrategy::build`.
///
/// - An editor buffer is a `.rhai` source the run holds ([`EDITOR_BUFFER_FILE`]); a sweep point's
///   overrides join the table and reach the script as `param()` bindings.
/// - A file is read under `files.dir` when it is `.rhai`, and built by `files.rust` when it is
///   `.rs` (a hub builds none: it routes the run to a sandboxed child).
/// - A built artifact named by `(name, sha)` is REFUSED, naming the file to run instead (plan 2b): no
///   hub loads an artifact, and a `.rs` FILE runs in a sandboxed child.
pub fn strategy_config(
    spec: &StrategySpec,
    files: &StrategyFiles,
) -> Result<ProfileStrategy, HarnessError> {
    Ok(match spec {
        StrategySpec::Rhai(src) => ProfileStrategy {
            source: Some(StrategySource::of(src.clone())),
            ..ProfileStrategy::new(EDITOR_BUFFER_FILE, Params::new())
        },
        // The buffer's table is the profile's `[strategy.params]`, as a file's is: the table a scan point's
        // overrides are written over (`ProfileStrategy::with_overrides`), so a Run here is that point.
        StrategySpec::RhaiWithParameters { source, parameters } => ProfileStrategy {
            source: Some(StrategySource::of(source.clone())),
            ..ProfileStrategy::new(EDITOR_BUFFER_FILE, parameters.clone())
        },
        StrategySpec::File { file, params, sdk } => {
            let mut config =
                ProfileStrategy { sdk: *sdk, ..ProfileStrategy::new(file.clone(), params.clone()) };
            files.prepare(&mut config)?;
            config
        }
        StrategySpec::StrategyFile { name, .. } => {
            return Err(HarnessError::from(ProfileRefusal::StrategyArtifactNoLongerRun {
                name: name.clone(),
            }));
        }
    })
}

/// A strategy that would not build, in the class its spec says: a Rhai script's (an editor buffer or
/// a `.rhai` file) is a compile error, any other a strategy error.
pub(crate) fn spec_error(spec: &StrategySpec, e: &HarnessError) -> RunError {
    let rhai_file = matches!(spec, StrategySpec::File { file, .. } if file.ends_with(".rhai"));
    match spec {
        StrategySpec::Rhai(_) | StrategySpec::RhaiWithParameters { .. } => {
            RunError::Compile(e.message().to_string())
        }
        _ if rhai_file => RunError::Compile(e.message().to_string()),
        _ => RunError::Strategy(e.message().to_string()),
    }
}

/// Resolve a [`StrategySpec`] into the boxed strategy every runner drives: [`strategy_config`]'s table,
/// built by `ProfileStrategy::build`.
pub fn build_strategy(
    spec: &StrategySpec,
    files: &StrategyFiles,
) -> Result<Box<dyn Strategy<OrderBrokerSimulator>>, RunError> {
    let config = strategy_config(spec, files).map_err(|e| spec_error(spec, &e))?;
    config.build().map_err(|e| spec_error(spec, &e))
}

/// [`build_strategy`] with one sweep point's `(name, value)` overrides written over the table
/// (`ProfileStrategy::with_overrides`, the harness's own merge): Rhai reads each as a `param()` binding,
/// a `.rs` file as the kind it carries.
pub fn build_strategy_with(
    spec: &StrategySpec,
    files: &StrategyFiles,
    overrides: &[(String, ParamValue)],
) -> Result<Box<dyn Strategy<OrderBrokerSimulator>>, RunError> {
    let config = strategy_config(spec, files).map_err(|e| spec_error(spec, &e))?;
    config.with_overrides(overrides).build().map_err(|e| spec_error(spec, &e))
}
