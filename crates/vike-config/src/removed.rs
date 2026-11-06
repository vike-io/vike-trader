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
//! names the exact key that replaces it, with the operator's own value rendered into a
//! copy-pasteable `vike-cli config set` line.
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
//! [`REMOVED_PROJECT_FILE`] was a per-project override file, sitting at the project ROOT, one level
//! ABOVE `<project>/settings/`, and overriding `[config]` and `[preferences]`. It is gone, and the
//! reason is the reason `<project>/settings/` exists at all: one home for every setting, so no
//! operator has to ask *which source won?* That question is where real defects hide, which this
//! crate's own history demonstrates twice over ([`crate::consumed`] for a key nothing reads, and
//! the layer itself, which was implemented, tested and printed in an operator-facing precedence
//! header while no binary read one).
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

mod project_file;
mod refuse;
mod table;

pub use project_file::{REMOVED_PROJECT_FILE, RemovedFileProbe};
pub(crate) use project_file::{refuse_removed_project_file, removed_project_file_message};
pub use refuse::refuse_removed_env;
pub use table::REMOVED_ENV;

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
    /// The settings SECTION that held it (`policy`, `config`, `preferences` or `flags` — a
    /// [`crate::SettingsSection`] word), `the settings database` on the decision-0095 venue rows
    /// (each has a key, so it is never rendered), or `secrets.env` for the credential store.
    /// Rendered only when [`Self::key`] is `None`: a section word renders as that section of the
    /// settings database, never as a path, and `secrets.env` renders the credential-store line.
    /// Empty when nothing in any settings section replaces it — the [`Self::why`] then says what
    /// does.
    pub file: &'static str,
    /// The FULL dotted key a `vike-cli config set` writes instead (`policy.max_notional_per_order`,
    /// `flags.poly_exec`, `venue.polymarket.proxy_host`), when the VALUE moved.
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
    /// misconfiguration it is reporting. A number the operator has to re-type into a `config set`
    /// line is the case for `true`.
    ///
    /// In [`REMOVED_ENV`] a row opts in with the `echoed` builder; the constructors set `false`.
    pub echo_value: bool,
    /// `Some` for a variable whose EMPTY value (`VAR=`, or only whitespace) meant something other
    /// than the default to its old reader — such a row is refused when empty too, with a message
    /// that says what the empty value meant. `None` (every other row): an empty value configured
    /// nothing, so it starts normally. See the module doc's "What counts as set".
    ///
    /// With a [`Self::key`], the refusal prints the line that says the same thing where the setting
    /// lives now ([`EmptyMeaning::write`]); without one there is no line to give, and it says that.
    ///
    /// In [`REMOVED_ENV`] a row sets it with the `empty_means` builder; the constructors leave it
    /// `None`.
    pub when_empty: Option<EmptyMeaning>,
}

/// The constructors [`REMOVED_ENV`]'s rows are spelled with: a row states its own name, home,
/// removal and reason, and inherits the defaults the table shares.
///
/// Two shapes, read off the table: a variable whose value MOVED to a key (`moved`), and one that
/// configures nothing now (`dropped`: `key: None`, so the unused [`ValueMap::Verbatim`]). Both
/// start with `echo_value: false` and `when_empty: None`; `echoed` and `empty_means` are the only
/// ways a row departs from them, so a row that says neither withholds its value and starts on a
/// blank.
///
/// ⚠ `crates/vike-ops/tests/container_deploy/container_image_gate/readers.rs`'s `removed_variables` reads
/// each row's FIRST argument out of the table's TEXT, keyed on `RemovedSetting::moved(` and
/// `RemovedSetting::dropped(`: a renamed or added constructor re-anchors it, or that gate goes
/// red.
impl RemovedSetting {
    /// A variable whose VALUE moved to `key`: the refusal prints a paste-ready `vike-cli config
    /// set` line for it, carrying the operator's value through `value`.
    ///
    /// The value is NOT echoed until [`Self::echoed`] says so — the safe default, because a row
    /// that forgets to opt in withholds a value rather than printing a secret.
    const fn moved(
        var: &'static str,
        file: &'static str,
        key: &'static str,
        value: ValueMap,
        removed_in: &'static str,
        why: &'static str,
    ) -> Self {
        Self {
            var,
            file,
            key: Some(key),
            value,
            removed_in,
            why,
            echo_value: false,
            when_empty: None,
        }
    }

    /// A variable that configures nothing now: no key, so no line to paste, and `file` (possibly
    /// empty) and `why` carry the whole answer. `value` is [`ValueMap::Verbatim`], unused without a
    /// key, and the value is never echoed (`only_rows_with_a_key_to_paste_echo_their_value`).
    const fn dropped(
        var: &'static str,
        file: &'static str,
        removed_in: &'static str,
        why: &'static str,
    ) -> Self {
        Self {
            var,
            file,
            key: None,
            value: ValueMap::Verbatim,
            removed_in,
            why,
            echo_value: false,
            when_empty: None,
        }
    }

    /// The row's value MAY appear in the refusal ([`Self::echo_value`] `true`).
    const fn echoed(self) -> Self {
        Self { echo_value: true, ..self }
    }

    /// The row refuses an EMPTY value too, saying what it meant ([`Self::when_empty`]).
    const fn empty_means(self, meaning: EmptyMeaning) -> Self {
        Self { when_empty: Some(meaning), ..self }
    }
}

#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::path::Path;

#[cfg(test)]
use crate::error::ConfigError;

#[path = "removed_tests.rs"]
#[cfg(test)]
mod removed_tests;
