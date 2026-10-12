//! The boot phases: the boot itself, the store root, the bind guard.

use std::process::ExitCode;

/// Every venue's `venue_setting` rows as the broker's client table reads them — Polymarket's socket
/// batching, each CEX feed's mark-stream row — taken from [`run`]'s ONE read of the table (`None`:
/// no settings directory). Empty for no directory, no database or no rows.
///
/// A store that will not open is logged HERE for what it costs the feed clients (every venue takes
/// its built-in defaults), and reads as empty. Polymarket's egress takes the same read through
/// `declare_polymarket_egress` (compiled only where Polymarket's egress is), whose shared policy
/// logs the egress half — so a build linking Polymarket says both, and one that does not still
/// says this one.
#[cfg(feature = "serve-datafusion")]
pub(super) fn venue_settings_of(
    loaded: Option<
        &Result<
            std::collections::BTreeMap<String, vike_secrets::venue_setting::VenueSettings>,
            vike_secrets::DbError,
        >,
    >,
) -> std::collections::BTreeMap<String, vike_secrets::venue_setting::VenueSettings> {
    match loaded {
        Some(Ok(all)) => all.clone(),
        Some(Err(e)) => {
            tracing::error!(
                error = %e,
                "the venue_setting table could not be read; every venue's market feed takes its \
                 built-in defaults (each venue's charter mark-stream default, the default socket \
                 batching)"
            );
            std::collections::BTreeMap::new()
        }
        None => std::collections::BTreeMap::new(),
    }
}

/// **Phase: the boot** — `vike_boot::boot` with this root's `BootSpec`, every arm of which names how
/// this daemon departs from the shared startup sequence and why. `run` calls it first, after argv
/// is parsed and before the log subscriber exists, which is why the loader's findings come back as
/// DATA for `run` to log.
#[cfg(feature = "serve-datafusion")]
pub(super) fn boot_root(
    vars: &std::collections::HashMap<String, String>,
    cwd: Option<&std::path::Path>,
) -> Result<vike_boot::Booted, String> {
    // The workspace's ONE startup sequence. This root uses the narrowest slice of it there is — the
    // identity line and the log home — and every way it departs is a named arm below, which is the
    // point: those departures used to be prose in this comment block and nothing could see them.
    vike_boot::boot(&vike_boot::BootSpec {
        env: vars,
        cwd,
        identity: vike_boot::Identity {
            name: env!("CARGO_PKG_NAME"),
            version: env!("CARGO_PKG_VERSION"),
        },
        // ⚠ REFUSE since decision 0095. The `Ignore` this replaced argued that the removed variables
        // were ceilings this server never read; REMOVED_ENV now also carries the Polymarket egress
        // variables it DID read, and ignoring a leftover `POLY_PROXY_ENABLED=false` would put it back
        // on the default proxy in silence — the change the spec's D3 refuses. A stale ceiling
        // variable now stops this server too; the release's deploy pre-flight names every such line
        // before anything is installed.
        removed_env: vike_boot::RemovedEnv::Refuse,
        // ⚠ **This was `SettingsLoad::Skip` until `docs/decisions/0066`, and the JOIN is that
        // record's decision 3 rather than a tidy-up.** The old reason ("this crate loads no
        // `vike_config::Settings` at all … It joins when it reads them") predicted its own end,
        // and `flags.venue_catalog_off` is what ended it: the venue-catalog lane is ON by default
        // now, so its REFUSAL is the only switch, and a refusal living in a file this process never
        // opened would be a default-on behaviour with NO off switch — precisely the state
        // `vike_config::Flags`' `*_off` pair exists to make impossible. The record rules the
        // ordering explicitly: the root joins the settings load, and the default flips with it or
        // after it, never before.
        //
        // What the join costs, stated rather than discovered: this daemon now reads
        // `<project>/settings/*.toml` and the mirrored settings-database rows, so a settings tree
        // that is present and BROKEN is a startup failure here as it already is at every other
        // root. That is the same disposition `vike-tradehub` and `vike-cli` have, and the loader's
        // own split (a store that will not OPEN degrades; a row that is ILLEGAL refuses) is
        // unchanged.
        //
        // Its store root and listen address are rows too (`config.store_root`,
        // `config.datahub_bind_addr`, decision 0111): no variable names either.
        // ⚠ REFUSE, the same arm as `vike-tradehub` and for the same reason one rung down: this
        // daemon's own `config.*` and `flags.*` decide what it records and where it writes, and a
        // recorder running on a resolution nothing can vouch for writes a tape nobody can trust.
        // Its deployed unit carries an `ExecStartPre=vike-cli config check`, so on that box the
        // refusal is met before the process starts rather than as a restart loop.
        settings: vike_boot::SettingsLoad::Load,
        // ⚠ The REASON here changed when `docs/decisions/0025-datahub-remote-posture.md` was
        // adopted; the ARM did not, and that is deliberate. It used to read "this server
        // authenticates nothing and mounts no venue", which is now false in its first clause — this
        // server CAN authenticate, and its node keys live in the credential store.
        //
        // It stays `Deferred` because the departure is about TIMING, not need: the keys are read a
        // few lines below, from `booted.settings_dir_override` — the answer THIS BOOT's one project
        // walk produced — rather than from a walk of this binary's own. That is the whole point of
        // the consolidation `vike_ops::settings`'s `VIKE_SETTINGS_DIR` block describes (four
        // composition-root reads became one under `vike-boot`), and taking `LoadWith` would mean
        // this root reading `$VIKE_SETTINGS_DIR` for itself to feed a loader — a fifth walk, and a
        // fifth chance for the log home, the store root and the credentials to answer with three
        // different projects.
        credentials: vike_boot::Credentials::Deferred(
            "this server mounts no venue and can place no order. It DOES read the credential store \
             — for its own node keys — but a few lines later, off this boot's own \
             `settings_dir_override`, so the project walk stays the one this crate performed \
             rather than a second one of the binary's.",
        ),
        log_home: vike_boot::LogHome::UnderSettings,
        // ⚠ The REASON here changed with the settings join above and the ARM did not, which is the
        // shape this file uses everywhere: it used to read "there are no settings to disclose",
        // and after `docs/decisions/0066` there is exactly one. `vike_config::boot_lines` performs
        // a SECOND read of the settings files to recover each row's ORIGIN and renders the RISK
        // CEILINGS — none of which this server reads, mounts a venue for, or can act on. What it
        // does consume discloses ITSELF, loudly, at the gate line below
        // (`vike_catalog::venue_catalog_gate_line`, which fires whichever way the verdict went), so
        // the one setting is in the log either way and a whole-tree disclosure would only add
        // ceilings this process cannot honour.
        disclosure: vike_boot::Disclosure::Skip(
            "the one setting this server consumes (`flags.venue_catalog_off`) discloses itself at \
             the venue-catalog gate line, whichever way the verdict went. A whole-tree disclosure \
             would describe the risk ceilings, which this server neither reads nor could act on.",
        ),
    })
}

/// **Phase: the hist-store root** — resolved through the shared precedence, and LOGGED with the rung
/// that answered before anything opens it. It owns the one `CARGO_MANIFEST_DIR` read in this file
/// (a debug-build rung only), so `crates/vike-ops/tests/hygiene/compile_time_path_gate.rs` keys its row on
/// this function. `run` calls this once, after the node keys and before the recording profile.
#[cfg(feature = "serve-datafusion")]
pub(super) fn resolve_hist_store(
    explicit: Option<std::path::PathBuf>,
    config: &vike_config::Config,
    vars: &std::collections::HashMap<String, String>,
    cwd: Option<&std::path::Path>,
) -> vike_model::paths::store_path::StoreRoot {
    // `"market_data/hist"` used to be the last resort here — a CWD-RELATIVE literal, so launching the
    // server from anywhere but the repo root silently created an empty store beside the shell and
    // answered every query with zero rows. Resolved through the shared precedence instead: this
    // run's `--store DIR`, then the `config.store_root` row (decision 0111: `VIKE_DATAHUB_STORE`
    // and `VIKE_HIST_STORE` refuse startup naming it), then the repo checkout if this machine has
    // one, then this PROJECT's own
    // `<project>/market_data/hist`, then the per-user `…/vike-data`.
    // The project rung matters most for exactly this binary: it runs from its project root under
    // systemd, where there is no checkout, and before that rung existed the store resolved under
    // the service user's `$HOME` — outside the directory the operator installed and backs up.
    // ⚠ ...and the repo checkout is a DEBUG-BUILD rung only. `env!("CARGO_MANIFEST_DIR")` is a
    // string literal in the binary, and `release.yml` builds the multicall that links it
    // (`vike-backend`; a standalone `vike-datahub` asset too, when this was measured) `--release`
    // on a runner whose checkout path names the runner
    // account — a path `--remap-path-prefix` cannot touch, because that flag rewrites what the
    // COMPILER emits and not what a crate embeds itself. The release's box-path guard
    // (`scripts/refuse_box_paths.sh`) refused both assets by name. A release build passes no rung
    // and resolves from the project walk down, which is the rung this binary takes under systemd
    // anyway; `vike_model::paths::store_path`'s module doc (rung 4) carries the argument, and the
    // attribute rather than `cfg!()` is what keeps the literal out of the bytes.
    #[cfg(debug_assertions)]
    let repo_default = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .map(|r| r.join("market_data").join("hist"));
    #[cfg(not(debug_assertions))]
    let repo_default: Option<std::path::PathBuf> = None;
    //
    // ⚠ `resolve_store_root_from`, never the bare `resolve_store_root` ladder: that one's project
    // and per-user defaults are two adjacent `Option<PathBuf>`s, so transposing them here would
    // COMPILE SILENTLY and put the tape somewhere else. This form has no two arguments of the same
    // type, and it resolves the project WALK (and `$VIKE_SETTINGS_DIR`, and the
    // `XDG_DATA_HOME`/`HOME`/`LOCALAPPDATA` trio) inside `vike_model` out of the environment map
    // this binary collects — the same shape as the `project_log_dir` call above.
    //
    // The row is the `configured` rung — stated for this box, so it outranks every default.
    let resolved = vike_model::paths::store_path::resolve_store_root_from(
        explicit,
        config.store_root.as_ref().map(|p| p.display().to_string()),
        repo_default.as_deref(),
        cwd,
        vars,
    );
    // Say WHICH root, and by which rung, BEFORE anything opens it. A store does not merge: if this
    // answer ever moves, the old store is simply no longer read and every query returns zero rows —
    // which looks exactly like an empty date range. This line is what makes that visible, and it
    // comes before the open so a failure to open is preceded by the path that failed.
    tracing::info!(
        store = %resolved.root.display(),
        rung = resolved.rung.as_str(),
        "hist store root resolved: {}",
        resolved.rung.why()
    );
    resolved
}

/// **Phase: the bind guard** — classifies the listen address BEFORE the store opens or the listener
/// binds, and `Err` carries the exit status (2: a refused configuration) for every combination it
/// refuses. `run` calls this after the dry run, which binds nothing and so is answered above it.
///
/// The non-loopback opt-in is the `flags.datahub_allow_public_bind` row; the variable that carried
/// it, `VIKE_DATAHUB_ALLOW_PUBLIC_BIND`, refuses startup naming that row (decision 0111).
#[cfg(feature = "serve-datafusion")]
pub(super) fn check_bind(
    flags: &vike_config::Flags,
    addr: &str,
    keys: &Option<vike_node_proto::auth::NodeKeys>,
    store_unreadable: bool,
) -> Result<(), ExitCode> {
    use std::net::ToSocketAddrs;

    // Classify the bind target BEFORE the store opens or the listener binds, and REFUSE a
    // non-loopback bind without the named opt-in — the tradehub treatment
    // (`crates/vike-tradehub/src/server.rs`'s `bind_decision`), mirrored into this crate's own
    // `server.rs`. It matters MORE here than there whenever this server is KEY-LESS — which is
    // still the default: that build authenticates NOTHING (the `Hello` informs, it does not gate),
    // so loopback plus the SSH tunnel is the ONLY barrier in front of history reads,
    // and — in a `backfill-serve` build — WRITES into the store plus the irreversible DeleteSeries, and
    // a key-less server is therefore refused a non-loopback bind even WITH the opt-in (see the ⚠
    // below). On a KEYED server the guard still applies, for a different reason: the handshake is
    // plaintext, so the tunnel is what supplies confidentiality and integrity either way. And the
    // address alone cannot hold the posture: before this guard one stray address exposed the
    // server with no warning.
    //
    // The opt-in is the `flags.datahub_allow_public_bind` row, and it is a SEPARATE knob rather than an inference from the
    // address because the mistake this catches is typing an address — no address can be its own
    // consent. ⚠ A refusal EXITS, where the tradehub daemon keeps trading headless: serving is
    // this process's only job, so "do not bind" and "do not run" are the same decision. Exit 2 —
    // the refused-configuration family this binary already uses for a rejected argument — not
    // FAILURE: nothing was tried and failed; the configuration was refused before anything opened.
    //
    // ⚠ The KEYS are the guard's THIRD input, not a decoration on its message. `ServerAuth::of`
    // classifies the very value handed to `serve_authed` below, so the guard and the server cannot
    // disagree about whether this process authenticates — and a non-loopback bind on a KEY-LESS
    // server is now REFUSED rather than warned about, opt-in or no opt-in. That combination used
    // to be a `ProceedExposed(_) if keys.is_none()` arm right here that logged a `warn!` and FELL
    // THROUGH to bind and serve, which is the state
    // `docs/decisions/0025-datahub-remote-posture.md` names in its own reopening conditions: "an
    // opt-in warning in front of an unauthenticated write verb is the exact state this record
    // exists to prevent". The decision lives in `server.rs` rather than as an `if keys.is_none()`
    // guard here for two reasons: it is CHEAPER to test there, beside the rest of the policy — ⚠ not
    // because a match arm in this `main` is unreachable from a test, which the first draft of this
    // comment claimed and `crates/vike-datahub/tests/help_cli.rs` refutes by spawning this very
    // binary and asserting its status and streams (and which now covers this refusal end to end) —
    // and a NEW ENUM VARIANT makes every match on
    // `BindDecision` — this one and any future caller's — fail to compile until its author
    // confronts the case, where a dropped match guard would silently restore the old behaviour.
    //
    // The `flags.datahub_allow_public_bind` row is the opt-in's only source (decision 0111). The
    // compute daemon reads the same row the same way
    // (`crates/vike-backtest/src/backtest_cli/serve.rs`'s `run_serve`).
    let allow_public = flags.datahub_allow_public_bind;
    let resolved_addrs: Vec<std::net::SocketAddr> =
        addr.to_socket_addrs().map(|it| it.collect()).unwrap_or_default();
    let server_auth = vike_datahub_client::bind::ServerAuth::of(keys.as_ref());
    match vike_datahub_client::bind::bind_decision(&resolved_addrs, allow_public, server_auth) {
        vike_datahub_client::bind::BindDecision::Proceed => {}
        // Reachable only on a KEYED server now — `bind_decision` answers the key-less half with
        // `RefuseUnauthenticated` instead — so this text may say "node keys ARE configured"
        // without a match guard to check it.
        vike_datahub_client::bind::BindDecision::ProceedExposed(exposed) => {
            tracing::warn!(
                %addr, %exposed,
                "vike-datahub binding a NON-LOOPBACK address (flags.datahub_allow_public_bind). \
                 Node keys ARE configured, so every connection must authenticate — but the \
                 handshake is PLAINTEXT and authenticates the CONNECTION, not each frame, so keep \
                 an SSH tunnel or a VPN in front of it for confidentiality and integrity"
            );
        }
        // ⚠ The combination this guard gained: reachable off-box, consented to, and authenticating
        // NOTHING. The message names both variables and both fixes — and names no VALUE: `keys` is
        // a redacting-`Debug` `NodeKeys` and nothing here reads a credential, so no secret can
        // reach a log line from this arm.
        vike_datahub_client::bind::BindDecision::RefuseUnauthenticated(exposed)
            if store_unreadable =>
        {
            // ⚠ THE SAME REFUSAL, THE OTHER CAUSE. Reaching the arm below instead would tell an
            // operator who HAS keys that they have none, and `Restart=on-failure` would repeat it
            // until somebody read the earlier `error!` line and connected the two. The disposition
            // is unchanged — still fail closed, still exit 2 — only the diagnosis is true.
            tracing::error!(
                %addr, %exposed,
                "config.datahub_bind_addr is NOT loopback and this server could not READ its credential \
                 store, so it has no node keys to authenticate with — refusing to start. This is \
                 NOT a missing configuration: the store is present and its keys may well be \
                 correct. Fix the file's permissions (`vike-cli secrets path` prints it; the \
                 earlier `credential store PRESENT but UNREADABLE` line names the OS error) and \
                 restart. Until then this daemon will keep exiting 2 and being restarted"
            );
            return Err(ExitCode::from(2));
        }
        vike_datahub_client::bind::BindDecision::RefuseUnauthenticated(exposed) => {
            tracing::error!(
                %addr, %exposed,
                "config.datahub_bind_addr is NOT loopback and this server has NO node keys — refusing \
                 to start. flags.datahub_allow_public_bind consents to being REACHABLE; it is not \
                 consent to serve history, and (in a backfill-serve build) WRITES into the store \
                 plus the IRREVERSIBLE DeleteSeries, to anyone who can open a socket. \
                 Either set VIKE_DATAHUB_OBSERVE_KEY (reads) and VIKE_DATAHUB_CONTROL_KEY (the \
                 Backfill write and the DeleteSeries removal) in the credential store — \
                 `vike-cli secrets path` prints the file — or put the address back on 127.0.0.1 \
                 and reach it with `ssh -L 7878:localhost:7878 <host>`. Keep the tunnel or a VPN \
                 either way: the handshake is plaintext even when the keys ARE set"
            );
            return Err(ExitCode::from(2));
        }
        vike_datahub_client::bind::BindDecision::Refuse(exposed) => {
            tracing::error!(
                %addr, %exposed,
                keyed = keys.is_some(),
                "config.datahub_bind_addr is NOT loopback — refusing to start. This server is meant to be \
                 reached over an SSH tunnel (its handshake is plaintext even when node keys ARE \
                 set): keep the address on 127.0.0.1 and run `ssh -L 7878:localhost:7878 <host>`. \
                 If this box genuinely must listen on a trusted network, `vike-cli config set \
                 flags.datahub_allow_public_bind true` — and set VIKE_DATAHUB_OBSERVE_KEY / \
                 VIKE_DATAHUB_CONTROL_KEY in the credential store first, so what is exposed is \
                 authenticated"
            );
            return Err(ExitCode::from(2));
        }
    }
    Ok(())
}

/// Open the ONE hist store this process serves — and, under `--record`, writes.
///
/// Extracted because the dry run opens it too, from a different point in the sequence (before the
/// bind guard, since it binds nothing), and two `DataFusionHist::open` call sites with two error
/// messages is how a store failure starts reading differently depending on which flag you passed.
#[cfg(feature = "serve-datafusion")]
pub(super) fn open_hist_store(store_dir: &str) -> Result<vike_data::DataFusionHist, ExitCode> {
    vike_data::DataFusionHist::open(store_dir).map_err(|e| {
        tracing::error!(store = %store_dir, error = %e, "vike-datahub: failed to open hist store");
        ExitCode::FAILURE
    })
}
