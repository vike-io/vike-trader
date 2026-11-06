//! **How an operator WRITES a settings key** — the remedy clause every operator-facing message
//! renders through, rather than spelling `vike-cli config set <key> <value>` at each call site by
//! hand.
//!
//! # One arm
//!
//! **There is exactly one way to write a settings key, on every box, because there is exactly one
//! store** (`docs/decisions/0086`). So this module renders the STORE remedy alone —
//! `vike-cli config set <dotted key> <value>` — unconditionally.
//!
//! # No retype confirm, for any key (0086 point 7)
//!
//! `WriteRemedy` never renders a `--confirm <key>` flag: the ceremony is deleted (*"confirmation
//! over confirmation … a nightmare"*). Every rendered command is a plain
//! `vike-cli config set <key> <value>`, guarded key or not.

use crate::write::SettingsSection;

/// **What an operator does about an UNSOUND settings store, by cause** — the one paragraph every
/// surface that refuses on [`crate::Settings::seal_refusal`] ends with (the ordering root's boot
/// refusal, the `trade`/`mcp` refusal), so they cannot drift apart about the cure.
///
/// It names both causes because the surfaces only hold the cause as a sentence: a ROW the loader
/// refuses is removed with `vike-cli config unset` (decision 0161; `vike-cli config check` prints
/// the exact line per row), and a seal whose row COUNT is off is repaired only by restoring the
/// settings database from the box's nightly backup — `config unset` refuses that state, because
/// moving a count that is already wrong would hide the erase it exists to catch.
pub const UNSOUND_STORE_REPAIR: &str = "Diagnose with `vike-cli config check`: it names every ROW the loader refuses and prints the \
     `vike-cli config unset <section>.<key>` line that removes it (the seal's row count moves \
     with the row, in one transaction, so the store loads again). Only when the cause above is the \
     seal's ROW COUNT — rows that arrived or left by a hand INSERT/DELETE, or by a restored backup \
     — is there no row to remove: nothing can tell which rows are the right ones, so that case is \
     repaired by restoring `<project>/settings/db/vike.db` from this box's nightly backup.";

impl SettingsSection {
    /// The remedy for writing `leaf` inside this section — `policy.deadman_timeout_ms`'s home,
    /// say. Takes the LEAF rather than a dotted string, so the dotted spelling (which
    /// `vike-cli config show` also uses) is COMPUTED and the call site has no parse that can fail.
    #[must_use]
    pub fn write_remedy(self, leaf: &'static str) -> WriteRemedy {
        WriteRemedy { section: self, leaf }
    }
}

/// One settings key an operator-facing message is telling somebody to WRITE.
///
/// Every method is a pure `String` render. Nothing here decides a level, logs, or resolves a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteRemedy {
    section: SettingsSection,
    leaf: &'static str,
}

impl WriteRemedy {
    /// The dotted spelling — `policy.deadman_timeout_ms`.
    #[must_use]
    pub fn dotted(&self) -> String {
        format!("{}.{}", self.section.section(), self.leaf)
    }

    /// **Why this key has no value on this box.**
    #[must_use]
    pub fn absent_clause(&self) -> String {
        // ⚠ No `so` inside this clause: every caller reads *"{absent_clause}, so <the
        // consequence>"*, and an inner `so` makes the operator's eye bind the consequence to the
        // wrong part of the sentence.
        format!("the settings database does not carry `{}`", self.dotted())
    }

    /// **Where the paste-ready block that follows must go** — the clause a message puts in front of
    /// [`Self::write_line`].
    #[must_use]
    pub fn write_clause(&self) -> String {
        "run this".to_string()
    }

    /// **The paste-ready line itself** — the `vike-cli config set` invocation that reaches the row.
    ///
    /// `value` is rendered exactly as given, so a caller that needs quoting supplies it
    /// (`"\"live\""`). That is deliberate: this module cannot know a key's type, and a quoting rule
    /// invented here would be a second authority against [`crate::write::write_setting_row`]'s own
    /// parser.
    #[must_use]
    pub fn write_line(&self, value: &str) -> String {
        format!("vike-cli config set {} {value}", self.dotted())
    }

    /// **The same instruction as one INLINE clause**, for a message that names the switch mid
    /// sentence instead of ending in a paste-ready block.
    #[must_use]
    pub fn inline_write(&self, value: &str) -> String {
        format!("`vike-cli config set {} {value}`", self.dotted())
    }

    /// **Where the value lives, as a noun** — for a message that reports rather than instructs
    /// (*`link_deadman_grace_ms = 0` in X turns it off for EVERY venue*).
    #[must_use]
    pub fn holder(&self) -> String {
        "the settings database".to_string()
    }

    /// **How an operator UNSETS the key** — the other half of [`Self::write_line`]:
    /// `vike-cli config unset <key>` (decision 0161) removes the row, so the key resolves to its
    /// compiled-in default again — whatever that default is today, which is the point of removing
    /// the row rather than writing the current default back into a new one (that would freeze
    /// today's number into the store against a later change of the default).
    ///
    /// Takes no value: the verb names none, and a caller that used to pass the key's default to
    /// render a `config set` line no longer has a number to keep in step with the constant.
    #[must_use]
    pub fn unset_clause(&self) -> String {
        format!("Run `vike-cli config unset {}`", self.dotted())
    }
}

#[path = "remedy_tests.rs"]
#[cfg(test)]
mod remedy_tests;
