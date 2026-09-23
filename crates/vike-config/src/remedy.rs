//! **How an operator WRITES a settings key on THIS box** — the remedy clause, rendered from the
//! [`Authority`] that actually answered rather than from the file that used to.
//!
//! # The defect this module exists to remove
//!
//! A safety warning whose REMEDY cannot arm the thing it is about. MEASURED in the live journal on
//! the CI box, v0.1.33, 2026-09-23T02:41:43Z, on the boot of a daemon that a few lines later logged
//! `binance: DEMO credentials present → LIVE exec client (real demo orders)`:
//!
//! ```text
//! WARN THE SILENCE DEAD-MAN IS OFF: <project>/settings/policy.toml does not name
//!      `deadman_timeout_ms` … To arm it for a venue that never closes, add this to
//!      <project>/settings/policy.toml …
//! ```
//!
//! …twenty-three lines under this box's own disclosure, on the same boot:
//!
//! ```text
//! INFO settings file: policy.toml ABSENT (…/settings/policy.toml)
//! INFO settings: the settings DATABASE answers for every key on this box (`vike-cli config adopt`
//!      sealed it) — the settings files above are NOT read for resolution, whatever they hold
//! ```
//!
//! So an operator who follows the instruction creates a file nothing reads, restarts, sees the same
//! warning, and has no way to tell why. The warning's SUBSTANCE was right — there genuinely is no
//! dead-man — and only its remedy was unfollowable.
//!
//! # The class, and why the cure is a module rather than an edit
//!
//! It was the THIRD instance in two days of one shape: an operator-facing string that is true in
//! the shape the code was written for and false in the shape the box is actually in.
//!
//! 1. the boot banner credited `policy.toml` for values the DATABASE supplied (fixed — that is why
//!    [`crate::boot_lines`] takes a [`crate::StoreLayer`]);
//! 2. `config mirror`'s repair advice named `config compare`, a verb blind to the plane that
//!    failed;
//! 3. this one.
//!
//! The cure is the same each time — **the message must follow the store that answered** — and a
//! cure applied one string at a time is a cure that has to be re-derived at the fourth instance.
//! So the two halves every such message needs are rendered HERE, from the [`Authority`] the
//! resolving process already carries on [`crate::Settings::authority`], and a caller threads that
//! value rather than re-resolving anything. ⚠ Re-resolving the settings directory a second time is
//! its own defect class (`crates/vike-boot/tests/one_owner.rs` gates it), so this module opens no
//! file, reads no environment and probes nothing: it is a pure function of the arm it is handed.
//!
//! # Why `vike-cli config set` is the STORE arm's remedy
//!
//! Because it is the one write that lands in the store that answers. [`crate::set_setting`] writes
//! the settings TOML **and**, under [`crate::write::RowSync::Mirror`], re-derives the touched
//! section into the database inside the same lock — so on an adopted box the ROW moves, and on an
//! unadopted one the file was always the answer anyway. A hand-edited file is the half of that
//! which does nothing here.
//!
//! ⚠ **Which is also why the FILES arm is not rewritten to say `config set`.** It would be
//! correct — but the file snippet is what every operator of an unadopted box has in their runbook
//! and their shell history, and a class fix is not a licence to churn a message that was already
//! true. `crates/vike-tradehub/tests/deadman_absent_warning.rs` pins the files arm BYTE-IDENTICAL
//! to what shipped before this module existed, for exactly that reason.

use crate::source::Authority;
use crate::write::SettingsFile;

impl Authority {
    /// The two halves of *write this key* on a box whose settings come from `self`.
    ///
    /// Takes the FILE and the LEAF rather than a dotted string, so the dotted spelling
    /// (`policy.deadman_timeout_ms` — the one [`crate::set_setting`] and `vike-cli config show`
    /// both use) is COMPUTED and the call site has no parse that can fail. A caller holding only a
    /// dotted key reaches [`SettingsFile::parse`] first and handles its `None` itself.
    #[must_use]
    pub fn write_remedy(self, file: SettingsFile, leaf: &'static str) -> WriteRemedy {
        WriteRemedy { authority: self, file, leaf }
    }
}

/// One settings key an operator-facing message is telling somebody to WRITE, plus the
/// [`Authority`] that decides what writing it actually means on this box.
///
/// Every method is a pure `String` render. Nothing here decides a level, logs, or resolves a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteRemedy {
    authority: Authority,
    file: SettingsFile,
    leaf: &'static str,
}

impl WriteRemedy {
    /// The dotted spelling — `policy.deadman_timeout_ms`.
    #[must_use]
    pub fn dotted(&self) -> String {
        format!("{}.{}", self.file.section(), self.leaf)
    }

    /// The project-relative path of the settings file this key BELONGS to, named whether or not it
    /// is read. `<project>/settings/policy.toml`.
    ///
    /// ⚠ Naming the file is not the defect; naming it as a WRITE TARGET on a box that does not
    /// read it is. The store arm below uses this to say the file is INERT, which is the one thing
    /// an operator staring at an absent `policy.toml` most needs to hear.
    #[must_use]
    pub fn file_path(&self) -> String {
        format!("<project>/settings/{}", self.file.file_name())
    }

    /// **Why this key has no value on this box**, in the vocabulary of the source that answered.
    ///
    /// Files: ``<project>/settings/policy.toml does not name `deadman_timeout_ms` `` —
    /// byte-identical to what the dead-man warning has always said.
    ///
    /// Store: the DATABASE does not carry the dotted key, and the file that would have held it is
    /// named as the thing that is NOT read — because on an adopted box the file is usually absent
    /// too, and *absent* and *inert* send an operator to opposite places.
    #[must_use]
    pub fn absent_clause(&self) -> String {
        match self.authority {
            Authority::Files => format!("{} does not name `{}`", self.file_path(), self.leaf),
            // ⚠ No `so` inside this clause: every caller reads *"{absent_clause}, so <the
            // consequence>"*, and an inner `so` makes the operator's eye bind the consequence to
            // the file rather than to the missing key.
            Authority::Store => format!(
                "the settings DATABASE does not carry `{}` (this box is ADOPTED — `vike-cli \
                 config adopt` sealed it, and {} is NOT read for resolution whatever it holds)",
                self.dotted(),
                self.file_path()
            ),
        }
    }

    /// **Where the paste-ready block that follows must go** — the clause a message puts in front of
    /// [`Self::write_line`].
    ///
    /// Files: `add this to <project>/settings/policy.toml`. Store: `run this`.
    #[must_use]
    pub fn write_clause(&self) -> String {
        match self.authority {
            Authority::Files => format!("add this to {}", self.file_path()),
            Authority::Store => "run this".to_string(),
        }
    }

    /// **The paste-ready line itself** — a TOML assignment on a files box, the `vike-cli config
    /// set` that reaches the rows on an adopted one.
    ///
    /// `value` is rendered exactly as given, so a caller that needs TOML quoting supplies it
    /// (`"\"live\""`). That is deliberate: this module cannot know a key's type, and a quoting rule
    /// invented here would be a second authority against [`crate::set_setting`]'s own parser.
    #[must_use]
    pub fn write_line(&self, value: &str) -> String {
        match self.authority {
            Authority::Files => format!("{} = {value}", self.leaf),
            Authority::Store => {
                format!("vike-cli config set {} {value}{}", self.dotted(), self.confirm())
            }
        }
    }

    /// **The ` --confirm <key>` a guarded key owes, or the empty string.**
    ///
    /// ⚠ **Without this the STORE arm renders a command that is REFUSED**, and refused in the one
    /// place these messages are most likely to be consumed. `crates/vike-cli/src/cmd/config_set.rs`'s
    /// `resolve_confirm` demands the retyped key for every key
    /// [`crate::requires_typed_confirm`] answers for, and off a terminal there is no prompt to fall
    /// back on — a runbook line, an `ssh host 'vike-cli …'`, a pre-flight or an agent all get a
    /// usage error. MEASURED 2026-09-23 on the CI box: `config set policy.deadman_timeout_ms 60000`
    /// refused with *"is a policy RISK CEILING … re-run with `--confirm <key>`"*, which is how this
    /// was found.
    ///
    /// ⚠ **The FILES arm must never grow one** — a TOML line owes no ceremony, and the pairing test
    /// asserts the asymmetry. That asymmetry is the point: this module exists because the adopted
    /// box was being handed the worse instruction, and a remedy that is merely DIFFERENT from the
    /// broken one is not a fix.
    fn confirm(&self) -> String {
        if crate::requires_typed_confirm(&self.dotted()) {
            format!(" --confirm {}", self.dotted())
        } else {
            String::new()
        }
    }

    /// **The same instruction as one INLINE clause**, for a message that names the switch mid
    /// sentence instead of ending in a paste-ready block.
    ///
    /// Files: ``` `reconcile_off = true` in <project>/settings/flags.toml ``` — byte-identical to
    /// what these messages already said. Store: ``` `vike-cli config set flags.reconcile_off
    /// true` ```, and no file is named, because on that box naming one is the defect.
    #[must_use]
    pub fn inline_write(&self, value: &str) -> String {
        match self.authority {
            Authority::Files => {
                format!("`{} = {value}` in {}", self.leaf, self.file_path())
            }
            Authority::Store => {
                format!("`vike-cli config set {} {value}{}`", self.dotted(), self.confirm())
            }
        }
    }

    /// **Where the value lives on this box, as a noun** — for a message that reports rather than
    /// instructs (*`link_deadman_grace_ms = 0` in X turns it off for EVERY venue*).
    ///
    /// Files: `<project>/settings/policy.toml`. Store: `the settings database` — never a path,
    /// because there is nothing at a path for that operator to look at.
    #[must_use]
    pub fn holder(&self) -> String {
        match self.authority {
            Authority::Files => self.file_path(),
            Authority::Store => "the settings database".to_string(),
        }
    }

    /// **How an operator UNSETS the key** — the other half of [`Self::write_line`], and not its
    /// mirror image: a files box deletes a LINE, an adopted box writes the default back through
    /// the same verb, because no `config unset` exists.
    ///
    /// `default_value` is what the key resolves to with nothing written, rendered as the caller
    /// would write it.
    #[must_use]
    pub fn unset_clause(&self, default_value: &str) -> String {
        match self.authority {
            Authority::Files => format!("Delete the line from {}", self.file_path()),
            Authority::Store => {
                format!(
                    "Run `vike-cli config set {} {default_value}{}`",
                    self.dotted(),
                    self.confirm()
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The FILES arm is the text that shipped before this module existed, and it is pinned here as
    /// well as at the dead-man warning: a class fix that quietly reworded a message which was
    /// already correct would be churn wearing a repair's clothes.
    #[test]
    fn the_files_arm_names_the_file_and_writes_a_toml_line() {
        let r = Authority::Files.write_remedy(SettingsFile::Policy, "deadman_timeout_ms");
        assert_eq!(r.dotted(), "policy.deadman_timeout_ms");
        assert_eq!(
            r.absent_clause(),
            "<project>/settings/policy.toml does not name `deadman_timeout_ms`"
        );
        assert_eq!(r.write_clause(), "add this to <project>/settings/policy.toml");
        assert_eq!(r.write_line("60000"), "deadman_timeout_ms = 60000");
        assert_eq!(
            Authority::Files
                .write_remedy(SettingsFile::Flags, "reconcile_off")
                .inline_write("true"),
            "`reconcile_off = true` in <project>/settings/flags.toml"
        );
        assert_eq!(r.holder(), "<project>/settings/policy.toml");
        assert_eq!(r.unset_clause("0"), "Delete the line from <project>/settings/policy.toml");
    }

    /// The STORE arm names the verb that reaches the rows and NEVER names a file as somewhere to
    /// write. It may still name the file — as the thing that is not read.
    #[test]
    fn the_store_arm_names_config_set_and_never_a_file_to_write_into() {
        let r = Authority::Store.write_remedy(SettingsFile::Policy, "deadman_timeout_ms");
        assert_eq!(
            r.write_line("60000"),
            "vike-cli config set policy.deadman_timeout_ms 60000 --confirm policy.deadman_timeout_ms",
            "the ONE write that lands in the store that answers"
        );
        assert_eq!(r.write_clause(), "run this");
        let inline = Authority::Store
            .write_remedy(SettingsFile::Flags, "reconcile_off")
            .inline_write("true");
        assert_eq!(
            inline,
            "`vike-cli config set flags.reconcile_off true --confirm flags.reconcile_off`"
        );
        assert!(
            !inline.contains("settings/flags.toml"),
            "the store arm may not name a FILE as somewhere to write: {inline}"
        );
        assert_eq!(r.holder(), "the settings database");
        assert_eq!(
            r.unset_clause("0"),
            "Run `vike-cli config set policy.deadman_timeout_ms 0 --confirm policy.deadman_timeout_ms`",
            "there is no `config unset`, so the way back is the same verb"
        );
        let absent = r.absent_clause();
        assert!(absent.contains("the settings DATABASE does not carry"), "{absent}");
        assert!(
            absent.contains("NOT read for resolution"),
            "the file is named as INERT, not as a place to write: {absent}"
        );
    }

    /// ⚠ The property the whole module exists for, stated as a test rather than as prose: the two
    /// arms' WRITE instructions must differ. An assertion satisfied by both is the shape that let
    /// the defect ship.
    #[test]
    fn the_two_arms_never_render_the_same_write_instruction() {
        for file in SettingsFile::ALL {
            let files = Authority::Files.write_remedy(file, "some_key");
            let store = Authority::Store.write_remedy(file, "some_key");
            assert_ne!(files.write_line("1"), store.write_line("1"), "{file:?}");
            assert_ne!(files.inline_write("1"), store.inline_write("1"), "{file:?}");
            assert_ne!(files.write_clause(), store.write_clause(), "{file:?}");
            assert_ne!(files.absent_clause(), store.absent_clause(), "{file:?}");
            assert_ne!(files.holder(), store.holder(), "{file:?}");
            assert_ne!(files.unset_clause("0"), store.unset_clause("0"), "{file:?}");
        }
    }

    /// Every file's section is the first segment `vike-cli config set` demands, so the rendered
    /// command is one this box can actually run.
    #[test]
    fn every_settings_file_renders_a_dotted_key_config_set_would_accept() {
        for file in SettingsFile::ALL {
            let r = Authority::Store.write_remedy(file, "k");
            assert_eq!(r.dotted(), format!("{}.k", file.section()));
            assert_eq!(SettingsFile::parse(file.section()), Some(file));
            assert!(r.write_line("v").starts_with("vike-cli config set "));
            assert!(r.file_path().ends_with(".toml"));
        }
    }

    /// ⚠ **A guarded key's rendered command must carry the `--confirm` it owes, and the FILES arm
    /// must never grow one.**
    ///
    /// `crates/vike-cli/src/cmd/config_set.rs`'s `resolve_confirm` REFUSES a
    /// [`crate::requires_typed_confirm`] key with no `--confirm` when stdin is not a terminal — so
    /// without the suffix the store arm hands a runbook, an `ssh host '…'` or an agent a command
    /// that cannot run. MEASURED on the CI box 2026-09-23: *"is a policy RISK CEILING … re-run with
    /// `--confirm <key>`"*.
    ///
    /// The pairing is the point. A test that only checked "the store arm mentions `--confirm`"
    /// would pass with the files arm mentioning it too, which would be a second defect wearing the
    /// first one's fix.
    #[test]
    fn a_guarded_keys_store_command_carries_its_confirm_and_the_file_line_never_does() {
        let guarded = Authority::Store.write_remedy(SettingsFile::Policy, "deadman_timeout_ms");
        assert!(crate::requires_typed_confirm(&guarded.dotted()), "fixture must be guarded");
        for rendered in
            [guarded.write_line("60000"), guarded.inline_write("60000"), guarded.unset_clause("0")]
        {
            assert!(
                rendered.contains("--confirm policy.deadman_timeout_ms"),
                "a guarded key's command is REFUSED off a tty without it: {rendered}"
            );
        }

        let files = Authority::Files.write_remedy(SettingsFile::Policy, "deadman_timeout_ms");
        for rendered in
            [files.write_line("60000"), files.inline_write("60000"), files.unset_clause("0")]
        {
            assert!(!rendered.contains("--confirm"), "a TOML line owes no ceremony: {rendered}");
        }

        // ...and an UNGUARDED key must not grow the suffix it does not owe.
        let plain = Authority::Store.write_remedy(SettingsFile::Flags, "venue_catalog_off");
        assert!(!crate::requires_typed_confirm(&plain.dotted()), "fixture must be unguarded");
        assert!(!plain.write_line("true").contains("--confirm"), "{}", plain.write_line("true"));
    }
}
