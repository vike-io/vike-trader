//! Loading the daemon-profile row, resolving its mounts, the run profile and the paper risk budget.

use std::process::ExitCode;

use crate::MountResolved;
use crate::config::resolve_paper_risk_limits;
use vike_tradehub_core::MountPlacement;

use super::args::Args;
use super::mount_rows::mounts_wire_params;

/// **Startup phase — WHICH DAEMON PROFILE is live.** The settings store's ACTIVE daemon-profile row
/// is the only source since decision 0086: this reads the store's profile rows, warns about the
/// RETIRED `--config` argument, and lowers the winning row through `ProfileDaemon::from_toml_str`.
///
/// Returns the whole [`vike_secrets::profile_store::ProfileSet`] snapshot (the run-profile phase,
/// [`resolve_run_profile`], reads the SAME one rather than opening the store a second time), the
/// active daemon row's NAME, and the lowered profile. An `Err` is the refusal ([`ProfileRefusal`]);
/// the refusal itself (the `tracing::error!` and the `eprintln!` an operator sees) is printed here,
/// exactly as when this body was inline.
pub(super) fn load_daemon_profile(
    booted: &vike_boot::Booted,
    args: &Args,
) -> Result<
    (vike_secrets::profile_store::ProfileSet, Option<String>, crate::config::ProfileDaemon),
    ProfileRefusal,
> {
    // ⚠ WHICH PROFILE IS LIVE — since decision 0086 ("settings live only in the database")
    // verdict 1, the ACTIVE ROW IS THE ONLY SOURCE: no binary reads a profile TOML any more, so a
    // `--config` argument (RETIRED — see `Args`'s doc on `config_path`) decides nothing and a box
    // with no active daemon-profile row has no profile to fall back to. The read half needs NO unit
    // change and never did: 0057's EROFS section measures that `ProtectSystem=strict` leaves reads
    // untouched, and this daemon already opens this same store read-only at mount for the account
    // table.
    // ⚠ Tracked separately from `store_profiles` itself: a store that EXISTS and cannot be read is
    // an ERROR, never "no profiles" — the same distinction the credential store draws
    // (`StoreHealth::Unreadable`) — and the refusal below must say WHICH one this box hit, because
    // only one of them has a cure this binary can name. `profile create daemon` writes a NEW row; it
    // cannot repair a store that will not open, and telling an operator to run it for THAT fault
    // would send them to the wrong tool.
    let mut profile_store_unreadable: Option<String> = None;
    let store_profiles = match booted.settings_dir.as_deref() {
        Some(dir) => {
            match vike_secrets::profile_store::read_profiles(&vike_secrets::db_path_in(dir)) {
                Ok(p) => p,
                Err(e) => {
                    // Said once, in the refusal below that carries it.
                    profile_store_unreadable = Some(e.to_string());
                    vike_secrets::profile_store::ProfileSet::none()
                }
            }
        }
        None => vike_secrets::profile_store::ProfileSet::none(),
    };
    // The RETIRED flag's one-release warning (the `RETIRED_PROFILE_FLAG` idiom
    // `vike-datahub`'s `datahub_cli` carries for its own renamed flag) — said whenever the argument
    // was given at all, regardless of whether an active row exists, because the value is never
    // consulted either way.
    if let Some(given) = &args.config_path {
        tracing::warn!(
            "--config {given} is RETIRED (0086) and IGNORED — the daemon profile comes from the \
             ACTIVE daemon-profile row, never from a file. Drop this argument from the unit's \
             ExecStart= line; a future release refuses it outright."
        );
    }
    let active_daemon = store_profiles
        .active(vike_secrets::profile_store::ProfileKind::Daemon)
        .map(|p| p.row.name.clone());
    let profile = match active_daemon.as_deref().and_then(|n| store_profiles.by_name(n)) {
        // The row won. Its body goes back through `ProfileDaemon::from_toml_str` — the EXISTING
        // parser and every refusal it carries — rather than through a second validator, which is
        // 0057's *What is LOST* requirement stated in its own words.
        Some(stored) => {
            tracing::info!("daemon profile: `{}` (the active row)", stored.row.name);
            match crate::profile_rows::rows_to_daemon_profile(stored) {
                Ok(p) => p,
                Err(e) => {
                    let why = format!("bad profile row `{}`: {e}", stored.row.name);
                    eprintln!("vike-tradehub: {why}");
                    return Err(ProfileRefusal::Refused(why));
                }
            }
        }
        // ⚠ NO FILE FALLBACK, by 0086 verdict 1 — a profile TOML is never read by this binary,
        // however it was invoked. This is a NARROWING of a promise this crate's spawn tests used to
        // prove for the CREDENTIAL half of this same store: an unreadable store no longer leaves
        // this daemon running on paper, because the mount config lives in the same store now and
        // there is no second rung left to read it from. That promise still holds for credentials
        // alone (an unreadable store still yields an EMPTY credential map, never a refusal, by the
        // arm below) — it is the PROFILE side that changed.
        None => {
            // The store EXISTS and will not open — `profile create daemon` cannot cure this; it writes
            // through the very open this daemon just failed. Name the credential store's own repair
            // instead, since it is the same file.
            if let Some(e) = &profile_store_unreadable {
                let msg = format!(
                    "the settings store exists but its profile rows could not be read ({e}), so \
                     this binary cannot learn what to mount. THE DAEMON CANNOT REPAIR THIS ITSELF \
                     — it opens the store read-only, and a read-only open may not replay a \
                     journal, so a restart meets the identical state. This is the SAME \
                     store credentials live in — repair it from an OPERATOR SHELL, where the \
                     directory is writable: run any `vike-cli secrets` command (`vike-cli secrets \
                     ls` is enough — opening the store read-write is what replays a rollback \
                     journal a killed writer left behind), then restart. `vike-cli config \
                     profile create daemon` writes a NEW row through this same store and cannot repair \
                     an unopenable one."
                );
                eprintln!("vike-tradehub: {msg}");
                return Err(ProfileRefusal::Refused(msg));
            }
            // No store, or a store with profile tables and no active row — an ordinary unconfigured
            // box, cured by writing the first row. Setup alone cures it, so the operator-needed status
            // (the units and the server install's supervisor park on it instead of restarting every
            // 5 s into the same refusal, decision 0260) and ONE line naming the command.
            let why = "vike-tradehub: setup needed: run `vike-cli config profile create daemon <name> --venue <venue> \
                 --asset-class <class> --symbol <sym> --strategy <path under strategies/>` (or \
                 --token-id on polymarket; it also activates \
                 it; any name but `default`, which a server install's datahub records under, and profile \
                 names are one namespace), then start this daemon again — no ACTIVE daemon-profile row in the settings \
                 store, and this binary reads no profile file any more (0086)";
            // Not logged here: a start over a settings database runs EMPTY instead (`super::empty_node`), and
            // a start with none says this cause in its own refusal (`super::refused::refused_start`).
            return Err(ProfileRefusal::SetupNeeded(why.to_string()));
        }
    };
    Ok((store_profiles, active_daemon, profile))
}

/// Why [`load_daemon_profile`] found no profile to mount. A start parks on either (exit 78, `super::refused`), except
/// `SetupNeeded` over a settings database that exists, which starts the EMPTY node (`super::empty_node`);
/// `--check-profile` keeps its own statuses (78 no active row, 1 refused), which `vike-cli config check` reads.
pub(super) enum ProfileRefusal {
    /// No ACTIVE daemon-profile row: setup alone cures it.
    SetupNeeded(String),
    /// A row that does not load, or a store whose profile rows cannot be read.
    Refused(String),
}

impl ProfileRefusal {
    /// The words, as the operator was told them.
    pub(super) fn why(&self) -> &str {
        match self {
            ProfileRefusal::SetupNeeded(why) | ProfileRefusal::Refused(why) => why,
        }
    }
}

/// **`--check-profile`** — [`load_daemon_profile`] over the boot this process just ran, and its
/// verdict as the exit code: the ACTIVE row through the SAME read and the SAME loader as a start,
/// with nothing mounted and nothing written (the caller stops before the log subscriber). A refusal is
/// already on stderr in the daemon's own words; a profile that loads says so on stdout.
///
/// Both halves of the one live profile are judged (decision 0297): the daemon part here and the run
/// part through [`load_run_part`]. An ACTIVE row of the retired `run` kind is NOT judged here: the
/// start refuses it ([`resolve_run_profile`]), and `vike-cli config check` WARNS with the fold line,
/// because it runs before every daemon and a failure would hold the hubs too.
pub(super) fn check_profile(booted: &vike_boot::Booted, args: &Args) -> ExitCode {
    match load_daemon_profile(booted, args) {
        Ok((store_profiles, name, _)) => {
            let run = match load_run_part(&store_profiles) {
                Ok(Some(_)) => "with its run part",
                Ok(None) => "(it carries no run part)",
                Err(_) => return ExitCode::FAILURE,
            };
            println!(
                "vike-tradehub: the ACTIVE daemon profile `{}` loads {run}",
                name.unwrap_or_default()
            );
            ExitCode::SUCCESS
        }
        Err(ProfileRefusal::SetupNeeded(_)) => {
            ExitCode::from(vike_utility::cli::OPERATOR_NEEDED_EXIT)
        }
        Err(ProfileRefusal::Refused(_)) => ExitCode::FAILURE,
    }
}

/// **Startup phase — per-mount RESOLUTION** (split-plane I10): every row of the profile is lowered
/// to its strategy-free spec and its strategy FILE resolved, exactly as a single-mount profile always
/// was. `multi` is `!profile.mounts.is_empty()`, decided by [`run`] because it is also the switch the
/// later phases read; `strategies` is the project's strategies folder every row's file is read from
/// (`crate::mount_factory::project_strategies_dir`, `None` on a process with no project, which
/// refuses); `live` is the live gate (`flags.tradehub_live`), under which a file whose header does not
/// say `runs_on = "live"` is refused. An `Err` is the refusal, on stderr, logged once by `super::refused`.
pub(super) fn resolve_mounts(
    profile: &crate::config::ProfileDaemon,
    multi: bool,
    strategies: Option<&std::path::Path>,
    live: bool,
) -> Result<Vec<MountResolved>, String> {
    let refuse = |at: String, e: String| {
        tracing::error!("{at}{e}");
        eprintln!("vike-tradehub: {at}{e}");
        format!("{at}{e}")
    };
    let Some(strategies) = strategies else {
        let why = crate::mount_factory::strategies_dir_or_refusal().err().unwrap_or_default();
        return Err(refuse(String::new(), why));
    };
    let mut resolved: Vec<MountResolved> = Vec::new();
    for (i, row) in profile.mount_rows().into_iter().enumerate() {
        let at = if multi { format!("mounts[{i}]: ") } else { String::new() };
        // The strategy-free mount (venue / symbol / interval / seed_cash / tick_size / the paper fee
        // scalars) — what BOTH generic mount builders take. ONE derivation, so every mount agrees
        // about the mount identity and the fee model.
        let mut spec = row.to_mount_spec();
        // A `[[mounts]]` row mounts under its DERIVED controller id (venue/symbol/interval +
        // strategy identity — `ProfileDaemon::derived_controller_id`, duplicates already refused
        // at load). A single-mount profile keeps `None` — the runtime's legacy triple derivation,
        // so an existing deployment's state sidecar / journal attribution keys are untouched.
        if multi {
            spec.controller_id = Some(row.derived_controller_id());
        }
        // The mount's STRATEGY CODE (decision 0270): the store's row for the mount id the core keys this mount by and
        // the strategy's name (its file's stem), insert-or-get, so a restart mints the code it minted before.
        let mount_id = vike_core::strategy_state::mount_id_with(
            spec.controller_id.as_deref(),
            &spec.venue,
            &spec.symbol,
            &spec.interval,
        );
        spec.order_client_id_code = crate::mount_factory::mount_order_client_id_code(
            super::settings::booted_settings_dir(),
            &mount_id,
            row.strategy_name(),
        )
        .map_err(|e| refuse(String::new(), e))?;
        // WHICH strategy: the row's FILE, read, hashed, its header and params judged, then compiled
        // here (`.rhai`), or, for a `.rs` file, NOT built: the start never waits for a build (the owner's
        // ruling of 2026-11-14), so the mount is deactivated `building <file>` and its build runs once the node
        // is up (`super::awaited_builds`) — `ProfileDaemon::resolve_mount_awaiting_build`.
        // `ProfileDaemon::validate` already refused a row naming no file at LOAD.
        let (mounted, building) = row
            .resolve_mount_awaiting_build(strategies, live, spec.controller_id.as_deref())
            .map_err(|e| refuse(at.clone(), format!("strategy resolve failed: {e}")))?;
        let strategy_params = match &building {
            Some(build) => crate::config::building_words(build.file()),
            None => mounted.effective_params().to_string(),
        };
        let remote = mounted.remote();
        // The file and the sha256 of the bytes read, which the core journals as the mount opens.
        spec.strategy_file = Some(mounted.strategy_file());
        // …and when its bar orders fill on a paper engine (`fill = "close"`: at that bar's close).
        spec.fill_at_close = mounted.fills_at_close();
        let strategy = mounted.into_strategy();
        // The symbols the strategy trades beside the mount's own become the mount's LEGS on its venue
        // (one broker plan, step 12): the node admits them, their bars reach the strategy, and an order
        // naming one is routed there while every other symbol is refused. A strategy whose declaration
        // panics declares nothing and trades its own symbol alone (the `inputs` read below is guarded
        // the same way).
        let symbols = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| strategy.symbols()))
            .unwrap_or_else(|_| {
                tracing::error!(
                    "the mounted strategy's symbol declaration panicked: it trades its own symbol alone"
                );
                Vec::new()
            });
        spec.declare_strategy_legs(symbols).map_err(|e| refuse(at.clone(), e))?;
        resolved.push(MountResolved { row, spec, strategy, strategy_params, remote, building });
    }
    // An `[inputs]` row no mounted strategy reads would remap nothing (decision 0271): refused, as a
    // backtest refuses one at its run (`vike_backtest::run::data_plan::resolve_inputs`). The strategy's own
    // call is guarded as the core guards it: a panic declares nothing here and refuses the mount there.
    // ⚠ Not judged while a `.rs` mount waits for its build: what it reads is known only once it opens.
    if resolved.iter().any(|m| m.building.is_some()) {
        return Ok(resolved);
    }
    let declared: Vec<String> = resolved
        .iter()
        .flat_map(|m| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| m.strategy.inputs()))
                .unwrap_or_default()
        })
        .collect();
    if let Some(key) = profile.inputs.keys().find(|k| !declared.contains(k)) {
        let e = format!(
            "[inputs] {key:?} names no input a mounted strategy reads, so it would remap nothing \
             (they read {declared:?}). An [inputs] row is keyed by the name the strategy's code writes"
        );
        eprintln!("vike-tradehub: {e}");
        return Err(e);
    }
    Ok(resolved)
}

/// **Startup phase — the PRIMARY mount's identity**: the config of the declared primary row, the
/// strategy name to echo (`+`-joined across a `[[mounts]]` profile) and the params line to echo.
/// Says which row is primary and whether anybody chose it.
pub(super) fn primary_mount_identity(
    profile: &crate::config::ProfileDaemon,
    resolved: &[MountResolved],
    multi: bool,
) -> (MountPlacement, String, String) {
    // The PRIMARY mount — the daemon's historical singular identity (summary token, mode line,
    // seed policy). On a single-mount profile this IS the mount, byte-identically.
    //
    // ⚠ THIS WAS `resolved[0]`, AND THE INDEX WAS THE WHOLE DECLARATION. 0057's `tradehub.toml`
    // verdict is that the primary must become EXPLICIT before mounts become rows, because a TABLE
    // HAS NO INHERENT ORDER and reproducing "the first one" from rows would carry an accident
    // forward as a requirement. `ProfileDaemon::primary_mount` is the one resolution; a profile
    // that declares nothing still answers index 0, so every profile that has ever shipped mounts
    // byte-identically to before this line changed.
    let primary = profile.primary_mount();
    let spec = resolved[primary.index()].spec.clone();
    // Say WHICH row it is and whether anybody chose it. A declared primary that nobody can see is
    // the same defect as an undeclared one, and the `word()` half is what tells an operator reading
    // a startup log that the daemon picked row 0 because nothing said otherwise.
    tracing::info!(
        index = primary.index(),
        how = primary.word(),
        venue = %spec.venue,
        symbol = %spec.symbol,
        "the PRIMARY mount — the daemon's singular identity (summary token, mode line, seed policy)"
    );
    let strategy_name = if multi {
        resolved.iter().map(|m| m.row.strategy_name()).collect::<Vec<_>>().join("+")
    } else {
        profile.strategy_name().to_string()
    };
    // ⚠ ECHO WHAT WAS ACTUALLY MOUNTED, not just its name. Logging `strategy = <name>` alone left an
    // operator unable to tell — at startup or afterwards from the log — which numbers the strategy is
    // running, which is half of what made a silently-dropped params key invisible. The resolve makes
    // such a key impossible; this makes the resolved configuration READABLE
    // (`crate::config::MountResolvedStrategy::effective_params`). A `[[mounts]]` profile echoes ONE
    // self-addressed line per row (`mounts_wire_row` — the same rendering the StrategyStatus wire
    // rows carry), joined; a single-mount profile keeps the one-line echo.
    let strategy_params = if multi {
        resolved.iter().map(mounts_wire_params).collect::<Vec<_>>().join(" | ")
    } else {
        resolved[primary.index()].strategy_params.clone()
    };
    (spec, strategy_name, strategy_params)
}

/// **Startup phase — the OPERATOR risk-budget ProfileRun: the RUN part of the ACTIVE daemon row,
/// and nothing else** (decision 0111: no `--profile`, no `VIKE_RUN_PROFILE`, no file; decision
/// 0297: one live profile). `Ok(None)` — a body with no run part — leaves the PAPER mount on
/// `RiskLimits::new()` and makes a LIVE mount REFUSE (`vike_tradehub_core::EngineError::MissingRiskBudget`).
/// `journal_rung` is the WAL's rows, resolved ONCE by [`run`] because the paper and live mounts
/// read them too. An `Err` is the refusal, on stderr, logged once by `super::refused`. A start has passed
/// [`refuse_an_active_run_row`] before it gets here.
pub(super) fn resolve_run_profile(
    store_profiles: &vike_secrets::profile_store::ProfileSet,
    journal_rung: &crate::profile_rows::JournalRung,
) -> Result<Option<vike_config::ProfileRun>, String> {
    let run_profile = load_run_part(store_profiles)?;
    // ⚠ A `[sinks.journal]` with no `config.journal_dir` row TUNES NOTHING (decision 0111, amended
    // 2026-10-11: the row alone turns the journal on and names its directory), and an operator who
    // wrote the tuning may believe it armed the journal — so it is said, ONCE, here at the
    // resolution point every consumer of `journal_config_for` shares. Silent on every other box.
    if journal_rung.dir.is_none() && run_profile.as_ref().is_some_and(|p| p.sinks.journal.is_some())
    {
        tracing::warn!(
            "the write-ahead command journal is OFF: no `config.journal_dir` row names its \
             directory, so the active run profile's [sinks.journal] tuning applies to nothing. \
             `vike-cli config set config.journal_dir <dir>` turns the journal on."
        );
    }
    Ok(run_profile)
}

/// **Startup phase — THE FENCE (decision 0297): an ACTIVE row of the retired `run` kind refuses the
/// start**, right after the daemon profile loads and before a mount is resolved (which writes a
/// strategy code), naming the fold line rendered from that row's own settings. Ignoring the row
/// would silently drop every ceiling it carries that does not refuse on absence (`max_leverage`,
/// `max_orders_per_window`, `guards.max_drawdown`, `[guards.margin_call]`) and start a `halted` body
/// ACTIVE. An `Err` is the refusal, on stderr, logged once by `super::refused`.
pub(super) fn refuse_an_active_run_row(
    store_profiles: &vike_secrets::profile_store::ProfileSet,
) -> Result<(), String> {
    let Some(retired) = store_profiles.active(vike_secrets::profile_store::ProfileKind::Run) else {
        return Ok(());
    };
    let daemon = store_profiles
        .active(vike_secrets::profile_store::ProfileKind::Daemon)
        .map_or("<the active daemon profile>", |p| p.row.name.as_str());
    let why = format!(
        "the retired `run` profile `{}` is still ACTIVE (decision 0297: its ceilings are the run \
         part of the daemon profile now), and this daemon does not start while it is. Fold it into \
         the active daemon profile, then deselect it:\n\n    {}\n    vike-cli config profile deactivate \
         run\n\nthen start this daemon again.",
        retired.row.name,
        vike_config::fold_command(retired, daemon)
    );
    eprintln!("vike-tradehub: {why}");
    Err(why)
}

/// **The RUN part of the ACTIVE daemon row** (`risk.*`, `guards.*`, `sinks.*`; decision 0297),
/// through `vike_config::rows_to_run_profile`, the one reader — `Ok(None)` when the body carries
/// none. A start ([`resolve_run_profile`]) and `--check-profile` ([`check_profile`]) both read it
/// here. An `Err` is the refusal, on stderr, logged once by `super::refused`.
fn load_run_part(
    store_profiles: &vike_secrets::profile_store::ProfileSet,
) -> Result<Option<vike_config::ProfileRun>, String> {
    let Some(stored) = store_profiles
        .active(vike_secrets::profile_store::ProfileKind::Daemon)
        .filter(|p| p.settings.keys().any(|path| vike_secrets::profile_store::is_run_part(path)))
    else {
        tracing::info!(
            "run profile: the active daemon profile carries no run part ([risk], [guards], \
             [sinks]). The paper mount runs without an operator [risk] budget and a live mount \
             refuses to start; `vike-cli config profile create run <daemon profile>` writes one."
        );
        return Ok(None);
    };
    // ⚠ An `Err` here is a HARD startup failure. Reading a corrupt row as "no run profile" would
    // make it indistinguishable from an absent one — and an absent one is what a live mount REFUSES
    // on, while a paper mount would run with no operator budget at all and the operator believing
    // the row is in force.
    match vike_config::rows_to_run_profile(stored) {
        Ok(p) => {
            tracing::info!("run profile: the run part of `{}` (the active row)", stored.row.name);
            Ok(Some(p))
        }
        Err(e) => {
            let why = format!("bad run part of profile row `{}`: {e}", stored.row.name);
            eprintln!("vike-tradehub: {why}");
            Err(why)
        }
    }
}

/// **Startup phase — the PAPER mount's operator risk budget**, from the resolved run profile
/// (`RiskLimits::new()` absent one). An `Err` is the refusal, on stderr, logged once by `super::refused`.
pub(super) fn paper_risk_budget(
    run_profile: &Option<vike_config::ProfileRun>,
) -> Result<vike_model::RiskLimits, String> {
    // ⚠ This used to be a blanket `if p.guards != Guards::default() { warn!("[guards] … is set but
    // NOT consumed …") }`. It is gone because the statement is no longer true: the LIVE arm applies
    // `[guards]` and `[sinks]` to its `CoreConfig` through
    // `vike_tradehub_core::apply_guards_and_sinks`, and discloses — BY KEY — only the two
    // guards and three sinks that genuinely still reach nothing. A blanket warning over a section
    // that is now mostly wired would be the mirror image of the original defect: an operator told
    // their armed guard was ignored.
    //
    // The PAPER arm still consumes `[risk]` alone; its `CoreConfig` is built elsewhere and wiring
    // it is a separate change, so it is not claimed here.
    // The PAPER mount's operator risk budget — `RiskLimits::new()` (byte-identical to the
    // pre-Task-2 daemon) absent a profile, else the profile's `[risk]` table applied via
    // `ProfileRun::apply_risk` at the PAPER tier (see `resolve_paper_risk_limits`'s doc: the
    // paper mount is the paper simulator, so no grid is fetched). The LIVE arm ALSO consumes this
    // same `run_profile`'s `[risk]` and threads it straight into `vike_tradehub_core::make_engine` for every
    // wired market, which judges it per mounted account's tier (decision 0156 item 7).
    let paper_risk_limits = match resolve_paper_risk_limits(run_profile.as_ref()) {
        Ok(r) => r,
        Err(e) => {
            let why = format!("run profile risk budget error: {e}");
            eprintln!("vike-tradehub: {why}");
            return Err(why);
        }
    };
    if run_profile.is_some() {
        tracing::info!(
            max_notional_per_order = ?paper_risk_limits.max_notional_per_order,
            max_total_exposure = ?paper_risk_limits.max_total_exposure,
            max_orders_per_window = ?paper_risk_limits.max_orders_per_window,
            max_leverage = ?paper_risk_limits.max_leverage,
            // The ENFORCED form of `max_leverage` (issue #822) — logged alongside the declared
            // cap so an operator can see the buying-power check actually armed, not just the
            // number they typed.
            initial_margin_requirement = ?paper_risk_limits.initial_margin_requirement,
            "ProfileRun loaded — operator risk budget armed on the PAPER mount"
        );
    }
    Ok(paper_risk_limits)
}
