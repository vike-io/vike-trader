//! **How an operator WRITES a settings key** — the remedy clause every operator-facing message
//! renders through, rather than spelling `vike-cli config set <key> <value>` at each call site by
//! hand.
//!
//! # Former shape, and why it collapsed to one arm
//!
//! Until `docs/decisions/0086` this module rendered TWO arms — one for a box whose settings files
//! still answered, one for a box that had crossed over to the database — because a message that
//! told an operator to edit `policy.toml` on a box that no longer read it sent them to write a file
//! nothing opens. MEASURED in the live journal on the CI box, v0.1.33, 2026-09-23T02:41:43Z: a warning
//! told an operator to add a key to `<project>/settings/policy.toml`, twenty-three lines under a
//! disclosure line from the SAME boot saying that file was ABSENT and the database answered for
//! every key.
//!
//! 0086 removes the second arm rather than fixing the first: **there is exactly one way to write a
//! settings key now, on every box, because there is exactly one store** (*"there are no settings
//! files any more, as source, fallback, export or way back"*). So this module keeps the STORE
//! rendering alone — `vike-cli config set <dotted key> <value>` — and every caller that used to
//! thread a `vike_config::Authority` through to pick an arm now renders unconditionally.
//!
//! # No retype confirm, for any key (0086 point 7)
//!
//! `WriteRemedy` never renders a `--confirm <key>` flag: the ceremony is deleted (*"confirmation
//! over confirmation … a nightmare"*). Every rendered command is a plain
//! `vike-cli config set <key> <value>`, guarded key or not.

use crate::write::SettingsFile;

impl SettingsFile {
    /// The remedy for writing `leaf` inside this section — `policy.deadman_timeout_ms`'s home,
    /// say. Takes the LEAF rather than a dotted string, so the dotted spelling (which
    /// `vike-cli config show` also uses) is COMPUTED and the call site has no parse that can fail.
    #[must_use]
    pub fn write_remedy(self, leaf: &'static str) -> WriteRemedy {
        WriteRemedy { file: self, leaf }
    }
}

/// One settings key an operator-facing message is telling somebody to WRITE.
///
/// Every method is a pure `String` render. Nothing here decides a level, logs, or resolves a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteRemedy {
    file: SettingsFile,
    leaf: &'static str,
}

impl WriteRemedy {
    /// The dotted spelling — `policy.deadman_timeout_ms`.
    #[must_use]
    pub fn dotted(&self) -> String {
        format!("{}.{}", self.file.section(), self.leaf)
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

    /// **How an operator UNSETS the key** — the other half of [`Self::write_line`], and not its
    /// mirror image: there is no `config unset`, so "unsetting" a key is writing the default back
    /// through the same verb.
    ///
    /// `default_value` is what the key resolves to with nothing written, rendered as the caller
    /// would write it.
    #[must_use]
    pub fn unset_clause(&self, default_value: &str) -> String {
        format!("Run `vike-cli config set {} {default_value}`", self.dotted())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_remedy_names_config_set_and_never_a_file_to_write_into() {
        let r = SettingsFile::Policy.write_remedy("deadman_timeout_ms");
        assert_eq!(
            r.write_line("60000"),
            "vike-cli config set policy.deadman_timeout_ms 60000",
            "the ONE write that lands in the one store"
        );
        assert_eq!(r.write_clause(), "run this");
        let inline = SettingsFile::Flags.write_remedy("reconcile_off").inline_write("true");
        assert_eq!(inline, "`vike-cli config set flags.reconcile_off true`");
        assert!(
            !inline.contains(".toml"),
            "the remedy may not name a FILE as somewhere to write: {inline}"
        );
        assert_eq!(r.holder(), "the settings database");
        assert_eq!(
            r.unset_clause("0"),
            "Run `vike-cli config set policy.deadman_timeout_ms 0`",
            "there is no `config unset`, so the way back is the same verb"
        );
        let absent = r.absent_clause();
        assert!(absent.contains("the settings database does not carry"), "{absent}");
    }

    /// Every file's section is the first segment `vike-cli config set` demands, so the rendered
    /// command is one this box can actually run.
    #[test]
    fn every_settings_section_renders_a_dotted_key_config_set_would_accept() {
        for file in SettingsFile::ALL {
            let r = file.write_remedy("k");
            assert_eq!(r.dotted(), format!("{}.k", file.section()));
            assert_eq!(SettingsFile::parse(file.section()), Some(file));
            assert!(r.write_line("v").starts_with("vike-cli config set "));
        }
    }

    /// **No rendered command ever carries `--confirm`, for any key** — the deleted ceremony
    /// (0086 point 7).
    #[test]
    fn no_rendered_command_ever_carries_a_confirm_flag() {
        for (file, leaf) in [
            (SettingsFile::Policy, "deadman_timeout_ms"),
            (SettingsFile::Flags, "venue_catalog_off"),
        ] {
            let r = file.write_remedy(leaf);
            for rendered in [r.write_line("60000"), r.inline_write("60000"), r.unset_clause("0")] {
                assert!(!rendered.contains("--confirm"), "{rendered}");
            }
        }
    }
}
