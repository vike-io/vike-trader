//! The boot: the one environment sweep, the settings load, and the two lazy node-key resolutions.

use std::collections::HashMap;
use std::path::Path;

use crate::Resolved;
use crate::cmd;
use crate::cmd::nodekeys::NodeKeyring;

/// Refuse a stale environment, then resolve this machine's per-order notional ceiling
/// (the `policy.max_notional_per_order` row of the settings database).
///
/// `VIKE_MAX_ORDER_NOTIONAL` is no longer read by anything: it named the same per-order ceiling the
/// trading binaries enforce, and a ceiling any exported variable can raise is not a ceiling. An
/// operator who still has it set believes a cap is in force, so this REFUSES rather than ignoring
/// it — `vike_config::refuse_removed_env` writes the message, naming the file and key that replace
/// it. The check runs for every subcommand, including read-only ones: a variable that is stale is
/// stale, and the error is exactly the diagnostic that explains it.
///
/// A key with no row is not an error (`None` — no advisory cap, today's behaviour); a row the
/// settings schema refuses is.
///
/// The read of `std::env::vars()` happens HERE, at the dispatcher, per the settings-registry rule;
/// neither `vike_config` nor `vike_boot` ever touches `std::env`. The ORDER of the steps below is
/// `vike_boot::boot`'s — four other roots run the same one — and each way this dispatcher departs
/// from it is a named arm of [`vike_boot::BootSpec`] carrying its own reason.
///
/// It also SURFACES the loader's non-fatal resolutions, which this dispatcher used to drop on the
/// floor. `vike_config` returns them as DATA rather than logging them, deliberately: it does not
/// depend on `tracing`, because a library that writes to a caller's stderr on its own initiative
/// cannot be used by a binary whose STDOUT is a protocol — which `vike-cli mcp`'s is. The obligation
/// to emit them is therefore the binary's, and a preference clamped to a policy ceiling used to take
/// effect here with no output at all: a limit the operator believes they set and does not have. They
/// go to **stderr**, never stdout, for the MCP reason above.
///
/// The same sweep also yields THE settings directory, which `cmd::secrets` and `cmd::trade` both
/// need, and its SIBLING `<project>/user_data` for `cmd::init`. Several consumers, one read — which
/// is also what keeps `cmd::init` free of any `std::env` call of its own.
pub(super) fn resolve_policy() -> Result<Resolved, String> {
    let vars: HashMap<String, String> = std::env::vars().collect();
    let cwd = std::env::current_dir().ok();
    // ⚠ The ORDER — refuse, resolve the directory ONCE, load — belongs to `vike-boot`, not to this
    // file. Four other composition roots run the same sequence, each used to carry its own copy of
    // it, and the walk happening in five places is what made the CI box's "no policy, no credentials,
    // every venue silently paper" expensive to fix. The three arms below are the ways THIS root
    // genuinely departs, each stating its reason where a diff can see it.
    let booted = vike_boot::boot(&vike_boot::BootSpec {
        env: &vars,
        cwd: cwd.as_deref(),
        identity: vike_boot::Identity {
            name: env!("CARGO_PKG_NAME"),
            version: env!("CARGO_PKG_VERSION"),
        },
        removed_env: vike_boot::RemovedEnv::Refuse,
        // ⚠ REPORT, and this is the arm the whole design turns on. `resolve_policy` runs for EVERY
        // subcommand, so a refusal here would take down `config mirror`, `config check` and every
        // `secrets` verb — the exact commands whose names the refusal prints. That is the JSON
        // incident of 2026-09-18 in one line: its first implementation refused a row it could not
        // read, a stale Windows path took every binary down on upgrade, and
        // `vike-cli config mirror` went with them. The mark rides `Settings::store_refusal`, and
        // `trade`/`mcp` refuse on it where a repair verb does not.
        settings: vike_boot::SettingsLoad::Load,
        // `config show`/`check` render the ceilings, but a refusal here would take down
        // `config migrate-store` itself — the same argument as `SettingsLoad::Load` above; the mark
        // rides `Settings::store_refusal`, on which `trade`/`mcp` refuse.
        ceilings: vike_boot::Ceilings::InterpretOrMark { now_ms: vike_model::now_ms() },

        credentials: vike_boot::Credentials::Deferred(
            "the credential store is opened ONLY for the node-facing surfaces (trade — the REPL, \
             the group-less one-shot status/halt/resume, and every one-shot verb under its \
             order/position/strategy groups, alike — mcp, and the top-level read-only report), \
             inside their own dispatch arms — see `node_keyring`. A provenance \
             or backtest command has no business opening the file that holds every venue key on \
             the box, and this crate must not link `vike_bridge_core`'s transport stack to read \
             it.",
        ),
        log_home: vike_boot::LogHome::Elsewhere(
            "this CLI builds no log subscriber at all: it is a short-lived command whose stdout is \
             a protocol under `mcp`, and everything it has to say goes to stderr.",
        ),
        disclosure: vike_boot::Disclosure::Skip(
            "`boot_lines` re-reads every settings file to recover each row's ORIGIN, which is a \
             daemon's one-off cost and a command-line tool's per-invocation one. `vike-cli config \
             show` is the surface that prints all of it, on request.",
        ),
    })?;
    // ⚠ **NO BOOT ANCHOR is written here, and the exclusion is deliberate.** The two roots that
    // write one — `vike-desktop` and `vike-tradehub` (it said `vike-app`, "the two roots that
    // ENFORCE the ceilings", until 2026-09-28; the desktop mounts no venue since #1610, so the
    // ceilings it loads cap only its order-entry preview) — each call
    // `vike_boot::journal_boot_settings(..)` once, appending one `boot_settings` line per start to
    // `vike_model::change_journal`. This one does not, for two reasons, and
    // `crates/vike-boot/tests/boot_journal_wiring.rs` is where the row carrying them lives (it
    // fails if this file quietly starts writing one).
    //
    // First, RATE: this function runs for EVERY subcommand, `secrets path` and `config show`
    // included, so the ledger's growth would track how often a human or an MCP client types a
    // command — unbounded, and uncorrelated with anything changing. That is the shape the change
    // journal's own module doc refuses for connectivity events: a per-invocation stream mixed into
    // a per-change ledger buries the ledger.
    //
    // Second, and worse, TRUTH: a `boot_settings` record claims the EFFECTIVE ceilings, and in this
    // process none of them is. `max_notional_per_order` is an advisory guardrail on two surfaces
    // (`trade`, `mcp`); the other two govern a venue mount this binary never performs.
    //
    // What is LOST is real and worth naming: on a box where `vike-cli` is the only vike binary that
    // ever runs, no anchor is ever written, so a hand edit of the `policy` rows there (anything but
    // `vike-cli config set`, which journals its own write) is bracketed by nothing. `vike-cli
    // config show` still PRINTS the effective values and their origin on demand — it just does not
    // durably record them.
    //
    // WHICH rung answered — see `Resolved::settings_dir_origin`. Derived from the same override
    // value the resolver was handed (`vike-boot` returns it already trimmed and blank-filtered), so
    // the two cannot disagree about a blank one.
    let settings_dir_origin = cmd::config::check::dir_origin(
        booted.settings_dir_override.as_deref(),
        booted.settings_dir.as_deref(),
    );
    // `<project>/user_data` — the SIBLING of the settings directory, off the SAME walk (see
    // `Resolved::user_data_dir`). Resolved here, in the one place this crate reads the environment,
    // so `cmd::init` takes it as a parameter and names no variable of its own.
    //
    // ⚠ Literally the same walk now, not merely the same rules: it is `user_data_dir_beside` over
    // the directory `vike_boot::boot` ALREADY resolved. `project_user_data_dir_from`, which stood
    // here, falls back to a walk of its own that does not honour `$VIKE_SETTINGS_DIR` — so
    // `vike-cli init` scaffolded into one project while `config show` reported another, on exactly
    // the deployments the override exists for.
    let user_data_dir = vike_model::paths::state_path::user_data_dir_beside(
        vars.get("VIKE_USER_DATA_DIR").map(String::as_str),
        booted.settings_dir.as_deref(),
    );
    for line in settings_warning_lines(&booted.settings) {
        eprintln!("{line}");
    }
    // `<project>` — see `Resolved::project_root`. The empty-parent filter is the same one
    // `user_data_dir_beside` applies: a relative `settings` has `""` as its parent, and joining a
    // sibling onto that would silently name a directory in the working directory.
    let project_root = booted
        .settings_dir
        .as_deref()
        .and_then(Path::parent)
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf);
    Ok(Resolved {
        policy_max_notional: booted.settings.policy.max_notional_per_order,
        // BOTH marks, and the `or` is what closes the one hazard the store-unreadable arm's own
        // degrade argument does not cover. That argument is *an unreadable settings store ALWAYS
        // co-occurs with an all-paper mount*, because `vike_secrets::Backend` decides both on one
        // file probe — true for a whole-store failure (a rollback journal, a schema out of range, a
        // corrupt header). It is NOT true for a failure isolated to the `setting` TABLE:
        // `read_settings` issues per-table queries, so `SELECT … FROM setting` can return
        // `SQLITE_CORRUPT` while the `credential` table reads perfectly — venues arm LIVE off
        // credentials that loaded, and the ceilings fall back to the files with `adoption` `None`,
        // which also skips the drift block, so not even a per-key warning is printed. Refusing the
        // two ACTING verbs costs a healthy box nothing and removes that corner.
        seal_refusal: booted
            .settings
            .seal_refusal
            .clone()
            .or_else(|| booted.settings.store_refusal.clone()),
        settings_dir: booted.settings_dir,
        settings_dir_origin,
        // The boot's OWN answer, not a second derivation — see `Resolved::state_dir`.
        state_dir: booted.state_dir,
        // Carried, not re-read: `cmd::secrets`' fallback rung needs the VALUE and not just the
        // origin verdict above — see `Resolved::settings_dir_override`.
        settings_dir_override: booted.settings_dir_override,
        user_data_dir,
        project_root,
        // This box's dial default for a node — read off the SAME loaded `Settings` the policy
        // ceiling above comes from, so `vike-cli config show` and the `backend` verbs can never
        // disagree about which node this box is pointed at.
        node_addr: booted.settings.config.node_addr.clone(),
        // …and this box's dial default for the COMPUTE daemon, off that same loaded `Settings` for
        // the same reason. `study` is its only reader today; `config show` reports it either way,
        // which is precisely why it must be READ somewhere — see `vike_config::CONSUMPTION`.
        backtest_addr: booted.settings.config.backtest_addr.clone(),
        // …and this box's dial address for the DATA server, off that same loaded `Settings`. It is
        // the CLIENT half of the datahub pair — see the field for why the server's own bind address
        // is a different key, and for what a box whose datahub is not on the compiled-in default
        // used to get from the CLI while the GUI reached it.
        datahub_addr: booted.settings.config.datahub_addr.clone(),
        // ⚠ The datahub KEY PAIR is NOT resolved here any more. It was, off this same sweep, and that
        // read the PROCESS ENVIRONMENT AND NOTHING ELSE — so keys sitting in the credential store,
        // which is where this binary's own refusal text tells an operator to put them, did nothing.
        // It is now [`datahub_keyring`], lazily, env first and store second: the precedence
        // `cmd::nodekeys::resolve` already applies to the TRADEHUB pair.
        env: vars,
    })
}

/// The NODE-key store — `<project>/settings/node.env`, falling back to the credential store for a
/// pair that has not been moved yet, and SAYING SO when it does.
///
/// A store that EXISTS but cannot be read is reported on **stderr** (never stdout — `mcp`'s stdout
/// is a protocol) and treated as EMPTY rather than fatal. Deliberate: an exported key must still
/// work when the file is broken, and the "no key anywhere" message this then produces NAMES the
/// store — so the operator is pointed at the same file either way, twice. There is no
/// silent-wrong-credential hazard to weigh against that: a key that does not resolve cannot open a
/// connection at all.
///
/// ⚠ The warning is emitted here rather than inside `vike-secrets` because that crate carries no
/// logging dependency and returns findings as DATA — the same division `permission_warning` already
/// has. It goes to stderr, never stdout, because `vike-cli mcp`'s stdout is a protocol.
///
/// ⚠ It is printed ONCE PER RESOLUTION, not once per key. Five arms resolve keys and two pairs
/// exist; a per-key notice would put four identical lines in front of an operator who has one thing
/// to do.
///
/// ⚠ **`is_node_key` is the CALLER's own family, never the four-name `is_platform_key`, and this
/// binary is the one that HAS to get that right because it is the one that WRITES both files.**
/// `resolve_node_keys` answers *which file*, and the caller then reads its own pair out of it — so
/// with the wide predicate, a box where `vike-cli datahub setup` had written `node.env` made that
/// file the answer for the TRADEHUB pair too, and a working tradehub pair still in `secrets.env`
/// resolved to nothing: `trade`/`report`/`mcp` then signed with no key and the node answered
/// `bad mac`, with no migration notice, because the source was `NodeFile` rather than the legacy
/// one. Both of this function's callers name their own family
/// (`vike_model::credential_keys::is_tradehub_node_key` / `is_datahub_node_key`), which is decision
/// 0051's "answers wholly" scoped to the pair it is actually about.
pub(super) fn node_key_store(
    resolved: &Resolved,
    is_node_key: impl Fn(&str) -> bool,
) -> HashMap<String, String> {
    let settings = resolved.settings_dir.as_deref().and_then(|p| p.to_str());
    match vike_secrets::resolve_node_keys(settings, is_node_key) {
        Ok((r, source)) => {
            if let Some(w) = &r.warning {
                eprintln!("vike-cli: ⚠ {w}");
            }
            if source == vike_secrets::NodeKeySource::LegacyCredentialStore {
                let dir = resolved
                    .settings_dir
                    .as_ref()
                    .map_or_else(|| "<project>/settings".to_string(), |d| d.display().to_string());
                eprintln!("vike-cli: ⚠ {}", vike_secrets::legacy_node_key_notice(&dir));
            }
            r.secrets.into_map()
        }
        Err(e) => {
            eprintln!("vike-cli: cannot read the node-key store: {e}");
            HashMap::new()
        }
    }
}

/// Resolve the two vike-tradehub NODE KEYS for the surface about to run: the process environment
/// first, then `<project>/settings/node.env` (or the settings database's `node_key` table once
/// migrated) — the store `vike-tradehub` itself loads them from.
///
/// Called ONLY from the node-facing arms of [`dispatch`] — `trade` (the REPL, the three
/// group-less one-shot words `status`/`halt`/`resume`, and every one-shot verb under the
/// `order`/`position`/`strategy` groups) and `mcp`, plus the top-level read-only `report`. Every
/// READ among those — `trade status`, each group's own `ls`, and `report` — uses just the
/// keyring's observe half. Every other subcommand keeps the file unopened: the least credential
/// exposure that still fixes the defect.
///
/// ⚠ This used to name exactly TWO reads, `trade status` and `report`, and that was the whole set
/// the day it was written — before the trade-CLI-plane's group layer gave `order`/`position`/
/// `strategy` their own `ls` reads, each connecting the observe half only for the identical
/// reason. No count is written here any more for the same reason `crate::cmd::trade::plane`'s
/// group roster is not re-typed elsewhere: a fixed number rots the moment a group grows another
/// read.
pub(super) fn node_keyring(resolved: &Resolved) -> NodeKeyring {
    // ⚠ The PATH handed on is the NODE store's, and which file it names is the whole question this
    // branch exists to answer. `cmd::nodekeys::resolve` puts it in the "where would I have found
    // this" message an operator with NO key reads — and that operator has nothing to migrate, so
    // naming `secrets.env` would send them to write a key into the deprecated file and then be told
    // by the notice below to move it. `backend setup` writes `node.env`; the refusal names
    // `node.env`; the two mouths of this binary say one thing. A box that HAS a legacy pair never
    // sees this message at all — its keys resolve, and the migration notice is what it gets
    // instead.
    let store = node_key_store(resolved, vike_model::credential_keys::is_tradehub_node_key);
    let store_path = resolved.settings_dir.as_ref().map(|d| d.join(vike_secrets::NODE_FILE));
    cmd::nodekeys::resolve(&resolved.env, &store, store_path.as_deref().map(Path::new))
}

/// The DATAHUB node pair — `VIKE_DATAHUB_OBSERVE_KEY` / `VIKE_DATAHUB_CONTROL_KEY` — resolved
/// PROCESS ENVIRONMENT FIRST, CREDENTIAL STORE SECOND.
///
/// ⚠ **This used to be a field resolved off the environment sweep alone, and that was the same
/// defect this file had already found and fixed for the TRADEHUB pair.** The module doc above
/// records that one: both node keys "used to read `std::env::var` and nothing else, so a
/// correctly-configured box answered 'nothing to do' and exited". The datahub pair was written to
/// the same shape afterwards, its doc comment claiming it worked "exactly as `node_keyring` does" —
/// which it did not. MEASURED 2026-09-08 against a keyed datahub: the pair in
/// `<project>/settings/secrets.env` gave "no node keys were supplied", and the identical pair
/// exported into the environment served 6.8 billion rows.
///
/// The worst part was the refusal text, not the resolution: `vike-datahub-client`'s message names
/// the credential store as the remedy, and following it exactly left an operator broken. A refusal
/// that names the wrong remedy is more expensive than one that names none.
///
/// ⚠ The store read is why this is a FUNCTION rather than a field: it opens the NODE-key store
/// (`node.env`, or the settings database's `node_key` table once migrated — not, since 2026-09-08,
/// the file holding every venue key, bar the legacy fallback below), and only the arms that can
/// dial a datahub call it. That is a
/// deliberate widening of the rule the module doc states — "a provenance or backtest command has no
/// business opening" the credential store — and the argument for it is that a `backtest --addr` IS a
/// node-facing invocation, the category the rule already excepts for `trade`/`mcp`. The narrower
/// alternative (read the store only when the invocation names a remote) was measured and REFUSED:
/// `cmd::data`'s address resolves to `DEFAULT_ADDR` whether or not `--addr` was passed, and
/// `vike_config::Config::datahub_addr` is a second way to be remote with no flag at all, so "did
/// this command go remote" cannot be answered from the argv the dispatcher holds.
///
/// The environment still WINS, so a box that exports the pair opens no file at all.
pub(super) fn datahub_keyring(resolved: &Resolved) -> Option<vike_node_proto::auth::NodeKeys> {
    if let Some(keys) = vike_node_proto::auth::node_keys_from_vars(&resolved.env) {
        return Some(keys);
    }
    // ⚠ The NODE store, not the credential store. Before 2026-09-08 this read `secrets.env`, which
    // meant an arm needing a low-sensitivity key opened the file holding 168 venue secrets. It now
    // reads `node.env` and falls back to the old file only while a box has not migrated, saying so.
    let store = node_key_store(resolved, vike_model::credential_keys::is_datahub_node_key);
    vike_node_proto::auth::node_keys_from_vars(&store)
}

/// Every non-fatal resolution the loader made, formatted for stderr — the PURE half of
/// [`resolve_policy`]'s surfacing step.
///
/// A function rather than an inline loop because "the binary that loaded the settings SURFACES the
/// warnings, never swallows them" is a real property with a real failure mode, and a property worth
/// stating is worth gating — see `a_clamp_warning_is_surfaced_not_swallowed`.
pub(super) fn settings_warning_lines(settings: &vike_config::Settings) -> Vec<String> {
    settings.warnings.iter().map(|w| format!("vike-cli: settings: {w}")).collect()
}
