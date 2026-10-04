//! **Settings that USED to configure something and no longer do** — refused by name, never ignored.
//!
//! Two of them: [`refuse_removed_env`] for the environment variables that carried a risk ceiling,
//! and [`refuse_removed_project_file`] for the whole per-project override FILE. Same argument in
//! both cases, and it is the only argument this module makes.
//!
//! Phase 5 of the settings-unification design
//! (`docs/superpowers/specs/2026-08-04-settings-unification-design.md`) is the one phase that
//! changes behaviour: the risk ceilings move into [`crate::Policy`], and **their environment
//! overrides are deleted**. A ceiling you can raise from the environment is not a ceiling —
//! anyone who can set env on the box (a shell export, a stale systemd unit, a CI script, an
//! inherited parent process) silently widens the limit, with no file changed, no diff, no review,
//! and a run that looks completely normal.
//!
//! ## Why a removed variable must ERROR rather than be ignored
//!
//! Deleting the read is only half the job. The operator who set `VIKE_MAX_ORDER_NOTIONAL=250`
//! believes a 250-unit ceiling is active. If the new build simply stops reading it, that belief
//! becomes silently false and the process trades **uncapped** — strictly worse than either keeping
//! the variable or refusing to start. So a set-but-removed variable is a **startup refusal** that
//! names the exact file and key that replaces it, with the operator's own value rendered into a
//! copy-pasteable TOML line.
//!
//! This is the same argument [`crate::flags`] makes for rejecting a truthy typo, and the same one
//! [`crate::policy::PolicyPatch`]'s `deny_unknown_fields` makes for rejecting a mistyped key: a
//! setting that silently does nothing is the failure mode worth engineering against.
//!
//! ## What counts as "set"
//!
//! Present with a **non-empty** value after trimming. An empty (`VAR=`) or whitespace-only value
//! never configured anything under the old readers either — every one of them parsed the string as
//! `f64` and fell back to the permissive default — so nobody can believe a ceiling was active from
//! one, and refusing to start over a leftover blank line in a unit file would convert a harmless
//! artefact into an outage.
//!
//! ⚠ **That rule is a property of each row, not of the table — three rows are the exception, and
//! [`RemovedSetting::when_empty`] is where they say so.** For `POLY_SOCKS_PROXY` and
//! `POLY_WS_PROXY_ENABLED` an empty value was NOT the default: the deleted Polymarket resolver read
//! `POLY_SOCKS_PROXY=` as "connect DIRECT" on both lanes, and `POLY_WS_PROXY_ENABLED=` as "send the
//! WebSocket lanes direct" (only `1`/`true`/`yes`/`on` kept them on the tunnel). A unit that carries
//! either line is therefore an operator who chose to bypass the tunnel, and a build that skipped it
//! would start, read nothing, and dial the built-in SOCKS proxy at `127.0.0.1:1080` — a change of
//! egress with no error anywhere, which is exactly what this module exists to refuse. So those two
//! refuse EMPTY too, and say what the empty value meant and which row now says it. The third is
//! `POLY_REDEEM_HALT`, the auto-redeem kill switch: both of its readers halted on the variable being
//! PRESENT, so `POLY_REDEEM_HALT=` halted exactly as `=1` did, and skipping the blank line would drop
//! a halt in silence the day something starts the poller. It has no row to write — nothing starts
//! that poller — so its refusal says what the blank meant and prints no line. The rest of the table
//! keeps the rule above; `every_other_row_still_treats_an_empty_value_as_unset` pins the roster of
//! exceptions so a fourth one is a decision rather than a drift.
//!
//! ## I/O ownership
//!
//! The map is a PARAMETER, like everywhere else in this crate: the BINARY collects
//! `std::env::vars()` (and/or the workspace `.env`) and calls this before it starts anything. See
//! [`fn@crate::load`].
//!
//! ## The removed FILE — `<project>/vike.toml`
//!
//! [`REMOVED_PROJECT_FILE`] was a fifth settings file, sitting at the project ROOT and overriding
//! `[config]` and `[preferences]` from `<project>/settings/*.toml`. It is gone, and the reason is
//! the reason `<project>/settings/` exists at all: twelve PRs consolidated every setting,
//! credential and state file into ONE directory with four files of clear ownership, and a fifth
//! file one level ABOVE that directory reintroduces the question the consolidation removed — *which
//! file won?* That question is where real defects hide, which this crate's own history demonstrates
//! twice over ([`crate::consumed`] for a key nothing reads, and the layer itself, which was
//! implemented, tested and printed in an operator-facing precedence header while no binary read
//! one).
//!
//! ⚠ **Removing the reader is the dangerous half, exactly as it is for a variable.** The file was
//! genuinely wired for the hour between the change that wired it and the change that removed the
//! layer, so an operator may have one on disk that TOOK EFFECT. Ignoring it would make their belief
//! silently false — the identical failure mode `VIKE_MAX_ORDER_NOTIONAL` gets refused for. So a
//! present one is a startup REFUSAL naming the file and both destinations.
//!
//! ⚠ **And a path that cannot be PROBED refuses too — but says so, rather than claiming the file
//! is there.** Fail-closed is right (absence must be established, never assumed); a fail-closed
//! verdict dressed up as a positive finding is not. [`RemovedFileProbe`] carries which of the two
//! happened, and [`removed_project_file_message`] writes a different message for each.
//!
//! ⚠ **Nothing here deletes, moves or rewrites it.** It is the operator's file, holding their
//! keys; a loader that "helpfully" migrated it would be making an edit nobody reviewed. The message
//! says what to move where, and stops.
//!
//! Unlike [`refuse_removed_env`], this one is NOT a call a composition root has to remember: it is
//! folded into [`crate::load_with_cli`], so every binary that loads settings at all performs it.
//! That placement is deliberate and it is the lesson of the layer being removed — a root that can
//! forget a parameter can equally forget a call, and there is no reason to leave a fourth chance to
//! forget lying around.

use std::collections::HashMap;
use std::path::Path;

use crate::error::ConfigError;

/// The per-project override file that USED to sit above `<project>/settings/*.toml`.
///
/// Kept as a named constant even though nothing loads it any more: it is the string the refusal,
/// the gate in `crates/vike-config/tests/layers_are_reachable.rs` and
/// `crates/vike-cli/tests/settings_layers_reachable.rs` all have to agree on, and a tombstone
/// spelled once cannot drift from the message that names it.
pub const REMOVED_PROJECT_FILE: &str = "vike.toml";

/// Refuse to start when a `<project>/vike.toml` is present. `Ok(())` is the overwhelmingly common
/// case — nobody has one.
///
/// `settings_dir` is `<project>/settings`, so the project is its PARENT: one [`Path::parent`] call,
/// which resolves no directory of its own (this crate never walks, never expands `~`, never reads a
/// platform variable — see [`fn@crate::load`]). A `None` settings directory means there is no project
/// to look in, and a settings directory at a filesystem root has no parent; both skip the check,
/// because there is no file that could be misleading anybody.
///
/// ⚠ The probe is [`std::fs::metadata`] and only `NotFound` counts as absent. Any other error means
/// we could not ESTABLISH absence, and a file we cannot see is precisely the one an operator would
/// believe is in force — so it refuses and names the path, the same reasoning `read_toml` uses for
/// preferring a failed read over a prior `Path::exists()`.
///
/// ⚠ **Which is why the outcome is CARRIED rather than collapsed.** Fail-closed is the right
/// verdict and it is not changing; asserting *"this file is present"* on the strength of it is not.
/// See [`RemovedFileProbe`].
pub(crate) fn refuse_removed_project_file(settings_dir: Option<&Path>) -> Result<(), ConfigError> {
    let Some(project) = settings_dir.and_then(Path::parent) else { return Ok(()) };
    let file = project.join(REMOVED_PROJECT_FILE);
    match std::fs::metadata(&file) {
        Ok(_) => Err(ConfigError::RemovedProjectFile { file, probe: RemovedFileProbe::Present }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => {
            Err(ConfigError::RemovedProjectFile { file, probe: RemovedFileProbe::Unestablished(e) })
        }
    }
}

/// **What the stat actually established.** The two answers that are not `NotFound`, kept apart
/// because only ONE of them is a statement about a file that exists.
///
/// Both REFUSE — [`refuse_removed_project_file`] is fail-closed and stays that way, for the reason
/// its own doc gives. What they must not share is the message. Under a systemd unit's
/// `ProtectHome=yes` a stat of any path below `/home`, `/root` or `/run/user` returns **`EACCES`,
/// not `ENOENT`** (measured on the CI box), so a project root inside a user's home produced a hard
/// startup refusal that named a `vike.toml` **which did not exist** and told the operator to delete
/// it. Neither half of that is a cosmetic defect: it asserts something false, it names none of the
/// causes that actually produce it, and — because this check is layer 0 of [`crate::load_with_cli`]
/// — it fires before anything else in boot, so a false one MASKS whatever the real problem was.
///
/// The shape is `vike_data`'s `unwritable_store_root`, one crate over: a sandbox errno is decorated
/// with the unit directive that causes it and the directive that fixes it, rather than surfacing as
/// a bare `os error 13` pointing at the disk.
#[derive(Debug)]
pub enum RemovedFileProbe {
    /// [`std::fs::metadata`] SUCCEEDED, so something really is there under that name — a file, or
    /// a directory somebody created by mistake, which is at least as confusing. This is the answer
    /// that has earned the right to say *is present* and *delete it*.
    Present,
    /// The stat failed with something other than `NotFound`: absence was not established, and
    /// neither was presence.
    ///
    /// Carries the [`std::io::Error`] verbatim rather than a rendered string — the errno is the
    /// operator's first clue, and it is what [`std::error::Error::source`] hands a programmatic
    /// caller.
    Unestablished(std::io::Error),
}

/// The whole operator-facing refusal for a [`REMOVED_PROJECT_FILE`], ready to print — one message
/// per [`RemovedFileProbe`] answer.
///
/// Split from the error variant so the text lives beside the argument it makes, and so a test can
/// assert the message without constructing a filesystem.
pub(crate) fn removed_project_file_message(file: &Path, probe: &RemovedFileProbe) -> String {
    match probe {
        RemovedFileProbe::Present => present_project_file_message(file),
        RemovedFileProbe::Unestablished(e) => unestablished_project_file_message(file, e),
    }
}

/// The refusal for a file that IS there. Says so, and says what to do with it.
fn present_project_file_message(file: &Path) -> String {
    format!(
        "{path} is present, but the per-project override layer is NO LONGER READ — removed because \
         a fifth settings file above <project>/settings/ reintroduces the `which file won?` \
         question that directory exists to answer. There are no settings files to move its keys \
         into any more either (`docs/decisions/0086`): write each `[config]`/`[preferences]` key it \
         holds with `vike-cli config set config.<key> <value>` / `vike-cli config set \
         preferences.<key> <value>`, then delete {path}.\n\
         [policy] and [flags] tables were never accepted in it; write those with `vike-cli config \
         set policy.<key> <value>` / `vike-cli config set flags.<key> <value>`.\n\
         Nothing has been moved or deleted for you: it is your file.",
        path = file.display(),
    )
}

/// The refusal for a path that could not be PROBED. Same fail-closed verdict, and it claims
/// nothing at all about the file — because nothing is known about it.
///
/// The likeliest causes are named because the errno alone sends an operator hunting for a file that
/// is very probably not there: `ProtectHome=` and `ProtectSystem=` are the two unit directives that
/// turn "nothing here" into `Permission denied`, and `VIKE_SETTINGS_DIR` is the way to point the
/// process at a project it can actually see. The relocation instructions from
/// [`present_project_file_message`] are still offered, but conditionally — *if* it really is there.
fn unestablished_project_file_message(file: &Path, error: &std::io::Error) -> String {
    format!(
        "{path} could not be STATTED: {error} — this is NOT the same as absent, and it is NOT \
         evidence that the file is there. The per-project override layer is NO LONGER READ, and a \
         path whose absence cannot be ESTABLISHED refuses rather than being assumed empty: a file \
         nobody can see is precisely the one an operator would believe is in force.\n\
         ⚠ It may not exist at all. Under a systemd unit the likeliest cause is the sandbox rather \
         than a leftover file: ProtectHome=yes makes /home, /root and /run/user unreachable, so a \
         stat below them fails with `Permission denied` whether or not anything is there, and \
         ProtectSystem=strict does the same outside what ReadWritePaths= / ReadOnlyPaths= name. \
         Give the unit access to the project directory {project}, relax ProtectHome= to read-only, \
         or name a project the process can see with VIKE_SETTINGS_DIR. Otherwise it is ordinary \
         permissions: a parent directory this process cannot search (no `x` bit) fails identically.\n\
         If {path} really is there, it is the removed layer: write its `[config]`/`[preferences]` \
         keys with `vike-cli config set config.<key> <value>` / `vike-cli config set \
         preferences.<key> <value>`, then delete it.\n\
         Nothing has been moved or deleted for you.",
        path = file.display(),
        project = file.parent().unwrap_or(Path::new(".")).display(),
    )
}

/// How the operator's own value becomes the replacement key's value in the refusal's paste-ready
/// `vike-cli config set` line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueMap {
    /// A positive number (a ceiling): the operator's value, else a placeholder.
    PositiveNumber,
    /// The operator's value, verbatim.
    Verbatim,
    /// The exact-`1` switch grammar: `1` becomes `true`, anything else `false`.
    Switch,
    /// The value does NOT carry over: the line writes `value`, for an operator who means it, and
    /// `lead` says when.
    OnPurpose { value: &'static str, lead: &'static str },
    /// A secret: the line reads the value from stdin (`-`), and nothing prints the value.
    Stdin,
    /// The exact-`1` switch carried as `1`/`0` — an `ExactOne` venue field.
    ExactOne,
    /// [`Self::ExactOne`] for a variable whose old reader compared the UNTRIMMED value exactly —
    /// no trimming, no `#` comment. A padded or commented `1`/`0` never acted there: the process
    /// ran the default, so the refusal says that and offers the row only as what the operator may
    /// have MEANT (`unacted_spelling`).
    ExactOneUntrimmed,
    /// A boolean whose old reader treated anything but `false`/`0`/`no`/`off` as ON.
    TrueUnlessFalsey,
    /// A boolean whose old reader treated anything but `1`/`true`/`yes`/`on` as OFF.
    FalseUnlessTruthy,
    /// The first whitespace-separated token — what every Polymarket reader took (a trailing
    /// `# comment` is annotation).
    FirstToken,
    /// A list: the tokens the old reader took (commas and whitespace separate; `#` ends it), joined
    /// by commas so the pasted line carries one argument.
    List,
    /// ONE variable that became the SAME field on several venues and whose only effect was OFF at
    /// the exact, untrimmed `0` (`VIKE_MARK_STREAMS`). A `0` prints one `… 0` line for `key` and
    /// one for each of `also`; any other value configured nothing but the defaults and prints
    /// nothing to write. A padded or commented `0` is refused as [`Self::ExactOneUntrimmed`] is.
    KillSwitchEach { lead: &'static str, also: &'static [&'static str] },
}

/// What an EMPTY value of a retired variable used to MEAN — for the rows where that was not "the
/// default". See [`RemovedSetting::when_empty`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmptyMeaning {
    /// What the old reader did with `VAR=`, as a clause that completes "its old reader took it to
    /// mean …".
    pub meant: &'static str,
    /// The value that says the same thing where the setting lives now — written to
    /// [`RemovedSetting::key`], on stdin for a [`ValueMap::Stdin`] row. Must pass the target
    /// field's own grammar (`every_empty_meaning_passes_its_fields_grammar` holds it). EMPTY for a
    /// row with no key: there is no setting to write it to, and the refusal says so
    /// (`an_empty_meaning_writes_a_value_exactly_when_the_row_has_a_key` holds the pairing).
    pub write: &'static str,
}

/// One environment variable that has been REMOVED, and where its value lives now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemovedSetting {
    /// The variable that is no longer read.
    pub var: &'static str,
    /// The settings SECTION that held it, spelled as the file name it used to have (`policy.toml`
    /// for the `policy` section; `docs/decisions/0086` removed the files), or `secrets.env` for the
    /// credential store. Rendered only when [`Self::key`] is `None`: a `.toml` name renders as the
    /// section word, never as a path, and `secrets.env` renders the credential-store line. Empty
    /// when nothing in any settings section replaces it — the [`Self::why`] then says what does.
    pub file: &'static str,
    /// The FULL dotted key a `vike-cli config set` writes instead (`policy.max_notional_per_order`,
    /// `policy.venues.binance`, `flags.poly_exec`, `venue.polymarket.proxy_host`), when the VALUE
    /// moved.
    ///
    /// `None` when the variable configured something that no longer exists at all: [`Self::file`]
    /// still names where the answer comes from, but there is no line for the operator to paste.
    pub key: Option<&'static str>,
    /// How the operator's value is carried into the paste-ready line (unused when [`Self::key`] is
    /// `None`).
    pub value: ValueMap,
    /// Which change removed it — so the message dates itself and can be searched for.
    pub removed_in: &'static str,
    /// One sentence on WHY it was removed, printed with the refusal. An operator who is told only
    /// "this moved" will re-add the variable somewhere else; one who is told a ceiling must not be
    /// env-settable will not.
    pub why: &'static str,
    /// Whether the variable's VALUE may appear in the refusal.
    ///
    /// `false` for anything that is ITSELF a secret. A refusal that echoes one turns a startup
    /// error into a credential in every log that captured stderr, which is a worse outcome than the
    /// misconfiguration it is reporting. A number the operator has to re-type into a TOML line is
    /// the case for `true`.
    pub echo_value: bool,
    /// `Some` for a variable whose EMPTY value (`VAR=`, or only whitespace) meant something other
    /// than the default to its old reader — such a row is refused when empty too, with a message
    /// that says what the empty value meant. `None` (every other row): an empty value configured
    /// nothing, so it starts normally. See the module doc's "What counts as set".
    ///
    /// With a [`Self::key`], the refusal prints the line that says the same thing where the setting
    /// lives now ([`EmptyMeaning::write`]); without one there is no line to give, and it says that.
    pub when_empty: Option<EmptyMeaning>,
}

/// Why the four `{VENUE}_MAINNET` rows are refused — decision 0095.
const MAINNET_WHY: &str = "the arming ceiling alone chooses the network now — `live` means MAINNET — \
     and the settings store's migration rewrote this venue's `live` lines to `demo`, the network \
     they meant while this variable was unset; a box that DID set it traded mainnet and will not \
     again until the ceiling says so";

/// The lead line of a `{VENUE}_MAINNET` refusal's paste-ready command.
const MAINNET_ON_PURPOSE: &str = "To trade MAINNET on purpose (the venue's LIVE keys must be in the credential store), set its \
     ceiling instead";

/// Why the eleven Polymarket rows are refused — decision 0095.
const POLY_WHY: &str = "no environment variable configures a venue any more (decision 0095): this \
     setting is a row in the settings database, and a variable that still looked set would \
     silently configure nothing";

/// Why the six venue toggles are refused — decision 0095.
///
/// ⚠ It never spells the paste command itself: every line a refusal prints that starts with that
/// command is one a test counts, so the reason names the verb only as `config set`.
const VENUE_TOGGLE_WHY: &str = "no environment variable configures a venue any more (decision \
     0095): this setting is a row in the settings database, written with `config set` and \
     recorded in the change journal";

/// The lead line of the retired `VIKE_MARK_STREAMS` master's refusal.
const MARK_STREAMS_KILL_LEAD: &str = "The one switch became one field per venue. `0` — the only \
     value it ever acted on — keeps every venue's mark stream off; set each field instead";

/// The `removed_in` of the tool, smoke and unstarted-code rows (spec PR 5 of decision 0095).
const PR5_REMOVED_IN: &str = "decision 0095 (venues read no environment)";

/// D4 of decision 0095, for a variable whose only reader is code no composition root starts.
const NOTHING_STARTS: &str = "nothing starts the code it configured (no composition root spawns \
     it, and its entry point takes the value as a parameter now), so the variable was already \
     changing nothing — D4 of decision 0095";

/// [`NOTHING_STARTS`] for the one variable that was a KILL SWITCH: an operator told only "unset it"
/// reads "your halt is gone", so the refusal also names the switch that stays — the halt FILE the
/// poller checks every tick, which is not a variable and which this decision did not touch.
const REDEEM_HALT_WHY: &str = "nothing starts the auto-redeem poller it halted (no composition \
     root spawns it, and `AutoRedeemPoller::spawn` takes `halted` as a parameter now), so the \
     variable was already changing nothing — D4 of decision 0095. The poller's RUNTIME kill switch \
     is unchanged and is not a variable: the halt FILE its caller hands `AutoRedeemPoller::spawn`, \
     checked every tick";

/// Why `VIKE_HALT_FILE` is refused — decision 0099.
///
/// ⚠ It carries the whole answer, because the row has no key to paste and no settings file to name:
/// the sentinel's LOCATION stopped being a setting at all. Three things have to be in it. The file
/// is unchanged and so is how it is used (`touch`), so an operator who reads "removed" does not
/// conclude the kill switch went; the one path now, so they know what to `touch`; and why a stale
/// value is refused rather than ignored, because that is the sentence that stops a well-meaning
/// operator from "just" suppressing the refusal. It never spells the paste command, for the reason
/// [`VENUE_TOGGLE_WHY`] gives.
const HALT_FILE_WHY: &str = "the kill switch's FILE is unchanged but its location is no longer a \
     setting: it is always `<project>/settings/state/HALT`, in the state directory the daemon \
     booted with (`VIKE_SETTINGS_DIR` moves it together with the settings), and the daemon logs \
     the path it resolved at startup (`HALT sentinel path resolved`). A value left in a unit would \
     send an operator to `touch` a path nothing watches — a dead kill switch that reads exactly \
     like an armed one — so it refuses to start instead of being ignored";

/// The removed variables. Every entry is refused at startup by [`refuse_removed_env`].
///
/// The first two carried the SAME idea — a per-order notional ceiling — under two names, one for
/// the GUI (`vike-app`, via `vike_app_core::orders::order_entry::OrderLimits`, plus `vike-cli`'s advisory
/// client-side guardrail) and one for the headless daemon (`vike-tradehub`'s server-edge
/// `ControlLimitsConfig`). Both now read [`crate::Policy::max_notional_per_order`], which is the
/// point: one key, one file, one authority.
///
/// The last four carry decision 0095's retired `{VENUE}_MAINNET` switches: the ceiling
/// (`policy.venues.<venue>`) alone chooses the network now, and each row stays so a stale switch is
/// refused rather than silently doing nothing.
///
/// The Polymarket rows follow (decision 0095). Two of them — `POLY_SOCKS_PROXY` and
/// `POLY_WS_PROXY_ENABLED` — carry a [`RemovedSetting::when_empty`]: their blank value meant
/// "direct", so they refuse blank too.
///
/// The venue toggles close the table (decision 0095): `HYPERLIQUID_HIP3` and
/// `VIKE_ALLOW_WITHDRAW_KEYS` were the environment layer of two `flags.*` rows that no longer have
/// one, and `VIKE_BINANCE_TRADE_LITE_FILL`, `VIKE_BYBIT_FAST_EXEC`, `VIKE_MARK_STREAMS_ASTER` and
/// the `VIKE_MARK_STREAMS` master became `venue.*` fields. None of the six carries a `when_empty`:
/// every deleted reader took a blank value as unset
/// (`a_blank_retired_venue_toggle_starts_normally_because_blank_was_unset` names what each did).
/// The three fields carry [`ValueMap::ExactOneUntrimmed`] and the master
/// [`ValueMap::KillSwitchEach`]: their readers compared the untrimmed value exactly, so a padded
/// or commented `1`/`0` is refused as the default it ran, not as the row it spells.
/// ⚠ `VIKE_MARK_STREAMS_ASTER` stands BEFORE the master on purpose: the
/// refusal prints its blocks in this table's order, so an operator who had both set and pastes the
/// lines top to bottom ends on the master's `0` for aster — the master kill was absolute in the
/// deleted resolver (`the_master_kill_still_wins_when_both_mark_stream_variables_are_set`).
/// `VIKE_MARK_STREAMS_BINANCE` and its bybit/okx/hyperliquid twins are NOT rows: only aster's
/// per-venue spelling was ever read, so refusing one would stop a correct daemon over a spelling
/// that never did anything.
///
/// The tools, the smokes and the code nothing starts close the table (decision 0095's spec PR 5):
/// the five `ctrader_authorize` variables (three became flags, the app pair is read from the
/// credential store), the two egress-guard variables (the smokes pass constants), and ten read only
/// by code no composition root starts — the chain watcher's four, the auto-redeem pair, the
/// heartbeat, the resolve and outcome pollers, and the DVOL cadence (D4: each is a parameter of that
/// code now). Every one carries `key: None` and an empty `file`, and prints no `config set` line: the
/// five poller flags keep their `flags.*` rows, but nothing reads those rows, so pointing at one
/// would confirm something false. ⚠ That makes these refusals of variables a RUNNING process never
/// read — a deliberate departure from the "belief made false" argument above, taken so a leftover
/// variable is named rather than silently dropped the day something starts its code. Only
/// `POLY_REDEEM_HALT` carries a `when_empty` (its readers halted on presence).
pub const REMOVED_ENV: &[RemovedSetting] = &[
    RemovedSetting {
        var: "VIKE_MAX_ORDER_NOTIONAL",
        file: "policy.toml",
        key: Some("policy.max_notional_per_order"),
        value: ValueMap::PositiveNumber,
        removed_in: "Phase 5 (settings unification)",
        why: "an order-size ceiling any exported variable can raise is not a ceiling",
        echo_value: true,
        when_empty: None,
    },
    RemovedSetting {
        var: "VIKE_TRADEHUB_MAX_ORDER_NOTIONAL",
        file: "policy.toml",
        key: Some("policy.max_notional_per_order"),
        value: ValueMap::PositiveNumber,
        removed_in: "Phase 5 (settings unification)",
        why: "an order-size ceiling any exported variable can raise is not a ceiling",
        echo_value: true,
        when_empty: None,
    },
    RemovedSetting {
        // ⚠ The THIRD shape on this list, and the one to copy from when a key is deleted for being
        // UNREAD rather than for being an env-settable ceiling. `VIKE_STATE_DIR` named the
        // DESKTOP's strategy-state sidecar directory; `crates/vike-desktop/src/main.rs`'s
        // `state_dir_path` was its only reader and went with the desktop cut's local core, leaving
        // `config.state_dir` declared, validated, and reported by `vike-cli config show` as the
        // ORIGIN of an effective value that configured nothing. `crate::consumed`'s row for that
        // key had specified exactly this end state in writing since the reader was deleted.
        //
        // ⚠ `key: None` and `file: "config.toml"` together are doing something specific: the value
        // did NOT move, so there is no line to paste — but the operator asking "where do I set this
        // now" has to be told the answer is nowhere, in the section they were using, rather than
        // left to guess. The one thing they must NOT conclude is that `VIKE_STATE_ROOT` is the same
        // knob under a new name: that variable is the state ROOT, it is read by
        // vike-tradehub/vike-app-core/vike-studio, and `vike_model::paths::state_path`'s module doc
        // records the collision between the two names.
        //
        // ⚠ The six `flags.toml` keys deleted in the same change are deliberately NOT here — their
        // variables are still read by the venue adapters that own them, so refusing one would take
        // down a correct deployment. `crate::flags::REMOVED_FLAG_KEYS` is their (file-only)
        // tombstone and argues the split.
        var: "VIKE_STATE_DIR",
        file: "config.toml",
        key: None,
        value: ValueMap::Verbatim,
        removed_in: "the unread-settings sweep (settings unification)",
        why: "nothing reads it — its one reader, the desktop's strategy-state sidecar resolver, \
              went with the desktop's local core. ⚠ VIKE_STATE_ROOT is a DIFFERENT directory and \
              is not a replacement for it",
        // ⚠ `false`, and NOT because the value is a secret — a directory path is not one. This row
        // carries `key: None`, so the refusal offers no TOML line to paste, and
        // `only_rows_with_a_key_to_paste_echo_their_value` holds the pairing: a row with nothing to
        // paste has no reason to echo, and an echo with no line beside it reads as a suggestion
        // that the value should be moved somewhere. It should not be moved anywhere.
        echo_value: false,
        when_empty: None,
    },
    RemovedSetting {
        // ⚠ Same THIRD shape as `VIKE_STATE_DIR` above — a key deleted for having no reader left,
        // not an env-settable ceiling. It gated the live-feed tee `open_tradehub_recorder` in the
        // daemon's CLI — the writer that put quote/trade/book rows into the history store from
        // inside the order-signing process.
        //
        // ⚠ That function is GONE rather than moved, so the name above is deliberately NOT written
        // as a path-plus-symbol citation: there is no live site to point at, and
        // `crates/vike-ops/tests/citation_gate.rs` is right to refuse one — a citation whose file
        // still exists while the symbol it names does not is exactly the silent rot that gate was
        // written for, and it caught this comment's first draft. The bare name stays in prose
        // because it is the evidence for the claim around it.
        //
        // Both it and the journal materializer went with
        // `docs/decisions/0084-only-the-datahub-touches-the-store.md`: the store has one
        // writer plane, and the recorder that survives runs inside the datahub — with the watchdog,
        // the silence detection and the record profiles this tee never had.
        //
        // ⚠ `key: None` for the same reason that entry gives: the value did NOT move, so there is
        // no line to paste. An operator asking "where do I turn recording on now" is told the
        // answer is `vike-backend datahub --record-profile <name>`, in the OTHER daemon, rather
        // than left to guess — and the one thing they must not conclude is that the feature moved
        // under a new flag name here.
        //
        // ⚠ **That flag is spelled correctly TODAY and the owner has ruled it will be renamed** to
        // `--recorder-profile [NAME]`, with an optional value
        // (`docs/superpowers/specs/2026-09-22-data-realtime-record-design.md`, ruling 5 — SHAPE
        // ACCEPTED, nothing built). Named here so the rename finds this site: a refusal that points
        // an operator at a flag that no longer exists is the same defect as the one this row cures,
        // one layer along.
        //
        // ⚠ It belongs HERE rather than in `crate::flags::REMOVED_FLAG_KEYS` by that table's own
        // rule: those rows exist because their variables are STILL READ by the venue adapters that
        // own them, so refusing one would take down a correct box. This variable is read by nothing
        // after 0084, which is exactly the graduation condition that table names.
        var: "VIKE_TRADEHUB_RECORD",
        file: "flags.toml",
        key: None,
        value: ValueMap::Verbatim,
        removed_in: "0084 (only the datahub touches the store)",
        why: "the daemon stopped writing the store; the recorder that survives runs in the datahub",
        echo_value: false,
        when_empty: None,
    },
    RemovedSetting {
        // ⚠ The SIBLING of the row above, and it dies of the same change one step removed.
        // `VIKE_TRADEHUB_RECORD` gated the live-feed tee; this named the DIRECTORY that tee wrote
        // into. With the tee gone its reader — `tick_store_root` — went too, and it was the LAST
        // reader anywhere: the desktop's had already gone with the GUI's local tick plane. So an
        // operator who sets this now configures NOTHING, silently, while believing they have
        // directed where ticks land. That is the whole reason this table exists.
        //
        // ⚠ **The refusal was withheld until it was MEASURED, because it fails a daemon's
        // startup.** A refusal for a variable somebody actually sets is an outage, not a
        // correction. Swept on the live box 2026-09-22 before adding this row: not in any unit's
        // `Environment=`, not in `vike-tradehub`'s `EnvironmentFile` (`<project>/.env`), nowhere
        // under `/etc/systemd`, `/etc/environment`, `/etc/profile*` or `/etc/default`, in no shell
        // profile, and — the check that actually settles it — in the real `/proc/<pid>/environ` of
        // all three running daemons, which carry it zero times. No tracked `deploy/` unit or
        // `settings/*.toml` names it either. The only hits on disk were the string compiled INTO
        // the shipped binaries by this very registry.
        //
        // ⚠ `key: None`, so `echo_value: false` (`only_rows_with_a_key_to_paste_echo_their_value`
        // holds the pairing): the value did not MOVE to a file key, so there is no line to paste.
        // Where ticks land is the datahub's question now, and its store is named by
        // `VIKE_DATAHUB_STORE`.
        var: "VIKE_TICK_STORE",
        file: "config.toml",
        key: None,
        value: ValueMap::Verbatim,
        removed_in: "0084 (only the datahub touches the store)",
        why: "its last reader went with the daemon's live-feed tee; the datahub names its own store",
        echo_value: false,
        when_empty: None,
    },
    RemovedSetting {
        var: "VIKE_SECRETS_PASSPHRASE",
        file: "secrets.env",
        key: None,
        value: ValueMap::Verbatim,
        removed_in: "the one-store change (settings unification)",
        why: "nothing consumes it — credentials are read from the project's own store, in plaintext",
        echo_value: false,
        when_empty: None,
    },
    RemovedSetting {
        var: "BINANCE_MAINNET",
        file: "policy.toml",
        key: Some("policy.venues.binance"),
        value: ValueMap::OnPurpose { value: "live", lead: MAINNET_ON_PURPOSE },
        removed_in: "decision 0095 (a `live` ceiling means mainnet)",
        why: MAINNET_WHY,
        echo_value: true,
        when_empty: None,
    },
    RemovedSetting {
        var: "BYBIT_MAINNET",
        file: "policy.toml",
        key: Some("policy.venues.bybit"),
        value: ValueMap::OnPurpose { value: "live", lead: MAINNET_ON_PURPOSE },
        removed_in: "decision 0095 (a `live` ceiling means mainnet)",
        why: MAINNET_WHY,
        echo_value: true,
        when_empty: None,
    },
    RemovedSetting {
        var: "OKX_MAINNET",
        file: "policy.toml",
        key: Some("policy.venues.okx"),
        value: ValueMap::OnPurpose { value: "live", lead: MAINNET_ON_PURPOSE },
        removed_in: "decision 0095 (a `live` ceiling means mainnet)",
        why: MAINNET_WHY,
        echo_value: true,
        when_empty: None,
    },
    RemovedSetting {
        var: "HYPERLIQUID_MAINNET",
        file: "policy.toml",
        key: Some("policy.venues.hyperliquid"),
        value: ValueMap::OnPurpose { value: "live", lead: MAINNET_ON_PURPOSE },
        removed_in: "decision 0095 (a `live` ceiling means mainnet)",
        why: MAINNET_WHY,
        echo_value: true,
        when_empty: None,
    },
    RemovedSetting {
        var: "POLY_EXEC",
        file: "the settings database",
        key: Some("flags.poly_exec"),
        value: ValueMap::Switch,
        removed_in: "decision 0095 (venues read no environment)",
        why: POLY_WHY,
        echo_value: true,
        when_empty: None,
    },
    RemovedSetting {
        var: "POLY_RECONCILE",
        file: "the settings database",
        key: Some("flags.poly_reconcile"),
        value: ValueMap::Switch,
        removed_in: "decision 0095 (venues read no environment)",
        why: POLY_WHY,
        echo_value: true,
        when_empty: None,
    },
    RemovedSetting {
        var: "POLY_PROXY_ENABLED",
        file: "the settings database",
        key: Some("venue.polymarket.proxy_enabled"),
        value: ValueMap::TrueUnlessFalsey,
        removed_in: "decision 0095 (venues read no environment)",
        why: POLY_WHY,
        echo_value: true,
        when_empty: None,
    },
    RemovedSetting {
        var: "POLY_PROXY_HOST",
        file: "the settings database",
        key: Some("venue.polymarket.proxy_host"),
        value: ValueMap::FirstToken,
        removed_in: "decision 0095 (venues read no environment)",
        why: POLY_WHY,
        echo_value: true,
        when_empty: None,
    },
    RemovedSetting {
        var: "POLY_PROXY_PORT",
        file: "the settings database",
        key: Some("venue.polymarket.proxy_port"),
        value: ValueMap::FirstToken,
        removed_in: "decision 0095 (venues read no environment)",
        why: POLY_WHY,
        echo_value: true,
        when_empty: None,
    },
    RemovedSetting {
        var: "POLY_SOCKS_PROXY",
        file: "the settings database",
        key: Some("venue.polymarket.socks_proxy"),
        value: ValueMap::Stdin,
        removed_in: "decision 0095 (venues read no environment)",
        why: POLY_WHY,
        echo_value: false,
        // The deleted resolver read `POLY_SOCKS_PROXY=` as the `none`/`direct` sentinel — no proxy
        // on EITHER lane — so an empty line here is an operator who chose to bypass the tunnel.
        when_empty: Some(EmptyMeaning {
            meant: "connect DIRECT on both lanes (REST and WebSocket) — the same as `direct`",
            write: "direct",
        }),
    },
    RemovedSetting {
        var: "POLY_WS_PROXY_ENABLED",
        file: "the settings database",
        key: Some("venue.polymarket.ws_proxy_enabled"),
        value: ValueMap::FalseUnlessTruthy,
        removed_in: "decision 0095 (venues read no environment)",
        why: POLY_WHY,
        echo_value: true,
        // Any value but `1`/`true`/`yes`/`on` — an empty one included — sent the WebSocket lanes
        // DIRECT while REST kept its tunnel; only the truthy spellings left them on it.
        when_empty: Some(EmptyMeaning {
            meant: "send the WebSocket lanes DIRECT while REST kept its tunnel (only `1`, `true`, \
                    `yes` or `on` left them on it)",
            write: "false",
        }),
    },
    RemovedSetting {
        var: "POLY_RATE_GATE",
        file: "the settings database",
        key: Some("venue.polymarket.rate_gate"),
        value: ValueMap::ExactOne,
        removed_in: "decision 0095 (venues read no environment)",
        why: POLY_WHY,
        echo_value: true,
        when_empty: None,
    },
    RemovedSetting {
        var: "POLY_EXEC_MARKETS",
        file: "the settings database",
        key: Some("venue.polymarket.exec_markets"),
        value: ValueMap::List,
        removed_in: "decision 0095 (venues read no environment)",
        why: POLY_WHY,
        echo_value: true,
        when_empty: None,
    },
    RemovedSetting {
        var: "POLY_PRESUBMIT_REGISTER",
        file: "the settings database",
        key: Some("venue.polymarket.presubmit_register"),
        value: ValueMap::ExactOne,
        removed_in: "decision 0095 (venues read no environment)",
        why: POLY_WHY,
        echo_value: true,
        when_empty: None,
    },
    RemovedSetting {
        var: "POLY_WS_TOKENS_PER_SOCKET",
        file: "the settings database",
        key: Some("venue.polymarket.ws_tokens_per_socket"),
        value: ValueMap::FirstToken,
        removed_in: "decision 0095 (venues read no environment)",
        why: POLY_WHY,
        echo_value: true,
        when_empty: None,
    },
    RemovedSetting {
        var: "HYPERLIQUID_HIP3",
        file: "the settings database",
        key: Some("flags.hyperliquid_hip3"),
        value: ValueMap::Switch,
        removed_in: "decision 0095 (venues read no environment)",
        why: VENUE_TOGGLE_WHY,
        echo_value: true,
        when_empty: None,
    },
    RemovedSetting {
        var: "VIKE_ALLOW_WITHDRAW_KEYS",
        file: "the settings database",
        key: Some("flags.allow_withdraw_keys"),
        value: ValueMap::Switch,
        removed_in: "decision 0095 (venues read no environment)",
        why: VENUE_TOGGLE_WHY,
        echo_value: true,
        when_empty: None,
    },
    RemovedSetting {
        var: "VIKE_BINANCE_TRADE_LITE_FILL",
        file: "the settings database",
        key: Some("venue.binance.trade_lite_fill"),
        value: ValueMap::ExactOneUntrimmed,
        removed_in: "decision 0095 (venues read no environment)",
        why: VENUE_TOGGLE_WHY,
        echo_value: true,
        when_empty: None,
    },
    RemovedSetting {
        var: "VIKE_BYBIT_FAST_EXEC",
        file: "the settings database",
        key: Some("venue.bybit.fast_exec"),
        value: ValueMap::ExactOneUntrimmed,
        removed_in: "decision 0095 (venues read no environment)",
        why: VENUE_TOGGLE_WHY,
        echo_value: true,
        when_empty: None,
    },
    // ⚠ BEFORE the master below, and the order is the argument — see this table's doc.
    RemovedSetting {
        var: "VIKE_MARK_STREAMS_ASTER",
        file: "the settings database",
        key: Some("venue.aster.mark_streams"),
        value: ValueMap::ExactOneUntrimmed,
        removed_in: "decision 0095 (venues read no environment)",
        why: VENUE_TOGGLE_WHY,
        echo_value: true,
        when_empty: None,
    },
    RemovedSetting {
        var: "VIKE_MARK_STREAMS",
        file: "the settings database",
        key: Some("venue.binance.mark_streams"),
        value: ValueMap::KillSwitchEach {
            lead: MARK_STREAMS_KILL_LEAD,
            also: &[
                "venue.bybit.mark_streams",
                "venue.okx.mark_streams",
                "venue.hyperliquid.mark_streams",
                "venue.aster.mark_streams",
            ],
        },
        removed_in: "decision 0095 (venues read no environment)",
        why: VENUE_TOGGLE_WHY,
        echo_value: true,
        when_empty: None,
    },
    // --- the tools, the smokes and the code nothing starts (spec PR 5) ---------------------------
    //
    // No key and no file: the replacement is a command-line flag, a credential-store row, a
    // smoke's constant, or a PARAMETER of code no composition root starts (D4), and the `why`
    // carries the whole answer. Five of them used to feed `flags.*` rows that still exist; those
    // rows are read by nothing (`crate::CONSUMPTION`), so a `config set` line would confirm
    // something false. None echoes its value: `CTRADER_CLIENT_SECRET` is a secret and
    // `POLY_CHAIN_RPC_URL` can carry an endpoint's key.
    //
    // The cTrader five were read only by the `ctrader_authorize` tool, which boots nothing — so it
    // does not reach this table — and refuses the same five itself, out of its own sweep
    // (`crates/bridges/ctrader/src/bin/ctrader_authorize.rs`'s `refuse_retired_variables`). The two
    // app-pair rows name `vike-cli secrets set` as coming AFTER the unset this refusal's tail asks
    // for, because `vike-cli` itself refuses to start while one is set.
    RemovedSetting {
        var: "CTRADER_REDIRECT_URI",
        file: "",
        key: None,
        value: ValueMap::Verbatim,
        removed_in: PR5_REMOVED_IN,
        why: "the ctrader_authorize tool takes it on its command line: pass --redirect-uri <URI>",
        echo_value: false,
        when_empty: None,
    },
    RemovedSetting {
        var: "CTRADER_SCOPE",
        file: "",
        key: None,
        value: ValueMap::Verbatim,
        removed_in: PR5_REMOVED_IN,
        why: "the ctrader_authorize tool takes it on its command line: pass --scope <SCOPE>",
        echo_value: false,
        when_empty: None,
    },
    RemovedSetting {
        var: "CTRADER_TOKEN_FILE",
        file: "",
        key: None,
        value: ValueMap::Verbatim,
        removed_in: PR5_REMOVED_IN,
        why: "the ctrader_authorize tool takes it on its command line: pass --token-file <PATH>",
        echo_value: false,
        when_empty: None,
    },
    RemovedSetting {
        var: "CTRADER_CLIENT_ID",
        file: "",
        key: None,
        value: ValueMap::Verbatim,
        removed_in: PR5_REMOVED_IN,
        why: "the ctrader_authorize tool reads the app pair from the credential store, the rows \
              every cTrader mount reads; `vike-cli secrets set CTRADER_CLIENT_ID` writes it there \
              (the value on stdin) — run that after the unset below, because vike-cli refuses to \
              start while the variable is set",
        echo_value: false,
        when_empty: None,
    },
    RemovedSetting {
        var: "CTRADER_CLIENT_SECRET",
        file: "",
        key: None,
        value: ValueMap::Verbatim,
        removed_in: PR5_REMOVED_IN,
        why: "the ctrader_authorize tool reads the app pair from the credential store, the rows \
              every cTrader mount reads; `vike-cli secrets set CTRADER_CLIENT_SECRET` writes it \
              there (the value on stdin) — run that after the unset below, because vike-cli \
              refuses to start while the variable is set",
        echo_value: false,
        when_empty: None,
    },
    RemovedSetting {
        var: "POLY_EGRESS_PROBE_URL",
        file: "",
        key: None,
        value: ValueMap::Verbatim,
        removed_in: PR5_REMOVED_IN,
        why: "the egress guard takes its probe URL as a parameter; the polymarket smokes pass \
              vike_polymarket::DEFAULT_EGRESS_PROBE",
        echo_value: false,
        when_empty: None,
    },
    RemovedSetting {
        var: "POLY_EXPECT_EGRESS_COUNTRY",
        file: "",
        key: None,
        value: ValueMap::Verbatim,
        removed_in: PR5_REMOVED_IN,
        why: "the egress guard takes the expected country as a parameter; the order-placing \
              polymarket smokes pass vike_polymarket::DUBLIN_EGRESS_COUNTRY, the read-only ones \
              none",
        echo_value: false,
        when_empty: None,
    },
    RemovedSetting {
        var: "POLY_CHAIN_WATCH",
        file: "",
        key: None,
        value: ValueMap::Verbatim,
        removed_in: PR5_REMOVED_IN,
        why: NOTHING_STARTS,
        echo_value: false,
        when_empty: None,
    },
    RemovedSetting {
        var: "POLY_CHAIN_RPC_URL",
        file: "",
        key: None,
        value: ValueMap::Verbatim,
        removed_in: PR5_REMOVED_IN,
        why: NOTHING_STARTS,
        echo_value: false,
        when_empty: None,
    },
    RemovedSetting {
        var: "POLY_CHAIN_MAX_SPAN",
        file: "",
        key: None,
        value: ValueMap::Verbatim,
        removed_in: PR5_REMOVED_IN,
        why: NOTHING_STARTS,
        echo_value: false,
        when_empty: None,
    },
    RemovedSetting {
        var: "POLY_CHAIN_PROXY",
        file: "",
        key: None,
        value: ValueMap::Verbatim,
        removed_in: PR5_REMOVED_IN,
        why: NOTHING_STARTS,
        echo_value: false,
        when_empty: None,
    },
    RemovedSetting {
        var: "POLY_AUTO_REDEEM",
        file: "",
        key: None,
        value: ValueMap::Verbatim,
        removed_in: PR5_REMOVED_IN,
        why: NOTHING_STARTS,
        echo_value: false,
        when_empty: None,
    },
    RemovedSetting {
        var: "POLY_REDEEM_HALT",
        file: "",
        key: None,
        value: ValueMap::Verbatim,
        removed_in: PR5_REMOVED_IN,
        why: REDEEM_HALT_WHY,
        echo_value: false,
        // Both readers — the poller's kill switch and `Flags::apply_env` — halted on PRESENCE, so
        // an empty line halted exactly as `=1` did. Nothing starts the poller, so there is no row
        // to write: the refusal says what the blank meant and prints no line.
        when_empty: Some(EmptyMeaning {
            meant: "HALTED the auto-redeem poller — both of its readers tested PRESENCE, so \
                    `POLY_REDEEM_HALT=` halted exactly as `=1` did",
            write: "",
        }),
    },
    RemovedSetting {
        var: "POLY_HEARTBEAT",
        file: "",
        key: None,
        value: ValueMap::Verbatim,
        removed_in: PR5_REMOVED_IN,
        why: NOTHING_STARTS,
        echo_value: false,
        when_empty: None,
    },
    RemovedSetting {
        var: "VIKE_PM_RESOLVE",
        file: "",
        key: None,
        value: ValueMap::Verbatim,
        removed_in: PR5_REMOVED_IN,
        why: NOTHING_STARTS,
        echo_value: false,
        when_empty: None,
    },
    RemovedSetting {
        var: "VIKE_HL_OUTCOME",
        file: "",
        key: None,
        value: ValueMap::Verbatim,
        removed_in: PR5_REMOVED_IN,
        why: NOTHING_STARTS,
        echo_value: false,
        when_empty: None,
    },
    RemovedSetting {
        var: "VIKE_RECORD_DVOL_CADENCE_MS",
        file: "",
        key: None,
        value: ValueMap::Verbatim,
        removed_in: PR5_REMOVED_IN,
        why: NOTHING_STARTS,
        echo_value: false,
        when_empty: None,
    },
    // --- the HALT sentinel's path (decision 0099) -------------------------------------------------
    //
    // The one row here that retires a KILL SWITCH's configuration rather than a ceiling or a venue
    // toggle, and the reason it REFUSES where a quiet ignore would have been tidier. The old
    // resolver took `VIKE_HALT_FILE` first, so a unit that set it named the file an operator
    // `touch`ed; a build that merely stopped reading it would leave that operator `touch`ing a path
    // nothing watches. `key: None`, `file: ""` and `echo_value: false` for the reasons the
    // other key-less rows give (`only_rows_with_a_key_to_paste_echo_their_value`); no `when_empty`,
    // because the old resolver trimmed the value and fell through on a blank one, so a blank line
    // says nothing the default does not. Its settings-registry row moved here from
    // `vike-bridge-core` (a ratchet shrink) and the `vike-paper` test row left with the test that
    // read it.
    RemovedSetting {
        var: "VIKE_HALT_FILE",
        file: "",
        key: None,
        value: ValueMap::Verbatim,
        removed_in: "decision 0099 (the HALT sentinel's path is data)",
        why: HALT_FILE_WHY,
        echo_value: false,
        when_empty: None,
    },
];

/// Refuse to start when a REMOVED variable is set. `Ok(())` is the overwhelmingly common case.
///
/// "Set" means non-empty after trimming, EXCEPT for the rows that carry a
/// [`RemovedSetting::when_empty`]: an empty `POLY_SOCKS_PROXY` meant "connect direct", so it refuses
/// too (with a message about what the blank line used to do) — the module doc's "What counts as
/// set" argues why. Both the daemons' boots and the deploy pre-flight
/// (`vike-cli config retired-env`) call this one function, so they cannot disagree about WHICH
/// variables refuse — or about the message: the pre-flight's
/// `crates/vike-cli/src/cmd/config_retired_env.rs`'s `judge` hands it the value as the process
/// would receive it, padding included, so a `" 1"` gets `unacted_spelling`'s block from both. (It
/// used to trim first and print the ordinary line the boot withholds; that file's
/// `the_pre_flight_prints_exactly_what_the_daemons_boot_prints` holds the two equal over every row
/// and every spelling.) What the pre-flight can still miss is a value that never reaches it — the
/// deploy helper's own `Environment=` tokenization — which is the helper's, not this function's.
///
/// The `Err` string is the whole operator-facing message, ready to print: one block per offending
/// variable, each naming the variable, the file, the key, and the exact line to write. Every
/// offender is reported in ONE pass — fixing a stale unit file one restart at a time is a worse
/// experience than being handed the full list.
///
/// `String` rather than [`crate::ConfigError`] on purpose: every `ConfigError` variant is shaped to
/// name a file that was read or a layer that failed, and this failure is neither — nothing was
/// read, and the thing to fix is the process environment.
pub fn refuse_removed_env(vars: &HashMap<String, String>) -> Result<(), String> {
    // `(row, the value as the process received it, that value trimmed)`.
    let offenders: Vec<(&RemovedSetting, &str, &str)> = REMOVED_ENV
        .iter()
        .filter_map(|r| {
            let full = vars.get(r.var)?.as_str();
            let raw = full.trim();
            // An empty value configured nothing — EXCEPT where the row says the old reader gave it a
            // meaning (`when_empty`): there, skipping it would change egress with no error.
            if raw.is_empty() && r.when_empty.is_none() {
                return None;
            }
            Some((r, full, raw))
        })
        .collect();
    if offenders.is_empty() {
        return Ok(());
    }

    let mut out = String::new();
    for (r, full, raw) in &offenders {
        if !out.is_empty() {
            out.push('\n');
        }
        // A spelling an exact-match old reader never acted on has its own message: the process
        // ran the default, which is not what the ordinary line below would write.
        if let Some(block) = unacted_spelling(r, full, raw, vars) {
            out.push_str(&block);
            continue;
        }
        let (var, file) = (r.var, r.file);
        // The EMPTY-but-meaningful case has its own message: there is no value to echo or map, and
        // the point is what the blank line used to DO.
        if let (true, Some(empty), Some(key)) = (raw.is_empty(), r.when_empty, r.key) {
            let write = shell_word(empty.write);
            let line = match r.value {
                // A secret field takes its value on stdin only (`config set` refuses it on argv).
                ValueMap::Stdin => format!("printf {write} | vike-cli config set {key} -"),
                _ => format!("vike-cli config set {key} {write}"),
            };
            out.push_str(&format!(
                "{var} is set to an EMPTY value, but {var} is NO LONGER READ — removed in \
                 {removed_in}, because {why}.\n\
                 An empty value was NOT \"unset\" for this variable: its old reader took it to \
                 mean {meant}. Nothing reads it now, so this process would fall back to the \
                 built-in default instead — with no error anywhere. Say the same thing where the \
                 setting lives now:\n\n    {line}\n\nthen unset {var}.\n",
                removed_in = r.removed_in,
                why = r.why,
                meant = empty.meant,
            ));
            continue;
        }
        // …and the same case with NO key: the blank meant something, and nothing takes its place.
        if let (true, Some(empty), None) = (raw.is_empty(), r.when_empty, r.key) {
            out.push_str(&format!(
                "{var} is set to an EMPTY value, but {var} is NO LONGER READ — removed in \
                 {removed_in}, because {why}.\n\
                 An empty value was NOT \"unset\" for this variable: its old reader took it to \
                 mean {meant}. No setting takes its place, so there is nothing to write.\n\n\
                 then unset {var}.\n",
                removed_in = r.removed_in,
                why = r.why,
                meant = empty.meant,
            ));
            continue;
        }
        // The value is echoed only where the row says it may be — see `RemovedSetting::echo_value`.
        let setting = if r.echo_value { format!("{var}={raw}") } else { var.to_string() };
        out.push_str(&format!(
            "{setting} is set, but {var} is NO LONGER READ — removed in {removed_in}, \
             because {why}.\n",
            removed_in = r.removed_in,
            why = r.why,
        ));
        match r.key {
            Some(key) => {
                let (lead, value) = match r.value {
                    ValueMap::PositiveNumber => ("Set it instead", toml_value(raw)),
                    ValueMap::Verbatim => ("Set it instead", raw.to_string()),
                    ValueMap::Switch => (
                        "Set it instead",
                        (raw.split('#').next().unwrap_or("").trim() == "1").to_string(),
                    ),
                    ValueMap::OnPurpose { value, lead } => (lead, value.to_string()),
                    ValueMap::Stdin => (
                        "Set it instead, with the value on stdin (never on the command line)",
                        "-".to_string(),
                    ),
                    // An exact spelling, or one whose first token is not `1`/`0`: the padded and
                    // commented `1`/`0` of an untrimmed reader never get here (`unacted_spelling`).
                    ValueMap::ExactOne | ValueMap::ExactOneUntrimmed => (
                        "Set it instead",
                        if first_token(raw) == "1" { "1" } else { "0" }.to_string(),
                    ),
                    ValueMap::TrueUnlessFalsey => (
                        "Set it instead",
                        (!matches!(
                            first_token(raw).to_ascii_lowercase().as_str(),
                            "false" | "0" | "no" | "off"
                        ))
                        .to_string(),
                    ),
                    ValueMap::FalseUnlessTruthy => (
                        "Set it instead",
                        matches!(
                            first_token(raw).to_ascii_lowercase().as_str(),
                            "1" | "true" | "yes" | "on"
                        )
                        .to_string(),
                    ),
                    ValueMap::FirstToken => ("Set it instead", shell_word(first_token(raw))),
                    ValueMap::List => ("Set it instead", shell_word(&list_value(raw))),
                    ValueMap::KillSwitchEach { lead, .. } => {
                        (lead, if first_token(raw) == "0" { "0" } else { "" }.to_string())
                    }
                };
                // A per-venue split names every key it became; every other row names its one, and
                // renders byte-identically to before the split existed.
                let also: &[&str] = match r.value {
                    ValueMap::KillSwitchEach { also, .. } => also,
                    _ => &[],
                };
                let keys = std::iter::once(key).chain(also.iter().copied());
                if value.is_empty() {
                    let named = keys.map(|k| format!("`{k}`")).collect::<Vec<_>>().join(", ");
                    out.push_str(&format!(
                        "Its value configured only the default, so there is nothing to write for \
                         {named}.\n\n"
                    ));
                } else {
                    out.push_str(&format!("{lead}:\n\n"));
                    for k in keys {
                        out.push_str(&format!("    vike-cli config set {k} {value}\n"));
                    }
                    out.push('\n');
                }
            }
            // A row with NO file and NO key has no settings home at all — the replacement is a flag,
            // a store row, or nothing (D4), and its `why` carries the whole answer.
            None if file.is_empty() => {}
            // A SETTINGS section (`file` ends `.toml`): there is no settings file to name any more
            // (`docs/decisions/0086`), and the key was deleted rather than moved, so there is no row
            // to name in its place either — say so, naming the SECTION word rather than a path.
            None if file.ends_with(".toml") => {
                let section = file.trim_end_matches(".toml");
                out.push_str(&format!(
                    "nothing in the settings database's `{section}` section reads it any more — \
                     the setting was deleted, not moved.\n\n"
                ));
            }
            // `secrets.env` is a credential file, not a settings TOML, and remains real: it is
            // still where a credential-store-less box reads keys from.
            None => {
                out.push_str(&format!(
                    "<project>/settings/{file} is the only file consulted for this on a box that \
                     has not migrated to the settings database; once \
                     <project>/settings/db/vike.db exists, the database answers instead.\n\n"
                ));
            }
        }
        out.push_str(&format!("then unset {var}.\n"));
    }
    Err(out)
}

/// The refusal for a spelling an exact-match old reader never acted on — `None` for every other
/// row and value, which keep the ordinary message.
///
/// [`ValueMap::ExactOneUntrimmed`] and [`ValueMap::KillSwitchEach`] rows had readers that compared
/// the UNTRIMMED value with `1`/`0`. So `" 1"`, `"1 # x"` or `"0 # x"` — a first token of `1`/`0`
/// that is not the whole value — ran the DEFAULT. The ordinary line would write what the operator
/// probably meant, which would change what the box does. This message shows the value as the
/// process received it, names the default that ran, makes writing nothing the way to keep it, and
/// prints the row only under "If you MEANT". For the master, a `1` means only "the defaults", so
/// it offers no row at all.
///
/// `vars` is the whole environment being judged. A venue whose OWN retired variable is also set
/// (`VIKE_MARK_STREAMS_ASTER` beside the master) did not run its default, so the master's block
/// names that variable for it instead of claiming a state.
fn unacted_spelling(
    r: &RemovedSetting,
    full: &str,
    raw: &str,
    vars: &HashMap<String, String>,
) -> Option<String> {
    let key = r.key?;
    let (also, meant): (&[&str], Option<&str>) = match (r.value, first_token(raw)) {
        (_, token) if full == token || !matches!(token, "0" | "1") => return None,
        (ValueMap::ExactOneUntrimmed, token) => (&[], Some(token)),
        (ValueMap::KillSwitchEach { also, .. }, token) => (also, (token == "0").then_some("0")),
        _ => return None,
    };
    let keys: Vec<&str> = std::iter::once(key).chain(also.iter().copied()).collect();
    // Another retired variable that writes this key and is set too: it decided that venue.
    let own_variable = |k: &str| {
        REMOVED_ENV
            .iter()
            .find(|o| {
                o.var != r.var
                    && o.key == Some(k)
                    && vars.get(o.var).is_some_and(|v| !v.trim().is_empty())
            })
            .map(|o| o.var)
    };
    let defaults = keys
        .iter()
        .map(|k| {
            let (venue, word) = declared_default(k);
            match (keys.len() > 1, own_variable(k)) {
                (true, Some(other)) => format!("{venue} per `{other}`"),
                (true, None) => format!("{venue} {word}"),
                (false, _) => word.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    let var = r.var;
    // The value is echoed only where the row says it may be — see `RemovedSetting::echo_value`.
    let (setting, spelling) = if r.echo_value {
        (format!("{var}={full:?}"), format!("{full:?}"))
    } else {
        (var.to_string(), "that spelling".to_string())
    };
    let mut out = format!(
        "{setting} is set, but {var} is NO LONGER READ — removed in {removed_in}, because {why}.\n\
         Its old reader compared the value EXACTLY — no trimming, no `#` comment — so it did NOT \
         act on {spelling}: the process that ran took the default ({defaults}). To keep that, \
         write nothing.\n",
        removed_in = r.removed_in,
        why = r.why,
    );
    if let Some(meant) = meant {
        out.push_str(&format!("If you MEANT `{meant}`:\n\n"));
        for k in &keys {
            out.push_str(&format!("    vike-cli config set {k} {meant}\n"));
        }
        out.push('\n');
    }
    out.push_str(&format!("then unset {var}.\n"));
    Some(out)
}

/// `(venue, "on" | "off")` for a `venue.<venue>.<field>` key, read from the venue-field catalog —
/// the default `config show` prints too, so the refusal cannot name a different one.
fn declared_default(key: &str) -> (&str, &'static str) {
    let (venue, field) =
        key.strip_prefix("venue.").and_then(|rest| rest.split_once('.')).unwrap_or((key, ""));
    let word = match vike_model::venues::venue_fields::venue_field(venue, field).map(|f| f.default)
    {
        Some("1") => "on",
        Some("0") => "off",
        _ => "its built-in default",
    };
    (venue, word)
}

/// Render the operator's own value into the suggested TOML line when it is a value the key would
/// actually accept, else a placeholder.
///
/// Echoing garbage back (`max_notional_per_order = nope`) would hand over a line that fails the
/// loader with a *second*, unrelated error — and a non-positive number is rejected by
/// [`crate::Policy::apply`] as "a ceiling of 0 denies every order". Both cases get the placeholder
/// so the suggestion is always a line that works.
fn toml_value(raw: &str) -> String {
    match raw.parse::<f64>() {
        Ok(v) if v.is_finite() && v > 0.0 => raw.to_string(),
        _ => "<a positive number, in quote currency>".to_string(),
    }
}

/// The first whitespace-separated token before any `#` — how every Polymarket reader read its value.
fn first_token(raw: &str) -> &str {
    raw.split('#').next().unwrap_or("").split_whitespace().next().unwrap_or("")
}

/// A list value's tokens (commas and whitespace separate; `#` ends the list), joined by commas.
fn list_value(raw: &str) -> String {
    raw.split('#')
        .next()
        .unwrap_or("")
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(",")
}

/// `value` as ONE shell word: unchanged when every character passes a shell untouched, single-quoted
/// otherwise — the printed line is meant to be pasted.
fn shell_word(value: &str) -> String {
    if value.chars().all(|c| c.is_ascii_alphanumeric() || "._-:/,@%+=".contains(c)) {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', r"'\''"))
    }
}

#[path = "removed_tests.rs"]
#[cfg(test)]
mod removed_tests;
