//! **The `trade` PLANE's group layer.** Every verb lives in a group and the group is REQUIRED —
//! there is no bare `vike-cli trade <verb>` — mirroring `crate::cmd::data`'s own group split. The
//! BUILT groups (`account | order | position | reconcile | strategy | venue`) are `clap` subcommands of `crate::cmd::trade`'s
//! `Cli`, each with its own verb enum, so their routing and their roster in `--help` are `clap`'s;
//! what this module owns is the answer to every OTHER leading word ([`refusal`]).
//!
//! ⚠ **THE ONE EXEMPTION, and the rule that licenses it.** `status`, `halt` and `resume` stay at
//! the top level with no group, and that is not a compatibility carve-out: it is the
//! risk-direction law showing up in the grammar. **A verb that takes NO BOOK takes no group**, and
//! all three are node-wide and risk-REDUCING. `halt` under pressure is one word. `resume`'s
//! per-venue twin is `venue resume <venue>`: it names an EXCHANGE, the `venue` group's noun (the
//! link dead-man's halt is the whole exchange's, decision 0180), so it lives in that group, verb
//! first like every other (the owner's stage-5 ruling 1; it was the group-less `resume-venue`).
//!
//! An unbuilt group is NAMED in the roster and refused BY NAME, never reported as unknown: an
//! operator who types it has read the design, and telling them it is wrong would be false.
//!
//! A word that was renamed is refused BY NAME with the word that replaced it ([`RENAMED_WORDS`],
//! [`renamed`]): never an alias (root `CLAUDE.md`'s one-name rule), never "unknown".

use crate::refusals::RetiredTradeSpelling;

/// The plane's groups: name, and the question the group answers.
pub(crate) const GROUPS: &[(&str, &str)] = &[
    ("account", "ONE account's engine: halt, reduce, resume, remove a symbol"),
    ("order", "the working set, and the intent to change it"),
    ("position", "what is held"),
    ("reconcile", "reconcile divergences held for an operator"),
    ("strategy", "what trades by itself"),
    ("venue", "what an account's venue says: balance, open orders, positions; resume one venue"),
    ("watch", "the live stream"),
];

/// A group that is designed and not built. ⚠ ONE sentence, spelled once: a per-call-site rewording
/// is how two refusals for one fact come to disagree.
pub(crate) fn unbuilt_group_message(group: &str) -> String {
    format!(
        "`trade {group}` is designed but not built yet - every other group `trade --help` names \
         is. See docs/superpowers/specs/2026-09-21-trade-cli-surface-design.md and its phase table."
    )
}

/// The rendered refusal of an old spelling, `error [cli.trade.renamed]: `<was>` is now `<now>`…`:
/// the one way this plane shows a word it renamed (a group, a verb, a flag, a REPL word).
pub(crate) fn renamed(was: impl Into<String>, now: impl Into<String>) -> String {
    RetiredTradeSpelling::Renamed { was: was.into(), now: now.into() }.rendered()
}

/// The plane's leading words that were renamed (stage 5's naming pass), and the command that
/// replaced each. Each is refused by [`refusal`] through [`renamed`].
pub(crate) const RENAMED_WORDS: &[(&str, &str)] = &[
    ("recon", "trade reconcile"),
    ("resume-venue", "trade venue resume"),
    ("market-exit", "trade position close-all"),
    ("panic", "trade position close-all"),
    ("pos", "trade position ls"),
];

/// REPL words that have a one-shot spelling now. ⚠ Refused BY NAME with the replacement: a
/// deprecation that fails without naming its replacement costs a support round trip.
pub(crate) const RETIRED_FLAT_SPELLINGS: &[(&str, &str)] = &[
    ("submit", "trade order submit"),
    ("buy", "trade order submit <account> <symbol> buy"),
    ("sell", "trade order submit <account> <symbol> sell"),
    ("cancel", "trade order cancel"),
    ("modify", "trade order modify"),
    ("mass-cancel", "trade order mass-cancel"),
    ("orders", "trade order ls"),
    ("flatten", "trade position flatten"),
    ("close-all", "trade position close-all"),
    ("positions", "trade position ls"),
    ("mount", "trade strategy mount"),
    ("unmount", "trade strategy unmount"),
];

/// What a leading word that is none of the plane's subcommands earns — the refusal's text. ⚠ The
/// roster is what every refusal is derived from.
///
/// * `state` — the REMOVED read-or-halt verb, answered with the words that replaced it.
/// * `watch` — a group that is designed and not built.
/// * a renamed word — the command that replaced it ([`RENAMED_WORDS`]).
/// * a retired REPL spelling — its one-shot replacement.
/// * anything else — the group roster.
pub(crate) fn refusal(word: &str) -> String {
    match word {
        // The one-shot twin of `parse_line`'s `state` arm: `vike-cli trade state halted` is a line
        // that sat in runbooks and unit files, so the refusal names the words that replaced it
        // rather than reporting `state` as an unknown ARGUMENT.
        "state" => super::STATE_MIGRATION_HINT.to_string(),
        "watch" => unbuilt_group_message(word),
        other => {
            if let Some((_, now)) = RENAMED_WORDS.iter().find(|(was, _)| *was == other) {
                return renamed(format!("trade {other}"), *now);
            }
            if let Some((_, now)) = RETIRED_FLAT_SPELLINGS.iter().find(|(was, _)| *was == other) {
                return format!(
                    "`trade {other}` is a REPL word, not a command. One-shot it is `{now}`. Every \
                     trade verb lives in a GROUP ({}) - the plane got too wide to be flat.",
                    group_roster()
                );
            }
            format!("unknown `trade` group '{other}' ({})", group_roster())
        }
    }
}

/// The roster as one string, DERIVED from [`GROUPS`] rather than re-typed. The hand-written copy
/// this shape replaced in `crate::cmd::data` omitted a subcommand that had shipped months earlier.
fn group_roster() -> String {
    GROUPS.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(" | ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::args;

    #[test]
    fn an_unbuilt_group_is_refused_by_name_and_says_it_is_designed() {
        assert_eq!(
            refusal("watch"),
            unbuilt_group_message("watch"),
            "it may not re-word the shared sentence"
        );
        assert!(refusal("watch").contains("designed"), "the refusal must say it is coming");
    }

    /// A group in the roster is either a real subcommand of the plane's `clap` command (the three
    /// built ones) or refused by name — a group nobody classified would fall into the unknown
    /// arm and read as a typo.
    #[test]
    fn every_group_in_the_roster_is_either_a_subcommand_or_refused_by_name() {
        let command = args::command::<crate::cmd::trade::Cli>(crate::cmd::trade::BIN);
        for (name, _) in GROUPS {
            let built = command.find_subcommand(name).is_some();
            let refused_by_name = refusal(name) == unbuilt_group_message(name);
            assert!(built ^ refused_by_name, "group '{name}' reads as unknown, or as both");
        }
    }

    #[test]
    fn a_repl_word_is_refused_with_its_one_shot_spelling() {
        let msg = refusal("submit");
        assert!(msg.contains("trade order submit"), "name the replacement: {msg}");
    }

    #[test]
    fn the_removed_state_word_is_answered_with_the_words_that_replaced_it() {
        assert_eq!(refusal("state"), crate::cmd::trade::STATE_MIGRATION_HINT);
    }

    #[test]
    fn an_unknown_group_names_the_roster() {
        let msg = refusal("nonsense");
        for (name, _) in GROUPS {
            assert!(msg.contains(name), "the refusal must name '{name}': {msg}");
        }
    }

    /// A renamed word is refused by its id, naming a command the plane really has, and is no group
    /// or retired REPL word as well.
    #[test]
    fn a_renamed_word_is_refused_naming_a_command_that_exists() {
        let mut command = args::command::<crate::cmd::trade::Cli>(crate::cmd::trade::BIN);
        command.build();
        for (was, now) in RENAMED_WORDS {
            assert_eq!(
                refusal(was),
                renamed(format!("trade {was}"), *now),
                "`trade {was}` is refused as renamed"
            );
            assert!(refusal(was).starts_with("error [cli.trade.renamed]: "), "{}", refusal(was));
            let mut at = &command;
            for word in now.split(' ').skip(1) {
                at = at
                    .find_subcommand(word)
                    .unwrap_or_else(|| panic!("`{now}` names no command (no `{word}`)"));
            }
            assert!(!GROUPS.iter().any(|(g, _)| g == was), "`{was}` is a group too");
            assert!(!RETIRED_FLAT_SPELLINGS.iter().any(|(w, _)| w == was), "`{was}` twice");
        }
    }

    #[test]
    fn no_retired_spelling_is_also_a_group() {
        for (was, _) in RETIRED_FLAT_SPELLINGS {
            assert!(
                !GROUPS.iter().any(|(g, _)| g == was),
                "'{was}' is both a group and a retired spelling - one of the two is wrong"
            );
        }
    }
}
