//! **The seam a `.rs` strategy file runs through: its own sandboxed process.**
//!
//! No hub process loads strategy code (decisions 0082/0083 stand): a Rust strategy file is built by
//! the builder and runs in a `vike-backend strategy-sandbox` child this daemon spawns over a pipe
//! (owner 2026-11-13, Q2), jailed before it loads the artifact. The daemon mounts the PROXY of that
//! process, an ordinary `Strategy<LiveBroker>` (`vike_tradehub_core`'s `RemoteStrategy`), and every
//! order the child asks for passes the core's gates like any other strategy's.
//!
//! [`open`] is the ONE place the profile's resolution hands a `.rs` file over:
//! `ProfileDaemon::resolve_mount` has read the file, hashed it, parsed its header and judged the
//! params table against it, and gives [`open`] everything the child's `Open` frame carries. What
//! comes back is the proxy plus the rows the file's `echo` answered, or the file's own refusal
//! (`Refused{reason}`: its `refuse` export, a build failure, no sandbox on this box) in its words.
//!
//! [`open`] has the file built first (the builder [`use_builder`] set: the process that needs a file
//! built asks the builder, plan 2b §3); a process started without a builder refuses every `.rs` file
//! by name. A file is opened only from the profile at startup: a runtime (wire) mount of one is refused
//! ([`not_at_runtime`]), because its build and its child's start would run on the core's thread.
//!
//! ## The start never waits for a build (the owner's ruling of 2026-11-14)
//!
//! The daemon's START resolves a `.rs` file without building it ([`awaiting_build`]): everything up to
//! the build is judged at once (the file, its header, its `runs_on`, its params, its sha256), and the
//! mount is DEACTIVATED with the reason `building <file>` ([`building_words`]). Its placeholder
//! ([`AwaitingBuild`]) rides the assembly only so the mount is laid out with the others, and the core
//! parks it (`vike_core::CoreConfig::mounts_awaiting_strategy`); the build and the child's start run on a
//! thread of their own ([`SandboxBuild::open`], driven by `tradehub_cli`'s `awaited_builds`), and the
//! mount starts BY ITSELF when they answer. A build the compiler refuses leaves it deactivated with the
//! diagnostic ([`OpenRefusal::Refused`]); a builder that does not answer is asked again
//! ([`OpenRefusal::BuilderUnanswered`]). The node never exits for either, and its ready line waits on
//! neither.
//!
//! What the file is told when it opens (`vike_model::params::MountFacts`): its own series, the mount's
//! price grid and the static fee lane's floor; the ACCOUNT's own rate reaches it through the mount's
//! fee re-arm (`vike_tradehub_core::remote_strategy::RemoteHandle::fee_rearm`), which rebuilds it in a
//! fresh child at that rate, as the in-process maker was re-armed. For a mount that waited for its build,
//! the rate the assembly resolved for its account is kept ([`SandboxBuild::fee_capture`]) and the re-arm
//! runs right after its child opens.
//!
//! A key the file's header declares `type = "model"` names a run of `<project>/user_data/runs/`, and the
//! file is opened with that run's model TEXT in its place ([`params_with_models`]; the jailed child reads
//! no file). A run that cannot be read refuses the mount before its build.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use vike_model::params::Params;
use vike_strategy::file::StrategyFileHeader;
use vike_tradehub_core::remote_strategy::{
    RemoteHandle, RemoteSpec, RemoteStrategy, SandboxCommand,
};

use vike_utility::errors::CarriedRefusal;

use crate::refusals::MountProfileRefusal;

/// Why a `.rs` file did not come to run: refused for good, or its builder did not answer. Each holds the refusal with
/// its one id kept (the mount row's `refusal_id`), its text bare.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenRefusal {
    /// The file does not run, and asking again changes nothing: the compiler's diagnostics, the file's own
    /// refusal, no builder or no sandbox in this process, a build of other text than was read.
    Refused(CarriedRefusal),
    /// The strategy builder did not answer (not up yet, a version skew, its key unreadable or refused):
    /// the build is asked for again.
    BuilderUnanswered(CarriedRefusal),
}

impl std::fmt::Display for OpenRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenRefusal::Refused(why) | OpenRefusal::BuilderUnanswered(why) => write!(f, "{why}"),
        }
    }
}

/// How this daemon has a `.rs` file built: `(builds directory, file path, file text)` in, the path of
/// the built artifact on this box and the numbered SDK it was built against out (`None`: a development
/// tree's, which is not frozen; decision 0296), or why not ([`OpenRefusal`]: the compiler's verdict,
/// and the builder's refusal of a retired SDK, is [`OpenRefusal::Refused`], a builder that did not
/// answer [`OpenRefusal::BuilderUnanswered`]). The builder's client, and the sandboxed child's own
/// check of the artifact's path. Blocking for as long as the build takes.
pub type FileBuilder =
    Box<dyn Fn(&Path, &str, &str) -> Result<(PathBuf, Option<u32>), OpenRefusal> + Send + Sync>;

/// The builder this daemon asks. Set once by the composition root (`crates/vike-backend/src/main.rs`'s
/// `tradehub_main`): the builder's client is a crate this one cannot name (both layer 65).
static FILE_BUILDER: OnceLock<FileBuilder> = OnceLock::new();

/// Build every `.rs` file this daemon mounts with `build` (the first call wins).
pub fn use_builder(build: FileBuilder) {
    let _ = FILE_BUILDER.set(build);
}

/// What a `.rs` file's sandboxed process is opened with: the file, the bytes the daemon read (the
/// builder builds from text, never from the folder), and the mount facts the file is told (its
/// venue, symbol, interval and price grid, and the maker round-trip fee floor of the venue's static
/// fee lane).
#[derive(Clone, Copy)]
pub struct SandboxOpen<'a> {
    /// The project's strategies folder the file was read from (its builds live beside it, under
    /// `user_data/strategy-builds`).
    pub strategies: &'a Path,
    /// The file as the profile named it (relative to the strategies folder).
    pub file: &'a str,
    /// The file on this box.
    pub path: &'a Path,
    /// The bytes that were read and hashed.
    pub source: &'a str,
    /// Lowercase-hex sha256 of [`Self::source`].
    pub sha256: &'a str,
    /// The file's parsed header.
    pub header: &'a StrategyFileHeader,
    /// The `[strategy.params]` table, already judged against the header.
    pub params: &'a Params,
    /// The mount the file trades: venue, symbol, interval and price grid.
    pub mount: &'a vike_tradehub_core::MountPlacement,
    /// The break-even round-trip maker fee of the mount's static fee lane
    /// (`vike_tradehub_core::maker_round_trip_fee_for`), `None` when no lane states one.
    pub maker_round_trip_fee: Option<f64>,
    /// The daemon's live gate (`flags.tradehub_live`): its mounts trade a demo or live account, and run
    /// only a frozen SDK ([`frozen_for_live`]).
    pub live: bool,
}

/// What an opened `.rs` file answers.
pub struct SandboxOpened {
    /// The proxy of the sandboxed process.
    pub strategy: Box<dyn vike_model::Strategy<vike_core::LiveBroker> + Send>,
    /// The rows the file's `echo` export answered: each knob and what it RESOLVED to, in the
    /// file's order (empty when the file exports no `echo`).
    pub echo: Vec<(String, String)>,
    /// The host's handle on the sandboxed mount: the core's lane is attached through it once the
    /// core runs, and the mount's fee re-arm is taken from it. `None` from a stand-in.
    pub remote: Option<RemoteHandle>,
    /// The SDK the builder built the file against (`BuildOk`'s `sdk`, decision 0296): what the journal's
    /// `JournalStrategyFile::sdk` records. `None` for a development tree's build, a placeholder or a stand-in.
    pub sdk: Option<u32>,
}

/// Open `request`'s file in its own sandboxed process: build it, then start the child and open the
/// strategy in it. [`open_built`] with the refusal in words, for a caller that waits.
///
/// # Errors
///
/// As [`open_built`], in words.
pub(crate) fn open(request: &SandboxOpen<'_>) -> Result<SandboxOpened, String> {
    open_built(request).map_err(|e| e.to_string())
}

/// [`open`]'s body: build `request`'s file (the mount does not run while it builds), then start the child
/// and open the strategy in it. Blocking for as long as the build takes.
///
/// # Errors
///
/// [`OpenRefusal::BuilderUnanswered`] when the builder did not answer; [`OpenRefusal::Refused`] for every
/// other reason: no builder in this process, no project to keep the build in, the compiler's diagnostics,
/// a build of other text than was read, or the child's own refusal.
pub(crate) fn open_built(request: &SandboxOpen<'_>) -> Result<SandboxOpened, OpenRefusal> {
    let file = request.file;
    let Some(build) = FILE_BUILDER.get() else {
        return Err(OpenRefusal::Refused(
            MountProfileRefusal::NoBuilder { file: file.to_string() }.carried(),
        ));
    };
    // The strategies folder is the BOOTED project's (`mount_factory::project_strategies_dir`), so its
    // parent is the project root the boot resolved: the builds dir is derived from it, never walked
    // for again (vike-boot's `one_owner` gate: a booting crate resolves the project once).
    let builds = request
        .strategies
        .parent()
        .map(|root| {
            root.join(vike_paths::state_path::PROJECT_USER_DATA_DIR)
                .join(vike_paths::state_path::STRATEGY_BUILDS_SUBDIR)
        })
        .ok_or_else(|| {
            OpenRefusal::Refused(
                MountProfileRefusal::NoProjectForBuild {
                    file: file.to_string(),
                    strategies: request.strategies.display().to_string(),
                }
                .carried(),
            )
        })?;
    // Before the build: a model run that cannot be read refuses the mount without one. The runs root is
    // the builds dir's sibling, `<project>/user_data/runs`.
    let params = params_with_models(
        request.header,
        request.params,
        &builds.with_file_name(vike_paths::state_path::RUNS_SUBDIR),
    )
    .map_err(|why| OpenRefusal::Refused(not_running(file, why.into())))?;
    tracing::warn!(file, sha256 = request.sha256, "NOT RUNNING: building strategy file {file}");
    let (artifact, sdk) = build(&builds, file, request.source).map_err(|e| match e {
        OpenRefusal::Refused(why) => OpenRefusal::Refused(
            MountProfileRefusal::BuildFailed { file: file.to_string(), why }.carried(),
        ),
        OpenRefusal::BuilderUnanswered(why) => OpenRefusal::BuilderUnanswered(
            MountProfileRefusal::BuilderUnanswered { file: file.to_string(), why }.carried(),
        ),
    })?;
    if artifact.file_stem().and_then(|s| s.to_str()) != Some(request.sha256) {
        return Err(OpenRefusal::Refused(
            MountProfileRefusal::BuiltOtherText {
                file: file.to_string(),
                artifact: artifact.display().to_string(),
                sha256: request.sha256.to_string(),
            }
            .carried(),
        ));
    }
    frozen_for_live(file, request.live, sdk)?;
    if request.header.strategy.sdk.is_none() {
        tracing::warn!(
            file,
            sdk = ?sdk,
            "strategy file {file} pins no SDK, so it was built against {}, the newest on this box: it \
             moves to a newer SDK when one is installed. Pin it with `[strategy] sdk = N` in its header \
             to keep it where it is",
            sdk_words(sdk)
        );
    }
    let params = toml::to_string(&params).map_err(|e| {
        OpenRefusal::Refused(
            format!("strategy file {file}: its params table did not serialize: {e}").into(),
        )
    })?;
    let mount = request.mount;
    let spec = RemoteSpec {
        file: file.to_string(),
        sha256: request.sha256.to_string(),
        params,
        facts: vike_model::params::MountFacts {
            symbol: mount.symbol.clone(),
            venue: mount.venue.clone(),
            interval: mount.interval.clone(),
            tick_size: Some(mount.tick_size),
            static_round_trip_fee_rate: request.maker_round_trip_fee,
            account_round_trip_fee_rate: None,
        },
        controller_id: mount.controller_id.clone(),
        build: vike_buildinfo::summary(),
    };
    // The child is named the builds directory and the artifact under it, and opens it beneath that.
    let beneath = artifact.strip_prefix(&builds).map_err(|_| {
        OpenRefusal::Refused(
            MountProfileRefusal::ArtifactOutsideBuilds {
                file: file.to_string(),
                artifact: artifact.display().to_string(),
                builds: builds.display().to_string(),
            }
            .carried(),
        )
    })?;
    let launcher = SandboxCommand::multicall(&builds, beneath)
        .map_err(|e| {
            OpenRefusal::Refused(
                MountProfileRefusal::OwnProgramUnreadable {
                    file: file.to_string(),
                    why: e.to_string(),
                }
                .carried(),
            )
        })?
        .launcher();
    // The child's refusal keeps its id (`FromChild::Refused`'s), which the row shows.
    let (proxy, remote) = RemoteStrategy::open(spec, launcher)
        .map_err(|why| OpenRefusal::Refused(not_running(file, why)))?;
    let echo = proxy.echo().map(<[_]>::to_vec).unwrap_or_default();
    tracing::info!(
        file,
        sdk = ?sdk,
        artifact = %artifact.display(),
        "strategy file built against {} and opened in its sandboxed process",
        sdk_words(sdk)
    );
    Ok(SandboxOpened { strategy: Box::new(proxy), echo, remote: Some(remote), sdk })
}

/// A `.rs` file that does not run for `why` (a model run it reads, or its sandboxed process's refusal).
fn not_running(file: &str, why: CarriedRefusal) -> CarriedRefusal {
    MountProfileRefusal::SandboxNotRunning { file: file.to_string(), why }.carried()
}

/// The SDK a build answered, in the words a log line says it: `SDK 3`, or the development tree's.
fn sdk_words(sdk: Option<u32>) -> String {
    sdk.map_or_else(|| "the development tree (no numbered SDK)".to_string(), |n| format!("SDK {n}"))
}

/// **A live or demo daemon runs only a FROZEN SDK** (decision 0296; the research behind it: a live mount
/// refuses an SDK not yet frozen). `sdk` `None` is a build against a development tree (a builder started
/// on a checkout, `--workspace-root`), whose sources move under the file with every commit: refused on a
/// `live` daemon, in words with the fix. A numbered SDK is installed once and never changes, and a paper
/// daemon runs either.
///
/// # Errors
///
/// [`OpenRefusal::Refused`] for a development tree's build on a live daemon.
pub(crate) fn frozen_for_live(file: &str, live: bool, sdk: Option<u32>) -> Result<(), OpenRefusal> {
    if !live || sdk.is_some() {
        return Ok(());
    }
    Err(OpenRefusal::Refused(
        MountProfileRefusal::DevelopmentSdkOnLive { file: file.to_string() }.carried(),
    ))
}

/// The params table the file is opened with: `params` with each `type = "model"` key's run id replaced by
/// that run's model text under `runs` (`vike_strategy::file::StrategyFileHeader::with_model_texts`, read by
/// `vike_runs::read_model_text`). The mount keeps the id; only the file is handed the text.
///
/// # Errors
///
/// A model run that cannot be read, naming the key, the run and why.
pub(crate) fn params_with_models(
    header: &StrategyFileHeader,
    params: &Params,
    runs: &Path,
) -> Result<Params, String> {
    header.with_model_texts(params, |run| {
        vike_runs::read_model_text(runs, run).map_err(|e| e.to_string())
    })
}

/// The reason a `.rs` mount waiting for its build is NOT RUNNING with: `NOT RUNNING: building <file>`.
#[must_use]
pub(crate) fn building_words(file: &str) -> String {
    format!("NOT RUNNING: building {file}")
}

/// The placeholder a `.rs` mount rides the assembly on while its file builds. The core PARKS that mount
/// (`vike_core::CoreConfig::mounts_awaiting_strategy`), so this strategy is never called: it has every
/// default hook, declares no input and no leg, and places nothing.
struct AwaitingBuild;

impl vike_model::Strategy<vike_core::LiveBroker> for AwaitingBuild {}

/// **A `.rs` file the start resolved and did not build**: everything `open_built` takes, owned, so the
/// build and the child's start run on a thread of their own after the node is up. Cloned at the start,
/// the clones sharing [`Self::fee_capture`]'s slot.
#[derive(Clone)]
pub struct SandboxBuild {
    strategies: PathBuf,
    file: String,
    path: PathBuf,
    source: String,
    sha256: String,
    header: StrategyFileHeader,
    params: Params,
    mount: vike_tradehub_core::MountPlacement,
    maker_round_trip_fee: Option<f64>,
    live: bool,
    /// The fee schedule the assembly resolved for the mount's ACCOUNT, once the fee arm ran (live only).
    account_fee: Arc<OnceLock<Option<vike_model::FeeSchedule>>>,
}

impl SandboxBuild {
    fn of(request: &SandboxOpen<'_>) -> Self {
        SandboxBuild {
            strategies: request.strategies.to_path_buf(),
            file: request.file.to_string(),
            path: request.path.to_path_buf(),
            source: request.source.to_string(),
            sha256: request.sha256.to_string(),
            header: request.header.clone(),
            params: request.params.clone(),
            mount: request.mount.clone(),
            maker_round_trip_fee: request.maker_round_trip_fee,
            live: request.live,
            account_fee: Arc::default(),
        }
    }

    /// The file as the profile named it.
    #[must_use]
    pub fn file(&self) -> &str {
        &self.file
    }

    /// The id the core names this mount by (`vike_core::strategy_state::mount_id_with`).
    #[must_use]
    pub fn mount_id(&self) -> String {
        vike_core::strategy_state::mount_id_with(
            self.mount.controller_id.as_deref(),
            &self.mount.venue,
            &self.mount.symbol,
            &self.mount.interval,
        )
    }

    /// The mount's fee re-arm at the assembly (`vike_tradehub_core::FeeRearm`): it KEEPS the schedule its
    /// account resolved, for [`Self::account_fee`], and replaces nothing (the placeholder is parked).
    pub fn fee_capture(&self) -> vike_tradehub_core::FeeRearm {
        let slot = Arc::clone(&self.account_fee);
        Box::new(move |schedule| {
            let _ = slot.set(schedule);
            None
        })
    }

    /// What the assembly's fee arm handed [`Self::fee_capture`]: `None` when it never ran (the paper arm).
    #[must_use]
    pub fn account_fee(&self) -> Option<Option<vike_model::FeeSchedule>> {
        self.account_fee.get().copied()
    }

    /// Build the file and open it in its sandboxed process, with `open` (`open_built` in the daemon).
    ///
    /// # Errors
    ///
    /// As `open`.
    pub fn open(
        &self,
        open: &dyn Fn(&SandboxOpen<'_>) -> Result<SandboxOpened, OpenRefusal>,
    ) -> Result<SandboxOpened, OpenRefusal> {
        open(&SandboxOpen {
            strategies: &self.strategies,
            file: &self.file,
            path: &self.path,
            source: &self.source,
            sha256: &self.sha256,
            header: &self.header,
            params: &self.params,
            mount: &self.mount,
            maker_round_trip_fee: self.maker_round_trip_fee,
            live: self.live,
        })
    }
}

/// The start's `.rs` opener: it builds NOTHING. It keeps `request` in `kept` (under `controller_id`, the id
/// the core keys the mount by) and answers the placeholder ([`AwaitingBuild`]), so the resolution around it
/// (the file, its header, its params, its sha256) is judged exactly as for an opened file.
pub(crate) fn awaiting_build(
    request: &SandboxOpen<'_>,
    controller_id: Option<&str>,
    kept: &std::cell::RefCell<Option<SandboxBuild>>,
) -> SandboxOpened {
    let mut mount = request.mount.clone();
    mount.controller_id = controller_id.map(str::to_string);
    *kept.borrow_mut() = Some(SandboxBuild::of(&SandboxOpen { mount: &mount, ..*request }));
    SandboxOpened { strategy: Box::new(AwaitingBuild), echo: Vec::new(), remote: None, sdk: None }
}

/// **What each `.rs` mount the start deactivated for its build says now**, by mount id: `Err(why)` while it
/// does not run (building, or its build failed: the row's params text, under the refusal's id when it has one),
/// `Ok(params)` once it runs (its row's params line). Written by the build threads, read by the publisher's mount
/// rows and its identity block's params line (`crate::publish::PublisherHandle::with_mount_builds`). A cheap `Clone`.
#[derive(Clone, Default)]
pub struct MountBuilds(Arc<MountBuildsShared>);

#[derive(Default)]
struct MountBuildsShared {
    states: Mutex<BTreeMap<String, Result<String, CarriedRefusal>>>,
    /// Bumped by every answer, after it is written ([`MountBuilds::generation`]).
    generation: AtomicU64,
}

impl MountBuilds {
    /// Say `state` for `mount_id`.
    pub fn set(&self, mount_id: &str, state: Result<String, CarriedRefusal>) {
        let mut states = self.0.states.lock().unwrap_or_else(PoisonError::into_inner);
        states.insert(mount_id.to_string(), state);
        self.0.generation.fetch_add(1, Ordering::Release);
    }

    /// What `mount_id` says now; `None` for a mount that never waited for a build.
    #[must_use]
    pub fn get(&self, mount_id: &str) -> Option<Result<String, CarriedRefusal>> {
        self.0.states.lock().unwrap_or_else(PoisonError::into_inner).get(mount_id).cloned()
    }

    /// How many answers were said so far: while it holds, every [`Self::get`] answers as before, so a reader
    /// may keep what it made of them (the publisher's identity line, `crate::publish`).
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.0.generation.load(Ordering::Acquire)
    }
}

/// The `.rs` arm of a RUNTIME (wire) mount: refused, because its build and its child's start would
/// run on the core's thread. A `.rs` file mounts from the profile at startup ([`open`]).
///
/// # Errors
///
/// Always, naming the file.
pub(crate) fn not_at_runtime(request: &SandboxOpen<'_>) -> Result<SandboxOpened, String> {
    Err(MountProfileRefusal::RsAtRuntime { file: request.file.to_string() }.to_string())
}
