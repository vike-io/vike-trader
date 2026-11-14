//! RUNTIME strategy-mount resolution (split-plane B5) — the daemon's
//! [`vike_core::CoreConfig::strategy_factory`], built on the SAME `ProfileDaemon` machinery a
//! `[strategy]` table faces at load, so a wire [`MountSpec`] meets the profile vocabulary's OWN
//! refusals (no file, the retired `name`/`rhai` keys, the path rule, the file's header and its
//! params checks) rather than a parallel, drift-prone re-implementation.
//!
//! The reuse is LITERAL: [`profile_for`] builds the spec's one-mount [`ProfileDaemon`] (typed,
//! the value a parse of the equivalent profile document yields) and runs it through
//! `ProfileDaemon::validate` — the refusals [`ProfileDaemon::from_toml_str`] applies after its
//! parse — then [`resolve_spec`] resolves through [`ProfileDaemon::resolve_strategy`], the same
//! call `main` makes for the spawn-time mount. A `[strategy]` selection is REQUIRED on this path as
//! on the profile's: there is no default strategy.
//!
//! Validation runs TWICE by design: once at the server edge ([`validate_spec`] from
//! `server::control::lower_command`, so a remote peer gets a `Response::Error` carrying the real message)
//! and again inside the factory on the core's fold thread — the edge is UX, the factory is the
//! authority (an in-process caller skips the edge entirely).
//!
//! ## The strategies folder, and the fence on a `file` that arrives over the wire
//!
//! A strategy file is read from the project's strategies folder ([`project_strategies_dir`]:
//! `<project>/strategies/`, beside the `settings/` folder the process declared), by a path relative
//! to it with no `..` (`vike_strategy::file::scan::resolve`). A process that declared no project
//! reads no file and refuses every mount.
//!
//! `wire_file_refusal` answers at the preview and at `server::control::lower_command`. A wire `file`
//! passes only when, with every link followed, it is a regular `.rs` or `.rhai` file of at most
//! [`MAX_STRATEGY_FILE_BYTES`] INSIDE the strategies folder. Everything else gets one sentence,
//! decided so that it says nothing about whether a file outside the folder exists: a device or a
//! FIFO would stall the fold, and a `KEY=value` file linked in would echo its value through a
//! header or compile error into `MOUNT REFUSED` and `recent_events`.
//!
//! The daemon's PROFILE, the operator's own document, takes the regular-file and size checks
//! (`read_strategy_file`) and no link fence; so do a resurrected record and an in-process command.
//!
//! The wire mount's file is read at that door, on the connection's thread, and handed to the
//! factory through `DOOR_READS`, because the factory runs on the fold and `MountSpec` (in `vike-execution`,
//! a measured crate) has nowhere to carry it. The compile stays on the fold, bounded by the size cap
//! and the engine's own limits; a resurrected record and an in-process command are still read there,
//! a resurrected one ahead of the first market message.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use vike_core::LiveBroker;
use vike_execution::MountSpec;

use crate::config::{ProfileDaemon, ProfileDaemonConfig, ProfileStrategy};

/// The largest strategy file this daemon reads. A shipped file is a few KiB, so this bounds a mistake
/// rather than a real strategy; without it a big file is memory on the fold.
pub const MAX_STRATEGY_FILE_BYTES: u64 = 1024 * 1024;

/// What a wire mount read at the door, by the file's path on this box, until the factory takes it.
/// Bounded: one entry per file inside the strategies folder, each under [`MAX_STRATEGY_FILE_BYTES`].
/// An entry the factory never reached (a duplicate mount id, a full lane) is replaced by the next
/// wire mount of that file.
static DOOR_READS: Mutex<BTreeMap<PathBuf, String>> = Mutex::new(BTreeMap::new());

/// The project's strategies folder, `<project>/strategies/`
/// (`vike_paths::state_path::PROJECT_STRATEGIES_DIR`): the folder beside the `settings/` folder whose
/// state directory this process declared (`vike_boot::halt::declare_project_state_dir`, at boot, with
/// the boot's ONE walk's `Booted::state_dir`, `<settings>/state`), through the one resolver over a
/// resolved settings directory (`vike_paths::state_path::strategies_dir_beside`). `None` in a process
/// that declared no project (a test that never booted), where no file is read.
///
/// ⚠ **The answer is the BOOTED project's, so it is only as right as the boot's.** A process started with
/// `VIKE_SETTINGS_DIR=<dir>` reads `<dir>/../strategies`: the deploy's pre-flight therefore lays its probe
/// out as a project, `<probe>/settings` beside `<probe>/strategies` (a link to the real project's folder,
/// `deploy/docker/vikectl`'s `preflight`), rather than naming the probe directory itself.
pub fn project_strategies_dir() -> Option<PathBuf> {
    strategies_dir_of_state(&vike_boot::halt::declared_project_state_dir()?)
}

/// [`project_strategies_dir`] over a declared state directory, `<settings>/state`.
fn strategies_dir_of_state(state: &Path) -> Option<PathBuf> {
    vike_paths::state_path::strategies_dir_beside(state.parent())
}

/// [`project_strategies_dir`], or the refusal of a mount on a process that has none.
pub(crate) fn strategies_dir_or_refusal() -> Result<PathBuf, String> {
    project_strategies_dir().ok_or_else(no_project_refusal)
}

fn no_project_refusal() -> String {
    "this process declared no project, so it has no `strategies/` folder to read a strategy file \
     from: start it inside a project (the folder holding `settings/`)"
        .to_string()
}

/// Why the file at `path` may not be read as a strategy file, whoever named it: `None` for a regular
/// file within the cap. It opens nothing, so a FIFO cannot block it.
fn strategy_file_refusal(path: &Path) -> Option<String> {
    let shown = path.display();
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(e) => return Some(format!("read strategy file {shown}: {e}")),
    };
    if !meta.is_file() {
        return Some(format!("read strategy file {shown}: not a regular file"));
    }
    (meta.len() > MAX_STRATEGY_FILE_BYTES).then(|| strategy_file_too_big(path))
}

fn strategy_file_too_big(path: &Path) -> String {
    format!("read strategy file {}: larger than {MAX_STRATEGY_FILE_BYTES} bytes", path.display())
}

/// Read the strategy file at `path` after [`strategy_file_refusal`], and no further than the cap: a
/// `/proc` file is regular and reports a length of 0, so the stat alone does not bound the read.
///
/// shortcut: a FIFO swapped in between the stat and the open still blocks; that takes write access to
/// the strategies folder itself, and opening with `O_NONBLOCK` is the upgrade if it ever matters.
fn read_strategy_file(path: &Path) -> Result<String, String> {
    if let Some(refusal) = strategy_file_refusal(path) {
        return Err(refusal);
    }
    let mut source = String::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(MAX_STRATEGY_FILE_BYTES + 1).read_to_string(&mut source))
        .map_err(|e| format!("read strategy file {}: {e}", path.display()))?;
    if source.len() as u64 > MAX_STRATEGY_FILE_BYTES {
        return Err(strategy_file_too_big(path));
    }
    Ok(source)
}

/// **The fence on a `file` that arrives over the wire**, the module doc's rule: `None` when it
/// passes. Read-only, so a preview can ask it. A path the strategies folder cannot hold (absolute,
/// `..`, not `.rs`/`.rhai`) is answered before the filesystem is asked anything, a missing file
/// reads as one outside the folder, and a process with no strategies folder refuses every file.
pub(crate) fn wire_file_refusal(file: &str) -> Option<String> {
    let outside = || {
        format!(
            "`file` {file:?} is not a `.rs` or `.rhai` file inside this node's `strategies/` \
             folder, and a strategy file named to the node may be nothing else. Put the file under \
             the project's `strategies/` folder and name its path there."
        )
    };
    let root = match strategies_dir_or_refusal() {
        Ok(root) => root,
        Err(why) => return Some(why),
    };
    let Ok(path) = vike_strategy::file::scan::resolve(&root, file) else { return Some(outside()) };
    match (root.canonicalize(), path.canonicalize()) {
        (Ok(root), Ok(real))
            if real.starts_with(&root)
                && vike_strategy::file::scan::language_of(&real).is_some() =>
        {
            strategy_file_refusal(&path)
        }
        _ => Some(outside()),
    }
}

/// [`wire_file_refusal`], then read the file here, on the connection's thread, for the factory to
/// take on the fold ([`strategy_source`]).
pub(crate) fn read_wire_file(file: &str) -> Result<(), String> {
    if let Some(refusal) = wire_file_refusal(file) {
        return Err(refusal);
    }
    let path = vike_strategy::file::scan::resolve(&strategies_dir_or_refusal()?, file)?;
    let source = read_strategy_file(&path)?;
    DOOR_READS.lock().unwrap_or_else(PoisonError::into_inner).insert(path, source);
    Ok(())
}

/// The text of the strategy file at `path`: what a wire mount read at the door, else the file,
/// checked.
pub(crate) fn strategy_source(path: &Path) -> Result<String, String> {
    let door_read = DOOR_READS.lock().unwrap_or_else(PoisonError::into_inner).remove(path);
    door_read.map_or_else(|| read_strategy_file(path), Ok)
}

/// Validate a runtime mount spec with the daemon's profile refusals, resolving nothing. Pure over
/// the spec (no filesystem read: the file's existence, header and compile are checked at resolve,
/// exactly like a profile load vs. its resolve — see `ProfileStrategy::file`'s doc for that split).
pub fn validate_spec(spec: &MountSpec) -> Result<(), String> {
    profile_for(spec).map(|_| ())
}

/// Resolve a runtime mount spec into the strategy object to mount — validate ([`profile_for`]),
/// then the SAME resolution the spawn-time mount goes through, over the strategies folder
/// `strategies` and under the node's live gate `live`; a `.rs` file is refused here
/// (`crate::config::not_at_runtime`: its build and its child's start would run on the core's thread).
pub fn resolve_spec(
    spec: &MountSpec,
    strategies: &Path,
    live: bool,
) -> Result<Box<dyn vike_model::Strategy<LiveBroker> + Send>, String> {
    Ok(resolve_spec_file(spec, strategies, live)?.into_strategy())
}

/// [`resolve_spec`], keeping which file it read and that file's sha256
/// (`MountResolvedStrategy::strategy_file`).
fn resolve_spec_file(
    spec: &MountSpec,
    strategies: &Path,
    live: bool,
) -> Result<crate::config::MountResolvedStrategy, String> {
    profile_for(spec)?.resolve_mount_with(strategies, live, &crate::config::not_at_runtime)
}

/// The daemon's [`vike_core::StrategyFactory`]: [`resolve_spec`] over the strategies folder
/// `strategies` (the daemon passes [`project_strategies_dir`]; `None` refuses every mount), under the
/// node's live gate `live` (`flags.tradehub_live`: a file whose header does not say
/// `runs_on = "live"` is refused on a live node). Runs on the core's fold thread inside the
/// `Command::MountStrategy` arm (an occasional operator verb — the `CoreConfig::strategy_factory`
/// doc carries the latency argument). A wire mount's file was read before it got here; the module
/// doc says which routes still read on this thread. It hands the core the file and the sha256 of the
/// bytes it compiled, which the core journals as the mount opens.
pub fn strategy_factory(strategies: Option<PathBuf>, live: bool) -> vike_core::StrategyFactory {
    Box::new(move |spec: &MountSpec| {
        let root = strategies.as_deref().ok_or_else(no_project_refusal)?;
        let mounted = resolve_spec_file(spec, root, live)?;
        let file = mounted.strategy_file();
        Ok((mounted.into_strategy(), Some(file)))
    })
}

/// **The strategy code of `strategy_name` on `mount_id`** (decision 0270): the settings store's `strategy_code` row
/// under `settings_dir`, insert-or-get, so a mounted strategy keeps its code across a restart and a re-mount and
/// another strategy on the same series gets another. Read for every boot mount (`tradehub_cli`'s `resolve_mounts`),
/// a wire mount (`server::control::lower_command`) and a resurrected one ([`resurrect_runtime_mounts`]), never on
/// the fold. `Ok(None)`, the mount's ids carrying `0000`, on a box with no store (a paper or test box), said at
/// `warn` once per mount; an empty name, the codes all spent and a store that does not answer are refused in words.
pub(crate) fn mount_order_client_id_code(
    settings_dir: Option<&Path>,
    mount_id: &str,
    strategy_name: &str,
) -> Result<Option<vike_model::orders::client_id::OrderClientIdStrategyCode>, String> {
    if strategy_name.trim().is_empty() {
        return Err(format!(
            "mount `{mount_id}` names no strategy, and the strategy's name is what its order client ids' \
             strategy code is kept by: name the strategy"
        ));
    }
    let no_store = || {
        tracing::warn!(
            target: "vike_tradehub::mount_factory",
            mount_id,
            strategy = strategy_name,
            "no settings store: this mount's order client ids carry strategy code 0000 (no strategy)"
        );
        Ok(None)
    };
    let Some(dir) = settings_dir else { return no_store() };
    match vike_secrets::strategy_code::strategy_code_for(dir, mount_id, strategy_name) {
        Ok(id) => Ok(vike_model::orders::client_id::OrderClientIdStrategyCode::new(id)),
        Err(e) if matches!(e.kind, vike_secrets::StoreErrorKind::NoSettingsDatabase) => no_store(),
        Err(e) => Err(format!("mount `{mount_id}`: its strategy code could not be read: {e}")),
    }
}

/// The name a mount's strategy code is kept by: the stem of the file the spec names
/// (`ProfileDaemon::strategy_name`'s rule, the boot mounts' own), empty for a spec naming none.
pub(crate) fn order_client_id_strategy_name(spec: &MountSpec) -> &str {
    spec.file
        .as_deref()
        .and_then(|file| Path::new(file).file_stem())
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or_default()
}

/// [`mount_order_client_id_code`] for a runtime `spec`, under the mount id the core keys it by.
pub(crate) fn runtime_mount_order_client_id_code(
    settings_dir: Option<&Path>,
    spec: &MountSpec,
) -> Result<Option<vike_model::orders::client_id::OrderClientIdStrategyCode>, String> {
    let mount_id = vike_core::strategy_state::mount_id_with(
        spec.controller_id.as_deref(),
        &spec.venue,
        &spec.symbol,
        &spec.interval,
    );
    mount_order_client_id_code(settings_dir, &mount_id, order_client_id_strategy_name(spec))
}

/// What [`resurrect_runtime_mounts`] did: how many recorded mounts were re-sent into the core's
/// command lane, and how many were skipped at the validation edge. Counts, not verdicts — a SENT
/// record can still refuse INSIDE the core (duplicate id, venue-less engine), where the refusal
/// is a recent-events note exactly as it would be for a wire mount.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ResurrectOutcome {
    /// Records re-sent as `Command::MountStrategy` (the core decides the rest).
    pub sent: usize,
    /// Records skipped at the edge — [`validate_spec`] refused (logged per record).
    pub skipped: usize,
}

/// Resurrect RUNTIME strategy mounts from the topology sidecar (split-plane B5, residual
/// closed): read `vike_core::mount_topology`'s records under `state_dir` and re-send each as an
/// ordinary [`vike_execution::Command::MountStrategy`] through `send` — the SAME command a wire mount
/// lowers into, so every record meets the same resolution and the same refusals it met the first
/// time. The split mirrors the wire path exactly: each record is edge-validated with
/// [`validate_spec`] first (the check `server::control::lower_command` applies to a remote peer's spec —
/// a record that no longer validates is SKIPPED with a `warn!` naming the mount and the reason),
/// and a record that passes the edge still faces the core arm's own authority checks (duplicate
/// id, engine-less venue, the factory's resolve), whose refusals surface as recent-events notes.
/// NOTHING here can fail a boot: a corrupt topology file reads as empty inside
/// `vike_core::mount_topology::read` (loudly), an absent one silently, and this function returns
/// counts, never an error.
///
/// ORDERING CONTRACT (the call-site rule, stated here because both daemon arms must obey it):
/// call this AFTER the core spawned and BEFORE the feeds arm. The ingest lane is FIFO
/// ([`vike_core::CoreHandle::send_command`] is the lossless lane), so every resurrected mount
/// folds ahead of the first market message — and the mount arm loads the mount's
/// `<mount_id>.json` strategy-state sidecar as part of mounting, so a resurrected strategy has
/// its saved state before any tick can reach it.
pub fn resurrect_runtime_mounts(
    state_dir: &std::path::Path,
    mut send: impl FnMut(vike_execution::Command),
) -> ResurrectOutcome {
    let mut outcome = ResurrectOutcome::default();
    for mut record in vike_core::mount_topology::read(state_dir) {
        let id = record.mount_id();
        // The strategy code is asked of the store again (insert-or-get: the same code it had), never taken from the
        // file: a record written before codes existed has none.
        let coded = validate_spec(&record.spec).and_then(|()| {
            let settings = crate::tradehub_cli::settings::booted_settings_dir();
            record.spec.order_client_id_code =
                runtime_mount_order_client_id_code(settings, &record.spec)?;
            Ok(())
        });
        match coded {
            Err(reason) => {
                tracing::warn!(
                    target: "vike_tradehub::mount_factory",
                    mount_id = %id,
                    %reason,
                    "skipping a recorded runtime mount that no longer validates — the daemon \
                     boots without it; fix the spec (or unmount intent) and re-mount by hand"
                );
                outcome.skipped += 1;
            }
            Ok(()) => {
                tracing::info!(
                    target: "vike_tradehub::mount_factory",
                    mount_id = %id,
                    venue = %record.spec.venue,
                    symbol = %record.spec.symbol,
                    "resurrecting runtime strategy mount from the topology sidecar"
                );
                send(vike_execution::Command::MountStrategy(Box::new(record.spec)));
                outcome.sent += 1;
            }
        }
    }
    outcome
}

/// Build the spec's one-mount [`ProfileDaemon`] and run the profile's FULL load validation
/// ([`ProfileDaemon::validate`], the refusals [`ProfileDaemon::from_toml_str`] applies after its
/// parse) — the one reuse point everything above shares.
///
/// The profile is built TYPED, field for field, and is the value a parse of the equivalent
/// one-mount document would yield: the spec's `venue`/`symbol`/`interval`, a `[strategy]` table
/// carrying its `file`/`name`/`rhai`/`params`, and every other field at its serde default (`None`,
/// [`ProfileDaemonConfig::default`], no `[[mounts]]`). Nothing the parse did is skipped: the document
/// held only keys `deny_unknown_fields` admits, and the params are already a [`vike_model::params::Params`] (a
/// non-table never deserialized into a `MountSpec`). The spec's `account` and `controller_id`
/// are deliberately NOT carried, exactly as the document never carried them: the core's mount
/// arm resolves those against its engines.
///
/// ⚠ So a runtime mount's price grid is the venue's DEFAULT (`ProfileDaemon::to_mount_spec`): a
/// sandboxed file is told that grid at open, and the engine's real one is the broker's to answer.
fn profile_for(spec: &MountSpec) -> Result<ProfileDaemon, String> {
    let profile = ProfileDaemon {
        venue: Some(spec.venue.clone()),
        asset_class: None,
        symbol: Some(spec.symbol.clone()),
        token_id: None,
        strategy: Some(ProfileStrategy {
            file: spec.file.clone(),
            name: spec.name.clone(),
            rhai: spec.rhai.clone(),
            params: spec.params.clone(),
        }),
        resolution_ts_ms: None,
        interval: Some(spec.interval.clone()),
        interval_ms: None,
        qty: None,
        half_spread: None,
        tick_size: None,
        seed_cash: None,
        data_only: None,
        account: None,
        daemon: ProfileDaemonConfig::default(),
        // A runtime mount's spec names no `[inputs]`: the core's one input opener carries the profile's.
        inputs: std::collections::BTreeMap::new(),
        mounts: Vec::new(),
    };
    profile.validate()?;
    Ok(profile)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_strategy_files;
    use vike_model::params::ParamValue;
    use vike_strategies_check::makers::spread_maker::SpreadMaker;

    /// **The deploy probe's layout** (`deploy/docker/vikectl`'s `preflight`): a probe laid out as a project,
    /// `<probe>/settings` beside `<probe>/strategies`, reads its strategy files from `<probe>/strategies` (the link to
    /// the real folder); the layout before it, the settings copy as the probe directory itself, read
    /// `<project>/var/deploy/strategies`, which does not exist. A real project reads its own `strategies/`.
    #[test]
    fn the_strategies_folder_is_the_one_beside_the_booted_settings() {
        let at = |state: &str| strategies_dir_of_state(Path::new(state));
        assert_eq!(
            at("/srv/vike/var/deploy/probe.AbC123/settings/state"),
            Some(PathBuf::from("/srv/vike/var/deploy/probe.AbC123/strategies")),
            "the probe laid out as a project reads its own strategies/, the link to the real one"
        );
        assert_eq!(
            at("/srv/vike/var/deploy/probe.AbC123/state"),
            Some(PathBuf::from("/srv/vike/var/deploy/strategies")),
            "the old layout (VIKE_SETTINGS_DIR = the probe) read a folder no deploy makes"
        );
        assert_eq!(
            at("/srv/vike/settings/state"),
            Some(PathBuf::from("/srv/vike/strategies")),
            "a real project"
        );
        assert_eq!(at("state"), None, "a bare relative settings dir has no project above it");
    }

    /// A spec whose `params` is the JSON object the wire and the sidecar carry, decoded as
    /// `MountSpec` decodes it.
    fn spec(
        file: Option<&str>,
        name: Option<&str>,
        rhai: Option<&str>,
        params: serde_json::Value,
    ) -> MountSpec {
        MountSpec {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            account: None,
            controller_id: Some("rt-a".into()),
            file: file.map(str::to_string),
            name: name.map(str::to_string),
            rhai: rhai.map(str::to_string),
            params: serde_json::from_value(params).expect("a parameter table"),
            order_client_id_code: None,
        }
    }

    /// A spec naming `file`, with `params`.
    fn file_spec(file: &str, params: serde_json::Value) -> MountSpec {
        spec(Some(file), None, None, params)
    }

    /// `header`-only `.rs` twin of today's registry `grid` row, for the load-only cases below.
    const GRID_RS: &str = "//! ```toml\n//! [strategy]\n//! runs_on = \"live\"\n\
                           //! [params.rungs]\n//! type = \"integer\"\n//! [params.size]\n\
                           //! type = \"number\"\n//! [params.step]\n//! type = \"number\"\n//! ```\n";

    /// **A mount's strategy code** (decision 0270): the same `(mount id, strategy name)` answers the same code
    /// across a restart (a second ask of the same store, as the next boot makes) and a re-mount, another strategy on
    /// the same series and the same strategy on another series get codes of their own; a box with NO store mints
    /// `0000` (`None`) and refuses nothing; a mount that names no strategy is refused in words.
    #[test]
    fn a_mounted_strategy_keeps_its_code_across_a_restart() {
        use vike_model::orders::client_id::OrderClientIdStrategyCode;
        let dir = tempfile::tempdir().expect("a temp dir");
        vike_secrets::create_empty_store_for_test(&vike_secrets::db_path_in(dir.path()))
            .expect("a fresh settings store");
        let code = |mount: &str, name: &str| {
            mount_order_client_id_code(Some(dir.path()), mount, name)
                .expect("a code")
                .expect("a store")
        };
        let first = code("binance__BTCUSDT__1m", "ema_cross");
        assert_eq!(first, OrderClientIdStrategyCode::new(1).expect("code 0001"));
        assert_eq!(code("binance__BTCUSDT__1m", "ema_cross"), first, "the restart's ask");
        let other = code("binance__BTCUSDT__1m", "grid");
        let elsewhere = code("binance__ETHUSDT__1m", "ema_cross");
        assert!(other != first && elsewhere != first && other != elsewhere);
        assert_eq!(code("binance__BTCUSDT__1m", "ema_cross"), first, "still the same code");
        // The runtime door asks under the core's own mount id and the name the spec's FILE gives it:
        // its stem, the rule a boot mount's `ProfileDaemon::strategy_name` follows.
        let s = file_spec("user/x.rhai", serde_json::json!({}));
        let runtime = runtime_mount_order_client_id_code(Some(dir.path()), &s).expect("a code");
        assert_eq!(runtime, Some(code("rt_a", "x")), "`rt-a` is the mount id `rt_a`");
        assert_eq!(
            order_client_id_strategy_name(&file_spec("grid_dca/grid.rs", serde_json::json!({}))),
            "grid"
        );
        assert_eq!(
            order_client_id_strategy_name(&spec(None, Some("grid"), None, serde_json::json!({}))),
            "",
            "a retired name gives no name: the spec is refused before a code is asked"
        );

        let bare = tempfile::tempdir().expect("a temp dir");
        assert_eq!(
            mount_order_client_id_code(Some(bare.path()), "m", "ema_cross"),
            Ok(None),
            "no store"
        );
        assert!(!vike_secrets::db_path_in(bare.path()).exists(), "no store was created");
        assert_eq!(
            mount_order_client_id_code(None, "m", "ema_cross"),
            Ok(None),
            "no project at all"
        );

        let refused =
            mount_order_client_id_code(Some(dir.path()), "m", " ").expect_err("no strategy named");
        assert!(refused.contains("mount `m` names no strategy"), "{refused}");
    }

    /// A live-capable strategy file with well-formed params validates AND resolves, on a paper node
    /// and a live one; the factory reads it under the folder it was given.
    #[test]
    fn a_strategy_file_resolves() {
        let root = test_strategy_files::standard();
        let s = file_spec("buy_hold.rhai", serde_json::json!({"size": 2.0}));
        validate_spec(&s).expect("buy_hold.rhai with a size validates");
        for live in [false, true] {
            resolve_spec(&s, root.path(), live).expect("resolves to a boxed strategy");
        }
        let mut factory = strategy_factory(Some(root.path().to_path_buf()), false);
        let (_, file) = factory(&s).expect("the factory too");
        // The core journals what the factory read: the file and the sha256 of its bytes.
        let bytes = std::fs::read(root.path().join("buy_hold.rhai")).expect("the file");
        let want = vike_journal::JournalStrategyFile {
            path: "buy_hold.rhai".into(),
            sha256: vike_hub_protocol::hash::sha256_hex(&bytes),
        };
        assert_eq!(file, Some(want));
    }

    /// The profile vocabulary's own refusals reach the wire caller VERBATIM — a missing file and an
    /// undeclared params key each refuse with `ProfileDaemon::resolve_mount`'s message, not a
    /// re-spelled one; a factory given no folder refuses every mount naming why.
    #[test]
    fn profile_refusals_surface_verbatim() {
        let root = test_strategy_files::standard();
        let missing =
            resolve_spec(&file_spec("no_such.rhai", serde_json::json!({})), root.path(), false)
                .err()
                .expect("a missing file refuses");
        assert!(
            missing.contains("read strategy file") && missing.contains("no_such.rhai"),
            "{missing}"
        );

        let unread = file_spec("buy_hold.rhai", serde_json::json!({"qtyy": 0.5}));
        validate_spec(&unread).expect("key names are the file's: judged at resolve");
        let unread =
            resolve_spec(&unread, root.path(), false).err().expect("an unread key refuses");
        assert!(unread.contains("does not read these"), "{unread}");

        let mut nowhere = strategy_factory(None, false);
        let nowhere = nowhere(&file_spec("buy_hold.rhai", serde_json::json!({})))
            .err()
            .expect("no folder, no file");
        assert!(nowhere.contains("no `strategies/` folder"), "{nowhere}");
    }

    /// The runtime path takes the profile's selection rules: a spec must name a FILE (there is no
    /// default strategy on this path either), the retired `name`/`rhai` are refused with the fix, and
    /// the node's live gate refuses a file whose header does not say `runs_on = "live"`.
    #[test]
    fn runtime_selection_refuses_no_file_the_retired_keys_and_a_paper_file_on_a_live_node() {
        let e = validate_spec(&spec(None, None, None, serde_json::json!({})))
            .expect_err("a selection is required");
        assert!(e.contains("names no strategy file"), "{e}");
        for retired in [
            spec(None, Some("spread_maker"), None, serde_json::json!({})),
            spec(None, None, Some("u/x.rhai"), serde_json::json!({})),
            spec(Some("buy_hold.rhai"), Some("buy_hold"), None, serde_json::json!({})),
        ] {
            let e = validate_spec(&retired).expect_err("a retired key never mounts");
            assert!(e.contains("is retired"), "{retired:?}: {e}");
        }
        let root = test_strategy_files::standard();
        let paper = file_spec("paper_only.rhai", serde_json::json!({}));
        resolve_spec(&paper, root.path(), false).expect("a paper file mounts on a paper node");
        let e = resolve_spec(&paper, root.path(), true).err().expect("not on a live node");
        assert!(e.contains("cannot mount live"), "{e}");
    }

    /// A JSON integer stays an INTEGER and a JSON float a FLOAT across the wire → `Params` →
    /// profile hop: `rungs = 2.0` would be absent to the grid's strict integer read.
    #[test]
    fn the_wire_hop_keeps_integer_and_float_distinct() {
        let s = file_spec("grid.rs", serde_json::json!({"rungs": 2, "size": 2.0, "step": 1.5}));
        let profile = profile_for(&s).expect("a well-formed grid spec validates");
        let params = &profile.strategy.as_ref().expect("a [strategy] table").params;
        assert_eq!(params.get("rungs").and_then(|v| v.as_integer()), Some(2));
        assert_eq!(params.get("size").and_then(|v| v.as_float()), Some(2.0));
        // ...and the header's integer `rungs` takes the integer the wire carried.
        let root = test_strategy_files::folder(&[("grid.rs", GRID_RS)]);
        let opened =
            |_: &crate::config::SandboxOpen<'_>| -> Result<crate::config::SandboxOpened, String> {
                Ok(crate::config::SandboxOpened {
                    strategy: Box::new(SpreadMaker::new(1.0, 0.5)),
                    echo: Vec::new(),
                    remote: None,
                })
            };
        profile
            .resolve_mount_with(root.path(), false, &opened)
            .unwrap_or_else(|e| panic!("an integer `rungs` is the type the header declares: {e}"));
    }

    /// The `profile_for` this crate had until the typed build, kept here FROZEN as the oracle the
    /// typed build must agree with: the spec rendered as a one-mount profile DOCUMENT and parsed
    /// through [`ProfileDaemon::from_toml_str`].
    fn profile_via_document(spec: &MountSpec) -> Result<ProfileDaemon, String> {
        let params = toml::Value::try_from(&spec.params)
            .map_err(|e| format!("render mount spec params as TOML: {e}"))?;
        let mut strategy = toml::value::Table::new();
        if let Some(f) = &spec.file {
            strategy.insert("file".to_string(), toml::Value::String(f.clone()));
        }
        if let Some(n) = &spec.name {
            strategy.insert("name".to_string(), toml::Value::String(n.clone()));
        }
        if let Some(r) = &spec.rhai {
            strategy.insert("rhai".to_string(), toml::Value::String(r.clone()));
        }
        strategy.insert("params".to_string(), params);
        let mut root = toml::value::Table::new();
        root.insert("venue".to_string(), toml::Value::String(spec.venue.clone()));
        root.insert("symbol".to_string(), toml::Value::String(spec.symbol.clone()));
        root.insert("interval".to_string(), toml::Value::String(spec.interval.clone()));
        root.insert("strategy".to_string(), toml::Value::Table(strategy));
        let text = toml::to_string(&toml::Value::Table(root))
            .map_err(|e| format!("render mount spec as a profile document: {e}"))?;
        ProfileDaemon::from_toml_str(&text)
    }

    /// **The typed build IS the document round trip it replaced.** For every spec below — the
    /// accepted ones (integers, floats, a negative zero, strings with escapes, arrays, nested and
    /// empty tables, a script file) and each refusal the profile validation raises — `profile_for`
    /// answers what the old render-and-parse answered: the same profile, compared by its full
    /// `Debug` (every field, every float exactly), or the same refusal sentence.
    #[test]
    fn the_typed_profile_is_the_profile_the_document_round_trip_parsed() {
        let nested = serde_json::json!({
            "venues": { "binance": { "levels": [1, 2.5, "a", [true]], "empty": {} } },
        });
        let base = || file_spec("buy_hold.rs", serde_json::json!({}));
        let cases: Vec<MountSpec> = vec![
            base(),
            file_spec("grid.rs", serde_json::json!({"rungs": 2, "size": 2.0, "step": 1.5})),
            file_spec("grid.rs", serde_json::json!({"rungs": 2.0})),
            file_spec("trend/momentum.rs", nested.clone()),
            file_spec("buy_hold.rs", nested),
            file_spec("buy_hold.rs", serde_json::json!({"qtyy": 0.5})),
            spec(None, Some("no_such_strategy"), None, serde_json::json!({})),
            spec(None, None, None, serde_json::json!({})),
            spec(Some("buy_hold.rs"), Some("buy_hold"), None, serde_json::json!({})),
            file_spec("x.rhai", serde_json::json!({"n": 3, "k": -0.0})),
            file_spec("x.rhai", serde_json::json!({"s": "quoted \" back \\ tab \t"})),
            spec(None, None, Some("x.rhai"), serde_json::json!({})),
            file_spec(" ", serde_json::json!({})),
            file_spec("../x.rhai", serde_json::json!({})),
            MountSpec { symbol: String::new(), ..base() },
            MountSpec { symbol: " ".into(), ..base() },
            MountSpec { venue: "odd \"venue\"\n".into(), ..base() },
        ];
        for s in &cases {
            let typed = profile_for(s).map(|p| format!("{p:?}"));
            let parsed = profile_via_document(s).map(|p| format!("{p:?}"));
            assert_eq!(typed, parsed, "typed build and document round trip disagree on {s:?}");
        }
        // Not vacuous: both outcomes occur, and a float's sign survives the typed build.
        assert!(cases.iter().any(|s| profile_for(s).is_ok()));
        assert!(cases.iter().any(|s| profile_for(s).is_err()));
        let script = profile_for(&cases[9]).expect("a numeric rhai table validates");
        assert_eq!(
            script.strategy.expect("a [strategy] table").params.get("k"),
            Some(&ParamValue::Float(-0.0))
        );
    }
}
