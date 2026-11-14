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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use vike_model::params::Params;
use vike_strategy::file::StrategyFileHeader;
use vike_tradehub_core::remote_strategy::{
    RemoteHandle, RemoteSpec, RemoteStrategy, SandboxCommand,
};

/// Why a `.rs` file did not come to run: refused for good, or its builder did not answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenRefusal {
    /// The file does not run, and asking again changes nothing: the compiler's diagnostics, the file's own
    /// refusal, no builder or no sandbox in this process, a build of other text than was read.
    Refused(String),
    /// The strategy builder did not answer (not up yet, a version skew, its key unreadable or refused):
    /// the build is asked for again.
    BuilderUnanswered(String),
}

impl std::fmt::Display for OpenRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenRefusal::Refused(why) | OpenRefusal::BuilderUnanswered(why) => f.write_str(why),
        }
    }
}

/// How this daemon has a `.rs` file built: `(builds directory, file path, file text)` in, the path of
/// the built artifact on this box out (the builder's client, and the sandboxed child's own rule for
/// where an artifact lives), or why not ([`OpenRefusal`]: the compiler's verdict is
/// [`OpenRefusal::Refused`], a builder that did not answer [`OpenRefusal::BuilderUnanswered`]). Blocking
/// for as long as the build takes.
pub type FileBuilder = Box<dyn Fn(&Path, &str, &str) -> Result<PathBuf, OpenRefusal> + Send + Sync>;

/// The builder this daemon asks. Set once by the composition root (`crates/vike/src/main.rs`'s
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
        return Err(OpenRefusal::Refused(format!(
            "strategy file {file}: a `.rs` strategy file runs in a sandboxed process of its own, built \
             by the strategy builder, and this process was started without one. Start the daemon as \
             `vike-backend tradehub`, or name a `.rhai` file"
        )));
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
            OpenRefusal::Refused(format!(
                "strategy file {file}: no project above {} to keep its build in",
                request.strategies.display()
            ))
        })?;
    tracing::warn!(file, sha256 = request.sha256, "NOT RUNNING: building strategy file {file}");
    let artifact = build(&builds, file, request.source).map_err(|e| match e {
        OpenRefusal::Refused(why) => OpenRefusal::Refused(format!(
            "strategy file {file}: NOT RUNNING: its build failed: {why}"
        )),
        OpenRefusal::BuilderUnanswered(why) => OpenRefusal::BuilderUnanswered(format!(
            "strategy file {file}: NOT RUNNING: building, and the strategy builder did not answer: {why}"
        )),
    })?;
    if artifact.file_stem().and_then(|s| s.to_str()) != Some(request.sha256) {
        return Err(OpenRefusal::Refused(format!(
            "strategy file {file}: the builder built {}, not the text this daemon read (sha256 {})",
            artifact.display(),
            request.sha256
        )));
    }
    let params = toml::to_string(request.params).map_err(|e| {
        OpenRefusal::Refused(format!(
            "strategy file {file}: its params table did not serialize: {e}"
        ))
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
    };
    let launcher = SandboxCommand::multicall(&artifact)
        .map_err(|e| {
            OpenRefusal::Refused(format!(
                "strategy file {file}: this daemon's own program cannot be read: {e}"
            ))
        })?
        .launcher();
    let (proxy, remote) = RemoteStrategy::open(spec, launcher)
        .map_err(|e| OpenRefusal::Refused(format!("strategy file {file}: NOT RUNNING: {e}")))?;
    let echo = proxy.echo().map(<[_]>::to_vec).unwrap_or_default();
    tracing::info!(file, artifact = %artifact.display(), "strategy file built and opened in its sandboxed process");
    Ok(SandboxOpened { strategy: Box::new(proxy), echo, remote: Some(remote) })
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
    #[must_use]
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
    SandboxOpened { strategy: Box::new(AwaitingBuild), echo: Vec::new(), remote: None }
}

/// **What each `.rs` mount the start deactivated for its build says now**, by mount id: `Err(why)` while it
/// does not run (building, or its build failed: the reason), `Ok(params)` once it runs (its row's params
/// line). Written by the build threads, read by the publisher's mount rows
/// (`crate::publish::PublisherHandle::with_mount_builds`). A cheap `Clone`.
#[derive(Clone, Default)]
pub struct MountBuilds(Arc<Mutex<BTreeMap<String, Result<String, String>>>>);

impl MountBuilds {
    /// Say `state` for `mount_id`.
    pub fn set(&self, mount_id: &str, state: Result<String, String>) {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).insert(mount_id.to_string(), state);
    }

    /// What `mount_id` says now; `None` for a mount that never waited for a build.
    #[must_use]
    pub fn get(&self, mount_id: &str) -> Option<Result<String, String>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).get(mount_id).cloned()
    }
}

/// The `.rs` arm of a RUNTIME (wire) mount: refused, because its build and its child's start would
/// run on the core's thread. A `.rs` file mounts from the profile at startup ([`open`]).
///
/// # Errors
///
/// Always, naming the file.
pub(crate) fn not_at_runtime(request: &SandboxOpen<'_>) -> Result<SandboxOpened, String> {
    Err(format!(
        "strategy file {}: a `.rs` strategy file mounts from the daemon's profile at startup (it is \
         built and started in a sandboxed process of its own, which a runtime mount would do on the \
         core's thread). Name it in the profile, or mount a `.rhai` file here",
        request.file
    ))
}
