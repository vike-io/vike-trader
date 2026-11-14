//! The boot: the one environment sweep and the settings load. What it answers folds into
//! `vike_client::resolved`'s `Resolved`, and the two lazy hub-key resolutions live there too.

use std::collections::HashMap;

use vike_client::resolved::Resolved;

/// Refuse a stale environment, then resolve this machine's per-order notional ceiling
/// (the `policy.max_notional_per_order` row of the settings database).
///
/// `VIKE_MAX_ORDER_NOTIONAL` is no longer read by anything: it named the same per-order ceiling the
/// trading binaries enforce, and a ceiling any exported variable can raise is not a ceiling. An
/// operator who still has it set believes a cap is in force, so this REFUSES rather than ignoring
/// it — `vike_config::refuse_removed_env` writes the message, naming the file and key that replace
/// it. The check runs for every subcommand that runs, including read-only ones (a verb's help page
/// runs nothing and is answered before this, `crate::run`'s doc): a variable that is stale is stale,
/// and the error is exactly the diagnostic that explains it.
///
/// A key with no row is not an error (`None` — no advisory cap, today's behaviour); a row the
/// settings schema refuses is.
///
/// The read of `std::env::vars()` happens HERE, at the dispatcher, per the settings-registry rule;
/// neither `vike_config` nor `vike_boot` ever touches `std::env`. The ORDER of the steps below is
/// `vike_boot::boot`'s — four other roots run the same one — and each way this dispatcher departs
/// from it is a named arm of [`vike_boot::BootSpec`] carrying its own reason.
///
/// [`Resolved::from_booted`] SURFACES the loader's non-fatal resolutions, which this dispatcher used
/// to drop on the floor; a preference clamped to a policy ceiling used to take effect here with no
/// output at all: a limit the operator believes they set and does not have. They go to **stderr**,
/// never stdout, the stream a verb's answer owns.
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
        // subcommand, so a refusal here would take down `config set`, `config check` and every
        // `secrets` verb — the exact commands whose names the refusal prints. That is the JSON
        // incident of 2026-09-18 in one line: its first implementation refused a row it could not
        // read, a stale Windows path took every binary down on upgrade, and
        // `vike-cli config set` went with them. The mark rides `Settings::store_refusal`, and
        // `trade` refuses on it where a repair verb does not.
        settings: vike_boot::SettingsLoad::Load,
        credentials: vike_boot::Credentials::Deferred(
            "the credential store is opened ONLY for the node-facing surfaces (trade — the REPL, \
             the group-less one-shot status/halt/resume, and every one-shot verb under its \
             order/position/strategy groups, alike — and the top-level read-only report), \
             inside their own dispatch arms — see `hub_keyring`. A provenance \
             or backtest command has no business opening the file that holds every venue key on \
             the box, and this crate must not link `vike_bridge_core`'s transport stack to read \
             it.",
        ),
        log_home: vike_boot::LogHome::Elsewhere(
            "this CLI builds no log subscriber at all: it is a short-lived command whose stdout is \
             a verb's answer, and everything else it has to say goes to stderr.",
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
    // `vike_change_journal`. This one does not, for two reasons, and
    // `crates/vike-boot/tests/one_owner/boot_journal_wiring.rs` is where the row carrying them lives (it
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
    // (`trade`, and `vike-mcp` in its own process); the other two govern a venue mount this binary never performs.
    //
    // What is LOST is real and worth naming: on a box where `vike-cli` is the only vike binary that
    // ever runs, no anchor is ever written, so a hand edit of the `policy` rows there (anything but
    // `vike-cli config set`, which journals its own write) is bracketed by nothing. `vike-cli
    // config show` still PRINTS the effective values and their origin on demand — it just does not
    // durably record them.
    Ok(Resolved::from_booted(booted, vars, crate::cmd::args::PROGRAM))
}
