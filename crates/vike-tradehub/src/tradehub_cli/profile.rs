//! Loading the daemon-profile row, resolving its mounts, the run profile and the paper risk budget.

use std::collections::HashMap;
use std::path::Path;
use std::process::ExitCode;

use crate::ResolvedMount;
use crate::config::resolve_paper_risk_limits;
use vike_mount::MakerMountConfig;

use super::args::Args;
use super::mount_rows::mounts_wire_params;
use super::process_env;

/// **Startup phase — WHICH DAEMON PROFILE is live.** The settings store's ACTIVE daemon-profile row
/// is the only source since decision 0086: this reads the store's profile rows, warns about the
/// RETIRED `--config` argument, and lowers the winning row through `DaemonProfile::from_toml_str`.
///
/// Returns the whole [`vike_secrets::profile_store::Profiles`] snapshot (the run-profile phase,
/// [`resolve_run_profile`], reads the SAME one rather than opening the store a second time), the
/// active daemon row's NAME, and the lowered profile. An `Err` carries the exit code [`run`]
/// returns; the refusal itself (the `tracing::error!` and the `eprintln!` an operator sees) is
/// printed here, exactly as when this body was inline.
pub(super) fn load_daemon_profile(
    booted: &vike_boot::Booted,
    args: &Args,
) -> Result<
    (vike_secrets::profile_store::Profiles, Option<String>, crate::config::DaemonProfile),
    ExitCode,
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
    // only one of them has a cure this binary can name. `bootstrap-daemon` writes a NEW row; it
    // cannot repair a store that will not open, and telling an operator to run it for THAT fault
    // would send them to the wrong tool.
    let mut profile_store_unreadable: Option<String> = None;
    let store_profiles = match booted.settings_dir.as_deref() {
        Some(dir) => {
            match vike_secrets::profile_store::read_profiles(&vike_secrets::db_path_in(dir)) {
                Ok(p) => p,
                Err(e) => {
                    tracing::error!("the settings store's profile rows could not be read: {e}");
                    profile_store_unreadable = Some(e.to_string());
                    vike_secrets::profile_store::Profiles::none()
                }
            }
        }
        None => vike_secrets::profile_store::Profiles::none(),
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
        // The row won. Its body goes back through `DaemonProfile::from_toml_str` — the EXISTING
        // parser and every refusal it carries — rather than through a second validator, which is
        // 0057's *What is LOST* requirement stated in its own words.
        Some(stored) => {
            tracing::info!("daemon profile: `{}` (the active row)", stored.row.name);
            match crate::profile_rows::rows_to_daemon_profile(stored) {
                Ok(p) => p,
                Err(e) => {
                    tracing::error!("bad profile row `{}`: {e}", stored.row.name);
                    eprintln!("vike-tradehub: bad profile row `{}`: {e}", stored.row.name);
                    return Err(ExitCode::FAILURE);
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
            let msg = match &profile_store_unreadable {
                // The store EXISTS and will not open — `bootstrap-daemon` cannot cure this; it
                // writes through the very open this daemon just failed. Name the credential store's
                // own repair instead, since it is the same file.
                Some(e) => format!(
                    "the settings store exists but its profile rows could not be read ({e}), so \
                     this binary cannot learn what to mount. THE DAEMON CANNOT REPAIR THIS ITSELF \
                     — it opens the store read-only, and a read-only open may not replay a \
                     journal, so a restart meets the identical state. This is the SAME \
                     store credentials live in — repair it from an OPERATOR SHELL, where the \
                     directory is writable: run any `vike-cli secrets` command (`vike-cli secrets \
                     list` is enough — opening the store read-write is what replays a rollback \
                     journal a killed writer left behind), then restart. `vike-cli config \
                     bootstrap-daemon` writes a NEW row through this same store and cannot repair \
                     an unopenable one."
                ),
                // No store, or a store with profile tables and no active row — an ordinary
                // unconfigured box, cured by writing the first row.
                None => {
                    "no ACTIVE daemon-profile row in the settings store, and this binary reads \
                          no profile file any more (0086). Create one with `vike-cli config \
                          bootstrap-daemon <name> --venue <venue> --asset-class <class> --symbol \
                          <sym>` (or --token-id on polymarket) — it also activates it — then \
                          restart."
                        .to_string()
                }
            };
            tracing::error!("{msg}");
            eprintln!("vike-tradehub: {msg}");
            return Err(ExitCode::FAILURE);
        }
    };
    Ok((store_profiles, active_daemon, profile))
}

/// **Startup phase — per-mount RESOLUTION** (split-plane I10): every row of the profile is lowered
/// to its A-S config, its strategy-free spec and its strategy, exactly as a single-mount profile
/// always was. `multi` is `!profile.mounts.is_empty()`, decided by [`run`] because it is also the
/// switch the later phases read. An `Err` carries the exit code [`run`] returns.
pub(super) fn resolve_mounts(
    profile: &crate::config::DaemonProfile,
    multi: bool,
) -> Result<Vec<ResolvedMount>, ExitCode> {
    let mut resolved: Vec<ResolvedMount> = Vec::new();
    for (i, row) in profile.mount_rows().into_iter().enumerate() {
        let cfg = row.to_mount_config();
        // The strategy-free projection of that lowering (venue / symbol / interval / seed_cash /
        // the paper fee scalars) — what BOTH generic mount builders take. ONE derivation, so a
        // `[strategy]` mount and the default A-S mount cannot disagree about the mount identity or
        // the fee model.
        let mut spec = row.to_mount_spec();
        // A `[[mounts]]` row mounts under its DERIVED controller id (venue/symbol/interval +
        // strategy identity — `DaemonProfile::derived_controller_id`, duplicates already refused
        // at load). A single-mount profile keeps `None` — the runtime's legacy triple derivation,
        // so an existing deployment's state sidecar / journal attribution keys are untouched.
        if multi {
            spec.controller_id = Some(row.derived_controller_id());
        }
        // WHICH strategy. The A-S maker — absent `[strategy]` OR named
        // `spread_maker`/`gueant_maker` — comes from `DaemonProfile::mounted_maker`, the ONE
        // construction site, which is exactly `vike_mount::build_maker(&cfg)`: the very function
        // `build_paper_maker_core_with` / `build_live_maker_core` call. So the default path is the
        // historical A-S path with the `Box::new` moved one frame outward, and the NAMED path is
        // provably the same maker rather than the registry's `SpreadMaker::from_params`, which
        // reads `[strategy.params]` alone and would mount a materially different maker (see
        // `config::AS_MAKER_NAMES`). Every other name resolves through the shared `vike_strategy`
        // registry, the one a backtest profile resolves through.
        //
        // `DaemonProfile::validate` already rejected every unmountable name at LOAD (unknown /
        // simulator-only / resolves-but-cannot-trade) AND every params key the named strategy does
        // not read, so an error here means the two disagreed — worth failing loudly rather than
        // unwrapping.
        let strategy = match row.resolve_strategy(&cfg) {
            Ok(s) => s,
            Err(e) => {
                let at = if multi { format!("mounts[{i}]: ") } else { String::new() };
                tracing::error!("{at}strategy resolve failed: {e}");
                eprintln!("vike-tradehub: {at}strategy resolve failed: {e}");
                return Err(ExitCode::FAILURE);
            }
        };
        resolved.push(ResolvedMount { row, cfg, spec, strategy });
    }
    Ok(resolved)
}

/// **Startup phase — the PRIMARY mount's identity**: the config of the declared primary row, the
/// strategy name to echo (`+`-joined across a `[[mounts]]` profile) and the params line to echo.
/// Says which row is primary and whether anybody chose it.
pub(super) fn primary_mount_identity(
    profile: &crate::config::DaemonProfile,
    resolved: &[ResolvedMount],
    multi: bool,
) -> (MakerMountConfig, String, String) {
    // The PRIMARY mount — the daemon's historical singular identity (summary token, mode line,
    // seed policy). On a single-mount profile this IS the mount, byte-identically.
    //
    // ⚠ THIS WAS `resolved[0]`, AND THE INDEX WAS THE WHOLE DECLARATION. 0057's `tradehub.toml`
    // verdict is that the primary must become EXPLICIT before mounts become rows, because a TABLE
    // HAS NO INHERENT ORDER and reproducing "the first one" from rows would carry an accident
    // forward as a requirement. `DaemonProfile::primary_mount` is the one resolution; a profile
    // that declares nothing still answers index 0, so every profile that has ever shipped mounts
    // byte-identically to before this line changed.
    let primary = profile.primary_mount();
    let cfg = resolved[primary.index()].cfg.clone();
    // Say WHICH row it is and whether anybody chose it. A declared primary that nobody can see is
    // the same defect as an undeclared one, and the `word()` half is what tells an operator reading
    // a startup log that the daemon picked row 0 because nothing said otherwise.
    tracing::info!(
        index = primary.index(),
        how = primary.word(),
        venue = %resolved[primary.index()].cfg.venue,
        symbol = %resolved[primary.index()].cfg.token_id,
        "the PRIMARY mount — the daemon's singular identity (summary token, mode line, seed policy)"
    );
    let strategy_name = if multi {
        resolved.iter().map(|m| m.row.strategy_name()).collect::<Vec<_>>().join("+")
    } else {
        profile.strategy_name().to_string()
    };
    // ⚠ ECHO WHAT WAS ACTUALLY MOUNTED, not just its name. Logging `strategy = <name>` alone left an
    // operator unable to tell — at startup or afterwards from the log — which numbers the strategy is
    // running, which is half of what made a silently-dropped params key invisible. `validate` makes
    // such a key impossible; this makes the resolved configuration READABLE. For the A-S maker it
    // reports knobs the profile never states (the venue-selected price domain / variance mode),
    // because those are the ones that decide whether it quotes at all. A `[[mounts]]` profile
    // echoes ONE self-addressed line per row (`mounts_wire_row` — the same rendering the
    // StrategyStatus wire rows carry), joined; a single-mount profile keeps the historical
    // one-line echo byte-identically.
    let strategy_params = if multi {
        resolved.iter().map(mounts_wire_params).collect::<Vec<_>>().join(" | ")
    } else {
        profile.effective_params(&cfg)
    };
    (cfg, strategy_name, strategy_params)
}

/// **Startup phase — the OPERATOR risk-budget RunProfile.** The active run-profile ROW if there is
/// one, else `--profile` / `VIKE_RUN_PROFILE`; `Ok(None)` is the untouched default. `journal_vars`
/// is [`journal_vars`]'s map, resolved ONCE by [`run`] because the paper and live mounts read it
/// too. An `Err` carries the exit code [`run`] returns.
pub(super) fn resolve_run_profile(
    store_profiles: &vike_secrets::profile_store::Profiles,
    args: &Args,
    journal_vars: &HashMap<String, String>,
) -> Result<Option<vike_core::RunProfile>, ExitCode> {
    // The OPERATOR risk-budget RunProfile (RunProfile wiring, Settings STEP 2 PR 1, Task 2) —
    // INDEPENDENT of `profile` (the `DaemonProfile` above, which owns the venue/token_id/A-S mount
    // shape): `--profile`/`VIKE_RUN_PROFILE` names a `vike_core::RunProfile` this daemon consumes
    // ONLY for its `[risk]` table (`run_profile.mode` is ignored here — the
    // `DaemonProfile` above already fully owns that shape). Resolved from the REAL process env (the
    // `VIKE_RECONCILE` idiom), never the `.env` creds map. `Ok(None)` (no explicit path AND no env
    // var) is the untouched default; a resolved-but-broken profile is a loud startup failure, never
    // a silent fall-through to the hardcoded risk defaults.
    let run_profile_vars: HashMap<String, String> = process_env().clone();
    // ⚠ **THE RUN PROFILE'S ROW RUNG — and this block was a DECLARED PARTIAL until it landed.**
    // It used to warn that *"this binary does not read run-profile bodies from rows yet (0057
    // Phase 2)"* and then resolve from `--profile`/`VIKE_RUN_PROFILE` regardless, with `None`
    // hardcoded where the row goes. It now takes the exact shape of the DAEMON-profile rung five
    // screens above: the owner's ruling (`vike_secrets::profile_store::select`) picks the winner,
    // the disclosure names everything it shadowed, and a winning row's BODY goes back through
    // `RunProfile::from_toml_str` — the EXISTING parser and every refusal it carries — rather than
    // through a second validator.
    let active_run = store_profiles
        .active(vike_secrets::profile_store::ProfileKind::Run)
        .map(|p| p.row.name.clone());
    let run_selection = crate::profile_rows::select_run_profile(
        active_run.as_deref(),
        args.profile_path.as_deref(),
        &run_profile_vars,
    );
    if run_selection.row_shadowed_something() {
        tracing::warn!("{}", crate::profile_rows::selection_line("run profile", &run_selection));
    } else {
        tracing::info!("{}", crate::profile_rows::selection_line("run profile", &run_selection));
    }
    let run_profile = match active_run.as_deref().and_then(|n| store_profiles.by_name(n)) {
        // The row won.
        //
        // ⚠ An `Err` here is a HARD startup failure and deliberately does NOT fall through to the
        // file rung. Falling through would make a corrupt row indistinguishable from an absent one
        // — and an absent one is what a live mount REFUSES on, so the fall-through would quietly
        // convert a refusal into a mount judging orders against the file's ceilings while the
        // operator believes the row is in force. Same disposition as the daemon-profile rung above.
        Some(stored) => match crate::profile_rows::rows_to_run_profile(stored) {
            Ok(p) => Some(p),
            Err(e) => {
                tracing::error!("bad run profile row `{}`: {e}", stored.row.name);
                eprintln!("vike-tradehub: bad run profile row `{}`: {e}", stored.row.name);
                return Err(ExitCode::FAILURE);
            }
        },
        // No row — byte-identical to every box that has not crossed, which is every box and every
        // CI lane.
        None => match vike_core::resolve_profile(
            args.profile_path.as_deref().map(Path::new),
            &run_profile_vars,
        ) {
            Ok(p) => p,
            Err(e) => {
                tracing::error!("bad run profile: {e}");
                eprintln!("vike-tradehub: bad run profile: {e}");
                return Err(ExitCode::FAILURE);
            }
        },
    };
    // ⚠ THE JOURNAL RUNG'S OWN DISCLOSURE, emitted ONCE here rather than at either of the two
    // `CoreConfig` sites that call `journal_config_for` — both take this same `run_profile` and
    // this same `journal_vars`, so one line at the resolution point says it for the paper mount
    // and the live one alike, and says it before either is built. Silent otherwise: a resolved
    // profile makes the `VIKE_JOURNAL_DIR` / `config.journal_dir` rung unreachable, and a box that
    // completes the migration (activate the row, drop the shadowed `VIKE_RUN_PROFILE=` line) lands
    // in exactly that state. `crate::profile_rows::journal_rung_shadowed` carries the argument and
    // answers `None` on every box that cannot be affected.
    if let Some(line) =
        crate::profile_rows::journal_rung_shadowed(run_profile.as_ref(), journal_vars)
    {
        tracing::warn!("{line}");
    }
    Ok(run_profile)
}

/// **Startup phase — the PAPER mount's operator risk budget**, from the resolved run profile
/// (`RiskLimits::new()` absent one). An `Err` carries the exit code [`run`] returns.
pub(super) fn paper_risk_budget(
    run_profile: &Option<vike_core::RunProfile>,
) -> Result<vike_exec::RiskLimits, ExitCode> {
    // ⚠ This used to be a blanket `if p.guards != Guards::default() { warn!("[guards] … is set but
    // NOT consumed …") }`. It is gone because the statement is no longer true: the LIVE arm applies
    // `[guards]` and `[sinks]` to its `CoreConfig` through
    // `vike_core::RunProfile::apply_guards_and_sinks`, and discloses — BY KEY — only the two
    // guards and three sinks that genuinely still reach nothing. A blanket warning over a section
    // that is now mostly wired would be the mirror image of the original defect: an operator told
    // their armed guard was ignored.
    //
    // The PAPER arm still consumes `[risk]` alone; its `CoreConfig` is built elsewhere and wiring
    // it is a separate change, so it is not claimed here.
    // The PAPER mount's operator risk budget — `RiskLimits::new()` (byte-identical to the
    // pre-Task-2 daemon) absent a profile, else the profile's `[risk]` table applied via
    // `RunProfile::apply_risk` (see `resolve_paper_risk_limits`'s doc: the `GridSource` it uses is
    // derived from the profile's own `mode`, not chosen at this call site). The LIVE arm below now
    // ALSO consumes this same `run_profile` (via `risk_for_live_venue_mount`, gated on
    // `mode == Mode::Live`) and threads it straight into `vike_mount::make_engine` for every wired market
    // — see that call site below for the mode guard this needed once it stopped being out of scope.
    let paper_risk_limits = match resolve_paper_risk_limits(run_profile.as_ref()) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("run profile risk budget error: {e}");
            eprintln!("vike-tradehub: run profile risk budget error: {e}");
            return Err(ExitCode::FAILURE);
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
            im_requirement = ?paper_risk_limits.im_requirement,
            "RunProfile loaded — operator risk budget armed on the PAPER mount"
        );
    }
    Ok(paper_risk_limits)
}
