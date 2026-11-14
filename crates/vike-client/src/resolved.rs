//! The fold of a booted process into what a verb reads, and the two lazy hub-key resolutions.
//!
//! ⚠ **The boot call itself stays in each program** (`crates/vike-boot/tests/one_owner.rs`'s
//! `a_crate_that_boots_may_not_walk_again` polices only the crates that call it), and so does the
//! program's environment sweep, which arrives here as a parameter. This file folds the
//! `vike_boot::Booted` it is handed and resolves no project of its own: `bin` is the name a stderr
//! line opens with, because a program's stdout may be a protocol and its stderr is still read.

use std::collections::HashMap;
use std::path::Path;

use crate::hubkeys::HubKeyring;

/// What a program resolves ONCE, before any verb runs, out of its boot and its one environment
/// sweep — see [`Resolved::from_booted`].
pub struct Resolved {
    /// `max_notional_per_order` (the `policy.max_notional_per_order` row), or `None` when unset.
    pub policy_max_notional: Option<f64>,
    /// **EITHER settings mark** — [`vike_config::Settings::seal_refusal`] (the store was read and
    /// says something illegal) or [`vike_config::Settings::store_refusal`] (it could not be read at
    /// all) — carried so the router can gate the verbs that ACT on a ceiling without the loader
    /// having refused every verb that REPAIRS one.
    ///
    /// One field for two marks because the two differ in what the OPERATOR must do and not in what
    /// this binary must do: in both states the values this process resolved are not reliably the
    /// ones a daemon on this box would resolve, and in both the repair is a verb that has to keep
    /// running. The message names which it was.
    ///
    /// ⚠ This is the field that makes the sentence in `vike-cli`'s `resolve_policy`, at its
    /// `settings:` arm, TRUE. That comment has claimed "`trade` and `vike-mcp` refuse on it where a repair
    /// verb does not" since the mark was introduced, and so has
    /// [`vike_config::Settings::store_refusal`]'s own doc — and until this field existed NOTHING in
    /// this crate read either one: both marks were printed as warnings and every verb ran. A claim
    /// two comments make about a gate that is not there is worse than no gate, because it is what a
    /// reader checks instead of the code.
    pub seal_refusal: Option<String>,
    /// `<project>/settings` — THE settings directory, or `None` when no project sits above the
    /// working directory. Resolved here rather than inside a subcommand because the rule is that
    /// only the composition root reads the environment, and this one already had the map in hand:
    /// `cmd::secrets` inspects the credential store inside it, and `cmd::trade` persists its REPL
    /// history under its `state/` sub-directory.
    pub settings_dir: Option<std::path::PathBuf>,
    /// `<project>/settings/state` — the PROGRAM-WRITTEN state root, off the SAME walk (it is
    /// `vike_boot::Booted::state_dir`, never re-derived: the `_from`-less resolvers are
    /// `$VIKE_SETTINGS_DIR`-BLIND, so a second walk could answer with a different project than the
    /// one the settings and credentials came from).
    ///
    /// Reaches the APPENDING arms — deliberately not counted here, since `config set` made the
    /// count wrong the day it landed and a count is the shape of claim this repository has watched
    /// rot. The arms that take it are visible in `vike-cli`'s `dispatch`, each one carrying its own
    /// reason. `secrets set` writes one
    /// `vike_change_journal` `credential_write` record beside the store write; `vike-mcp --trace`
    /// writes the AGENT TRANSCRIPT into a sibling directory (`crates/vike-mcp/src/trace.rs`). `None` (no
    /// project above the working directory) means the first one still writes the store and records
    /// NOTHING — an append-only ledger in a guessed directory is worse than a counted absence, the
    /// same rule `vike_boot::journal_boot_settings` follows — while the second REFUSES to start,
    /// because there the operator ASKED for a record and a silent absence is what they would
    /// discover after the incident. `crates/vike-mcp/src/config.rs`'s `resolve_trace` argues the asymmetry.
    pub state_dir: Option<std::path::PathBuf>,
    /// The `$VIKE_SETTINGS_DIR` value the boot HONOURED — trimmed, and `None` when blank or unset.
    ///
    /// ⚠ **It is the RUNG, and it is carried because only this root can know it.** Below here the
    /// two are indistinguishable: the resolver takes the override as a parameter and returns a bare
    /// path. `vike-cli`'s `config` arm folds it into a two-state verdict for `config check`'s refusal
    /// (`cmd::config::check::dir_origin`); `cmd::secrets` needs the VALUE.
    ///
    /// ⚠ **This used to be "not derivable from [`Resolved::settings_dir`]", and that has CHANGED.**
    /// `vike_boot::boot` resolved the directory as `spec.cwd.and_then(..)`, so a process whose
    /// working directory had been removed, unmounted or made unsearchable got `settings_dir: None`
    /// while this stayed `Some(..)` — the pairing #1514 taught `cmd::secrets` to survive. The boot
    /// now honours a name with no walk (`vike_secrets::project_settings_dir_for`), so `Some` here
    /// implies `Some` there, holding the same path, and that fallback rung is unreachable through
    /// the dispatcher. It is KEPT rather than deleted: it is the belt against the boot regressing,
    /// and `cmd::secrets`'s `settings_dir_of` states its own reachability at its site.
    pub settings_dir_override: Option<String>,
    /// `<project>/user_data` — THE user-content directory (strategies, run profiles, results,
    /// notebooks), or `None` when no project sits above the working directory.
    ///
    /// A SIBLING of [`Resolved::settings_dir`], never a child, and resolved from the SAME walk so
    /// the two can never answer with different projects — `crates/vike-paths/src/state_path.rs`'s
    /// `PROJECT_USER_DATA_DIR` argues why the ownership split matters. `$VIKE_USER_DATA_DIR` names
    /// it outright; it is a separate variable from `$VIKE_SETTINGS_DIR` because pointing the app at
    /// a strategy library on another disk is a different question from relocating a deployment's
    /// settings, and one variable for both would force them to move together.
    ///
    /// ⚠ Unlike `settings_dir`, this is where the directory BELONGS rather than one that was found:
    /// a tree with no `user_data/` is the ordinary state of a fresh install, which is precisely the
    /// case `init` exists to fix.
    pub user_data_dir: Option<std::path::PathBuf>,
    /// `<project>` itself — the PARENT of [`Resolved::settings_dir`], and the root the two
    /// machine-owned siblings hang off: `bin/` (where a project's tools are installed) and `tmp/`
    /// (where a rewritten profile is staged before it is handed to a child process).
    ///
    /// Resolved from the SAME walk as everything else, for the reason that walk exists — "which
    /// project am I in" must have ONE answer — and by the same `parent()` rule
    /// `vike_paths::state_path::user_data_dir_beside` applies to the sibling beside it. ⚠ Neither
    /// of those two directories has an environment variable of its own, deliberately
    /// (`crates/vike-paths/src/state_path.rs`'s `PROJECT_TMP_DIR` argues both halves), so unlike
    /// `user_data_dir` there is nothing to honour here but the parent.
    ///
    /// `None` when no project sits above the working directory: the `--local` engine then falls
    /// back to `PATH`, and a run that would need scratch REFUSES rather than reaching for the
    /// system temp directory.
    pub project_root: Option<std::path::PathBuf>,
    /// `config.node_addr` — THIS box's dial address for a running `vike-tradehub` node, which only
    /// `backend status` reads: no verb takes it as a default for `--node`. `None` = the key is unset,
    /// which is every box that has never attached to one.
    ///
    /// ⚠ It is the CLIENT's address, and its daemon-side twin `config.tradehub_addr` is deliberately
    /// NOT read here: that key is where a daemon BINDS, on the daemon's own box, and with a tunnel
    /// in front the two agree only by coincidence of the tunnel.
    /// `crates/vike-config/src/config.rs`'s `Config::node_addr` sorts every one of that file's
    /// addresses by whose box holds the value and whether that box listens or dials.
    ///
    /// Reaches the `backend` arm alone today. The verbs that still REQUIRE `--node`
    /// (`report`, `trade` — REPL and one-shot alike — and `vike-mcp`) are a deliberately separate
    /// change: each parses the flag as mandatory, and making it optional is a per-verb migration
    /// rather than a dispatcher edit.
    pub node_addr: Option<String>,
    /// `config.node_ack_wait_ms` — how long `trade` (the REPL and every one-shot write verb) and the
    /// `vike-mcp`'s write tools wait for the node's verdict on the command they just sent, off the SAME
    /// loaded `Settings` as [`Resolved::node_addr`] (its pair). Always a value: a box with no row
    /// waits the two seconds both surfaces always waited, and the range is enforced where the row
    /// is written and loaded, never here.
    ///
    /// Reaches the `trade` and `vike-mcp` arms as a PARAMETER, down to the
    /// `TradehubControlHandle::await_outcome` call each of them makes (a `src/cmd/` file reads no
    /// settings of its own). ⚠ It only decides when the client stops waiting and says the outcome is
    /// NOT known: the command was already sent and may still execute, and nothing here cancels or
    /// re-sends it.
    pub node_ack_wait: std::time::Duration,
    /// `config.backtest_addr` — THIS box's dial address for the backtesthub
    /// (`vike-backend backtesthub --addr`), and the middle rung of the ladder both planes fold:
    /// `--addr` → this
    /// → `vike_config::DEFAULT_BACKTEST_ADDRESS`. `None` = the key is unset, which is every box that
    /// has not been pointed at one.
    ///
    /// ⚠ Unlike [`Resolved::node_addr`] it is NOT a client-only key: the backtesthub BINDS the
    /// same value on its own box, because it runs beside the store it computes over and no tunnel
    /// separates the two. `crates/vike-config/src/config.rs`'s `Config::backtest_addr` argues why
    /// one key serves both ends where every other plane here needs a pair.
    ///
    /// Reaches the `backtest` arm and the `research` one (whose `study` sub-verb dials the same
    /// daemon), and it is resolved HERE rather than in either of
    /// them for the rule this whole struct exists for: only the composition root reads settings,
    /// and a `src/cmd/` file takes what it needs as a parameter. ⚠ Both fold it through ONE
    /// function, `vike-cli`'s `cmd::backtest::resolve_address` — the two planes dial the same compute
    /// daemon and read the same setting, so a second copy of the fold would be the defect that
    /// function was written to remove: two copies could disagree about a blank rung and aim one at
    /// `7878` or `7879`.
    ///
    /// ⚠ It reached the `study` arm ALONE until 2026-09-13, and that arm RETIRED — a field written
    /// and never read is `dead_code` on a lane that runs `-D warnings`, so ruling 1's `research`
    /// plane supplying a successor reader is load-bearing rather than incidental.
    pub backtest_addr: Option<String>,
    /// `config.datahub_addr` — THIS box's dial address for the DATA server, and the middle rung of
    /// the ladder the datahub dialers fold: `--addr` → this → the verb's own compiled-in default.
    /// `None` = the key is unset, which is every box that has not been pointed at one.
    ///
    /// ⚠ Unlike [`Resolved::backtest_addr`] this one IS client-only, and the distinction is the
    /// whole reason there are two keys: the data server binds `config.datahub_advertise_addr` on
    /// its own box, while this names where a client DIALS — across a tunnel, if that is how the
    /// operator reaches it. `crates/vike-config/src/config.rs`'s `Config::datahub_addr` argues the
    /// split.
    ///
    /// Reaches the `data` arm and `vike-mcp` (whose `list_series` and `delete_series` are the
    /// two tools on the data plane), for the rule this whole struct exists for: only the
    /// composition root reads settings, and a `src/cmd/` file takes what it needs as a parameter.
    ///
    /// ⚠ It reached NEITHER until this field existed, and the shape of that hole is worth keeping:
    /// `crates/vike-desktop/src/app_methods.rs` read the key and was the ONLY reader, so
    /// `vike_config::CONSUMPTION` was satisfied by one consumer while every CLI dialer sat on its
    /// own compiled-in `127.0.0.1:7878`. On a box whose datahub is anywhere else, the GUI reached
    /// it and `vike-cli data` did not — two clients, one setting, two answers, and the failure
    /// reads as "the datahub is down" rather than "I dialled the wrong place". It is the exact
    /// defect [`Resolved::backtest_addr`] was threaded here to close on the backtest plane, one
    /// plane over.
    pub datahub_addr: Option<String>,
    /// THIS box's patience on a bad node link — the `preferences.observe_stall_timeout_ms` and
    /// `preferences.control_reply_timeout_ms` rows, raise-only windows (refused below their built-in
    /// values at the write and the load) that default to the window every dial had before the rows
    /// existed.
    ///
    /// Reaches every `trade` and `vike-mcp` dial of a node's observe stream and control channel, through
    /// [`hub_keyring`]'s `HubKeyring::patience` (the keyring is what each of those verbs already
    /// carries to its connect): a `src/cmd/` file reads no setting of its own, so the connect takes
    /// it as a parameter (`connect_with_read_timeout` / `connect_with_reply_timeout`). It does not
    /// move the one-shot reads (`status`, the group `ls` verbs' snapshot wait, `report`), which keep
    /// their own fixed bounds.
    pub link_patience: vike_tradehub_client::liveness::LinkPatience,
    /// The process environment, kept for `secrets set --from-env NAME` (the one verb that takes a
    /// value from a named variable). The dispatcher owns this map; nothing below it reads
    /// `std::env`, and NO hub key comes out of it: [`hub_keyring`] and [`datahub_keyring`] read the
    /// store alone (the four hub-key variables are `vike_config::REMOVED_ENV` refusals).
    pub env: HashMap<String, String>,
    /// THE credential store handle the boot opened — `vike_boot::Booted::store`, over
    /// [`Resolved::settings_dir`] and under the scope `VIKE_NO_LIVE_KEYS` declared (a malformed
    /// value has already refused the boot). No I/O was performed opening it. `hub_key_store`
    /// reads through it, and every arm that opens the store over a directory of its own carries its
    /// SCOPE ([`vike_secrets::CredentialStore::scope`]), so no read in this binary is made under a
    /// wider scope than the process declared.
    pub store: vike_secrets::CredentialStore,
}

impl Resolved {
    /// Fold what the boot answered, and the program's own environment sweep, into what its verbs
    /// read. `bin` is the name the loader's warnings go out under.
    ///
    /// It also SURFACES the loader's non-fatal resolutions, on **stderr**. `vike_config` returns them
    /// as DATA rather than logging them, deliberately: it does not depend on `tracing`, because a
    /// library that writes to a caller's stderr on its own initiative cannot be used by a binary
    /// whose STDOUT is a protocol. The obligation to emit them is therefore the binary's, and this is
    /// where every program that boots meets it.
    pub fn from_booted(
        booted: vike_boot::Booted,
        vars: HashMap<String, String>,
        bin: &str,
    ) -> Self {
        // `<project>/user_data` — the SIBLING of the settings directory, off the SAME walk (see
        // `Resolved::user_data_dir`). Resolved here, in the fold of the one environment sweep, so
        // `cmd::init` takes it as a parameter and names no variable of its own.
        //
        // ⚠ Literally the same walk now, not merely the same rules: it is `user_data_dir_beside` over
        // the directory `vike_boot::boot` ALREADY resolved. `project_user_data_dir_from`, which stood
        // here, falls back to a walk of its own that does not honour `$VIKE_SETTINGS_DIR` — so
        // `vike-cli init` scaffolded into one project while `config show` reported another, on exactly
        // the deployments the override exists for.
        let user_data_dir = vike_paths::state_path::user_data_dir_beside(
            vars.get("VIKE_USER_DATA_DIR").map(String::as_str),
            booted.settings_dir.as_deref(),
        );
        for line in settings_warning_lines(bin, &booted.settings) {
            eprintln!("{line}");
        }
        // The `trade`/`vike-mcp` previews' advisory quantity cap, off the SAME loaded `Settings` the policy
        // ceiling below comes from (decision 0111): the row alone, read by `crate::verbs::guardrail_capabilities`.
        crate::verbs::install_max_order_quantity_row(booted.settings.preferences.max_order_qty);
        // `<project>` — see `Resolved::project_root`. The empty-parent filter is the same one
        // `user_data_dir_beside` applies: a relative `settings` has `""` as its parent, and joining a
        // sibling onto that would silently name a directory in the working directory.
        let project_root = booted
            .settings_dir
            .as_deref()
            .and_then(Path::parent)
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf);
        Resolved {
            policy_max_notional: booted.settings.policy.max_notional_per_order,
            // BOTH marks, and the `or` is what closes the one hazard the store-unreadable arm's own
            // degrade argument does not cover. That argument is *an unreadable settings store ALWAYS
            // co-occurs with an all-paper mount*, because `vike_secrets::StoreBackend` decides both on one
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
            // The boot's OWN answer, not a second derivation — see `Resolved::state_dir`.
            state_dir: booted.state_dir,
            // Carried, not re-read: `cmd::secrets`' fallback rung needs the VALUE and not just an
            // origin verdict — see `Resolved::settings_dir_override`.
            settings_dir_override: booted.settings_dir_override,
            user_data_dir,
            project_root,
            // This box's dial default for a node — read off the SAME loaded `Settings` the policy
            // ceiling above comes from, so `vike-cli config show` and the `backend` verbs can never
            // disagree about which node this box is pointed at.
            node_addr: booted.settings.config.node_addr.clone(),
            // …and its pair: how long this client waits for the node's verdict on a command it sent
            // (`Resolved::node_ack_wait`). The loader already enforced the range, so this is a plain
            // unit conversion; the value reaches `trade` and `vike-mcp` as a parameter.
            node_ack_wait: std::time::Duration::from_millis(u64::from(
                booted.settings.config.node_ack_wait_ms,
            )),
            // …and this box's dial default for the backtesthub, off that same loaded `Settings` for
            // the same reason. `study` is its only reader today; `config show` reports it either way,
            // which is precisely why it must be READ somewhere — see `vike_config::CONSUMPTION`.
            backtest_addr: booted.settings.config.backtest_addr.clone(),
            // …and this box's dial address for the DATA server, off that same loaded `Settings`. It is
            // the CLIENT half of the datahub pair — see the field for why the server's own bind address
            // is a different key, and for what a box whose datahub is not on the compiled-in default
            // used to get from the CLI while the GUI reached it.
            datahub_addr: booted.settings.config.datahub_addr.clone(),
            // …and this box's link patience, off that same loaded `Settings`: the two raise-only
            // windows `trade` and `vike-mcp` dial a node with. Both are `u32` rows with a built-in value, so
            // a box that never wrote them resolves the windows every dial always had.
            link_patience: vike_tradehub_client::liveness::LinkPatience::from_millis(
                booted.settings.preferences.observe_stall_timeout_ms,
                booted.settings.preferences.control_reply_timeout_ms,
            ),
            // ⚠ The datahub KEY PAIR is NOT resolved here any more. It was, off this same sweep, and that
            // read the PROCESS ENVIRONMENT AND NOTHING ELSE — so keys sitting in the credential store,
            // which is where this binary's own refusal text tells an operator to put them, did nothing.
            // It is now [`datahub_keyring`], lazily, from the store alone, like the TRADEHUB pair.
            env: vars,
            // The boot's ONE handle (no I/O yet): `hub_key_store` reads through it, and the arms that
            // open the store over a directory of their own take its scope.
            store: booted.store,
        }
    }
}

/// The NODE-key store — the `hub_key` table of `<project>/settings/db/vike.db`, narrowed to the
/// caller's own family. With no database there is no hub key.
///
/// A store that EXISTS but cannot be read is reported on **stderr** (never stdout — `mcp`'s stdout
/// is a protocol) and treated as EMPTY rather than fatal. Deliberate: the "no key" message this
/// then produces NAMES the store — so the operator is pointed at the same store twice, once by the
/// read error and once by the refusal. There is no silent-wrong-credential hazard to weigh against
/// that: a key that does not resolve cannot open a connection at all.
///
/// ⚠ The warning is emitted here rather than inside `vike-secrets` because that crate's reads log
/// nothing and return findings as DATA — the same division `permission_warning` already has. It goes to stderr, never stdout, because `vike-mcp`'s stdout is a protocol.
/// (`vike_secrets::CredentialRead::report` is NOT called: it writes through `tracing`, and this binary
/// builds no subscriber, so the finding is printed here, on stderr, as it always was.)
///
/// ⚠ It is printed ONCE PER RESOLUTION, not once per key. Five arms resolve keys and two pairs
/// exist; a per-key notice would put four identical lines in front of an operator who has one thing
/// to do.
///
/// ⚠ **`is_hub_key` is the CALLER's own family, never the four-name `is_platform_key`.**
/// `vike_secrets::CredentialStore::hub_keys` returns the names the predicate admits and no others,
/// so a process that
/// authenticates to one service never holds the other's key. Both of this function's callers name their own family (`vike_venues::credential_keys::is_tradehub_pair_key`
/// / `is_datahub_pair_key`).
///
/// It reads through the boot's own handle ([`Resolved::store`]), over the settings directory the
/// boot resolved; with no project resolved there is no location, and the read answers EMPTY (no
/// hub key), which is what the walk's relative `settings/` last resort answered on such a box.
fn hub_key_store(
    resolved: &Resolved,
    bin: &str,
    is_hub_key: impl Fn(&str) -> bool,
) -> HashMap<String, String> {
    match resolved.store.hub_keys(is_hub_key) {
        Ok(r) => {
            if let Some(w) = &r.warning {
                eprintln!("{bin}: ⚠ {w}");
            }
            r.secrets.into_map()
        }
        Err(e) => {
            eprintln!("{bin}: cannot read the hub-key store: {e}");
            HashMap::new()
        }
    }
}

/// Resolve the two vike-tradehub NODE KEYS for the surface about to run, from the settings
/// database's `hub_key` table — the store `vike-tradehub` itself loads them from, and the only
/// source (no process variable carries either key; both names are `vike_config::REMOVED_ENV` rows).
///
/// Called ONLY from the node-facing arms of the router — `trade` (the REPL, the three
/// group-less one-shot words `status`/`halt`/`resume`, and every one-shot verb under the
/// `order`/`position`/`strategy` groups) and `mcp`, plus the top-level read-only `report`. Every
/// READ among those — `trade status`, each group's own `ls`, and `report` — uses just the
/// keyring's observe half. Every other subcommand keeps the file unopened: the least credential
/// exposure that still fixes the defect.
///
/// ⚠ This used to name exactly TWO reads, `trade status` and `report`, and that was the whole set
/// the day it was written — before the trade-CLI-plane's group layer gave `order`/`position`/
/// `strategy` their own `ls` reads, each connecting the observe half only for the identical
/// reason. No count is written here any more for the same reason `vike-cli`'s `cmd::trade::plane`'s
/// group roster is not re-typed elsewhere: a fixed number rots the moment a group grows another
/// read.
pub fn hub_keyring(resolved: &Resolved, bin: &str) -> HubKeyring {
    // ⚠ The PATH handed on is the NODE store's — the settings database, the only store.
    // `crate::hubkeys::resolve` puts it in the "where would I have found this" message an operator
    // with NO key reads. `backend setup` writes the database; the refusal names the database; the
    // two mouths of this binary say one thing.
    let store = hub_key_store(resolved, bin, vike_venues::credential_keys::is_tradehub_pair_key);
    let store_path = resolved.settings_dir.as_ref().map(|d| vike_secrets::db_path_in(d));
    crate::hubkeys::resolve(&store, store_path.as_deref().map(Path::new))
        .with_patience(resolved.link_patience)
}

/// The DATAHUB node pair — `VIKE_DATAHUB_OBSERVE_KEY` / `VIKE_DATAHUB_CONTROL_KEY` — resolved from
/// the settings database's `hub_key` table ONLY. No process variable carries either key (both names
/// are `vike_config::REMOVED_ENV` rows, refused at startup).
///
/// ⚠ **This used to be a field resolved off the environment sweep alone, and that was the same
/// defect this file had already found and fixed for the TRADEHUB pair.** `crate::hubkeys`' module
/// doc records that one: both hub keys "used to read `std::env::var` and nothing else, so a
/// correctly-configured box answered 'nothing to do' and exited". The datahub pair was written to
/// the same shape afterwards, its doc comment claiming it worked "exactly as `hub_keyring` does" —
/// which it did not. MEASURED 2026-09-08 against a keyed datahub: the pair in the credential store
/// gave "no hub keys were supplied", and the identical pair exported into the environment served
/// 6.8 billion rows.
///
/// The worst part was the refusal text, not the resolution: `vike-datahub-client`'s message names
/// the credential store as the remedy, and following it exactly left an operator broken. A refusal
/// that names the wrong remedy is more expensive than one that names none.
///
/// ⚠ The store read is why this is a FUNCTION rather than a field: it opens the NODE-key store
/// (the settings database's `hub_key` table, narrowed to the datahub family — never the table of
/// venue keys), and only the arms that can
/// dial a datahub call it. That is a
/// deliberate widening of the rule `vike-cli`'s boot states — "a provenance or backtest command has no
/// business opening" the credential store — and the argument for it is that a `backtesthub --addr` IS a
/// node-facing invocation, the category the rule already excepts for `trade`/`mcp`. The narrower
/// alternative (read the store only when the invocation names a remote) was measured and REFUSED:
/// `cmd::data`'s address resolves to `vike_config::DEFAULT_DATAHUB_ADDRESS` whether or not `--addr`
/// was passed, and `vike_config::Config::datahub_addr` is a second way to be remote with no flag at
/// all, so "did this command go remote" cannot be answered from the argv the dispatcher holds.
pub fn datahub_keyring(resolved: &Resolved, bin: &str) -> Option<vike_hub_protocol::auth::HubKeys> {
    // ⚠ The NODE store, not the credential store: an arm needing a low-sensitivity key must not
    // open the table holding 168 venue secrets. It reads the `hub_key` table, the datahub family
    // only.
    let store = hub_key_store(resolved, bin, vike_venues::credential_keys::is_datahub_pair_key);
    vike_hub_protocol::auth::hub_keys_from_vars(&store)
}

/// Every non-fatal resolution the loader made, formatted for stderr — the PURE half of
/// [`Resolved::from_booted`]'s surfacing step.
///
/// A function rather than an inline loop because "the binary that loaded the settings SURFACES the
/// warnings, never swallows them" is a real property with a real failure mode, and a property worth
/// stating is worth gating — see `a_clamp_warning_is_surfaced_not_swallowed`.
pub fn settings_warning_lines(bin: &str, settings: &vike_config::Settings) -> Vec<String> {
    settings.warnings.iter().map(|w| format!("{bin}: settings: {w}")).collect()
}

/// Loads `<project>/user_data/indicators/*.rhai` and installs them process-wide, so a script run by
/// ANY subcommand can call them.
///
/// This is the composition root doing the I/O on the library's behalf — `vike_rhai`'s
/// `install_user_indicators` takes already-compiled prototypes precisely so that no library resolves
/// this directory for itself (its doc is the authority on why, and names `vike_log::init` as the
/// precedent). The load-plus-install pair is `vike_rhai::load_and_install_user_indicators`, which
/// is what a root wires WHEN it is wired — `git grep -l load_and_install_user_indicators -- crates` finds
/// every file that NAMES the pair — which is not the same as the set of callers: `vike-desktop`
/// appears there because it documents why it does NOT use it (it has TWO consumers of one load and
/// needs the report, not just the messages), and it is deliberately not written down here. ⚠ It is NOT yet every root that
/// compiles Rhai: `crates/vike-backtesthub/src/backtest_cli.rs` and its `cheap_np_*` siblings reach
/// the Rhai compile of `ProfileStrategy::build` (`crates/vike-backtest/src/harness/strategy_builder.rs`'s
/// `rhai_overrides`) and call none of this, so a user indicator does not resolve there. That is a
/// known gap, not a claim.
/// This function used to be a hand-written copy of the pair, and a copy per root is exactly how
/// they would come to disagree about which directory a user's indicators live in.
///
/// ⚠ **Every rejected file is REPORTED on stderr, and a bad one never fails the command.** From
/// inside a strategy, an indicator that failed to load is indistinguishable from a typo in the call
/// — both are function-not-found — so this is the only place that can say which. Aborting instead
/// would be worse: one unrelated half-edited indicator file would block every `vike-cli` invocation,
/// including the `init` that scaffolds the examples. **stderr, never stdout** — `vike-mcp`'s
/// stdout is a protocol, which is also why the messages come back as data rather than being printed
/// by the library that produced them.
pub fn install_user_indicators(bin: &str, user_data_dir: Option<&Path>) {
    let Some(dir) = user_data_dir else {
        return;
    };
    for message in vike_rhai::load_and_install_user_indicators(dir) {
        eprintln!("{bin}: {message}");
    }
}

/// The ONE wording of a verb refused while this box's settings seal is unsound
/// ([`Resolved::seal_refusal`], `why`). `command` is what the operator typed
/// (`vike-cli trade`), so the sentence reads as that command's own refusal.
pub fn seal_refusal_text(command: &str, why: &str) -> String {
    format!(
        "REFUSED: `{command}` will not run while this box's settings seal is \
         unsound.\n\n{why}\n\nThis verb is refused because it ACTS on the ceilings — it places \
         or gates orders — and the values this process resolved are not the ones the box was \
         sealed with. `vike-cli config check` still runs and reports the fault. There are no \
         settings FILES any more (docs/decisions/0086), so there is no `config compare` or \
         `config adopt --undo` to step back to.\n\n{}",
        vike_config::UNSOUND_STORE_REPAIR
    )
}

#[path = "resolved_tests.rs"]
#[cfg(test)]
mod resolved_tests;
